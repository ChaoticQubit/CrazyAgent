import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-model-choice.ts");
const view = await import("../src/screens/settings/models-section.tsx");

const own = [
  { id: "opus", origin: "catalog" },
  { id: "my-model", origin: "pending" },
  { id: "good-model", origin: "verified" },
  { id: "bad-model", origin: "failed" },
];
const ownIds = (extra = {}) => renderToStaticMarkup(createElement(view.OwnIdsView, {
  candidates: own, field: "", busy: null, note: "", onField: () => {}, onAdd: () => {}, onRemove: () => {}, ...extra,
}));

test("a test launch says what it decided in the TUI's words, a pending reason as the engine gave it (DEC-150 M-6)", () => {
  const models = { harness: "codex", rows: [], candidates: own, qualified: true, reason: null };
  assert.equal(view.testNote("my-model", { kind: "ok", value: { tested: "verified", reason: null, models } }),
    "my-model passed its test launch. You can now choose it, tick it and launch it.");
  assert.equal(view.testNote("my-model", { kind: "ok", value: { tested: "failed", reason: null, models } }),
    "my-model failed its test launch. The provider refused this model. It stays listed and can't be used.");
  assert.equal(view.testNote("my-model", { kind: "ok", value: { tested: "pending", reason: "No signed-in Codex account in this pool.", models } }),
    "my-model still waits for its test launch. No signed-in Codex account in this pool.");
});

test("a refused test launch says why: busy, an unknown pool and a pool with no account (DEC-150 M-6)", () => {
  assert.equal(view.testNote("my-model", { kind: "error", code: "model_choice_test_busy" }),
    "Could not test my-model. A test launch for this harness is already running. Try again when it ends.");
  assert.equal(client.modelChoiceRefusal({ code: "model_choice_pool_unknown" }), "That pool no longer exists, so nothing was tested. Pick another one.");
  assert.equal(client.modelChoiceRefusal({ code: "model_choice_pool_needed" }), "Pick a pool with at least one account to run the test launch.");
});

test("only a pending owner id of Claude Code or Codex offers Test, and a running test shows on its row (DEC-150 M-6)", () => {
  assert.equal(client.testLaunchHarness("codex"), true);
  assert.equal(client.testLaunchHarness("claude-code"), true);
  assert.equal(client.testLaunchHarness("opencode"), false);
  const offered = ownIds({ canTest: true });
  assert.ok(offered.includes('aria-label="Test my-model"'), offered);
  for (const id of ["good-model", "bad-model", "opus"]) assert.ok(!offered.includes(`aria-label="Test ${id}"`), id);
  assert.ok(!ownIds({ canTest: false }).includes("aria-label=\"Test "), "a harness without a test launch offers none");
  const running = ownIds({ canTest: true, testing: "my-model" });
  assert.ok(running.includes("Testing. This can take up to 2 minutes."), running);
  assert.ok(running.includes("Testing..."), running);
});

test("Test asks which pool runs the launch, as Edit with AI asks, and runs only once one is picked (DEC-150 M-6)", () => {
  const pools = [{ id: "main", name: "Main" }, { id: "spare", name: "Spare" }];
  const ask = (extra) => renderToStaticMarkup(createElement(view.TestPoolView, {
    model: "my-model", pools, picked: null, busy: false, onPick: () => {}, onRun: () => {}, onCancel: () => {}, ...extra,
  }));
  const none = ask({});
  assert.ok(none.includes("Which pool runs the test launch of my-model?"), none);
  assert.ok(none.includes(">Main<") && none.includes(">Spare<"), none);
  assert.match(none, /<button[^>]*disabled=""[^>]*>Run test<\/button>/);
  const picked = ask({ picked: "spare" });
  assert.ok(/value="spare" checked=""/.test(picked), picked);
  assert.match(picked, /<button(?![^>]*disabled)[^>]*>Run test<\/button>/);
  assert.ok(ask({ picked: "spare", busy: true }).includes("Testing..."));
  assert.ok(ask({ pools: [] }).includes("No pool yet. Add one under Pools, then try again."));
});
