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
  "renderDashboardV2MediaBody owns the media route shell",
  { concurrency: false },
  async () => {
    const { document } = installFakeDom();
    const container = appendRoot(
      document,
      "div",
      "dashboard-v2-media-content",
    );

    globalThis.fetch = async (url) => {
      const href = String(url);
      if (href === "/api/v1/media") {
        return new Response(JSON.stringify({ files: [] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      throw new Error(`Unexpected fetch: ${href}`);
    };

    const mediaLibrary = await loadCompiledFrontendModule("features/media-library.js");
    const result = mediaLibrary.renderDashboardV2MediaBody(container, {
      routeChanged: true,
    });
    assert.equal(result.needsDashboardRuntimeRefresh, true);
    await result.rendered;

    assert.equal(container.dataset.mediaRouteBody, "v2");
    assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
    assert.match(container.innerHTML, /id="media-library-root"/);
    assert.match(container.innerHTML, /aria-label="Search media library"/);
    assert.match(container.innerHTML, /aria-label="Upload media file"/);
    assert.doesNotMatch(container.innerHTML, /<h1[^>]*>Media Library<\/h1>/);
  },
);
