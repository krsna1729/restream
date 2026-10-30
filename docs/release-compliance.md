# Release compliance

Every distributed binary or container needs provenance, vulnerability,
license, source-availability and startup evidence. The engine binary links
Rust crates and a few native libraries; container images also carry a GPL
FFmpeg executable, so both sets of licenses matter. The
scripts below enforce these properties; this page states them.

## Contents

- [Required release properties](#required-release-properties)
- [Evidence owners](#evidence-owners)
- [Native licenses](#native-licenses)
- [Application license](#application-license)
- [Dependency policy](#dependency-policy)

## Required release properties

A release must:

- build from checked-in Rust and frontend locks;
- keep the source commit, build timestamp and full-provenance runtime SBOM;
- scan Rust dependencies and the SBOM under the current policy;
- prove the host binary starts outside the source tree and serves its
  embedded frontend;
- prove the container starts with its required runtime setup;
- include the Restream license, third-party notices, license texts and
  source-availability information;
- keep diagnostic tracing. A `tracing/release_max_level_*` compile-time cap
  needs an explicit observability review.

## Evidence owners

| Owner | Owns |
|---|---|
| `cargo xtask qualify release-evidence` ([source](../crates/xtask/src/qualify/evidence.rs)) | Scanners (`cargo audit`, `cargo deny check advisories licenses bans sources`, `grype`, `trivy`), artifact inspection, startup proofs, failure policy |
| `cargo xtask release package-binaries` ([source](../crates/xtask/src/release/package.rs)) | Supported executables, archive names, bundle contents |
| `restream --emit-sbom <path>` ([source](../src/main.rs)), run by `cargo xtask release package-binaries` | Runtime SBOM |

Run them through the [release runbook](release-runbook.md). Changing a
scanner, asset or smoke test is a script and CI change, not a docs change.
SBOMs and packages are build outputs; do not commit them.
Dependency advisory checks also run on pull requests; the full SBOM scan
remains scheduled. Require both dependency checks in branch protection before
merge.

## Native licenses

The runtime SBOM lists the native components in the shipped binary:
srt-rs, SQLite, libgcc, the Rust standard library and glibc. The binary links
no FFmpeg, x264 or x265 code.

An SBOM is inventory and scanner input. It does not replace license texts,
attribution, copyright notices or source-availability obligations.

Container images ship the BtbN GPL FFmpeg build as separate `ffmpeg` and
`ffprobe` executables; `restream ffmpeg-fetch` installs the same build on a
host. Before external distribution, confirm that the image meets that
build's GPL obligations and the compiler-runtime obligations of the binary. See
[Source distribution](source-distribution.md#binary-and-container-distributions).

## Application license

Restream is MIT-licensed. That does not replace the obligations of its
native components or of the FFmpeg executable shipped beside it. The
[third-party component manifest](../distribution/THIRD_PARTY_COMPONENTS.md)
is the notice-chain entry point.

## Dependency policy

`deny.toml` owns Cargo source, advisory, license, ban and duplicate-family
policy. Document temporary exceptions beside that policy and remove them
when the upstream graph converges.
