use std::os::raw::c_int;

use super::srt_stream_id::percent_decode;

pub(super) struct SrtEgressUrl {
    pub(super) host_port: String,
    pub(super) streamid: String,
    pub(super) bond_addrs: Vec<String>,
    pub(super) passphrase: String,
    pub(super) pbkeylen: Option<c_int>,
}

pub(super) fn parse_srt_egress_url(url: &str) -> SrtEgressUrl {
    let url_cleaned = url.replace("srt://", "");
    let parts: Vec<&str> = url_cleaned.split('?').collect();
    let host_port = parts[0].to_string();

    let mut streamid = String::new();
    let mut bond_addrs: Vec<String> = Vec::new();
    let mut passphrase = String::new();
    let mut pbkeylen = None;
    if parts.len() > 1 {
        for param in parts[1].split('&') {
            let key_val: Vec<&str> = param.splitn(2, '=').collect();
            if key_val.len() == 2 {
                match key_val[0] {
                    "streamid" => streamid = percent_decode(key_val[1]),
                    "passphrase" => passphrase = percent_decode(key_val[1]),
                    "pbkeylen" => pbkeylen = key_val[1].parse::<c_int>().ok(),
                    "bond" => {
                        bond_addrs = key_val[1].split(',').map(|s| s.to_string()).collect();
                    }
                    _ => {}
                }
            }
        }
    }
    SrtEgressUrl {
        host_port,
        streamid,
        bond_addrs,
        passphrase,
        pbkeylen,
    }
}
