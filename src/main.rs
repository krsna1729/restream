//! Binary entry point — delegates to `restream::run_app()`.
//! The tokio multi-threaded runtime is used for all async I/O.
//! CPU-bound FFmpeg work runs on dedicated OS threads (see `src/lib.rs` docs).

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn available_parallelism_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn parse_cpu_list_count(value: &str) -> Option<usize> {
    let mut count = 0usize;
    for item in value.trim().split(',').filter(|item| !item.is_empty()) {
        let (start, end) = match item.split_once('-') {
            Some((start, end)) => (start.trim(), end.trim()),
            None => {
                item.trim().parse::<usize>().ok()?;
                count = count.checked_add(1)?;
                continue;
            }
        };
        let start = start.parse::<usize>().ok()?;
        let end = end.parse::<usize>().ok()?;
        if end < start {
            return None;
        }
        count = count.checked_add(end - start + 1)?;
    }
    (count > 0).then_some(count)
}

fn parse_cpu_allowed_list(status: &str) -> Option<usize> {
    status.lines().find_map(|line| {
        line.strip_prefix("Cpus_allowed_list:")
            .and_then(parse_cpu_list_count)
    })
}

fn process_cpu_mask_count() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| parse_cpu_allowed_list(&status))
}

fn parse_cpu_max_quota(value: &str) -> Option<usize> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?;
    let period = parts.next()?.parse::<usize>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<usize>().ok()?;
    Some(quota.div_ceil(period).max(1))
}

fn cgroup_cpu_quota_count() -> Option<usize> {
    std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
        .ok()
        .and_then(|cpu_max| parse_cpu_max_quota(&cpu_max))
}

fn effective_cpu_count() -> usize {
    let mut cpus = available_parallelism_count().max(1);
    if let Some(mask_cpus) = process_cpu_mask_count() {
        cpus = cpus.min(mask_cpus.max(1));
    }
    if let Some(quota_cpus) = cgroup_cpu_quota_count() {
        cpus = cpus.min(quota_cpus.max(1));
    }
    cpus.max(1)
}

fn default_runtime_worker_threads(effective_cpus: usize) -> usize {
    let effective_cpus = effective_cpus.max(1);
    if effective_cpus <= 2 {
        effective_cpus
    } else {
        effective_cpus.div_ceil(3).clamp(2, 8)
    }
}

fn runtime_worker_threads() -> usize {
    let default = default_runtime_worker_threads(effective_cpu_count());
    env_usize("RESTREAM_TOKIO_WORKER_THREADS", default).max(1)
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

    let config = std::sync::Arc::new(restream::AppConfig::from_env());

    // Initialise FFmpeg binary path from config (synchronous, before any
    // async task can race with it). Must happen before ffmpeg_bin_path()
    // consumers run — OnceLock init is thread-safe but we keep it on the
    // main thread for clarity.
    restream::ffmpeg_extract::init(config.ffmpeg_bin_path.clone());
    let worker_threads = runtime_worker_threads();
    let max_blocking_threads = runtime_max_blocking_threads();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(max_blocking_threads)
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(restream::run_app(config));

    restream::ffmpeg_extract::cleanup_ffmpeg();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_cpu_allowed_lists() {
        assert_eq!(parse_cpu_list_count("0"), Some(1));
        assert_eq!(parse_cpu_list_count("0-5"), Some(6));
        assert_eq!(parse_cpu_list_count("0-1,4,6-7"), Some(5));
        assert_eq!(parse_cpu_list_count("3-1"), None);
        assert_eq!(parse_cpu_list_count(""), None);
    }

    #[test]
    fn extracts_cpu_allowed_list_from_proc_status() {
        let status = "Name:\trestream\nCpus_allowed_list:\t0-1,4\n";
        assert_eq!(parse_cpu_allowed_list(status), Some(3));
    }

    #[test]
    fn parses_cgroup_v2_cpu_quota() {
        assert_eq!(parse_cpu_max_quota("max 100000"), None);
        assert_eq!(parse_cpu_max_quota("100000 100000"), Some(1));
        assert_eq!(parse_cpu_max_quota("150000 100000"), Some(2));
        assert_eq!(parse_cpu_max_quota("250000 100000"), Some(3));
    }

    #[test]
    fn default_tokio_workers_are_conservative_for_io_fanout() {
        assert_eq!(default_runtime_worker_threads(1), 1);
        assert_eq!(default_runtime_worker_threads(2), 2);
        assert_eq!(default_runtime_worker_threads(6), 2);
        assert_eq!(default_runtime_worker_threads(12), 4);
        assert_eq!(default_runtime_worker_threads(64), 8);
    }
}
