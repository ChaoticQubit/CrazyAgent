import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";
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
    const transformed = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(transformed.errors, [], `production module did not transform: ${url}`);
    return { format: "module", source: transformed.code, shortCircuit: true };
  },
});

const { App } = await import("../src/app.tsx");

test("desktop entry makes account sign-in discoverable before starting a project conversation", () => {
  const markup = renderToStaticMarkup(createElement(App));
  assert.match(markup, /Owner disconnected/);
  assert.match(markup, /<button\b[^>]*data-testid="connect-owner"[^>]*>Connect to engine<\/button>/);
  assert.match(markup, /<button\b[^>]*data-testid="preview-sign-in"[^>]*>[^<]*Sign in[^<]*<\/button>/,
    "the real application must expose a visible account sign-in action without inspecting metadata");
  assert.match(markup, /Accounts and sign-in/);
  assert.match(markup, /<button\b[^>]*>Projects<\/button>/);
  assert.match(markup, /<button\b[^>]*>Conversation<\/button>/);
  assert.doesNotMatch(markup, /<input\b[^>]*type="password"/);
  assert.doesNotMatch(markup, /(?:Account authenticated|Signed in as|Master is running)/);
  assert.ok(Buffer.byteLength(markup, "utf8") < 32768, "initial preview markup must remain bounded");
});
