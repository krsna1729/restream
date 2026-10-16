import {
  QueryCache,
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app.js";
import { NoValue } from "./polled.js";
import { createControlRoomRouter } from "./router.js";

const container = document.getElementById("root");
if (!container) throw new Error("control room: #root is missing");

const router = createControlRoomRouter(App);

// Polled panels read through TanStack Query: one cache shared by every
// workspace, the last value kept while a load fails, no retry of its own (the
// panel's cadence is the retry), polling that keeps its cadence in a hidden
// tab, and no gate on the browser's own online flag (the engine is usually
// this host).
const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error) => {
      if (error instanceof NoValue) return;
      console.error("control room poll failed", error);
    },
  }),
  defaultOptions: {
    queries: {
      retry: false,
      refetchIntervalInBackground: true,
      refetchOnWindowFocus: true,
      refetchOnReconnect: false,
      networkMode: "always",
    },
  },
});

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
