// What this browser can decode, for choosing a preview: an H.265 source is
// served as it is to a browser that decodes H.265 (no FFmpeg on the
// server), and as the engine's H.264 copy to one that does not.

/** H.265 Main, the profile most encoders send. */
export const HEVC_MAIN = "hvc1.1.6.L93.B0";

export function isHevc(codec: string | null | undefined): boolean {
  const name = (codec ?? "").toLowerCase();
  return name === "hevc" || name === "h265" || name === "h.265";
}

/** The parts of the browser the HLS check reads; `globalThis` in the app. */
export interface HlsDecodeEnv {
  MediaSource?: { isTypeSupported(type: string): boolean };
  ManagedMediaSource?: { isTypeSupported(type: string): boolean };
  document?: {
    createElement(tag: "video"): { canPlayType(type: string): string };
  };
}

/** Whether HLS in this browser plays fMP4 H.265: through Media Source
 * Extensions (hls.js; Managed on iOS), or natively (Safari). */
export function hlsDecodesHevc(
  env: HlsDecodeEnv = globalThis as HlsDecodeEnv,
): boolean {
  const type = `video/mp4; codecs="${HEVC_MAIN}"`;
  try {
    if (
      env.MediaSource?.isTypeSupported(type) ||
      env.ManagedMediaSource?.isTypeSupported(type)
    )
      return true;
    return (
      (env.document?.createElement("video").canPlayType(type) ?? "") !== ""
    );
  } catch {
    return false;
  }
}

/** The program preview to request: the passthrough one for an H.265 source
 * this browser decodes, with the H.264 copy to fall back to. */
export function programPreviewChoice(
  urls: { source: string; h264: string },
  sourceCodec: string | null | undefined,
  decodesHevc: boolean,
): { url: string; fallbackUrl: string | null } {
  if (isHevc(sourceCodec) && decodesHevc)
    return { url: urls.source, fallbackUrl: urls.h264 };
  return { url: urls.h264, fallbackUrl: null };
}

/** The parts of WebCodecs the MoQ check reads. */
export interface MoqDecodeEnv {
  VideoDecoder?: {
    isConfigSupported(config: {
      codec: string;
    }): Promise<{ supported?: boolean }>;
  };
}

/** Why a MoQ preview cannot play this source here, or `null` if it can (or
 * the codec is not one the check knows). MoQ sends the source as it is. */
export async function moqUndecodableReason(
  sourceCodec: string | null | undefined,
  env: MoqDecodeEnv = globalThis as MoqDecodeEnv,
): Promise<string | null> {
  if (!isHevc(sourceCodec)) return null;
  try {
    const result = await env.VideoDecoder?.isConfigSupported({
      codec: HEVC_MAIN,
    });
    if (result?.supported) return null;
  } catch {
    // Unknown codec string: treat as unsupported.
  }
  return "this browser cannot decode H.265 (it needs a hardware decoder here): use HLS, which sends an H.264 copy";
}
