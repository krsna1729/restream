use super::*;

fn spec(input: &str, url: &str) -> SrtCallSpec {
    SrtCallSpec {
        pipeline_id: "p1".to_string(),
        input_id: input.to_string(),
        stream_key: format!("{input}-key"),
        selected: true,
        url: url.to_string(),
    }
}

type Calls = CallSupervisor<u64>;

fn caller_peer(raw: u64) -> u64 {
    raw
}

/// One call taken through resolution to the Owner command.
async fn dialed(calls: &mut Calls, now: Instant) -> CallToken {
    let resolved = calls.resolved.recv().await.expect("a resolution");
    let token = resolved.0;
    match calls.resolved(resolved, now) {
        Some(IngressCommand::Call { remote, .. }) => {
            assert_eq!(remote, "127.0.0.1:9000".parse().unwrap());
        }
        _ => panic!("a resolved call is dialled"),
    }
    token
}

#[test]
fn converge_reports_only_new_or_changed_calls() {
    let inputs = SrtCallInputs::default();
    let a = spec("a", "srt://127.0.0.1:9000");
    assert_eq!(inputs.converge(vec![a.clone()]), ["a"]);
    assert!(
        inputs.converge(vec![a.clone()]).is_empty(),
        "unchanged rows start nothing"
    );
    let changed = spec("a", "srt://127.0.0.1:9001");
    assert_eq!(inputs.converge(vec![changed]), ["a"]);
    assert!(
        inputs.converge(Vec::new()).is_empty(),
        "a removal starts nothing"
    );
}

#[test]
fn call_urls_need_srt_host_and_port() {
    assert!(validate_srt_call_url("srt://encoder.example:9000?streamid=cam1").is_ok());
    assert!(validate_srt_call_url("srt://[::1]:9000").is_ok());
    for bad in [
        "udp://encoder:9000",
        "srt://encoder",
        "srt://:9000",
        "srt://encoder:0",
        "srt://encoder:99999",
        "srt://encoder:9000?latency=900000",
    ] {
        assert!(validate_srt_call_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn backoff_doubles_to_its_cap() {
    assert_eq!(backoff(1), Duration::from_secs(1));
    assert_eq!(backoff(2), Duration::from_secs(2));
    assert_eq!(backoff(5), Duration::from_secs(16));
    assert_eq!(backoff(6), BACKOFF_MAX);
    assert_eq!(backoff(u32::MAX), BACKOFF_MAX);
}

#[tokio::test]
async fn a_failed_call_redials_after_its_backoff_and_a_connected_one_publishes() {
    let inputs = SrtCallInputs::default();
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000?streamid=cam")]);
    let mut calls = Calls::new();
    let t0 = Instant::now();
    calls.tick(&inputs, t0, |_| true);
    let first = dialed(&mut calls, t0).await;

    calls.failed(first, "refused", t0);
    calls.tick(&inputs, t0 + Duration::from_millis(900), |_| true);
    assert!(
        calls.resolved.try_recv().is_err(),
        "no redial inside the backoff"
    );
    calls.tick(&inputs, t0 + Duration::from_secs(1), |_| true);
    let second = dialed(&mut calls, t0).await;
    assert_ne!(first, second, "each attempt has its own token");
    calls.failed(first, "late verdict for the old attempt", t0);

    let peer = caller_peer(7);
    assert!(calls.started(second, peer));
    assert!(calls.owns(&peer));
    let publisher = calls.connected(peer).expect("the call publishes");
    assert_eq!(publisher.pipeline.input_id, "a");
    assert_eq!(publisher.stream_key, "a-key");
    assert!(publisher.pipeline.selected);
}

#[tokio::test]
async fn an_ended_call_redials_promptly_and_an_unwanted_one_is_hung_up() {
    let inputs = SrtCallInputs::default();
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000")]);
    let mut calls = Calls::new();
    let t0 = Instant::now();
    calls.tick(&inputs, t0, |_| true);
    let token = dialed(&mut calls, t0).await;
    let peer = caller_peer(1);
    calls.started(token, peer);
    calls.connected(peer).expect("publishing");

    // A call that ran redials after the first backoff step, not a long one.
    calls.disconnected(peer, "listener closed", t0);
    assert!(!calls.owns(&peer));
    calls.tick(&inputs, t0 + Duration::from_secs(1), |_| true);
    let token = dialed(&mut calls, t0).await;
    let peer = caller_peer(2);
    calls.started(token, peer);

    // The row goes away: the live session is hung up.
    inputs.converge(Vec::new());
    let mut hung_up = Vec::new();
    calls.tick(&inputs, t0, |peer| {
        hung_up.push(peer);
        true
    });
    assert_eq!(hung_up, [peer]);
    assert!(!calls.owns(&peer));
    assert!(calls.connected(peer).is_none());
}

#[tokio::test]
async fn a_call_admitted_after_its_row_left_is_refused() {
    let inputs = SrtCallInputs::default();
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000")]);
    let mut calls = Calls::new();
    let t0 = Instant::now();
    calls.tick(&inputs, t0, |_| true);
    let token = dialed(&mut calls, t0).await;
    inputs.converge(Vec::new());
    calls.tick(&inputs, t0, |_| true);
    assert!(
        !calls.started(token, caller_peer(3)),
        "the pool's late admission is hung up, not published"
    );
}

#[tokio::test]
async fn a_busy_command_slot_defers_the_hang_up_to_the_next_tick() {
    let inputs = SrtCallInputs::default();
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000")]);
    let mut calls = Calls::new();
    let t0 = Instant::now();
    calls.tick(&inputs, t0, |_| true);
    let token = dialed(&mut calls, t0).await;
    let peer = caller_peer(4);
    calls.started(token, peer);
    inputs.converge(Vec::new());
    calls.tick(&inputs, t0, |_| false);
    assert!(calls.owns(&peer), "nothing changes while the slot is taken");
    let mut hung_up = Vec::new();
    calls.tick(&inputs, t0, |peer| {
        hung_up.push(peer);
        true
    });
    assert_eq!(hung_up, [peer]);
}

#[tokio::test]
async fn a_replaced_calls_late_admission_is_refused_while_its_successor_dials() {
    let inputs = SrtCallInputs::default();
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000")]);
    let mut calls = Calls::new();
    let t0 = Instant::now();
    calls.tick(&inputs, t0, |_| true);
    let old = dialed(&mut calls, t0).await;
    // The row changes while the first dial is still in the pool's queue.
    inputs.converge(vec![spec("a", "srt://127.0.0.1:9000?streamid=new")]);
    calls.tick(&inputs, t0, |_| true);
    let new = dialed(&mut calls, t0).await;
    assert!(
        !calls.started(old, caller_peer(5)),
        "the old attempt is hung up"
    );
    assert!(!calls.owns(&caller_peer(5)));
    assert!(calls.started(new, caller_peer(6)));
}

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

#[test]
fn calls_prefer_ipv4_and_dial_ipv6_only_listeners_over_ipv6() {
    let dual = vec![addr("[2001:db8::1]:9000"), addr("192.0.2.1:9000")];
    assert_eq!(
        place(std::slice::from_ref(&dual)),
        Ok(vec![addr("192.0.2.1:9000")])
    );
    let v6 = vec![addr("[2001:db8::1]:9000")];
    assert_eq!(place(std::slice::from_ref(&v6)), Ok(v6.clone()));
    // A bond: one leg v4-only, one dual-stack -> both legs over IPv4.
    let v4 = vec![addr("192.0.2.2:9001")];
    assert_eq!(
        place(&[v4.clone(), dual]),
        Ok(vec![addr("192.0.2.2:9001"), addr("192.0.2.1:9000")])
    );
    assert!(place(&[v4, v6]).is_err(), "no shared family");
}
