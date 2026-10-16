import {
  QueryCache,
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { router } from "./router.js";

const container = document.getElementById("root");
if (!container) throw new Error("control room: #root is missing");

// Polled panels read through TanStack Query: one cache shared by every
// workspace, the last value kept while a load fails, no retry of its own (the
// panel's cadence is the retry), and a load when the tab comes back.
const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error) => console.error("control room poll failed", error),
  }),
  defaultOptions: {
    queries: {
      retry: false,
      refetchOnWindowFocus: true,
      refetchOnReconnect: false,
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
