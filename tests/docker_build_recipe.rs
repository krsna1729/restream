//! The production image is built by Dockerfile stages that stage the Cargo
//! workspace by hand: a dependency-warming layer (member MANIFESTS only, so the
//! cache boundary survives) and the real build (`runtime-tree`, with the real
//! member sources). The workspace currently has no path members. Adding a path workspace member without teaching the recipe
//! breaks release-image construction ("failed to load manifest for workspace
//! member"), which local `cargo` never notices. This guard keeps the two in
//! step; it is a cheap source check, not a Dockerfile parser.

const CARGO_TOML: &str = include_str!("../Cargo.toml");
const DOCKERFILE: &str = include_str!("../Dockerfile");

fn workspace_members() -> Vec<String> {
    let workspace = CARGO_TOML
        .split("[workspace]")
        .nth(1)
        .expect("root Cargo.toml has a [workspace] table");
    let workspace = workspace.split("\n[").next().unwrap_or(workspace);
    let Some(members) = workspace.split("members").nth(1) else {
        return Vec::new();
    };
    members
        .split('[')
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("workspace.members array")
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The text of one Dockerfile stage, from its `FROM ... AS <name>` line to the
/// next `FROM`.
fn stage(name: &str) -> &'static str {
    let marker = format!(" AS {name}\n");
    let start = DOCKERFILE
        .find(&marker)
        .unwrap_or_else(|| panic!("Dockerfile has no stage {name}"));
    let rest = &DOCKERFILE[start + marker.len()..];
    let end = rest.find("\nFROM ").unwrap_or(rest.len());
    &rest[..end]
}

/// A deleted member must not linger in the recipe: staging a directory that no
/// longer exists fails the image build.
#[test]
fn the_recipe_stages_no_path_outside_the_workspace_members() {
    let members = workspace_members();
    for line in DOCKERFILE
        .lines()
        .filter(|line| line.trim_start().starts_with("COPY crates/"))
    {
        assert!(
            members.iter().any(|member| line.contains(member.as_str())),
            "Dockerfile stages a non-member path: `{}`",
            line.trim()
        );
    }
}

#[test]
fn every_workspace_member_manifest_is_staged_for_the_warm_workspace() {
    let warm = stage("rust-build");
    for member in workspace_members() {
        let copy = format!("COPY {member}/Cargo.toml {member}/Cargo.toml");
        assert!(
            warm.contains(&copy),
            "rust-build must stage `{copy}` before the dependency-warming build"
        );
        let copy_position = warm.find(&copy).expect("checked above");
        let warm_build = warm
            .find("RESTREAM_BUILD_PROFILE=release")
            .expect("rust-build warms the dependency graph");
        assert!(
            copy_position < warm_build,
            "{member}'s manifest must be staged before the warm build runs"
        );
        assert!(
            warm.contains(&format!("{member}/src/lib.rs")),
            "{member} needs a dummy source in the warm layer so its manifest is valid"
        );
    }
}

#[test]
fn every_workspace_member_real_source_is_staged_for_the_final_build() {
    let tree = stage("runtime-tree");
    for member in workspace_members() {
        let copy = format!("COPY {member}/ {member}/");
        let member_at = tree
            .find(&copy)
            .unwrap_or_else(|| panic!("runtime-tree must stage `{copy}` (the real member)"));
        let build_at = tree
            .find("RESTREAM_BUILD_PROFILE=release")
            .expect("runtime-tree runs the real application build");
        assert!(
            member_at < build_at,
            "{member}'s real source must be present before the final build"
        );
    }
}

/// COPY preserves checkout-time mtimes, older than the dummy sources the warm
/// layer compiled; without a touch Cargo reuses the dummy artifacts (a stub
/// main, and an empty lib per path member) and the real build fails or ships a
/// stub.
#[test]
fn the_real_sources_are_touched_so_cargo_rebuilds_them_over_the_warm_dummies() {
    let tree = stage("runtime-tree");
    let touch_at = tree
        .find("touch")
        .expect("runtime-tree must touch the real sources before the final build");
    let build_at = tree
        .find("RESTREAM_BUILD_PROFILE=release")
        .expect("final build");
    assert!(touch_at < build_at);
    let expected = if workspace_members().is_empty() {
        "find src"
    } else {
        "find crates src"
    };
    assert!(tree.contains(expected), "touch must cover `{expected}`");
}

/// `[patch.crates-io]` path overrides are part of dependency resolution: the
/// warm layer must stage each one before its dependency-only build.
#[test]
fn every_patch_path_is_staged_before_the_warm_build() {
    let warm = stage("rust-build");
    let warm_build = warm
        .find("RESTREAM_BUILD_PROFILE=release")
        .expect("rust-build warms the dependency graph");
    let Some(patches) = CARGO_TOML.split("[patch.crates-io]").nth(1) else {
        return;
    };
    let patches = patches.split("\n[").next().unwrap_or(patches);
    for path in patches.split("path = \"").skip(1) {
        let path = path.split('"').next().expect("quoted path");
        let top = path.split('/').next().expect("path has a first component");
        let copy = format!("COPY {top}/ {top}/");
        let staged = warm
            .find(&copy)
            .unwrap_or_else(|| panic!("rust-build must stage `{copy}` for patch path {path}"));
        assert!(
            staged < warm_build,
            "{path} must be staged before the warm build"
        );
    }
}
