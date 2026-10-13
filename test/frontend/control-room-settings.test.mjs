import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  NEW_PROFILE,
  banLabel,
  nextProfileName,
  passphraseProblem,
  passwordProblem,
  profileRows,
  profilesToSave,
  resetOne,
  scopeLabel,
} = await loadCompiledFrontendModule("control-room/settings-model.js");

test("forgetting one client sends the engine's own scope key, which it accepts", () => {
  // The engine rejects any scope it does not know ("all", "ip") with 400; the
  // classic dashboard sent those, so its resets never worked.
  const attempt = { scope: "srt-publish", ip: "10.0.0.7", failureCount: 3, banned: true };
  assert.deepEqual(resetOne(attempt), { scope: "srt-publish", ip: "10.0.0.7" });
});

test("attempts read in operator words; a ban counts down in whole seconds", () => {
  assert.equal(scopeLabel("dashboard-login"), "Dashboard login");
  assert.equal(scopeLabel("future-scope"), "future-scope");
  assert.equal(banLabel({ banned: false }), "tracking");
  assert.equal(banLabel({ banned: true, banRemainingMs: 1_001 }), "banned 2 s");
  assert.equal(banLabel({ banned: true, banRemainingMs: null }), "banned");
});

test("a password change needs the current one and a new one of 12 bytes, given twice", () => {
  assert.match(passwordProblem("", "long-enough-1", "long-enough-1"), /current/);
  assert.match(passwordProblem("admin", "x".repeat(11), "x".repeat(11)), /12 bytes/);
  assert.equal(passwordProblem("admin", "x".repeat(12), "x".repeat(12)), null);
  // Four three-byte characters are 12 bytes: long enough.
  assert.equal(passwordProblem("admin", "€€€€", "€€€€"), null);
  assert.match(passwordProblem("admin", "long-enough-1", "long-enough-2"), /differ/);
});

test("an SRT passphrase is 10 to 79 bytes once trimmed, counted as bytes", () => {
  assert.notEqual(passphraseProblem("123456789"), null);
  assert.equal(passphraseProblem("1234567890"), null);
  assert.equal(passphraseProblem("x".repeat(79)), null);
  assert.notEqual(passphraseProblem("x".repeat(80)), null);
  // The engine trims first: 12 characters with spaces are 8 bytes, too short.
  assert.notEqual(passphraseProblem("  short123  "), null);
  // Five three-byte characters are 15 bytes: long enough.
  assert.equal(passphraseProblem("€€€€€"), null);
});

test("profiles list the built-ins first, then the rest by name", () => {
  const rows = profileRows({
    zeta: NEW_PROFILE,
    "1080p": NEW_PROFILE,
    alpha: NEW_PROFILE,
    h264: NEW_PROFILE,
    "720p": NEW_PROFILE,
  });
  assert.deepEqual(
    rows.map((row) => row.name),
    ["h264", "720p", "1080p", "alpha", "zeta"],
  );
  assert.equal(nextProfileName(rows), "new_profile");
  assert.equal(
    nextProfileName([...rows, { name: "new_profile" }, { name: "new_profile_2" }]),
    "new_profile_3",
  );
});

test("profiles are saved only when every name is set, unique, and the built-ins stay", () => {
  const builtIns = ["h264", "720p", "1080p"].map((name) => ({ name, profile: NEW_PROFILE }));
  const ok = profilesToSave([...builtIns, { name: " mine ", profile: NEW_PROFILE }], [], {});
  assert.deepEqual(Object.keys(ok.profiles).sort(), ["1080p", "720p", "h264", "mine"]);
  assert.match(profilesToSave([...builtIns, { name: "  ", profile: NEW_PROFILE }], [], {}).problem, /name/);
  assert.match(
    profilesToSave(
      [...builtIns, { name: "x", profile: NEW_PROFILE }, { name: "x ", profile: NEW_PROFILE }],
      [],
      {},
    ).problem,
    /Two profiles/,
  );
  assert.match(profilesToSave(builtIns.slice(1), [], {}).problem, /"h264" must stay/);
});

test("a stored profile an output names cannot be removed; one already missing does not block", () => {
  const builtIns = ["h264", "720p", "1080p"].map((name) => ({ name, profile: NEW_PROFILE }));
  const stored = { h264: NEW_PROFILE, "720p": NEW_PROFILE, "1080p": NEW_PROFILE, mine: NEW_PROFILE };
  const output = (video) => ({ id: "o1", pipelineId: "p1", name: "CDN", url: "", config: { video, audio: { mode: "all" } } });
  const usesMine = output({ mode: "preset", preset: "mine" });
  assert.match(profilesToSave(builtIns, [usesMine], stored).problem, /Output "CDN" uses profile "mine"/);
  assert.ok("profiles" in profilesToSave([...builtIns, { name: "mine", profile: NEW_PROFILE }], [usesMine], stored));
  assert.ok("profiles" in profilesToSave(builtIns, [output({ mode: "source" })], stored));
  // The output already names a profile the engine does not have (it falls
  // back today); an unrelated edit still saves.
  const usesGone = output({ mode: "preset", preset: "gone" });
  assert.ok("profiles" in profilesToSave(builtIns, [usesGone], stored));
});
