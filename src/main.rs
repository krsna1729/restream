//! Binary entry point — delegates to `restream::run_app()`.
//! The tokio multi-threaded runtime is used for all async I/O.
//! CPU-bound FFmpeg work runs on dedicated OS threads (see `src/lib.rs` docs).

const TOKIO_THREAD_NAME: &str = "restream-tokio";

fn main() {
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
