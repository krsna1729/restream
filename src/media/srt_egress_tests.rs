use super::*;
use std::net::SocketAddr;

#[test]
fn to_libc_sockaddr_v4_encodes_family_port_and_address_correctly() {
    let addr: SocketAddr = "203.0.113.5:4433".parse().unwrap();
    let (storage, len) = to_libc_sockaddr(addr);
    assert_eq!(len, std::mem::size_of::<libc::sockaddr_in>() as c_int);
    // SAFETY: to_libc_sockaddr wrote a valid sockaddr_in into storage
    // for a V4 address; reading it back through the same cast pattern
    // the production code uses to write it is sound.
    unsafe {
        let sin = &storage as *const _ as *const libc::sockaddr_in;
        assert_eq!((*sin).sin_family, libc::AF_INET as libc::sa_family_t);
        assert_eq!(u16::from_be((*sin).sin_port), 4433);
        assert_eq!((*sin).sin_addr.s_addr.to_ne_bytes(), [203, 0, 113, 5]);
    }
}

#[test]
fn to_libc_sockaddr_v6_encodes_family_port_and_address_correctly() {
    let addr: SocketAddr = "[2001:db8::1]:9000".parse().unwrap();
    let (storage, len) = to_libc_sockaddr(addr);
    assert_eq!(len, std::mem::size_of::<libc::sockaddr_in6>() as c_int);
    let expected_octets = match addr.ip() {
        std::net::IpAddr::V6(v6) => v6.octets(),
        std::net::IpAddr::V4(_) => unreachable!(),
    };
    // SAFETY: to_libc_sockaddr wrote a valid sockaddr_in6 into storage
    // for a V6 address; reading it back mirrors the write-side cast.
    unsafe {
        let sin6 = &storage as *const _ as *const libc::sockaddr_in6;
        assert_eq!((*sin6).sin6_family, libc::AF_INET6 as libc::sa_family_t);
        assert_eq!(u16::from_be((*sin6).sin6_port), 9000);
        assert_eq!((*sin6).sin6_addr.s6_addr, expected_octets);
    }
}

#[tokio::test]
async fn resolve_host_parses_ip_port_on_fast_path_without_dns() {
    let resolved = resolve_host("127.0.0.1:9999").await;
    assert_eq!(resolved, Some("127.0.0.1:9999".parse().unwrap()));
}

#[tokio::test]
async fn resolve_host_returns_none_for_host_string_missing_port() {
    // No ':' separator means ToSocketAddrs rejects the string before
    // any DNS lookup is attempted, so this stays deterministic and
    // network-free.
    let resolved = resolve_host("not-a-valid-host-string").await;
    assert_eq!(resolved, None);
}

#[test]
fn srt_egress_muxer_port_claim_serializes_first_port_selection() {
    let state = std::sync::Mutex::new(None);

    let first_claim = claim_srt_egress_muxer_port(&state);
    assert_eq!(first_claim.bind_port(), None);
    assert!(
        state.try_lock().is_err(),
        "first connector must hold the claim until it records the connected local port"
    );
    assert!(first_claim.record_first_connected_port(41000));

    let reuse_claim = claim_srt_egress_muxer_port(&state);
    assert_eq!(reuse_claim.bind_port(), Some(41000));
    assert!(
        !reuse_claim.record_first_connected_port(42000),
        "later connectors must not replace the learned muxer port"
    );
    assert_eq!(*state.lock().unwrap(), Some(41000));
}
