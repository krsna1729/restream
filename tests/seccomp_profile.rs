//! The shipped Docker seccomp profile is part of the runtime compatibility
//! contract (Restream's native RTMP/SRT ingress and the SRT egress Compio
//! runtime all need io_uring, which Docker's default profile denies). It must
//! stay exactly "Moby default + a proven io_uring delta": auditable, minimal,
//! and never a broad allow.

use serde_json::{Value, json};

const PROFILE: &str = include_str!("../distribution/docker/restream-seccomp.json");
const BASE: &str = include_str!("../distribution/docker/moby-default-seccomp-docker-v29.8.1.json");

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("valid JSON")
}

fn delta() -> Value {
    json!({
        "names": ["io_uring_enter", "io_uring_register", "io_uring_setup"],
        "action": "SCMP_ACT_ALLOW"
    })
}

#[test]
fn profile_is_valid_json_and_never_allows_by_default() {
    let profile = parse(PROFILE);
    assert_eq!(profile["defaultAction"], "SCMP_ACT_ERRNO");
    assert_ne!(profile["defaultAction"], "SCMP_ACT_ALLOW");
    assert!(profile["syscalls"].is_array());
}

#[test]
fn profile_is_exactly_the_documented_baseline_plus_the_io_uring_delta() {
    let profile = parse(PROFILE);
    let base = parse(BASE);
    // Every top-level key other than the syscall list is the baseline's.
    for key in ["defaultAction", "defaultErrnoRet", "archMap"] {
        assert_eq!(profile[key], base[key], "{key} must stay the baseline's");
    }
    let base_syscalls = base["syscalls"].as_array().expect("baseline syscalls");
    let syscalls = profile["syscalls"].as_array().expect("profile syscalls");
    assert_eq!(
        syscalls.len(),
        base_syscalls.len() + 1,
        "exactly one entry was added to the baseline"
    );
    assert_eq!(&syscalls[..base_syscalls.len()], base_syscalls.as_slice());
    assert_eq!(syscalls.last().expect("delta"), &delta());
}

#[test]
fn the_delta_is_an_unconditional_io_uring_allow_and_nothing_broader() {
    let last = parse(PROFILE)["syscalls"]
        .as_array()
        .and_then(|entries| entries.last().cloned())
        .expect("delta entry");
    let names: Vec<&str> = last["names"]
        .as_array()
        .expect("names")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        names,
        ["io_uring_enter", "io_uring_register", "io_uring_setup"]
    );
    assert_eq!(last["action"], "SCMP_ACT_ALLOW");
    for forbidden in ["includes", "excludes", "args", "errnoRet"] {
        assert!(
            last.get(forbidden).is_none(),
            "delta must not carry {forbidden}"
        );
    }
    // The baseline itself must not already grant these (a Docker version that
    // does would make the delta redundant and the provenance stale).
    assert!(!BASE.contains("io_uring"), "baseline provenance is stale");
}

#[test]
fn both_release_architectures_are_represented() {
    let profile = parse(PROFILE);
    let archs: Vec<&str> = profile["archMap"]
        .as_array()
        .expect("archMap")
        .iter()
        .filter_map(|entry| entry["architecture"].as_str())
        .collect();
    assert!(archs.contains(&"SCMP_ARCH_X86_64"));
    assert!(archs.contains(&"SCMP_ARCH_AARCH64"));
}

#[test]
fn no_privileged_syscalls_are_granted_outside_the_baseline() {
    // Everything beyond the baseline is the delta, so a broadening (clone3,
    // mount, ptrace, bpf, ...) can only appear as a change to that entry.
    let profile = parse(PROFILE);
    let entries = profile["syscalls"].as_array().expect("syscalls");
    let added = &entries[entries.len() - 1];
    let text = added.to_string();
    for syscall in [
        "clone3", "mount", "ptrace", "bpf", "unshare", "setns", "keyctl",
    ] {
        assert!(
            !text.contains(syscall),
            "the delta must not grant {syscall}"
        );
    }
}
