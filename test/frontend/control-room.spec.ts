import { readFile } from "node:fs/promises";

import { expect, test } from "@playwright/test";

// The control room against a running engine: Fleet follows the runtime state
// stream, so a change made through the API shows without a reload.

test("fleet shows a no-publisher cause live and drops it when the output stops", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-fleet-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crfleet_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs`,
      {
        data: {
          name: "cr-out",
          url: "rtmp://127.0.0.1:9/live/cr",
          config: { video: { mode: "source" }, audio: { mode: "all" } },
        },
      },
    );
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    const start = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`,
    );
    expect(start.ok()).toBe(true);

    // An unknown workspace opens Fleet.
    await page.goto("/control-room.html?w=unknown");
    await expect(
      page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
        name: "Fleet",
      }),
    ).toHaveAttribute("aria-current", "page");

    // No publisher: one upstream cause for the pipeline's wanted output.
    const attention = page.getByRole("region", { name: /Needs attention/ });
    const item = attention.getByRole("listitem").filter({ hasText: name });
    await expect(item).toContainText("ingest: no publisher connected");
    await expect(item).toContainText("1 output: cr-out");
    await expect(item.getByRole("link", { name })).toHaveAttribute(
      "href",
      new RegExp(`[?&]w=operate&p=${pipelineId}$`),
    );
    await expect(page.getByRole("status")).toContainText("live · rev");

    const stop = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/stop`,
    );
    expect(stop.ok()).toBe(true);
    // A stopped output is not wanted: the cause is gone, with no reload.
    await expect(item).toHaveCount(0);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("operate: find a pipeline, start and stop an output, promote an input", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-operate-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `croperate_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs`,
      {
        data: {
          name: "cr-out",
          url: "srt://127.0.0.1:9?streamid=cr",
          config: { video: { mode: "source" }, audio: { mode: "all" } },
        },
      },
    );
    expect(output.ok()).toBe(true);
    const backup = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/inputs`,
      { data: { label: "Backup" } },
    );
    expect(backup.ok()).toBe(true);

    // The inventory finds it by name, in any case.
    await page.goto("/control-room.html?w=pipelines");
    await page.getByRole("searchbox", { name: "Search pipelines" }).fill(name.toUpperCase());
    const rows = page.getByRole("row").filter({ hasText: pipelineId });
    await expect(rows).toHaveCount(1);
    await expect(rows).toContainText("no input");
    await expect(rows).toContainText("SRT 1");
    await rows.getByRole("link", { name }).click();
    await expect(page).toHaveURL(new RegExp(`w=operate&p=${pipelineId}`));
    await expect(page.getByRole("heading", { name })).toBeVisible();

    // The media graph and the matrix come from the engine; the stopped
    // output is in neither delivering count.
    await expect(
      page.getByRole("listitem", { name: "Destinations srt" }),
    ).toContainText("0 / 1 delivering");
    const matrixRow = page.getByRole("row").filter({ hasText: "cr-out" });
    await expect(matrixRow).toContainText("stopped");

    // Start from the matrix: desired follows over the state stream.
    await matrixRow.getByRole("button", { name: "Start cr-out" }).click();
    await expect(matrixRow.getByRole("cell").nth(2)).toHaveText("running");
    await expect(matrixRow).toContainText("no publisher connected");
    await matrixRow.getByRole("button", { name: "Stop cr-out" }).click();
    await expect(matrixRow.getByRole("cell").nth(2)).toHaveText("stopped");

    // Promote the backup: the deck shows it selected once the API answers.
    const deck = page.getByRole("region", { name: /Source deck/ });
    await deck.getByRole("button", { name: "Promote Backup" }).click();
    await expect(
      deck.getByRole("listitem", { name: "Input Backup" }),
    ).toContainText("selected · offline");
    await expect(
      deck.getByRole("button", { name: "Promote Primary" }),
    ).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("inspect and monitor: one pipeline's anatomy and signals on one clock", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-inspect-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crinspect_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs`,
      {
        data: {
          name: "cr-out",
          url: "rtmp://127.0.0.1:9/live/cr",
          config: { video: { mode: "source" }, audio: { mode: "all" } },
        },
      },
    );
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;

    // The nav keeps the pipeline when the workspace changes.
    await page.goto(`/control-room.html?w=operate&p=${pipelineId}`);
    await page
      .getByRole("navigation", { name: "Workspaces" })
      .getByRole("link", { name: "Inspect" })
      .click();
    await expect(page).toHaveURL(new RegExp(`w=inspect&p=${pipelineId}`));
    await expect(page.getByRole("heading", { name })).toBeVisible();

    const map = page.getByRole("region", { name: /Resource map/ });
    await expect(
      map.getByRole("row").filter({ hasText: "restream" }),
    ).toContainText("measured");
    await expect(
      page.getByRole("region", { name: "How sure the numbers are" }),
    ).toContainText("process rss");
    // No publisher: no source ring, said rather than drawn as empty.
    await expect(
      page.getByRole("region", { name: "Source ring" }),
    ).toContainText("No source ring runs.");

    await page
      .getByRole("navigation", { name: "Workspaces" })
      .getByRole("link", { name: "Monitor" })
      .click();
    const signals = page.getByRole("region", { name: /Correlated signals/ });
    // Nothing wanted yet: no delivery figure rather than 0% or 100%.
    const delivering = signals.getByLabel("Wanted outputs delivering now");
    await expect(delivering).toHaveText("--");

    const start = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`,
    );
    expect(start.ok()).toBe(true);
    // Wanted but without a publisher: none of it delivers.
    await expect(delivering).toHaveText("0%");
    await expect(signals).toContainText(/[2-9] samples|\d\d+ samples/);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("incidents and capacity: an alert's evidence packet and an engine-made projection", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-incident-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crincident_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    // Wanted output, no publisher: the engine raises "No active publisher".
    const output = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs`,
      {
        data: {
          name: "cr-out",
          url: "rtmp://127.0.0.1:9/live/cr",
          config: { video: { mode: "source" }, audio: { mode: "all" } },
        },
      },
    );
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    expect(
      (await page.request.post(
        `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`,
      )).ok(),
    ).toBe(true);

    await page.goto("/control-room.html?w=incidents");
    const queue = page.getByRole("region", { name: /^Open/ });
    const incident = queue.getByRole("button").filter({ hasText: `pipeline ${name}` }).first();
    await expect(incident).toBeVisible();
    await incident.click();
    const packet = page.getByRole("region", { name: "Evidence packet" });
    await expect(packet).toContainText(`pipeline ${name}`);
    await expect(packet.getByText("Operator action")).toBeVisible();

    await page
      .getByRole("navigation", { name: "Workspaces" })
      .getByRole("link", { name: "Capacity" })
      .click();
    await expect(page.getByRole("heading", { name: /Flow Doctor/ })).toBeVisible();
    const scenario = page.getByRole("region", { name: /Scenario/ });
    await scenario.getByLabel("More outputs").fill("5000");
    await scenario.getByRole("button", { name: "Project" }).click();
    const result = scenario.getByLabel("Scenario result");
    await expect(result).toContainText("With 5000 more outputs and 0 more stages");
    await expect(result).toContainText("Projected hottest center");
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("media and system: probe, rename and delete an upload; read the host and its owners", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const stamp = Date.now();
  const uploaded = `cr-media-${stamp}.mp4`;
  const renamed = `cr-renamed-${stamp}.mp4`;
  await page.goto("/control-room.html?w=media");
  try {
    // A checked-in file whose keyframes are 5 s apart: wider than the live
    // GOP target, so it can only feed a pipeline through a re-encode.
    await page.getByLabel("Media file to upload").setInputFiles({
      name: uploaded,
      mimeType: "video/mp4",
      buffer: await readFile("test/fixtures/media-library/sparse-gop-5s.mp4"),
    });
    const sources = page.getByRole("list", { name: "Source files" });
    await sources.getByRole("button", { name: new RegExp(uploaded) }).click();
    const readiness = page.getByLabel("Ingest readiness");
    await expect(readiness).toContainText("needs Live Optimized");
    await expect(readiness).toContainText("5.00 s");

    await page.getByLabel("New file name").fill(renamed);
    await page.getByRole("button", { name: "Rename" }).click();
    await expect(page.getByRole("heading", { name: renamed })).toBeVisible();

    await page.getByRole("button", { name: `Delete ${renamed}` }).click();
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(sources.getByRole("button", { name: new RegExp(renamed) })).toHaveCount(0);
  } finally {
    for (const name of [uploaded, renamed]) {
      await page.request.delete(`/api/v1/media/${encodeURIComponent(name)}`);
    }
  }

  await page
    .getByRole("navigation", { name: "Workspaces" })
    .getByRole("link", { name: "System" })
    .click();
  await expect(page.getByRole("list", { name: "Host checks" })).toContainText(
    "Open file descriptors",
  );
  // The demo runs one owner per ingress listener; the page shows what the
  // engine reports, not what it was configured with.
  const topology = page.getByRole("region", { name: /Execution topology/ });
  await expect(topology.getByText("RTMP ingress").locator("..")).toContainText("1");
  await expect(topology.getByText("SRT ingress").locator("..")).toContainText("1");
  await expect(page.getByRole("region", { name: /SRT ingress owners/ })).toContainText(
    "No dropped or late work since the engine started",
  );
  await expect(page.getByRole("region", { name: "Build and provenance" })).toContainText(
    "Commit",
  );
});

test("settings: save a name and a profile, refuse a short passphrase, forget a failed client", async ({
  page,
}) => {
  // One failed sign-in from this client, so there is an attempt to forget.
  const failed = await page.request.post("/api/v1/auth/login", {
    data: { password: "not-the-password" },
  });
  expect(failed.status()).toBe(401);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const before = await (await page.request.get("/api/v1/settings")).json();
  const name = `cr-${Date.now()}`;

  await page.goto("/control-room.html?w=settings");
  try {
    const general = page.getByRole("region", { name: "General" });
    await general.getByLabel("Server name").fill(name);
    await general.getByRole("button", { name: "Save" }).click();
    await expect(general.getByRole("status")).toHaveText("saved");

    const profiles = page.getByRole("region", { name: "Transcode profiles" });
    await profiles.getByRole("button", { name: "Add profile" }).click();
    await profiles.getByRole("button", { name: "Save" }).click();
    await expect(profiles.getByRole("status")).toHaveText("saved");

    const stored = await (await page.request.get("/api/v1/settings")).json();
    expect(stored.serverName).toBe(name);
    expect(Object.keys(stored.transcodeProfiles)).toContain("new_profile");

    const srt = page.getByRole("region", { name: "SRT ingest" });
    await srt.getByLabel("Encryption").selectOption("encrypted");
    await srt.getByLabel(/Passphrase/).fill("short");
    await expect(srt.getByRole("button", { name: "Save" })).toBeDisabled();
    await expect(srt).toContainText("10 to 79 bytes");

    // The page's password fields reach the engine: a wrong current password
    // is judged as wrong (not as an empty new one), and nothing changes.
    const password = page.getByRole("region", { name: "Dashboard password" });
    await password.getByLabel("Current password").fill("not-the-password");
    await password.getByLabel("New password", { exact: true }).fill("a-long-new-password");
    await password.getByLabel("New password again").fill("a-long-new-password");
    await password.getByRole("button", { name: "Save" }).click();
    await expect(page.locator("#error-alert")).toContainText("Current password is incorrect");

    // The engine accepts the reset the page sends (its own scope key).
    const attempts = page.getByRole("table", { name: "Failed attempts" });
    const forget = attempts.getByRole("button", { name: /Forget .* for Dashboard login/ });
    await expect(forget.first()).toBeVisible();
    await forget.first().click();
    await expect(
      page.getByRole("region", { name: "Failed attempts" }),
    ).toContainText("No client has failed recently");
  } finally {
    await page.request.patch("/api/v1/settings", {
      data: { serverName: before.serverName, transcodeProfiles: before.transcodeProfiles },
    });
  }
});

test("changes: the engine refuses an incomplete change, then a destination is added through approve, apply and verify", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const name = `cr-changes-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crchanges_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  await page.goto("/control-room.html?w=changes");
  try {
    const define = page.getByRole("region", { name: "Define the change" });
    const stages = page.getByRole("list", { name: "Change progress" }).getByRole("listitem");
    await define.getByLabel("Why (kept in the audit log)").fill("add a DR destination");
    await define.getByLabel("Pipeline").selectOption({ label: name });

    // An add without a name or URL: the engine says why, nothing is created.
    await define.getByRole("button", { name: "Preview" }).click();
    const judgement = page.getByRole("region", { name: "Engine judgement" });
    await expect(judgement).toContainText("addOutput requires a non-empty name.");
    await expect(stages.nth(1)).toHaveAttribute("data-state", "failed");
    await expect(page.getByRole("button", { name: "Create operation" })).toHaveCount(0);

    await define.getByLabel("Name").fill("cr-dr");
    await define.getByLabel("URL").fill("rtmp://127.0.0.1:9/live/crdr");
    // The earlier judgement no longer speaks for the edited change.
    await expect(judgement).toHaveCount(0);
    await define.getByRole("button", { name: "Preview" }).click();
    await expect(judgement.getByText("valid", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Create operation" }).click();

    const operation = page.getByRole("region", { name: "Operation" });
    const status = operation.locator("[data-status]");
    await expect(status).toHaveText("awaitingApproval");
    await operation.getByLabel("Approval reason (audited)").fill("drill");
    await operation.getByRole("button", { name: "Approve" }).click();
    await expect(status).toHaveText("approved");
    await operation.getByRole("button", { name: "Apply" }).click();
    await expect(status).toHaveText("applied");
    await operation.getByRole("button", { name: "Verify" }).click();
    await expect(status).toHaveText("verified");
    await expect(operation.getByRole("table", { name: "Verification" })).toContainText("passed");
    await expect(stages.nth(3)).toHaveAttribute("data-state", "done");

    // The engine stored the destination on this pipeline.
    const settings = await (await page.request.get("/api/v1/settings")).json();
    expect(
      settings.outputs.some(
        (output: { name: string; pipelineId: string }) =>
          output.name === "cr-dr" && output.pipelineId === pipelineId,
      ),
    ).toBe(true);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("changes: an operation survives a reload, and a verification that failed can run again", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const name = `cr-changes-start-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crchstart_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: "cr-start",
        url: "rtmp://127.0.0.1:9/live/crstart",
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      },
    });
    expect(output.ok()).toBe(true);

    await page.goto("/control-room.html?w=changes");
    const define = page.getByRole("region", { name: "Define the change" });
    await define.getByLabel("Why (kept in the audit log)").fill("start the backup");
    await define.getByLabel("Pipeline").selectOption({ label: name });
    await define.getByRole("combobox", { name: /^Change\b/ }).selectOption("startOutput");
    await define.getByRole("combobox", { name: /^Destination\b/ }).selectOption({ label: "cr-start" });
    await define.getByRole("button", { name: "Preview" }).click();
    await page.getByRole("button", { name: "Create operation" }).click();
    const operation = page.getByRole("region", { name: "Operation" });
    const status = operation.locator("[data-status]");
    await operation.getByRole("button", { name: "Approve" }).click();
    await expect(status).toHaveText("approved");

    // Leaving the page does not strand the approved operation.
    await page.reload();
    await expect(status).toHaveText("approved");
    await expect(operation).toContainText("start the backup");
    await operation.getByRole("button", { name: "Apply" }).click();
    await expect(status).toHaveText("applied");

    // No publisher: the output cannot be running yet, and the page says so
    // and offers the check again instead of ending the operation.
    await operation.getByRole("button", { name: "Verify", exact: true }).click();
    await expect(status).toHaveText("verificationFailed");
    await expect(operation.getByRole("table", { name: "Verification" })).toContainText("pendingInput");
    await expect(operation.getByRole("button", { name: "Verify again" })).toBeVisible();

    await operation.getByRole("button", { name: "Start a new change" }).click();
    await page.reload();
    await expect(page.getByRole("region", { name: "Define the change" })).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("changes: an apply the engine refuses shows the failed operation and its reason", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const name = `cr-changes-fail-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crchfail_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: "cr-gone",
        url: "rtmp://127.0.0.1:9/live/crgone",
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      },
    });
    const outputId: string = (await output.json()).output.id;

    await page.goto("/control-room.html?w=changes");
    const define = page.getByRole("region", { name: "Define the change" });
    await define.getByLabel("Why (kept in the audit log)").fill("retire the old CDN");
    await define.getByLabel("Pipeline").selectOption({ label: name });
    await define.getByRole("combobox", { name: /^Change\b/ }).selectOption("stopOutput");
    await define.getByRole("combobox", { name: /^Destination\b/ }).selectOption({ label: "cr-gone" });
    await define.getByRole("button", { name: "Preview" }).click();
    await page.getByRole("button", { name: "Create operation" }).click();
    const operation = page.getByRole("region", { name: "Operation" });
    const status = operation.locator("[data-status]");
    await operation.getByRole("button", { name: "Approve" }).click();
    await expect(status).toHaveText("approved");

    // The destination goes away between approval and apply.
    await page.request.delete(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}`);
    await operation.getByRole("button", { name: "Apply" }).click();
    await expect(status).toHaveText("failed");
    await expect(operation.getByRole("alert")).toContainText("does not exist");
    await expect(operation.getByRole("button", { name: "Apply" })).toHaveCount(0);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("manage: create a pipeline, edit its inputs, destinations, source and key, record, then delete it", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const name = `cr-manage-${stamp}`;
  const clip = `cr-manage-${stamp}.mp4`;
  const upload = await page.request.post("/api/v1/media/upload", {
    multipart: {
      file: {
        name: clip,
        mimeType: "video/mp4",
        buffer: await readFile("test/fixtures/media-library/sparse-gop-5s.mp4"),
      },
    },
  });
  expect(upload.ok()).toBe(true);
  const stored = async (id: string) =>
    (await (await page.request.get("/api/v1/settings")).json()).pipelines.find(
      (pipeline: { id: string }) => pipeline.id === id,
    );
  let pipelineId = "";

  try {
    await page.goto("/control-room.html?w=pipelines");
    await page.getByRole("button", { name: "New pipeline" }).click();
    const create = page.getByRole("region", { name: "New pipeline" });
    await create.getByLabel("Name").fill(name);
    await create.getByRole("button", { name: "Save" }).click();
    // Created: the cockpit of the new pipeline opens.
    await expect(page).toHaveURL(/w=operate/);
    pipelineId = new URL(page.url()).searchParams.get("p") ?? "";
    expect((await stored(pipelineId)).name).toBe(name);

    // A backup input: added, disabled, deleted.
    await page.getByLabel("New input label").fill("cr-backup");
    await page.getByRole("button", { name: "Add input" }).click();
    await page.getByRole("button", { name: "Disable cr-backup" }).click();
    await expect(page.getByRole("button", { name: "Enable cr-backup" })).toBeVisible();
    await page.getByRole("button", { name: "Delete input cr-backup" }).click();
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(page.getByRole("listitem", { name: "Input cr-backup" })).toHaveCount(0);

    // A destination on a preset server: the engine stores the joined URL and
    // the preset's RTMP flavour; then it is renamed and deleted.
    await page.getByRole("button", { name: "Add destination" }).click();
    const add = page.getByRole("region", { name: "New destination" });
    await add.getByLabel("Name").fill("cr-yt");
    await add.getByRole("combobox", { name: /^Server\b/ }).selectOption({ label: "YouTube" });
    await add.getByLabel("Stream key").fill("abcd-efgh");
    await add.getByRole("button", { name: "Save" }).click();
    const matrix = page.getByRole("region", { name: /Destination matrix/ });
    await expect(matrix.getByRole("cell", { name: "cr-yt", exact: true })).toBeVisible();
    const settings = await (await page.request.get("/api/v1/settings")).json();
    const output = settings.outputs.find(
      (candidate: { pipelineId: string }) => candidate.pipelineId === pipelineId,
    );
    expect(output.url).toBe("rtmp://a.rtmp.youtube.com/live2/abcd-efgh");
    expect(output.config.protocol).toEqual({ type: "rtmp", mode: "enhanced" });

    await page.getByRole("button", { name: "Edit cr-yt" }).click();
    const edit = page.getByRole("region", { name: "Destination cr-yt" });
    await edit.getByLabel("Name").fill("cr-yt2");
    await edit.getByRole("button", { name: "Save" }).click();
    await expect(matrix.getByRole("cell", { name: "cr-yt2", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Delete cr-yt2" }).click();
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(matrix.getByRole("cell", { name: "cr-yt2", exact: true })).toHaveCount(0);

    // A file source: set, played, stopped, and back to a live publisher.
    const controls = page.getByRole("region", { name: "Pipeline controls" });
    await controls.getByRole("button", { name: "Settings" }).click();
    const pipelineSettings = page.getByRole("region", { name: "Pipeline settings" });
    await pipelineSettings.getByRole("combobox", { name: /^Source\b/ }).selectOption("file");
    await pipelineSettings.getByRole("combobox", { name: /^File\b/ }).selectOption(clip);
    await pipelineSettings.getByRole("button", { name: "Save" }).click();
    await expect(pipelineSettings.getByRole("status")).toHaveText("saved");
    await controls.getByRole("button", { name: `Play file ${clip}` }).click();
    await controls.getByRole("button", { name: `Stop file ${clip}` }).click();
    await expect(controls.getByRole("button", { name: `Play file ${clip}` })).toBeVisible();
    await pipelineSettings.getByRole("combobox", { name: /^Source\b/ }).selectOption("live");
    await pipelineSettings.getByRole("button", { name: "Save" }).click();
    await expect(controls.getByRole("button", { name: /file/ })).toHaveCount(0);
    expect((await stored(pipelineId)).inputSource).toBeNull();

    // A new stream key, once.
    const oldKey = (await stored(pipelineId)).streamKey;
    await pipelineSettings.getByLabel(/Issue a new stream key/).check();
    await pipelineSettings.getByRole("button", { name: "Save" }).click();
    await expect(pipelineSettings.getByLabel(/Issue a new stream key/)).not.toBeChecked();
    const newKey = (await stored(pipelineId)).streamKey;
    expect(newKey).not.toBe(oldKey);
    expect(newKey).toBeTruthy();

    // Recording armed and disarmed, as the engine stores it. `active` is not
    // asserted: the file publisher stopped above may still be leaving.
    const recordingEnabled = async () =>
      (await (await page.request.get(`/api/v1/pipelines/${pipelineId}/summary`)).json()).recording
        .enabled;
    await controls.getByRole("button", { name: "Record", exact: true }).click();
    await expect(controls.getByRole("button", { name: "Stop recording", exact: true })).toBeVisible();
    expect(await recordingEnabled()).toBe(true);
    await controls.getByRole("button", { name: "Stop recording", exact: true }).click();
    await expect(controls.getByRole("button", { name: "Record", exact: true })).toBeVisible();
    expect(await recordingEnabled()).toBe(false);

    await controls.getByRole("button", { name: `Delete pipeline ${name}` }).click();
    await controls.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(page).toHaveURL(/w=pipelines/);
    expect(await stored(pipelineId)).toBeUndefined();
    pipelineId = "";
  } finally {
    if (pipelineId) await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
    await page.request.delete(`/api/v1/media/${encodeURIComponent(clip)}`);
  }
});

test("logs: a pipeline's history, then its rows live; paused, nothing arrives; engine-only rows name no pipeline", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const settings = await (await page.request.get("/api/v1/settings")).json();
  const main = settings.pipelines.find((pipeline: { name: string }) => pipeline.name === "Main");
  const output = settings.outputs.find((candidate: { name: string }) => candidate.name === "sink-a");
  const outputPath = `/api/v1/pipelines/${main.id}/outputs/${output.id}`;
  const writtenStarts = async () =>
    (
      await (await page.request.get(`/api/v1/logs?output_id=${output.id}&limit=1000`)).json()
    ).logs.filter((row: { message: string }) => row.message === "output job started").length;

  try {
    await page.goto(`/control-room.html?w=logs&p=${main.id}`);
    await expect(page.getByRole("combobox", { name: /^Scope\b/ })).toHaveValue(`p:${main.id}`);
    const rows = page.getByRole("table", { name: "Log rows" }).locator("tbody tr");
    const live = page.getByRole("region", { name: "Logs" }).getByRole("status");
    await expect(live).toHaveText("live");
    // The history holds the pipeline's lifecycle; a stop written after the
    // page loaded arrives without a reload, named by its destination.
    await expect(rows.filter({ hasText: "output started" }).first()).toBeVisible();
    const stopped = rows.filter({ hasText: "output job stopped" });
    const stoppedBefore = await stopped.count();
    expect((await page.request.post(`${outputPath}/stop`)).ok()).toBe(true);
    await expect(stopped).toHaveCount(stoppedBefore + 1);
    await expect(stopped.first()).toContainText("sink-a");

    // Paused: the engine writes the start, the page does not show it.
    await page.getByRole("button", { name: "Pause live" }).click();
    await expect(live).toHaveText("paused");
    const started = rows.filter({ hasText: "output job started" });
    const startedShown = await started.count();
    const startsWritten = await writtenStarts();
    expect((await page.request.post(`${outputPath}/start`)).ok()).toBe(true);
    await expect.poll(writtenStarts).toBe(startsWritten + 1);
    await page.waitForTimeout(1000);
    await expect(started).toHaveCount(startedShown);
    // Resumed: the history read again includes it.
    await page.getByRole("button", { name: "Resume live" }).click();
    await expect(live).toHaveText("live");
    await expect(started).toHaveCount(startedShown + 1);

    await page.getByRole("combobox", { name: /^Scope\b/ }).selectOption("engine");
    await expect(rows.first()).toBeVisible();
    const places = await rows.locator("td:nth-child(3)").allTextContents();
    expect(new Set(places)).toEqual(new Set(["engine"]));
  } finally {
    await page.request.post(`${outputPath}/start`);
  }
});

test("diagnostics: run the engine's checks on a live pipeline and download the report", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const settings = await (await page.request.get("/api/v1/settings")).json();
  const main = settings.pipelines.find((pipeline: { name: string }) => pipeline.name === "Main");

  await page.goto(`/control-room.html?w=inspect&p=${main.id}`);
  await page.getByRole("button", { name: "Run diagnostics" }).click();
  const verdict = page
    .getByRole("region", { name: "Diagnostics" })
    .getByRole("status")
    .filter({ hasText: /checks/ });
  await expect(verdict).toBeVisible({ timeout: 90_000 });
  const checks = page.getByRole("list", { name: "Diagnostic checks" }).locator(":scope > li");
  const shown = await checks.count();
  expect(shown).toBeGreaterThan(0);
  await expect(verdict).toContainText(`${shown} checks`);

  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download report" }).click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(new RegExp(`^diagnostic-Main-${main.id}-[0-9_-]+\\.json$`));
  const report = JSON.parse(await readFile(await file.path(), "utf8"));
  expect(report.schema).toBe("restream.diagnostics.v1");
  expect(report.pipeline).toEqual({ id: main.id, name: "Main" });
  expect(report.results).toHaveLength(shown);
});
