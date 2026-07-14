# Release runbook

This project has two release gates:

1. local due diligence, which proves the pushed tree with the same canonical scripts used elsewhere in the repo; and
2. the GitHub Release workflow dry-run, which builds the production artifacts in the worker before any tag is pushed.

The tag publish step is deliberately separate. Branch workflow dispatches never publish a GitHub Release or GHCR image. Only a `v*` tag does that.

## Prerequisites

- Clean checkout on the release branch.
- `gh auth status` succeeds for the target repository.
- Docker is available if running local evidence.
- The release evidence tools are installed for local due diligence: `cargo-audit`, `cargo-deny`, `grype`, and `trivy`.
- The branch has been pushed to GitHub before dispatching the dry-run.

CI may take longer the first time a worker misses the native static dependency cache. The release workflow restores the canonical native prefix cache before building and saves it immediately after a miss so later jobs do not pay that cost again.

The GitHub release workflow keeps the full live harness coverage, but it does
not run the old monolithic suite in one job. It builds the canonical bench
harness binaries once, uploads them as a short-lived artifact, then fans out
stable shard names through `scripts/release/harness-shard.sh`. Each shard runs
inside the prebuilt `ci-harness-runtime` Docker target through
`scripts/ci/run-harness-shard-in-runtime.sh`, so apt and the pinned MediaMTX
download happen when the CI runtime image is published, not once per shard.
GitHub Free standard hosted runners allow more concurrency than we use here,
but the workflow caps the matrix at 12 to smooth artifact downloads and live
process startup without serializing the suite. Packaging/evidence stays behind
the full harness matrix. If a future plan or repository policy changes the
concurrency, change only the workflow cap; shard ownership remains in
`scripts/lib/release-shards.sh`.

Refresh the CI harness runtime image after changing Docker/runtime bootstrap
inputs:

```sh
gh workflow run ci-harness-runtime-image.yml --ref <release-branch>
```

The image is published as
`ghcr.io/krsna1729/restream-ci-harness-runtime:ubuntu24`. The Release workflow
expects this image to exist before a dry-run; if the image pull fails, publish
the runtime image first rather than falling back to per-shard apt installs.

Each shard has a script-owned timeout so a stuck runner leg fails with a clear
`TIMEOUT` instead of consuming the full GitHub job limit. The buckets are grouped
from observed local shard timings and kept at least 2x over those timings:
smoke/correctness shards use 5 minutes, small mixed shards use 15 minutes,
medium mixed/resource shards use 25 minutes, full bitrate measurement shards use
30 minutes, and unknown future shards fall back to 20 minutes. The workflow job
timeout is slightly higher so artifacts can still upload after the script exits.
Inspect the catalog with `scripts/release/harness-shard.sh list`, and inspect a
single shard with `scripts/release/harness-shard.sh explain <shard>`.
Setup is bounded separately: CI system dependency installation retries apt
operations and wraps MediaMTX bootstrap with a short timeout, so a single wedged
hosted runner fails in setup instead of looking like a harness hang. Release
harness shards should not use that path; they should run in the CI harness
runtime image so setup stalls cannot masquerade as harness failures.
Release CI uses the same script-owned dependency profiles as local setup: build
jobs install only the Rust/native build group, while the CI harness image is
built from the runtime harness group plus pinned MediaMTX. Keep package names in
`scripts/lib/debian-packages.sh`; workflows and wrapper scripts should select
profiles, not maintain package lists.

## 1. Run local due diligence

Use a release-style version string, usually the tag you intend to publish:

```sh
scripts/release/local-due-diligence.sh v0.2.0
```

This wrapper intentionally reuses the repo scripts instead of duplicating their logic. It runs:

- `cargo fmt --all --check`
- `npm run test:frontend`
- `scripts/check/api-contract.sh`
- `scripts/check/test-hygiene.sh`
- `scripts/check/fixture-discipline.sh`
- `scripts/release/prepare-build-tree.sh`
- `scripts/harness/run.sh suite -- --run-id <derived-id> --continue-on-fail`
- `scripts/release/package-binaries.sh <version>`
- `scripts/check/release-evidence.sh <oci-tarball> <sbom> <binary-bundle>`

If the full live harness has already been run separately and you only need the packaging/evidence pass, use:

```sh
scripts/release/local-due-diligence.sh v0.2.0 --skip-harness
```

## 2. Dispatch the GitHub dry-run

Push the branch, then start the non-publishing Release workflow:

```sh
scripts/release/dispatch-dry-run.sh feat/rust-backend-rewrite-v2
```

The script prints the newest Release workflow run for that branch, including the run id, URL, status, and head SHA. Keep the run id for the publish step.

The dry-run is green only after:

- every full live harness shard has passed;
- `scripts/release/package-binaries.sh <version>` has packaged all supported Linux executables;
- `scripts/check/release-evidence.sh <oci-tarball> <sbom> <binary-bundle>` has passed.

Packaging and release evidence intentionally start after the harness matrix is
green, so a broken live release candidate does not spend scanner/container time
producing artifacts that cannot be published.

Monitor it with:

```sh
gh run watch <run-id>
```

Do not publish from a stale run. If the branch changes after the dry-run starts, dispatch a fresh dry-run for the new head SHA.

## 3. Publish by tag

After the dry-run completes successfully, publish with:

```sh
scripts/release/tag-and-publish.sh v0.2.0 <successful-dry-run-id>
```

The script refuses to tag unless:

- the checkout is clean;
- the supplied run is the `Release` workflow;
- the supplied run was a `workflow_dispatch` dry-run;
- the run completed successfully;
- the run head SHA exactly matches the current checkout; and
- the local and remote tag do not already exist.

Pushing the tag triggers the publishing path, which creates the GitHub Release assets and pushes the GHCR image tags.

Monitor the publishing run with:

```sh
gh run list --workflow release.yml --branch v0.2.0 --limit 1
gh run watch <tag-run-id>
```

## 4. Verify published artifacts

After the tag workflow is green:

```sh
gh release view v0.2.0 --json tagName,targetCommitish,assets,url
```

Expected release assets include:

- `restream-v0.2.0-linux-x86_64.tar.gz`
- `restream-v0.2.0-oci.tar.gz`
- `restream-v0.2.0.sbom.cdx.json`

GitHub exposes a SHA-256 digest beside each release asset, so the release does
not publish separate checksum sidecars.

Check the container registry tags:

```sh
docker manifest inspect ghcr.io/krsna1729/restream:v0.2.0
docker manifest inspect ghcr.io/krsna1729/restream:latest
```

The OCI tarball is the same scratch-based image exported as a downloadable tar archive. Users can load it without pulling from GHCR:

```sh
docker load -i restream-v0.2.0-oci.tar.gz
docker run --rm -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me -p 3030:3030 restream:v0.2.0
```

Binary-bundle users can download the Linux tarball, unpack it, and run:

```sh
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me ./run restream
```

## Failed publish

If the tag workflow fails before a GitHub Release is created, inspect and decide whether to remove the tag:

```sh
git tag -d v0.2.0
git push origin :refs/tags/v0.2.0
```

If a public GitHub Release was created, do not overwrite it. Fix forward with a new version tag.
