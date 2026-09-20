//! The production image is built by Dockerfile stages that stage the Cargo
//! workspace by hand: a dependency-warming layer (member MANIFESTS only, so the
//! cache boundary survives) and the real build (`runtime-tree`, with the real
//! member sources). Adding a path workspace member without teaching the recipe
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
    let members = workspace
        .split("members")
        .nth(1)
        .and_then(|rest| rest.split('[').nth(1))
        .and_then(|rest| rest.split(']').next())
        .expect("workspace.members array");
    members
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

#[test]
fn the_workspace_has_members_the_recipe_must_stage() {
    assert!(
        workspace_members()
            .iter()
            .any(|member| member == "crates/restream-dataplane"),
        "the guard below is only meaningful while the workspace has path members"
    );
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
/// layer compiled; without a touch Cargo reuses the dummy artifacts (an empty
/// dataplane lib) and the real build fails or ships a stub.
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
    assert!(
        tree.contains("find crates src"),
        "touch must cover members and src"
    );
}
