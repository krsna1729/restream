import { fileURLToPath } from "node:url";

import { defineConfig } from "vite";

export default defineConfig({
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  publicDir: false,
  build: {
    emptyOutDir: false,
    lib: {
      entry: fileURLToPath(
        new URL("./web/ts/app/dashboard-v2-entry.tsx", import.meta.url),
      ),
      fileName: () => "dashboard-v2-entry.js",
      formats: ["es"],
    },
    minify: "oxc",
    outDir: "public/js/app",
    target: "es2021",
  },
});
