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
  "renderDashboardV2StatusBody owns the status route shell",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const container = appendRoot(
      document,
      "div",
      "dashboard-v2-status-content",
    );
    const status = await loadCompiledFrontendModule("features/status.js");
    const rendered = status.renderDashboardV2StatusBody(container);
    assert.equal(typeof rendered?.then, "function");
    await rendered;

    assert.equal(container.dataset.statusRouteBody, "v2");
    assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
    assert.match(container.innerHTML, /id="status-versions"/);
    assert.match(container.innerHTML, /aria-label="Refresh status data"/);
  },
);
