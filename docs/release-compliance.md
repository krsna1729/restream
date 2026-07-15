# Release compliance

Restream combines Rust crates with native media libraries. Every distributed
binary or container is a release artifact that needs provenance, vulnerability,
license, source-availability, and startup evidence.

## Contents

- [Required release properties](#required-release-properties)
- [Executable evidence owner](#executable-evidence-owner)
- [Native license basis](#native-license-basis)
- [Application license](#application-license)
- [Dependency Policy](#dependency-policy)

## Required release properties

A release must:

- build from checked-in Rust, frontend, and native input locks;
- preserve the exact source commit, build timestamp, native build identity, and
  full-provenance runtime SBOM. The public identity field is
  `restream.nativeBuildId`;
- scan Rust dependencies and the SBOM under the repository's current policy;
- prove the downloadable host binary starts outside the source tree and serves
  its embedded frontend;
- prove the container runtime artifact starts with its required runtime setup;
- include the Restream license, required third-party notices and license texts,
  and source-availability information;
- preserve diagnostic tracing unless an explicit observability review approves
  a compile-time restriction.

These are properties of the produced bytes, not a hand-maintained command
checklist.

## Executable evidence owner

[scripts/check/release-evidence.sh](../scripts/check/release-evidence.sh) owns
the exact scanners, artifact inspection, startup proofs, and failure policy.
[scripts/release/package-binaries.sh](../scripts/release/package-binaries.sh)
owns the supported executable set, archive names, and bundle contents.

Release operators should invoke those owners through the
[release runbook](release-runbook.md). Do not copy their tool list, internal
sequence, asset inventory, or smoke-test URLs into compliance prose; changing
those is an executable-policy change and must happen in the scripts and CI.

The enforced dependency-policy entry points are `cargo audit` and
`cargo deny check advisories licenses bans sources`. They are named here as
release requirements; arguments, installation, and orchestration remain owned
by the evidence script. Release builds must also preserve diagnostic events;
introducing a `tracing/release_max_level_*` compile-time cap requires an
explicit observability review.

Generated SBOMs and packaged artifacts remain build outputs. They are not
checked into the source tree merely to document a release.

## Native license basis

The runtime SBOM inventories native libraries linked into the shipped binary.
Depending on the selected build, that can include FFmpeg components, x264,
x265, SRT, Mbed TLS, SQLite, compiler runtimes, glibc, and the Rust standard
library. The immutable native input pins are owned by
`scripts/build/native/native-inputs.lock`.

An SBOM is scanner and inventory input; it does not replace license texts,
attribution, copyright notices, or source-availability obligations.

x264 and x265 are reported as `GPL-2.0-or-later`. FFmpeg's effective license
depends on its native configuration. Before external distribution, confirm that
the exact build and distribution terms satisfy all GPL, LGPL, MPL, and
compiler-runtime obligations, or use a reviewed compatible build.

## Application license

Restream is MIT-licensed. That application license does not replace the
obligations of native components linked into or distributed with the runtime.
The checked-in [third-party component manifest](../distribution/THIRD_PARTY_COMPONENTS.md)
is the notice-chain entry point.

## Dependency Policy

`deny.toml` owns Cargo source, advisory, license, ban, and duplicate-family
policy. Temporary exceptions must remain documented beside that policy and
should be removed when the upstream dependency graph converges.
