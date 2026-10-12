import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app.js";

const container = document.getElementById("root");
if (!container) throw new Error("control room: #root is missing");
createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
