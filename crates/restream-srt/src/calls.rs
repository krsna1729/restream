//! Wanted SRT caller inputs, written by the reconciler and read by the SRT
//! server. The server supervises dialing and redialling these inputs as
//! publishers, just like listener peers.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::egress_connect::validate_srt_egress_url;

/// One `srt_call` input the rows want running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrtCallSpec {
    pub pipeline_id: String,
    pub input_id: String,
    pub stream_key: String,
    pub selected: bool,
    /// `srt://host:port?streamid=…&passphrase=…&latency=…`
    pub url: String,
}

/// The wanted calls, written by the reconciler and read by the SRT server.
#[derive(Default)]
pub struct SrtCallInputs {
    wanted: Mutex<HashMap<String, SrtCallSpec>>,
    generation: AtomicU64,
}

impl SrtCallInputs {
    /// Replace the wanted set. Returns the inputs whose call is new or
    /// changed (and so is (re)dialled); unchanged rows return nothing.
    pub fn converge(&self, specs: Vec<SrtCallSpec>) -> Vec<String> {
        let mut wanted = restream_sync::lock(&self.wanted);
        let next: HashMap<String, SrtCallSpec> = specs
            .into_iter()
            .map(|spec| (spec.input_id.clone(), spec))
            .collect();
        if *wanted == next {
            return Vec::new();
        }
        let mut started: Vec<String> = next
            .iter()
            .filter(|(id, spec)| wanted.get(*id) != Some(*spec))
            .map(|(id, _)| id.clone())
            .collect();
        started.sort();
        *wanted = next;
        self.generation.fetch_add(1, Ordering::Release);
        started
    }

    /// The latest wanted set when the caller's generation is out of date.
    pub fn snapshot_since(&self, seen: u64) -> Option<(u64, HashMap<String, SrtCallSpec>)> {
        let generation = self.generation.load(Ordering::Acquire);
        (generation != seen).then(|| (generation, restream_sync::lock(&self.wanted).clone()))
    }
}

/// Reject a caller-input URL this engine could not dial.
pub fn validate_srt_call_url(url: &str) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("srt://") else {
        return Err("an srtCall input's sourceUrl must start with srt://".to_string());
    };
    let host = rest.split('?').next().unwrap_or_default();
    let usable = host.rsplit_once(':').is_some_and(|(name, port)| {
        !name.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
    });
    if !usable {
        return Err(
            "an srtCall input's sourceUrl needs a host and port (srt://host:port)".to_string(),
        );
    }
    validate_srt_egress_url(url)
}
