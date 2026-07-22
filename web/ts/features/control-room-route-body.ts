import {
  renderControlRoom,
  setControlRoomContainerId,
} from "./control-room/index.js";

export function renderDashboardV2ControlRoomBody(containerId: string): void {
  setControlRoomContainerId(containerId);
  renderControlRoom();
  const container = document.getElementById(containerId);
  if (container) container.dataset.controlRoomRouteBody = "v2";
}
