// Output URL parsing and preset matching for the destination editors.

export interface OutputServerPreset {
  label: string;
  value: string;
  rtmpMode?: "legacy" | "enhanced";
}

export const OUTPUT_SERVER_PRESETS: Record<string, OutputServerPreset[]> = {
  rtmp: [
    { label: "Custom", value: "" },
    {
      label: "YouTube",
      value: "rtmp://a.rtmp.youtube.com/live2/",
      rtmpMode: "enhanced",
    },
    {
      label: "YT Backup",
      value: "rtmp://b.rtmp.youtube.com/live2?backup=1/",
      rtmpMode: "enhanced",
    },
    { label: "Facebook", value: "rtmps://live-api-s.facebook.com:443/rtmp/" },
    { label: "Twitch", value: "rtmp://live.twitch.tv/app/" },
    {
      label: "Kick",
      value: "rtmps://fa723fc1b171.global-contribute.live-video.net/app/",
    },
    {
      label: "VDO Cipher",
      value: "rtmp://live-ingest-01.vd0.co:1935/livestream/",
    },
  ],
  hls: [
    { label: "Custom", value: "" },
    {
      label: "YouTube",
      value:
        "https://a.upload.youtube.com/http_upload_hls?cid=${stream_key}&copy=0&file=out.m3u8",
    },
    {
      label: "YT Backup",
      value:
        "https://b.upload.youtube.com/http_upload_hls?cid=${stream_key}&copy=1&file=out.m3u8",
    },
  ],
  srt: [{ label: "Custom", value: "" }],
};

function safeParseUrl(rawUrl: string): URL | null {
  try {
    return new URL(rawUrl);
  } catch {
    return null;
  }
}

function safeDecodeUrlComponent(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

export function protocolUsesOutputServerPresets(protocol: string): boolean {
  return protocol === "rtmp" || protocol === "hls";
}

export function resolvePresetOutputUrl(
  serverUrl: string,
  rawInput: string,
): string {
  const normalizedInput = String(rawInput || "").trim();
  if (!serverUrl) return normalizedInput;
  if (serverUrl.includes("${stream_key}")) {
    return serverUrl.replaceAll(
      "${stream_key}",
      encodeURIComponent(normalizedInput),
    );
  }
  return `${serverUrl}${normalizedInput}`;
}

export interface MatchedPreset {
  value: string;
  inputValue: string;
  rtmpMode?: "legacy" | "enhanced";
}

export function matchOutputServerPreset(
  protocol: string,
  rawUrl: string,
): MatchedPreset | null {
  const presets = OUTPUT_SERVER_PRESETS[protocol] || [];
  const candidateUrl = String(rawUrl || "").trim();
  if (!candidateUrl) return null;
  for (const preset of presets) {
    if (!preset.value) continue;
    if (preset.value.includes("${stream_key}")) {
      const [prefix, suffix] = preset.value.split("${stream_key}");
      if (candidateUrl.startsWith(prefix) && candidateUrl.endsWith(suffix)) {
        const captured = candidateUrl.slice(
          prefix.length,
          candidateUrl.length - suffix.length,
        );
        const matched: MatchedPreset = {
          value: preset.value,
          inputValue: safeDecodeUrlComponent(captured),
        };
        if (preset.rtmpMode) matched.rtmpMode = preset.rtmpMode;
        return matched;
      }
      continue;
    }
    if (candidateUrl.startsWith(preset.value)) {
      const matched: MatchedPreset = {
        value: preset.value,
        inputValue: candidateUrl.slice(preset.value.length),
      };
      if (preset.rtmpMode) matched.rtmpMode = preset.rtmpMode;
      return matched;
    }
  }
  return null;
}

export function isAbsoluteUrl(rawValue: string): boolean {
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(rawValue || "");
}

export function detectOutputProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) return "hls";
  if (/^srt:\/\//i.test(url)) return "srt";
  return "rtmp";
}

export function extractCandidateStreamToken(rawUrl: string): string {
  const parsed = safeParseUrl(rawUrl);
  if (parsed) {
    const streamKeyQuery = parsed.searchParams.get("cid");
    if (streamKeyQuery) return streamKeyQuery;

    const srtStreamId = parsed.searchParams.get("streamid");
    if (srtStreamId) {
      const normalized = srtStreamId.replace(/^publish:/, "");
      const segs = normalized.split("/").filter(Boolean);
      return segs.length > 0 ? segs[segs.length - 1] : srtStreamId;
    }

    const segments = parsed.pathname.split("/").filter(Boolean);
    if (/^https?:\/\//i.test(rawUrl)) {
      const last = segments.length > 0 ? segments[segments.length - 1] : "";
      if (/\.m3u8$/i.test(last)) {
        const stem = last.replace(/\.m3u8$/i, "");
        if (/^out$/i.test(stem) && segments.length > 1)
          return segments[segments.length - 2];
        return stem;
      }
    }
    return segments.length > 0 ? segments[segments.length - 1] : "";
  }

  const plain = String(rawUrl || "").trim();
  if (!plain) return "";
  const base = plain.split("?")[0].split("#")[0];
  const protocollessBase = base.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
  const segments = protocollessBase.split("/").filter(Boolean);
  const last = segments.length > 0 ? segments[segments.length - 1] : base;
  if (/\.m3u8$/i.test(last)) {
    const stem = last.replace(/\.m3u8$/i, "");
    if (/^out$/i.test(stem) && segments.length > 1)
      return segments[segments.length - 2];
    return stem;
  }
  return segments.length > 1 ? last : base;
}

export function getDefaultOutputToken(rawUrl: string): string {
  return extractCandidateStreamToken(rawUrl) || "test";
}

export interface SrtFields {
  host: string;
  port: string;
  streamId: string;
  passphrase: string;
  pbkeylen: string;
  extraQuery: string;
}

export function parseSrtFields(
  rawUrl: string,
  defaultHost = "localhost",
): SrtFields {
  const parsed = safeParseUrl(rawUrl);
  if (!parsed) {
    const token = getDefaultOutputToken(rawUrl);
    return {
      host: defaultHost,
      port: "6000",
      streamId: `publish:${token}`,
      passphrase: "",
      pbkeylen: "16",
      extraQuery: "",
    };
  }
  const isSrt = parsed.protocol === "srt:";
  const knownKeys = new Set(["streamid", "passphrase", "pbkeylen"]);
  // Other options stay query text as the URL has them (the field is
  // `&`-separated, so a decoded `&` in a value could not come back).
  const extraEntries = parsed.search
    .slice(1)
    .split("&")
    .filter((part) => part && !knownKeys.has(part.split("=")[0]));
  let streamId = parsed.searchParams.get("streamid") || "";
  if (!streamId && !isSrt)
    streamId = `publish:${getDefaultOutputToken(rawUrl)}`;
  return {
    host: parsed.hostname || defaultHost,
    port: isSrt ? parsed.port || "6000" : "6000",
    streamId,
    passphrase: isSrt ? parsed.searchParams.get("passphrase") || "" : "",
    pbkeylen: isSrt ? parsed.searchParams.get("pbkeylen") || "16" : "16",
    extraQuery: isSrt ? extraEntries.join("&") : "",
  };
}

export function buildDefaultCustomOutputUrl(
  protocol: string,
  rawSeed = "",
  hostname = "localhost",
): string {
  const token = getDefaultOutputToken(rawSeed);
  if (protocol === "hls") return `http://${hostname}/hls/${token}/out.m3u8`;
  if (protocol === "srt")
    return `srt://${hostname}:6000?streamid=publish:${token}`;
  return `rtmp://${hostname}:1935/live/${token}`;
}

/** The SRT URL the editor's fields describe; empty without a host. */
export function buildSrtUrl(fields: SrtFields): string {
  const host = fields.host.trim();
  if (!host) return "";
  const values: Array<[string, string]> = [];
  const streamId = fields.streamId.trim();
  if (streamId) values.push(["streamid", streamId]);
  const passphrase = fields.passphrase.trim();
  if (passphrase) {
    values.push(["passphrase", passphrase]);
    values.push(["pbkeylen", fields.pbkeylen === "24" || fields.pbkeylen === "32" ? fields.pbkeylen : "16"]);
  }
  // Field values are decoded text (`parseSrtFields`): encode `#`, `&`, `=`,
  // `%` and spaces, keep `:`, `,` and `/` readable.
  const query = values.map(
    ([key, value]) =>
      `${key}=${encodeURIComponent(value).replace(/%3A/gi, ":").replace(/%2C/gi, ",").replace(/%2F/gi, "/")}`,
  );
  // Other options are query text already (see `parseSrtFields`).
  for (const segment of fields.extraQuery.split("&")) {
    const part = segment.trim();
    if (part) query.push(part);
  }
  const port = fields.port.trim() || "6000";
  return `srt://${host}:${port}${query.length ? `?${query.join("&")}` : ""}`;
}

/** The URL an output editor describes: SRT from its fields; otherwise a full
 * URL as typed, or the preset server joined with the stream key. */
export function effectiveOutputUrl(
  protocol: string,
  serverUrl: string,
  rawInput: string,
  srt: SrtFields,
): string {
  if (protocol === "srt") return buildSrtUrl(srt);
  const input = rawInput.trim();
  return isAbsoluteUrl(input) ? input : resolvePresetOutputUrl(serverUrl, input);
}
