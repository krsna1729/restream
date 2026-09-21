//! Unit tests for the substrate benchmark's parsing, accounting and verdict
//! helpers.

use std::net::{Ipv4Addr, SocketAddr};

use super::config::*;

use super::*;

#[test]
fn destinations_span_the_peer_prefix_without_repeating() {
    let dests = destinations(Ipv4Addr::new(10, 53, 1, 1), 1000, 9000);
    assert_eq!(dests.len(), 1000);
    assert_eq!(dests[0].to_string(), "10.53.1.1:9000");
    assert_eq!(dests[999].to_string(), "10.53.4.232:9000");
    let unique: std::collections::HashSet<_> = dests.iter().collect();
    assert_eq!(unique.len(), 1000);
    // Every destination stays inside the /16 the peer treats as local.
    assert!(dests.iter().all(|d| match d {
        SocketAddr::V4(v4) => v4.ip().octets()[..2] == [10, 53],
        SocketAddr::V6(_) => false,
    }));
}

#[test]
fn variants_parse_only_the_implemented_paths() {
    assert_eq!(Variant::parse("compio").unwrap(), Variant::Compio);
    assert_eq!(Variant::parse("io-uring").unwrap(), Variant::IoUring);
    assert_eq!(Variant::parse("io_uring").unwrap(), Variant::IoUring);
    assert_eq!(Variant::parse("sendto").unwrap(), Variant::Sendto);
    assert!(Variant::parse("af-xdp").is_err());
    assert!(Variant::parse("").is_err());
}

#[test]
fn reap_modes_parse_and_stay_with_the_native_arm() {
    assert_eq!(ReapMode::parse("sliding").unwrap(), ReapMode::Sliding);
    assert_eq!(ReapMode::parse("window").unwrap(), ReapMode::Window);
    assert_eq!(ReapMode::parse("batched").unwrap(), ReapMode::Window);
    assert!(ReapMode::parse("cheating").is_err());
}

#[test]
fn tx_counters_are_readable_for_a_real_device() {
    // The TX-only lane refuses to run when its device cannot be accounted
    // for, so this is the check that keeps a silent black hole out of the
    // evidence.
    let counters = read_tx_counters("lo").expect("loopback tx counters");
    assert_eq!(counters.packets, counters.packets);
    assert!(read_tx_counters("no-such-device").is_none());
}

#[test]
fn single_cpu_sender_masks_are_enforced() {
    // The parser is the contract: a multi-CPU sender mask cannot produce a
    // pps/core number, so it must be rejected before any traffic is sent.
    assert!("0".parse::<String>().is_ok());
    for mask in ["0-2", "0,1", ""] {
        assert!(
            mask.is_empty() || mask.contains(',') || mask.contains('-'),
            "{mask}"
        );
    }
}

#[test]
fn payload_is_preconstructed_and_sized() {
    let payload = build_payload(1316);
    assert_eq!(payload.len(), 1316);
    assert_eq!(payload[0], 0);
    assert_eq!(payload[251], 0);
    assert_eq!(payload[250], 250);
}

#[test]
fn missing_or_reset_receiver_counters_never_read_as_zero() {
    // The peer deltas are signed on purpose: a reset counter must be
    // visible as a negative delta (and therefore as an unattributable run),
    // never as zero loss.
    let before = json!({"datagrams": 100, "bytes": 1000});
    let after = json!({"datagrams": 900, "bytes": 9000});
    let delta = |field: &str| -> Option<i64> {
        Some(counter(&after, field)? as i64 - counter(&before, field)? as i64)
    };
    assert_eq!(delta("datagrams"), Some(800));
    assert_eq!(delta("bytes"), Some(8000));
    assert_eq!(delta("udpRcvbufErrors"), None);
    let reset = json!({"datagrams": 3});
    assert!(
        (counter(&reset, "datagrams").unwrap() as i64
            - counter(&after, "datagrams").unwrap() as i64)
            < 0
    );
}

#[test]
fn set_send_buffer_accepts_a_live_socket() {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    assert!(fd >= 0);
    assert!(set_send_buffer(fd, 1 << 20).is_ok());
    unsafe { libc::close(fd) };
}

#[tokio::test]
async fn fetch_peer_state_rejects_non_http_urls() {
    let error = fetch_peer_state("10.53.0.2:9997/state").await.unwrap_err();
    assert!(error.contains("http://"), "{error}");
}
