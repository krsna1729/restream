//! Binary entry point — delegates to `restream::run_app()`.
//! CONTROL Tokio owns the API, database and lifecycle; a separate MEDIA Tokio
//! runtime runs container and shared-stage work. Production RTMP, SRT and MoQ
//! transport stays on Compio owners. Blocking pipes and files use dedicated OS
//! threads; codecs run in FFmpeg children. See `docs/architecture.md` and
//! `docs/runtime-crossings.md` for ownership and bounded crossings.

const TOKIO_THREAD_NAME: &str = "restream-tokio";

fn main() {
    restream::malloc_tuning::apply_from_env();
    let mut args = std::env::args_os();
    let _program = args.next();
    if let Some(flag) = args.next() {
        if flag == "host-check" || flag == "host-tune" {
            if args.next().is_some() {
                print_usage_and_exit();
            }
            let mut out = std::io::stdout().lock();
            std::process::exit(if flag == "host-check" {
                restream::host_tuning::host_check(&mut out)
            } else {
                restream::host_tuning::host_tune(&mut out)
            });
        }
        if flag == "ffmpeg-fetch" {
            let channel = match args.next() {
                None => restream::ffmpeg_binary::Channel::Pinned,
                Some(arg) if arg == "--latest" => restream::ffmpeg_binary::Channel::Latest,
                Some(_) => print_usage_and_exit(),
            };
            if args.next().is_some() {
                print_usage_and_exit();
            }
            std::process::exit(restream::ffmpeg_binary::fetch(
                channel,
                &mut std::io::stdout().lock(),
            ));
        }
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

    // Seed the configured FFmpeg path before any consumer can resolve it;
    // `run_app` resolves and logs the executable once logging is up.
    restream::ffmpeg_binary::init(config.ffmpeg_bin_path.clone());
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
}

fn print_usage_and_exit() -> ! {
    eprintln!(
        "usage: restream [host-check | host-tune | ffmpeg-fetch [--latest] | --emit-sbom <path>]"
    );
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
