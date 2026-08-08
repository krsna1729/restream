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

// Regression coverage for real, previously-shipped bugs in
// web/ts/features/settings/security.ts:
// - populateIngestSecuritySettings/saveIngestSecurity read/wrote ids
//   (settings-ingest-auth-mode/settings-ingest-static-key) and a mode/staticKey
//   shape that don't exist on the backend IngestSecurityConfig model.
// - saveDashboardPassword read ids (settings-current-password/settings-new-password/
//   settings-confirm-password) that don't exist in the rendered markup, so
//   `newPassword` was always empty and every attempt failed with "New password
//   cannot be empty" before a request was ever sent.
// - saveDashboardPassword's and syncDashboardPasswordPrompt's saved-feedback/prompt
//   ids (password-changed-saved, dashboard-password-change-prompt) didn't match the
//   real ids (dashboard-password-saved, dashboard-password-prompt) either.
// - syncDashboardPasswordPrompt read a nonexistent `passwordChangeRequired` field
//   instead of the real `dashboardPasswordChangeRecommended` field.

test(
  "populateIngestSecuritySettings reads state into the real ingest-security-* ids",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const failureLimit = appendElement(
      document,
      "input",
      "ingest-security-failure-limit",
    );
    const failureWindow = appendElement(
      document,
      "input",
      "ingest-security-failure-window-ms",
    );
    const banMs = appendElement(document, "input", "ingest-security-ban-ms");
    const trackedIpLimit = appendElement(
      document,
      "input",
      "ingest-security-tracked-ip-limit",
    );

    const security = await loadCompiledFrontendModule(
      "features/settings/security.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = {
      ingestSecurity: {
        failureLimit: 5,
        failureWindowMs: 60000,
        banMs: 300000,
        trackedIpLimit: 1000,
      },
    };

    security.populateIngestSecuritySettings();

    assert.equal(failureLimit.value, "5");
    assert.equal(failureWindow.value, "60000");
    assert.equal(banMs.value, "300000");
    assert.equal(trackedIpLimit.value, "1000");
  },
);

test(
  "saveIngestSecurity sends the real IngestSecurityConfig shape, not mode/staticKey",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    appendElement(document, "input", "ingest-security-failure-limit").value =
      "10";
    appendElement(
      document,
      "input",
      "ingest-security-failure-window-ms",
    ).value = "120000";
    appendElement(document, "input", "ingest-security-ban-ms").value =
      "600000";
    appendElement(
      document,
      "input",
      "ingest-security-tracked-ip-limit",
    ).value = "2000";
    appendElement(document, "span", "ingest-security-saved").classList.add(
      "hidden",
    );

    let capturedBody = null;
    globalThis.fetch = async (url, init) => {
      assert.equal(String(url), "/api/v1/settings");
      capturedBody = JSON.parse(init.body);
      return makeResponse({
        serverName: "s",
        ingestHost: "",
        ingestSecurity: capturedBody.ingestSecurity,
        recordingSettings: {},
        srtIngest: {},
        backendPolicy: {},
      });
    };

    const security = await loadCompiledFrontendModule(
      "features/settings/security.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { ingestSecurity: {} };

    await security.saveIngestSecurity();

    assert.deepEqual(capturedBody.ingestSecurity, {
      failureLimit: 10,
      failureWindowMs: 120000,
      banMs: 600000,
      trackedIpLimit: 2000,
    });
  },
);

test(
  "saveDashboardPassword reads the real current/new/confirm-password-input ids",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const currentInput = appendElement(
      document,
      "input",
      "current-password-input",
    );
    currentInput.value = "old-secret";
    const newInput = appendElement(document, "input", "new-password-input");
    newInput.value = "new-secret-value";
    const confirmInput = appendElement(
      document,
      "input",
      "confirm-password-input",
    );
    confirmInput.value = "new-secret-value";
    appendElement(document, "span", "dashboard-password-saved").classList.add(
      "hidden",
    );
    appendElement(document, "div", "dashboard-password-prompt").classList.add(
      "hidden",
    );

    let capturedUrl = null;
    let capturedBody = null;
    globalThis.fetch = async (url, init) => {
      capturedUrl = String(url);
      capturedBody = JSON.parse(init.body);
      return makeResponse({ ok: true });
    };

    const security = await loadCompiledFrontendModule(
      "features/settings/security.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { dashboardPasswordChangeRecommended: false };

    await security.saveDashboardPassword();

    assert.equal(capturedUrl, "/api/v1/auth/change-password");
    assert.equal(capturedBody.currentPassword, "old-secret");
    assert.equal(capturedBody.newPassword, "new-secret-value");
    assert.equal(currentInput.value, "");
    assert.equal(newInput.value, "");
    assert.equal(confirmInput.value, "");
  },
);

test(
  "syncDashboardPasswordPrompt reads the real dashboard-password-prompt id and dashboardPasswordChangeRecommended field",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const promptEl = appendElement(
      document,
      "div",
      "dashboard-password-prompt",
    );
    promptEl.classList.add("hidden");

    const security = await loadCompiledFrontendModule(
      "features/settings/security.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.config = { dashboardPasswordChangeRecommended: true };

    security.syncDashboardPasswordPrompt();

    assert.equal(promptEl.classList.contains("hidden"), false);

    state.config = { dashboardPasswordChangeRecommended: false };
    security.syncDashboardPasswordPrompt();

    assert.equal(promptEl.classList.contains("hidden"), true);
  },
);
