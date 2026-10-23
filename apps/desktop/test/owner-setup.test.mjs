import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) {
      return nextLoad(url, context);
    }
    const result = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(result.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: result.code, shortCircuit: true };
  },
});

const { errorMessage } = await import("../src/engine-client.ts");

test("pool and owner profile refusals read as plain sentences", () => {
  assert.equal(errorMessage({ code: "pool_member_limit" }), "A pool can hold up to 15 accounts");
  assert.equal(errorMessage({ code: "pool_in_use" }), "This pool is used by a project. Remove it from the project first.");
  assert.equal(errorMessage({ code: "pool_name_bounds" }), "Use a pool name of 1 to 64 characters, without control characters.");
  for (const code of ["owner_profile_name", "owner_profile_role", "owner_profile_bio", "owner_profile_stack", "owner_profile_bounds"]) {
    assert.notEqual(errorMessage({ code }), errorMessage({ code: "unknown" }), code);
  }
});
