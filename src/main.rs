//! Binary entry point — delegates to `restream::run_app()`.
//! Tokio owns application and control async work (API, database, pipeline
//! lifecycle, telemetry). Production SRT and RTMP/RTMPS transport I/O runs on
//! dedicated Compio/io_uring owner threads; blocking FFmpeg/codec work runs on
//! dedicated OS threads or subprocesses (see `src/lib.rs` docs).

const TOKIO_THREAD_NAME: &str = "restream-tokio";

fn main() {
    limit_malloc_arenas();
    let mut args = std::env::args_os();
    let _program = args.next();
    if let Some(flag) = args.next() {
        if flag == "--emit-sbom" {
            let Some(path) = args.next() else {
                print_usage_and_exit();
            };
            if args.next().is_some() {
                print_usage_and_exit();
            }
            let path = std::path::Path::new(&path);
            let result = restream::emit_sbom(path);
            match result {
                Ok(()) => {
                    println!("updated {}", path.display());
                    return;
                }
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
        }
        print_usage_and_exit();
    }

    let config = std::sync::Arc::new(restream::AppConfig::from_env());

    // Initialise FFmpeg binary path from config (synchronous, before any
    // async task can race with it). Must happen before ffmpeg_bin_path()
    // consumers run — OnceLock init is thread-safe but we keep it on the
    // main thread for clarity.
    restream::ffmpeg_extract::init(config.ffmpeg_bin_path.clone());
    let worker_threads = config.tokio_runtime.worker_threads;
    let max_blocking_threads = config.tokio_runtime.max_blocking_threads;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(max_blocking_threads)
        .thread_name(TOKIO_THREAD_NAME)
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(restream::run_app(config));

    restream::ffmpeg_extract::cleanup_ffmpeg();
}

/// Cap glibc's malloc arenas at two unless the operator set
/// `MALLOC_ARENA_MAX`. glibc otherwise grows up to eight arenas per core and
/// keeps each one's freed memory resident: in the `fault.srt-output-stall`
/// proof an SRT destination freezing surged RSS by 66–72 MB (plateauing, not
/// leaking) against 37 MB with two arenas, and at 500 RTMP / 50 SRT outputs
/// steady RSS fell 47 / 34 MB with no measurable CPU change (7 and 3
/// interleaved runs). Must run before any thread starts.
fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: mallopt only adjusts allocator tuning; called on the main
        // thread before any other thread exists.
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, 2);
        }
    }
}

fn print_usage_and_exit() -> ! {
    eprintln!("usage: restream [--emit-sbom <path>]");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokio_thread_name_fits_linux_comm_limit() {
        assert!(
            TOKIO_THREAD_NAME.len() <= 15,
            "Linux task comm truncates names longer than 15 bytes"
        );
    }
}
