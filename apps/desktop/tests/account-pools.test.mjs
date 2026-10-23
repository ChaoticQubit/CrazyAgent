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

const screens = await import("../src/screens/model-access.tsx");
const { formFromRecord } = await import("../src/model-access.ts");
const { useOwnerBrowser } = await import("../src/state.ts");

function PoolFields({ form, connections, current, hasMore }) {
  const state = useOwnerBrowser();
  assert.equal(typeof state.loadPoolConnections, "function");
  assert.equal(typeof state.changePoolConnection, "function");
  return createElement(screens.PoolConnectionPicker, {
    form, connections, current, hasMore, blocked: state.busy || state.pending,
    onLoad: state.loadPoolConnections, onChange: state.changePoolConnection,
  });
}

test("production pool picker renders plain connection configuration as explicit checkboxes", () => {
  assert.equal(typeof screens.PoolConnectionPicker, "function", "pool editing must render a connection picker instead of requiring copied connection IDs");
  const connection = {
    id: "first", provider: "local", harness: "rcli", protocol: "openai", account_ref: "personal",
    endpoint: { kind: "http", url: "http://127.0.0.1:11434/v1" },
    model: { id: "first-model", author: "local" }, auth: { method: "none" },
  };
  const second = { ...connection, id: "second", account_ref: "work", model: { id: "second-model", author: "local" } };
  const form = formFromRecord({ kind: "pool", pool: { id: "retained-pool", members: ["off-page", "first"], projects: [], roles: [], fallback_allowed: false } }, "pool-ticket");
  const markup = renderToStaticMarkup(createElement(PoolFields, { form, connections: [connection, second], current: true, hasMore: true }));
  assert.match(markup, /Choose saved connections/);
  const first = markup.match(/<input\b[^>]*data-pool-connection="first"[^>]*>/)?.[0];
  const other = markup.match(/<input\b[^>]*data-pool-connection="second"[^>]*>/)?.[0];
  assert.ok(first);
  assert.ok(other);
  assert.match(first, /type="checkbox"/);
  assert.match(first, /checked=""/);
  assert.doesNotMatch(first, /disabled=""/);
  assert.doesNotMatch(other, /checked=""/);
  assert.match(markup, /first-model/);
  assert.match(markup, /second-model/);
  assert.match(markup, /2 selected/);
  assert.match(markup, /Selections on other pages are retained/);
  assert.match(markup, /data-testid="next-pool-connections"/);
  assert.doesNotMatch(markup, /(?:Signed in as|Account authenticated|Model ready)/);
  assert.equal(form.id, "retained-pool");
  assert.equal(form.members, "off-page, first");
  assert.ok(Buffer.byteLength(markup, "utf8") < 32768);
});

test("production pool picker keeps stale choices disabled and an empty input free of invented connections", () => {
  assert.equal(typeof screens.PoolConnectionPicker, "function");
  const form = formFromRecord({ kind: "pool", pool: { id: "pool", members: ["retained"], projects: [], roles: [], fallback_allowed: false } }, "old-ticket");
  const markup = renderToStaticMarkup(createElement(PoolFields, { form, connections: [], current: false, hasMore: false }));
  assert.match(markup, /data-testid="load-pool-connections"/);
  assert.match(markup, /Load current saved connections/);
  assert.match(markup, /1 selected/);
  assert.doesNotMatch(markup, /data-pool-connection=/);
  assert.doesNotMatch(markup, /data-testid="next-pool-connections"/);
  assert.equal(form.members, "retained");
});

test("production pool picker refuses edits and paging from stale configuration input", () => {
  assert.equal(typeof screens.PoolConnectionPicker, "function");
  const connection = {
    id: "retained", provider: "local", harness: "rcli", protocol: "openai", account_ref: "account",
    endpoint: { kind: "http", url: "http://127.0.0.1:11434/v1" },
    model: { id: "model", author: "local" }, auth: { method: "none" },
  };
  const form = formFromRecord({ kind: "pool", pool: { id: "pool", members: ["retained"], projects: [], roles: [], fallback_allowed: false } }, "old-ticket");
  const markup = renderToStaticMarkup(createElement(PoolFields, { form, connections: [connection], current: false, hasMore: true }));
  const choice = markup.match(/<input\b[^>]*data-pool-connection="retained"[^>]*>/)?.[0];
  assert.ok(choice);
  assert.match(choice, /checked=""/);
  assert.match(choice, /disabled=""/);
  const next = markup.match(/<button\b[^>]*data-testid="next-pool-connections"[^>]*>/)?.[0];
  assert.ok(next);
  assert.match(next, /disabled=""/);
});
