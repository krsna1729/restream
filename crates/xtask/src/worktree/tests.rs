//! Filesystem and ordinary-repository tests only: no test creates a worktree.
use super::*;
mod frontend_deps;
use std::fs::{File, FileTimes};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::time::{Duration, UNIX_EPOCH};

fn put(path: impl AsRef<Path>, content: impl AsRef<[u8]>) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}
fn timestamp(path: &Path, seconds: u64) {
    File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
        .unwrap();
}
fn cmd(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    cmd(Command::new("git").arg("init").arg("--quiet").arg(path));
}

#[test]
fn worktree_parser_preserves_flag_composition_and_rejects_unknown_flags() {
    let options = parse(&[
        "--full-target-cache",
        "--with-incremental",
        "--no-target-cache",
        "--no-cargo-config",
        "--no-node-modules",
        "--force-cleanup",
        "--cleanup",
        "--base",
        "topic~2",
        "--branch",
        "custom/topic",
        "--path",
        "tree with spaces",
        "--source",
        "source tree",
        "id",
    ])
    .unwrap()
    .unwrap();
    assert!(options.full && options.incremental && options.force && options.cleanup);
    assert!(!options.target && !options.cargo && !options.node);
    assert_eq!(options.base, "topic~2");
    assert_eq!(options.branch.as_deref(), Some("custom/topic"));
    assert_eq!(options.path.as_deref(), Some(Path::new("tree with spaces")));
    assert!(parse(&["--share-native", "id"]).is_err());
    assert!(parse(&["--path"]).is_err());
    assert!(parse(&["one", "two"]).is_err());
    assert!(parse(&[]).is_err());
    assert!(
        parse(&["--check-frontend-deps", "fixture"])
            .unwrap()
            .unwrap()
            .id
            .is_empty()
    );
    assert_eq!(parse(&["id", "--", "ignored"]).unwrap().unwrap().id, "id");
}

#[test]
fn worktree_dry_run_never_creates_checkout_cache_or_metadata() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("repo");
    init_repo(&source);
    put(
        source.join("target/debug/deps/libdependency-a.rlib"),
        b"artifact",
    );
    put(source.join(".cargo/config.toml"), b"[build]\njobs=1\n");
    let destination = temp.path().join("destination with spaces");
    run(&[
        "--dry-run",
        "--source",
        source.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
        "--no-node-modules",
        "dry",
    ])
    .unwrap();
    assert!(!destination.exists());
    assert!(!source.join(".git/worktrees").exists());
    assert!(!source.join(".git/refs/heads/codex").exists());
}

#[test]
fn worktree_cleanup_lets_git_protect_main_and_unrelated_directories() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("repo");
    init_repo(&source);
    put(source.join("owned-file"), b"keep");
    let unrelated = temp.path().join("unrelated");
    init_repo(&unrelated);
    put(unrelated.join("other-file"), b"keep other");
    let actions = Actions { dry: false };
    assert!(cleanup(&actions, &source, &source, true).is_err());
    assert!(cleanup(&actions, &source, &unrelated, true).is_err());
    assert_eq!(fs::read(source.join("owned-file")).unwrap(), b"keep");
    assert_eq!(
        fs::read(unrelated.join("other-file")).unwrap(),
        b"keep other"
    );
    assert!(cleanup(&actions, &source, &temp.path().join("missing"), false).is_err());
    assert!(!source.join(".git/worktrees").exists());
}

#[test]
fn worktree_copy_detaches_directory_symlinks_and_preserves_source_and_permissions() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    put(source.join("bin/tool"), b"cache");
    fs::set_permissions(source.join("bin/tool"), fs::Permissions::from_mode(0o751)).unwrap();
    symlink("bin/tool", source.join("tool")).unwrap();
    symlink(&source, &destination).unwrap();
    cache::sync_tree(&Actions { dry: false }, &source, &destination, "fixture").unwrap();
    assert!(!destination.is_symlink());
    assert_eq!(fs::read(destination.join("tool")).unwrap(), b"cache");
    assert_eq!(
        fs::metadata(destination.join("bin/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    let a = fs::metadata(source.join("bin/tool")).unwrap();
    let b = fs::metadata(destination.join("bin/tool")).unwrap();
    assert_ne!((a.dev(), a.ino()), (b.dev(), b.ino()));
    put(destination.join("bin/tool"), b"destination only");
    assert_eq!(fs::read(source.join("bin/tool")).unwrap(), b"cache");
    put(destination.join("stale"), b"old");
    cache::sync_tree(&Actions { dry: false }, &source, &destination, "fixture").unwrap();
    assert!(!destination.join("stale").exists());
    cache::sync_tree(&Actions { dry: false }, &source, &source, "same").unwrap();
    assert_eq!(fs::read(source.join("bin/tool")).unwrap(), b"cache");
}

#[test]
fn worktree_subset_selects_latest_complete_local_groups_and_omits_incremental() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    let deps = source.join("debug/deps");
    put(deps.join("libdependency-all.rlib"), b"third-party");
    for index in 0..5 {
        for extension in ["rlib", "rmeta", "d"] {
            let path = deps.join(format!("restream-{index}.{extension}"));
            put(&path, format!("group {index}"));
            timestamp(&path, index + 10);
        }
    }
    put(source.join("debug/build/compiler/output"), b"build");
    put(
        source.join("debug/.fingerprint/dependency/lib.json"),
        b"fingerprint",
    );
    put(source.join("debug/restream"), b"binary");
    put(source.join("release/restream"), b"release must not copy");
    put(
        source.join("debug/incremental/restream-a/state"),
        b"must not copy",
    );
    let selected = temp.path().join("selected");
    cache::debug_deps(
        &Actions { dry: false },
        &deps,
        &selected,
        &cache::Keep::Numeric(3.0),
    )
    .unwrap();
    for index in 0..5 {
        for extension in ["rlib", "rmeta", "d"] {
            assert_eq!(
                selected
                    .join(format!("restream-{index}.{extension}"))
                    .exists(),
                index >= 2
            );
        }
    }
    cache::seed_subset(&Actions { dry: false }, &source, &destination, false).unwrap();
    assert!(
        destination
            .join("debug/deps/libdependency-all.rlib")
            .is_file()
    );
    assert!(destination.join("debug/build/compiler/output").is_file());
    assert!(
        destination
            .join("debug/.fingerprint/dependency/lib.json")
            .is_file()
    );
    assert!(destination.join("debug/restream").is_file());
    assert!(!destination.join("release").exists());
    assert!(!destination.join("debug/incremental").exists());
}

#[test]
fn worktree_artifact_recency_uses_newest_member_and_retains_whole_groups() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    for (name, time) in [
        ("restream-a.rlib", 10),
        ("restream-a.d", 40),
        ("restream-b.rlib", 30),
        ("restream-c.d", 20),
    ] {
        let path = temp.path().join(name);
        put(&path, b"artifact");
        timestamp(&path, time);
    }
    fs::create_dir(temp.path().join("restream-directory")).unwrap();
    symlink("restream-b.rlib", temp.path().join("restream-link.rlib")).unwrap();
    assert_eq!(
        cache::latest_hashes(temp.path(), "restream", &cache::Keep::Numeric(2.0)).unwrap(),
        ["a", "b"]
    );
    assert_eq!(
        cache::latest_hashes(temp.path(), "restream", &cache::Keep::Numeric(1.5)).unwrap(),
        ["a"]
    );
    assert!(
        cache::latest_hashes(temp.path(), "restream", &cache::Keep::Numeric(0.0))
            .unwrap()
            .is_empty()
    );
    assert!(
        cache::latest_hashes(temp.path(), "restream", &cache::Keep::Numeric(-1.0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        cache::latest_hashes(temp.path(), "restream", &cache::Keep::Text("abc".into())).unwrap(),
        ["a", "b", "c"]
    );
}

#[test]
fn worktree_incremental_selection_is_bounded_and_prefix_specific() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("source");
    for (name, time) in [
        ("restream-a", 10),
        ("restream-b", 30),
        ("restream-c", 20),
        ("unrelated-new", 40),
    ] {
        let dir = source.join("debug/incremental").join(name);
        put(dir.join("state"), b"state");
        timestamp(&dir, time);
    }
    let picked = cache::latest_incremental(
        &source.join("debug/incremental"),
        "restream",
        &cache::Keep::Numeric(2.0),
    )
    .unwrap();
    assert_eq!(
        picked
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        ["restream-b", "restream-c"]
    );
    let destination = temp.path().join("destination");
    cache::incremental(
        &Actions { dry: false },
        &source.join("debug/incremental"),
        &destination,
        &cache::Keep::Numeric(2.0),
    )
    .unwrap();
    assert!(!destination.join("unrelated-new").exists());
    assert!(!destination.join("restream-a").exists());
    assert!(destination.join("restream-b/state").exists());
    assert!(destination.join("restream-c/state").exists());
}

#[test]
fn worktree_subset_detaches_shared_target_and_keeps_other_tree_unchanged() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("source");
    put(
        source.join("debug/deps/libdependency-a.rlib"),
        b"third-party",
    );
    let unrelated = temp.path().join("unrelated");
    put(unrelated.join("precious"), b"unrelated");
    let target = temp.path().join("target");
    symlink(&unrelated, &target).unwrap();
    cache::seed_subset(&Actions { dry: false }, &source, &target, false).unwrap();
    assert!(!target.is_symlink());
    assert!(target.join("debug/deps/libdependency-a.rlib").exists());
    assert_eq!(fs::read(unrelated.join("precious")).unwrap(), b"unrelated");
    assert!(!unrelated.join("debug").exists());
}

#[test]
fn worktree_refresh_full_cache_uses_existing_checkout_without_registering_a_worktree() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    init_repo(&source);
    init_repo(&destination);
    put(source.join("target/release/restream"), b"release");
    put(
        source.join("target/debug/incremental/restream-a/state"),
        b"incremental",
    );
    run(&[
        "--source",
        source.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
        "--full-target-cache",
        "--no-node-modules",
        "--no-cargo-config",
        "full",
    ])
    .unwrap();
    assert_eq!(
        fs::read(destination.join("target/release/restream")).unwrap(),
        b"release"
    );
    assert!(
        destination
            .join("target/debug/incremental/restream-a/state")
            .exists()
    );
    assert!(!source.join(".git/worktrees").exists());
    assert!(!destination.join(".git/worktrees").exists());
}

#[test]
fn worktree_state_preserves_literal_paths_and_records_new_cleanup_entrypoint() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let options = Options {
        id: "a'quoted $id".into(),
        full: true,
        incremental: true,
        ..Options::default()
    };
    let destination = temp.path().join("tree with $ and ' quote");
    let work_root = destination
        .join(".local/artifacts/agents")
        .join(&options.id);
    write_state(
        &Actions { dry: false },
        &options,
        "custom/topic",
        temp.path(),
        &destination,
        &work_root,
    )
    .unwrap();
    let content = fs::read_to_string(destination.join(".agent-state/setup.env")).unwrap();
    assert!(content.contains(&format!(
        "AGENT_WORK_ROOT={}\n",
        shell_value(&work_root.to_string_lossy())
    )));
    assert!(content.contains("AGENT_TARGET_CACHE_MODE='full'\n"));
    assert!(content.contains("AGENT_TARGET_CACHE_WITH_INCREMENTAL='1'\n"));
    assert_eq!(shell_value("a'b$()"), "'a'\\''b$()'");
    assert!(content.contains("cargo xtask worktree --cleanup"));
}
