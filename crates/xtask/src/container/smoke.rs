use super::process::{OwnedChild, Signals, execute, install_executable, quiet, quiet_capture};
use crate::workflow::{Error, WorkflowResult, output, unique_dir};
use regex::Regex;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const HELP: &str = "usage: cargo xtask container smoke [--image NAME] [--archive PATH] [--load-archive PATH] [--seccomp-profile PATH] [--use-existing-image] [--diagnostic-unconfined]\nProves non-root, mount-free health plus real SRT and RTMP-to-RTMPS media under the shipped seccomp profile. CONTAINER_ENGINE selects the engine; CONTAINER_EXTRA_ARGS is whitespace-split. RESTREAM_HARNESS_BIN selects the live harness. RESTREAM_CONTAINER_SMOKE_REPORT writes durable JSON/log evidence. RESTREAM_CONTAINER_SMOKE_SKIP_SRT=1 skips ALL live transport proofs (never in CI). Unconfined is a diagnostic control, never a deployment recommendation.";

struct Options {
    root: PathBuf,
    image: String,
    engine: String,
    profile: PathBuf,
    archive: Option<PathBuf>,
    load: Option<PathBuf>,
    existing: bool,
    diagnostic: bool,
}
impl Options {
    fn parse(args: &[&str]) -> Result<Option<Self>, Error> {
        let root = crate::repo_root().canonicalize()?;
        let mut options = Self {
            profile: root.join("distribution/docker/restream-seccomp.json"),
            root,
            image: crate::workflow::env_or("RESTREAM_CONTAINER_IMAGE", "restream:release-smoke"),
            engine: crate::workflow::env_or("CONTAINER_ENGINE", "docker"),
            archive: None,
            load: None,
            existing: false,
            diagnostic: false,
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match *arg {
                "--image" | "--archive" | "--load-archive" | "--seccomp-profile" => {
                    let value = args
                        .next()
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| Error::from(format!("{arg} requires a value\n{HELP}")))?;
                    match *arg {
                        "--image" => options.image = (*value).into(),
                        "--archive" => options.archive = Some(options.path(value)),
                        "--load-archive" => options.load = Some(options.path(value)),
                        _ => options.profile = options.path(value),
                    }
                }
                "--use-existing-image" => options.existing = true,
                "--diagnostic-unconfined" => options.diagnostic = true,
                "-h" | "--help" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                _ => {
                    return Err(Error::usage(format!(
                        "container-smoke: unknown argument: {arg}\n{HELP}"
                    )));
                }
            }
        }
        Ok(Some(options))
    }
    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
    fn engine(&self) -> Command {
        let mut c = Command::new(&self.engine);
        c.current_dir(&self.root);
        c
    }
    fn text(&self, args: &[&str]) -> Result<String, Error> {
        Ok(
            String::from_utf8_lossy(&output(self.engine().args(args))?.stdout)
                .trim()
                .into(),
        )
    }
}

struct Smoke {
    options: Options,
    name: String,
    label: String,
    dirs: Vec<PathBuf>,
    signals: Signals,
    stage: &'static str,
    passed: bool,
    default_health: &'static str,
    default_srt: &'static str,
    default_note: String,
    shipped_health: &'static str,
    shipped_srt: &'static str,
    shipped_rtmps: &'static str,
    unconfined_srt: &'static str,
    probe_logs: String,
    default_logs: String,
    shipped_logs: String,
    srt_log: String,
    rtmps_log: String,
    rtmps_harness_log: String,
}
impl Smoke {
    fn new(options: Options) -> Self {
        Self {
            options,
            name: format!("restream-release-smoke-{}", std::process::id()),
            label: format!("restream.container-smoke={}", std::process::id()),
            dirs: Vec::new(),
            signals: Signals,
            stage: "start",
            passed: false,
            default_health: "not-run",
            default_srt: "not-run",
            default_note: String::new(),
            shipped_health: "not-run",
            shipped_srt: "not-run",
            shipped_rtmps: "not-run",
            unconfined_srt: "not-run",
            probe_logs: String::new(),
            default_logs: String::new(),
            shipped_logs: String::new(),
            srt_log: String::new(),
            rtmps_log: String::new(),
            rtmps_harness_log: String::new(),
        }
    }
    fn dir(&mut self) -> Result<PathBuf, Error> {
        let dir = unique_dir(&std::env::temp_dir(), "restream-container-smoke")?;
        self.dirs.push(dir.clone());
        Ok(dir)
    }
    fn remove_health(&self) {
        quiet(self.options.engine().args(["rm", "-f", &self.name]));
    }
    fn image(&self) -> WorkflowResult {
        let o = &self.options;
        if o.existing {
            return Ok(());
        }
        if let Some(archive) = &o.load {
            require_nonempty(archive, "image archive")?;
            return execute(
                o.engine().arg("load").arg("-i").arg(archive),
                &self.signals,
                None,
            );
        }
        let commit = String::from_utf8_lossy(
            &output(
                Command::new("git")
                    .current_dir(&o.root)
                    .args(["rev-parse", "HEAD"]),
            )?
            .stdout,
        )
        .trim()
        .to_owned();
        let timestamp = String::from_utf8_lossy(
            &output(Command::new("git").current_dir(&o.root).args([
                "show",
                "-s",
                "--format=%cI",
                "HEAD",
            ]))?
            .stdout,
        )
        .trim()
        .to_owned();
        let mut command = o.engine();
        let cache = crate::workflow::env_or("RESTREAM_DOCKER_GHA_CACHE", "0") == "1";
        if cache {
            command.args(["buildx", "build"]);
        } else {
            command.arg("build");
        }
        command
            .arg("--build-arg")
            .arg(format!("RESTREAM_BUILD_GIT_COMMIT={commit}"))
            .arg("--build-arg")
            .arg(format!("RESTREAM_BUILD_TIMESTAMP={timestamp}"))
            .args(["--target", "runtime", "-t", &o.image]);
        if cache {
            command.args([
                "--cache-from",
                "type=gha,scope=runtime-release",
                "--cache-to",
                "type=gha,mode=max,scope=runtime-release",
                "--load",
            ]);
        }
        execute(command.arg("."), &self.signals, None)
    }
    fn health(&mut self, shipped: bool) -> Result<bool, Error> {
        self.remove_health();
        let mut command = self.options.engine();
        command
            .args(["run", "-d", "--name", &self.name, "--label", &self.label])
            .args(super::split_env("CONTAINER_EXTRA_ARGS"));
        if shipped {
            command
                .arg("--security-opt")
                .arg(format!("seccomp={}", self.options.profile.display()));
        }
        command
            .args([
                "-e",
                "RESTREAM_INITIAL_ADMIN_PASSWORD=release-smoke-password",
                "-p",
                "127.0.0.1::3030",
                &self.options.image,
            ])
            .stdout(Stdio::null());
        execute(&mut command, &self.signals, None)?;
        if self
            .options
            .text(&["inspect", "--format", "{{json .Mounts}}", &self.name])?
            != "[]"
        {
            return Err("container-smoke: runtime unexpectedly requires a mount".into());
        }
        let ports = self.options.text(&["port", &self.name, "3030/tcp"])?;
        let port = ports.lines().find_map(|line| {
            line.rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
        });
        let mut healthy = false;
        if let Some(port) = port {
            let url = format!("http://127.0.0.1:{port}/healthz");
            for _ in 0..20 {
                self.signals.check()?;
                if self.curl(&url).is_ok() {
                    healthy = true;
                    break;
                }
                self.signals.sleep(Duration::from_secs(1))?;
            }
            if healthy {
                self.signals.sleep(Duration::from_secs(4))?;
                healthy = self.options.text(&[
                    "inspect",
                    "--format",
                    "{{.State.Running}}",
                    &self.name,
                ])? == "true"
                    && self.curl(&url).is_ok();
            }
        }
        self.probe_logs = quiet_capture(self.options.engine().args(["logs", &self.name]));
        self.remove_health();
        Ok(healthy)
    }
    fn curl(&self, url: &str) -> WorkflowResult {
        execute(
            Command::new("curl")
                .args(["--fail", "--silent", "--show-error", url])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            &self.signals,
            None,
        )
    }
    fn media(&mut self, seccomp: &str, rtmps: bool) -> WorkflowResult {
        let harness = self.options.root.join(crate::workflow::env_or(
            "RESTREAM_HARNESS_BIN",
            "target/bench/test_harness",
        ));
        if !harness
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return Err(Error::exit(
                3,
                format!(
                    "container-smoke: live harness is not executable: {}; build with cargo xtask build-bench",
                    harness.display()
                ),
            ));
        }
        let trust = self
            .options
            .root
            .join("test/fixtures/tls/mediamtx-rtmps-cert.pem");
        if rtmps {
            fs::File::open(&trust).map_err(|e| {
                Error::exit(3, format!("RTMPS trust fixture {}: {e}", trust.display()))
            })?;
        }
        let directory = self.dir()?;
        let shim = install_executable(&directory, "restream-container-shim")?;
        let log = fs::File::create(directory.join("harness.log"))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .current_dir(&self.options.root)
            .arg("harness")
            .env("CONTAINER_ENGINE", &self.options.engine)
            .env("CONTAINER_IMAGE", &self.options.image)
            .env("CONTAINER_SECCOMP", seccomp)
            .env("CONTAINER_LABEL", &self.label)
            .env("RESTREAM_BIN", shim)
            .env("HARNESS_BIN", harness)
            .env("BENCH_BUILD", "never")
            .env("WORK_DIR", &directory)
            .env_remove("RESTREAM_CONTAINER_MOUNTS_JSON")
            .stdout(log.try_clone()?)
            .stderr(log);
        let timeout = if rtmps {
            let media = directory.join("media");
            fs::create_dir(&media)?;
            for path in [&directory, &media] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o777))?;
            }
            let mounts = vec![
                "--volume".to_owned(),
                format!("{}:{}:ro", trust.display(), trust.display()),
                "--volume".into(),
                format!("{}:{}:rw", media.display(), media.display()),
                "--env".into(),
                format!("RESTREAM_MEDIA_DIR={}", media.display()),
            ];
            command
                .env(
                    "RESTREAM_CONTAINER_MOUNTS_JSON",
                    serde_json::to_string(&mounts).map_err(|e| e.to_string())?,
                )
                .env("MIXED_OUTPUT_GROUPS", "rtmps.src.a0")
                .env("MIXED_RESTREAM_LOG", directory.join("restream.log"))
                .env("RESTREAM_MEDIA_DIR", &media)
                .arg("mixed.live.rtmp.h264.a1.bf0");
            Duration::from_secs(600)
        } else {
            command
                .env("MSR_PEER", "sink")
                .env("RESOURCE_SWEEP_SCENARIOS", "egress-growth-source-srt")
                .env("RESOURCE_SWEEP_EGRESS_COUNTS", "1")
                .env("RESOURCE_SWEEP_SAMPLE_SECS", "3")
                .env("RESOURCE_SWEEP_SETTLE_SECS", "2")
                .env("RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS", "30")
                .arg("resource-sweep");
            Duration::from_secs(300)
        };
        command.args(["--", "--no-netns"]);
        let result = execute(&mut command, &self.signals, Some(timeout));
        let app_log = fs::read_to_string(directory.join("restream.log")).unwrap_or_default();
        let harness_log = fs::read_to_string(directory.join("harness.log")).unwrap_or_default();
        for line in harness_log.lines().filter(|l| {
            l.contains("outputs-progress ")
                || l.contains("harness failed")
                || l.contains("rtmps.src.a0")
        }) {
            println!("  {line}");
        }
        if rtmps {
            self.rtmps_log = app_log;
            self.rtmps_harness_log = harness_log;
        } else {
            self.srt_log = app_log;
        }
        result
    }
    fn proofs(&mut self) -> WorkflowResult {
        self.stage = "image-build-or-load";
        self.image()?;
        self.stage = "image-inspection";
        for (template, expected, description) in [
            ("{{.Config.User}}", "1000:1000", "non-root runtime user"),
            ("{{.Config.WorkingDir}}", "/", "runtime workdir"),
        ] {
            let actual = self.options.text(&[
                "image",
                "inspect",
                "--format",
                template,
                &self.options.image,
            ])?;
            if actual != expected {
                return Err(format!(
                    "container-smoke: expected {description} {expected}, got {actual:?}"
                )
                .into());
            }
        }
        let skip = crate::workflow::env_or("RESTREAM_CONTAINER_SMOKE_SKIP_SRT", "0") == "1";
        self.stage = "default-seccomp-control";
        println!("container-smoke: === default engine seccomp profile (negative control) ===");
        if self.health(false)? {
            self.default_health = "ok";
            if skip {
                self.default_srt = "skipped";
            } else if self.media("", false).is_ok() {
                self.default_srt = "ok";
                self.default_note =
                    "the engine's default profile already allows the required io_uring syscalls"
                        .into();
            } else {
                self.signals.check()?;
                self.default_srt = "blocked";
                if !io_uring_denied(&self.srt_log) {
                    return Err(format!("SRT failed under default profile for a reason other than io_uring denial:\n{}", self.srt_log).into());
                }
                self.default_note =
                    "SRT egress unavailable under the default profile: io_uring denied (expected)"
                        .into();
            }
        } else {
            self.default_health = "blocked";
            if !io_uring_denied(&self.probe_logs) {
                return Err(format!("image failed under default profile for a reason other than io_uring denial:\n{}", self.probe_logs).into());
            }
            self.default_note = "startup failed under the default profile: io_uring denied (expected; Restream's ingress and egress all need io_uring)".into();
        }
        self.default_logs = self.probe_logs.clone();
        println!(
            "container-smoke: default seccomp: health={} srt={} ({})",
            self.default_health, self.default_srt, self.default_note
        );
        self.stage = "shipped-profile";
        self.shipped_health = "fail";
        let health = self.health(true);
        self.shipped_logs = self.probe_logs.clone();
        if !health? {
            return Err(format!(
                "image did not become healthy under shipped profile:\n{}",
                self.probe_logs
            )
            .into());
        }
        self.shipped_health = "ok";
        if skip {
            self.shipped_srt = "skipped";
            self.shipped_rtmps = "skipped";
            eprintln!(
                "container-smoke: WARNING: all live transport proofs SKIPPED; this does NOT prove container egress"
            );
        } else {
            let profile = self.options.profile.to_string_lossy().into_owned();
            self.shipped_srt = "fail";
            self.media(&profile, false)?;
            if !srt_runtime_ready(&self.srt_log) {
                return Err(
                    "no Compio io_uring SRT runtime was instantiated in the container".into(),
                );
            }
            self.shipped_srt = "ok";
            if let Some(line) = self
                .srt_log
                .lines()
                .find(|l| l.contains("srt egress owner attached"))
            {
                println!("  {}", strip_ansi(line));
            }
            self.shipped_rtmps = "fail";
            self.media(&profile, true)?;
            self.shipped_rtmps = "ok";
        }
        println!(
            "container-smoke: shipped profile: health={} srt={} rtmps={}",
            self.shipped_health, self.shipped_srt, self.shipped_rtmps
        );
        if self.options.diagnostic {
            println!(
                "container-smoke: === DIAGNOSTIC seccomp=unconfined (never a deployment recommendation) ==="
            );
            self.unconfined_srt = if self.media("unconfined", false).is_ok() {
                "ok"
            } else {
                "failed"
            };
            self.signals.check()?;
            println!(
                "container-smoke: unconfined control: srt={}",
                self.unconfined_srt
            );
        }
        if let Some(archive) = &self.options.archive {
            self.save(archive)?;
        }
        self.passed = true;
        println!("container-smoke: PASS image={}", self.options.image);
        Ok(())
    }
    fn save(&self, archive: &Path) -> WorkflowResult {
        if let Some(parent) = archive.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut save = self.options.engine();
        save.args(["save", &self.options.image])
            .stdout(Stdio::piped());
        let mut producer = OwnedChild::spawn(&mut save)?;
        let pipe = producer.take_stdout()?;
        let file = fs::File::create(archive)?;
        execute(
            Command::new("gzip").arg("-n").stdin(pipe).stdout(file),
            &self.signals,
            None,
        )?;
        producer.wait()
    }
    fn report(&self) -> WorkflowResult {
        let Some(path) =
            std::env::var_os("RESTREAM_CONTAINER_SMOKE_REPORT").filter(|v| !v.is_empty())
        else {
            return Ok(());
        };
        let path = self.options.root.join(path);
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        for (name, log) in [
            ("default-profile-startup.log", &self.default_logs),
            ("shipped-profile-startup.log", &self.shipped_logs),
            ("last-srt-probe-restream.log", &self.srt_log),
            ("last-rtmps-probe-restream.log", &self.rtmps_log),
            ("last-rtmps-probe-harness.log", &self.rtmps_harness_log),
        ] {
            if !log.is_empty() {
                fs::write(directory.join(name), strip_ansi(log))?;
            }
        }
        let version = quiet_capture(self.options.engine().args([
            "version",
            "--format",
            "{{.Server.Version}}",
        ]));
        let kernel = quiet_capture(Command::new("uname").arg("-sr"));
        let report = json!({"result": if self.passed {"pass"} else {"fail"}, "failed_stage": if self.passed {""} else {self.stage},
            "image": self.options.image, "engine": self.options.engine, "engine_version": version.trim(), "kernel": kernel.trim(),
            "default_seccomp": {"health": self.default_health, "srt": self.default_srt, "note": self.default_note},
            "shipped_profile": {"path": self.options.profile, "health": self.shipped_health, "srt": self.shipped_srt, "rtmps": self.shipped_rtmps},
            "unconfined_control": self.unconfined_srt});
        fs::write(path, format!("{report}\n"))?;
        Ok(())
    }
}
impl Drop for Smoke {
    fn drop(&mut self) {
        if let Err(error) = self.report() {
            eprintln!("container-smoke: cannot write report: {error}");
        }
        self.remove_health();
        // SIGKILL cannot run the standalone shim's Drop; the smoke owns every
        // daemon-side container with its unique label, independent of CLI PIDs.
        let ids = quiet_capture(self.options.engine().args([
            "ps",
            "-aq",
            "--filter",
            &format!("label={}", self.label),
        ]));
        for id in ids
            .lines()
            .filter(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            quiet(self.options.engine().args(["rm", "-f", id]));
        }
        for directory in &self.dirs {
            let _ = fs::remove_dir_all(directory);
        }
    }
}
fn require_nonempty(path: &Path, description: &str) -> WorkflowResult {
    if path.metadata().is_ok_and(|m| m.is_file() && m.len() > 0) {
        Ok(())
    } else {
        Err(format!(
            "container-smoke: {description} not found or empty: {}",
            path.display()
        )
        .into())
    }
}
pub(super) fn io_uring_denied(log: &str) -> bool {
    static PATTERN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"native RTMP acceptor|Compio runtime failed to build|production runtime builds|io_uring.*(Operation not permitted|Function not implemented)|Operation not permitted \(os error 1\)|Function not implemented \(os error 38\)").expect("constant regex")
    });
    PATTERN.is_match(log)
}
pub(super) fn srt_runtime_ready(log: &str) -> bool {
    static PATTERN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"srt egress shard runtime ready.*io_uring.*true").expect("constant regex")
    });
    PATTERN.is_match(log)
}
fn strip_ansi(log: &str) -> String {
    static PATTERN: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new("\u{1b}\\[[0-9;]*m").expect("constant regex"));
    PATTERN.replace_all(log, "").into_owned()
}
pub(super) fn run(args: &[&str]) -> WorkflowResult {
    let Some(options) = Options::parse(args)? else {
        return Ok(());
    };
    require_nonempty(&options.profile, "seccomp profile")?;
    let mut smoke = Smoke::new(options);
    smoke.proofs()
}
