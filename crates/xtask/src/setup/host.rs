use crate::workflow::{Error, WorkflowResult, checked};
use std::{
    fs,
    process::{Command, Stdio},
};

const CONFIG: &str = "# Required for Restream's private-loopback live harness and 8 MiB SRT UDP buffers.\nkernel.unprivileged_userns_clone=1\nuser.max_user_namespaces=28633\nnet.core.rmem_max=26214400\nnet.core.wmem_max=8388608\n";
const USAGE: &str = "Usage: xtask setup harness-host-prereqs [--configure]\nChecks private user/network namespaces, process limits, SRT UDP buffers, listener\nbacklog and ephemeral ports. --configure persists the approved sysctls to\n/etc/sysctl.d/99-restream-harness.conf and applies them. Never disables AppArmor.";

fn configure_flag(args: &[&str]) -> Result<Option<bool>, Error> {
    let mut configure = false;
    for arg in args {
        match *arg {
            "--configure" => configure = true,
            "-h" | "--help" => return Ok(None),
            other => {
                return Err(Error::usage(format!(
                    "harness-host-prereqs: unknown argument: {other}\n{USAGE}"
                )));
            }
        }
    }
    Ok(Some(configure))
}

fn as_root(program: &str) -> Result<Command, Error> {
    // SAFETY: geteuid only reads this process's effective user ID.
    if unsafe { libc::geteuid() } == 0 {
        return Ok(Command::new(program));
    }
    if !crate::workflow::on_path("sudo") {
        return Err("harness-host-prereqs: need sudo to configure host settings".into());
    }
    let mut command = Command::new("sudo");
    command.arg(program);
    Ok(command)
}

fn value(key: &str) -> String {
    fs::read_to_string(format!("/proc/sys/{}", key.replace('.', "/")))
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}
fn sufficient(value: &str, threshold: u64) -> bool {
    value.parse::<u64>().is_ok_and(|value| value >= threshold)
}
fn port_count(value: &str) -> Option<i64> {
    let mut fields = value.split_whitespace();
    let low = fields.next()?.parse::<u32>().ok()?;
    let high = fields.next()?.parse::<u32>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(i64::from(high) - i64::from(low) + 1)
}

fn limits() -> (String, String) {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: getrlimit fully initializes `limit` when it returns zero, and
    // it is only read after that check.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == 0 {
        // SAFETY: initialized by the successful getrlimit above.
        let limit = unsafe { limit.assume_init() };
        let render = |value| {
            if value == libc::RLIM_INFINITY {
                "unlimited".to_owned()
            } else {
                format!("{value}")
            }
        };
        return (render(limit.rlim_cur), render(limit.rlim_max));
    }
    ("unavailable".to_owned(), "unavailable".to_owned())
}

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    let Some(configure) = configure_flag(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    if !cfg!(target_os = "linux") {
        eprintln!("harness-host-prereqs: Linux host settings are not applicable");
        return Ok(());
    }
    if configure {
        let temp = crate::workflow::TempDir::new("restream-xtask")?;
        let conf = temp.path().join("99-restream-harness.conf");
        fs::write(&conf, CONFIG)?;
        checked(
            as_root("install")?
                .args(["-m", "0644"])
                .arg(&conf)
                .arg("/etc/sysctl.d/99-restream-harness.conf"),
        )?;
        checked(as_root("sysctl")?.arg("--system").stdout(Stdio::null()))?;
        println!("harness-host-prereqs: persisted live-harness sysctls");
    }
    let namespaces = match crate::workflow::spawn(
        Command::new("unshare")
            .args(["--user", "--map-root-user", "--net", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    ) {
        Ok(mut child) => crate::workflow::wait(&mut child)?.success(),
        Err(error) if crate::workflow::interrupted().is_some() => return Err(error),
        Err(_) => false,
    };
    if namespaces {
        println!("harness-host-prereqs: private live-harness network namespaces are available");
    } else {
        eprintln!(
            "harness-host-prereqs: private live-harness network namespaces are unavailable.\nUse --no-netns only as a temporary fallback, or configure this host explicitly:\n  cargo xtask setup harness-host-prereqs --configure\n\nIf AppArmor still blocks unshare afterwards, ask the host administrator to\napprove that policy change; this helper never disables AppArmor."
        );
    }
    let (soft, hard) = limits();
    println!("harness-host-prereqs: nofile soft={soft} hard={hard}");
    if sufficient(&hard, 65536) {
        println!(
            "harness-host-prereqs: RLIMIT_NOFILE hard limit supports Restream and 1,200-output MSR"
        );
    } else {
        eprintln!(
            "harness-host-prereqs: RLIMIT_NOFILE hard limit is below the 65,536 production/MSR policy.\nThe harness raises its soft limit automatically, but cannot raise the hard limit.\nFor systemd, configure LimitNOFILE=65536 (or higher); for an interactive shell,\nstart it from a session whose `ulimit -Hn` is at least 65536."
        );
    }
    let backlog = value("net.core.somaxconn");
    println!(
        "harness-host-prereqs: net.core.somaxconn={backlog} net.core.netdev_max_backlog={} net.ipv4.udp_mem={} fs.file-max={}",
        value("net.core.netdev_max_backlog"),
        value("net.ipv4.udp_mem"),
        value("fs.file-max")
    );
    if sufficient(&backlog, 4096) {
        println!("harness-host-prereqs: TCP listener backlog supports the 1,200-output MSR target");
    } else {
        eprintln!(
            "harness-host-prereqs: net.core.somaxconn should be at least 4096 for 1,200 concurrent RTMP peers"
        );
    }
    let ports = value("net.ipv4.ip_local_port_range");
    if let Some(count) = port_count(&ports) {
        println!("harness-host-prereqs: net.ipv4.ip_local_port_range={ports} ({count} ports)");
        if count < 4096 {
            eprintln!(
                "harness-host-prereqs: local ephemeral-port range should provide at least 4096 ports for MSR"
            );
        }
    } else {
        eprintln!("harness-host-prereqs: net.ipv4.ip_local_port_range={ports}");
    }
    let rmem = value("net.core.rmem_max");
    let wmem = value("net.core.wmem_max");
    if sufficient(&rmem, 26214400) && sufficient(&wmem, 8388608) {
        println!("harness-host-prereqs: SRT UDP buffer ceilings satisfy the 8 MiB policy");
    } else {
        eprintln!(
            "harness-host-prereqs: SRT UDP buffer ceilings are below the live-harness policy\n(rmem_max={rmem}, wmem_max={wmem}; need at least 26214400 and 8388608).\nConfigure them explicitly with:\n  cargo xtask setup harness-host-prereqs --configure"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_requires_explicit_flag_and_unknown_flags_fail() {
        assert_eq!(configure_flag(&[]).unwrap(), Some(false));
        assert_eq!(configure_flag(&["--configure"]).unwrap(), Some(true));
        assert_eq!(configure_flag(&["--help"]).unwrap(), None);
        assert!(configure_flag(&["--disable-apparmor"]).is_err());
    }
    #[test]
    fn readiness_boundaries() {
        assert!(!sufficient("26214399", 26214400));
        assert!(sufficient("26214400", 26214400));
        assert!(!sufficient("unavailable", 1));
        assert_eq!(port_count("10000\t14095"), Some(4096));
        assert_eq!(port_count("10000 14094"), Some(4095));
        assert_eq!(port_count("1 2 3"), None);
    }
}
