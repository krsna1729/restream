import assert from "node:assert/strict";
import test from "node:test";

import {
  FakeElement,
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

function makeResponse(payload, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function makeOutputStatus(overrides = {}) {
  return {
    desiredState: "started",
    status: "off",
    flapping: false,
    retrying: false,
    ...overrides,
  };
}

test("preview HLS config stays close to the live edge with bounded catch-up", async () => {
  const { buildPreviewHlsConfig } = await loadCompiledFrontendModule(
    "features/hls-playback-config.js",
  );

  assert.deepEqual(buildPreviewHlsConfig(), {
    startLevel: -1,
    lowLatencyMode: true,
    liveSyncDurationCount: 1,
    liveMaxLatencyDurationCount: 2,
    maxLiveSyncPlaybackRate: 1.5,
    backBufferLength: 6,
  });
});

test("audio caps load and detection logic normalizes payloads and URL inference", async () => {
  installFakeDom();
  globalThis.fetch = async (url) => {
    assert.equal(String(url), "/api/v1/audio-caps");
    return makeResponse({
      caps: {
        "youtube:rtmp": { maxTracks: 2, maxChannels: null, codecs: ["aac"] },
        "generic:hls": { maxTracks: null, maxChannels: 6, codecs: null },
      },
      platformLabels: {
        youtube: "YouTube",
        facebook: "Facebook Live",
        vdocipher: "VdoCipher",
        generic: "Everywhere",
      },
    });
  };

  const audioCaps = await loadCompiledFrontendModule("core/audio-caps.js");

  assert.equal(audioCaps.isAudioCapsLoaded(), false);
  await audioCaps.loadAudioCaps();

  assert.equal(audioCaps.isAudioCapsLoaded(), true);
  assert.deepEqual(audioCaps.getAudioCaps("youtube", "rtmp"), {
    maxTracks: 2,
    maxChannels: Infinity,
    codecs: ["aac"],
  });
  assert.deepEqual(audioCaps.getAudioCaps("generic", "hls"), {
    maxTracks: Infinity,
    maxChannels: 6,
    codecs: "any",
  });
  assert.equal(audioCaps.getAudioPlatformLabel("generic"), "Everywhere");
  assert.equal(
    audioCaps.detectAudioPlatform("https://live.vd0.co/channel/test"),
    "vdocipher",
  );
  assert.equal(
    audioCaps.detectAudioProtocol("https://example.com/live/out.m3u8"),
    "hls",
  );
  assert.equal(audioCaps.detectAudioProtocol("bad-url", "srt"), "srt");
});

test("output status helpers distinguish intent, running, retrying, and unexpected down states", async () => {
  installFakeDom();
  const status = await loadCompiledFrontendModule("core/output-status.js");

  assert.equal(
    status.isOutputIntentStopped(makeOutputStatus({ desiredState: "stopped" })),
    true,
  );
  assert.equal(
    status.isOutputRunning(makeOutputStatus({ status: "running" })),
    true,
  );
  assert.equal(
    status.isOutputRetrying(makeOutputStatus({ status: "retrying" })),
    true,
  );
  assert.equal(
    status.isOutputManagedActive(makeOutputStatus({ retrying: true })),
    true,
  );
  assert.equal(
    status.isOutputFlapping(makeOutputStatus({ flapping: true })),
    true,
  );
  assert.equal(
    status.isOutputUnexpectedlyDown(
      makeOutputStatus({ desiredState: "started", status: "off" }),
    ),
    true,
  );
  assert.equal(
    status.isOutputUnexpectedlyDown(
      makeOutputStatus({ desiredState: "stopped", status: "off" }),
    ),
    false,
  );
});

test("core utils cover URL, masking, formatting, clipboard, and selection helpers", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/dashboard?mode=overview";
  let pushedUrl = null;
  window.history.pushState = (_state, _title, url) => {
    pushedUrl = String(url);
    window.location.href = String(url);
  };

  const title = document.createElement("title");
  title.setAttribute("data-name", "Dashboard");
  document.body.appendChild(title);

  const serverName = document.createElement("div");
  serverName.id = "server-name";
  document.body.appendChild(serverName);

  const copied = document.createElement("div");
  copied.id = "copied-notification";
  copied.classList.add("hidden");
  document.body.appendChild(copied);

  const saving = document.createElement("div");
  saving.id = "saving-badge";
  saving.classList.add("hidden");
  document.body.appendChild(saving);

  const copyTarget = document.createElement("div");
  copyTarget.id = "copy-target";
  copyTarget.dataset.copy = "secret-value";
  copyTarget.innerText = "secret-value";
  document.body.appendChild(copyTarget);

  const utils = await loadCompiledFrontendModule("core/utils.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");
  state.pipelines = [{ id: "pipe-1", name: "Primary" }];
  window.location.href = "http://localhost/dashboard?p=pipe-1";

  assert.equal(utils.msToHHMMSS(3_661_000), "1:01:01");
  assert.equal(utils.escapeHtml(`a<&>"'`), "a&lt;&amp;&gt;&quot;&#39;");
  assert.match(
    utils.maskSecret("rtmp://example.com/live/abcdefghijklmnopqrstuvwxyz"),
    /\*\*\*/,
  );
  assert.match(
    utils.sanitizeLogMessage(
      "rtmp://example.com/live/abcdefghijklmnopqrstuvwxyz",
    ),
    /\*\*\*/,
  );
  const hostileRedacted = utils.escapeRedactedHtml(
    'rtmp://example.com/live/abcdefghijklmnopqrstuvwxyz"><img src=x onerror=alert(1)>',
  );
  assert.match(hostileRedacted, /\*\*\*/);
  assert.doesNotMatch(hostileRedacted, /<img/i);
  assert.match(hostileRedacted, /&lt;img/);
  assert.equal(utils.formatCodecName("avc1"), "H.264");
  assert.equal(utils.formatCodecName("opus"), "Opus");
  assert.equal(utils.isValidOutput("rtmps://example.com/live/key"), true);
  assert.equal(utils.isValidMonitoringUrl("srt://example.com:9000"), true);

  utils.setUrlParam("mode", "inspect");
  assert.match(pushedUrl, /mode=inspect/);
  assert.equal(utils.getUrlParam("mode"), "inspect");

  utils.writeSelectedPipelineHint({ id: "pipe-1", name: "Primary" });
  assert.deepEqual(utils.readSelectedPipelineHint(), {
    id: "pipe-1",
    name: "Primary",
  });

  utils.setServerConfig("Studio");
  assert.equal(document.title, "Studio: Dashboard - Primary");
  assert.equal(serverName.textContent, "Restream: Studio");

  utils.showLoading();
  assert.equal(saving.classList.contains("flex"), true);
  utils.hideLoading();
  assert.equal(saving.classList.contains("hidden"), true);

  await utils.copyData("copy-target");
  assert.equal(copied.classList.contains("hidden"), false);

  assert.equal(utils.getStatusColor("warning"), "yellow");
  assert.equal(
    utils.formatMaskedStreamKey("channel_secretvalue"),
    "channel_se***ue",
  );
  assert.equal(utils.formatChannelCount(6), "5.1 (6 ch)");
});

test("editor output-url helpers resolve presets, protocols, and SRT fields", async () => {
  const outputUrl = await loadCompiledFrontendModule(
    "features/editor/output-url.js",
  );

  assert.equal(outputUrl.protocolUsesOutputServerPresets("hls"), true);
  assert.equal(
    outputUrl.resolvePresetOutputUrl(
      "https://a.upload.youtube.com/http_upload_hls?cid=${stream_key}",
      "stream key",
    ),
    "https://a.upload.youtube.com/http_upload_hls?cid=stream%20key",
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmp://a.rtmp.youtube.com/live2/abc123",
    ),
    {
      value: "rtmp://a.rtmp.youtube.com/live2/",
      inputValue: "abc123",
      rtmpMode: "enhanced",
    },
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmp://b.rtmp.youtube.com/live2?backup=1/backup-key",
    ),
    {
      value: "rtmp://b.rtmp.youtube.com/live2?backup=1/",
      inputValue: "backup-key",
      rtmpMode: "enhanced",
    },
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmps://live-api-s.facebook.com:443/rtmp/abc123",
    ),
    {
      value: "rtmps://live-api-s.facebook.com:443/rtmp/",
      inputValue: "abc123",
    },
  );
  assert.equal(
    outputUrl.detectOutputProtocol("https://example.com/live/out.m3u8"),
    "hls",
  );
  assert.equal(
    outputUrl.extractCandidateStreamToken(
      "srt://example.com:9000?streamid=publish:main-feed",
    ),
    "main-feed",
  );
  assert.equal(
    outputUrl.getDefaultOutputToken("https://example.com/hls/show/out.m3u8"),
    "show",
  );
  assert.deepEqual(
    outputUrl.parseSrtFields(
      "srt://example.com:10080?streamid=publish:feed&passphrase=supersecret1&pbkeylen=24&latency=200",
    ),
    {
      host: "example.com",
      port: "10080",
      streamId: "publish:feed",
      passphrase: "supersecret1",
      pbkeylen: "24",
      extraQuery: "latency=200",
    },
  );
  assert.equal(
    outputUrl.buildDefaultCustomOutputUrl(
      "rtmp",
      "rtmp://seed/live/key",
      "demo",
    ),
    "rtmp://demo:1935/live/key",
  );
});

test("audio track labels persist friendly names with title and language fallbacks", async () => {
  installFakeDom();
  const labels = await loadCompiledFrontendModule(
    "features/audio-track-labels.js",
  );

  const track = { pid: 256, index: 1, language: "eng", title: "Main Mix" };
  assert.equal(labels.audioTrackKey(track, 0), "pid:256");
  assert.equal(
    labels.audioTrackIdentifier(track, 0),
    "PID 0x100 / Track 2 / ENG",
  );
  assert.equal(labels.getAudioTrackLabel("pipe-1", track, 0), "Main Mix");

  labels.setAudioTrackStoredLabel("pipe-1", track, 0, "Program");
  assert.equal(labels.getAudioTrackStoredLabel("pipe-1", track, 0), "Program");
  assert.equal(labels.getAudioTrackLabel("pipe-1", track, 0), "Program");

  labels.setAudioTrackStoredLabel("pipe-1", track, 0, " ");
  assert.equal(labels.getAudioTrackStoredLabel("pipe-1", track, 0), "");
  assert.equal(
    labels.getAudioTrackLabel("pipe-1", { index: 2, language: "spa" }, 0),
    "SPA",
  );
});

test("pipeline parsing maps input, output, retry, and throughput fields", async () => {
  installFakeDom();
  const { parsePipelinesInfo } =
    await loadCompiledFrontendModule("core/pipeline.js");

  const config = {
    pipelines: [
      {
        id: "pipe-1",
        name: "Pipeline 1",
        streamKey: "stream-key",
        inputSource: "file:clip.ts",
        srtIngestPolicy: "allow",
        ingestUrls: { rtmp: "rtmp://example.com/live/key", srt: null },
        fileIngest: { configured: true, id: "ingest-1" },
      },
    ],
    outputs: [
      {
        id: "out-1",
        pipelineId: "pipe-1",
        name: "Primary",
        desiredState: "started",
        url: "rtmp://dest/live/key",
        monitoringUrl: "https://example.com/hls/out.m3u8",
        encoding: "source",
      },
    ],
    jobs: [
      {
        pipelineId: "pipe-1",
        outputId: "out-1",
        startedAt: "2026-06-30T00:00:10Z",
      },
      {
        pipelineId: "pipe-1",
        outputId: "out-1",
        startedAt: "2026-06-30T00:00:20Z",
      },
    ],
  };

  const baseHealth = {
    pipelines: {
      "pipe-1": {
        input: {
          status: "off",
          disconnectGraceActive: true,
          disconnectGraceRemainingMs: 1800,
          bytesReceived: 12_000,
          bytesSent: 5_000,
          readers: 2,
          bitrateKbps: 3200.44,
          video: { codec: "h264", width: 1280, height: 720 },
          audioTracks: [
            {
              trackIndex: 0,
              pid: 256,
              codec: "aac",
              channels: 2,
              sampleRate: 48_000,
              language: "eng",
            },
          ],
          publisher: { protocol: "srt", remoteAddr: "10.0.0.5:9000" },
          unexpectedReaders: { count: 1 },
          lastSessionProtocol: "srt",
          recentDisconnectError: true,
        },
        outputs: {
          "out-1": {
            status: "retrying",
            retrying: true,
            uptimeSecs: 12.5,
            totalSize: 10_000,
            bytesSent: 10_000,
            bytesDelivered: 10_000,
            lastError: "connection reset",
            lastErrorAt: "2026-06-30T00:00:11Z",
            monitoringUrl: "https://example.com/hls/out.m3u8",
          },
        },
        recording: { enabled: true, active: false },
        hlsPreview: {
          active: true,
          persistentConsumers: 2,
          lastAccessAgeMs: 4_000,
          segments: 5,
          playlistBytes: 512,
        },
      },
    },
  };

  const originalDateNow = Date.now;
  let fakeNow = 1_000;
  let first;
  let second;
  try {
    Date.now = () => fakeNow;
    first = parsePipelinesInfo(config, baseHealth);
    fakeNow += 1_000;
    second = parsePipelinesInfo(config, {
      pipelines: {
        "pipe-1": {
          ...baseHealth.pipelines["pipe-1"],
          outputs: {
            "out-1": {
              ...baseHealth.pipelines["pipe-1"].outputs["out-1"],
              status: "running",
              uptimeSecs: 9.25,
              totalSize: 30_000,
              bytesSent: 30_000,
              bytesDelivered: 30_000,
            },
          },
        },
      },
    });
  } finally {
    Date.now = originalDateNow;
  }

  assert.equal(first[0].input.status, "warning");
  assert.equal(first[0].input.audioTracks[0].pid, 256);
  assert.equal(first[0].recording.enabled, true);
  assert.equal(first[0].hlsPreview.segments, 5);
  assert.equal(first[0].outs[0].retrying, true);
  assert.equal(first[0].stats.unexpectedReadersCount, 1);
  assert.equal(first[0].outs[0].job.startedAt, "2026-06-30T00:00:20Z");
  assert.equal(first[0].outs[0].time, null);
  assert.equal(second[0].outs[0].time, 9_250);
  assert.equal(second[0].outs[0].bitrateKbps, 160);
});

test("pipeline parsing tolerates malformed collection and nested config shapes", async () => {
  installFakeDom();
  const { parsePipelinesInfo } =
    await loadCompiledFrontendModule("core/pipeline.js");

  assert.deepEqual(
    parsePipelinesInfo(
      { pipelines: {}, outputs: "not-an-array", jobs: null },
      { pipelines: [] },
    ),
    [],
  );

  const parsed = parsePipelinesInfo(
    {
      pipelines: [
        null,
        "not-a-pipeline",
        {
          id: "pipe-malformed",
          name: "Safe pipeline",
          streamKey: "safe-key",
          ingestUrls: "not-an-object",
        },
        { id: "" },
      ],
      outputs: [
        null,
        {
          id: "out-malformed",
          pipelineId: "pipe-malformed",
          name: "Safe output",
          url: 42,
          config: {
            video: null,
            audio: "not-an-object",
            protocol: { type: "rtmp", mode: "future-mode" },
          },
        },
        { id: null, pipelineId: "pipe-malformed" },
      ],
      jobs: [null, { pipelineId: "pipe-malformed", outputId: null }],
    },
    {
      pipelines: {
        "pipe-malformed": {
          input: {
            video: "not-a-track",
            audioTracks: { length: 1, 0: { codec: "aac" } },
          },
          outputs: { "out-malformed": "not-health" },
          recording: null,
          hlsPreview: "not-health",
        },
      },
    },
  );

  assert.equal(parsed.length, 1);
  assert.equal(parsed[0].id, "pipe-malformed");
  assert.deepEqual(parsed[0].ingestUrls, { rtmp: null, srt: null });
  assert.equal(parsed[0].input.video, null);
  assert.deepEqual(parsed[0].input.audioTracks, []);
  assert.equal(parsed[0].outs.length, 1);
  assert.equal(parsed[0].outs[0].url, "");
  assert.deepEqual(parsed[0].outs[0].config, {
    video: { mode: "source", codec: "auto" },
    audio: { mode: "all" },
    protocol: { type: "rtmp", mode: "legacy" },
  });
});

test("pipeline parsing rejects invalid telemetry and lets valid job timestamps win", async () => {
  installFakeDom();
  const { parsePipelinesInfo } =
    await loadCompiledFrontendModule("core/pipeline.js");

  const originalDateNow = Date.now;
  const now = Date.parse("2026-07-01T00:00:30Z");
  let parsed;
  try {
    Date.now = () => now;
    parsed = parsePipelinesInfo(
      {
        pipelines: [
          { id: "pipe-telemetry", name: "Telemetry", streamKey: "key" },
        ],
        outputs: [
          {
            id: "out-telemetry",
            pipelineId: "pipe-telemetry",
            name: "Telemetry output",
            url: "rtmp://example.test/live/key",
            config: null,
          },
        ],
        jobs: [
          {
            pipelineId: "pipe-telemetry",
            outputId: "out-telemetry",
            startedAt: "not-a-date",
          },
          {
            pipelineId: "pipe-telemetry",
            outputId: "out-telemetry",
            startedAt: "2026-07-01T00:00:20Z",
          },
          {
            pipelineId: "pipe-telemetry",
            outputId: "out-telemetry",
            endedAt: "still-not-a-date",
          },
        ],
      },
      {
        pipelines: {
          "pipe-telemetry": {
            input: {
              status: "on",
              publishStartedAt: "2026-07-01T00:00:10Z",
              bytesReceived: -1,
              bytesSent: Number.NaN,
              readers: Number.POSITIVE_INFINITY,
              bitrateKbps: Number.POSITIVE_INFINITY,
              lastProgressAgeMs: -5,
              disconnectGraceRemainingMs: -1,
              recentDisconnectCount: -2,
              lastSessionBytesReceived: Number.NaN,
              audioTracks: { length: 1 },
              audio: {
                trackIndex: 2,
                pid: 300,
                codec: "aac",
                channels: 2,
                sampleRate: 48_000,
              },
              unexpectedReaders: { count: -1 },
            },
            outputs: {
              "out-telemetry": {
                status: "running",
                uptimeSecs: -1,
                totalSize: Number.POSITIVE_INFINITY,
                bitrateKbps: Number.NaN,
                lastProgressAgeMs: -1,
                recentFailureCount: -1,
                retryAttempts: Number.POSITIVE_INFINITY,
                retryBackoffMs: -1,
                retryRemainingMs: Number.NaN,
              },
            },
            hlsPreview: {
              persistentConsumers: -1,
              lastAccessAgeMs: -1,
              segments: Number.NaN,
              playlistBytes: Number.POSITIVE_INFINITY,
            },
          },
        },
      },
    );
  } finally {
    Date.now = originalDateNow;
  }

  const pipeline = parsed[0];
  const output = pipeline.outs[0];
  assert.equal(pipeline.input.time, 20_000);
  assert.equal(pipeline.input.bytesReceived, 0);
  assert.equal(pipeline.input.bytesSent, 0);
  assert.equal(pipeline.input.readers, 0);
  assert.equal(pipeline.input.bitrateKbps, null);
  assert.equal(pipeline.input.lastProgressAgeMs, null);
  assert.equal(pipeline.input.audioTracks.length, 1);
  assert.equal(pipeline.input.audioTracks[0].index, 2);
  assert.equal(pipeline.input.audioTracks[0].sample_rate, 48_000);
  assert.equal(pipeline.input.unexpectedReadersCount, 0);
  assert.equal(pipeline.input.disconnectGraceRemainingMs, null);
  assert.equal(pipeline.input.recentDisconnectCount, 0);
  assert.equal(pipeline.input.lastSessionBytesReceived, null);
  assert.equal(pipeline.hlsPreview.persistentConsumers, 0);
  assert.equal(pipeline.hlsPreview.lastAccessAgeMs, null);
  assert.equal(pipeline.hlsPreview.segments, 0);
  assert.equal(pipeline.hlsPreview.playlistBytes, 0);
  assert.equal(output.job.startedAt, "2026-07-01T00:00:20Z");
  assert.equal(output.time, 10_000);
  assert.equal(output.totalSize, null);
  assert.equal(output.bitrateKbps, null);
  assert.equal(output.lastProgressAgeMs, null);
  assert.equal(output.recentFailureCount, 0);
  assert.equal(output.retryAttempts, null);
  assert.equal(output.retryBackoffMs, null);
  assert.equal(output.retryRemainingMs, null);
});

test("pipeline throughput state follows direct samples and is pruned on output removal", async () => {
  installFakeDom();
  const { parsePipelinesInfo } =
    await loadCompiledFrontendModule("core/pipeline.js");

  const pipeline = { id: "pipe-state", name: "State", streamKey: "key" };
  const output = {
    id: "out-state",
    pipelineId: "pipe-state",
    name: "State output",
    url: "rtmp://example.test/live/key",
    config: null,
  };
  const config = { pipelines: [pipeline], outputs: [output], jobs: [] };
  const healthAt = (totalSize, bitrateKbps) => ({
    pipelines: {
      "pipe-state": {
        outputs: {
          "out-state": { status: "running", totalSize, bitrateKbps },
        },
      },
    },
  });

  const originalDateNow = Date.now;
  let fakeNow = 1_000;
  try {
    Date.now = () => fakeNow;
    const direct = parsePipelinesInfo(config, healthAt(10_000, 900));
    assert.equal(direct[0].outs[0].bitrateKbps, 900);

    fakeNow += 1_000;
    const fallback = parsePipelinesInfo(config, healthAt(30_000, undefined));
    assert.equal(fallback[0].outs[0].bitrateKbps, 160);

    fakeNow += 1_000;
    parsePipelinesInfo(
      { pipelines: [pipeline], outputs: [], jobs: [] },
      { pipelines: {} },
    );

    fakeNow += 1_000;
    const recreated = parsePipelinesInfo(config, healthAt(50_000, undefined));
    assert.equal(recreated[0].outs[0].bitrateKbps, null);
  } finally {
    Date.now = originalDateNow;
  }
});

test("ingest detail rendering and publisher quality helpers surface operator-facing values", async () => {
  const { document } = installFakeDom();
  const grid = document.createElement("div");
  const heading = document.createElement("div");
  heading.id = "ingest-url-details-heading";
  const note = document.createElement("div");
  note.id = "ingest-url-details-note";
  document.body.appendChild(heading);
  document.body.appendChild(note);
  document.body.appendChild(grid);

  const ingestDetails = await loadCompiledFrontendModule(
    "features/ingest-url-details.js",
  );
  const publisherQuality = await loadCompiledFrontendModule(
    "features/publisher-quality.js",
  );
  const deps = await loadCompiledFrontendModule(
    "features/pipeline-dependencies.js",
  );

  const parsedRtmp = ingestDetails.parseProtocolAwareIngestUrl(
    "rtmp",
    "rtmps://user:pass@example.com:443/live/stream-key",
  );
  const parsedSrt = ingestDetails.parseProtocolAwareIngestUrl(
    "srt",
    "srt://example.com:10080?streamid=publish:feed&latency=200&mode=caller&passphrase=secret&pbkeylen=16&maxbw=1000000&foo=bar",
  );

  assert.equal(parsedRtmp.serverUrl, "rtmps://example.com:443/live");
  assert.equal(parsedRtmp.streamKey, "stream-key");
  assert.equal(parsedSrt.streamKey, "feed");

  ingestDetails.renderProtocolDetails(grid, "srt", parsedSrt);
  assert.equal(heading.textContent, "Operator Fields");
  assert.equal(note.classList.contains("hidden"), false);
  assert.equal(grid.children.length > 3, true);
  assert.equal(
    grid.children[2].querySelector("code") instanceof FakeElement,
    true,
  );

  const srtAlerts = publisherQuality.getPublisherQualityAlerts({
    protocol: "srt",
    quality: {
      srtBonded: true,
      srtGroupMemberCount: 1,
      srtGroupActiveMembers: 0,
      packetsReceivedLossPerSec: 5.5,
      packetsReceivedLoss: 42,
      packetsReceivedDropPerSec: 0,
      packetsReceivedRetransPerSec: 11,
      packetsReceivedRetrans: 7,
      packetsReceivedUndecryptPerSec: 1,
      packetsReceivedUndecrypt: 2,
      inboundRTPPacketsLost: 101,
      inboundRTPPacketsInError: 21,
      inboundRTPPacketsJitter: 31,
      msRTT: 210,
    },
  });
  const rtmpMetrics = publisherQuality.getPublisherQualityMetrics({
    protocol: "rtmp",
    quality: {
      tcpReceiveRateMbps: 4.5,
      tcpRttMs: 220.1,
      tcpRttVarMs: 8.4,
      tcpRcvRttMs: 6.2,
      tcpLastRcvMs: 5200,
      tcpUnacked: 0,
      tcpRetrans: 3,
      tcpLost: 2,
      tcpSndCwndBytes: 120_000,
      tcpRcvSpaceBytes: 65_535,
    },
  });

  assert.equal(publisherQuality.normalizePublisherProtocolLabel("srt"), "SRT");
  assert.ok(srtAlerts.some((alert) => alert.code === "srt_bond_members"));
  assert.ok(rtmpMetrics.some((metric) => metric.code === "tcp_rtt"));

  deps.setPipelineViewDependencies({
    refreshDashboard: async () => {},
  });
  assert.equal(
    typeof deps.pipelineViewDependencies.refreshDashboard,
    "function",
  );
});
