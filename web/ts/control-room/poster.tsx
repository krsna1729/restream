// A media file's poster: the engine's still of it, loaded lazily and
// cached by the browser; a quiet tile where the file has no picture.

import { useState } from "react";

import { mediaPosterUrl } from "../core/api.js";
import { Icon } from "../ui/icons.js";

export function Poster(props: { name: string; className?: string }) {
  const [failed, setFailed] = useState(false);
  const shape = `aspect-video shrink-0 overflow-hidden rounded-field bg-base-content/10 ${props.className ?? ""}`;
  if (failed) {
    return (
      <span aria-hidden="true" className={`grid place-items-center text-base-content/40 ${shape}`}>
        <Icon name="media" className="size-4" />
      </span>
    );
  }
  return <img src={mediaPosterUrl(props.name)} alt="" loading="lazy" decoding="async" className={`object-cover ${shape}`} onError={() => setFailed(true)} />;
}
