import { globSync } from "node:fs";

import { defineConfig } from "vite";

// The one frontend build. Every module under web/ts is an entry and
// `preserveModules` keeps one output file per source module (public/js
// mirrors web/ts, with its exports), as the dashboard lazy-loads its React
// views and tests import single modules from the shipped tree.
const entries = globSync("web/ts/**/*.{ts,tsx}", {
  exclude: (file) => file.endsWith(".d.ts"),
});
export default defineConfig({
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  publicDir: false,
  build: {
    emptyOutDir: false,
    lib: {
      entry: entries,
      formats: ["es"],
    },
    minify: "oxc",
    outDir: "public/js",
    rollupOptions: {
      output: {
        // Dependencies go to vendor/: Node treats any file under a
        // node_modules directory as CommonJS, which breaks importing the
        // shipped modules in tests.
        entryFileNames: (chunk) =>
          `${chunk.name.replace(/^node_modules\//, "vendor/")}.js`,
        preserveModules: true,
        preserveModulesRoot: "web/ts",
      },
    },
    target: "es2021",
  },
});
