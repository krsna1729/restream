//! Crate root for the restream server.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

#[cfg(feature = "mcp-http-backend")]
pub mod agent_backends;
#[cfg(any(feature = "agent-plane", feature = "mcp-core"))]
pub mod agent_core;
#[cfg(feature = "agent-execution")]
pub mod agent_execution;
#[cfg(feature = "mcp-core")]
pub mod agent_mcp;
#[cfg(feature = "agent-plane")]
pub mod agent_plane;
pub mod alerts;
pub mod api;
pub(crate) mod api_runtime_views;
pub mod api_view_models;
pub mod application;
pub mod capacity;
pub mod config;
pub mod db;
pub mod diag;
pub mod domain;
pub mod events;
pub mod ffmpeg_extract;
pub mod infrastructure;
pub mod logging;
pub mod media;
pub mod planner;
pub mod runtime;
pub mod runtime_info;
pub mod secret_display;
pub(crate) mod system_sampling;
pub mod test_fixtures;

pub use config::{AppConfig, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
pub use infrastructure::bootstrap::run_app;
pub use runtime_info::emit_sbom;

/// # Safety
///
/// Exported with the C ABI as a link-time shim for the removed libavcodec
/// symbol. Callers must pass a codec context pointer; the pointer is never
/// dereferenced here, so any value is accepted.
#[cfg(restream_ffmpeg_needs_avcodec_close_shim)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn avcodec_close(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
) -> std::ffi::c_int {
    0
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{cell::Cell, panic, sync::Once};

    thread_local! {
        static SUPPRESS_EXPECTED_PANIC_HOOK: Cell<bool> = const { Cell::new(false) };
    }

    static INSTALL_PANIC_HOOK: Once = Once::new();

    struct RestoreSuppression(bool);

    impl Drop for RestoreSuppression {
        fn drop(&mut self) {
            SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.set(self.0));
        }
    }

    pub(crate) fn with_expected_panic_suppressed<R>(operation: impl FnOnce() -> R) -> R {
        INSTALL_PANIC_HOOK.call_once(|| {
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                if !SUPPRESS_EXPECTED_PANIC_HOOK.with(Cell::get) {
                    previous(info);
                }
            }));
        });

        let previous = SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.replace(true));
        let _restore = RestoreSuppression(previous);
        operation()
    }
}

#[cfg(test)]
pub(crate) mod test_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ACTIVE: Cell<bool> = const { Cell::new(false) };
        static COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ACTIVE.with(|active| {
                if active.get() {
                    COUNT.with(|count| count.set(count.get().saturating_add(1)));
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    pub fn begin() {
        COUNT.with(|count| count.set(0));
        ACTIVE.with(|active| active.set(true));
    }

    pub fn end() -> usize {
        ACTIVE.with(|active| active.set(false));
        COUNT.with(Cell::get)
    }
}
