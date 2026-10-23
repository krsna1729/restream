import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { watchUrl, embedSnippet, linkState } = await loadCompiledFrontendModule("control-room/share-model.js");

test("the watch URL keeps the dashboard's origin and proxy prefix, not its query", () => {
  assert.equal(watchUrl("https://ops.example/restream/?w=operate&p=x#y", "abc"), "https://ops.example/restream/watch/abc");
  assert.equal(watchUrl("http://127.0.0.1:3030/", "abc"), "http://127.0.0.1:3030/watch/abc");
});

test("the embed is an escaped, responsive iframe of the watch URL", () => {
  const snippet = embedSnippet("https://ops.example/watch/abc", 'Lobby "cam" <1>');
  assert.ok(snippet.startsWith('<iframe src="https://ops.example/watch/abc"'));
  assert.ok(snippet.includes('title="Lobby &quot;cam&quot; &lt;1&gt;"'));
  assert.ok(snippet.includes("allowfullscreen"));
});

test("a link is live, expiring within a day, or expired", () => {
  assert.equal(linkState({ expiresAtMs: null }, 5), "live");
  assert.equal(linkState({ expiresAtMs: 10 }, 10), "expired");
  assert.equal(linkState({ expiresAtMs: 3_600_000 }, 0), "expiring");
  assert.equal(linkState({ expiresAtMs: 3 * 86_400_000 }, 0), "live");
});
