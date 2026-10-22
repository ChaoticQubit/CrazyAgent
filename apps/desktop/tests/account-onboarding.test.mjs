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
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) return nextLoad(url, context);
    const transformed = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(transformed.errors, [], "production source must load before behavior assertions");
    return { format: "module", source: transformed.code, shortCircuit: true };
  },
});

const { ModelAccess } = await import("../src/screens/model-access.tsx");
const { useOwnerBrowser } = await import("../src/state.ts");

function AccountSettings() {
  return createElement(ModelAccess, { state: useOwnerBrowser() });
}

test("real account settings offer provider-first managed sign-in without routing fields", () => {
  const markup = renderToStaticMarkup(createElement(AccountSettings));
  const form = markup.match(/<form\b[^>]*data-testid="account-onboarding-form"[^>]*>[\s\S]*?<\/form>/)?.[0];
  assert.ok(form, "account settings must show the provider-first onboarding form before advanced metadata");
  assert.match(form, /Sign in to a provider/);
  assert.match(form, /<option\b[^>]*value="codex"[^>]*>Codex<\/option>/);
  assert.match(form, /<option\b[^>]*value="claude"[^>]*>Claude<\/option>/);
  assert.match(form, /Account label \(optional\)/);
  assert.match(form, /data-testid="account-onboarding-label"/);
  assert.match(form, /Continue to official sign-in/);
  assert.doesNotMatch(form, /(?:Harness reference|Protocol reference|Profile reference|Account reference|Provider reference|Model author reference|Endpoint URL)/);
  assert.doesNotMatch(form, /type="password"/);
  assert.doesNotMatch(form, /(?:Signed in as|Account authenticated|Model ready)/);
  assert.ok(Buffer.byteLength(markup, "utf8") < 65536);
});

test("real account settings expose ordinary key and compatible endpoint setup alongside managed providers", () => {
  const markup = renderToStaticMarkup(createElement(AccountSettings));
  const form = markup.match(/<form\b[^>]*data-testid="account-onboarding-form"[^>]*>[\s\S]*?<\/form>/)?.[0];
  assert.ok(form, "normal account onboarding must be visible without opening Advanced");
  assert.match(form, /<option\b[^>]*value="api_key"[^>]*>API key or compatible endpoint<\/option>/);
  assert.match(form, /<option\b[^>]*value="local"[^>]*>Local endpoint<\/option>/);
  assert.match(markup, /data-testid="model-tab-pools"/);
  assert.match(markup, /data-testid="model-access-authority"/);
  assert.match(markup, /data-testid="model-auth-unavailable"/);
});
