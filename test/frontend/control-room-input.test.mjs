import assert from "node:assert/strict";
import test from "node:test";

import { installFakeDom, loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  AUTH_KINDS,
  AUTH_NONE_NOTE,
  authFields,
  chooseAuthKind,
  inputDraft,
  inputFields,
  inputProblem,
  inputSourceLines,
  inputWrite,
  replaceAuth,
  storedAuthNote,
} = await loadCompiledFrontendModule("control-room/input-model.js");

const RTMP = "rtmp://host:1935/live/key-a";
const SRT = "srt://host:8890?streamid=publish:key-a";
const ORIGIN = "https://origin.example/live/master.m3u8";

function input(overrides = {}) {
  return {
    id: "in-1",
    pipelineId: "pipe-1",
    label: "Encoder A",
    streamKey: "key-a",
    role: "primary",
    enabled: true,
    selected: true,
    kind: "keyed",
    sourceUrl: null,
    variantRule: "highestBandwidth",
    authKind: "none",
    authUsername: null,
    ingestUrls: { rtmp: RTMP, srt: SRT },
    previewUrl: "/hls/inputs/in-1/master.m3u8",
    runtime: {
      connected: false,
      forwardingState: null,
      protocol: null,
      uptimeSeconds: null,
      bytesReceived: 0,
      remoteAddr: null,
      video: null,
      audio: null,
      quality: null,
    },
    ...overrides,
  };
}

function pull(overrides = {}) {
  return input({
    kind: "hlsPull",
    sourceUrl: ORIGIN,
    streamKey: "",
    ingestUrls: null,
    ...overrides,
  });
}

test("the Source choice decides which pull fields the editor shows", () => {
  const keyed = inputDraft(null);
  assert.equal(keyed.kind, "keyed");
  assert.deepEqual(inputFields(keyed), []);

  const pulling = { ...keyed, kind: "hlsPull" };
  assert.deepEqual(inputFields(pulling), ["sourceUrl", "variantRule", "auth"]);

  // Switching back hides them again: a keyed input waits for a publisher.
  assert.deepEqual(inputFields({ ...pulling, kind: "keyed" }), []);
});

test("each authentication choice reveals exactly its own fields", () => {
  assert.deepEqual(AUTH_KINDS, ["none", "bearer", "basic", "headers"]);
  const pulling = { ...inputDraft(null), kind: "hlsPull" };
  const shown = {
    none: [],
    bearer: ["token"],
    basic: ["username", "password"],
    headers: ["headers"],
  };
  for (const [kind, fields] of Object.entries(shown)) {
    assert.deepEqual(authFields(chooseAuthKind(pulling, kind)), fields, kind);
  }
  // A keyed input is never asked for a credential.
  assert.deepEqual(authFields({ ...pulling, kind: "keyed" }), []);
  assert.match(AUTH_NONE_NOTE, /signed URL/);
  assert.match(AUTH_NONE_NOTE, /cookies/);
});

test("a stored credential is state, not a value", () => {
  const bearer = pull({ authKind: "bearer" });
  const draft = inputDraft(bearer);
  assert.equal(draft.keepAuth, true);
  assert.equal(draft.token, "");
  assert.equal(draft.username, "");
  assert.equal(draft.password, "");
  assert.equal(storedAuthNote(bearer), "Token stored");
  // Keeping it shows no secret field at all.
  assert.deepEqual(authFields(draft), []);

  assert.equal(storedAuthNote(pull({ authKind: "basic", authUsername: "alice" })), "Signed in as alice");
  assert.equal(storedAuthNote(pull({ authKind: "headers" })), "Custom headers stored");
  assert.equal(storedAuthNote(input()), null);
});

test("saving a stored credential untouched sends no auth at all", async () => {
  const stored = pull({ authKind: "bearer" });
  const body = inputWrite(inputDraft(stored), stored);
  assert.ok(!("auth" in body));
  assert.deepEqual(body, {
    label: "Encoder A",
    kind: "hlsPull",
    sourceUrl: ORIGIN,
    variantRule: "highestBandwidth",
  });

  // The body the API client actually puts on the wire: the regression that
  // would silently wipe a customer's token.
  installFakeDom();
  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    requests.push({
      url: String(url),
      method: options.method || "GET",
      body: options.body ? JSON.parse(options.body) : null,
    });
    return new Response(JSON.stringify({ input: stored }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };
  const api = await loadCompiledFrontendModule("core/api.js");
  await api.updatePipelineInput("pipe-1", stored.id, body);
  assert.equal(requests[0].method, "PATCH");
  assert.ok(!("auth" in requests[0].body));

  // The source becoming keyed drops the credential it can no longer carry:
  // the engine refuses a keyed source that still has one.
  assert.deepEqual(inputWrite({ ...inputDraft(stored), kind: "keyed" }, stored), {
    label: "Encoder A",
    kind: "keyed",
    sourceUrl: null,
    variantRule: "highestBandwidth",
    auth: { kind: "none" },
  });
  // A keyed input that never had a credential still sends none.
  assert.deepEqual(inputWrite(inputDraft(input()), input()), {
    label: "Encoder A",
    kind: "keyed",
    sourceUrl: null,
    variantRule: "highestBandwidth",
  });

  // Clearing sends `{"kind":"none"}`; replacing sends the new secret.
  assert.deepEqual(inputWrite(chooseAuthKind(inputDraft(stored), "none"), stored).auth, {
    kind: "none",
  });
  const replacing = { ...replaceAuth(inputDraft(stored)), token: "new-token" };
  assert.deepEqual(inputWrite(replacing, stored).auth, { kind: "bearer", token: "new-token" });
  // A blank replacement is refused rather than sent as an empty secret.
  assert.equal(inputProblem(replaceAuth(inputDraft(stored))), "A bearer token needs a token.");

  // A create has nothing stored to keep: it sends its credential.
  const fresh = { ...inputDraft(null), kind: "hlsPull", label: "Origin", sourceUrl: ORIGIN };
  assert.deepEqual(
    inputWrite({ ...chooseAuthKind(fresh, "basic"), username: "alice", password: "hunter2" }, null).auth,
    { kind: "basic", username: "alice", password: "hunter2" },
  );
  assert.deepEqual(
    inputWrite(
      {
        ...chooseAuthKind(fresh, "headers"),
        headers: [
          { name: " X-Token ", value: "s3cret" },
          { name: "", value: "ignored" },
        ],
      },
      null,
    ).auth,
    { kind: "headers", headers: [{ name: "X-Token", value: "s3cret" }] },
  );
});

test("a pull input's row names its URL and no ingest URLs", () => {
  const stored = pull({ variantRule: "lowestBandwidth" });
  assert.deepEqual(inputSourceLines(stored), [ORIGIN]);
  // Even if ingest URLs ever appeared, a pull input accepts no pushes.
  assert.deepEqual(inputSourceLines({ ...stored, ingestUrls: { rtmp: RTMP, srt: SRT } }), [
    ORIGIN,
  ]);

  const draft = inputDraft(stored);
  assert.equal(draft.sourceUrl, ORIGIN);
  assert.equal(draft.variantRule, "lowestBandwidth");
  assert.equal(draft.label, "Encoder A");
});

test("a keyed input keeps today's behaviour", () => {
  const keyed = input();
  const draft = inputDraft(keyed);
  assert.equal(draft.kind, "keyed");
  assert.equal(draft.authKind, "none");
  assert.deepEqual(inputFields(draft), []);
  // A keyed primary lists nothing; a backup lists its publish URLs.
  assert.deepEqual(inputSourceLines(keyed), []);
  assert.deepEqual(inputSourceLines(input({ id: "in-2", role: "backup", label: "Backup" })), [
    RTMP,
    SRT,
  ]);

  const body = inputWrite({ ...draft, label: " Encoder A " }, keyed);
  assert.deepEqual(body, {
    label: "Encoder A",
    kind: "keyed",
    sourceUrl: null,
    variantRule: "highestBandwidth",
  });
  assert.ok(!("auth" in body));
});

test("what the engine would refuse is caught before submit", () => {
  const origin = { ...inputDraft(null), label: "Origin", kind: "hlsPull" };
  assert.equal(inputProblem(origin), "A pull input needs the HLS source URL.");
  assert.match(
    inputProblem({ ...origin, sourceUrl: "rtmp://origin.example/master.m3u8" }),
    /absolute http or https URL/,
  );
  assert.match(inputProblem({ ...origin, sourceUrl: "origin.example/master.m3u8" }), /absolute http or https URL/);
  assert.equal(inputProblem({ ...origin, sourceUrl: ORIGIN }), null);
  assert.equal(inputProblem({ ...origin, sourceUrl: ORIGIN, authKind: "bearer" }), "A bearer token needs a token.");
  assert.equal(
    inputProblem({ ...origin, sourceUrl: ORIGIN, authKind: "basic", password: "p" }),
    "Basic auth needs a username.",
  );
  assert.equal(
    inputProblem({ ...origin, sourceUrl: ORIGIN, authKind: "headers", headers: [] }),
    "Add a header, or choose None.",
  );
  assert.equal(
    inputProblem({
      ...origin,
      sourceUrl: ORIGIN,
      authKind: "headers",
      headers: [{ name: "X Token", value: "v" }],
    }),
    '"X Token" is not a valid header name.',
  );
  // A keyed input needs neither URL nor credential, but still a label.
  assert.equal(inputProblem({ ...origin, kind: "keyed", sourceUrl: "" }), null);
  assert.equal(inputProblem({ ...origin, sourceUrl: ORIGIN, label: "   " }), "An input needs a label.");
});
