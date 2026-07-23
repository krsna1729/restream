import assert from "node:assert/strict";
import test from "node:test";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

function appendRoot(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

test(
  "renderDashboardV2SettingsBody owns the settings route body",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const container = appendRoot(
      document,
      "div",
      "dashboard-v2-settings-content",
    );

    const settings = await loadCompiledFrontendModule("features/settings.js");
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = {
      backendPolicy: {},
      ingestHost: "127.0.0.1",
      ingestSecurity: {},
      recordingSettings: {},
      serverName: "Synthetic Restream",
      srtIngest: {},
      transcodeProfiles: {
        mobile: {
          preset: "veryfast",
          tune: "zerolatency",
          crf: 26,
          gop: 60,
          bframes: 0,
          bitrate: 0,
          maxBitrate: 0,
          width: 854,
          height: 480,
        },
      },
    };

    settings.renderDashboardV2SettingsBody(container);

    assert.equal(container.dataset.settingsRouteBody, "v2");
    assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
    assert.doesNotMatch(container.innerHTML, /<h1[^>]*>Settings<\/h1>/);
    assert.match(container.innerHTML, /data-settings-action="save-server-name"/);
    assert.match(container.innerHTML, /value="Synthetic Restream"/);
    assert.match(container.innerHTML, /id="settings-route-summary"/);

    assert.match(container.innerHTML, /id="settings-account-actions-toggle"/);
    assert.match(container.innerHTML, /id="settings-logout-btn"/);
  },
);

test(
  "settings uses the effective server name when config stores a blank name",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const nameInput = appendRoot(document, "input", "settings-server-name");
    const summary = appendRoot(document, "p", "settings-route-summary");

    const settings = await loadCompiledFrontendModule("features/settings.js");
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = {
      backendPolicy: {},
      ingestHost: "",
      ingestSecurity: {},
      recordingSettings: {},
      serverName: "",
      srtIngest: {},
      transcodeProfiles: {},
    };

    await settings.loadSettings({ embedded: true });

    assert.equal(nameInput.value, "Restream");
    assert.match(summary.textContent, /Restream settings/);
  },
);
