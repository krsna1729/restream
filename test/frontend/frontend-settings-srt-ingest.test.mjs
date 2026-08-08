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

// Regression coverage for a real, previously-shipped bug: populateSrtIngestSettings/
// saveSrtIngest read/wrote a completely different set of element ids
// (settings-srt-enabled/settings-srt-port/settings-srt-latency/settings-srt-passphrase)
// than the ids the settings page actually renders
// (srt-ingest-mode-input/srt-ingest-passphrase-input/srt-ingest-pbkeylen-input), and
// sent a payload shape (`enabled`/`port`) that doesn't exist on the backend
// SrtGlobalIngestConfig model at all. The save button was silently a no-op / would
// fail server-side deserialization on every click.

test(
  "populateSrtIngestSettings reads state into the real srt-ingest-* input ids",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const modeInput = appendElement(document, "select", "srt-ingest-mode-input");
    const passphraseInput = appendElement(
      document,
      "input",
      "srt-ingest-passphrase-input",
    );
    const pbkeylenInput = appendElement(
      document,
      "select",
      "srt-ingest-pbkeylen-input",
    );
    const latencyInput = appendElement(
      document,
      "input",
      "srt-ingest-latency-ms-input",
    );

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = {
      srtIngest: {
        mode: "encrypted",
        passphrase: "correct-horse-battery-staple",
        pbkeylen: 24,
        latencyMs: 400,
      },
    };

    configSections.populateSrtIngestSettings();

    assert.equal(modeInput.value, "encrypted");
    assert.equal(passphraseInput.value, "correct-horse-battery-staple");
    assert.equal(pbkeylenInput.value, "24");
    assert.equal(latencyInput.value, "400");
  },
);

test(
  "populateSrtIngestSettings defaults latency to 250ms when unset",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const latencyInput = appendElement(
      document,
      "input",
      "srt-ingest-latency-ms-input",
    );

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { srtIngest: {} };

    configSections.populateSrtIngestSettings();

    assert.equal(latencyInput.value, "250");
  },
);

test(
  "saveSrtIngest sends the real SrtGlobalIngestConfig shape, not enabled/port",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    appendElement(document, "select", "srt-ingest-mode-input").value =
      "encrypted";
    appendElement(document, "input", "srt-ingest-passphrase-input").value =
      "correct-horse-battery-staple";
    appendElement(document, "select", "srt-ingest-pbkeylen-input").value =
      "32";
    appendElement(document, "input", "srt-ingest-latency-ms-input").value =
      "2000";
    appendElement(document, "span", "srt-ingest-saved").classList.add(
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
        recordingSettings: {},
        srtIngest: capturedBody.srtIngest,
        backendPolicy: {},
      });
    };

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { srtIngest: {} };

    await configSections.saveSrtIngest();

    assert.deepEqual(capturedBody.srtIngest, {
      mode: "encrypted",
      passphrase: "correct-horse-battery-staple",
      pbkeylen: 32,
      latencyMs: 2000,
    });
  },
);

test(
  "saveSrtIngest clears passphrase and pbkeylen defaults to 16 in plaintext mode",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    appendElement(document, "select", "srt-ingest-mode-input").value =
      "plaintext";
    appendElement(document, "input", "srt-ingest-passphrase-input").value =
      "leftover-secret-value";
    appendElement(document, "select", "srt-ingest-pbkeylen-input").value =
      "24";
    appendElement(document, "input", "srt-ingest-latency-ms-input").value =
      "250";
    appendElement(document, "span", "srt-ingest-saved").classList.add(
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
        srtIngest: capturedBody.srtIngest,
        backendPolicy: {},
      });
    };

    const configSections = await loadCompiledFrontendModule(
      "features/settings/config-sections.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { srtIngest: {} };

    await configSections.saveSrtIngest();

    assert.equal(capturedBody.srtIngest.mode, "plaintext");
    assert.equal(capturedBody.srtIngest.passphrase, null);
  },
);
