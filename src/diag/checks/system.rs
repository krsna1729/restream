use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sysinfo::{Disks, Networks, System};

use crate::media::engine::MediaEngine;

use super::super::model::DiagResult;

fn media_root_path(media_dir: &str) -> PathBuf {
    let configured_path = PathBuf::from(media_dir);
    let absolute = if configured_path.is_absolute() {
        configured_path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(configured_path)
    };
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn disk_for_path<'a>(disks: &'a Disks, path: &Path) -> Option<(&'a sysinfo::Disk, usize)> {
    disks
        .iter()
        .filter_map(|disk| {
            let mount = disk.mount_point();
            path.starts_with(mount)
                .then_some((disk, mount.components().count()))
        })
        .max_by_key(|(_, depth)| *depth)
}

pub(in crate::diag) async fn check_system_resources(idx: u32, media_dir: &str) -> DiagResult {
    let start = Instant::now();
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu_pct: f32 = sys.global_cpu_usage();
    let total_mem = sys.total_memory();
    let used_mem = sys.used_memory();
    let mem_pct = (used_mem * 100).checked_div(total_mem).unwrap_or(0);

    let media_root = media_root_path(media_dir);
    let disks = Disks::new_with_refreshed_list();
    let selected_disk = disk_for_path(&disks, &media_root).map(|(disk, _)| disk);
    let (total_disk, used_disk, disk_scope, mount_point) = if let Some(disk) = selected_disk {
        let total = disk.total_space();
        let used = total.saturating_sub(disk.available_space());
        (
            total,
            used,
            "media directory backing mount",
            disk.mount_point().display().to_string(),
        )
    } else {
        let (total, used) = disks.iter().fold((0u64, 0u64), |(total, used), disk| {
            (
                total + disk.total_space(),
                used + (disk.total_space() - disk.available_space()),
            )
        });
        (total, used, "all reported mounts", "aggregate".to_string())
    };
    let disk_pct = (used_disk * 100).checked_div(total_disk).unwrap_or(0);

    let mut issues = vec![];
    let mut lines = vec![];

    lines.push(format!("CPU cores: {}", sys.cpus().len()));
    lines.push(format!("CPU usage: {:.1}%", cpu_pct));
    lines.push(format!(
        "RAM total: {} GiB",
        total_mem / (1024 * 1024 * 1024)
    ));
    lines.push(format!(
        "RAM used: {} GiB ({:.1}%)",
        used_mem / (1024 * 1024 * 1024),
        mem_pct
    ));
    lines.push(format!("Disk scope: {}", disk_scope));
    lines.push(format!("Disk mount: {}", mount_point));
    lines.push(format!("Media dir: {}", media_root.display()));
    lines.push(format!(
        "Disk total: {:.1} GiB",
        total_disk as f64 / (1024.0 * 1024.0 * 1024.0)
    ));
    lines.push(format!(
        "Disk used: {:.1} GiB ({}%)",
        used_disk as f64 / (1024.0 * 1024.0 * 1024.0),
        disk_pct
    ));

    if cpu_pct > 90.0 {
        issues.push(format!(
            "CPU usage is very high ({:.1}%). This may cause encoding delays or stream drops.",
            cpu_pct
        ));
    }
    if mem_pct > 90 {
        issues.push(format!(
            "RAM usage is {}%. Risk of OOM killing the streaming process.",
            mem_pct
        ));
    }
    if disk_pct > 95 {
        issues.push(format!(
            "Disk is {}% full. Recordings and HLS segments may fail.",
            disk_pct
        ));
    }

    DiagResult::ok(
        idx,
        "System Resources",
        "CPU, RAM, and disk utilization",
        "sysinfo::System::new_all()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(in crate::diag) async fn check_srt_listener_owner(
    idx: u32,
    engine: &Arc<MediaEngine>,
) -> DiagResult {
    let start = Instant::now();
    let stats = engine.srt_listener_diag_snapshot().await;
    let owner = &stats.ingress_owner;

    let mut lines = vec![];
    let mut issues = vec![];

    lines.push(format!(
        "Active SRT ingest streams: {}",
        stats.active_ingest_count
    ));
    lines.push(format!(
        "Bonded ingest available: {}",
        if stats.bonding_available { "yes" } else { "no" }
    ));
    lines.push(format!("Owner faulted: {}", owner.faulted));
    lines.push(format!(
        "Receive mode: {}",
        if owner.managed_rx {
            "ManagedMultishot"
        } else {
            "RawReadiness"
        }
    ));
    lines.push(format!("Live peers: {}", owner.peers));
    lines.push(format!(
        "RX: {} packets, {} bytes, ring depth {}, ring drops {}, truncated {}",
        owner.rx_packets,
        owner.rx_bytes,
        owner.rx_ring_depth,
        owner.rx_ring_dropped,
        owner.rx_truncated
    ));
    lines.push(format!(
        "TX: {} packets, in flight {}/{}, failed {}, pool exhaustions {}",
        owner.tx_packets,
        owner.tx_in_flight,
        owner.tx_capacity,
        owner.tx_failed,
        owner.tx_exhaustions
    ));
    lines.push(format!(
        "Admission: {} requests, {} rejected, {} deferred, {} credential failures",
        owner.policy_requests,
        owner.policy_rejections,
        owner.policy_deferred,
        owner.credential_failures
    ));

    if owner.faulted {
        issues.push(
            "The SRT ingress Owner faulted; the listener has stopped and needs a restart."
                .to_string(),
        );
    }
    if !stats.bonding_available {
        issues.push("The srt-rs listener has not started with bonded-input support.".to_string());
    }
    if owner.rx_ring_dropped > 0 {
        issues.push(format!(
            "The Owner receive ring dropped {} datagrams — input was lost before protocol \
             processing.",
            owner.rx_ring_dropped
        ));
    }
    if owner.rx_truncated > 0 {
        issues.push(format!(
            "{} received datagrams were truncated by the receive path.",
            owner.rx_truncated
        ));
    }
    if owner.tx_failed > 0 {
        issues.push(format!(
            "{} SRT listener datagram sends failed or were short.",
            owner.tx_failed
        ));
    }

    DiagResult::ok(
        idx,
        "SRT Listener Owner",
        "Compio Owner receive, transmit and admission state for all SRT ingest streams",
        "srt-transport Compio Owner metrics",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(in crate::diag) async fn check_network_bandwidth(idx: u32) -> DiagResult {
    let start = Instant::now();
    let net1 = Networks::new_with_refreshed_list();
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let net2 = Networks::new_with_refreshed_list();

    let mut total_rx_bytes = 0u64;
    let mut total_tx_bytes = 0u64;
    let mut lines = vec![];
    let mut issues = vec![];

    for (interface, data2) in net2.iter() {
        if let Some(data1) = net1.get(interface) {
            let rx = data2
                .total_received()
                .saturating_sub(data1.total_received());
            let tx = data2
                .total_transmitted()
                .saturating_sub(data1.total_transmitted());
            if rx > 0 || tx > 0 {
                let rx_kbps = (rx * 8 * 2) / 1000;
                let tx_kbps = (tx * 8 * 2) / 1000;
                lines.push(format!(
                    "{}: RX {} Kbps TX {} Kbps",
                    interface, rx_kbps, tx_kbps
                ));
                total_rx_bytes += rx;
                total_tx_bytes += tx;
            }
        }
    }

    let total_rx_kbps = (total_rx_bytes * 8 * 2) / 1000;
    let total_tx_kbps = (total_tx_bytes * 8 * 2) / 1000;

    if lines.is_empty() {
        lines.push("No active network interfaces detected in sample.".to_string());
        issues.push("No network traffic detected during the diagnostic sample.".to_string());
    } else {
        lines.push(format!(
            "Total: RX {} Kbps TX {} Kbps",
            total_rx_kbps, total_tx_kbps
        ));
    }

    DiagResult::ok(
        idx,
        "Network Bandwidth",
        "Per-interface RX/TX throughput (500ms sample)",
        "sysinfo::Networks::new_with_refreshed_list()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SRT listener check reports real Owner state and raises exactly the
    /// Owner-derived issues; it never mentions kernel queue occupancy.
    #[tokio::test]
    async fn srt_listener_owner_check_reports_owner_state_and_issues() {
        let engine = Arc::new(MediaEngine::new());
        let stats = engine.listener_stats_handle();
        stats
            .bonding_available
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let healthy = check_srt_listener_owner(8, &engine).await;
        assert_eq!(healthy.name, "SRT Listener Owner");
        assert!(healthy.issues.is_empty(), "{:?}", healthy.issues);
        assert!(healthy.stdout.contains("Owner faulted: false"));
        assert!(!healthy.stdout.contains("UDP recv queue"));

        let owner = &stats.ingress_owner;
        owner
            .faulted
            .store(true, std::sync::atomic::Ordering::Relaxed);
        owner
            .rx_ring_dropped
            .store(3, std::sync::atomic::Ordering::Relaxed);
        owner
            .rx_truncated
            .store(2, std::sync::atomic::Ordering::Relaxed);
        owner
            .tx_failed
            .store(1, std::sync::atomic::Ordering::Relaxed);
        let unhealthy = check_srt_listener_owner(8, &engine).await;
        assert_eq!(unhealthy.issues.len(), 4, "{:?}", unhealthy.issues);
    }

    #[test]
    fn media_root_path_joins_relative_dir_onto_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let resolved = media_root_path(".");
        assert_eq!(resolved, std::fs::canonicalize(&cwd).unwrap_or(cwd));
    }

    #[test]
    fn media_root_path_falls_back_to_joined_path_when_nonexistent() {
        let resolved = media_root_path("definitely-does-not-exist-xyz-123");
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        assert_eq!(resolved, cwd.join("definitely-does-not-exist-xyz-123"));
    }

    #[test]
    fn media_root_path_preserves_absolute_nonexistent_path_unchanged() {
        let absolute = PathBuf::from("/definitely-does-not-exist-xyz-123");
        let resolved = media_root_path(absolute.to_str().unwrap());
        assert_eq!(resolved, absolute);
    }

    #[test]
    fn media_root_path_handles_empty_input_as_relative_to_cwd() {
        let resolved = media_root_path("");
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        assert_eq!(resolved, std::fs::canonicalize(&cwd).unwrap_or(cwd));
    }

    #[test]
    fn disk_for_path_returns_none_when_no_mount_is_a_prefix() {
        let disks = Disks::new();
        let bogus = Path::new("/definitely-not-a-real-mount-xyz-123");
        assert!(disk_for_path(&disks, bogus).is_none());
    }
}
