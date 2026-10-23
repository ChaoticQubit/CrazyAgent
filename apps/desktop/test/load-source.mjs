import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      if (specifier.endsWith(".svg?raw") || specifier.endsWith(".css")) {
        return { url: new URL(specifier, context.parentURL).href, shortCircuit: true };
      }
      for (const extension of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (extension === "" && !/\.tsx?$/.test(specifier)) continue;
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot)) return nextLoad(url, context);
    if (url.endsWith(".svg?raw")) {
      const text = readFileSync(new URL(url.slice(0, -"?raw".length)), "utf8");
      return { format: "module", source: `export default ${JSON.stringify(text)};`, shortCircuit: true };
    }
    if (url.endsWith(".css")) return { format: "module", source: "export {};", shortCircuit: true };
    if (!/\.tsx?$/.test(url)) return nextLoad(url, context);
    const result = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(result.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: result.code, shortCircuit: true };
  },
});
