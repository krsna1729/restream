import { expect, test } from "@playwright/test";

const HARNESS_PATH = "/browser-dom-harness.html";

type HarnessAudioTrack = {
  index: number;
  codec: string;
  channels: number;
  sample_rate: number;
  language?: string;
  title?: string;
  pid?: number;
};

async function mountPreviewPipe(
  page: import("@playwright/test").Page,
  audioTracks: HarnessAudioTrack[] = [
    {
      index: 0,
      codec: "aac",
      channels: 2,
      sample_rate: 48000,
      language: "eng",
      title: "Main Mix",
    },
    {
      index: 1,
      codec: "aac",
      channels: 2,
      sample_rate: 48000,
      language: "spa",
      title: "Commentary",
    },
  ],
): Promise<void> {
  await page.goto(HARNESS_PATH);
  await page.evaluate(async (inputAudioTracks) => {
    const container = document.getElementById("video-player");
    if (!container) throw new Error("no container");

    const pipe = {
      id: "browser-dom-pipe",
      name: "Browser DOM Pipe",
      key: "browser_dom_key",
      inputSource: null,
      ingestUrls: { rtmp: null, srt: null },
      input: {
        status: "on",
        time: null,
        video: { codec: "h264", width: 1920, height: 1080, fps: 30 },
        audio: { codec: "aac", channels: 2, sample_rate: 48000 },
        audioTracks: inputAudioTracks,
        bytesReceived: 0,
        bytesSent: 0,
        readers: 0,
        bitrateKbps: null,
        publisher: null,
        unexpectedReadersCount: 0,
      },
      outs: [],
      stats: {
        inputBitrateKbps: null,
        outputBitrateKbps: null,
        readerCount: 0,
        outputCount: 0,
        readerMismatch: false,
        unexpectedReadersCount: 0,
      },
      recording: { enabled: false, active: false },
    };

    class FakeHls {
      static Events = {
        MANIFEST_PARSED: "manifestParsed",
        AUDIO_TRACKS_UPDATED: "audioTracksUpdated",
        ERROR: "error",
      };

      static isSupported() {
        return true;
      }

      handlers: Record<string, ((event: string, data?: unknown) => void)[]> =
        {};
      audioTrack = 0;

      constructor() {
        const testWindow = window as typeof window & {
          __previewTestHls?: FakeHls;
          __previewTestHlsConstructed?: number;
        };
        testWindow.__previewTestHls = this;
        testWindow.__previewTestHlsConstructed =
          (testWindow.__previewTestHlsConstructed || 0) + 1;
      }

      loadSource(_src: string) {}
      attachMedia(_video: HTMLVideoElement) {}
      destroy() {
        const testWindow = window as typeof window & {
          __previewTestHlsDestroyed?: number;
        };
        testWindow.__previewTestHlsDestroyed =
          (testWindow.__previewTestHlsDestroyed || 0) + 1;
      }

      on(event: string, handler: (event: string, data?: unknown) => void) {
        (this.handlers[event] ||= []).push(handler);
      }

      emit(event: string, data?: unknown) {
        for (const handler of this.handlers[event] || []) {
          handler(event, data);
        }
      }
    }

    (
      window as typeof window & {
        Hls: typeof FakeHls;
      }
    ).Hls = FakeHls;
    window.fetch = async () =>
      new Response("#EXTM3U\n#EXT-X-VERSION:3\n", {
        status: 200,
        headers: { "content-type": "application/vnd.apple.mpegurl" },
      });

    const { renderInputPreview } = await import("/js/features/input-preview.js");
    renderInputPreview(container, pipe as never);
  }, audioTracks);
}

test.describe("Frontend Browser DOM", () => {
  test("dashboard selected-pipeline grid does not overflow mobile viewports", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 375, height: 812 });
    await page.goto(HARNESS_PATH);
    await page.evaluate(() => {
      document.body.innerHTML = `
        <main>
          <div id="dashboard-v2-operate-panel" class="mx-auto grid min-h-0 w-full max-w-7xl flex-1 gap-4 p-4 ">
            <div class="border-base-content/10 bg-base-200 w-full max-w-[18rem] overflow-y-auto rounded-lg border"></div>
            <div data-dashboard-v2-operate-detail-shell class="border-base-content/10 bg-base-200 overflow-y-auto rounded-lg border p-4"></div>
            <div data-dashboard-v2-operate-output-shell class="border-base-content/10 bg-base-200 w-full min-w-0 overflow-y-auto rounded-lg border p-4 xl:min-w-[24rem]"></div>
          </div>
        </main>`;
    });

    const overflow = await page.evaluate(() => ({
      gridScrollWidth: document.getElementById("dashboard-v2-operate-panel")?.scrollWidth,
      gridClientWidth: document.getElementById("dashboard-v2-operate-panel")?.clientWidth,
      gridTemplate: getComputedStyle(
        document.getElementById("dashboard-v2-operate-panel") as HTMLElement,
      ).gridTemplateColumns,
    }));

    expect(overflow.gridScrollWidth).toBeLessThanOrEqual(
      overflow.gridClientWidth,
    );
    expect(overflow.gridTemplate).not.toContain("384px");
  });

  test("login form submits through base-path-aware fetch and keeps password toggle keyboard reachable", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      (
        window as typeof window & { __RESTREAM_BASE_PATH__?: string }
      ).__RESTREAM_BASE_PATH__ = "/restream";
    });
    await page.goto("/login.html");
    const requests: unknown[] = [];
    await page.exposeFunction("recordLoginRequest", (request: unknown) => {
      requests.push(request);
    });
    await page.evaluate(() => {
      window.fetch = async (url, init) => {
        await (
          window as typeof window & {
            recordLoginRequest: (request: unknown) => Promise<void>;
          }
        ).recordLoginRequest({
          url: String(url),
          method: init?.method,
          body: init?.body,
        });
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      };
    });

    await expect(page.locator("form#login-form")).toBeVisible();
    await expect
      .poll(() => page.locator("#toggle-password-btn").evaluate((el) => el.tabIndex))
      .toBeGreaterThanOrEqual(0);
    await page.locator("#toggle-password-btn").click();
    await expect(page.locator("#password-input")).toHaveAttribute(
      "type",
      "text",
    );
    await expect(page.locator("#toggle-password-btn")).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    await page.locator("#password-input").fill("secret-password");
    await page.locator("form#login-form").evaluate((form) => {
      form.addEventListener("submit", (event) => event.preventDefault(), {
        once: true,
      });
    });
    await page.locator("form#login-form").dispatchEvent("submit");

    expect(requests).toEqual([
      {
        url: "/restream/api/v1/auth/login",
        method: "POST",
        body: JSON.stringify({ password: "secret-password" }),
      },
    ]);
  });

  test("login returns to the preserved dashboard location after auth expiry", async ({
    page,
  }) => {
    const returnPath = "/?mode=pipeline&view=operate&p=pipe-retrying#outputs";
    await page.goto(`/login.html?return=${encodeURIComponent(returnPath)}`);
    const requests: unknown[] = [];
    await page.exposeFunction("recordLoginReturnRequest", (request: unknown) => {
      requests.push(request);
    });
    await page.evaluate(() => {
      window.fetch = async (url, init) => {
        await (
          window as typeof window & {
            recordLoginReturnRequest: (request: unknown) => Promise<void>;
          }
        ).recordLoginReturnRequest({
          url: String(url),
          method: init?.method,
          body: init?.body,
        });
        return new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      };
    });

    await page.locator("#password-input").fill("secret-password");
    await page.locator("#login-btn").click();
    await page.waitForURL(`**${returnPath}`);

    expect(requests).toEqual([
      {
        url: "/api/v1/auth/login",
        method: "POST",
        body: JSON.stringify({ password: "secret-password" }),
      },
    ]);
  });

  test("preview audio picker opens and switches tracks without the full app server", async ({
    page,
  }) => {
    await mountPreviewPipe(page);

    const playBtn = page.locator("#video-player button", {
      hasText: "Play preview",
    });
    await expect(playBtn).toBeVisible();
    await playBtn.click();

    await page.waitForFunction(
      () =>
        Boolean(
          (
            window as typeof window & {
              __previewTestHls?: unknown;
            }
          ).__previewTestHls,
        ),
    );
    await page.evaluate(() => {
      const testWindow = window as typeof window & {
        Hls: { Events: Record<string, string> };
        __previewTestHls?: {
          emit: (event: string, data?: unknown) => void;
          audioTrack: number;
        };
      };
      testWindow.__previewTestHls?.emit(
        testWindow.Hls.Events.AUDIO_TRACKS_UPDATED,
        {
          audioTracks: [
            { id: 0, name: "Main Mix", lang: "eng" },
            { id: 1, name: "Commentary", lang: "spa" },
          ],
        },
      );
    });

    const audioPickerButton = page.locator(
      '#video-player button[aria-haspopup="listbox"]',
    );
    await expect(audioPickerButton).toBeVisible();
    await expect(audioPickerButton).toHaveText("Audio: Main Mix");
    await expect(audioPickerButton).toHaveAttribute("aria-expanded", "false");

    await audioPickerButton.click();
    await expect(audioPickerButton).toHaveAttribute("aria-expanded", "true");

    const commentaryOption = page
      .locator('[role="option"]')
      .filter({ hasText: "Commentary" })
      .first();
    await expect(commentaryOption).toBeVisible();
    await commentaryOption.click();

    await expect(audioPickerButton).toHaveText("Audio: Commentary");
    await expect(audioPickerButton).toHaveAttribute("aria-expanded", "false");

    const selectedTrack = await page.evaluate(() => {
      const testWindow = window as typeof window & {
        __previewTestHls?: { audioTrack: number };
      };
      return testWindow.__previewTestHls?.audioTrack ?? null;
    });
    expect(selectedTrack).toBe(1);
  });

  test("preview retries a fatal HLS error before playback starts", async ({
    page,
  }) => {
    await mountPreviewPipe(page);
    await page
      .locator("#video-player button", { hasText: "Play preview" })
      .click();
    await page.waitForFunction(
      () =>
        (
          window as typeof window & {
            __previewTestHlsConstructed?: number;
          }
        ).__previewTestHlsConstructed === 1,
    );

    await page.evaluate(() => {
      const testWindow = window as typeof window & {
        Hls: { Events: Record<string, string> };
        __previewTestHls?: {
          emit: (event: string, data?: unknown) => void;
        };
      };
      testWindow.__previewTestHls?.emit(testWindow.Hls.Events.ERROR, {
        fatal: true,
      });
    });

    await page.waitForFunction(
      () =>
        (
          window as typeof window & {
            __previewTestHlsConstructed?: number;
          }
        ).__previewTestHlsConstructed === 2,
    );
    expect(
      await page.evaluate(
        () =>
          (
            window as typeof window & {
              __previewTestHlsDestroyed?: number;
            }
          ).__previewTestHlsDestroyed,
      ),
    ).toBe(1);
  });

  test("preview audio picker surfaces all high-index tracks and switches to the last one", async ({
    page,
  }) => {
    const languages = [
      "eng", "spa", "fra", "deu", "ita", "por", "jpn", "kor",
      "hin", "tam", "tel", "mal", "ara", "rus", "zho", "ind",
    ];
    const audioTracks = Array.from({ length: 16 }, (_, index) => ({
      index,
      codec: "aac",
      channels: index % 2 === 0 ? 2 : 1,
      sample_rate: 48000,
      language: languages[index],
      title: `Track ${index + 1}`,
      pid: 0x101 + index,
    }));
    await mountPreviewPipe(page, audioTracks);

    const playBtn = page.locator("#video-player button", {
      hasText: "Play preview",
    });
    await expect(playBtn).toBeVisible();
    await playBtn.click();

    await page.waitForFunction(
      () =>
        Boolean(
          (
            window as typeof window & {
              __previewTestHls?: unknown;
            }
          ).__previewTestHls,
        ),
    );
    await page.evaluate(() => {
      const testWindow = window as typeof window & {
        Hls: { Events: Record<string, string> };
        __previewTestHls?: {
          emit: (event: string, data?: unknown) => void;
          audioTrack: number;
        };
      };
      testWindow.__previewTestHls?.emit(
        testWindow.Hls.Events.AUDIO_TRACKS_UPDATED,
        {
          audioTracks: Array.from({ length: 16 }, (_, index) => ({
            id: index,
            name: `Track ${index + 1}`,
            lang: `lang${index}`,
          })),
        },
      );
    });

    const audioPickerButton = page.locator(
      '#video-player button[aria-haspopup="listbox"]',
    );
    await expect(audioPickerButton).toBeVisible();
    await audioPickerButton.click();

    const track16Option = page
      .locator('[role="option"]')
      .filter({ hasText: "Track 16" })
      .first();
    await expect(track16Option).toBeVisible();
    await expect(track16Option).toContainText("PID 0x110");
    await expect(track16Option).not.toContainText(" / Track 16 / ");
    await expect(track16Option).not.toContainText(" / IND / Track 16");
    await track16Option.click();

    await expect(audioPickerButton).toHaveText("Audio: Track 16");

    const selectedTrack = await page.evaluate(() => {
      const testWindow = window as typeof window & {
        __previewTestHls?: { audioTrack: number };
      };
      return testWindow.__previewTestHls?.audioTrack ?? null;
    });
    expect(selectedTrack).toBe(15);
  });
});
