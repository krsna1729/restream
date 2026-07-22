import type { AppLogRow } from "../../types.js";
import type { HistoryEventClassification } from "./utils.js";
import {
  getNormalizedEventType,
  getEventData,
  getPipelineInputState,
  inferIntentionalStop,
  getTargetAndLevel,
} from "./utils.js";

export function getPipelineSemanticKind(log: AppLogRow): string {
  const eventType = getNormalizedEventType(log);
  const message = String(log?.message || "");
  const level = String(log?.level || "").toUpperCase();
  const target = String(log?.target || "");
  const inputState = getPipelineInputState(log);

  if (
    eventType === "pipeline.config.created" ||
    eventType.startsWith("pipeline.config.") ||
    message.startsWith("[config]")
  ) {
    return "config";
  }
  if (eventType === "ingest.connected" || inputState === "on") {
    return "ingest_on";
  }
  if (
    eventType === "ingest.disconnected" ||
    inputState === "off" ||
    eventType === "pipeline.input_state.reset"
  ) {
    return "ingest_off";
  }
  if (inputState === "warning") return "input_warning";
  if (inputState === "error") return "input_error";
  if (eventType === "stage.started") return "stage_start";
  if (eventType === "stage.stopped") return "stage_stop";
  if (eventType === "egress.started" || eventType === "lifecycle.start") {
    return "output_start";
  }
  if (eventType === "egress.stopped" || eventType === "lifecycle.stop") {
    return "output_stop";
  }
  if (eventType === "egress.failed") return "output_fail";
  if (
    target.includes("external_transcoder") &&
    (level === "WARN" ||
      level === "ERROR" ||
      message.includes("ffmpeg stderr") ||
      message.includes("failed to spawn ffmpeg") ||
      message.includes("stdin write failed"))
  ) {
    return "stage_fault";
  }
  return "generic";
}

export function classifyHistoryEvent(
  log: AppLogRow,
  logs: AppLogRow[] = [],
  index = -1,
): HistoryEventClassification {
  const eventType = getNormalizedEventType(log);
  const eventData = getEventData(log);

  if (eventType === "lifecycle.desired_state_changed") {
    const desiredRunning = eventData?.state === "running";
    return {
      type: "desired_state",
      label: desiredRunning ? "Start requested" : "Stop requested",
      badgeClass: desiredRunning ? "badge-info" : "badge-warning",
    };
  }
  if (eventType === "lifecycle.started") {
    return { type: "started", label: "Started", badgeClass: "badge-success" };
  }
  if (eventType === "egress.started") {
    return {
      type: "started",
      label: "Egress Started",
      badgeClass: "badge-success",
    };
  }
  if (eventType === "egress.stopped") {
    return {
      type: "stopped",
      label: "Egress Stopped",
      badgeClass: "badge-stopped",
    };
  }
  if (eventType === "egress.failed") {
    return {
      type: "failed",
      label: "Egress Failed",
      badgeClass: "badge-error",
    };
  }
  if (eventType === "lifecycle.start") {
    return {
      type: "starting",
      label: "Start issued",
      badgeClass: "badge-info",
    };
  }
  if (eventType === "lifecycle.stop") {
    const message = String(log?.message || "");
    if (/ingest is no longer active/i.test(message)) {
      return {
        type: "stopping",
        label: "Stopped for input loss",
        badgeClass: "badge-warning",
      };
    }
    return {
      type: "stopping",
      label: "Stop issued",
      badgeClass: "badge-stopped",
    };
  }
  if (eventType === "lifecycle.stop_requested") {
    return {
      type: "stopping",
      label: "Stop requested",
      badgeClass: "badge-warning",
    };
  }
  if (eventType === "lifecycle.auto_start_suppressed") {
    return {
      type: "suppressed",
      label: "Auto-start skipped",
      badgeClass: "badge-info",
    };
  }
  if (eventType === "lifecycle.failed_on_error") {
    return { type: "failed", label: "Failed", badgeClass: "badge-error" };
  }
  if (eventType === "lifecycle.retry_decision") {
    if (
      eventData?.scheduled === false &&
      eventData?.reason === "desired_state_stopped"
    ) {
      return {
        type: "retry_suppressed",
        label: "Retry skipped",
        badgeClass: "badge-info",
      };
    }
    if (eventData?.scheduled === false) {
      return {
        type: "retry_update",
        label: "Retry not scheduled",
        badgeClass: "badge-ghost",
      };
    }
    return {
      type: "retry_update",
      label: "Retry queued",
      badgeClass: "badge-warning",
    };
  }
  if (eventType === "lifecycle.retry_suppressed") {
    return {
      type: "retry_suppressed",
      label: "Retry skipped",
      badgeClass: "badge-info",
    };
  }
  if (eventType === "lifecycle.retry_exhausted") {
    return {
      type: "retry_exhausted",
      label: "Retry exhausted",
      badgeClass: "badge-error",
    };
  }
  if (eventType === "lifecycle.marked_stopped_no_process") {
    return { type: "stopped", label: "Stopped", badgeClass: "badge-stopped" };
  }
  if (eventType === "lifecycle.config_created") {
    return {
      type: "config",
      label: "Config Created",
      badgeClass: "badge-secondary",
    };
  }
  if (eventType === "lifecycle.config_changed") {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (eventType.startsWith("lifecycle.config_")) {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (eventType === "lifecycle.exited") {
    const failed = eventData?.status === "failed";
    const requestedStop =
      typeof eventData?.requestedStop === "boolean"
        ? eventData.requestedStop
        : inferIntentionalStop(logs, index);
    return {
      type: failed && !requestedStop ? "failed" : "stopped",
      label:
        failed && requestedStop
          ? "Stopped"
          : failed
            ? "Exited (failed)"
            : "Exited",
      badgeClass: failed && !requestedStop ? "badge-error" : "badge-stopped",
    };
  }
  if (eventType === "output.exit") {
    return { type: "log", label: "Log", badgeClass: "badge-ghost" };
  }

  const message = String(log?.message || "");

  if (message.startsWith("[lifecycle] desired_state")) {
    const desiredRunning = /state=running/.test(message);
    return {
      type: "desired_state",
      label: desiredRunning ? "Start requested" : "Stop requested",
      badgeClass: desiredRunning ? "badge-info" : "badge-warning",
    };
  }
  if (message.startsWith("[lifecycle] started")) {
    return { type: "started", label: "Started", badgeClass: "badge-success" };
  }
  if (message.startsWith("[lifecycle] stop_requested")) {
    return {
      type: "stopping",
      label: "Stop requested",
      badgeClass: "badge-warning",
    };
  }
  if (message.startsWith("[lifecycle] auto_start_suppressed")) {
    return {
      type: "suppressed",
      label: "Auto-start skipped",
      badgeClass: "badge-info",
    };
  }
  if (message.startsWith("[lifecycle] failed_on_error")) {
    return { type: "failed", label: "Failed", badgeClass: "badge-error" };
  }
  if (message.startsWith("[lifecycle] retry_decision")) {
    if (
      /scheduled=false/.test(message) &&
      /reason=desired_state_stopped/.test(message)
    ) {
      return {
        type: "retry_suppressed",
        label: "Retry skipped",
        badgeClass: "badge-info",
      };
    }
    if (/scheduled=false/.test(message)) {
      return {
        type: "retry_update",
        label: "Retry not scheduled",
        badgeClass: "badge-ghost",
      };
    }
    return {
      type: "retry_update",
      label: "Retry queued",
      badgeClass: "badge-warning",
    };
  }
  if (message.startsWith("[lifecycle] retry_exhausted")) {
    return {
      type: "retry_exhausted",
      label: "Retry exhausted",
      badgeClass: "badge-error",
    };
  }
  if (message.startsWith("[lifecycle] marked_stopped_no_process")) {
    return { type: "stopped", label: "Stopped", badgeClass: "badge-stopped" };
  }
  if (message.startsWith("[lifecycle] config_created")) {
    return {
      type: "config",
      label: "Config Created",
      badgeClass: "badge-secondary",
    };
  }
  if (message.startsWith("[lifecycle] config_changed")) {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (message.startsWith("[lifecycle] config_")) {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (message.startsWith("[lifecycle] exited")) {
    const failed = /status=failed/.test(message);
    const requestedStop = inferIntentionalStop(logs, index);
    return {
      type: failed && !requestedStop ? "failed" : "stopped",
      label:
        failed && requestedStop
          ? "Stopped"
          : failed
            ? "Exited (failed)"
            : "Exited",
      badgeClass: failed && !requestedStop ? "badge-error" : "badge-stopped",
    };
  }
  if (message.startsWith("[exit]")) {
    return { type: "log", label: "Log", badgeClass: "badge-ghost" };
  }

  return { type: "log", label: "Log", badgeClass: "badge-ghost" };
}

export function classifyPipelineHistoryEvent(
  log: AppLogRow,
): HistoryEventClassification {
  const eventType = getNormalizedEventType(log);
  const eventData = getEventData(log);

  if (eventType === "pipeline.config.created") {
    return {
      type: "config",
      label: "Config Created",
      badgeClass: "badge-secondary",
    };
  }
  if (eventType.startsWith("pipeline.config.")) {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (eventType === "pipeline.input_state.initialized") {
    const finalState = String(eventData?.state || "").toLowerCase();
    if (finalState === "on")
      return { type: "on", label: "Input On", badgeClass: "badge-success" };
    if (finalState === "warning")
      return {
        type: "warning",
        label: "Input Warning",
        badgeClass: "badge-warning",
      };
    if (finalState === "error")
      return { type: "error", label: "Input Error", badgeClass: "badge-error" };
    if (finalState === "off")
      return { type: "off", label: "Input Off", badgeClass: "badge-stopped" };
  }
  if (eventType === "pipeline.input_state.transitioned") {
    const finalState = String(eventData?.to || "").toLowerCase();
    if (finalState === "on")
      return { type: "on", label: "Input On", badgeClass: "badge-success" };
    if (finalState === "warning")
      return {
        type: "warning",
        label: "Input Warning",
        badgeClass: "badge-warning",
      };
    if (finalState === "error")
      return { type: "error", label: "Input Error", badgeClass: "badge-error" };
    if (finalState === "off")
      return { type: "off", label: "Input Off", badgeClass: "badge-stopped" };
  }
  if (eventType === "pipeline.input_state.reset") {
    return { type: "reset", label: "Input Reset", badgeClass: "badge-info" };
  }
  if (eventType === "ingest.connected") {
    return {
      type: "on",
      label: "Ingest Connected",
      badgeClass: "badge-success",
    };
  }
  if (eventType === "ingest.disconnected") {
    return {
      type: "off",
      label: "Ingest Disconnected",
      badgeClass: "badge-stopped",
    };
  }
  if (eventType === "stage.started") {
    return { type: "stage", label: "Stage Started", badgeClass: "badge-info" };
  }
  if (eventType === "stage.stopped") {
    return {
      type: "stage",
      label: "Stage Stopped",
      badgeClass: "badge-ghost",
    };
  }
  if (eventType === "egress.started") {
    return {
      type: "egress",
      label: "Output Started",
      badgeClass: "badge-success",
    };
  }
  if (eventType === "egress.stopped") {
    return {
      type: "egress",
      label: "Output Stopped",
      badgeClass: "badge-stopped",
    };
  }
  if (eventType === "egress.failed") {
    return {
      type: "egress",
      label: "Output Failed",
      badgeClass: "badge-error",
    };
  }
  if (eventType === "lifecycle.start") {
    return {
      type: "egress",
      label: "Output Start Issued",
      badgeClass: "badge-info",
    };
  }
  if (eventType === "lifecycle.stop") {
    const msg = String(log?.message || "");
    return {
      type: "egress",
      label: /ingest is no longer active/i.test(msg)
        ? "Output Stop for Input Loss"
        : "Output Stop Issued",
      badgeClass: /ingest is no longer active/i.test(msg)
        ? "badge-warning"
        : "badge-stopped",
    };
  }

  const { target, level } = getTargetAndLevel(log);
  const message = String(log?.message || "");

  if (target.includes("external_transcoder")) {
    if (message.includes("failed to spawn ffmpeg")) {
      return {
        type: "ffmpeg",
        label: "FFmpeg Spawn Failed",
        badgeClass: "badge-error",
      };
    }
    if (message.includes("stdin write failed")) {
      return {
        type: "ffmpeg",
        label: "FFmpeg Pipe Failed",
        badgeClass: "badge-error",
      };
    }
    if (message.includes("ffmpeg stderr")) {
      return {
        type: "ffmpeg",
        label: "FFmpeg stderr",
        badgeClass: level === "ERROR" ? "badge-error" : "badge-warning",
      };
    }
    return {
      type: "ffmpeg",
      label: "External Stage",
      badgeClass: level === "ERROR" ? "badge-error" : "badge-info",
    };
  }

  if (message.startsWith("[config] created")) {
    return {
      type: "config",
      label: "Config Created",
      badgeClass: "badge-secondary",
    };
  }
  if (message.startsWith("[config]")) {
    return {
      type: "config",
      label: "Config Updated",
      badgeClass: "badge-secondary",
    };
  }
  if (message.startsWith("[input_state]")) {
    let finalState = "";
    if (message.includes("->")) {
      finalState = message.split("->").pop()!.trim().toLowerCase();
    } else {
      const match = message.match(/initial_state\s*=\s*([a-z_]+)/i);
      finalState = (match && match[1] ? match[1] : "").toLowerCase();
    }

    if (finalState === "on")
      return { type: "on", label: "Input On", badgeClass: "badge-success" };
    if (finalState === "warning")
      return {
        type: "warning",
        label: "Input Warning",
        badgeClass: "badge-warning",
      };
    if (finalState === "error")
      return { type: "error", label: "Input Error", badgeClass: "badge-error" };
    if (finalState === "off")
      return { type: "off", label: "Input Off", badgeClass: "badge-stopped" };
  }

  return { type: "log", label: "Event", badgeClass: "badge-ghost" };
}

export function getPipelineTimelineLogs(logs: AppLogRow[]): AppLogRow[] {
  const items = Array.isArray(logs) ? logs : [];
  return items.filter((log) => {
    const eventType = getNormalizedEventType(log);
    if (
      eventType.startsWith("pipeline.config.") ||
      eventType.startsWith("pipeline.input_state.") ||
      eventType.startsWith("ingest.") ||
      eventType.startsWith("stage.") ||
      eventType.startsWith("egress.") ||
      eventType === "lifecycle.start" ||
      eventType === "lifecycle.stop"
    ) {
      return true;
    }
    const target = String(log?.target || "");
    const level = String(log?.level || "").toUpperCase();
    const message = String(log?.message || "");
    if (target.includes("external_transcoder")) {
      return (
        level === "WARN" ||
        level === "ERROR" ||
        message.includes("ffmpeg stderr") ||
        message.includes("failed to spawn ffmpeg") ||
        message.includes("stdin write failed")
      );
    }
    return (
      message.startsWith("[config]") || message.startsWith("[input_state]")
    );
  });
}
