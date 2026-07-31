use crate::media::srt::{SrtEgressPollError, SrtEgressSocketError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtBackendAddError {
    Socket(SrtEgressSocketError),
    Poller(SrtEgressPollError),
}

impl From<SrtEgressSocketError> for SrtBackendAddError {
    fn from(error: SrtEgressSocketError) -> Self {
        Self::Socket(error)
    }
}
