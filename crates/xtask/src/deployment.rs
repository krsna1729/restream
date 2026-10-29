//! Host service installation and the standalone, repository-independent launcher.
use crate::workflow::{Error, WorkflowResult, checked, output};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const INSTALL_USAGE: &str = "usage: cargo xtask deploy install-systemd-service [--now]\n\nInstalls /etc/systemd/system/restream.service (root required).\nEnvironment overrides: RESTREAM_SERVICE_NAME (restream), RESTREAM_BIN (/usr/local/bin/restream),\nRESTREAM_USER (restream), RESTREAM_GROUP (restream), RESTREAM_WORKDIR (/var/lib/restream),\nRESTREAM_ENV_FILE (/etc/restream/restream.env), RESTREAM_CPU_AFFINITY, RESTREAM_NUMA_POLICY,\nRESTREAM_NUMA_MASK. Use --now to enable and restart the installed unit.";

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let Some((&"install-systemd-service", args)) = args.split_first() else {
        return Err(Error::usage(INSTALL_USAGE));
    };
    let mut now = false;
    for arg in args {
        match *arg {
            "--now" => now = true,
            "-h" | "--help" => {
                println!("{INSTALL_USAGE}");
                return Ok(());
            }
            _ => return Err(Error::usage(INSTALL_USAGE)),
        }
    }
    install_service(now)
}

struct Service {
    name: String,
    bin: String,
    user: String,
    group: String,
    workdir: String,
    env_file: String,
    cpu: String,
    numa_policy: String,
    numa_mask: String,
}

fn setting(name: &str, default: &str) -> Result<String, Error> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(error) => Err(format!("{name}: {error}").into()),
    }
}

impl Service {
    fn from_env() -> Result<Self, Error> {
        let service = Self {
            name: setting("RESTREAM_SERVICE_NAME", "restream")?,
            bin: setting("RESTREAM_BIN", "/usr/local/bin/restream")?,
            user: setting("RESTREAM_USER", "restream")?,
            group: setting("RESTREAM_GROUP", "restream")?,
            workdir: setting("RESTREAM_WORKDIR", "/var/lib/restream")?,
            env_file: setting("RESTREAM_ENV_FILE", "/etc/restream/restream.env")?,
            cpu: setting("RESTREAM_CPU_AFFINITY", "")?,
            numa_policy: setting("RESTREAM_NUMA_POLICY", "")?,
            numa_mask: setting("RESTREAM_NUMA_MASK", "")?,
        };
        service.validate()?;
        Ok(service)
    }

    fn validate(&self) -> WorkflowResult {
        if self.name.is_empty()
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.@:".contains(&b))
        {
            return Err(
                "RESTREAM_SERVICE_NAME must be a single systemd unit name, without a path".into(),
            );
        }
        for (name, value) in [
            ("RESTREAM_USER", &self.user),
            ("RESTREAM_GROUP", &self.group),
        ] {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.$".contains(&b))
            {
                return Err(format!("{name} must be a user/group name or numeric identity").into());
            }
        }
        for (name, value) in [
            ("RESTREAM_BIN", &self.bin),
            ("RESTREAM_WORKDIR", &self.workdir),
            ("RESTREAM_ENV_FILE", &self.env_file),
        ] {
            validate_line(name, value)?;
            if !Path::new(value).is_absolute() {
                return Err(format!("{name} must be an absolute path").into());
            }
        }
        for (name, value) in [
            ("RESTREAM_CPU_AFFINITY", &self.cpu),
            ("RESTREAM_NUMA_POLICY", &self.numa_policy),
            ("RESTREAM_NUMA_MASK", &self.numa_mask),
        ] {
            validate_line(name, value)?;
        }
        Ok(())
    }

    fn unit(&self) -> String {
        let mut unit = format!(
            "[Unit]\nDescription=Restream media runtime\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser={}\nGroup={}\nWorkingDirectory={}\nEnvironmentFile=-{}\nExecStart={}\nRestart=on-failure\nRestartSec=2s\nLimitNOFILE=65536\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=full\nProtectHome=true\nReadWritePaths={}\n",
            self.user,
            self.group,
            quote_unit_path(&self.workdir, false),
            quote_unit_path(&self.env_file, false),
            quote_unit_path(&self.bin, true),
            quote_unit_path(&self.workdir, false)
        );
        for (name, value) in [
            ("CPUAffinity", &self.cpu),
            ("NUMAPolicy", &self.numa_policy),
            ("NUMAMask", &self.numa_mask),
        ] {
            if !value.is_empty() {
                unit.push_str(&format!("{name}={}\n", value.replace('%', "%%")));
            }
        }
        unit.push_str("\n[Install]\nWantedBy=multi-user.target\n");
        unit
    }
}

fn validate_line(name: &str, value: &str) -> WorkflowResult {
    if value.chars().any(char::is_control) {
        return Err(format!("{name} must not contain control characters").into());
    }
    // A trailing backslash continues the systemd directive onto the next line.
    if value.ends_with('\\') {
        return Err(format!("{name} must not end in a backslash").into());
    }
    Ok(())
}

fn quote_unit_path(value: &str, exec: bool) -> String {
    let mut quoted = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' if exec => quoted.push_str("$$"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn install_service(now: bool) -> WorkflowResult {
    #[cfg(unix)]
    // SAFETY: geteuid only reads this process's effective user ID.
    if unsafe { libc::geteuid() } != 0 {
        return Err("must run as root to write systemd unit files".into());
    }
    #[cfg(not(unix))]
    return Err("systemd service installation requires a Unix host".into());
    #[cfg(unix)]
    {
        let service = Service::from_env()?;
        let unit_name = format!("{}.service", service.name);
        let unit_path = Path::new("/etc/systemd/system").join(&unit_name);
        let temp = output(&mut Command::new("mktemp"))?;
        let temp_path = PathBuf::from(
            String::from_utf8(temp.stdout)
                .map_err(|e| e.to_string())?
                .trim_end_matches('\n'),
        );
        let _cleanup = TempUnit(temp_path.clone());
        fs::create_dir_all(
            Path::new(&service.env_file)
                .parent()
                .ok_or("environment file has no parent")?,
        )?;
        checked(Command::new("install").args([
            "-d",
            "-m",
            "0750",
            "-o",
            &service.user,
            "-g",
            &service.group,
            "--",
            &service.workdir,
        ]))?;
        if !Path::new(&service.env_file).is_file() {
            checked(Command::new("install").args([
                "-m",
                "0640",
                "-o",
                "root",
                "-g",
                "root",
                "--",
                "/dev/null",
                &service.env_file,
            ]))?;
        }
        fs::write(&temp_path, service.unit())?;
        checked(
            Command::new("install")
                .args(["-m", "0644", "-o", "root", "-g", "root", "--"])
                .arg(&temp_path)
                .arg(&unit_path),
        )?;
        checked(Command::new("systemctl").arg("daemon-reload"))?;
        println!("installed {}", unit_path.display());
        if !service.cpu.is_empty()
            || !service.numa_policy.is_empty()
            || !service.numa_mask.is_empty()
        {
            println!("placement:");
            for (key, value) in [
                ("CPUAffinity", &service.cpu),
                ("NUMAPolicy", &service.numa_policy),
                ("NUMAMask", &service.numa_mask),
            ] {
                if !value.is_empty() {
                    println!("  {key}={value}");
                }
            }
        }
        if now {
            checked(Command::new("systemctl").args(["enable", "--", &unit_name]))?;
            checked(Command::new("systemctl").args(["restart", "--", &unit_name]))?;
            checked(Command::new("systemctl").args([
                "--no-pager",
                "--full",
                "status",
                "--",
                &unit_name,
            ]))?;
        } else {
            println!("run 'systemctl enable --now {unit_name}' when ready");
        }
        Ok(())
    }
}

struct TempUnit(PathBuf);
impl Drop for TempUnit {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn parse_wrap<'a>(args: &'a [&'a str]) -> Result<(&'a str, &'a [&'a str]), Error> {
    let (scope, rest) = if args.first() == Some(&"--scope") {
        if args.len() < 2 {
            return Err(Error::usage("cgroup-wrap: --scope requires a name"));
        }
        (args[1], &args[2..])
    } else {
        ("", args)
    };
    if rest.first() != Some(&"--") {
        return Err(Error::usage(
            "usage: restream-cgroup-wrap [--scope <name>] -- <command> [args...]",
        ));
    }
    if rest.len() == 1 {
        return Err(Error::usage("cgroup-wrap: missing command"));
    }
    Ok((scope, &rest[1..]))
}

fn systemd_scope_command(scope: &str, command: &[&str], limits: &[(&str, OsString)]) -> Command {
    let mut run = Command::new("systemd-run");
    let unit = if scope.is_empty() {
        format!("restream-harness-{}", std::process::id())
    } else {
        format!("restream-harness-{scope}-{}", std::process::id())
    };
    run.args([
        "--user",
        "--scope",
        "--quiet",
        "--unit",
        &unit,
        "--same-dir",
    ]);
    for (property, value) in limits {
        if !value.is_empty() {
            let mut assignment = OsString::from(format!("{property}="));
            assignment.push(value);
            run.arg("--property").arg(assignment);
        }
    }
    run.arg("--").args(command);
    run
}

/// `command -v systemd-run`, with Bash's default search path when PATH is unset.
fn systemd_run_available() -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_else(|| OsString::from("/bin:/usr/bin")))
        .any(|dir| crate::workflow::executable(&dir.join("systemd-run")))
}

/// Exec, rather than spawn/wait: preserve PID, signals, cgroup membership and exit status.
pub(crate) fn cgroup_wrap(args: &[&str]) -> WorkflowResult {
    let (scope, command) = parse_wrap(args)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let limits = [
            (
                "CPUQuota",
                env::var_os("HARNESS_CGROUP_CPU_MAX").unwrap_or_default(),
            ),
            (
                "MemoryMax",
                env::var_os("HARNESS_CGROUP_MEMORY_MAX").unwrap_or_default(),
            ),
            (
                "TasksMax",
                env::var_os("HARNESS_CGROUP_PIDS_MAX").unwrap_or_default(),
            ),
        ];
        let mut run = if limits.iter().any(|(_, v)| !v.is_empty()) && systemd_run_available() {
            systemd_scope_command(scope, command, &limits)
        } else {
            let mut run = Command::new(command[0]);
            run.args(&command[1..]);
            run
        };
        let error = run.exec();
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            127
        } else {
            126
        };
        Err(Error::exit(
            code,
            format!(
                "cgroup-wrap: {}: {error}",
                run.get_program().to_string_lossy()
            ),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (scope, command);
        Err("cgroup-wrap requires a Unix host".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Service {
        Service {
            name: "restream".into(),
            bin: "/opt/media tools/restream$bin%".into(),
            user: "restream".into(),
            group: "restream".into(),
            workdir: "/var/lib/media files".into(),
            env_file: "/etc/media files/runtime.env".into(),
            cpu: "0-5".into(),
            numa_policy: "local".into(),
            numa_mask: "0".into(),
        }
    }
    #[test]
    fn rejects_directive_injection_and_relative_paths() {
        let mut config = service();
        config.name = "../escape".into();
        assert!(config.validate().is_err());
        config.name = "restream".into();
        config.cpu = "0\nExecStart=/bad".into();
        assert!(config.validate().is_err());
        config.cpu = "0\\".into();
        assert!(config.validate().is_err());
        config.cpu.clear();
        config.bin = "relative/restream".into();
        assert!(config.validate().is_err());
    }
    #[test]
    fn systemd_paths_stay_single_literal_values() {
        let service = service();
        service.validate().unwrap();
        let unit = service.unit();
        assert!(unit.contains("ExecStart=\"/opt/media tools/restream$$bin%%\"\n"));
        assert!(unit.contains("EnvironmentFile=-\"/etc/media files/runtime.env\"\n"));
        assert!(unit.contains("ReadWritePaths=\"/var/lib/media files\"\n"));
        assert_eq!(quote_unit_path("/a\\b\"c", false), "\"/a\\\\b\\\"c\"");
    }
    #[test]
    fn scope_limits_and_payload_are_separate_arguments() {
        let command = systemd_scope_command(
            "media",
            &["/a path/bin", "arg with spaces", "--"],
            &[
                ("CPUQuota", "150%".into()),
                ("MemoryMax", "2G".into()),
                ("TasksMax", "256".into()),
            ],
        );
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(
            args.windows(2)
                .any(|p| p == ["--property", "CPUQuota=150%"])
        );
        assert!(args.windows(2).any(|p| p == ["--property", "MemoryMax=2G"]));
        assert!(args.windows(2).any(|p| p == ["--property", "TasksMax=256"]));
        assert_eq!(
            &args[args.len() - 4..],
            ["--", "/a path/bin", "arg with spaces", "--"]
        );
    }
    #[test]
    fn parser_requires_separator_and_command() {
        assert!(parse_wrap(&[]).is_err());
        assert!(parse_wrap(&["--scope"]).is_err());
        assert!(parse_wrap(&["--"]).is_err());
        assert!(parse_wrap(&["--scope", "name", "program"]).is_err());
        assert_eq!(
            parse_wrap(&["--scope", "name", "--", "program", "a b"]).unwrap(),
            ("name", &["program", "a b"][..])
        );
    }
}
