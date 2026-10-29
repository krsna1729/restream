use super::{process, shim, smoke};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Engine {
    directory: PathBuf,
    executable: PathBuf,
}
impl Engine {
    fn new() -> Self {
        let directory =
            process::unique_dir(&std::env::temp_dir(), "container-engine-test").unwrap();
        let executable = directory.join("engine");
        // A Docker argv adapter, not a second implementation of the shim.
        // Production run_with owns all launching, waiting, forwarding and cleanup.
        fs::write(
            &executable,
            r#"#!/usr/bin/env python3
import json, os, pathlib, signal, sys, time
root = pathlib.Path(__file__).parent
with (root / 'events').open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1] == 'run':
    (root / 'pid').write_text(str(os.getpid()))
    if os.environ.get('RESTREAM_TEST_MODE') == 'fail':
        sys.exit(37)
    def terminated(sig, frame):
        (root / 'signal').write_text(str(sig))
        sys.exit(128 + sig)
    signal.signal(signal.SIGTERM, terminated)
    signal.signal(signal.SIGINT, terminated)
    (root / 'ready').write_text('ready')
    time.sleep(60)
"#,
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            executable,
        }
    }
    fn settings(&self, mode: &str) -> shim::Settings {
        shim::Settings {
            engine: self.executable.clone().into_os_string(),
            image: "image:proof".into(),
            name: "owned-test-container".into(),
            label: Some("proof=owned".into()),
            seccomp: Some("/profile with spaces.json".into()),
            extra: Vec::new(),
            environment: vec![("RESTREAM_TEST_MODE".into(), mode.into())],
        }
    }
    fn events(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.directory.join("events"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn assert_reaped(&self) {
        let pid: i32 = fs::read_to_string(self.directory.join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: signal zero only probes whether the recorded PID exists.
        let probe = unsafe { libc::kill(pid, 0) };
        assert_eq!(probe, -1, "engine client survived shim teardown");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn container_shim_forwards_values_and_denies_host_storage() {
    let engine = Engine::new();
    let mut settings = engine.settings("fail");
    for (key, value) in [
        ("RESTREAM_DB_PATH", "/host/db"),
        ("RESTREAM_LOG_DIR", "/host/logs"),
        ("RESTREAM_MEDIA_DIR", "/host/media"),
        ("RESTREAM_BIN", "/host/bin"),
        ("RESTREAM_CONTAINER_MOUNTS_JSON", "[]"),
        (
            "RESTREAM_INITIAL_ADMIN_PASSWORD",
            "quoted value $not-expanded",
        ),
        ("UNRELATED_SECRET", "not-forwarded"),
    ] {
        settings.environment.push((key.into(), value.into()));
    }
    let command = settings.command(&["--flag", "one quoted argument"]);
    let args: Vec<OsString> = command.get_args().map(OsString::from).collect();
    let forwarded: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "-e")
        .map(|pair| pair[1].to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        forwarded,
        ["RESTREAM_TEST_MODE", "RESTREAM_INITIAL_ADMIN_PASSWORD"]
    );
    assert!(args.windows(2).any(|pair| pair
        == [
            OsString::from("--security-opt"),
            OsString::from("seccomp=/profile with spaces.json")
        ]));
    assert_eq!(args.last().unwrap(), "one quoted argument");
    let password = command
        .get_envs()
        .find(|(key, _)| *key == "RESTREAM_INITIAL_ADMIN_PASSWORD")
        .unwrap()
        .1
        .unwrap();
    assert_eq!(password, "quoted value $not-expanded");
}

#[test]
fn container_shim_cleans_named_container_after_engine_error() {
    let engine = Engine::new();
    assert!(shim::run_with(&engine.settings("fail"), &[], || 0).is_err());
    let events = engine.events();
    assert_eq!(events[0][0], "run");
    assert_eq!(events[1], ["stop", "--time", "5", "owned-test-container"]);
    assert_eq!(events[2], ["rm", "-f", "owned-test-container"]);
    engine.assert_reaped();
}

#[test]
fn container_shim_term_and_int_stop_container_then_reap_client() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let engine = Engine::new();
        let result = shim::run_with(&engine.settings("wait"), &[], || {
            if engine.directory.join("ready").exists() {
                signal
            } else {
                0
            }
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(engine.directory.join("signal")).unwrap(),
            signal.to_string()
        );
        let events = engine.events();
        assert_eq!(events[0][0], "run");
        assert_eq!(events[1][0], "stop");
        assert_eq!(
            events.last().unwrap(),
            &["rm", "-f", "owned-test-container"]
        );
        engine.assert_reaped();
    }
}

#[test]
fn container_timeout_reaps_engine_client() {
    let engine = Engine::new();
    let settings = engine.settings("wait");
    assert!(
        process::execute(
            &mut settings.command(&[]),
            &process::Signals,
            Some(std::time::Duration::from_secs(1))
        )
        .is_err()
    );
    engine.assert_reaped();
}

#[test]
fn container_media_evidence_rejects_unrelated_failures_and_false_runtime() {
    assert!(smoke::io_uring_denied(
        "io_uring_setup: Operation not permitted"
    ));
    assert!(smoke::io_uring_denied("Compio runtime failed to build"));
    assert!(!smoke::io_uring_denied(
        "RTMPS certificate verification failed"
    ));
    assert!(smoke::srt_runtime_ready(
        "srt egress shard runtime ready io_uring=true"
    ));
    assert!(!smoke::srt_runtime_ready(
        "srt egress shard runtime ready io_uring=false"
    ));
    assert!(!smoke::srt_runtime_ready("srt egress owner attached"));
}
