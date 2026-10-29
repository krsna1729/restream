use super::*;
use serde_json::{Map, Value, json};
const PACKAGES: &[&str] = &[
    "hls.js",
    "react",
    "react-dom",
    "@tailwindcss/cli",
    "@types/react",
    "@types/react-dom",
    "tailwindcss",
    "typescript",
    "vite",
];
fn manifest(root: &Path) {
    put(root.join("package.json"), serde_json::to_vec(&json!({"private":true,"dependencies":{"hls.js":"*","react":"*","react-dom":"*"},"devDependencies":{"@tailwindcss/cli":"*","@types/react":"*","@types/react-dom":"*","tailwindcss":"*","typescript":"*","vite":"*"}})).unwrap());
}
fn binary(path: &Path) {
    put(path, b"");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn frontend_files(root: &Path) {
    for name in [
        "hls.js/dist/hls.min.js",
        "react/jsx-runtime.js",
        "react-dom/client.js",
        "@types/react/index.d.ts",
        "@types/react-dom/index.d.ts",
    ] {
        put(root.join("node_modules").join(name), b"");
    }
    for name in ["vite", "tsc", "tailwindcss"] {
        binary(&root.join("node_modules/.bin").join(name));
    }
}
fn complete_frontend(root: &Path) {
    manifest(root);
    frontend_files(root);
    for name in PACKAGES {
        put(
            root.join("node_modules").join(name).join("package.json"),
            serde_json::to_vec(&json!({"name":name,"version":"1.0.0"})).unwrap(),
        );
    }
}

#[test]
fn worktree_frontend_rejects_legacy_subset_and_corrupted_react_dom() {
    assert!(
        crate::workflow::on_path("npm"),
        "frontend regression scenarios require npm"
    );
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    let stale = temp.path().join("stale");
    manifest(&stale);
    binary(&stale.join("node_modules/.bin/tsc"));
    binary(&stale.join("node_modules/.bin/tailwindcss"));
    put(stale.join("node_modules/hls.js/dist/hls.min.js"), b"");
    assert!(!frontend::ready(&stale));
    let complete = temp.path().join("complete");
    complete_frontend(&complete);
    assert!(frontend::ready(&complete));
    fs::remove_file(complete.join("node_modules/react-dom/client.js")).unwrap();
    assert!(!frontend::ready(&complete));
}

#[test]
fn worktree_frontend_rejects_invalid_dependency_graph_and_nonexecutable_tools() {
    assert!(
        crate::workflow::on_path("npm"),
        "frontend regression scenarios require npm"
    );
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    complete_frontend(temp.path());
    fs::set_permissions(
        temp.path().join("node_modules/.bin/vite"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(!frontend::ready(temp.path()));
    fs::set_permissions(
        temp.path().join("node_modules/.bin/vite"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    put(
        temp.path().join("package.json"),
        br#"{"private":true,"dependencies":{"react":"2.0.0"}}"#,
    );
    assert!(!frontend::ready(temp.path()));
}

// Build genuine local npm packages: repair exercises the real npm ci path
// entirely offline, without a fake npm executable or embedded shell scripts.
fn locked_local_frontend(root: &Path) {
    let mut dependencies = Map::new();
    for name in PACKAGES {
        let package = root.join("packages").join(name);
        let bin = match *name {
            "vite" => Some("vite"),
            "typescript" => Some("tsc"),
            "@tailwindcss/cli" => Some("tailwindcss"),
            _ => None,
        };
        let mut manifest = json!({"name":name,"version":"1.0.0"});
        if let Some(bin) = bin {
            let mut bins = Map::new();
            bins.insert(bin.into(), Value::String("tool".into()));
            manifest["bin"] = Value::Object(bins);
            binary(&package.join("tool"));
        }
        put(
            package.join("package.json"),
            serde_json::to_vec(&manifest).unwrap(),
        );
        dependencies.insert(
            (*name).into(),
            Value::String(format!("file:packages/{name}")),
        );
        match *name {
            "hls.js" => put(package.join("dist/hls.min.js"), b""),
            "react" => put(package.join("jsx-runtime.js"), b""),
            "react-dom" => put(package.join("client.js"), b""),
            "@types/react" | "@types/react-dom" => put(package.join("index.d.ts"), b""),
            _ => {}
        }
    }
    put(
        root.join("package.json"),
        serde_json::to_vec(&json!({"private":true,"dependencies":dependencies})).unwrap(),
    );
    cmd(Command::new("npm")
        .args([
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--offline",
            "--no-audit",
            "--no-fund",
            "--prefix",
        ])
        .arg(root));
}

#[test]
fn worktree_frontend_repairs_missing_and_incomplete_cache_using_locked_local_packages() {
    assert!(
        crate::workflow::on_path("npm"),
        "frontend repair regression requires npm"
    );
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    locked_local_frontend(temp.path());
    assert!(!frontend::ready(temp.path()));
    frontend::ensure(&Actions { dry: false }, temp.path()).unwrap();
    assert!(frontend::ready(temp.path()));
    // Break a bin link without changing a local package. npm ci must rebuild it.
    fs::remove_file(temp.path().join("node_modules/.bin/vite")).unwrap();
    assert!(!frontend::ready(temp.path()));
    frontend::ensure(&Actions { dry: false }, temp.path()).unwrap();
    assert!(frontend::ready(temp.path()));
}

#[test]
fn worktree_frontend_without_lock_is_left_untouched() {
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    frontend::ensure(&Actions { dry: false }, temp.path()).unwrap();
    assert!(!temp.path().join("node_modules").exists());
}

#[test]
fn worktree_frontend_dry_run_does_not_install_an_incomplete_locked_cache() {
    assert!(
        crate::workflow::on_path("npm"),
        "frontend dry-run regression requires npm"
    );
    let temp = crate::workflow::TempDir::new("restream-worktree-test").unwrap();
    manifest(temp.path());
    put(temp.path().join("package-lock.json"), b"{}");
    assert!(frontend::ensure(&Actions { dry: true }, temp.path()).is_err());
    assert!(!temp.path().join("node_modules").exists());
}
