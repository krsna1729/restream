// Guides: everyday tasks step by step. Each step links to its page, and
// "Show me" opens that page with its spotlight tour.

import { useState } from "react";

import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { GUIDES, GUIDE_TOPICS, canFollow } from "./guides-model.js";
import type { Guide } from "./guides-model.js";
import { PAGE_HELP } from "./help-model.js";
import { WORKSPACE_LABELS } from "./route.js";
import type { Workspace } from "./route.js";
import { PlaceLink, usePlace } from "./router.js";
import { useSession } from "./session.js";
import { useTour } from "./shell/help.js";
import { ROLE_LABEL } from "./team-model.js";

export function Guides() {
  const { role } = useSession();
  const tour = useTour();
  const [topic, setTopic] = useState<Guide["topic"] | "all">("all");
  const shown = GUIDES.filter((guide) => topic === "all" || guide.topic === topic);
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Learn · Guides"
        title="How to get things done"
        subtitle={`${GUIDES.length} task guides. Each step links to the page it happens on; Show me lights that page up.`}
        actions={
          <button type="button" className="btn btn-primary btn-sm" onClick={tour.startOnboarding}>
            Take the tour
          </button>
        }
      />
      <div className="flex flex-wrap gap-2" role="group" aria-label="Guide topics">
        {(["all", ...GUIDE_TOPICS] as const).map((value) => (
          <button
            key={value}
            type="button"
            aria-pressed={topic === value}
            className={`btn btn-sm rounded-full ${topic === value ? "btn-primary" : "btn-outline border-base-content/20"}`}
            onClick={() => setTopic(value)}
          >
            {value === "all" ? "All" : value}
          </button>
        ))}
      </div>
      <div className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-3">
        {shown.map((guide) => (
          <GuideCard key={guide.id} guide={guide} followable={canFollow(role, guide)} />
        ))}
      </div>
    </div>
  );
}

function GuideCard(props: { guide: Guide; followable: boolean }) {
  const place = usePlace();
  const tour = useTour();
  const first = props.guide.steps.find((step) => step.workspace)?.workspace ?? null;
  const showMe = (workspace: Workspace) =>
    tour.start(PAGE_HELP[workspace].tour.map((step) => ({ ...step, workspace })));
  return (
    <Panel
      id={`guide-${props.guide.id}`}
      title={props.guide.title}
      subtitle={props.guide.topic}
      actions={
        props.guide.role !== "viewer" && (
          <span className={`badge badge-sm ${props.followable ? "badge-ghost" : "badge-warning"}`} title={props.followable ? undefined : "Your role cannot do every step"}>
            {ROLE_LABEL[props.guide.role]}
            {props.followable ? "" : " needed"}
          </span>
        )
      }
    >
      <p className="mb-3 text-sm text-base-content/80">{props.guide.summary}</p>
      <ol className="flex list-none flex-col gap-2 text-sm">
        {props.guide.steps.map((step, index) => (
          <li key={step.text} className="grid grid-cols-[1.75rem_minmax(0,1fr)] gap-2">
            <span aria-hidden="true" className="tabular grid size-6 place-items-center rounded-full bg-primary/15 text-xs font-semibold text-primary">
              {index + 1}
            </span>
            <span>
              {step.text}
              {step.workspace && (
                <>
                  {" "}
                  <PlaceLink place={{ workspace: step.workspace, pipelineId: place.pipelineId }} className="link link-primary whitespace-nowrap">
                    Open {WORKSPACE_LABELS[step.workspace]}
                  </PlaceLink>
                </>
              )}
            </span>
          </li>
        ))}
      </ol>
      {first && (
        <div className="mt-4 flex justify-end">
          <button type="button" className="btn btn-outline btn-sm" onClick={() => showMe(first)}>
            Show me on {WORKSPACE_LABELS[first]}
          </button>
        </div>
      )}
    </Panel>
  );
}
