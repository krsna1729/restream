//! Kernel settings Restream depends on.
//!
//! `restream host-check` reports them; `sudo restream host-tune` applies the
//! safe ones once and persists them; the server logs one warning per unmet
//! setting at startup. The server itself never needs root.

use std::path::Path;

/// Ceiling for SRT UDP receive-buffer requests (Linux caps `SO_RCVBUF` at it).
pub const REQUIRED_RMEM_MAX: u64 = 26_214_400;
/// Ceiling for SRT UDP send-buffer requests (8 MiB).
pub const REQUIRED_WMEM_MAX: u64 = 8_388_608;
/// `99-` so it applies after most distro and earlier Restream drop-ins.
const SYSCTL_CONF: &str = "/etc/sysctl.d/99-restream.conf";
const MODULES_CONF: &str = "/etc/modules-load.d/restream.conf";
const TUNE_HINT: &str = "run `sudo restream host-tune`";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Check {
    /// `/proc/sys` value must be at least this; `host-tune` raises it.
    SysctlAtLeast(u64),
    /// Kernel module must be loaded; `host-tune` loads and persists it.
    Module,
    /// Report only: `/proc/sys` value must equal this.
    SysctlEquals(u64),
    /// Report only: the ephemeral port range must span at least this many ports.
    PortRangeAtLeast(u64),
    /// Report only: the process `RLIMIT_NOFILE` hard limit.
    NofileHardAtLeast(u64),
}

impl Check {
    fn tunable(self) -> bool {
        matches!(self, Self::SysctlAtLeast(_) | Self::Module)
    }
}

struct Requirement {
    key: &'static str,
    check: Check,
    why: &'static str,
    /// What to do for a report-only requirement that `host-tune` leaves alone.
    advice: &'static str,
}

const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        key: "net.core.rmem_max",
        check: Check::SysctlAtLeast(REQUIRED_RMEM_MAX),
        why: "SRT UDP receive buffers",
        advice: TUNE_HINT,
    },
    Requirement {
        key: "net.core.wmem_max",
        check: Check::SysctlAtLeast(REQUIRED_WMEM_MAX),
        why: "SRT UDP send buffers",
        advice: TUNE_HINT,
    },
    Requirement {
        key: "net.core.somaxconn",
        check: Check::SysctlAtLeast(4096),
        why: "RTMP accept backlog under connection bursts",
        advice: TUNE_HINT,
    },
    Requirement {
        key: "tls",
        check: Check::Module,
        why: "kernel TLS for RTMPS outputs",
        advice: TUNE_HINT,
    },
    Requirement {
        key: "kernel.io_uring_disabled",
        check: Check::SysctlEquals(0),
        why: "Compio owner threads need io_uring",
        advice: "io_uring is disabled by host policy; set kernel.io_uring_disabled=0 if that policy allows it",
    },
    Requirement {
        key: "net.ipv4.ip_local_port_range",
        check: Check::PortRangeAtLeast(4096),
        why: "ephemeral ports for many concurrent outputs",
        advice: "widen net.ipv4.ip_local_port_range",
    },
    Requirement {
        key: "RLIMIT_NOFILE (hard)",
        check: Check::NofileHardAtLeast(65_536),
        why: "sockets and files for many outputs",
        advice: "set LimitNOFILE=65536 in the systemd unit (or /etc/security/limits.d for a shell)",
    },
];

#[derive(Debug, PartialEq)]
enum Status {
    Met(String),
    Unmet(String),
    Unknown,
}

/// Judges one requirement from its observed text: a `/proc/sys` value, the
/// RLIMIT hard limit, or `Some("loaded")`/`None` for a module.
fn evaluate(check: Check, observed: Option<&str>) -> Status {
    let number = |text: &str| text.trim().parse::<u64>().ok();
    match (check, observed) {
        (Check::Module, Some(_)) => Status::Met("loaded".into()),
        (Check::Module, None) => Status::Unmet("not loaded".into()),
        (_, None) => Status::Unknown,
        (Check::SysctlAtLeast(min) | Check::NofileHardAtLeast(min), Some(text)) => {
            match number(text) {
                Some(value) if value >= min => Status::Met(value.to_string()),
                Some(value) => Status::Unmet(value.to_string()),
                None => Status::Unknown,
            }
        }
        (Check::SysctlEquals(want), Some(text)) => match number(text) {
            Some(value) if value == want => Status::Met(value.to_string()),
            Some(value) => Status::Unmet(value.to_string()),
            None => Status::Unknown,
        },
        (Check::PortRangeAtLeast(min), Some(text)) => {
            let mut bounds = text
                .split_whitespace()
                .filter_map(|part| part.parse::<u64>().ok());
            match (bounds.next(), bounds.next()) {
                (Some(low), Some(high)) if high >= low => {
                    let ports = high - low + 1;
                    let shown = format!("{low}-{high} ({ports} ports)");
                    if ports >= min {
                        Status::Met(shown)
                    } else {
                        Status::Unmet(shown)
                    }
                }
                _ => Status::Unknown,
            }
        }
    }
}

fn need(check: Check) -> String {
    match check {
        Check::SysctlAtLeast(min) | Check::NofileHardAtLeast(min) => format!(">= {min}"),
        Check::SysctlEquals(want) => format!("= {want}"),
        Check::PortRangeAtLeast(min) => format!(">= {min} ports"),
        Check::Module => "loaded".into(),
    }
}

fn proc_sys_path(key: &str) -> String {
    format!("/proc/sys/{}", key.replace('.', "/"))
}

fn observe(requirement: &Requirement) -> Option<String> {
    match requirement.check {
        Check::Module => Path::new("/sys/module")
            .join(requirement.key)
            .exists()
            .then(|| "loaded".to_string()),
        Check::NofileHardAtLeast(_) => nofile_hard().map(|hard| hard.to_string()),
        _ => std::fs::read_to_string(proc_sys_path(requirement.key)).ok(),
    }
}

#[cfg(unix)]
fn nofile_hard() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only to the provided struct, which outlives
    // the call.
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0).then_some(limit.rlim_max)
}

#[cfg(not(unix))]
fn nofile_hard() -> Option<u64> {
    None
}

/// Renders the persisted sysctl file. A value already above the minimum is
/// kept, so tuning never lowers a host setting.
fn sysctl_conf(current: impl Fn(&str) -> Option<u64>) -> String {
    let mut conf = String::from("# Written by `restream host-tune`.\n");
    for requirement in REQUIREMENTS {
        if let Check::SysctlAtLeast(min) = requirement.check {
            let value = current(requirement.key).map_or(min, |value| value.max(min));
            conf.push_str(&format!(
                "# {}\n{} = {value}\n",
                requirement.why, requirement.key
            ));
        }
    }
    conf
}

fn print_report() -> bool {
    let mut all_met = true;
    for requirement in REQUIREMENTS {
        let status = evaluate(requirement.check, observe(requirement).as_deref());
        let (tag, current) = match &status {
            Status::Met(current) => ("ok  ", current.as_str()),
            Status::Unmet(current) => ("FAIL", current.as_str()),
            Status::Unknown => ("??  ", "unreadable"),
        };
        println!(
            "[{tag}] {} = {current} (need {}): {}",
            requirement.key,
            need(requirement.check),
            requirement.why
        );
        if !matches!(status, Status::Met(_)) {
            all_met = false;
            println!("       -> {}", requirement.advice);
        }
    }
    all_met
}

/// `restream host-check`: report every requirement; exit status 1 if any is
/// unmet or unreadable.
pub fn host_check() -> i32 {
    if print_report() { 0 } else { 1 }
}

/// `restream host-tune`: raise sysctl ceilings, load and persist kernel
/// modules, then report. Report-only requirements are left untouched.
pub fn host_tune() -> i32 {
    #[cfg(unix)]
    // SAFETY: geteuid has no preconditions.
    let root = unsafe { libc::geteuid() } == 0;
    #[cfg(not(unix))]
    let root = false;
    if !root {
        eprintln!("host-tune changes kernel settings and needs root: sudo restream host-tune");
        return 1;
    }

    let current = |key: &str| {
        std::fs::read_to_string(proc_sys_path(key))
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let mut failed = false;
    if let Err(error) = std::fs::write(SYSCTL_CONF, sysctl_conf(current)) {
        eprintln!("failed to write {SYSCTL_CONF}: {error}");
        failed = true;
    }
    for requirement in REQUIREMENTS.iter().filter(|r| r.check.tunable()) {
        match requirement.check {
            Check::SysctlAtLeast(min) if current(requirement.key).is_some_and(|v| v < min) => {
                if let Err(error) = std::fs::write(proc_sys_path(requirement.key), min.to_string())
                {
                    eprintln!("failed to set {}: {error}", requirement.key);
                    failed = true;
                }
            }
            Check::Module => {
                let loaded = std::process::Command::new("modprobe")
                    .arg(requirement.key)
                    .status()
                    .is_ok_and(|status| status.success());
                if !loaded {
                    eprintln!("modprobe {} failed", requirement.key);
                    failed = true;
                }
                if let Err(error) = std::fs::write(MODULES_CONF, format!("{}\n", requirement.key)) {
                    eprintln!("failed to write {MODULES_CONF}: {error}");
                    failed = true;
                }
            }
            _ => {}
        }
    }
    if !failed {
        println!("persisted {SYSCTL_CONF} and {MODULES_CONF}");
    }
    let all_met = print_report();
    if failed || !all_met { 1 } else { 0 }
}

/// Logs one warning per unmet or unreadable requirement at server startup.
pub fn warn_unmet() {
    for requirement in REQUIREMENTS {
        let observed = observe(requirement);
        let status = evaluate(requirement.check, observed.as_deref());
        if let Status::Met(_) = status {
            continue;
        }
        tracing::warn!(
            event_class = "lifecycle",
            event_type = "restream.host.setting_unmet",
            setting = requirement.key,
            current = ?status,
            need = %need(requirement.check),
            reason = requirement.why,
            fix = requirement.advice,
            "host setting below Restream's requirement",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_checks_treat_the_boundary_as_met() {
        let check = Check::SysctlAtLeast(4096);
        assert_eq!(evaluate(check, Some("4096\n")), Status::Met("4096".into()));
        assert_eq!(evaluate(check, Some("4095")), Status::Unmet("4095".into()));
        assert_eq!(evaluate(check, Some("garbage")), Status::Unknown);
        assert_eq!(evaluate(check, None), Status::Unknown);
    }

    #[test]
    fn a_missing_module_is_unmet_not_unknown() {
        assert_eq!(
            evaluate(Check::Module, None),
            Status::Unmet("not loaded".into())
        );
    }

    #[test]
    fn port_range_counts_inclusive_ports() {
        let check = Check::PortRangeAtLeast(4096);
        assert!(matches!(
            evaluate(check, Some("32768\t60999\n")),
            Status::Met(_)
        ));
        assert_eq!(
            evaluate(check, Some("60000 64094")),
            Status::Unmet("60000-64094 (4095 ports)".into())
        );
        assert_eq!(evaluate(check, Some("61000 60000")), Status::Unknown);
    }

    #[test]
    fn io_uring_must_be_fully_enabled() {
        let check = Check::SysctlEquals(0);
        assert!(matches!(evaluate(check, Some("0")), Status::Met(_)));
        // 1 restricts io_uring to a group; Restream does not run in that group.
        assert_eq!(evaluate(check, Some("1")), Status::Unmet("1".into()));
    }

    #[test]
    fn tuning_raises_low_values_and_never_lowers_high_ones() {
        let conf = sysctl_conf(|key| match key {
            "net.core.rmem_max" => Some(212_992),
            "net.core.wmem_max" => Some(67_108_864),
            _ => None,
        });
        assert!(conf.contains(&format!("net.core.rmem_max = {REQUIRED_RMEM_MAX}\n")));
        assert!(conf.contains("net.core.wmem_max = 67108864\n"));
        assert!(conf.contains("net.core.somaxconn = 4096\n"));
        // Report-only settings are never persisted.
        assert!(!conf.contains("io_uring_disabled"));
        assert!(!conf.contains("ip_local_port_range"));
    }
}
