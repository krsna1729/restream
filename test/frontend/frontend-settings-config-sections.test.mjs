import assert from "node:assert/strict";
import test from "node:test";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

function makeResponse(payload, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function appendElement(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

// Regression coverage for real, previously-shipped bugs: populateRecordingSettings/
// saveRecordingSettings and populateBackendPolicySettings/saveBackendPolicy read/wrote
// element ids (settings-rec-*, settings-backend-*) that don't exist anywhere in the
// rendered markup, and sent payload shapes (enabled/outputDir/format/...,
// allowExternalTranscoderExec/preferredEngine/strictMode) that don't exist on the
// backend RecordingSettings/BackendPolicy models at all. Both save buttons were
// silently no-ops / would fail server-side deserialization on every click.

test(
  "populateRecordingSettings reads state into the real recording-retain-source-ts id",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const retainInput = appendElement(
      document,
      "input",
      "recording-retain-source-ts",
    );

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { recordingSettings: { retainSourceTs: true } };

    configSections.populateRecordingSettings();

    assert.equal(retainInput.checked, true);
  },
);

test(
  "saveRecordingSettings sends the real RecordingSettings shape, not enabled/outputDir",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    appendElement(document, "input", "recording-retain-source-ts").checked = true;
    appendElement(document, "span", "recording-settings-saved").classList.add(
      "hidden",
    );

    let capturedBody = null;
    globalThis.fetch = async (url, init) => {
      assert.equal(String(url), "/api/v1/settings");
      assert.equal(init.method, "PATCH");
      capturedBody = JSON.parse(init.body);
      return makeResponse({
        serverName: "s",
        ingestHost: "",
        ingestSecurity: {},
        recordingSettings: capturedBody.recordingSettings,
        srtIngest: {},
        backendPolicy: {},
      });
    };

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { recordingSettings: {} };

    await configSections.saveRecordingSettings();

    assert.deepEqual(capturedBody.recordingSettings, { retainSourceTs: true });
  },
);

test(
  "populateBackendPolicySettings reads state into the real backend-policy-internal-* ids",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const videoPresets = appendElement(
      document,
      "input",
      "backend-policy-internal-video-presets",
    );
    const hevcToH264 = appendElement(
      document,
      "input",
      "backend-policy-internal-hevc-to-h264",
    );
    const hlsPreview = appendElement(
      document,
      "input",
      "backend-policy-internal-hls-preview",
    );
    const complexAudio = appendElement(
      document,
      "input",
      "backend-policy-internal-complex-audio",
    );

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = {
      backendPolicy: {
        internalVideoPresets: true,
        internalHevcToH264: false,
        internalHlsPreview: true,
        internalComplexAudio: false,
      },
    };

    configSections.populateBackendPolicySettings();

    assert.equal(videoPresets.checked, true);
    assert.equal(hevcToH264.checked, false);
    assert.equal(hlsPreview.checked, true);
    assert.equal(complexAudio.checked, false);
  },
);

test(
  "saveBackendPolicy sends the real BackendPolicy shape, not allowExternalTranscoderExec/preferredEngine",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    appendElement(
      document,
      "input",
      "backend-policy-internal-video-presets",
    ).checked = true;
    appendElement(
      document,
      "input",
      "backend-policy-internal-hevc-to-h264",
    ).checked = true;
    appendElement(
      document,
      "input",
      "backend-policy-internal-hls-preview",
    ).checked = false;
    appendElement(
      document,
      "input",
      "backend-policy-internal-complex-audio",
    ).checked = false;
    appendElement(document, "span", "backend-policy-saved").classList.add(
      "hidden",
    );

    let capturedBody = null;
    globalThis.fetch = async (_url, init) => {
      capturedBody = JSON.parse(init.body);
      return makeResponse({
        serverName: "s",
        ingestHost: "",
        ingestSecurity: {},
        recordingSettings: {},
        srtIngest: {},
        backendPolicy: capturedBody.backendPolicy,
      });
    };

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { backendPolicy: {} };

    await configSections.saveBackendPolicy();

    assert.deepEqual(capturedBody.backendPolicy, {
      internalVideoPresets: true,
      internalHevcToH264: true,
      internalHlsPreview: false,
      internalComplexAudio: false,
    });
  },
);
