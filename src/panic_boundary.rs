//! Fault domains for code that handles one external entity (an ingest
//! connection or peer, an egress output).
//!
//! A panic while handling one entity must end only that entity: the thread
//! that serves many of them keeps running. Each boundary sits at the single
//! seam where the entity's bytes enter (`egress::visit::visit_leaf`, the RTMP
//! connection future, the SRT ingress per-peer media step) and turns a panic
//! into that entity's failure. State shared with other entities must be safe
//! to observe after an unwind there: atomics, poison-tolerant locks, and
//! per-entity state that is dropped with the entity.

use std::any::Any;

/// Counts panics contained at a boundary, for the health surface.
pub(crate) static CONTAINED_PANICS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// The message of a panic payload (`panic!` with a literal or a format).
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Records one contained panic and returns its message.
pub(crate) fn record_contained(payload: &(dyn Any + Send)) -> &str {
    CONTAINED_PANICS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    panic_message(payload)
}

/// Total panics contained since start.
pub fn contained_panics() -> u64 {
    CONTAINED_PANICS.load(std::sync::atomic::Ordering::Relaxed)
}
