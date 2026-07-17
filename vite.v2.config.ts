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
      entry: {
        "dashboard-v2-checkpoints-entry": fileURLToPath(
          new URL(
            "./web/ts/app/dashboard-v2-checkpoints-entry.tsx",
            import.meta.url,
          ),
        ),
        "dashboard-v2-entry": fileURLToPath(
          new URL("./web/ts/app/dashboard-v2-entry.tsx", import.meta.url),
        ),
      },
      fileName: (_format, entryName) => `${entryName}.js`,
      formats: ["es"],
    },
    minify: "oxc",
    outDir: "public/js/app",
    rollupOptions: {
      output: {
        chunkFileNames: "dashboard-v2-[name].js",
      },
    },
    target: "es2021",
  },
});
