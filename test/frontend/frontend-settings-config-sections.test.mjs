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

// Regression coverage for a real, previously-shipped bug: populateRecordingSettings/
// saveRecordingSettings read/wrote element ids (settings-rec-*) that don't exist
// anywhere in the rendered markup, and sent a payload shape
// (enabled/outputDir/format/...) that doesn't exist on the backend
// RecordingSettings model. The save button was a silent no-op.

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
