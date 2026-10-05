use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use proptest::prelude::*;

use super::*;
use crate::media::hls::{HlsSegmentSnapshot, HlsStoreSnapshot};

fn snapshot(indices: std::ops::Range<u64>, duration: f64) -> HlsStoreSnapshot {
    HlsStoreSnapshot {
        playlist: String::new(),
        segments: indices
            .map(|index| HlsSegmentSnapshot {
                index,
                duration,
                data: Bytes::from(format!("store-{index}")),
            })
            .collect(),
    }
}

fn put(policy: &mut UploadPolicy, now: Instant) -> UploadRequest {
    match policy.next(now) {
        Next::Put(request) => request,
        other => panic!("expected a request, got {other:?}"),
    }
}

fn ok(policy: &mut UploadPolicy, now: Instant) {
    policy.on_result(UploadOutcome::Status(200), now);
}

fn listed(playlist: &[u8]) -> (u64, Vec<String>) {
    let text = std::str::from_utf8(playlist).unwrap();
    let sequence = text
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .unwrap()
        .parse()
        .unwrap();
    let names = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(str::to_string)
        .collect();
    (sequence, names)
}

/// A late output starts at the newest segment, numbered 0 (YouTube's first
/// playlist starts at sequence 0), then alternates segment and playlist.
#[test]
fn starts_at_the_newest_segment_and_alternates_segment_then_playlist() {
    let now = Instant::now();
    let mut policy = UploadPolicy::new("r1".into());
    policy.on_publish(&snapshot(40..45, 2.0));

    let segment = put(&mut policy, now);
    assert_eq!(segment.file_name, "r1-0.ts");
    assert_eq!(segment.body, Bytes::from("store-44"));
    assert_eq!(segment.content_type, HLS_SEGMENT_CONTENT_TYPE);
    assert_eq!(policy.next(now), Next::Wait(None), "one request at a time");
    ok(&mut policy, now);

    let playlist = put(&mut policy, now);
    assert_eq!(playlist.content_type, HLS_PLAYLIST_CONTENT_TYPE);
    assert_eq!(listed(&playlist.body), (0, vec!["r1-0.ts".to_string()]));
    ok(&mut policy, now);
    assert_eq!(policy.next(now), Next::Wait(None));

    policy.on_publish(&snapshot(41..47, 2.0));
    assert_eq!(put(&mut policy, now).file_name, "r1-1.ts");
    ok(&mut policy, now);
    assert_eq!(
        listed(&put(&mut policy, now).body).1,
        ["r1-0.ts", "r1-1.ts"]
    );
    ok(&mut policy, now);
    assert_eq!(put(&mut policy, now).file_name, "r1-2.ts", "index 46");
}

/// Client errors stop the output (Akamai: do not retry 400/403; YouTube
/// 401: expired cid); request timeouts and rate limits retry.
#[test]
fn client_errors_stop_and_server_errors_retry() {
    for status in [400, 401, 403, 404, 405] {
        let now = Instant::now();
        let mut policy = UploadPolicy::new("r".into());
        policy.on_publish(&snapshot(0..1, 2.0));
        put(&mut policy, now);
        assert_eq!(
            policy.on_result(UploadOutcome::Status(status), now),
            Some(ResultEffect::Rejected { status })
        );
        assert_eq!(policy.next(now), Next::Stop(Stopped::Rejected { status }));
    }
    for outcome in [
        UploadOutcome::Status(500),
        UploadOutcome::Status(503),
        UploadOutcome::Status(408),
        UploadOutcome::Status(429),
        UploadOutcome::Transport,
    ] {
        let now = Instant::now();
        let mut policy = UploadPolicy::new("r".into());
        policy.on_publish(&snapshot(0..1, 2.0));
        put(&mut policy, now);
        assert_eq!(
            policy.on_result(outcome, now),
            Some(ResultEffect::WillRetry { attempts: 1 }),
            "{outcome:?}"
        );
        assert!(matches!(policy.next(now), Next::Wait(Some(_))));
        let retry = put(&mut policy, now + Duration::from_millis(200));
        assert!(retry.fresh_connection, "a retry uses a new connection");
        assert_eq!(retry.file_name, "r-0.ts");
    }
}

/// Backoff doubles from 200 ms and stops growing at 5 s.
#[test]
fn backoff_doubles_up_to_its_cap() {
    let waits: Vec<_> = (1..=8).map(backoff).collect();
    assert_eq!(waits[0], Duration::from_millis(200));
    assert_eq!(waits[1], Duration::from_millis(400));
    assert_eq!(waits[4], Duration::from_millis(3_200));
    assert_eq!(waits[5], BACKOFF_MAX);
    assert_eq!(waits[7], BACKOFF_MAX);
}

/// A segment still failing one segment duration after its first send is
/// dropped (Akamai), and no later playlist lists it.
#[test]
fn a_segment_failing_past_its_duration_is_dropped_and_never_listed() {
    let start = Instant::now();
    let mut policy = UploadPolicy::new("r".into());
    policy.on_publish(&snapshot(0..1, 2.0));
    put(&mut policy, start);
    ok(&mut policy, start);
    put(&mut policy, start); // playlist [0]
    ok(&mut policy, start);

    policy.on_publish(&snapshot(0..3, 2.0));
    let mut now = start;
    loop {
        match policy.next(now) {
            Next::Put(request) if request.file_name == "r-1.ts" => {
                policy.on_result(UploadOutcome::Status(503), now);
            }
            Next::Put(request) => {
                assert_eq!(request.file_name, "r-2.ts");
                break;
            }
            Next::Wait(Some(until)) => now = until,
            other => panic!("{other:?}"),
        }
        assert!(now < start + Duration::from_secs(10), "never dropped");
    }
    assert_eq!(policy.dropped_segments(), 1);
    assert!(
        now >= start + Duration::from_secs(2),
        "kept for its duration"
    );
    ok(&mut policy, now);
    let (sequence, names) = listed(&put(&mut policy, now).body);
    assert_eq!((sequence, names), (2, vec!["r-2.ts".to_string()]));
}

/// A store whose indices went backwards was cleared: its segments are all
/// new, and the output's numbering continues.
#[test]
fn a_cleared_store_continues_the_output_numbering() {
    let now = Instant::now();
    let mut policy = UploadPolicy::new("r".into());
    policy.on_publish(&snapshot(7..8, 2.0));
    put(&mut policy, now);
    ok(&mut policy, now);
    put(&mut policy, now);
    ok(&mut policy, now);

    policy.on_publish(&snapshot(0..2, 2.0));
    let first = put(&mut policy, now);
    assert_eq!(
        (first.file_name.as_str(), first.body),
        ("r-1.ts", Bytes::from("store-0"))
    );
}

/// An intentional stop sends one final playlist with EXT-X-ENDLIST; its
/// failure is not retried.
#[test]
fn finish_sends_one_end_playlist() {
    for outcome in [UploadOutcome::Status(200), UploadOutcome::Status(503)] {
        let now = Instant::now();
        let mut policy = UploadPolicy::new("r".into());
        policy.on_publish(&snapshot(0..1, 2.0));
        put(&mut policy, now);
        ok(&mut policy, now);
        put(&mut policy, now);
        ok(&mut policy, now);
        policy.on_publish(&snapshot(0..2, 2.0));

        policy.finish();
        let end = put(&mut policy, now);
        let text = std::str::from_utf8(&end.body).unwrap();
        assert!(text.ends_with("#EXT-X-ENDLIST\n"), "{text}");
        assert!(!text.contains("r-1.ts"), "unsent segments are not listed");
        let effect = policy.on_result(outcome, now);
        if outcome == UploadOutcome::Status(503) {
            assert_eq!(effect, Some(ResultEffect::EndNotDelivered));
        }
        assert_eq!(policy.next(now), Next::Stop(Stopped::Ended));
    }
}

/// A stop during a retry backoff sends the end playlist at once.
#[test]
fn finish_during_a_backoff_does_not_wait_for_it() {
    let now = Instant::now();
    let mut policy = UploadPolicy::new("r".into());
    policy.on_publish(&snapshot(0..1, 2.0));
    put(&mut policy, now);
    ok(&mut policy, now);
    put(&mut policy, now);
    policy.on_result(UploadOutcome::Status(503), now);
    assert!(matches!(policy.next(now), Next::Wait(Some(_))));

    policy.finish();
    let end = put(&mut policy, now);
    assert!(matches!(
        end.target,
        UploadTarget::Playlist { end: true, .. }
    ));
}

/// A destination failing everything (a playlist retried without limit
/// blocks every segment behind it) holds a bounded backlog, not the whole
/// continuing stream: one failing destination must not exhaust shared
/// memory.
#[test]
fn a_failing_playlist_cannot_pin_an_unbounded_backlog() {
    let mut now = Instant::now();
    let mut policy = UploadPolicy::new("r".into());
    policy.on_publish(&snapshot(0..1, 2.0));
    put(&mut policy, now);
    ok(&mut policy, now);
    for store_next in 2..60u64 {
        policy.on_publish(&snapshot(store_next.saturating_sub(20)..store_next, 2.0));
        match policy.next(now) {
            Next::Put(_) => {
                // A destination failing everything.
                policy.on_result(UploadOutcome::Status(503), now);
            }
            Next::Wait(Some(until)) => now = until,
            other => panic!("{other:?}"),
        }
        assert!(policy.pending_segments() <= MAX_PENDING_SEGMENTS);
    }
    assert!(
        policy.dropped_segments() >= 50,
        "older segments were dropped"
    );
}

#[derive(Debug, Clone)]
enum Event {
    Publish { added: u64, duration_tenths: u16 },
    Result(UploadOutcome),
    Advance { millis: u64 },
    Finish,
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        30 => (1u64..4, 5u16..60).prop_map(|(added, duration_tenths)| Event::Publish {
            added,
            duration_tenths,
        }),
        60 => prop_oneof![
            4 => Just(UploadOutcome::Status(200)),
            1 => Just(UploadOutcome::Status(202)),
            2 => Just(UploadOutcome::Status(503)),
            2 => Just(UploadOutcome::Transport),
            1 => Just(UploadOutcome::Status(429)),
        ]
        .prop_map(Event::Result),
        30 => (0u64..3_000).prop_map(|millis| Event::Advance { millis }),
        1 => Just(Event::Finish),
    ]
}

proptest! {
    /// For any interleaving of publishes, results and time, what a server
    /// sees follows the ingest rules: every playlist lists only
    /// acknowledged segments, contiguous, at most PLAYLIST_WINDOW of them,
    /// with a media sequence that never decreases; segments go out in
    /// sequence order, each acknowledged at most once, never sent past its
    /// retry window; nothing follows a stop.
    #[test]
    fn what_the_server_sees_follows_the_ingest_rules(events in prop::collection::vec(event(), 1..200)) {
        let mut now = Instant::now();
        let mut policy = UploadPolicy::new("p".into());
        let mut store_next = 0u64;
        let mut in_flight: Option<UploadRequest> = None;
        let mut acknowledged = BTreeSet::new();
        let mut first_send = std::collections::HashMap::<String, Instant>::new();
        let mut last_media_sequence = 0u64;
        let mut last_target_duration = 0u64;
        let mut highest_sent: Option<u64> = None;
        let mut stopped = false;

        for event in events {
            match event {
                Event::Publish { added, duration_tenths } => {
                    store_next += added;
                    let duration = f64::from(duration_tenths) / 10.0;
                    policy.on_publish(&snapshot(store_next.saturating_sub(20)..store_next, duration));
                    prop_assert!(policy.pending_segments() <= MAX_PENDING_SEGMENTS, "unbounded backlog");
                }
                Event::Advance { millis } => now += Duration::from_millis(millis),
                Event::Finish => policy.finish(),
                Event::Result(outcome) => {
                    if let Some(request) = in_flight.take() {
                        let effect = policy.on_result(outcome, now);
                        prop_assert!(effect.is_some());
                        if matches!(effect, Some(ResultEffect::Acknowledged { .. }))
                            && let UploadTarget::Segment { sequence } = request.target
                        {
                            prop_assert!(acknowledged.insert(sequence), "acknowledged twice");
                        }
                    } else {
                        prop_assert_eq!(policy.on_result(outcome, now), None);
                    }
                }
            }
            if in_flight.is_some() {
                continue;
            }
            match policy.next(now) {
                Next::Put(request) => {
                    prop_assert!(!stopped, "a request after a stop");
                    match &request.target {
                        UploadTarget::Segment { sequence } => {
                            prop_assert!(!acknowledged.contains(sequence));
                            if let Some(highest) = highest_sent {
                                prop_assert!(*sequence >= highest, "out of order");
                            }
                            highest_sent = Some(*sequence);
                            let first = *first_send.entry(request.file_name.clone()).or_insert(now);
                            prop_assert!(now < first + Duration::from_secs(7), "sent past its window");
                        }
                        UploadTarget::Playlist { .. } => {
                            let (sequence, names) = listed(&request.body);
                            let text = std::str::from_utf8(&request.body).unwrap();
                            let target: u64 = text
                                .lines()
                                .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
                                .unwrap()
                                .parse()
                                .unwrap();
                            prop_assert!(target >= last_target_duration, "target duration decreased");
                            last_target_duration = target;
                            for extinf in text.lines().filter_map(|line| line.strip_prefix("#EXTINF:")) {
                                let duration: f64 = extinf.trim_end_matches(',').parse().unwrap();
                                prop_assert!(duration.ceil() as u64 <= target, "EXTINF above target");
                            }
                            prop_assert!(names.len() <= PLAYLIST_WINDOW);
                            prop_assert!(sequence >= last_media_sequence, "media sequence went back");
                            last_media_sequence = sequence;
                            for (offset, name) in names.iter().enumerate() {
                                let listed_sequence = sequence + offset as u64;
                                prop_assert_eq!(name, &format!("p-{listed_sequence}.ts"), "not contiguous");
                                prop_assert!(acknowledged.contains(&listed_sequence), "lists an outstanding segment");
                            }
                        }
                    }
                    in_flight = Some(request);
                }
                Next::Wait(Some(until)) => prop_assert!(until > now),
                Next::Wait(None) => {}
                Next::Stop(_) => stopped = true,
            }
        }
    }
}
