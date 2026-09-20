# Docker seccomp profile

## Contents

- [Why a profile is required](#why-a-profile-is-required)
- [What the profile is](#what-the-profile-is)
- [Refreshing the baseline](#refreshing-the-baseline)
- [Requirements (capabilities are authoritative)](#requirements-capabilities-are-authoritative)

`restream-seccomp.json` is the supported seccomp profile for the Restream
runtime image. Launch the image with it:

```sh
docker run --rm \
  --security-opt seccomp=restream-seccomp.json \
  -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me \
  -p 3030:3030 -p 1935:1935 -p 10080:10080/udp \
  restream:container
```

Release builds publish the same file as `restream-<version>-seccomp.json` next
to the runtime image archive; a source checkout is not needed. A copy also lives
inside the image at `/usr/share/doc/restream/distribution/docker/`.

No `--privileged`, no added capabilities, and never `seccomp=unconfined`.

## Why a profile is required

Restream's native RTMP acceptor, its SRT ingress and its SRT egress runtime
(Compio, forced `io_uring`, one runtime per SRT shard) all run on `io_uring`.
Docker's default seccomp profile stopped allowing `io_uring_setup`,
`io_uring_enter` and `io_uring_register` in Docker Engine 25.0. Under that
profile:

- `io_uring_setup` fails with `EPERM` (`Operation not permitted`; some engines
  return `ENOSYS`, `Function not implemented`);
- the RTMP acceptor cannot start and its critical task brings the process down,
  so the container does not stay up (`/healthz` can answer for a moment first);
- SRT egress cannot create its runtime and fails closed with a typed error
  (`SRT egress Compio runtime failed to build`). There is deliberately no
  fallback driver, runtime or backend.

`/healthz` therefore does not prove the deployment works. The repository's
container smoke (`scripts/check/container-smoke.sh`) proves real SRT egress
under this profile and records what the engine's default profile does.

## What the profile is

`restream-seccomp.json` = the Moby default seccomp profile + one entry:

```json
{ "names": ["io_uring_enter", "io_uring_register", "io_uring_setup"],
  "action": "SCMP_ACT_ALLOW" }
```

| | |
|---|---|
| Baseline | `moby-default-seccomp-docker-v29.8.1.json`, an unmodified copy of `vendor/github.com/moby/profiles/seccomp/default.json` at moby/moby tag `docker-v29.8.1` (`b2d20c90a74af78b3f0f967db92292e4a603c03d`) |
| Baseline SHA-256 | `536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74` |
| Delta | the single unconditional allow above (appended last; no `includes`/`excludes`/`args`) |
| Kept from baseline | `defaultAction` `SCMP_ACT_ERRNO`, `defaultErrnoRet` 1, `archMap` (x86-64 and AArch64), every other rule |

The delta is limited to what execution proved necessary. The full syscall trace
of a Restream process (startup, RTMP/SRT ingress, SRT egress through the Compio
runtime, shutdown) was compared with the baseline's allowed set: the only
syscalls it uses that the baseline does not allow are these three (`clone3` is
answered `ENOSYS` by the baseline, and glibc falls back to `clone`).

`io_uring` widens the kernel attack surface a container can reach, which is
exactly why Docker removed it from the default. The profile keeps every other
restriction of the default profile; if your policy cannot accept that trade,
the SRT egress runtime and the native ingress cannot run.

`tests/seccomp_profile.rs` enforces "baseline + exactly this delta": valid
JSON, no allow-by-default, the baseline untouched, both release architectures
present, and no other syscall granted.

## Refreshing the baseline

Run `scripts/dev/refresh-docker-seccomp.sh <moby-tag>` (for example
`docker-v29.9.0`). It downloads that tag's default profile, records its SHA-256,
regenerates `restream-seccomp.json` as baseline + delta, and updates the
versioned baseline copy. Then update the provenance table above, review the
diff of the baseline against the previous copy, and run
`scripts/check/container-smoke.sh` and `cargo test --test seccomp_profile`.
If a newer Docker default already allows `io_uring`, drop the delta (and its
test) instead of carrying a redundant rule.

## Requirements (capabilities are authoritative)

- **SRT egress always requires** a Linux kernel and a container syscall policy
  that let the forced Compio `io_uring` runtime be created and run
  (`io_uring_setup`/`enter`/`register`). RawReadiness does not make `io_uring`
  optional.
- **Managed multishot receive additionally requires** provided-buffer ring
  registration (`IORING_REGISTER_PBUF_RING`) and `recvmsg` multishot support.
- **RawReadiness needs neither.** On a host with a working `io_uring` runtime
  but no provided-buffer ring, the Owner attaches with the readiness receiver;
  this is the `ManagedPreferred` policy working as designed, and it is logged
  once per address family as `srt egress owner attached ... rx_mode=RawReadiness`
  together with the exact substrate reason.

No kernel version is documented as a requirement: what the runtime observes
(`srt egress shard runtime ready`, `srt egress owner attached`) is authoritative.
A policy denial (`EPERM`/`EACCES`) and an unsupported kernel feature (`EINVAL`)
are different problems and are reported differently
(`substrate_diagnosis=buffer-ring-denied-by-policy` versus
`buffer-ring-unsupported-by-kernel`).
