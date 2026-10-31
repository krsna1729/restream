//! Transport ownership guards. SRT and RTMP/RTMPS transport I/O
//! is owned by Compio/io_uring owner threads; these source checks keep retired
//! or bypassing transport paths from returning. Test-only items are excluded
//! item by item (`strip_test_items`), so production code that follows a test
//! block is still checked.
use super::collect_rust_sources;

/// `source` without its `#[cfg(test)]`-gated items: the attribute and the item
/// it gates, either up to the matching `}` of its first `{` or to the next `;`
/// when the item has no body (`mod tests;`, `use ...;`).
fn strip_test_items(source: &str) -> String {
    let mut kept = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("#[cfg(test)]") {
        kept.push_str(&rest[..start]);
        let item = &rest[start + "#[cfg(test)]".len()..];
        let body = item.find('{');
        let semicolon = item.find(';');
        let end = match (body, semicolon) {
            (Some(open), Some(semi)) if semi < open => semi + 1,
            (Some(open), _) => {
                let mut depth = 0usize;
                let mut close = item.len();
                for (offset, byte) in item.bytes().enumerate().skip(open) {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = offset + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                close
            }
            (None, Some(semi)) => semi + 1,
            (None, None) => item.len(),
        };
        rest = &item[end..];
    }
    kept.push_str(rest);
    kept
}

#[test]
fn strip_test_items_keeps_production_code_after_a_test_block() {
    let source = "use a;\n#[cfg(test)]\nmod tests { fn t() { let x = { 1 }; } }\n#[cfg(test)]\nuse b;\nfn after() {}\n";
    let production = strip_test_items(source);
    assert!(production.contains("use a;"));
    assert!(production.contains("fn after() {}"));
    assert!(!production.contains("mod tests"));
    assert!(!production.contains("use b;"));
}

/// SRT and RTMP/RTMPS transport I/O runs on Compio/io_uring
/// owners. Production transport code holds no Tokio network type, no byte
/// bridge into Tokio, and no runtime-selection switch; each transport builds
/// its runtime on io_uring and refuses anything else.
#[test]
fn production_transport_stays_on_io_uring_owners() {
    const FORBIDDEN: &[&str] = &[
        "tokio::net::",
        "tokio::io::duplex",
        "DuplexStream",
        "RESTREAM_RTMP_RUNTIME",
        "RESTREAM_SRT_RUNTIME",
    ];
    let mut inspect = |path: &std::path::Path, source: &str| {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "tests.rs" || path.components().any(|part| part.as_os_str() == "tests") {
            return;
        }
        let production = strip_test_items(source);
        for forbidden in FORBIDDEN {
            assert!(
                !production.contains(forbidden),
                "{} puts `{forbidden}` on the production transport path",
                path.display()
            );
        }
    };
    for root in [
        "crates/restream-engine/src/rtmp",
        "crates/restream-engine/src/srt",
        "crates/restream-engine/src/egress/backends",
        "crates/restream-rtmp/src",
        "crates/restream-srt/src",
        "crates/restream-moq/src",
        "crates/restream-mpegts/src",
    ] {
        collect_rust_sources(std::path::Path::new(root), &mut inspect);
    }
    for file in [
        "crates/restream-engine/src/rtmp.rs",
        "crates/restream-engine/src/srt.rs",
    ] {
        let source = std::fs::read_to_string(file).expect("transport module root is readable");
        inspect(std::path::Path::new(file), &source);
    }
    for (file, required) in [
        (
            "crates/restream-engine/src/rtmp/listener.rs",
            "DriverType::IoUring",
        ),
        (
            "crates/restream-engine/src/egress/backends/compio_tcp/poller.rs",
            "DriverType::IoUring",
        ),
        (
            "crates/restream-engine/src/egress/backends/srt/owner_set.rs",
            "production_runtime_builder",
        ),
    ] {
        let source = std::fs::read_to_string(file).expect("transport runtime source is readable");
        assert!(
            source.contains(required),
            "{file} must build its transport runtime via `{required}`"
        );
    }
}

/// RTMP/RTMPS transport is Compio/io_uring only. The retired epoll egress
/// poller must not return, not even as a test adapter: level-triggered test
/// readiness hid edge-triggered completion wakeup bugs from unit tests.
#[test]
fn media_transport_has_no_epoll_readiness_path() {
    const FORBIDDEN: &[&str] = &[
        "epoll_create",
        "epoll_ctl",
        "epoll_wait",
        "TcpEgressPoller",
        "TcpPollOps",
    ];
    for root in [
        "crates/restream-engine/src",
        "crates/restream-rtmp/src",
        "crates/restream-srt/src",
        "crates/restream-moq/src",
        "crates/restream-mpegts/src",
    ] {
        collect_rust_sources(std::path::Path::new(root), &mut |path, source| {
            let code: String = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect();
            for forbidden in FORBIDDEN {
                assert!(
                    !code.contains(forbidden),
                    "{} reintroduces retired epoll transport `{forbidden}`",
                    path.display()
                );
            }
        });
    }
}
