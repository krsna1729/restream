import { readFile } from "node:fs/promises";

import { expect, test, type Page } from "@playwright/test";

// The control room against a running engine: Fleet follows the runtime state
// stream, so a change made through the API shows without a reload.

test("a stale deep path still opens the workspace in the query", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  // The engine serves the page for unknown paths, and for /index.html: the
  // place comes from the query whatever the path is.
  await page.goto("/index.html?w=incidents");
  await expect(
    page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
      name: "Incidents",
    }),
  ).toHaveAttribute("aria-current", "page");

  // One segment: the page's own asset URLs are relative and resolve at the
  // root, so the shell is served in place.
  await page.goto("/old-path?w=media");
  await expect(
    page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
      name: "Media",
    }),
  ).toHaveAttribute("aria-current", "page");

  // A link from such a path canonicalises to the root.
  await page
    .getByRole("navigation", { name: "Workspaces" })
    .getByRole("link", { name: "Fleet" })
    .click();
  await expect(page).toHaveURL(/\/\?w=fleet$/);

  // A nested path is sent back to the shell's directory with its query, so
  // the relative asset URLs load and the workspace opens.
  await page.goto("/a/b?w=inspect");
  await expect(page).toHaveURL(/\/\?w=inspect$/);
  await expect(
    page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
      name: "Inspect",
    }),
  ).toHaveAttribute("aria-current", "page");
});

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
    await page.goto("/?w=unknown");
    await expect(
      page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
        name: "Fleet",
      }),
    ).toHaveAttribute("aria-current", "page");

    // No publisher: one upstream cause for the pipeline's wanted output,
    // rolled up with every other pipeline that has none.
    const attention = page.getByRole("region", { name: /Needs attention/ });
    const group = attention.getByRole("button", { name: /No publisher connected/ });
    if ((await group.getAttribute("aria-expanded")) !== "true") await group.click();
    const item = attention
      .getByRole("listitem")
      .filter({ hasText: name })
      .filter({ hasNot: page.getByRole("button") });
    await expect(item).toContainText("No publisher connected");
    await expect(item).toContainText("cr-out");
    await expect(item.getByRole("link", { name })).toHaveAttribute(
      "href",
      new RegExp(`[?&]w=operate&p=${pipelineId}$`),
    );
    await expect(page.getByRole("status").first()).toContainText("Live ·");

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

test("fleet: shared work, open incidents, the history and the worst-first table", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const stamp = Date.now();
  const name = `cr-fleetrow-${stamp}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crfleetrow_${stamp}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: "cr-out",
        url: "rtmp://127.0.0.1:9/live/cr",
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      },
    });
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    expect(
      (await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`)).ok(),
    ).toBe(true);

    await page.goto("/?w=fleet");
    await expect(page.getByRole("group", { name: "Shared work" })).toContainText(/\d+ stages/);
    await expect(page.getByRole("group", { name: "Open incidents" })).toContainText(
      /\d+ critical · \d+ warning/,
    );

    // The wanted output with no publisher is a degraded ingest output, sampled
    // into the page's history on the store's 2 s clock.
    const history = page.getByRole("region", { name: /Fleet history/ });
    await expect(history.getByRole("img", { name: /Delivery and headroom over time/ })).toBeVisible();
    await expect(history).toContainText(/now: .*ingest [1-9]/);
    await expect(history).toContainText(/[2-9]\d* samples|\d{2,} samples/);

    // Degraded rows come first, each linked to its Operate view.
    const table = page.getByRole("table", { name: "Worst pipelines" });
    const row = table.getByRole("row").filter({ hasText: name });
    await expect(row).toContainText("1 degraded");
    await expect(row.getByRole("link", { name })).toHaveAttribute(
      "href",
      new RegExp(`[?&]w=operate&p=${pipelineId}$`),
    );
    await expect(table.getByRole("row").nth(1)).toContainText("degraded");
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
    await page.goto("/?w=pipelines");
    await page.getByRole("searchbox", { name: "Search pipelines" }).fill(name.toUpperCase());
    const rows = page.getByRole("row").filter({ hasText: name });
    await expect(rows).toHaveCount(1);
    await expect(rows).toContainText("no source");
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
    await expect(matrixRow.getByRole("button", { name: "Stop cr-out" })).toBeVisible();
    await expect(matrixRow).toContainText("waiting for source");
    await expect(matrixRow).toContainText("No publisher connected");
    await matrixRow.getByRole("button", { name: "Stop cr-out" }).click();
    await expect(matrixRow.getByRole("button", { name: "Start cr-out" })).toBeVisible();
    await expect(matrixRow).toContainText("stopped");

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

test("operate: an input change shows from the state stream without a poll", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name: `cr-pushed-${stamp}`, streamKey: `crpushed_${stamp}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  // The page's own reads of a pipeline's inputs or graph: the deck and the
  // media path come from the state stream, so there must be none.
  const polls: string[] = [];
  page.on("request", (request) => {
    if (
      request.method() === "GET" &&
      /\/api\/v1\/pipelines\/[^/]+\/(inputs|graph)(\?|$)/.test(new URL(request.url()).pathname)
    ) {
      polls.push(request.url());
    }
  });
  try {
    await page.goto(`/?w=operate&p=${pipelineId}`);
    const deck = page.getByRole("region", { name: /Source deck/ });
    await expect(deck).toContainText("0 / 1 connected");

    // Changed through the API, not the page: only the stream can tell it.
    const backup = await page.request.post(`/api/v1/pipelines/${pipelineId}/inputs`, {
      data: { label: "Backup" },
    });
    expect(backup.ok()).toBe(true);
    const backupId: string = (await backup.json()).input.id;
    const item = deck.getByRole("listitem", { name: "Input Backup" });
    await expect(item).toContainText("offline");
    expect(
      (
        await page.request.patch(`/api/v1/pipelines/${pipelineId}/inputs/${backupId}`, {
          data: { enabled: false },
        })
      ).ok(),
    ).toBe(true);
    await expect(item).toContainText("disabled");
    await expect(deck).toContainText("0 / 2 connected");
    expect(polls).toEqual([]);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("incidents: the correlation reaches back to an incident older than the page", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const name = `cr-older-${stamp}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crolder_${stamp}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;
  try {
    const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: "cr-out",
        url: "rtmp://127.0.0.1:9/live/cr",
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      },
    });
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    expect(
      (await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`)).ok(),
    ).toBe(true);

    // The incident opens before any page is there to see it.
    let firstSeen = "";
    await expect
      .poll(async () => {
        const alerts = await (await page.request.get("/api/v1/alerts")).json();
        const alert = alerts.alerts.find(
          (candidate: { pipelineId?: string }) => candidate.pipelineId === pipelineId,
        );
        firstSeen = alert?.firstSeen ?? "";
        return firstSeen;
      })
      .not.toBe("");
    // The engine has sampled the degraded fleet since then.
    await expect
      .poll(
        async () => {
          const history = await (
            await page.request.get(
              `/api/v1/fleet/history?since=${encodeURIComponent(firstSeen)}`,
            )
          ).json();
          return history.samples.some(
            (sample: { degraded: { ingest: number } }) => sample.degraded.ingest > 0,
          );
        },
        { timeout: 20_000 },
      )
      .toBe(true);

    await page.goto("/?w=incidents");
    const incident = page
      .getByRole("region", { name: /^Open/ })
      .getByRole("button")
      .filter({ hasText: `${name}: ` })
      .first();
    await incident.click();
    // The chart spans the open: its cursor is drawn although the page
    // opened after it.
    const correlation = page.getByLabel("Correlation");
    await expect(correlation.locator('[data-series="incident"]').first()).toBeAttached();
    await expect(correlation.getByRole("img", { name: "Degraded outputs over time" })).toBeVisible();
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
    await page.goto(`/?w=operate&p=${pipelineId}`);
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

    // The output went from not wanted (0 degraded) to wanted and diagnosed
    // while the page watched: one incident onset, marked on the signals.
    await expect(signals.getByLabel("Degraded outputs now")).toHaveText("1");
    await expect(signals.getByLabel("Incident onsets")).toContainText("degraded at");
    await expect(signals.locator('[data-series="incident"]').first()).toBeAttached();

    await expect(page.getByRole("region", { name: "Input transport" })).toContainText(
      "No selected input is connected.",
    );
    await expect(page.getByRole("region", { name: "Shared media path" })).toContainText(
      /Source ring\s*not running/,
    );
    await expect(page.getByRole("region", { name: /Destination outlier/ })).toContainText(
      "Every rated output delivers at least 95%",
    );

    // Pause freezes the window; Compare overlays the live one on it.
    const compare = signals.getByRole("button", { name: "Compare" });
    await expect(compare).toBeDisabled();
    await signals.getByRole("button", { name: "Pause" }).click();
    await expect(signals).toContainText("paused at");
    const pausedCount = await signals.getByText(/\d+ samples/).textContent();
    await page.waitForTimeout(2_500);
    await expect(signals.getByText(/\d+ samples/)).toHaveText(pausedCount ?? "");
    await compare.click();
    await expect(compare).toHaveAttribute("aria-pressed", "true");
    await expect(signals.locator('[data-series="compare"]').first()).toBeAttached();
    await signals.getByRole("button", { name: "Resume" }).click();
    await expect(signals.locator('[data-series="compare"]')).toHaveCount(0);
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

    await page.goto("/?w=incidents");
    const queue = page.getByRole("region", { name: /^Open/ });
    // The title names the pipeline, not its id.
    const incident = queue.getByRole("button").filter({ hasText: `${name}: No active publisher` }).first();
    await expect(incident).toBeVisible();
    await incident.click();
    const packet = page.getByRole("region", { name: "Evidence packet" });
    await expect(packet).toContainText(`pipeline ${name}`);
    await expect(packet.getByText("Operator action")).toBeVisible();
    await expect(packet.getByText("measured").first()).toBeVisible();
    await expect(page.getByLabel("Correlation")).toBeVisible();

    const [download] = await Promise.all([
      page.waitForEvent("download"),
      packet.getByRole("button", { name: "Export evidence" }).click(),
    ]);
    const exported = JSON.parse(await readFile(await download.path(), "utf8"));
    expect(exported.status).toBe("open");
    expect(exported.alert.pipelineId).toBe(pipelineId);
    expect(exported.packet.evidence[0].confidence).toMatch(/^(measured|derived)$/);

    // Deleting the pipeline ends its incident: the engine lists it as
    // resolved in the chosen window (by id: the name went with it), and the
    // detail reads it as closed.
    expect((await page.request.delete(`/api/v1/pipelines/${pipelineId}`)).ok()).toBe(true);
    await page.getByLabel("Resolved window").selectOption("15m");
    const resolved = queue
      .getByRole("button")
      .filter({ hasText: /resolved (just now|.* ago)/ })
      // The title kept the name it had while open.
      .filter({ hasText: `${name}: ` })
      .first();
    await expect(resolved).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("resolved in the last 15m")).toBeVisible();
    await resolved.click();
    await expect(page.getByRole("region", { name: "Evidence packet" })).toContainText(
      `pipeline ${pipelineId}`,
    );
    await expect(
      page
        .locator('section[aria-labelledby="incident-detail"]')
        .getByText("resolved", { exact: true }),
    ).toBeVisible();

    await page
      .getByRole("navigation", { name: "Workspaces" })
      .getByRole("link", { name: "Capacity" })
      .click();
    await expect(page.getByRole("heading", { name: /Flow Doctor/ })).toBeVisible();
    const scenario = page.getByRole("region", { name: /Scenario/ });
    // The default scenario runs on arrival, so the calibration shows at once.
    const limits = scenario.getByRole("table", { name: "Capacity limits" });
    await expect(limits.getByRole("row", { name: /FFmpeg stages/ })).toContainText(
      /prior · \d+ CPUs|configured/,
    );
    await scenario.getByLabel("More outputs").fill("5000");
    await scenario.getByLabel("Protocol").selectOption("srt");
    await scenario.getByLabel("Encoding").selectOption("1");
    await scenario.getByRole("button", { name: "Project" }).click();
    const result = scenario.getByLabel("Scenario result");
    await expect(result).toContainText("With 5000 more SRT outputs and 1 more stages");
    await expect(result).toContainText("Projected hottest center");
    // The result says whose ingest the added outputs carry, or that none is live.
    await expect(result).toContainText(/No ingest is live|Each added output carries/);
    await expect(scenario.getByRole("link", { name: "draft it in Changes" })).toHaveAttribute(
      "href",
      /w=changes/,
    );
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
  await page.goto("/?w=media");
  // The storage card reads the host sample the shell polls (summary view).
  await expect(page.getByLabel("Media disk used")).toBeVisible({ timeout: 15_000 });
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

  // The owner counters are wall-clock measurements: a descheduled Owner
  // thread on a busy host records a slow pass with no peers. Pin them to one
  // known finding, so the card is checked against what it was given.
  await page.route("**/api/v1/engine/health", async (route) => {
    const response = await route.fetch();
    const health = await response.json();
    const owner = health.srtListener?.ingressOwner;
    if (owner) {
      Object.assign(owner, {
        faulted: false,
        rxRingDropped: 0,
        rxBufferExhaustions: 0,
        eventBridgeFullVisits: 0,
        telemetryDropped: 0,
        overloadDisconnects: 0,
        mediaSlowPasses20ms: 2,
      });
    }
    return route.fulfill({ response, json: health });
  });
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
  const owners = page.getByRole("region", { name: /SRT ingress owners/ });
  await expect(owners.getByRole("list", { name: "Owner findings" })).toHaveText(
    "2 media passes took over 20 ms.",
  );
  await expect(page.getByRole("region", { name: "Build and provenance" })).toContainText(
    "Commit",
  );
});

test("media: a recording's timeline, then the recording as a new pipeline's source", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const pipelineIds: string[] = [];
  let recorded: string | null = null;
  try {
    const pipelineId = await liveFilePipeline(page, `crrec${stamp}`);
    pipelineIds.push(pipelineId);
    expect((await page.request.post(`/api/v1/pipelines/${pipelineId}/recording/start`)).ok()).toBe(
      true,
    );
    // The recorder drops files under 5 s: let this one run past that.
    const sessionFile = async () => {
      const library = await (await page.request.get("/api/v1/media")).json();
      return (library.files as Array<{ name: string; pipelineId?: string | null }>).find(
        (file) => file.pipelineId === pipelineId,
      )?.name;
    };
    await expect.poll(sessionFile, { timeout: 30_000 }).toBeTruthy();
    recorded = (await sessionFile()) ?? null;

    await page.goto("/?w=media");
    await page
      .getByRole("list", { name: "Recordings" })
      .getByRole("button", { name: new RegExp(recorded ?? "") })
      .click();
    const timeline = page.getByRole("list", { name: "Recording timeline" });
    await expect(timeline).toContainText("Recording started");
    await expect(timeline).toContainText("Still recording");

    await page.waitForTimeout(6_000);
    expect((await page.request.post(`/api/v1/pipelines/${pipelineId}/recording/stop`)).ok()).toBe(
      true,
    );
    // The library poll picks up the finalized session, with no reload, and
    // keeps it selected though its .ts became the .mp4.
    await expect(timeline).toContainText("Stopped; MP4 written", { timeout: 30_000 });
    await expect(timeline).not.toContainText("Still recording");
    recorded = (await sessionFile()) ?? recorded;

    // The recording feeds a new pipeline: named after it, looping, chosen.
    await page.getByRole("button", { name: "Use as source" }).click();
    const form = page.getByRole("region", { name: "New pipeline" });
    await expect(form.getByLabel("Name")).toHaveValue((recorded ?? "").replace(/\.[^.]+$/, ""));
    await expect(form.getByRole("combobox", { name: /^File/ })).toHaveValue(recorded ?? "");
    await expect(form.getByLabel("Loop")).toBeChecked();
    await form.getByRole("button", { name: "Save" }).click();
    await expect(page).toHaveURL(/[?&]w=operate&p=[^&]+$/);
    const created = new URL(page.url()).searchParams.get("p") ?? "";
    pipelineIds.push(created);
    const ingest = await (await page.request.get(`/api/v1/pipelines/${created}/file-ingest`)).json();
    expect(ingest).toMatchObject({ configured: true, filename: recorded, loop: true });
  } finally {
    for (const id of pipelineIds.reverse()) await page.request.delete(`/api/v1/pipelines/${id}`);
    if (recorded) await page.request.delete(`/api/v1/media/${encodeURIComponent(recorded)}`);
  }
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

  await page.goto("/?w=settings");
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

  await page.goto("/?w=changes");
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
    // A stopped destination is wanted by nothing yet: no topology change, no load.
    await expect(page.getByRole("region", { name: "Impact" })).toContainText(
      "The change adds no outputs or stages",
    );
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
    await expect(operation.getByRole("table", { name: "Per-change checks" })).toContainText("passed");
    const gates = operation.getByRole("table", { name: "Verification gates" });
    await expect(gates.getByRole("row")).toHaveCount(4);
    await expect(gates).not.toContainText("failed");
    await expect(operation.getByRole("list", { name: "Convergence timeline" })).toContainText(
      "created, desired stopped",
    );
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

    await page.goto("/?w=changes");
    const define = page.getByRole("region", { name: "Define the change" });
    await define.getByLabel("Why (kept in the audit log)").fill("start the backup");
    await define.getByLabel("Pipeline").selectOption({ label: name });
    await define.getByRole("combobox", { name: /^Change\b/ }).selectOption("startOutput");
    await define.getByRole("combobox", { name: /^Destination\b/ }).selectOption({ label: "cr-start" });
    await define.getByRole("button", { name: "Preview" }).click();
    // Starting it adds one wanted RTMP output, which the scenario prices.
    const impact = page.getByRole("region", { name: "Impact" });
    const rtmp = impact.getByRole("table", { name: "Topology diff" }).getByRole("row", { name: /RTMP/ });
    await expect(rtmp.getByRole("cell")).toHaveText([/source/, "RTMP", "0", "1"]);
    await expect(impact.getByLabel("Projected load")).toContainText("Hottest center");
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
    await expect(operation.getByRole("table", { name: "Per-change checks" })).toContainText("pendingInput");
    // The fixed gates say why: nothing on the pipeline delivers yet.
    await expect(
      operation.getByRole("table", { name: "Verification gates" }).getByRole("row", { name: /Outputs it started are running/ }),
    ).toContainText("failed");
    await expect(operation.getByLabel("Live convergence")).toContainText("0 of 1 changed outputs converged");
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

    await page.goto("/?w=changes");
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
    await page.goto("/?w=pipelines");
    await page.getByRole("button", { name: "New pipeline" }).click();
    const create = page.getByRole("region", { name: "New pipeline" });
    await create.getByLabel("Name").fill(name);
    await create.getByRole("button", { name: "Save" }).click();
    // Created: the cockpit of the new pipeline opens.
    await expect(page).toHaveURL(/w=operate/);
    pipelineId = new URL(page.url()).searchParams.get("p") ?? "";
    expect((await stored(pipelineId)).name).toBe(name);

    // A backup input: added through the new-input form, disabled, deleted.
    await page.getByRole("button", { name: "Add input" }).click();
    const newInput = page.getByRole("region", { name: "New input" });
    await newInput.getByLabel("Label").fill("cr-backup");
    await newInput.getByRole("button", { name: "Save" }).click();
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

    // A pipeline whose destinations run does not offer a new key: the live
    // encoder would lose it at its next reconnect.
    const running = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: "cr-keylock",
        url: "rtmp://127.0.0.1:9/live/crkeylock",
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      },
    });
    expect(running.ok()).toBe(true);
    const runningId: string = (await running.json()).output.id;
    const outputPath = `/api/v1/pipelines/${pipelineId}/outputs/${runningId}`;
    expect((await page.request.post(`${outputPath}/start`)).ok()).toBe(true);
    await expect(pipelineSettings.getByLabel(/Issue a new stream key/)).toBeDisabled();
    expect((await page.request.post(`${outputPath}/stop`)).ok()).toBe(true);
    await expect(pipelineSettings.getByLabel(/Issue a new stream key/)).toBeEnabled();

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

/** The checked-in clip CI copies into the engine's media library (two video
 * streams, 16 audio tracks). */
const LIVE_CLIP = "colorbar-timer-2v16a.mp4";

/** A pipeline whose source the engine publishes itself (a looped file ingest
 * of `LIVE_CLIP`), so a case needs no outside publisher; returns once its
 * input is on. */
async function liveFilePipeline(page: Page, name: string): Promise<string> {
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `${name}_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const id: string = (await created.json()).pipeline.id;
  const source = await page.request.put(`/api/v1/pipelines/${id}/file-ingest`, {
    data: { filename: LIVE_CLIP, loop: true, liveOptimized: true, targetGopSeconds: 4 },
  });
  expect(source.ok(), `the media library holds ${LIVE_CLIP}`).toBe(true);
  const ingest = await (await page.request.get(`/api/v1/pipelines/${id}/file-ingest`)).json();
  expect((await page.request.post(`/api/v1/ingests/${ingest.id}/start`)).ok()).toBe(true);
  await expect
    .poll(
      async () =>
        (await (await page.request.get("/api/v1/engine/health")).json()).pipelines?.[id]?.input?.status,
      { timeout: 40_000 },
    )
    .toBe("on");
  return id;
}

/** A destination on any host: the engine's own RTMP ingest, under the key of
 * a pipeline made for it. */
async function loopbackSink(page: Page, name: string): Promise<{ pipelineId: string; url: string }> {
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `${name}_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;
  const inputs = await (await page.request.get(`/api/v1/pipelines/${pipelineId}/inputs`)).json();
  const url = String(inputs.inputs[0].ingestUrls.rtmp).replace("//localhost:", "//127.0.0.1:");
  return { pipelineId, url };
}

test("logs: a pipeline's history, then its rows live; paused, nothing arrives; engine-only rows name no pipeline", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const pipelineIds: string[] = [];
  try {
    const sourceId = await liveFilePipeline(page, `crlogs${stamp}`);
    pipelineIds.push(sourceId);
    const sink = await loopbackSink(page, `crlogssink${stamp}`);
    pipelineIds.push(sink.pipelineId);
    const outputName = `cr-logs-out-${stamp}`;
    const output = await page.request.post(`/api/v1/pipelines/${sourceId}/outputs`, {
      data: {
        name: outputName,
        url: sink.url,
        config: { video: { mode: "source" }, audio: { mode: "selectTracks", tracks: [0] } },
      },
    });
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    const outputPath = `/api/v1/pipelines/${sourceId}/outputs/${outputId}`;
    const writtenStarts = async () =>
      (
        await (await page.request.get(`/api/v1/logs?output_id=${outputId}&limit=1000`)).json()
      ).logs.filter((row: { message: string }) => row.message === "output job started").length;
    expect((await page.request.post(`${outputPath}/start`)).ok()).toBe(true);
    await expect.poll(writtenStarts, { timeout: 20_000 }).toBe(1);

    await page.goto(`/?w=logs&p=${sourceId}`);
    await expect(page.getByRole("combobox", { name: /^Scope\b/ })).toHaveValue(`p:${sourceId}`);
    const rows = page.getByRole("table", { name: "Log rows" }).locator("tbody tr");
    const live = page.getByRole("region", { name: "Logs" }).getByRole("status");
    await expect(live).toHaveText("live");
    // The history holds the pipeline's lifecycle; a stop written after the
    // page loaded arrives without a reload, named by its destination.
    const started = rows.filter({ hasText: "output job started" });
    await expect(started).toHaveCount(1);
    const stopped = rows.filter({ hasText: "output job stopped" });
    await expect(stopped).toHaveCount(0);
    expect((await page.request.post(`${outputPath}/stop`)).ok()).toBe(true);
    await expect(stopped).toHaveCount(1);
    await expect(stopped.first()).toContainText(outputName);

    // Paused: the engine writes the start, the page does not show it, and
    // keeps the rows it shows.
    const table = page.getByRole("table", { name: "Log rows" });
    await table.evaluate((element) => element.setAttribute("data-probe", "kept"));
    await page.getByRole("button", { name: "Pause live" }).click();
    await expect(live).toHaveText("paused");
    await expect(table).toHaveAttribute("data-probe", "kept");
    expect((await page.request.post(`${outputPath}/start`)).ok()).toBe(true);
    await expect.poll(writtenStarts, { timeout: 20_000 }).toBe(2);
    await page.waitForTimeout(1000);
    await expect(started).toHaveCount(1);
    // Resumed: the stream sends what was written while paused; the rows on
    // screen were never read again (the table, and its mark, stayed).
    await page.getByRole("button", { name: "Resume live" }).click();
    await expect(live).toHaveText("live");
    await expect(started).toHaveCount(2);
    await expect(table).toHaveAttribute("data-probe", "kept");

    await page.getByRole("combobox", { name: /^Scope\b/ }).selectOption("engine");
    await expect(rows.first()).toBeVisible();
    const places = await rows.locator("td:nth-child(3)").allTextContents();
    expect(new Set(places)).toEqual(new Set(["engine"]));
  } finally {
    for (const id of pipelineIds) await page.request.delete(`/api/v1/pipelines/${id}`);
  }
});

test("diagnostics: run the engine's checks on a live pipeline and download the report", async ({
  page,
}) => {
  test.setTimeout(150_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const name = `crdiag${Date.now()}`;
  let pipelineId: string | null = null;
  try {
    pipelineId = await liveFilePipeline(page, name);
    await page.goto(`/?w=inspect&p=${pipelineId}`);
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
    expect(file.suggestedFilename()).toMatch(new RegExp(`^diagnostic-${name}-${pipelineId}-[0-9_-]+\\.json$`));
    const report = JSON.parse(await readFile(await file.path(), "utf8"));
    expect(report.schema).toBe("restream.diagnostics.v1");
    expect(report.pipeline).toEqual({ id: pipelineId, name });
    expect(report.results).toHaveLength(shown);
  } finally {
    if (pipelineId) await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("previews and stage detail: play the program and an input; a transcode stage shows its pipe", async ({
  page,
}) => {
  test.setTimeout(180_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const playing = async (label: string, tracks: number) => {
    const preview = page.getByRole("group", { name: `Preview ${label}` });
    await preview.getByRole("button", { name: `Preview ${label}` }).click();
    await expect(preview.getByRole("status")).toHaveText("live", { timeout: 30_000 });
    // Live means frames advance, not only that a manifest loaded.
    const video = preview.getByLabel(`Video ${label}`);
    await expect
      .poll(() => video.evaluate((element: HTMLVideoElement) => element.currentTime))
      .toBeGreaterThan(0.5);
    // Each audio track is a choice; choosing one loads that rendition.
    const picker = preview.getByRole("combobox", { name: `Audio track of ${label}` });
    await expect(picker.locator("option")).toHaveCount(tracks);
    const rendition = page.waitForRequest(/\/audio\/3\/(index\.m3u8|init\.mp4|[^/]+\.m4s)/);
    await picker.selectOption({ index: 3 });
    await rendition;
    await preview.getByRole("button", { name: "Stop preview" }).click();
    await expect(video).toHaveCount(0);
  };
  const stamp = Date.now();
  const pipelineIds: string[] = [];
  try {
    const pipelineId = await liveFilePipeline(page, `crpreview${stamp}`);
    pipelineIds.push(pipelineId);
    await page.goto(`/?w=operate&p=${pipelineId}`);
    // The program plays in the media path's Preview view.
    await page.getByRole("tab", { name: "Preview" }).click();
    await playing("program", 16);
    await playing("Primary", 16);

    // Paused by the operator: the preview says so and keeps the video.
    const input = page.getByRole("group", { name: "Preview Primary" });
    await input.getByRole("button", { name: "Preview Primary" }).click();
    await expect(input.getByRole("status")).toHaveText("live", { timeout: 30_000 });
    await input.getByLabel("Video Primary").evaluate((element: HTMLVideoElement) => element.pause());
    await expect(input.getByRole("status")).toHaveText("paused");
    await expect(input.getByLabel("Video Primary")).toHaveCount(1);
    await input.getByRole("button", { name: "Stop preview" }).click();

    const sink = await loopbackSink(page, `crpreviewsink${stamp}`);
    pipelineIds.push(sink.pipelineId);
    const created = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
      data: {
        name: `cr-transcode-${stamp}`,
        url: sink.url,
        config: {
          video: { mode: "preset", preset: "720p" },
          audio: { mode: "selectTracks", tracks: [0] },
        },
      },
    });
    expect(created.ok()).toBe(true);
    const outputId: string = (await created.json()).output.id;
    expect((await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`)).ok()).toBe(true);
    await page.goto(`/?w=inspect&p=${pipelineId}`);
    await page.getByRole("button", { name: "video:720p:codec:h264", exact: true }).click();
    // The resource map has the stage's counters; its pipe to ffmpeg comes
    // from the stage's own telemetry.
    await expect(page.getByRole("group", { name: "Stage pipe" })).toContainText("idles", { timeout: 20_000 });

    // A playlist that stops growing (a stalled stage upstream) raises no
    // hls.js error: freeze every playlist at its last body, and the preview
    // must leave "live", then give up with a Retry.
    const playlists = new Map<string, string>();
    let frozen = false;
    await page.route(/\/hls\/.*\.m3u8(\?.*)?$/, async (route) => {
      const url = route.request().url();
      const last = playlists.get(url);
      if (frozen && last !== undefined) {
        return route.fulfill({ body: last, contentType: "application/vnd.apple.mpegurl" });
      }
      const response = await route.fetch();
      const body = await response.text();
      playlists.set(url, body);
      return route.fulfill({ response, body });
    });
    await page.goto(`/?w=operate&p=${pipelineId}`);
    await page.getByRole("tab", { name: "Preview" }).click();
    const program = page.getByRole("group", { name: "Preview program" });
    await program.getByRole("button", { name: "Preview program" }).click();
    const status = program.getByRole("status");
    await expect(status).toHaveText("live", { timeout: 30_000 });
    frozen = true;
    await expect(status).toHaveText("buffering", { timeout: 30_000 });
    await expect(status).toHaveText("stalled: no new frames", { timeout: 30_000 });
    await expect(program.getByRole("button", { name: "Retry preview program" })).toBeVisible();
    await expect(program.getByLabel("Video program")).toHaveCount(0);
  } finally {
    for (const id of pipelineIds) await page.request.delete(`/api/v1/pipelines/${id}`);
  }
});

test("program preview over MoQ: the switch plays the program straight from the engine and is kept", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  let pipelineId = "";
  try {
    pipelineId = await liveFilePipeline(page, `crmoq${Date.now()}`);
    await page.goto(`/?w=operate&p=${pipelineId}`);
    await page.getByRole("tab", { name: "Preview" }).click();
    const moq = page.getByRole("radio", { name: "MOQ" });
    await expect(page.getByRole("radio", { name: "HLS" })).toHaveAttribute("aria-checked", "true");
    await moq.click();
    const preview = page.getByRole("group", { name: "MoQ preview program" });
    await preview.getByRole("button", { name: "Preview program" }).click();
    // Live means a frame decoded; the canvas then shows the colour bars
    // (bright and dark areas), not an unpainted black frame.
    await expect(preview.getByRole("status")).toHaveText("live (MoQ)", { timeout: 30_000 });
    const canvas = preview.getByLabel("Video program");
    await expect
      .poll(() =>
        canvas.evaluate((element: HTMLCanvasElement) => {
          const thumb = document.createElement("canvas");
          thumb.width = 16;
          thumb.height = 9;
          const context = thumb.getContext("2d", { willReadFrequently: true });
          if (!context || element.width === 0) return 0;
          context.drawImage(element, 0, 0, 16, 9);
          const pixels = context.getImageData(0, 0, 16, 9).data;
          let low = 255;
          let high = 0;
          for (let index = 0; index < pixels.length; index += 4) {
            const luma = 0.2126 * pixels[index] + 0.7152 * pixels[index + 1] + 0.0722 * pixels[index + 2];
            low = Math.min(low, luma);
            high = Math.max(high, luma);
          }
          return high - low;
        }),
      )
      .toBeGreaterThan(64);
    await preview.getByRole("button", { name: "Stop preview" }).click();
    await expect(canvas).toHaveCount(0);
    // The choice is kept across visits.
    await page.reload();
    await page.getByRole("tab", { name: "Preview" }).click();
    await expect(page.getByRole("radio", { name: "MOQ" })).toHaveAttribute("aria-checked", "true");
  } finally {
    if (pipelineId) await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("matrix: a large fleet renders a window of rows and scrolls to the rest", async ({
  page,
}) => {
  // 500 outputs over the API, then two full renders.
  test.setTimeout(90_000);

  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-matrix-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crmatrix_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    // A fleet large enough that rendering every row would put thousands of
    // cells in the DOM. Nothing is started: this is the render, not media.
    const total = 500;
    for (let start = 0; start < total; start += 50) {
      const responses = await Promise.all(
        Array.from({ length: Math.min(50, total - start) }, (_, index) =>
          page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
            data: {
              name: `cr-m-${start + index}`,
              url: `rtmp://127.0.0.1:9/live/cr-m-${start + index}`,
              config: { video: { mode: "source" }, audio: { mode: "all" } },
            },
          }),
        ),
      );
      for (const response of responses) expect(response.ok()).toBe(true);
    }

    await page.goto(`/?w=operate&p=${pipelineId}`);
    const matrix = page.getByRole("region", { name: /Destination matrix/ });
    const table = matrix.getByRole("table", { name: "Destinations" });
    // Every row is in the table's count; only the window in view is in the
    // DOM (ui/data-table.tsx), so the render stays small.
    await expect(table).toHaveAttribute("aria-rowcount", String(total));
    await expect(matrix).toContainText(`${total} rows`);
    expect(await table.getByRole("row").count()).toBeLessThan(80);

    // Nothing is dropped: scrolling the matrix reaches its last row (by
    // name, as none is degraded).
    const last = table.getByRole("row").filter({ hasText: /^cr-m-99\b/ });
    await expect(last).toHaveCount(0);
    await table.locator(".overflow-y-auto").evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    await expect(table.getByRole("row").filter({ hasText: "cr-m-99" }).first()).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("operate views: inspect an egress group, list the groups, read the pipeline's timeline", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const pipelineIds: string[] = [];
  try {
    const pipelineId = await liveFilePipeline(page, `crgroup${stamp}`);
    pipelineIds.push(pipelineId);
    // One sink per output: a sink's key takes one publisher at a time.
    for (const name of ["cr-group-a", "cr-group-b"]) {
      const sink = await loopbackSink(page, `crgroupsink${name}${stamp}`.replaceAll("-", ""));
      pipelineIds.push(sink.pipelineId);
      const created = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
        data: {
          name,
          url: sink.url,
          config: { video: { mode: "source" }, audio: { mode: "selectTracks", tracks: [0] } },
        },
      });
      expect(created.ok()).toBe(true);
      const outputId: string = (await created.json()).output.id;
      expect(
        (await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`)).ok(),
      ).toBe(true);
    }

    await page.goto(`/?w=operate&p=${pipelineId}`);
    // Both RTMP destinations read one stage: one group, inspected in place.
    await page.getByRole("button", { name: /^Inspect RTMP group from / }).click();
    const inspector = page.getByRole("region", { name: "Selected RTMP group" });
    const delivery = inspector.getByRole("definition").first();
    await expect(delivery).toHaveText("2 / 2", { timeout: 30_000 });
    // The fabric shard carrying them comes from the engine's health report.
    await expect(inspector.getByRole("list", { name: "Fabric shards" })).toContainText(
      /shard \d+ · healthy · queue \d+ \/ \d+/,
      { timeout: 30_000 },
    );
    await expect(inspector).toContainText("No output of this group has a diagnosed fault.");

    // The same group in the table view; the selection follows.
    await page.getByRole("tab", { name: "Destinations" }).click();
    const groups = page.getByRole("table", { name: "Destination groups" });
    const row = groups.getByRole("row").filter({ hasText: "RTMP" });
    await expect(row).toContainText("2 / 2");
    await expect(row.getByRole("button", { name: "Inspect" })).toHaveAttribute("aria-pressed", "true");
    await row.getByRole("button", { name: "Inspect" }).click();
    await expect(inspector).toHaveCount(0);

    // The engine's kept lifecycle events for this pipeline, newest first.
    await page.getByRole("tab", { name: "Timeline" }).click();
    await expect(page.getByRole("list", { name: "Pipeline timeline" })).toContainText("cr-group-a");

    // The matrix's Detail switch adds its counters, and this browser keeps it.
    const matrix = page.locator('section[aria-labelledby="operate-destinations"]');
    await expect(matrix.getByText("Feed lag", { exact: true })).toHaveCount(0);
    await matrix.getByRole("button", { name: "Detail" }).click();
    await expect(matrix.getByText("Feed lag", { exact: true })).toBeVisible();
    await page.reload();
    await expect(matrix.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
    await expect(matrix.getByText("Feed lag", { exact: true })).toBeVisible();
  } finally {
    for (const id of pipelineIds) await page.request.delete(`/api/v1/pipelines/${id}`);
  }
});

test("pipelines: protocol mix, the stage two outputs share, and the recording card", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const name = `crpipes${stamp}`;
  const pipelineIds: string[] = [];
  try {
    const pipelineId = await liveFilePipeline(page, name);
    pipelineIds.push(pipelineId);
    const sinkNames: string[] = [];
    // One sink per output. Two outputs publishing to one ingest URL race for
    // that input's single publisher slot: the engine refuses the second
    // ("Input already has an active publisher"), and whether its retry lands
    // after the other's reconnect decides the card this test reads — so the
    // test measured a race, not the card.
    for (const output of ["cr-share-a", "cr-share-b"]) {
      const sinkName = `crpipessink${stamp}${output.slice(-1)}`;
      const sink = await loopbackSink(page, sinkName);
      pipelineIds.push(sink.pipelineId);
      sinkNames.push(sinkName);
      const created = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
        data: {
          name: output,
          url: sink.url,
          config: { video: { mode: "source" }, audio: { mode: "selectTracks", tracks: [0] } },
        },
      });
      expect(created.ok()).toBe(true);
      const outputId: string = (await created.json()).output.id;
      expect(
        (await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`)).ok(),
      ).toBe(true);
    }

    await page.goto("/?w=pipelines");
    // Both destinations are RTMP, counted fleet-wide.
    await expect(
      page.getByRole("region", { name: "Protocol mix" }).getByRole("listitem").filter({ hasText: "RTMP" }),
    ).toContainText("2");
    // Same pipeline, same video and audio: one stage serves both.
    const sharing = page.getByRole("region", { name: "Shared-stage efficiency" });
    await expect(sharing).toContainText("2 outputs", { timeout: 30_000 });
    await expect(sharing).toContainText(name);
    await expect(sharing).toContainText("2 of 2 running outputs");

    // The recording card selects only the pipeline that records.
    const cards = page.getByRole("region", { name: "Operational filters" });
    const recording = cards.getByRole("button", { name: /Recording active/ });
    await expect(recording).toContainText("0 pipelines");
    expect((await page.request.post(`/api/v1/pipelines/${pipelineId}/recording/start`)).ok()).toBe(
      true,
    );
    await expect(recording).toContainText("1 pipeline", { timeout: 30_000 });
    await recording.click();
    await expect(recording).toHaveAttribute("aria-pressed", "true");
    const rows = page.getByRole("table").getByRole("row");
    await expect(rows.filter({ hasText: name })).toHaveCount(1);
    // Neither sink is recording, so neither is listed while the filter is on.
    for (const sink of sinkNames) await expect(rows.filter({ hasText: sink })).toHaveCount(0);
    // A second click clears the card.
    await recording.click();
    for (const sink of sinkNames) await expect(rows.filter({ hasText: sink })).toHaveCount(1);
  } finally {
    for (const id of pipelineIds) await page.request.delete(`/api/v1/pipelines/${id}`);
  }
});

test("inspect: a node's metric cards and bottleneck judgement, then the global map", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const pipelineIds: string[] = [];
  try {
    const sourceId = await liveFilePipeline(page, `crnode${stamp}`);
    pipelineIds.push(sourceId);
    const sink = await loopbackSink(page, `crnodesink${stamp}`);
    pipelineIds.push(sink.pipelineId);
    const output = await page.request.post(`/api/v1/pipelines/${sourceId}/outputs`, {
      data: {
        name: `cr-node-out-${stamp}`,
        url: sink.url,
        config: { video: { mode: "source" }, audio: { mode: "selectTracks", tracks: [0] } },
      },
    });
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    expect(
      (await page.request.post(`/api/v1/pipelines/${sourceId}/outputs/${outputId}/start`)).ok(),
    ).toBe(true);

    await page.goto(`/?w=inspect&p=${sourceId}`);
    const map = page.getByRole("region", { name: /^Resource map/ });
    await map
      .getByRole("row")
      .filter({ hasText: "source ring" })
      .getByRole("button", { name: "Source ring" })
      .click();
    const ring = page.getByRole("region", { name: "Selected · Source ring" });
    // The output reads the ring: the graph names at least one consumer.
    await expect(ring.getByLabel("Node metrics")).toContainText(/Consumers\s*[1-9]/);
    // The ring draws on the shared memory pool, judged from the priors.
    const ringJudgement = ring.getByRole("note", { name: "Bottleneck judgement" });
    await expect(ringJudgement).toContainText("Not a bottleneck");
    await expect(ringJudgement).toContainText(/memory pool every pipeline shares is at \d+%/);
    await expect(ringJudgement).toContainText("calibration priors");

    // The output is judged on the shard it is placed on, measured.
    await map.getByRole("button", { name: "rtmp output" }).click();
    const out = page.getByRole("region", { name: "Selected · rtmp output" });
    const outJudgement = out.getByRole("note", { name: "Bottleneck judgement" });
    await expect(outJudgement).toContainText(/on shard \d+: the shard's command queue is at \d+%/, {
      timeout: 30_000,
    });
    await expect(outJudgement).toContainText("From a measurement.");

    // The global map asks for every pipeline: no pipeline_id.
    const global = page.waitForRequest(
      (request) =>
        request.url().includes("/api/v1/engine/resource-map") &&
        !request.url().includes("pipeline_id="),
    );
    await map.getByRole("button", { name: "Global resource map" }).click();
    await global;
    const runtime = page.getByRole("region", { name: /^Global resource map/ });
    await expect(runtime.getByRole("button", { name: "This pipeline only" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(runtime.getByRole("row").filter({ hasText: "restream" })).toContainText(
      "measured",
    );
  } finally {
    for (const id of pipelineIds) {
      await page.request.delete(`/api/v1/pipelines/${id}`);
    }
  }
});

test("channels: platforms from stored URLs, and a confirmed bulk go-live", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const name = `cr-channels-${stamp}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crchannels_${stamp}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;
  try {
    for (const [label, url] of [
      ["cr-ch-yt", "rtmp://a.rtmp.youtube.com/live2/cr-key"],
      ["cr-ch-tw", "rtmp://live.twitch.tv/app/cr-key"],
    ]) {
      const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
        data: { name: label, url, config: { video: { mode: "source" }, audio: { mode: "all" } } },
      });
      expect(output.ok()).toBe(true);
    }

    await page.goto("/?w=channels");
    await page.getByRole("combobox", { name: "Pipeline" }).selectOption(pipelineId);
    const youtube = page.getByRole("listitem", { name: "Channel cr-ch-yt" });
    await expect(youtube).toContainText("YouTube");
    await expect(page.getByRole("listitem", { name: "Channel cr-ch-tw" })).toContainText("Twitch");
    await expect(youtube.getByRole("switch")).toHaveAttribute("aria-checked", "false");

    // Bulk actions act on the filtered set: this pipeline's two channels.
    await page.getByRole("button", { name: "Go live (2)" }).click();
    const confirm = page.getByRole("dialog", { name: "Confirm bulk action" });
    await expect(confirm).toContainText("Go live on 2 channels?");
    await confirm.getByRole("button", { name: "Go live on 2" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Going live" })).toContainText("2 / 2");
    // The engine wants both running; with no publisher they wait for one.
    await expect(youtube.getByRole("switch")).toHaveAttribute("aria-checked", "true");
    await expect(youtube).toContainText("waiting for source");

    // A stop says what it touches, and can be undone for a minute.
    await page.getByRole("button", { name: "Stop (2)" }).click();
    await expect(confirm).toContainText("across 1 pipeline");
    await confirm.getByRole("button", { name: "Stop 2" }).click();
    await expect(youtube.getByRole("switch")).toHaveAttribute("aria-checked", "false");
    const undo = page.getByRole("status", { name: "Undo" });
    await expect(undo).toContainText("Stopped 2 channels");
    await undo.getByRole("button", { name: "Undo", exact: true }).click();
    await expect(youtube.getByRole("switch")).toHaveAttribute("aria-checked", "true");
    await expect(page.getByRole("listitem", { name: "Channel cr-ch-tw" }).getByRole("switch")).toHaveAttribute("aria-checked", "true");
    await expect(undo).toHaveCount(0);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("stream health: the flight recorder judges a live publisher's keyframes", async ({ page }) => {
  test.setTimeout(90_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const pipelineId = await liveFilePipeline(page, `crhealth${Date.now()}`);
  try {
    // The recorder samples every 2 s; two samples give a bitrate.
    await expect
      .poll(
        async () => {
          const response = await page.request.get(`/api/v1/pipelines/${pipelineId}/flight-recorder`);
          if (!response.ok()) return null;
          const trace = await response.json();
          return trace.samples.some((sample: { ingest: { bitrateBps: number | null } | null }) =>
            Boolean(sample.ingest?.bitrateBps),
          )
            ? trace.schema
            : null;
        },
        { timeout: 30_000 },
      )
      .toBe("restream.trace.v1");

    await page.goto(`/?w=health&p=${pipelineId}`);
    const verdicts = page.getByRole("list", { name: "Stream verdicts" });
    // The clip is re-encoded with a 4 s GOP: past the platforms' 2 s ask,
    // within their 4 s limit.
    await expect(verdicts).toContainText(/Keyframe every [34]\.\d s/, { timeout: 20_000 });
    await expect(page.getByRole("group", { name: "Video" })).toContainText(/\d+p/);
    await expect(page.getByRole("img", { name: "Ingest bitrate over time" })).toBeVisible();
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("stability lab: naive retries stay stuck after a blip, the engine's backoff recovers", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  await page.goto("/?w=stability");
  const story = page.getByRole("region", { name: "Chapter" });
  const slider = page.getByRole("slider", { name: "Position in the story" });

  await page.getByRole("button", { name: "Retry at once" }).click();
  await slider.fill(await slider.getAttribute("max") ?? "0");
  await expect(story).toContainText("Stuck");
  await expect(story).toContainText("the loop feeds itself");
  await expect(page.getByText("STALLED · 0 delivering")).toBeVisible();

  // The engine's own policy (from /capacity/scenario's retryPolicy).
  await page.getByRole("button", { name: "Engine backoff" }).click();
  await slider.fill(await slider.getAttribute("max") ?? "0");
  await expect(story).toContainText("Recovered");
  await expect(story).toContainText("engine backoff 5 s → 300 s");

  // A measured replay reads the pipeline's flight record.
  const pipelineId = await liveFilePipeline(page, `crlab${Date.now()}`);
  try {
    await page.getByRole("button", { name: "Replay a pipeline" }).click();
    await page.getByRole("combobox", { name: "Pipeline" }).selectOption(pipelineId);
    await expect(story).toContainText(/Healthy|Retrying|Degraded|No source/, { timeout: 20_000 });
    await expect(story).toContainText("measured every 2 s");
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("team: an admin adds a viewer, views as one, and the audit log records both", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const username = `cr-viewer-${Date.now()}`;

  await page.goto("/?w=team");
  const add = page.getByRole("region", { name: "Add a member" });
  await add.getByLabel("Username").fill(username);
  await add.getByLabel("Initial password").fill("correct-horse-battery");
  await add.getByRole("combobox", { name: "New member's role" }).selectOption("viewer");
  await add.getByRole("button", { name: "Add member" }).click();
  await expect(page.getByRole("table", { name: "Team members" })).toContainText(username);

  // View as a viewer: the engine enforces it, and the dashboard stops
  // offering what it would refuse.
  await page.getByLabel(/^Account: admin/).click();
  await page.getByRole("button", { name: "Viewer", exact: true }).click();
  const banner = page.getByRole("status").filter({ hasText: "Viewing as" });
  await expect(banner).toContainText("Viewer");
  await expect(page.getByRole("link", { name: "Team" })).toHaveCount(0);
  const refused = await page.request.post("/api/v1/pipelines", { data: { name: "refused" } });
  expect(refused.status()).toBe(403);
  await banner.getByRole("button", { name: "Back to Admin" }).click();
  await expect(banner).toHaveCount(0);

  await page.goto("/?w=audit");
  const log = page.getByRole("table", { name: "Audit log" });
  await expect(log).toContainText("Added a team member");
  await expect(log).toContainText("Refused");
  await expect(log).toContainText("Switched the role they view as");

  const members = (await (await page.request.get("/api/v1/users")).json()).users as Array<{ id: string; username: string }>;
  const created = members.find((member) => member.username === username);
  if (created) await page.request.delete(`/api/v1/users/${created.id}`);
});

test("notifications: a webhook channel's test arrives signed, and the delivery is logged", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const { createServer } = await import("node:http");
  const received: Array<{ signature: string | undefined; body: string }> = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      received.push({ signature: request.headers["x-restream-signature"] as string | undefined, body });
      response.writeHead(204).end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port;
  const name = `cr-hook-${Date.now()}`;
  try {
    await page.goto("/?w=notifications");
    const add = page.getByRole("region", { name: "Add a channel" });
    await add.getByRole("combobox", { name: "Channel kind" }).selectOption("webhook");
    await add.getByLabel("Channel name").fill(name);
    await add.getByLabel("Channel URL").fill(`http://127.0.0.1:${port}/hook`);
    await add.getByRole("button", { name: "Add channel" }).click();
    await expect(add.getByRole("status")).toContainText("Signing secret for");

    const row = page.getByRole("table", { name: "Notification channels" }).getByRole("row").filter({ hasText: name });
    await row.getByRole("button", { name: "Send test" }).click();
    await expect(row).toContainText("Delivered");
    expect(received).toHaveLength(1);
    expect(received[0].signature).toMatch(/^t=\d+,v1=[0-9a-f]{64}$/);
    expect(JSON.parse(received[0].body).event).toBe("test");
    await expect(page.getByRole("table", { name: "Notification deliveries" })).toContainText(name);
  } finally {
    server.close();
    const channels = (await (await page.request.get("/api/v1/notifications/channels")).json()).channels as Array<{ id: string; name: string }>;
    for (const channel of channels.filter((candidate) => candidate.name === name)) {
      await page.request.delete(`/api/v1/notifications/channels/${channel.id}`);
    }
  }
});

test("playlists: the editor saves the play order, and a running playlist moves to its next file", async ({ page }) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  // A 12 s clip, uploaded twice under two names: the playlist's two items.
  const clips = [`cr-pl-a-${stamp}.mp4`, `cr-pl-b-${stamp}.mp4`];
  for (const name of clips) {
    const uploaded = await page.request.post("/api/v1/media/upload", {
      multipart: {
        file: { name, mimeType: "video/mp4", buffer: await readFile("test/fixtures/media-library/sparse-gop-5s.mp4") },
      },
    });
    expect(uploaded.ok()).toBe(true);
  }
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name: `cr-playlist-${stamp}`, streamKey: `crplaylist_${stamp}` },
  });
  const pipelineId: string = (await created.json()).pipeline.id;
  try {
    // The editor: a file source, then a second file queued after it.
    await page.goto(`/?w=operate&p=${pipelineId}`);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const form = page.getByRole("region", { name: "Pipeline settings" });
    await form.getByRole("combobox", { name: "Source" }).selectOption("file");
    await form.getByRole("combobox", { name: "File", exact: true }).selectOption(clips[0]);
    await form.getByRole("combobox", { name: "Add to playlist" }).selectOption(clips[1]);
    await form.getByRole("button", { name: "Add", exact: true }).click();
    await expect(form.getByRole("group", { name: "Playlist" })).toContainText(`2.${clips[1]}`);
    await form.getByRole("checkbox", { name: "Loop the playlist" }).uncheck();
    await form.getByRole("checkbox", { name: /^Live Optimized/ }).check();
    await form.getByRole("button", { name: "Save" }).click();
    await expect
      .poll(async () => (await (await page.request.get(`/api/v1/pipelines/${pipelineId}/file-ingest`)).json()).playlist)
      .toEqual(clips);
    const state = await (await page.request.get(`/api/v1/pipelines/${pipelineId}/file-ingest`)).json();
    expect((await page.request.post(`/api/v1/ingests/${state.id}/start`)).ok()).toBe(true);
    const nowPlaying = async () =>
      (await (await page.request.get(`/api/v1/pipelines/${pipelineId}/file-ingest`)).json()).nowPlaying;
    await expect.poll(nowPlaying, { timeout: 20_000 }).toBe(0);
    await expect(page.getByRole("status", { name: "Now playing" })).toContainText(`1 of 2 · ${clips[0]}`, { timeout: 20_000 });
    // The first clip is 12 s at real time; then the engine plays the second.
    await expect.poll(nowPlaying, { timeout: 45_000 }).toBe(1);
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
    for (const name of clips) await page.request.delete(`/api/v1/media/${encodeURIComponent(name)}`);
  }
});

test("schedule: an event added in the UI can run now, and the engine runs a timed one on its own", async ({ page }) => {
  test.setTimeout(90_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const stamp = Date.now();
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name: `cr-sched-${stamp}`, streamKey: `crsched_${stamp}` },
  });
  const pipelineId: string = (await created.json()).pipeline.id;
  const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
    data: { name: "cr-sched-out", url: "rtmp://127.0.0.1:9/live/x", config: { video: { mode: "source" }, audio: { mode: "all" } } },
  });
  expect(output.ok()).toBe(true);
  const ids: string[] = [];
  try {
    await page.goto("/?w=schedule");
    const add = page.getByRole("region", { name: "Schedule an event" });
    await add.getByLabel("Event name").fill(`cr-go-live-${stamp}`);
    await add.getByRole("combobox", { name: "Pipeline to schedule" }).selectOption(pipelineId);
    await add.getByRole("button", { name: "Schedule it" }).click();
    const row = page.getByRole("row").filter({ hasText: `cr-go-live-${stamp}` });
    await expect(row).toContainText("Go live");
    await row.getByRole("button", { name: "Run now" }).click();
    await expect(page.getByRole("status").filter({ hasText: `cr-go-live-${stamp}` })).toContainText("1 of 1 destinations wanted running");

    // A one-off a few seconds ahead: the engine's scheduler runs it.
    const timed = await page.request.post("/api/v1/schedules", {
      data: { pipelineId, name: `cr-timed-${stamp}`, action: "stopOutputs", outputIds: [], atMs: Date.now() + 6_000, repeat: "once" },
    });
    expect(timed.ok()).toBe(true);
    const timedId: string = (await timed.json()).id;
    ids.push(timedId);
    await expect
      .poll(async () => {
        const all = (await (await page.request.get("/api/v1/schedules")).json()).schedules as Array<{ id: string; lastResult: string | null }>;
        return all.find((schedule) => schedule.id === timedId)?.lastResult;
      }, { timeout: 30_000 })
      .toBe("ok: 1 of 1 destinations stopped");
  } finally {
    const all = (await (await page.request.get("/api/v1/schedules")).json()).schedules as Array<{ id: string; pipelineId: string }>;
    for (const schedule of all.filter((candidate) => candidate.pipelineId === pipelineId)) {
      await page.request.delete(`/api/v1/schedules/${schedule.id}`);
    }
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});

test("share: a link made in Operate plays the pipeline without signing in, until revoked", async ({ page, browser }) => {
  test.setTimeout(120_000);
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);
  const pipelineId = await liveFilePipeline(page, `crshare${Date.now()}`);
  const anonymous = await browser.newContext();
  try {
    await page.goto(`/?w=operate&p=${pipelineId}`);
    const share = page.getByRole("region", { name: "Share" });
    await share.getByLabel("Share link label").fill("cr website");
    await share.getByRole("button", { name: "Create link" }).click();
    const item = share.getByRole("listitem", { name: "Share link cr website" });
    await expect(item).toContainText("Until");
    const url = (await item.locator("code").textContent())?.trim() ?? "";
    expect(url).toMatch(/\/watch\/[0-9a-f]{48}$/);
    const token = url.split("/watch/")[1];

    // Someone else, signed out: the player page and the stream it reads.
    const viewer = await anonymous.newPage();
    await viewer.goto(url);
    await expect(viewer.locator(".title")).toContainText("crshare");
    expect((await anonymous.request.get(`/api/v1/state`)).status()).toBe(401);
    await expect
      .poll(async () => (await anonymous.request.get(`/hls/${pipelineId}/master.m3u8?t=${token}`)).status(), { timeout: 40_000 })
      .toBe(200);
    expect((await anonymous.request.get(`/hls/${pipelineId}/master.m3u8`)).status()).toBe(401);

    await item.getByRole("button", { name: "Revoke cr website" }).click();
    await item.getByRole("button", { name: "Revoke", exact: true }).click();
    await expect(share.getByRole("listitem", { name: "Share link cr website" })).toHaveCount(0);
    expect((await anonymous.request.get(`/hls/${pipelineId}/master.m3u8?t=${token}`)).status()).toBe(401);
    expect((await anonymous.request.get(url)).status()).toBe(404);
  } finally {
    await anonymous.close();
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});
