/// The configuration options that govern how a RTMP server session should operate
#[derive(Clone)]
pub struct ServerSessionConfig {
    pub fms_version: String,
    pub chunk_size: u32,
    pub peer_bandwidth: u32,
    pub window_ack_size: u32,
    pub send_on_bw_done_message_on_start: bool,
    /// Largest inbound RTMP message accepted; longer declared lengths fail the
    /// session before any payload is buffered. (restream vendor patch)
    pub max_message_length: u32,
}

impl ServerSessionConfig {
    /// Creates a new server session config with overridable defaults
    pub fn new() -> ServerSessionConfig {
        ServerSessionConfig {
            fms_version: "FMS/3,0,1,1233".to_string(),
            peer_bandwidth: 2_500_000,
            window_ack_size: 1_073_741_824,
            chunk_size: 4096,
            send_on_bw_done_message_on_start: true,
            max_message_length: 0x00FF_FFFF,
        }
    }
}
