# Media Pipeline

This document covers the ingest-to-egress media pipeline: current shape,
protocol/codec boundaries, stage sharing, buffer sizing, and correctness
requirements.

For the performance optimization plan and benchmark results, see
[High-Performance Data Path](high-performance-data-path.md).

## Contents

- [Current shape](#current-shape)
- [Multi-input selection](#multi-input-selection)
- [Transcoder stages](#transcoder-stages)
- [Protocol and codec boundaries](#protocol-and-codec-boundaries)
- [Resolution presets](#resolution-presets)
- [H.265 egress policy](#h265-egress-policy)
- [Current protocol matrix](#current-protocol-matrix)
- [Minimum work per consumer](#minimum-work-per-consumer)
- [Harness coverage](#harness-coverage)
- [What is shared when outputs use the same video and audio config](#what-is-shared-when-outputs-use-the-same-video-and-audio-config)
- [Audio stage cache](#audio-stage-cache)
- [Buffer sizing for 4K 60fps](#buffer-sizing-for-4k-60fps)
- [Thread and memory ownership, ingest to egress](#thread-and-memory-ownership-ingest-to-egress)
- [SRT bonding](#srt-bonding)
- [Protocol correctness requirements](#protocol-correctness-requirements)

## Current shape

```mermaid
flowchart TD
    subgraph INGESTS["Ingest"]
        RI["RTMP input sessions\nFLV payload"]
        SI["SRT input sessions\nMPEG-TS"]
    end

    subgraph DEMUX["Ingest demux (inline, async)"]
        RD["RTMP parser\nFlv packets"]
        SD["TsDemuxer\nRaw packets"]
    end

    GATE{"Selected input gate"}
    SR[("source_ring\nSPMC RingBuffer 4096\nMediaPacket · Flv ∣ Raw")]

    subgraph PASSTHROUGH["Passthrough — encoding = source"]
        direction TB
        PT1["Flv · dest=RTMP\nBytes::clone → FLV tag\n→ RTMP socket"]
        PT2["Flv · dest=SRT/HLS\nvideo_for_ts strip hdr\nTsMuxer → MPEG-TS\n→ SRT socket / HLS store"]
        PT3["Raw · dest=RTMP\nbuild_avcc_seq_hdr\nvideo_for_rtmp → FLV tag\n→ RTMP socket"]
        PT4["Raw · dest=SRT/HLS\nTsMuxer → MPEG-TS\n→ SRT socket / HLS store"]
    end

    subgraph TRANSCODE["Transcoded — encoding = 720p (shared once per preset per pipeline)"]
        direction TB
        TIN["video_for_ts\nFlv: strip hdr  ∣  Raw: inject SPS/PPS\nTsMuxer → MPEG-TS"]
        FSTDIN[/"FFmpeg stdin\npipe"/]
        FF(["FFmpeg subprocess\nscale=1280:720 · libx265/libx264\n─────────────────\nstdin → stdout"])
        FSTDOUT[/"FFmpeg stdout\npipe"/]
        TDEM["TsDemuxer\nRaw packets"]
        OR[("output_ring\nSPMC RingBuffer 4096\nMediaPacket · Raw")]
        TOUT_R["dest=RTMP\nvideo_for_rtmp → FLV tag\n→ RTMP socket"]
        TOUT_S["dest=SRT/HLS\nTsMuxer → MPEG-TS\n→ SRT socket / HLS store"]
        TIN --> FSTDIN --> FF --> FSTDOUT --> TDEM --> OR
        OR --> TOUT_R
        OR --> TOUT_S
    end

    RI --> RD --> GATE
    SI --> SD --> GATE
    GATE -->|"selected only"| SR

    SR -->|"Flv · source · RTMP"| PT1
    SR -->|"Flv · source · SRT/HLS"| PT2
    SR -->|"Raw · source · RTMP"| PT3
    SR -->|"Raw · source · SRT/HLS"| PT4
    SR -->|"any format · 720p"| TIN
```

## Multi-input selection

A pipeline supports one primary and up to three backup input records. Each
record has a separate stream key and independent RTMP/SRT session. Unselected
connected sessions stay warm through socket receive, parsing, demux/probe,
metadata, transport metrics, and on-demand input-scoped HLS preview generation.
RTMP and SRT standbys keep the latest complete compressed GOP in a
protocol-task-local `StandbyGopCache`. The default per-input limits are 16 MiB
of payload and 2,048 packets. Crossing either limit invalidates the whole GOP;
non-keyframe packets are then discarded until the next keyframe starts a new
cache. The cache has no shared lock, async channel, decoder, or transform.

Explicit promotion demotes and drains the old gate, then arms the target gate.
On the target's next packet, a complete cached GOP activates the gate and is
drained exactly once from its retained video keyframe. Without a complete cache,
the target waits for its next live video keyframe. `InputTimestampMapper`
applies one offset to the replay so its first DTS follows the prior writer's
last DTS, with PTS/DTS composition offsets preserved across the whole GOP and
on later re-promotions. The gate stores forwarding state and in-flight lease
count in one atomic word; loom covers no overlapping writers and one activation
for a replay-ready boundary.

This is connected standby, not bonded ingest. Each publisher remains an
independent source and the operator chooses one. Libsrt socket groups remain the
only bonded SRT path. File inputs retain the next-live-keyframe promotion path;
the compressed-GOP cache applies to continuously connected RTMP/SRT publishers.

## Transcoder stages

Every non-passthrough encoding creates a **shared stage**: one process per
`(pipeline_id, preset, output_codec)` tuple regardless of how many outputs use
that resolved video shape.

### Stage graph

```mermaid
flowchart TD
    Source["source_ring"]
    Codec{"legacy RTMP source needs H.264?"}
    CodecStage["shared hevc_to_h264 stage"]
    Video{"video preset?"}
    VideoStage["shared codec-keyed video preset stage"]
    Audio{"audio routing suffix?"}
    AudioStage["shared audio filter stage"]
    Output["final ring_buf"]
    Egresses["all matching egress readers"]

    Source --> Codec
    Codec -->|yes| CodecStage --> Output
    Codec -->|no| Video
    Video -->|yes| VideoStage --> Audio
    Video -->|source passthrough| Audio
    Audio -->|yes| AudioStage --> Output
    Audio -->|no| Output
    Output --> Egresses
```

The `hevc_to_h264` stage is used only for source passthrough into legacy RTMP
when the ingest is H.265. Preset outputs resolve the codec first: legacy RTMP
`codec:auto` creates an H.264 preset stage (for example
`video:720p:codec:h264`), while Enhanced RTMP and SRT can create or share an
H.265 preset stage (for example `video:720p:codec:hevc`).

### Passthrough rule

`source` encodings **never** enter any transcoder stage. The egress reads
directly from `source_ring`. This is enforced in the reconciler (`src/lib.rs`)
before any `get_or_create_transcoder` call. `custom` output encodings are
rejected during output create/update because custom FFmpeg arguments are stored
for future implementation but not applied by the runtime.

### Stage-key naming

| Stage | Key format | Example |
|---|---|---|
| Video preset | `video:<preset>:codec:<codec>` | `video:720p:codec:h264`, `video:720p:codec:hevc` |
| H.265→H.264 | `hevc_to_h264:from:<upstream_key>` | `hevc_to_h264:from:source` |
| Audio filter | `audio:<op>:from:<video_key>` | `audio:atrack:0:from:video:720p:codec:h264` |

The `upstream_key` in the `hevc_to_h264` key encodes what ring feeds the
converter. Today that converter is reserved for source passthrough RTMP, so the
normal upstream key is `source`.

The video-preset key is shared across all compound encodings with the same
resolved video part (for example `720p+atrack:0` and `720p+remap:0:1` can both
use `video:720p:codec:h264`). The audio key embeds the upstream video key to
prevent cross-contamination between presets and output codecs.

### External transcoder (default)

```mermaid
flowchart LR
    Source["source_ring"] --> Reader["Reader + TsMuxer"]
    Reader -->|MPEG-TS bytes| Stdin["FFmpeg stdin"]
    Stdin --> Encode["scale + libx264/libx265"]
    Encode --> Stdout["FFmpeg stdout"]
    Stdout --> Demux["TsDemuxer"]
    Demux -->|Raw MediaPackets| Ring["shared output_ring"]
    Ring --> Rtmp["RTMP output"]
    Ring --> Srt["SRT output"]
    Ring --> Hls["HLS output"]
```

One `ffmpeg` subprocess per `(pipeline, preset)`. FFmpeg reads MPEG-TS from
stdin and writes transcoded MPEG-TS to `pipe:1` (stdout). A Tokio task reads
stdout, runs it through `TsDemuxer`, and pushes the resulting `MediaPacket`s
into `output_ring`.

This is the **default** backend. It is robust because FFmpeg errors are
isolated to the subprocess and logged to stderr; a crash restarts cleanly on
the next reconciler cycle.

### Internal transcoder (opt-in)

Set `RESTREAM_INTERNAL_VIDEO_PRESETS=1` to use the in-process libavcodec path
(`src/media/transcoder.rs`) for video-preset stages. HEVC-to-H.264 bridge
stages, HLS preview transcode stages, and complex audio stages have separate
rollout flags: `RESTREAM_INTERNAL_HEVC_TO_H264`,
`RESTREAM_INTERNAL_HLS_PREVIEW`, and `RESTREAM_INTERNAL_AUDIO_COMPLEX`. The
data flow is identical — the same `source_ring → output_ring` contract holds —
but uses `MemoryQueue`/`avio` callbacks instead of a subprocess pipe.

Current behavior: for `video:*` presets, the internal path uses
`run_ffmpeg_transcode_with_scale` and performs decode→scale→encode in-process
(`libx264` for H.264 input, `libx265` for H.265 input), while audio streams are
passed through. Source passthrough still bypasses the video transcoder.

The external FFmpeg subprocess backend remains the default. Backend selection
is explicit per stage family; there is no global switch that silently changes
every transform path.

### Muxing stages summary

| Stage | Role |
|---|---|
| SRT Ingest | `TsDemuxer` — demux MPEG-TS into `MediaPacket`s (inline async) |
| External transcoder | subprocess FFmpeg stdin→stdout; `TsMuxer` writes stdin, `TsDemuxer` reads stdout |
| Internal transcoder | in-process FFmpeg via `MemoryQueue`+`avio`; `TsMuxer` feeds input, output packets pushed directly to ring |
| SRT Egress | Shared `TsMuxer` task per unique `(pipeline, preset)` feeding a shared `TsChunkRing` (SPMC lock-free package ring) |
| HLS | `TsMuxer` remux to MPEG-TS, then segment in memory (inline async) |
| Recording | Raw MPEG-TS write to `.ts` file via `MemoryQueue` (OS thread) |



## Protocol and codec boundaries

| Area | Current state |
|---|---|
| RTMP H.264/AAC | Native ingest/play/egress; video uses DTS and carries FLV composition offset. B-frame round-trip still an E2E gate |
| SRT H.264/AAC | Native ingest/read/egress with MPEG-TS demux/remux |
| SRT H.265 | Codec mapping implemented; full E2E matrix remains a gate |
| RTMP H.265 | Enhanced RTMP ingest (H.265 arriving over RTMP) is not implemented. RTMP *egress* with H.265 source works: `hevc_to_h264` stage does full libavcodec decode→encode |
| Multi-track audio | SRT ingest preserves audio track indices plus MPEG-TS PID/language metadata where present |
| Audio remap/downmix | Channel-level DSP routes use an external FFmpeg audio stage (`pan` for remap, stereo resample for downmix); `atrack` remains packet-only |
| HLS pull routes/store | Implemented and tested; live segment generation uses native TsMuxer |
| HLS upload | Implemented; HTTP/HTTPS output URLs PUT new segments plus playlist to the target |
| RTMPS output | `rtmps://` URLs accepted by API and routed through RTMP egress with Rustls wrapping before the RTMP handshake |
| Custom output encoding | Not applied; `custom` is rejected by output create/update instead of being exposed as a passthrough runtime option |

## Resolution presets

The external transcoder stage applies `scale=WxH` and re-encodes preserving the
input codec: `libx265 -preset veryfast` for H.265 input, `libx264 -preset
veryfast` for H.264 input. The internal video-preset backend (when enabled
with `RESTREAM_INTERNAL_VIDEO_PRESETS=1`) uses the same preset table via
`run_ffmpeg_transcode_with_scale`.

| Preset | Resolution | Scale filter |
|---|---|---|
| `source` | passthrough | none — never enters transcoder |
| `480p` | 854×480 | `scale=854:480` |
| `720p` | 1280×720 | `scale=1280:720` |
| `1080p` | 1920×1080 | `scale=1920:1080` |


## H.265 egress policy

Standard RTMP (non-Enhanced) does not carry H.265. The reconciler enforces:

| Egress protocol | H.265 input | Behavior |
|---|---|---|
| Legacy RTMP | H.265 source | `hevc_to_h264:from:source` stage inserted; full libavcodec H.265→H.264 — **working** |
| Legacy RTMP | H.265 + video preset | typed `codec:auto` resolves to H.264, so the preset stage is keyed as `video:<preset>:codec:h264` and no HEVC bridge is needed |
| Enhanced RTMP | H.265 source/preset | HEVC is packetized as Enhanced FLV `hvc1`; encoded presets are keyed as `video:<preset>:codec:hevc` |
| SRT | H.265 source | Passthrough (MPEG-TS carries HEVC natively) — **working** |
| SRT | H.265 + video preset | `video:<preset>:codec:hevc` with libx265; same ring can be shared with Enhanced RTMP — **working** |
| HLS preview | H.265 source | Preview-only `hevc_preview_h264` stage converts to H.264 720p before served fMP4 HLS — **current path** |

Output configuration is symmetric at the model boundary: video mode, video
codec, audio routing, and protocol mode are typed fields. Protocol capability
validation remains asymmetric: legacy RTMP and HLS resolve `codec:auto` to
H.264, Enhanced RTMP and SRT can preserve H.265, and explicit unsupported
codec/protocol combinations are rejected before persistence.

## Current protocol matrix

| Ingest | RTMP egress | SRT egress | HLS preview | Recording |
|---|---|---|---|---|
| RTMP H.264 | Basic interop; B-frame timestamp gate | Implemented; full matrix gate | fMP4 HLS preview with alternate-audio renditions | Input-scoped mixed gate validates final MP4 |
| RTMP H.265 | Enhanced RTMP egress supported; legacy RTMP uses H.265→H.264 bridge | Not assumed | Not assumed | Not assumed |
| SRT H.264 | Packetization implemented; live matrix gate | Locally validated | fMP4 HLS preview with alternate-audio renditions | Input-scoped mixed gate validates final MP4 |
| SRT H.265 | RTMP: `hevc_to_h264` conversion working; SRT: passthrough working | Passthrough implemented; E2E gate | HEVC preview converts to H.264 720p before served fMP4 HLS | Input-scoped mixed gate validates final MP4 |
| File | RTMP-shaped via child FFmpeg | Implemented for compatible FLV codecs | Native fMP4 preview packager; HEVC uses preview transcode | Input-scoped mixed gate validates final MP4 |

HLS preview is now served as fragmented MP4 with `EXT-X-MAP`, `init.mp4`, and
`.m4s` media segments. The preview path uses one fMP4 muxer per HLS rendition:
one video-only rendition plus separate audio-only playlists for alternate
tracks. Remote HLS outputs intentionally remain MPEG-TS because HTTP PUT ingest
targets commonly require `.ts` media segments.

## Minimum work per consumer

All consumers that process packets from a ring buffer avoid per-packet heap
allocation by using the zero-allocation `_into` variants:

| Consumer | Video conversion | Audio conversion | Burst size |
|---|---|---|---|
| RTMP egress | `video_for_rtmp_into` | `audio_for_rtmp_into` | `pull_burst` 32 |
| SRT egress | None (Shared `TsMuxer`) | None (Shared `TsMuxer`) | `pull_burst` 32 (`TsChunkReader`) |
| SRT play subscriber | None (Shared `TsMuxer`) | None (Shared `TsMuxer`) | `pull_burst` 32 (`TsChunkReader`) |
| HLS segmenter | `video_for_ts_into` | `audio_for_ts_into` | `pull_burst` 32 |
| Recording | `video_for_ts_into` | `audio_for_ts_into` | `pull_burst` 32 |
| Transcoder feed | `video_for_ts` (Raw→Raw passthrough) | `audio_for_ts` | `pull_burst` 32 |

Scratch buffers (`video_conv_buf`, `audio_conv_buf`) are allocated once at
consumer startup and reused across packets. For `PayloadFormat::Raw` video, the
borrowed payload slice is returned directly (zero copy).

## Harness coverage

The live harness owns the changing scenario inventory. Use the catalog
inspection workflow in [Testing](testing.md#live-integration-tests) instead of
copying mode, scenario, or stage-count tables into this guide.

The canonical scenario definitions live in `test/harness/scenarios/` and the
generated mixed-matrix catalog under `src/bin/test_harness/`. Representative
rows cover RTMP, SRT, and file ingest; H.264 and H.265; single- and
multi-audio; passthrough and preset stages; B-frame timestamp behavior; and
cross-protocol egress.

Tests should assert the stable sharing contract rather than a copied process
count: identical `(pipeline_id, stage_key)` values reuse expensive work, while
each destination keeps its own sender. Current scenario composition and
resource measurements belong to the catalog and dated evidence.

## What is shared when outputs use the same video and audio config

Stage sharing is keyed by `(pipeline_id, stage_key)`:

```mermaid
flowchart LR
    A["output A: 720p"] --> Lookup["get_or_create_transcoder(720p)"]
    B["output B: 720p"] --> Lookup
    Lookup --> Stage["one shared transcoder"]
    Stage --> Ring["shared Arc&lt;RingBuffer&gt;"]
    Ring --> SenderA["independent sender A"]
    Ring --> SenderB["independent sender B"]
    SenderA --> FormatA["per-output packet formatting"]
    SenderB --> FormatB["per-output packet formatting"]
```

The per-packet format conversion (AVCC wrap, ADTS strip) is NOT shared between
egress tasks. This is intentional: sharing would require synchronization and
outweigh the ~700 ns per frame conversion cost. What IS shared is the far more
expensive encode stage (CPU-bound, seconds of latency). This invariant is
covered by `same_encoding_outputs_share_one_transcoder_stage` in engine tests,
and holds regardless of egress routing.

"Independent sender" above describes protocol/retry state ownership, not a
literal OS thread per destination: under the egress fabric (see
`docs/egress-implementation.md`), each output is a leaf serviced by a
shared shard OS thread alongside other outputs, not a dedicated thread.

Current measurements belong in the
[quality baseline ledger](agent-guidance/quality/baselines.md). This guide owns
the sharing invariant, not a copied performance snapshot.

## Audio stage cache

Output reconciliation splits compound encodings into a video stage and an audio
stage. Audio stages are keyed by the upstream stage identity as well as the
audio operation (e.g. `audio:atrack:0:from:video:720p:codec:h264`), preventing
outputs using different presets or codecs from cross-contaminating.

`atrack` stages run in the lightweight packet router and only select/reindex
audio tracks. `remap` and `downmix` stages run through the external FFmpeg
stage, copy video, filter one selected audio track to stereo AAC, and then feed
the normal MPEG-TS demux back into the shared output ring.

## Buffer sizing for 4K 60fps

| Component | Size | Constraint | Source |
|---|---|---|---|
| Standby GOP cache | 16 MiB payload / 2,048 packets per connected RTMP/SRT standby | Keeps only the latest complete compressed GOP; invalidates on either limit | `standby_gop.rs` |
| RingBuffer capacity | 4096 slots | ~24s at 170 pkt/s (4K60). Overflow fast-forwards to most recent keyframe | `engine.rs` |
| AVIO buffer | 32 KB | FFmpeg internal read/write chunk | `avio.rs` |
| MemoryQueue | Bounded `VecDeque<u8>` (2 MB) | Backpressure is structural: writer yields on full, consumer blocks on empty `read()` | `avio.rs` |
| HLS segment accumulator | 8 MB initial | 4K60 H.264 segment at 6s can reach 12 MB; grows if needed | `hls.rs` |
| HLS MAX_SEGMENTS | 10 | ~60s sliding window. 10 × 8 MB = 80 MB worst case per pipeline at 4K | `hls.rs` |
| HLS TARGET_DURATION | 6s | MIN_SEGMENT (1s) prevents micro-segments from keyframe bursts | `hls.rs` |
| RTMP TCP SO_RCVBUF/SO_SNDBUF | 128 KB before auth, 8 MB after publish auth | Limits unauthenticated connection footprint while preserving burst headroom for accepted publishers | `rtmp.rs` |
| SRT SRTO_LATENCY | 250 ms | Dejitter + retransmit window. At 50 Mbps = 1.56 MB in flight | `srt.rs` |
| SRT SRTO_LOSSMAXTTL | 256 packets | Reorder tolerance. At 50 Mbps/1316 B ≈ 54 ms | `srt.rs` |
| SRT UDP buffers | 8 MB | Kernel SO_RCVBUF/SNDBUF. Requires `rmem_max`/`wmem_max` ≥ 8 MB | `srt.rs` |
| SRT internal buffers | 12 MB | libsrt retransmission/reordering. ≥ latency × bitrate × (1+loss) | `srt.rs` |
| SRT SRTO_FC | 32768 packets | Flow control window. 32768 × 1316 B ≈ 43 MB window | `srt.rs` |
| SRT SRTO_MAXBW | unlimited | Auto-detect bandwidth from input rate | `srt.rs` |
| SRT recv buffer | 1316 bytes (single) / 2048 bytes (group) | One SRT payload per receive | `srt.rs` |

Runtime verification: `srt_log_effective_opts` reads back values after
`srt_setsockopt` and warns if the kernel clamped UDP buffers.

## Thread and memory ownership, ingest to egress

This section traces one publisher's media from its entry socket to its exit
socket for each protocol, naming which concurrency primitive owns each hop
and which structure owns the memory at that hop. It complements
[Architecture § Runtime ownership](architecture.md#runtime-ownership), which
states the general policy (Tokio owns non-blocking work; blocking native
calls are isolated on dedicated OS threads); this section applies that
policy to the two concrete ingest/egress protocols. Consistent with that
policy statement, this is an ownership and scaling-formula map, not a copied
thread count — exact counts depend on live CPU count, feed count, and
output count. A fully worked, measured example for one 1,200-output MSR run
(exact thread histogram, RSS breakdown, and a per-connection memory model)
lives in
[the MSR resource-attribution investigation](agent-guidance/quality/msr-1200-resource-attribution-2026-08-13.md).

### RTMP: ingest to egress

```mermaid
flowchart LR
    subgraph T1["Tokio worker pool (fixed size; RESTREAM_TOKIO_WORKER_THREADS)"]
        RS["RTMP socket accept + parse\n(inline async)"] --> Gate["Selected-input gate"]
        Gate --> SR[("source_ring — shared SPMC")]
        SR -->|"non-passthrough preset"| Prep["Reader + TsMuxer\n(inline async)"]
        Stdin["FFmpeg stdin writer\n(async pipe I/O)"]
        Stdout["FFmpeg stdout reader\n(async pipe I/O)"]
        Stdout --> Demux["TsDemuxer\n(inline async)"]
        Demux --> OR[("output_ring — shared SPMC,\none per (pipeline, preset)")]
    end
    subgraph P1["FFmpeg child process (own OS process, not a Restream thread)"]
        FF["scale + libx264/libx265"]
    end
    Prep --> Stdin --> FF --> Stdout
    subgraph S1["Egress fabric shard pool: OS threads,\ncount = OutputCount profile\n(ceil(feed output count / 128), capped at the CPU-derived ceiling)"]
        Leaf["RTMP leaf: chunking, ack,\noptional TLS state"] --> Send["non-blocking TCP write"]
    end
    SR -->|"passthrough"| Leaf
    OR --> Leaf
    Send --> Dest["Destination RTMP/RTMPS server"]
```

| Hop | Thread/process model | Memory owner |
|---|---|---|
| Socket accept, RTMP/FLV parse | Tokio worker, inline async | Per-connection read buffer; kernel `SO_RCVBUF`/`SO_SNDBUF` (128 KiB pre-auth, 8 MiB post-auth) — kernel-owned, not process RSS |
| `source_ring` / `output_ring` | No dedicated thread; shared structure | One fixed-capacity `RingBuffer` (1024 / 512 slots) per pipeline / per `(pipeline, preset)` stage, regardless of destination count |
| External transcoder (non-passthrough preset) | 1 FFmpeg child process + 2 Tokio tasks (stdin writer, stdout reader) per `(pipeline, preset)` | FFmpeg's own process memory (outside Restream's RSS) plus the pipe/`MemoryQueue` bridge |
| Egress shard (RTMP/RTMPS) | Fixed OS-thread pool per feed, sized by `EgressShardProfile::OutputCount`: `ceil(output_count / 128)`, capped at the CPU-derived ceiling (`src/config.rs`) | Per-leaf `LeafCommon`: small fixed state plus bounded pending bytes (`RESTREAM_EGRESS_MAX_PENDING_BYTES`, 256 KiB ceiling); TCP send buffer is kernel-owned (`RESTREAM_RTMP_STREAM_BUFFER_BYTES`, 8 MiB), not RSS |

### SRT: ingest to egress

```mermaid
flowchart LR
    subgraph L1["libsrt ingest multiplexer: 1 CSndQueue + 1 CRcvQueue\nworker-thread pair (one bound local UDP endpoint),\nplus 1 TsbPd thread per live ingest connection"]
        SS["SRT socket accept/recv"]
    end
    subgraph T2["Tokio worker pool"]
        SS --> Demux2["TsDemuxer\n(inline async)"]
        Demux2 --> SR2[("source_ring — shared SPMC")]
        SR2 -->|"non-passthrough preset"| Prep2["shared transform stage\n(as in RTMP path)"]
        Prep2 --> OR2[("output_ring")]
        OR2 --> Mux["shared TsMuxer,\n1 task per (pipeline, preset)\n(inline async)"]
        SR2 -->|"passthrough"| Mux
        Mux --> TCR[("TsChunkRing — shared SPMC\npackage ring")]
    end
    subgraph S2["Egress fabric shard pool: OS threads per feed,\ncount = SrtCpuParallel profile\n(always the CPU-derived ceiling, independent of that feed's output count)"]
        Leaf2["SRT leaf: connection,\ncongestion, encryption state"] --> Send2["non-blocking srt_sendmsg2"]
    end
    TCR --> Leaf2
    subgraph L2["libsrt egress multiplexer: 1 per (pipeline, shard) by default,\nshared across every feed of that pipeline assigned to that shard —\n1 CSndQueue + 1 CRcvQueue worker-thread pair"]
        Send2 --> Buf["CSndBuffer (~6 MB negotiated\nceiling per socket) + TSBPD\ndeadline enforcement"]
    end
    Buf --> Dest2["Destination SRT receiver"]
```

| Hop | Thread/process model | Memory owner |
|---|---|---|
| SRT ingest socket, libsrt multiplexer | 1 multiplexer for the whole ingest listener: 1 `CSndQueue` + 1 `CRcvQueue` thread pair, plus 1 `SRT:TsbPd` delivery-timing thread per live connection | libsrt's own `CRcvBuffer` per connection; kernel `SO_RCVBUF` is separate and kernel-owned |
| `TsDemuxer` → `source_ring` | Tokio worker, inline async | Shared `source_ring`, same structure as RTMP |
| Shared `TsMuxer` (SRT preparation) | 1 Tokio task per `(pipeline, preset)`, inline async | `TsChunkRing` (256-chunk shared ring, `RESTREAM_TS_RING_CAPACITY`) |
| Egress shard (SRT) | Fixed OS-thread pool **per feed** (a feed is one `(protocol, pipeline, encoding)` selection, e.g. one selected audio track), sized by `EgressShardProfile::SrtCpuParallel`: always the CPU-derived ceiling, **not reduced for a small feed** — this is deliberate, not an oversight; see below | Per-leaf `LeafCommon`, same small bound as RTMP |
| libsrt egress multiplexer | 1 per `(pipeline, shard id)` by default, shared across every feed of that pipeline on shard *N* — so multiplexer/thread count tracks `shard count x active pipeline count`, not feed count or output count (`src/media/egress/backends/srt/muxer_ports.rs`; `RESTREAM_SRT_EGRESS_MUXER_PORT_PIPELINE_SCOPED=0` reverts to sharing by shard id alone, engine-wide) | libsrt's own `CSndBuffer`, a real per-**socket** userspace allocation (~6.1 MB negotiated ceiling from `DESIRED_SRT_BUF`, `src/media/srt/socket.rs`) that counts toward process RSS — roughly 2.8x RTMP's per-connection memory cost in the measured example above. Kernel `SO_SNDBUF` (`DESIRED_UDP_BUF`, 8 MiB requested) is separate and does not count toward RSS |

`SrtCpuParallel` claiming the full CPU-derived shard ceiling for every SRT
feed regardless of that feed's own output count is the fix for a real,
previously-shipped bug, not an unexamined default: an earlier
output-count-scaled formula (matching RTMP's `OutputCount` profile) capped a
small SRT feed at 1 shard / 1 libsrt multiplexer, and one multiplexer's
single `CSndQueue` thread became a hard bottleneck once concurrent SRT
egress connections crossed roughly 120, triggering continuous `TLPKTDROP`
packet loss (full account in
[the SRT egress scale investigation](agent-guidance/quality/srt-egress-scale-investigation-2026-08-10.md)).
The corresponding cost is that egress-shard thread count for SRT scales with
**distinct SRT feed count** times the CPU-derived shard ceiling — a
process with many small, distinct SRT track selections pays the same
per-feed shard-thread cost as one with a few large ones. Making that
cheaper without reopening the fixed bug is an open, unimplemented
improvement — see the "Efficiency evaluation" note in the resource
attribution doc linked above for the specific tradeoff and why it was not
attempted in the same session that just proved the current design correct
at 1,200 outputs.

## SRT bonding

Production egress bonding supports both `SRT_GTYPE_BACKUP` and
`SRT_GTYPE_BROADCAST` in either the native libsrt or pure-Rust backend. The
mode is selected with `bondmode=backup|broadcast` on the `bond=` URL; omitting
it preserves the historical Backup default. The receiver must still advertise
the same group type in its GROUP handshake extension. The remaining phased
work is live differential scale/failover evidence for both modes, not a
second mode-specific implementation path. See [`srt-pure-rust-plan.md`](srt-pure-rust-plan.md)
and [`srt-pure-rust-design.md`](srt-pure-rust-design.md) for the layering and
interop rationale.

### Ingest

The SRT listener requests `SRTO_GROUPCONNECT=1`. A publisher-created bonded
connection is accepted as one logical group: the first member returns a group
ID from `srt_accept`, later members attach in the background, and one
`srt_recv(group_id)` loop feeds one demuxer/ring producer. `srt_group_data()`
reports member state through health/diagnostics.

StreamID alone does not create a group. Two independent sockets with matching
StreamIDs are rejected as duplicate publishers.

Requires libsrt compiled with `ENABLE_BONDING=ON`; startup warns and retains
single-link ingest otherwise. All builds link against the repo-managed static
SRT build from `.local/build/static/prefix`, so bonded-ingest support no longer
depends on the distro `libsrt` package.

### Egress

Bonded links use the `bond=` URL parameter, with an optional group mode:

```text
srt://primary:10080?streamid=publish:key&bond=backup1:10080,backup2:10080&bondmode=backup
srt://primary:10080?streamid=publish:key&bond=backup1:10080,backup2:10080&bondmode=broadcast
```

`bondmode=backup` creates an `SRT_GTYPE_BACKUP` group: the highest-weight
active member sends and standby members take over after failure.
`bondmode=broadcast` creates an `SRT_GTYPE_BROADCAST` group: active members
send the same sequence and the receiver deduplicates it by group sequence.
Both single-connection and bonded egress groups apply their resolved socket
options before transmission.

## Protocol correctness requirements

### Probe with matching ingest protocol

Probing must use the same read protocol as the active ingest. Cross-protocol
probing can create false positives (e.g., probing SRT ingest through RTMP
requires additional packetization). The diagnostics endpoint rejects mismatched
probe protocols.

### SRT Stream ID normalization

The listener accepts these shapes:

```text
publish:<key>             publisher:<key>
read:<key>                play:<key>           subscriber:<key>
<key>
#!::r=<key>,m=publish
#!::r=<key>,m=request
```

Query parameters are stripped before database validation.
Slash-delimited application prefixes are RTMP-only; SRT treats the decoded
resource string as the stream key.

### Media streams only

Read endpoints must emit media payload only. The pipeline selects the first
video stream and preserves all audio tracks. Subtitles, private data, second
video PIDs, and unknown stream types are excluded. The MPEG-TS remuxer rejects
unknown codec metadata rather than guessing H.264/AAC.

The control plane surfaces MPEG-TS stream identity metadata separately from the
media payload: video and audio metadata may include PID, language, and title
fields when descriptors are present, and audio tracks can be assigned local
operator-friendly labels in the dashboard.

### Timestamp semantics

RTMP video timestamps are decode timestamps. AVC/HEVC packets carry a signed
24-bit composition-time offset:

```text
DTS = RTMP timestamp
PTS = DTS + signed composition-time offset
```

Ingest stores both values correctly. RTMP play and egress use `packet.dts` as
the RTMP message timestamp for video (audio uses PTS). B-frame round-trip tests
remain desirable.

### H.265

H.265 must be tested explicitly and cannot be inferred from H.264 results.
SRT/MPEG-TS should preserve HEVC codec identity. RTMP H.265 requires Enhanced
RTMP handling. Until RTMP H.265 is proven end-to-end, diagnostics should prefer
SRT read/probe for SRT H.265 publishers.
