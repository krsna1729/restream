// An encoder's mark: its colour and a short monogram (or, for OBS, a
// simple three-dot glyph), so the source reads at a glance beside its
// name. Simplified shapes, not the products' artwork; an unknown encoder
// shows a neutral tile with a generic encoder glyph.

import type { EncoderIdentity } from "./encoder-model.js";

const BRAND: Record<string, { bg: string; fg: string; mono: string }> = {
  obs: { bg: "#302e31", fg: "#ffffff", mono: "" },
  streamlabs: { bg: "#80f5d2", fg: "#09161d", mono: "SL" },
  ffmpeg: { bg: "#0b7a0b", fg: "#ffffff", mono: "ff" },
  vmix: { bg: "#111827", fg: "#38bdf8", mono: "vM" },
  wirecast: { bg: "#1e40af", fg: "#ffffff", mono: "W" },
  xsplit: { bg: "#0f3b6f", fg: "#ffffff", mono: "X" },
  larix: { bg: "#d61f26", fg: "#ffffff", mono: "L" },
  prism: { bg: "#5b21b6", fg: "#ffffff", mono: "P" },
  gstreamer: { bg: "#b91c1c", fg: "#ffffff", mono: "Gst" },
  blackmagic: { bg: "#1f1f1f", fg: "#f97316", mono: "BM" },
  elgato: { bg: "#1d1d1b", fg: "#ffffff", mono: "E" },
  zoom: { bg: "#0b5cff", fg: "#ffffff", mono: "Z" },
  teradek: { bg: "#0f172a", fg: "#f59e0b", mono: "T" },
  haivision: { bg: "#004b87", fg: "#ffffff", mono: "H" },
  elemental: { bg: "#232f3e", fg: "#ff9900", mono: "AWS" },
  streamyard: { bg: "#0062ff", fg: "#ffffff", mono: "SY" },
};

/** The encoder's tile; `size` in px. Decorative: the name sits beside it. */
export function EncoderMark(props: {
  identity: EncoderIdentity;
  size?: number;
  className?: string;
}) {
  const size = props.size ?? 32;
  const brand = BRAND[props.identity.id];
  if (!brand) {
    return (
      <svg
        aria-hidden="true"
        viewBox="0 0 24 24"
        width={size}
        height={size}
        className={`shrink-0 text-base-content/70 ${props.className ?? ""}`}
      >
        <rect width={24} height={24} rx={5} className="fill-base-content/10" />
        <g
          fill="none"
          stroke="currentColor"
          strokeWidth={1.6}
          strokeLinejoin="round"
        >
          <rect x={5} y={7} width={10} height={10} rx={1.5} />
          <path d="M15 10.5l4-2.5v8l-4-2.5z" />
        </g>
      </svg>
    );
  }
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={`shrink-0 ${props.className ?? ""}`}
    >
      <rect width={24} height={24} rx={5} fill={brand.bg} />
      {props.identity.id === "obs" ? (
        <g fill="none" stroke={brand.fg} strokeWidth={1.4}>
          <circle cx={12} cy={12} r={7} />
          <circle cx={12} cy={8.6} r={2} fill={brand.fg} />
          <circle cx={9} cy={13.8} r={2} fill={brand.fg} />
          <circle cx={15} cy={13.8} r={2} fill={brand.fg} />
        </g>
      ) : (
        <text
          x={12}
          y={12}
          dy="0.36em"
          textAnchor="middle"
          fill={brand.fg}
          fontSize={
            brand.mono.length > 2 ? 7.5 : brand.mono.length > 1 ? 9.5 : 12
          }
          fontWeight={800}
          fontFamily="ui-sans-serif, system-ui, sans-serif"
        >
          {brand.mono}
        </text>
      )}
    </svg>
  );
}
