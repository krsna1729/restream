use std::ffi::CString;
use std::fmt;
use std::os::raw::{c_int, c_void};

use super::socket::last_srt_error;
use super::sys::{SRTO_SNDSYN, SRTO_STREAMID, SRTSOCKET, srt_setsockflag, srt_setsockopt};

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
pub(crate) struct SrtEgressSocketError {
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

impl fmt::Display for SrtEgressSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to set {}: {} ({})",
            self.option, self.message, self.code
        )
    }
}

impl std::error::Error for SrtEgressSocketError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SrtEgressSendMode {
    // Not constructed by any production call site (every caller passes
    // `FabricNonblocking`); kept because several tests still exercise this
    // arm's no-op behavior as a regression guard for the pre-fabric legacy
    // blocking-send mode.
    #[cfg_attr(not(test), allow(dead_code))]
    LegacyBlocking,
    FabricNonblocking,
}

pub(crate) fn configure_connected_srt_egress_socket(
    socket: SRTSOCKET,
    mode: SrtEgressSendMode,
) -> Result<(), SrtEgressSocketError> {
    configure_connected_srt_egress_socket_with(socket, mode, LibSrtSocketOps)
}

pub(crate) fn apply_srt_egress_stream_id(
    socket: SRTSOCKET,
    stream_id: &str,
) -> Result<(), SrtEgressSocketError> {
    apply_srt_egress_stream_id_with(socket, stream_id, LibSrtSocketOps)
}

fn apply_srt_egress_stream_id_with<O>(
    socket: SRTSOCKET,
    stream_id: &str,
    ops: O,
) -> Result<(), SrtEgressSocketError>
where
    O: SrtSocketOps,
{
    if stream_id.is_empty() {
        return Ok(());
    }
    let stream_id = CString::new(stream_id).map_err(|_| {
        SrtEgressSocketError::new(
            "SRTO_STREAMID",
            0,
            "stream ID contains null bytes".to_string(),
        )
    })?;
    ops.set_bytes(socket, SRTO_STREAMID, "SRTO_STREAMID", stream_id.as_bytes())
}

fn configure_connected_srt_egress_socket_with<O>(
    socket: SRTSOCKET,
    mode: SrtEgressSendMode,
    ops: O,
) -> Result<(), SrtEgressSocketError>
where
    O: SrtSocketOps,
{
    match mode {
        SrtEgressSendMode::LegacyBlocking => Ok(()),
        SrtEgressSendMode::FabricNonblocking => {
            configure_srt_egress_socket_with(socket, SrtEgressSocketConfig::NONBLOCKING_SEND, ops)
        }
    }
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

    fn set_bytes(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        option_name: &'static str,
        value: &[u8],
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

    fn set_bytes(
        &self,
        socket: SRTSOCKET,
        option: c_int,
        option_name: &'static str,
        value: &[u8],
    ) -> Result<(), SrtEgressSocketError> {
        // SAFETY: Category 8 - FFI boundary. `socket` is a live libsrt socket
        // handle. `value` points to initialized bytes that stay alive for the
        // duration of the call, and `value.len()` is the exact option length.
        let result = unsafe {
            srt_setsockopt(
                socket,
                0,
                option,
                value.as_ptr() as *const c_void,
                value.len() as c_int,
            )
        };
        if result >= 0 {
            return Ok(());
        }

        let (code, message) = last_srt_error();
        Err(SrtEgressSocketError::new(option_name, code, message))
    }
}

#[cfg(test)]
#[path = "egress_socket_tests.rs"]
mod tests;
