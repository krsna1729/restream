use super::*;
use proptest::prelude::*;

fn brief(id: &str, severity: NotifySeverity) -> AlertBrief {
    AlertBrief {
        id: id.into(),
        severity,
        title: format!("{id} is down"),
        cause: "The platform closed the connection.".into(),
        recommended_action: "Check the stream key.".into(),
        pipeline_id: Some("pipe 1".into()),
        output_id: None,
    }
}

fn kinds(events: &[NotifyEvent]) -> Vec<(EventKind, &str)> {
    events
        .iter()
        .map(|event| (event.kind, event.alert.id.as_str()))
        .collect()
}

#[test]
fn alerts_open_at_startup_are_learned_not_announced() {
    let mut transitions = AlertTransitions::default();
    let a = brief("a", NotifySeverity::Critical);
    assert!(transitions.observe(std::slice::from_ref(&a), 0).is_empty());
    assert!(transitions.observe(std::slice::from_ref(&a), 1).is_empty());
    // It still resolves when it clears.
    assert!(transitions.observe(&[], 2).is_empty());
    assert_eq!(
        kinds(&transitions.observe(&[], 3)),
        [(EventKind::Resolved, "a")]
    );
}

#[test]
fn an_alert_is_announced_once_it_holds_and_resolved_once_it_clears() {
    let mut transitions = AlertTransitions::default();
    transitions.observe(&[], 0);
    let a = brief("a", NotifySeverity::Warning);
    assert!(
        transitions.observe(std::slice::from_ref(&a), 1).is_empty(),
        "one sample is not enough"
    );
    assert_eq!(
        kinds(&transitions.observe(std::slice::from_ref(&a), 2)),
        [(EventKind::Opened, "a")]
    );
    assert!(
        transitions.observe(std::slice::from_ref(&a), 3).is_empty(),
        "announced once"
    );
    assert!(transitions.observe(&[], 4).is_empty());
    assert_eq!(
        kinds(&transitions.observe(&[], 5)),
        [(EventKind::Resolved, "a")]
    );
    assert!(transitions.observe(&[], 6).is_empty());
}

#[test]
fn a_flicker_pages_nobody() {
    let mut transitions = AlertTransitions::default();
    transitions.observe(&[], 0);
    let a = brief("a", NotifySeverity::Critical);
    for at in 1..20 {
        let current = if at % 2 == 0 { vec![a.clone()] } else { vec![] };
        assert!(transitions.observe(&current, at).is_empty(), "at {at}");
    }
}

// The model: per alert, the last CONFIRM_TICKS observations decide whether
// it is (announced) open; events are exactly the edges of that signal.
proptest! {
    #[test]
    fn events_are_the_confirmed_edges_of_each_alert(
        presence in proptest::collection::vec(proptest::collection::vec(any::<bool>(), 3), 1..40)
    ) {
        let ids = ["a", "b", "c"];
        let mut transitions = AlertTransitions::default();
        transitions.observe(&[], 0);
        let mut announced = [false; 3];
        let mut run = [(false, 0u8); 3];
        for (tick, row) in presence.iter().enumerate() {
            let current: Vec<_> = ids
                .iter()
                .zip(row)
                .filter(|(_, present)| **present)
                .map(|(id, _)| brief(id, NotifySeverity::Warning))
                .collect();
            let events = transitions.observe(&current, tick as i64 + 1);
            let mut expected = Vec::new();
            for (index, present) in row.iter().enumerate() {
                run[index] = if run[index].0 == *present { (*present, run[index].1.saturating_add(1)) } else { (*present, 1) };
                if *present && !announced[index] && run[index].1 >= CONFIRM_TICKS {
                    announced[index] = true;
                    expected.push((EventKind::Opened, ids[index]));
                } else if !*present && announced[index] && run[index].1 >= CONFIRM_TICKS {
                    announced[index] = false;
                    expected.push((EventKind::Resolved, ids[index]));
                }
            }
            let mut got = kinds(&events);
            got.sort_by_key(|(kind, id)| (*id, kind.as_str()));
            expected.sort_by_key(|(kind, id)| (*id, kind.as_str()));
            prop_assert_eq!(got, expected, "tick {}", tick);
        }
    }
}

#[test]
fn recipients_get_their_severity_and_resolutions_only_if_asked() {
    let open_warning = NotifyEvent {
        kind: EventKind::Opened,
        at_ms: 0,
        alert: brief("a", NotifySeverity::Warning),
    };
    let resolved_critical = NotifyEvent {
        kind: EventKind::Resolved,
        at_ms: 0,
        alert: brief("a", NotifySeverity::Critical),
    };
    let test = NotifyEvent {
        kind: EventKind::Test,
        at_ms: 0,
        alert: brief("t", NotifySeverity::Warning),
    };
    assert!(wants(NotifySeverity::Warning, false, &open_warning));
    assert!(!wants(NotifySeverity::Critical, true, &open_warning));
    assert!(!wants(NotifySeverity::Warning, false, &resolved_critical));
    assert!(wants(NotifySeverity::Critical, true, &resolved_critical));
    assert!(wants(NotifySeverity::Critical, false, &test));
}

#[test]
fn retries_are_bounded_and_jittered_upward() {
    assert_eq!(retry_delay_ms(0, 0.0), Some(2_000));
    assert_eq!(retry_delay_ms(1, 0.999), Some(15_000 + 3_746));
    assert_eq!(retry_delay_ms(2, 0.0), None, "three tries, then give up");
}

#[test]
fn each_channel_gets_its_own_shape() {
    let event = NotifyEvent {
        kind: EventKind::Opened,
        at_ms: 7,
        alert: brief("a", NotifySeverity::Critical),
    };
    let url = Some("https://ops.example/restream/");
    let webhook = render_body(ChannelKind::Webhook, &event, url);
    assert_eq!(webhook["schema"], "restream.notify.v1");
    assert_eq!(webhook["event"], "opened");
    assert_eq!(webhook["alert"]["severity"], "critical");
    assert_eq!(
        webhook["link"],
        "https://ops.example/restream/?w=incidents&p=pipe%201"
    );
    let slack = render_body(ChannelKind::Slack, &event, url);
    assert!(
        slack["text"]
            .as_str()
            .unwrap()
            .starts_with("*[CRITICAL] a is down*")
    );
    assert!(
        slack["text"]
            .as_str()
            .unwrap()
            .contains("Next: Check the stream key.")
    );
    let discord = render_body(ChannelKind::Discord, &event, None);
    assert_eq!(discord["embeds"][0]["color"], 0xd1242f);
    assert!(discord["embeds"][0]["url"].is_null());
    let resolved = NotifyEvent {
        kind: EventKind::Resolved,
        ..event
    };
    assert_eq!(push_payload(&resolved)["title"], "[RESOLVED] a is down");
}

#[test]
fn push_endpoints_must_be_public_https_names() {
    assert!(valid_push_endpoint(
        "https://fcm.googleapis.com/fcm/send/abc"
    ));
    assert!(valid_push_endpoint(
        "https://updates.push.services.mozilla.com/wpush/v2/x"
    ));
    for bad in [
        "http://fcm.googleapis.com/x",
        "https://127.0.0.1/x",
        "https://[::1]/x",
        "https://localhost:8080/x",
        "https://metadata/x",
        "https://user@printer.local/x",
        "https:///x",
    ] {
        assert!(!valid_push_endpoint(bad), "{bad}");
    }
    assert!(valid_channel_target(
        "https://hooks.slack.com/services/T/B/x"
    ));
    assert!(valid_channel_target("http://10.0.0.5:9000/hook"));
    assert!(!valid_channel_target("ftp://x/y"));
    assert!(!valid_channel_target("https://"));
}

#[test]
fn webhook_signatures_are_hmac_sha256_of_timestamp_dot_body() {
    // Independently: python3 hmac.new(b"whsec_test", b'1700000000.{"a":1}', sha256)
    assert_eq!(
        webhook_signature("whsec_test", 1_700_000_000, br#"{"a":1}"#),
        "t=1700000000,v1=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789"
    );
}
