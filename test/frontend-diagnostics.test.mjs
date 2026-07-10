import assert from "node:assert/strict";
import test from "node:test";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "./helpers/fake-dom.mjs";

function appendRoot(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function flushAsyncWork() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("diagnostics uses abortable JSON batches and rejects stale responses", async () => {
  const { document } = installFakeDom();
  const modal = appendRoot(document, "dialog", "diagnostics-modal");
  modal.showModal = () => {};
  appendRoot(document, "div", "diagnostics-title");
  appendRoot(document, "div", "diagnostics-probe-toggle");
  appendRoot(document, "div", "diagnostics-total-time");
  appendRoot(document, "div", "diagnostics-header");
  const list = appendRoot(document, "div", "diagnostics-list");
  for (const id of [
    "diagnostics-copy-all-btn",
    "diagnostics-download-btn",
    "diagnostics-ask-ai-btn",
  ]) {
    appendRoot(document, "button", id);
  }

  const pending = [];
  globalThis.fetch = (url, options = {}) => {
    const request = deferred();
    pending.push({ url: String(url), options, request });
    // Deliberately ignore abort when resolving to prove the generation guard.
    return request.promise;
  };

  const stateModule = await loadCompiledFrontendModule("core/state.js");
  stateModule.state.pipelines = [
    {
      id: "pipe-a",
      name: "Pipeline A",
      inputSource: "network",
      input: { publisher: { protocol: "rtmp" } },
    },
    {
      id: "pipe-b",
      name: "Pipeline B",
      inputSource: "network",
      input: { publisher: { protocol: "srt" } },
    },
  ];
  const diagnostics = await loadCompiledFrontendModule(
    "features/diagnostics.js",
  );

  diagnostics.openDiagnosticsModal("pipe-a");
  assert.match(list.innerHTML, /Running diagnostics/);
  assert.equal(pending[0].options.method, "POST");
  assert.equal(pending[0].options.body, undefined);

  diagnostics.openDiagnosticsModal("pipe-b");
  assert.equal(pending[0].options.signal.aborted, true);
  assert.equal(pending.length, 2);

  pending[0].request.resolve(
    new Response(
      JSON.stringify({
        protocol: "rtmp",
        totalDurationMs: 1,
        checks: [diagnosticCheck("Old response")],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    ),
  );
  await flushAsyncWork();
  assert.match(list.innerHTML, /Running diagnostics/);
  assert.doesNotMatch(list.innerHTML, /Old response/);

  pending[1].request.resolve(
    new Response(
      JSON.stringify({
        protocol: "srt",
        totalDurationMs: 17,
        checks: [diagnosticCheck("Current response")],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    ),
  );
  await flushAsyncWork();
  assert.match(list.innerHTML, /Current response/);
  assert.equal(
    document.getElementById("diagnostics-total-time").textContent,
    "17ms",
  );

  diagnostics.openDiagnosticsModal("pipe-a");
  const closeRequest = pending[2];
  modal.dispatchEvent({ type: "close" });
  assert.equal(closeRequest.options.signal.aborted, true);

  diagnostics.openDiagnosticsModal("pipe-a");
  pending[3].request.resolve(
    new Response(JSON.stringify({ error: "diagnostics failed" }), {
      status: 500,
      headers: { "content-type": "application/json" },
    }),
  );
  await flushAsyncWork();
  assert.match(list.innerHTML, /Diagnostics could not be completed/);
});

function diagnosticCheck(name) {
  return {
    index: 0,
    name,
    description: "Batch result",
    command: "native snapshot",
    stdout: "ok",
    stderr: "",
    exitCode: 0,
    durationMs: 1,
    issues: [],
  };
}
