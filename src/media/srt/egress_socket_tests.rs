use super::*;
use std::cell::RefCell;
use std::os::raw::c_int;
use std::rc::Rc;

type FlagCalls = Rc<RefCell<Vec<(SRTSOCKET, c_int, c_int)>>>;
type ByteCalls = Rc<RefCell<Vec<(SRTSOCKET, c_int, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct FakeSocketOps {
    calls: FlagCalls,
    bytes_calls: ByteCalls,
    fail: bool,
    fail_bytes: bool,
}

impl SrtSocketOps for FakeSocketOps {
    fn set_flag(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        value: c_int,
    ) -> Result<(), SrtEgressSocketError> {
        self.calls.borrow_mut().push((socket, option, value));
        if self.fail {
            return Err(SrtEgressSocketError::new(
                "SRTO_SNDSYN",
                4321,
                "fake error".to_string(),
            ));
        }
        Ok(())
    }

    fn set_bytes(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        option_name: &'static str,
        value: &[u8],
    ) -> Result<(), SrtEgressSocketError> {
        self.bytes_calls
            .borrow_mut()
            .push((socket, option, value.to_vec()));
        if self.fail_bytes {
            return Err(SrtEgressSocketError::new(
                option_name,
                8765,
                "fake byte option error".to_string(),
            ));
        }
        Ok(())
    }
}

#[test]
fn egress_socket_config_disables_synchronous_send() {
    let ops = FakeSocketOps::default();

    configure_srt_egress_socket_with(42, SrtEgressSocketConfig::NONBLOCKING_SEND, ops.clone())
        .unwrap();

    assert_eq!(ops.calls.borrow().as_slice(), &[(42, SRTO_SNDSYN, 0)]);
}

#[test]
fn egress_socket_config_surfaces_option_failure() {
    let ops = FakeSocketOps {
        fail: true,
        ..FakeSocketOps::default()
    };

    let error = configure_srt_egress_socket_with(42, SrtEgressSocketConfig::NONBLOCKING_SEND, ops)
        .expect_err("socket setup should fail");

    assert_eq!(error.option, "SRTO_SNDSYN");
    assert_eq!(error.code, 4321);
}

#[test]
fn fabric_connected_socket_disables_synchronous_send() {
    let ops = FakeSocketOps::default();

    configure_connected_srt_egress_socket_with(
        42,
        SrtEgressSendMode::FabricNonblocking,
        ops.clone(),
    )
    .unwrap();

    assert_eq!(ops.calls.borrow().as_slice(), &[(42, SRTO_SNDSYN, 0)]);
}

#[test]
fn legacy_connected_socket_preserves_existing_send_mode() {
    let ops = FakeSocketOps::default();

    configure_connected_srt_egress_socket_with(42, SrtEgressSendMode::LegacyBlocking, ops.clone())
        .unwrap();

    assert!(ops.calls.borrow().is_empty());
}

#[test]
fn empty_stream_id_skips_socket_option() {
    let ops = FakeSocketOps::default();

    apply_srt_egress_stream_id_with(42, "", ops.clone()).unwrap();

    assert!(ops.bytes_calls.borrow().is_empty());
}

#[test]
fn stream_id_rejects_interior_nul_before_socket_option() {
    let ops = FakeSocketOps::default();

    let error = apply_srt_egress_stream_id_with(42, "publish\0bad", ops.clone())
        .expect_err("interior NUL must fail before FFI");

    assert_eq!(error.option, "SRTO_STREAMID");
    assert_eq!(error.code, 0);
    assert!(ops.bytes_calls.borrow().is_empty());
}

#[test]
fn stream_id_sets_exact_bytes_without_nul_terminator() {
    let ops = FakeSocketOps::default();

    apply_srt_egress_stream_id_with(42, "publish:key", ops.clone()).unwrap();

    assert_eq!(
        ops.bytes_calls.borrow().as_slice(),
        &[(42, SRTO_STREAMID, b"publish:key".to_vec())]
    );
}

#[test]
fn stream_id_surfaces_socket_option_failure() {
    let ops = FakeSocketOps {
        fail_bytes: true,
        ..FakeSocketOps::default()
    };

    let error = apply_srt_egress_stream_id_with(42, "publish:key", ops)
        .expect_err("stream id option setup should fail");

    assert_eq!(error.option, "SRTO_STREAMID");
    assert_eq!(error.code, 8765);
}
