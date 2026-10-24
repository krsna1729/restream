# Release compliance

Every distributed binary or container needs provenance, vulnerability,
license, source-availability and startup evidence. Restream links Rust
crates with native media libraries, so the native licenses matter. The
scripts below enforce these properties; this page states them.

## Contents

- [Required release properties](#required-release-properties)
- [Evidence owners](#evidence-owners)
- [Native licenses](#native-licenses)
- [Application license](#application-license)
- [Dependency policy](#dependency-policy)

## Required release properties

A release must:

- build from checked-in Rust, frontend and native input locks;
- keep the source commit, build timestamp, native build identity
  (`restream.nativeBuildId`) and full-provenance runtime SBOM;
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
| [scripts/check/release-evidence.sh](../scripts/check/release-evidence.sh) | Scanners (`cargo audit`, `cargo deny check advisories licenses bans sources`, `grype`, `trivy`), artifact inspection, startup proofs, failure policy |
| [scripts/release/package-binaries.sh](../scripts/release/package-binaries.sh) | Supported executables, archive names, bundle contents |
| [scripts/build/emit-sbom.sh](../scripts/build/emit-sbom.sh) | Runtime SBOM |

Run them through the [release runbook](release-runbook.md). Changing a
scanner, asset or smoke test is a script and CI change, not a docs change.
SBOMs and packages are build outputs; do not commit them.
Dependency advisory checks also run on pull requests; the full SBOM scan
remains scheduled. Require both dependency checks in branch protection before
merge.

## Native licenses

The runtime SBOM lists the native libraries in the shipped binary. Depending
on the build, that includes FFmpeg, x264, x265, SRT, Mbed TLS, SQLite,
compiler runtimes, glibc and the Rust standard library. Native input pins
live in `scripts/build/native/native-inputs.lock`.

An SBOM is inventory and scanner input. It does not replace license texts,
attribution, copyright notices or source-availability obligations.

x264 and x265 are `GPL-2.0-or-later`. FFmpeg's effective license depends on
its configuration. Before external distribution, confirm that the exact
build meets all GPL, LGPL, MPL and compiler-runtime obligations, or use a
reviewed compatible build. See
[Source distribution](source-distribution.md#binary-and-container-distributions).

## Application license

Restream is MIT-licensed. That does not replace the obligations of native
components. The
[third-party component manifest](../distribution/THIRD_PARTY_COMPONENTS.md)
is the notice-chain entry point.

## Dependency policy

`deny.toml` owns Cargo source, advisory, license, ban and duplicate-family
policy. Document temporary exceptions beside that policy and remove them
when the upstream graph converges.
