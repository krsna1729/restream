// A platform's mark: its colour and a simple glyph (a play button for
// YouTube, a camera outline for Instagram), so a list of channels reads at
// a glance. Simplified shapes, not the brands' artwork; custom destinations
// show their protocol's monogram on a neutral tile.

import type { Platform } from "./channels-model.js";

const BRAND: Record<string, { bg: string; fg: string }> = {
  youtube: { bg: "#ff0033", fg: "#ffffff" },
  twitch: { bg: "#9146ff", fg: "#ffffff" },
  kick: { bg: "#53fc18", fg: "#0b0e0f" },
  facebook: { bg: "#0866ff", fg: "#ffffff" },
  x: { bg: "#000000", fg: "#ffffff" },
  linkedin: { bg: "#0a66c2", fg: "#ffffff" },
  instagram: { bg: "#d62976", fg: "#ffffff" },
  vimeo: { bg: "#1ab7ea", fg: "#ffffff" },
};

function Glyph(props: { id: string; fg: string }) {
  const { fg } = props;
  switch (props.id) {
    case "youtube":
      return <path d="M10 8.5v7l6-3.5z" fill={fg} />;
    case "twitch":
      return (
        <g fill="none" stroke={fg} strokeWidth={1.8} strokeLinejoin="round">
          <path d="M6.5 5.5h11v8l-3 3h-3l-2 2v-2h-3z" />
          <path d="M11 9v3M14.5 9v3" strokeLinecap="round" />
        </g>
      );
    case "instagram":
      return (
        <g fill="none" stroke={fg} strokeWidth={1.8}>
          <rect x={6} y={6} width={12} height={12} rx={3.5} />
          <circle cx={12} cy={12} r={2.8} />
          <circle cx={15.6} cy={8.4} r={0.6} fill={fg} stroke="none" />
        </g>
      );
    default: {
      const letter = { kick: "K", facebook: "f", x: "X", linkedin: "in", vimeo: "v" }[props.id] ?? "";
      return (
        <text x={12} y={12} dy="0.36em" textAnchor="middle" fill={fg} fontSize={letter.length > 1 ? 9.5 : 12} fontWeight={800} fontFamily="ui-sans-serif, system-ui, sans-serif">
          {letter}
        </text>
      );
    }
  }
}

/** The platform's tile; `size` in px. Decorative: the name sits beside it. */
export function PlatformMark(props: { platform: Platform; size?: number; className?: string }) {
  const size = props.size ?? 40;
  const brand = BRAND[props.platform.id];
  if (!brand) {
    return (
      <span
        aria-hidden="true"
        style={{ width: size, height: size, fontSize: size * 0.34 }}
        className={`grid shrink-0 place-items-center rounded-field bg-base-content/10 font-bold text-base-content/80 ${props.className ?? ""}`}
      >
        {props.platform.monogram}
      </span>
    );
  }
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" width={size} height={size} className={`shrink-0 ${props.className ?? ""}`}>
      <rect width={24} height={24} rx={5} fill={brand.bg} />
      <Glyph id={props.platform.id} fg={brand.fg} />
    </svg>
  );
}
