import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Suites that also smoke-test the shipped bundle load modules through here:
// the TypeScript sources by default (under tsx, `.js` resolves to `.ts`), or
// the built tree when FRONTEND_MODULES_DIR points at it (`public/js`).
const SOURCE_DIR = fileURLToPath(new URL("../../../web/ts/", import.meta.url));

export async function loadFrontendModule(relativePath) {
  const modulesDir = process.env.FRONTEND_MODULES_DIR || SOURCE_DIR;
  return import(pathToFileURL(path.resolve(modulesDir, relativePath)).href);
}
