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
      target: "es2022", sourceType: "module", jsx: { runtime: "automatic" },
    });
    assert.deepEqual(transformed.errors, [], "production source must load before behavior assertions");
    return { format: "module", source: transformed.code, shortCircuit: true };
  },
});
const screen = await import("../src/screens/account-onboarding.tsx");

test("ordinary configured account fields expose endpoint and model without routing identities", () => {
  assert.equal(typeof screen.ConfiguredAccountFields, "function", "production must render the ordinary configured endpoint form");
  const markup = renderToStaticMarkup(createElement(screen.ConfiguredAccountFields, {
    provider: "api_key", url: "https://api.example.test/v1", model: "model-a", disabled: false,
  }));
  assert.match(markup, /data-testid="account-onboarding-url"/);
  assert.match(markup, /data-testid="account-onboarding-model"/);
  assert.match(markup, /https:\/\/api.example.test\/v1/);
  assert.doesNotMatch(markup, /(?:Harness reference|Profile reference|Credential reference|Account reference|type="password")/);
  assert.doesNotMatch(markup, /(?:Authenticated|Model ready|setup unavailable)/);
});

test("the separate key form uses a masked uncontrolled field and explicitly forbids secret replay", () => {
  assert.equal(typeof screen.AccountSecretForm, "function", "production must provide separate transient private key entry");
  const markup = renderToStaticMarkup(createElement(screen.AccountSecretForm, { disabled: false }));
  const input = markup.match(/<input\b[^>]*data-testid="account-profile-secret"[^>]*>/)?.[0];
  assert.ok(input, "private entry must be visibly distinct from nonsecret account metadata");
  assert.match(input, /type="password"/);
  assert.match(input, /maxLength="8192"/i);
  assert.match(input, /autoComplete="off"/i);
  assert.doesNotMatch(input, /\b(?:value|name)=/);
  assert.match(markup, /Save private key/);
  assert.match(markup, /never automatically resent/i);
  assert.doesNotMatch(markup, /(?:Authenticated|Model ready)/);
});
