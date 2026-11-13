use super::fake::FakeDns;
use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};

fn submit(
    resolver: &Resolver,
    host: &str,
    timeout: Duration,
    answers: &flume::Sender<(String, Resolution)>,
) {
    let (answers, name) = (answers.clone(), host.to_string());
    resolver.submit(host, 80, Instant::now() + timeout, move |resolution| {
        let _ = answers.send((name, resolution));
    });
}

fn localhost(port: u16) -> Vec<SocketAddr> {
    vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)]
}

/// One name whose server hangs holds one pool thread: another name still
/// resolves at once.
#[test]
fn a_hung_name_never_delays_another_name() {
    let dns = FakeDns::new();
    let (answers, answered) = flume::unbounded();
    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_secs(30),
        &answers,
    );
    dns.wait_for_calls(1);

    let started = Instant::now();
    submit(
        &dns.resolver,
        "other.test",
        Duration::from_secs(30),
        &answers,
    );
    let (name, resolution) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("the other name is answered while the hung one runs");
    assert_eq!(name, "other.test");
    assert_eq!(resolution, Ok(localhost(80)));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

/// The caller hears back at its deadline while `getaddrinfo` still runs.
#[test]
fn a_request_is_answered_at_its_deadline_while_the_lookup_hangs() {
    let dns = FakeDns::new();
    let (answers, answered) = flume::unbounded();
    let started = Instant::now();
    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_millis(200),
        &answers,
    );
    let (_, resolution) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered at the deadline");
    assert_eq!(resolution, Err(ResolveError::TimedOut));
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(200) && waited < Duration::from_secs(1),
        "{waited:?}"
    );
    assert_eq!(dns.resolver.stats().timeouts, 1);
}

/// A full queue answers at once, counted; nothing goes unanswered.
#[test]
fn every_request_is_answered_when_the_queue_overflows() {
    let dns = FakeDns::with_pool(1, 2);
    let (answers, answered) = flume::unbounded();
    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_millis(300),
        &answers,
    );
    dns.wait_for_calls(1);
    let total = 10;
    for index in 0..total {
        submit(
            &dns.resolver,
            &format!("name-{index}.test"),
            Duration::from_millis(300),
            &answers,
        );
    }
    let overflowed = answered
        .drain()
        .filter(|(_, resolution)| *resolution == Err(ResolveError::Overflow))
        .count();
    assert_eq!(overflowed, total - 2, "two names fit the queue");
    assert_eq!(dns.resolver.stats().overflows, (total - 2) as u64);

    // The queued names and the hung one time out: one answer each.
    let rest: Vec<_> = (0..3)
        .map(|_| {
            answered
                .recv_timeout(Duration::from_secs(2))
                .expect("answered")
        })
        .collect();
    assert!(
        rest.iter()
            .all(|(_, resolution)| *resolution == Err(ResolveError::TimedOut)),
        "{rest:?}"
    );
    assert!(answered.recv_timeout(Duration::from_millis(100)).is_err());
}

/// Concurrent requests for one name share one lookup, and a request made
/// after the others timed out joins the lookup still running.
#[test]
fn concurrent_lookups_of_one_name_share_one_call() {
    let dns = FakeDns::new();
    let (answers, answered) = flume::unbounded();
    for _ in 0..8 {
        submit(&dns.resolver, "slow.test", Duration::from_secs(5), &answers);
    }
    for _ in 0..8 {
        let (_, resolution) = answered
            .recv_timeout(Duration::from_secs(2))
            .expect("answered");
        assert_eq!(resolution, Ok(localhost(80)));
    }
    assert_eq!(dns.calls(), 1);

    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_millis(50),
        &answers,
    );
    let (_, first) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered");
    assert_eq!(first, Err(ResolveError::TimedOut));
    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_millis(50),
        &answers,
    );
    let (_, retry) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered");
    assert_eq!(retry, Err(ResolveError::TimedOut));
    assert_eq!(dns.calls(), 2, "the retry joined the running lookup");
    assert_eq!(dns.resolver.stats().lookups, 2);
}

/// A panicking lookup fails that name only, counted; the thread serves the
/// next name.
#[test]
fn a_panicking_lookup_fails_only_that_request() {
    let dns = FakeDns::with_pool(1, 8);
    let (answers, answered) = flume::unbounded();
    let before = crate::panic_boundary::contained_panics();
    submit(
        &dns.resolver,
        "panic.test",
        Duration::from_secs(5),
        &answers,
    );
    submit(
        &dns.resolver,
        "after.test",
        Duration::from_secs(5),
        &answers,
    );
    let (_, panicked) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered");
    assert!(
        matches!(panicked, Err(ResolveError::Failed(_))),
        "{panicked:?}"
    );
    let (_, after) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered");
    assert_eq!(after, Ok(localhost(80)));
    assert!(crate::panic_boundary::contained_panics() > before);
}

/// Dropping the resolver answers whoever still waits.
#[test]
fn dropping_the_resolver_answers_waiting_requests() {
    let dns = FakeDns::new();
    let (answers, answered) = flume::unbounded();
    submit(
        &dns.resolver,
        "hung.test",
        Duration::from_secs(30),
        &answers,
    );
    dns.wait_for_calls(1);
    drop(dns);
    let (_, resolution) = answered
        .recv_timeout(Duration::from_secs(2))
        .expect("answered");
    assert_eq!(resolution, Err(ResolveError::Unavailable));
}

#[tokio::test]
async fn async_callers_await_the_answer_and_literals_skip_the_pool() {
    let dns = FakeDns::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    assert_eq!(
        dns.resolver.resolve("name.test", 443, deadline).await,
        Ok(localhost(443))
    );
    assert_eq!(
        dns.resolver.resolve("fail.test", 443, deadline).await,
        Err(ResolveError::Failed("no such name".to_string()))
    );
    assert_eq!(
        dns.resolver.resolve("[::1]", 9000, deadline).await,
        Ok(vec![SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9000)])
    );
    assert_eq!(
        dns.resolver
            .resolve("10.0.0.1", 1, deadline)
            .await
            .map(|a| a.len()),
        Ok(1)
    );
    assert_eq!(dns.calls(), 2);
}

#[test]
fn host_port_splits_like_the_system_parser() {
    assert_eq!(split_host_port("relay.test:9000"), Ok(("relay.test", 9000)));
    assert_eq!(split_host_port("[::1]:9000"), Ok(("[::1]", 9000)));
    assert!(split_host_port("relay.test").is_err());
    assert!(split_host_port("relay.test:port").is_err());
}
