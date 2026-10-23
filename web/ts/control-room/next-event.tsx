// "Next: Go live · Sunday service · Tomorrow 10:00 BST · in 17 h": the
// soonest scheduled event, for one pipeline or the whole plant, linking to
// the Schedule. Quiet when nothing is scheduled.

import { listSchedules } from "../core/api.js";
import { Icon } from "../ui/icons.js";
import { usePolled } from "./polled.js";
import { PlaceLink } from "./router.js";
import { ACTION_LABEL, nextUp } from "./schedule-model.js";
import { When } from "./when-view.js";

export function NextEvent(props: { pipelineId: string | null }) {
  const schedules = usePolled(["schedules"], listSchedules, 60_000);
  const next = schedules.value ? nextUp(schedules.value, props.pipelineId) : null;
  if (!next || next.nextAtMs === null) return null;
  return (
    <PlaceLink
      place={{ workspace: "schedule", pipelineId: props.pipelineId }}
      className="inline-flex max-w-full items-center gap-2 self-start rounded-full border border-base-content/15 bg-base-100 px-3 py-1 text-sm hover:border-primary/50"
    >
      <Icon name="schedule" className="size-4 shrink-0 text-primary" />
      <span className="truncate">
        <span className="font-medium">Next: {ACTION_LABEL[next.action]}</span>
        <span className="text-base-content/70"> · {next.name} · </span>
        <When atMs={next.nextAtMs} show="both" />
      </span>
    </PlaceLink>
  );
}
