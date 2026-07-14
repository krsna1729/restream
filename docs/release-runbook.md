# Release runbook

This runbook defines the human release sequence. Scripts own the gates,
artifact inventory, shard composition, timeouts, packaging layout, and
publication mechanics; this page does not repeat their internals.

## Contents

- [Preflight](#preflight)
- [Run local due diligence](#run-local-due-diligence)
- [Dispatch the GitHub dry-run](#dispatch-the-github-dry-run)
- [Publish the certified commit](#publish-the-certified-commit)
- [Verify publication](#verify-publication)
- [Handle a failed publish](#handle-a-failed-publish)

## Preflight

Use a clean checkout of the release branch and push its current commit before
dispatching remote certification. Confirm GitHub CLI authentication and Docker
availability.

The canonical scripts check their own required tools and inputs. Use their
`--help` output when setup fails rather than maintaining a separate package,
scanner, artifact, shard, or timeout list here.

Branch workflow dispatches are non-publishing. Only a version tag starts the
publishing path.

## Run local due diligence

Run the local wrapper with the intended version:

```sh
scripts/release/local-due-diligence.sh <vX.Y.Z>
```

The wrapper is the owner of the local gate sequence. It prepares a clean build
tree, runs current correctness and hygiene gates, exercises the live harness,
packages supported binaries, and certifies the produced artifacts. Inspect the
script output for the exact failing owner instead of rerunning a copied
subsequence from this guide.

If the same commit already has complete live-harness evidence and only the
packaging/evidence pass must be repeated, the wrapper exposes an explicit
`--skip-harness` option. Record why it is safe to use.

## Dispatch the GitHub dry-run

Start the non-publishing Release workflow for the pushed branch:

```sh
scripts/release/dispatch-dry-run.sh <release-branch>
```

The script reports the workflow run and exact head SHA. Keep its run ID. If the
branch changes, dispatch a new dry-run; a run for an older SHA cannot authorize
publication.

The workflow and release-shard scripts own matrix composition, concurrency,
runtime-image selection, setup retries, and timeouts. Inspect them or their
generated output when a job fails; do not copy those values into this runbook.

## Publish the certified commit

After the dry-run succeeds, publish from the unchanged clean checkout:

```sh
scripts/release/tag-and-publish.sh <vX.Y.Z> <successful-dry-run-id>
```

The script verifies the workflow identity, event type, required jobs, result,
head SHA, and tag state before it pushes anything. Do not recreate those checks
manually or bypass them with a direct tag push.

## Verify publication

Confirm the publishing workflow completed, then inspect the GitHub Release and
registry tags:

```sh
gh release view <vX.Y.Z> --json tagName,targetCommitish,assets,url
docker manifest inspect ghcr.io/krsna1729/restream:<vX.Y.Z>
docker manifest inspect ghcr.io/krsna1729/restream:latest
```

The current artifact names and contents are owned by
`scripts/release/package-binaries.sh`, and the required evidence is owned by
`scripts/check/release-evidence.sh`. Validate the actual release output
against those owners rather than against a list copied into this page.

See [Release compliance](release-compliance.md) for the legal and provenance
properties the packaged bytes must retain.

## Handle a failed publish

If publishing fails before a public GitHub Release exists, inspect the failed
run and decide whether the unpublished tag should be removed. If a public
release exists, do not overwrite it; fix forward with a new version.

Tag deletion is an exceptional, externally visible action. Confirm the release
state and repository policy before removing either a local or remote tag.
