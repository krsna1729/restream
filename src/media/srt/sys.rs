use std::os::raw::{c_char, c_int, c_void};

pub type SRTSOCKET = c_int;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sockaddr_in {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct SrtTraceBStats {
    pub ms_time_stamp: i64,
    pub pkt_sent_total: i64,
    pub pkt_recv_total: i64,
    pub pkt_snd_loss_total: c_int,
    pub pkt_rcv_loss_total: c_int,
    pub pkt_retrans_total: c_int,
    pub pkt_sent_ack_total: c_int,
    pub pkt_recv_ack_total: c_int,
    pub pkt_sent_nak_total: c_int,
    pub pkt_recv_nak_total: c_int,
    pub us_snd_duration_total: i64,
    pub pkt_snd_drop_total: c_int,
    pub pkt_rcv_drop_total: c_int,
    pub pkt_rcv_undecrypt_total: c_int,
    pub byte_sent_total: u64,
    pub byte_recv_total: u64,
    pub byte_rcv_loss_total: u64,
    pub byte_retrans_total: u64,
    pub byte_snd_drop_total: u64,
    pub byte_rcv_drop_total: u64,
    pub byte_rcv_undecrypt_total: u64,
    pub pkt_sent: i64,
    pub pkt_recv: i64,
    pub pkt_snd_loss: c_int,
    pub pkt_rcv_loss: c_int,
    pub pkt_retrans: c_int,
    pub pkt_rcv_retrans: c_int,
    pub pkt_sent_ack: c_int,
    pub pkt_recv_ack: c_int,
    pub pkt_sent_nak: c_int,
    pub pkt_recv_nak: c_int,
    pub mbps_send_rate: f64,
    pub mbps_recv_rate: f64,
    pub us_snd_duration: i64,
    pub pkt_reorder_distance: c_int,
    pub pkt_rcv_avg_belated_time: f64,
    pub pkt_rcv_belated: i64,
    pub pkt_snd_drop: c_int,
    pub pkt_rcv_drop: c_int,
    pub pkt_rcv_undecrypt: c_int,
    pub byte_sent: u64,
    pub byte_recv: u64,
    pub byte_rcv_loss: u64,
    pub byte_retrans: u64,
    pub byte_snd_drop: u64,
    pub byte_rcv_drop: u64,
    pub byte_rcv_undecrypt: u64,
    pub us_pkt_snd_period: f64,
    pub pkt_flow_window: c_int,
    pub pkt_congestion_window: c_int,
    pub pkt_flight_size: c_int,
    pub ms_rtt: f64,
    pub mbps_bandwidth: f64,
    pub byte_avail_snd_buf: c_int,
    pub byte_avail_rcv_buf: c_int,
    pub mbps_max_bw: f64,
    pub byte_mss: c_int,
    pub pkt_snd_buf: c_int,
    pub byte_snd_buf: c_int,
    pub ms_snd_buf: c_int,
    pub ms_snd_tsb_pd_delay: c_int,
    pub pkt_rcv_buf: c_int,
    pub byte_rcv_buf: c_int,
    pub ms_rcv_buf: c_int,
    pub ms_rcv_tsb_pd_delay: c_int,
    pub pkt_snd_filter_extra_total: c_int,
    pub pkt_rcv_filter_extra_total: c_int,
    pub pkt_rcv_filter_supply_total: c_int,
    pub pkt_rcv_filter_loss_total: c_int,
    pub pkt_snd_filter_extra: c_int,
    pub pkt_rcv_filter_extra: c_int,
    pub pkt_rcv_filter_supply: c_int,
    pub pkt_rcv_filter_loss: c_int,
    pub pkt_reorder_tolerance: c_int,
    pub pkt_sent_unique_total: i64,
    pub pkt_recv_unique_total: i64,
    pub byte_sent_unique_total: u64,
    pub byte_recv_unique_total: u64,
    pub pkt_sent_unique: i64,
    pub pkt_recv_unique: i64,
    pub byte_sent_unique: u64,
    pub byte_recv_unique: u64,
}

pub const SRTGROUP_MASK: c_int = 1 << 30;
pub const SRT_GTYPE_BACKUP: c_int = 2;
pub(super) const SRTS_CONNECTED: c_int = 5;
pub(super) const SRTS_BROKEN: c_int = 6;
pub(super) const SRT_GST_RUNNING: c_int = 2;
pub(super) const SRT_GST_BROKEN: c_int = 3;

pub(super) const SRT_EPOLL_IN: c_int = 0x1;
pub(super) const SRT_EPOLL_OUT: c_int = 0x4;
pub(super) const SRT_EPOLL_ERR: c_int = 0x8;

pub(super) const SRT_ESCLOSED: c_int = 1005;
pub(super) const SRT_ECONNLOST: c_int = 2001;
pub(super) const SRT_ENOCONN: c_int = 2002;
pub(super) const SRT_EASYNCSND: c_int = 6001;
pub(super) const SRT_EASYNCRCV: c_int = 6002;
pub(super) const SRT_ETIMEOUT: c_int = 6003;

#[repr(C)]
pub struct SrtSockOptConfig {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct SrtGroupMemberConfig {
    pub id: SRTSOCKET,
    pub srcaddr: libc::sockaddr_storage,
    pub peeraddr: libc::sockaddr_storage,
    pub weight: u16,
    pub config: *mut SrtSockOptConfig,
    pub errorcode: c_int,
    pub token: c_int,
}

#[repr(C)]
pub struct SrtSocketGroupData {
    pub id: SRTSOCKET,
    pub peeraddr: libc::sockaddr_storage,
    pub sockstate: c_int,
    pub weight: u16,
    pub memberstate: c_int,
    pub result: c_int,
    pub token: c_int,
}

// SAFETY: FFI declarations for the libsrt C library. All function signatures
// are verified against the libsrt public API (srt.h). The library is loaded
// at link time (dynamic or static) and is guaranteed to be present when
// srt_startup() succeeds during SrtServer::new(). None of these functions
// have Rust-side invariants beyond correct argument types, which are
// enforced by the Rust type system at each call site.
unsafe extern "C" {
    pub fn srt_getversion() -> u32;
    pub fn srt_startup() -> c_int;
    pub fn srt_cleanup() -> c_int;
    pub fn srt_setloglevel(level: c_int);
    pub fn srt_create_socket() -> SRTSOCKET;
    pub fn srt_create_group(gtype: c_int) -> SRTSOCKET;
    pub fn srt_close(u: SRTSOCKET) -> c_int;
    pub fn srt_bind(u: SRTSOCKET, name: *const sockaddr_in, namelen: c_int) -> c_int;
    pub fn srt_listen(u: SRTSOCKET, backlog: c_int) -> c_int;
    pub fn srt_listen_callback(
        lsn: SRTSOCKET,
        hook_fn: Option<
            unsafe extern "C" fn(
                opaq: *mut c_void,
                ns: SRTSOCKET,
                hsversion: c_int,
                peeraddr: *const libc::sockaddr,
                streamid: *const c_char,
            ) -> c_int,
        >,
        hook_opaque: *mut c_void,
    ) -> c_int;
    pub fn srt_accept(u: SRTSOCKET, addr: *mut sockaddr_in, addrlen: *mut c_int) -> SRTSOCKET;
    pub fn srt_getsockname(u: SRTSOCKET, name: *mut sockaddr_in, namelen: *mut c_int) -> c_int;
    pub fn srt_connect(u: SRTSOCKET, name: *const sockaddr_in, namelen: c_int) -> c_int;
    pub fn srt_connect_group(
        group: SRTSOCKET,
        name: *mut SrtGroupMemberConfig,
        arraysize: c_int,
    ) -> c_int;
    pub fn srt_group_data(
        group: SRTSOCKET,
        output: *mut SrtSocketGroupData,
        inoutlen: *mut usize,
    ) -> c_int;
    pub fn srt_prepare_endpoint(
        src: *const libc::sockaddr,
        adr: *const libc::sockaddr,
        namelen: c_int,
    ) -> SrtGroupMemberConfig;
    #[cfg(test)]
    pub fn srt_create_config() -> *mut SrtSockOptConfig;
    #[cfg(test)]
    pub fn srt_delete_config(config: *mut SrtSockOptConfig);
    #[cfg(test)]
    pub fn srt_config_add(
        config: *mut SrtSockOptConfig,
        option: c_int,
        contents: *const c_void,
        len: c_int,
    ) -> c_int;
    pub fn srt_recv(u: SRTSOCKET, buf: *mut u8, len: c_int) -> c_int;
    pub fn srt_recvmsg2(
        u: SRTSOCKET,
        buf: *mut u8,
        len: c_int,
        message_control: *mut c_void,
    ) -> c_int;
    pub fn srt_send(u: SRTSOCKET, buf: *const u8, len: c_int) -> c_int;
    pub fn srt_setsockopt(
        u: SRTSOCKET,
        level: c_int,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    pub fn srt_setsockflag(
        u: SRTSOCKET,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    pub fn srt_getsockopt(
        u: SRTSOCKET,
        level: c_int,
        optname: c_int,
        optval: *mut c_void,
        optlen: *mut c_int,
    ) -> c_int;
    pub fn srt_getlasterror(locp: *mut c_int) -> c_int;
    pub fn srt_getlasterror_str() -> *const c_char;
    pub fn srt_setrejectreason(sock: SRTSOCKET, value: c_int) -> c_int;
    pub fn srt_bistats(
        u: SRTSOCKET,
        perf: *mut SrtTraceBStats,
        clear: c_int,
        instantaneous: c_int,
    ) -> c_int;
    pub fn srt_epoll_create() -> c_int;
    pub fn srt_epoll_add_usock(eid: c_int, u: SRTSOCKET, events: *const c_int) -> c_int;
    pub fn srt_epoll_update_usock(eid: c_int, u: SRTSOCKET, events: *const c_int) -> c_int;
    pub fn srt_epoll_remove_usock(eid: c_int, u: SRTSOCKET) -> c_int;
    pub fn srt_epoll_release(eid: c_int) -> c_int;
    pub fn srt_epoll_wait(
        eid: c_int,
        readfds: *mut SRTSOCKET,
        rnum: *mut c_int,
        writefds: *mut SRTSOCKET,
        wnum: *mut c_int,
        ms_timeout: i64,
        lrfds: *mut c_int,
        lrnum: *mut c_int,
        lwfds: *mut c_int,
        lwnum: *mut c_int,
    ) -> c_int;
}

pub const SRTO_SNDSYN: c_int = 1;
pub const SRTO_RCVSYN: c_int = 2;
pub const SRTO_FC: c_int = 4;
pub const SRTO_SNDBUF: c_int = 5;
pub const SRTO_RCVBUF: c_int = 6;
pub const SRTO_UDP_SNDBUF: c_int = 8;
pub const SRTO_UDP_RCVBUF: c_int = 9;
pub const SRTO_REUSEADDR: c_int = 15;
pub const SRTO_MAXBW: c_int = 16;
pub const SRTO_LATENCY: c_int = 23;
pub const SRTO_PASSPHRASE: c_int = 26;
pub const SRTO_PBKEYLEN: c_int = 27;
pub const SRTO_CONNTIMEO: c_int = 36;
pub const SRTO_LOSSMAXTTL: c_int = 42;
pub const SRTO_RCVLATENCY: c_int = 43;
#[cfg(test)]
pub const SRTO_PEERLATENCY: c_int = 44;
pub const SRTO_STREAMID: c_int = 46;
pub const SRTO_TRANSTYPE: c_int = 50;
pub const SRTO_ENFORCEDENCRYPTION: c_int = 53;
pub const SRTO_GROUPCONNECT: c_int = 57;

pub const SRTT_LIVE: c_int = 0;
