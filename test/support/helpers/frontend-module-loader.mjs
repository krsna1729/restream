import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";

export function resolveFrontendModulesDir() {
  const modulesDir = process.env.FRONTEND_MODULES_DIR;

  assert.ok(modulesDir, "FRONTEND_MODULES_DIR must be set");

  return modulesDir;
}

export async function loadFrontendModule(relativePath) {
  const moduleUrl = pathToFileURL(
    path.join(resolveFrontendModulesDir(), relativePath),
  ).href;
  return import(moduleUrl);
}
