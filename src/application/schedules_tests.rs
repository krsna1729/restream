use super::*;
use proptest::prelude::*;

const T: i64 = 1_800_000_000_000;

#[test]
fn a_one_off_runs_at_its_time_within_the_grace_and_never_again() {
    assert_eq!(
        due(T, Repeat::Once, true, None, T - 1),
        Due::Nothing,
        "not yet"
    );
    assert_eq!(due(T, Repeat::Once, true, None, T), Due::Run(T));
    assert_eq!(
        due(T, Repeat::Once, true, None, T + RUN_GRACE_MS),
        Due::Run(T)
    );
    assert_eq!(
        due(T, Repeat::Once, true, Some(T), T + 5_000),
        Due::Nothing,
        "already ran"
    );
    assert_eq!(next_occurrence(T, Repeat::Once, T + 1), None);
}

#[test]
fn an_occurrence_missed_by_more_than_the_grace_is_skipped_not_fired_late() {
    assert_eq!(
        due(T, Repeat::Once, true, None, T + RUN_GRACE_MS + 1),
        Due::Skip(T)
    );
    // A daily event after a 3-day outage: only the latest is considered,
    // and it is skipped; the next one runs on time.
    let now = T + 3 * DAY_MS + 7_200_000;
    assert_eq!(
        due(T, Repeat::Daily, true, Some(T), now),
        Due::Skip(T + 3 * DAY_MS)
    );
    assert_eq!(
        due(
            T,
            Repeat::Daily,
            true,
            Some(T + 3 * DAY_MS),
            T + 4 * DAY_MS + 10
        ),
        Due::Run(T + 4 * DAY_MS)
    );
}

#[test]
fn a_disabled_schedule_never_runs() {
    assert_eq!(due(T, Repeat::Daily, false, None, T), Due::Nothing);
}

#[test]
fn weekly_repeats_every_seven_days() {
    assert_eq!(next_occurrence(T, Repeat::Weekly, T), Some(T + 7 * DAY_MS));
    assert_eq!(
        latest_occurrence(T, Repeat::Weekly, T + 13 * DAY_MS),
        Some(T + 7 * DAY_MS)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    // The model: occurrences are at + k·period; the latest is the largest
    // at or before now, the next the smallest after it.
    #[test]
    fn occurrences_bracket_now(offset in 0i64..(60 * DAY_MS), weekly in any::<bool>()) {
        let repeat = if weekly { Repeat::Weekly } else { Repeat::Daily };
        let period = if weekly { 7 * DAY_MS } else { DAY_MS };
        let now = T + offset;
        let latest = latest_occurrence(T, repeat, now).unwrap();
        let next = next_occurrence(T, repeat, now).unwrap();
        prop_assert!(latest <= now && now < next);
        prop_assert_eq!(next - latest, period);
        prop_assert_eq!((latest - T) % period, 0);
    }

    // Driving the scheduler tick by tick runs each occurrence exactly once
    // when it never misses the grace window.
    #[test]
    fn each_occurrence_runs_once(tick_s in 5i64..60, days in 1i64..4) {
        let mut last = None;
        let mut runs = Vec::new();
        let mut now = T - 1_000;
        // Up to the end of the last occurrence's grace window.
        while now <= T + days * DAY_MS + RUN_GRACE_MS {
            match due(T, Repeat::Daily, true, last, now) {
                Due::Run(at) => { runs.push(at); last = Some(at); }
                Due::Skip(_) => prop_assert!(false, "a {tick_s} s tick never misses"),
                Due::Nothing => {}
            }
            now += tick_s * 1_000;
        }
        let expected: Vec<i64> = (0..=days).map(|k| T + k * DAY_MS).collect();
        prop_assert_eq!(runs, expected);
    }
}

#[tokio::test]
async fn past_occurrences_are_not_run_on_create() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    // A schedule references its pipeline.
    crate::db::create_pipeline(&pool, "p", "P", "key-p", None, None)
        .await
        .unwrap();
    let draft = ScheduleDraft {
        pipeline_id: "p".into(),
        name: "Morning show".into(),
        action: ScheduleAction::StartOutputs,
        output_ids: vec![],
        at_ms: T,
        repeat: Repeat::Daily,
        enabled: true,
    };
    let created = create(&pool, "s1", &draft, T + 2 * DAY_MS + 5)
        .await
        .unwrap();
    assert_eq!(
        created.last_run_ms,
        Some(T + 2 * DAY_MS),
        "today's already passed"
    );
    assert_eq!(created.next_at_ms, Some(T + 3 * DAY_MS));
    assert_eq!(
        due(
            created.at_ms,
            created.repeat,
            created.enabled,
            created.last_run_ms,
            T + 2 * DAY_MS + 30_000
        ),
        Due::Nothing
    );
    let file = ScheduleDraft {
        action: ScheduleAction::StartFile,
        output_ids: vec!["o".into()],
        ..draft
    };
    assert!(matches!(
        create(&pool, "s2", &file, T).await,
        Err(ServiceError::Conflict(_))
    ));
}
