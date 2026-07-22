import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  installFakeDom,
  loadCompiledFrontendModule,
} from "./dashboard-contract/helpers.mjs";

test("renderDashboardV2PipelineInspectBody owns the inspect route body", async () => {
  const { document } = installFakeDom();
  const container = appendRoot(
    document,
    "div",
    "dashboard-v2-pipeline-inspect-content",
  );
  const routeBody = await loadCompiledFrontendModule(
    "features/pipeline-inspect-route-body.js",
  );
  const inspector = await loadCompiledFrontendModule(
    "features/pipeline-inspector/index.js",
  );
  inspector.configurePipelineInspectCheckpointPresentation({ v2Active: true });

  routeBody.renderDashboardV2PipelineInspectBody(container.id);

  assert.equal(container.dataset.pipelineInspectRouteBody, "v2");
  assert.match(container.innerHTML, /aria-label="Inspect pipeline"/);
  assert.match(container.innerHTML, /Graph Explorer/);
  assert.doesNotMatch(container.innerHTML, /Pipeline inspect/);
  assert.doesNotMatch(container.innerHTML, /Operate selected pipeline/);
  assert.doesNotMatch(container.innerHTML, /Run Diagnostics/);
  assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
});

test("renderDashboardV2ControlRoomBody owns the monitor route body", async () => {
  const { document } = installFakeDom();
  const container = appendRoot(
    document,
    "div",
    "dashboard-v2-control-room-content",
  );
  const routeBody = await loadCompiledFrontendModule(
    "features/control-room-route-body.js",
  );

  routeBody.renderDashboardV2ControlRoomBody(container.id);

  assert.equal(container.dataset.controlRoomRouteBody, "v2");
  assert.match(container.innerHTML, /Control Room/);
  assert.match(container.innerHTML, /aria-label="Filter monitor by pipeline"/);
  assert.match(container.innerHTML, /Monitor previews/);
  assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
});
