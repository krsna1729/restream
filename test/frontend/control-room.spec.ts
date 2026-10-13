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

    await page.getByRole("button", { name: "Delete…" }).click();
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
