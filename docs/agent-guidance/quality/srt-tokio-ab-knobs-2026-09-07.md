# Tokio SRT A/B knobs — quiet-host matrix — 2026-09-07

## Contents

- [Setup](#setup)
- [Scale scout](#scale-scout)
- [One-variable N=200 matrix](#one-variable-n200-matrix)
- [Decision](#decision)

Quiet-host measurement that decides whether PR #154's env knobs earn a
place on master. Raw artifacts stay in `.local/artifacts/pr154-ab/` (not
committed). Bench binaries were deleted and rebuilt before the matrix
(`target/bench/restream` 104,547,208 bytes,
`target/bench/test_harness` 50,415,888 bytes, commit `69200d8c`).

## Setup

- Branch: `cursor/srt-tokio-ab-knobs-bcd0` (`69200d8c`)
- Fixture: `.local/fixtures/bbb-1080p60-30a.mp4`
- `MSR_PEER=sink` `MSR_PROTOCOL_MIX=srt-only` `--no-netns`
- One knob per cell; the other two unset via `env -u`
- Noise floor for a "move": `|delta|` vs baseline **>10% and >50 MB** on
  `rssPeakKb` or `unattributedPeakKb`, or a PASS/FAIL / fabric-leaf-death
  change that would still show on a second cell

## Scale scout

| N | knobs | status | `msr.json` | `SRT fabric leaf terminated unexpectedly` |
|---|---|---|---|---|
| 30 | unset | PASS | yes | 0 |
| 200 | unset (6 s sample) | PASS | yes | 0 |
| 600 | unset | FAIL after 600/600 progress | **no** | 1275 |

N=600 reached 600/600 at 167 s (oscillating 515–599 with
`unregistered-cell` until then), then sink verification failed
(`bytesOut` dropped on three outputs). No resource aggregate. A/B is
therefore N=200, not BBB-600.

## One-variable N=200 matrix

`MSR_SAMPLE_SECS=20`. All four cells PASS, 200/200 egresses.

| cell | env | rssPeakKb | unattributedPeakKb | anonymousPeakKb | leaf deaths | vs baseline |
|---|---|---|---|---|---|---|
| baseline | unset | 475812 | 428225 | 436572 | 0 | — |
| udp-buf | `RESTREAM_SRT_UDP_BUF_BYTES=262144` | 315640 | 269237 | 277504 | 0 | rss −156 MB (−34%); unattr −155 MB (−37%) — **move** |
| recv-budget | `RESTREAM_SRT_RECV_BUDGET_DATAGRAMS=8` | 550756 | 503613 | 513068 | 5 | rss +73 MB (+16%); unattr +74 MB (+18%); 5 deaths — **move** |
| io-batch | `RESTREAM_SRT_IO_BATCH_CAPACITY=8` | 520756 | 476692 | 482676 | 0 | rss +44 MB (+9%); unattr +47 MB (+11%) — **null** (under both floors) |

Each override cell logged once at info (`SRT A/B knob override`). Baseline
logged none.

`udp-buf` cutting requested `SO_*BUF` from 8 MiB to 256 KiB dropped
anonymous heap, not just kernel skmem. That contradicts treating this
knob as a confirm-the-rejection null; `apply_optional_udp_buf` also
switches Caller/Listener `SocketBufferConfig` from `Auto` to
`Bytes`, which can size userspace protocol buffers. `#155` occupancy
fields remain the sender-window instrument; this knob is still a real
RSS discriminator.

The 6 s N=200 scout peaked at 355884 rssKb; the 20 s baseline climbed to
475812. Heap is still growing in the sample window, so peaks are
path-dependent. The udp-buf 20 s peak still sits **below** the 6 s
unset scout, so the drop is not just a shorter growth window.

## Decision

Merge the knobs. Two of three cells moved RSS (and recv-budget moved
stability) outside the noise floor. `IO_BATCH_CAPACITY` stays as a
send-path A/B with a measured null at N=200. Do not treat a later
BBB-600 FAIL without `msr.json` as an A/B result.
