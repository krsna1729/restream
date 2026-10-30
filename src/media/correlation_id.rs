//! Process-wide correlation ids for log lines that follow one attempt.

use std::sync::atomic::{AtomicU64, Ordering};

static CORRELATION_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn next_correlation_id(prefix: &str) -> String {
    let seq = CORRELATION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{seq:016x}")
}
