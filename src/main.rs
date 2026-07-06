//! Binary entry point — delegates to `restream::run_app()`.
//! The tokio multi-threaded runtime is used for all async I/O.
//! CPU-bound FFmpeg work runs on dedicated OS threads (see `src/lib.rs` docs).

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn runtime_worker_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    env_usize("RESTREAM_TOKIO_WORKER_THREADS", cpus.clamp(2, 16)).max(1)
}

fn runtime_max_blocking_threads() -> usize {
    env_usize("RESTREAM_TOKIO_MAX_BLOCKING_THREADS", 512).max(1)
}

fn main() {
    let mut args = std::env::args_os();
    let _program = args.next();
    if let Some(flag) = args.next() {
        if flag == "--emit-sbom" {
            let Some(path) = args.next() else {
                eprintln!("usage: restream --emit-sbom <path>");
                std::process::exit(2);
            };
            if args.next().is_some() {
                eprintln!("usage: restream --emit-sbom <path>");
                std::process::exit(2);
            }
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime")
                .block_on(restream::emit_repo_sbom(std::path::Path::new(&path)));
            match result {
                Ok(true) => {
                    println!("updated {}", std::path::Path::new(&path).display());
                    return;
                }
                Ok(false) => {
                    println!("unchanged {}", std::path::Path::new(&path).display());
                    return;
                }
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
        }
        eprintln!("usage: restream [--emit-sbom <path>]");
        std::process::exit(2);
    }

    // Extract embedded FFmpeg binary synchronously BEFORE the async runtime
    // spawns any threads. Must be called before ffmpeg_bin_path() consumers
    // run — this guarantees single-threaded initialization of the OnceLock
    // and eliminates any race between cached-path write and transcoder-stage
    // spawning.
    restream::ffmpeg_extract::ensure_ffmpeg_extracted();

    let worker_threads = runtime_worker_threads();
    let max_blocking_threads = runtime_max_blocking_threads();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(max_blocking_threads)
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(restream::run_app());

    restream::ffmpeg_extract::cleanup_ffmpeg();
}
