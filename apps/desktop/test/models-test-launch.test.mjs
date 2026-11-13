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

test("only a pending owner id on a harness the engine says has a test launch offers Test, and a running test shows on its row (DEC-150 M-6)", () => {
  const offered = ownIds({ testable: true });
  assert.ok(offered.includes('aria-label="Test my-model"'), offered);
  for (const id of ["good-model", "bad-model", "opus"]) assert.ok(!offered.includes(`aria-label="Test ${id}"`), id);
  assert.ok(!ownIds({ testable: false }).includes("aria-label=\"Test "), "a harness without a test launch offers none");
  const running = ownIds({ testable: true, testing: "my-model" });
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

test("a harness the engine says has no test launch says Not supported on Add and on a pending id, in the TUI's words (MUG2-L1)", () => {
  assert.equal(client.NO_TEST_LAUNCH, "An id you add needs a test launch, and this harness has none.");
  assert.equal("testLaunchHarness" in client, false, "the client keeps no harness list of its own");
  const none = ownIds({ testable: false });
  assert.ok(!none.includes('id="models-add-id"'), none);
  assert.ok(!/>Add<\/button>/.test(none), none);
  assert.ok(none.includes(client.NO_TEST_LAUNCH), none);
  assert.equal((none.match(/Not supported by this harness\./g) ?? []).length, 2, "on Add and on the pending id");
  assert.ok(!none.includes("Waiting for its test launch"), none);
  assert.ok(!/waits for a test launch/i.test(none), `the about line promises no test launch the harness lacks: ${none}`);
  assert.ok(none.includes('aria-label="Remove my-model"'), "the id can still be removed");
  const offered = ownIds({ testable: true });
  assert.ok(offered.includes('id="models-add-id"'), offered);
  assert.ok(!offered.includes("Not supported by this harness"), offered);
  assert.ok(offered.includes("It waits for a test launch and can be chosen once that passes."), offered);
});

test("an RCLI id names the provider it runs on: Add asks which in the TUI's words and adds only once one is picked (DEC-150 addendum 9e)", () => {
  const providers = [{ id: "openrouter", name: "OpenRouter" }, { id: "openai", name: "OpenAI API" }];
  const none = ownIds({ testable: true, providers, provider: null, field: "my-model" });
  assert.ok(none.includes("Which provider runs this id?"), none);
  assert.ok(none.includes(">OpenRouter<") && none.includes(">OpenAI API<"), none);
  assert.match(none, /<button[^>]*disabled=""[^>]*>Add<\/button>/);
  const picked = ownIds({ testable: true, providers, provider: "openai", field: "my-model" });
  assert.ok(/value="openai" checked=""/.test(picked), picked);
  assert.match(picked, /<button(?![^>]*disabled)[^>]*>Add<\/button>/);
  const plain = ownIds({ testable: true, providers: [], provider: null, field: "my-model" });
  assert.ok(!plain.includes("Which provider runs this id?"), plain);
  assert.match(plain, /<button(?![^>]*disabled)[^>]*>Add<\/button>/);
  assert.equal(client.modelChoiceRefusal({ code: "model_choice_provider_needed" }), "Pick the provider this id runs on.");
  assert.equal(client.modelChoiceRefusal({ code: "model_choice_provider_unknown" }), "This harness doesn't offer that provider. Pick one from the list.");
});
