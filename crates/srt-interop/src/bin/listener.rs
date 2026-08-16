//! Skeleton only (Phase 2 scope). Real listener-side interop harness lands
//! in Phase 3 once the vendored core is trimmed to restream's LIVE-only
//! scope. See docs/srt-pure-rust-plan.md Phase 3.

fn main() {
    let _ = shiguredo_srt::ConnectionRole::Listener;
    eprintln!("srt-interop-listener: skeleton only, see docs/srt-pure-rust-plan.md Phase 3");
}
