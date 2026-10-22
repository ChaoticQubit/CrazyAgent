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

const conversation = await import("../src/screens/conversation.tsx");
const { useOwnerBrowser } = await import("../src/state.ts");

test("master panel offers Start master in plain language before a session exists", () => {
  function Panel() {
    const state = useOwnerBrowser();
    return createElement(conversation.MasterPanel, { state: { ...state, connected: true }, ticket: "7" });
  }
  const markup = renderToStaticMarkup(createElement(Panel));
  assert.match(markup, /Master conversation/);
  assert.match(markup, /The master has not started for this project/);
  assert.match(markup, /data-testid="select-project-master"[^>]*>Start master</);
  assert.doesNotMatch(markup, /data-testid="select-project-master"[^>]*disabled/);
  assert.doesNotMatch(markup, /does not expose master selection/);
});

test("master panel disables Start master while disconnected", () => {
  function Panel() {
    return createElement(conversation.MasterPanel, { state: useOwnerBrowser(), ticket: "7" });
  }
  const markup = renderToStaticMarkup(createElement(Panel));
  assert.match(markup, /<button[^>]*disabled=""[^>]*data-testid="select-project-master"/);
});

test("turn status labels are plain language for every phase", () => {
  const label = (phase, reason = null) => conversation.turnStatus({ id: "t", ordinal: 1, phase, reason, reply: null });
  assert.equal(label("queued"), "Waiting to start");
  assert.equal(label("prepared"), "Waiting to start");
  assert.equal(label("running"), "Thinking");
  assert.equal(label("succeeded"), "Replied");
  assert.equal(label("blocked", "runtime_unavailable"), "Blocked (runtime unavailable)");
  assert.equal(label("uncertain"), "Outcome unclear");
  for (const phase of ["delivering", "waiting_approval", "failed", "cancelled"]) assert.ok(label(phase).length > 0);
});

test("busy bridge calls are retried and other failures are not", async () => {
  let calls = 0;
  const value = await conversation.retryBusy(async () => {
    calls += 1;
    if (calls < 3) throw { code: "busy" };
    return "read";
  }, 1);
  assert.equal(value, "read");
  assert.equal(calls, 3);
  calls = 0;
  await assert.rejects(conversation.retryBusy(async () => { calls += 1; throw { code: "stale_master" }; }, 1), { code: "stale_master" });
  assert.equal(calls, 1);
  calls = 0;
  await assert.rejects(conversation.retryBusy(async () => { calls += 1; throw { code: "busy" }; }, 1), { code: "busy" });
  assert.equal(calls, 5);
});
