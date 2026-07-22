#![allow(dead_code)]

use std::os::raw::{c_int, c_void};

use super::socket::last_srt_error;
use super::sys::{SRTO_SNDSYN, SRTSOCKET, srt_setsockflag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SrtEgressSocketConfig {
    pub synchronous_send: bool,
}

impl SrtEgressSocketConfig {
    pub const NONBLOCKING_SEND: Self = Self {
        synchronous_send: false,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SrtEgressSocketError {
    pub option: &'static str,
    pub code: c_int,
    pub message: String,
}

impl SrtEgressSocketError {
    fn new(option: &'static str, code: c_int, message: String) -> Self {
        Self {
            option,
            code,
            message,
        }
    }
}

pub(super) fn configure_srt_egress_socket(socket: SRTSOCKET) -> Result<(), SrtEgressSocketError> {
    configure_srt_egress_socket_with(
        socket,
        SrtEgressSocketConfig::NONBLOCKING_SEND,
        LibSrtSocketOps,
    )
}

fn configure_srt_egress_socket_with<O>(
    socket: SRTSOCKET,
    config: SrtEgressSocketConfig,
    ops: O,
) -> Result<(), SrtEgressSocketError>
where
    O: SrtSocketOps,
{
    ops.set_flag(
        socket,
        SRTO_SNDSYN,
        bool_to_srt_flag(config.synchronous_send),
    )
}

fn bool_to_srt_flag(value: bool) -> c_int {
    if value { 1 } else { 0 }
}

trait SrtSocketOps {
    fn set_flag(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        value: c_int,
    ) -> Result<(), SrtEgressSocketError>;
}

struct LibSrtSocketOps;

impl SrtSocketOps for LibSrtSocketOps {
    fn set_flag(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        value: c_int,
    ) -> Result<(), SrtEgressSocketError> {
        // SAFETY: `socket` is a live libsrt socket handle. `value` is a
        // correctly-sized c_int that stays alive for the duration of the call.
        let result = unsafe {
            srt_setsockflag(
                socket,
                option,
                &value as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            )
        };
        if result >= 0 {
            return Ok(());
        }

        let (code, message) = last_srt_error();
        Err(SrtEgressSocketError::new("SRTO_SNDSYN", code, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct FakeSocketOps {
        calls: Rc<RefCell<Vec<(SRTSOCKET, c_int, c_int)>>>,
        fail: bool,
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

        let error =
            configure_srt_egress_socket_with(42, SrtEgressSocketConfig::NONBLOCKING_SEND, ops)
                .expect_err("socket setup should fail");

        assert_eq!(error.option, "SRTO_SNDSYN");
        assert_eq!(error.code, 4321);
    }
}
