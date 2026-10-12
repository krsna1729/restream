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

test("a new password must be given twice, the same", () => {
  assert.match(passwordProblem("", ""), /Enter/);
  assert.match(passwordProblem("secret-1", "secret-2"), /differ/);
  assert.equal(passwordProblem("secret-1", "secret-1"), null);
});

test("an SRT passphrase is 10 to 79 bytes, counted as bytes", () => {
  assert.notEqual(passphraseProblem("123456789"), null);
  assert.equal(passphraseProblem("1234567890"), null);
  assert.equal(passphraseProblem("x".repeat(79)), null);
  assert.notEqual(passphraseProblem("x".repeat(80)), null);
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
  const ok = profilesToSave([...builtIns, { name: " mine ", profile: NEW_PROFILE }]);
  assert.deepEqual(Object.keys(ok.profiles).sort(), ["1080p", "720p", "h264", "mine"]);
  assert.match(profilesToSave([...builtIns, { name: "  ", profile: NEW_PROFILE }]).problem, /name/);
  assert.match(
    profilesToSave([...builtIns, { name: "x", profile: NEW_PROFILE }, { name: "x ", profile: NEW_PROFILE }])
      .problem,
    /Two profiles/,
  );
  assert.match(profilesToSave(builtIns.slice(1)).problem, /"h264" must stay/);
});
