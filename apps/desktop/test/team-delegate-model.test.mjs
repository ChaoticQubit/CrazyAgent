import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const controls = await import("../src/screens/team/controls.tsx");
const projects = await import("../src/api-projects.ts");

const row = (slot, effective, source) => ({ slot, here: null, effective, source, version: 1, access: { kind: "editable" } });
const models = (agent, allowed, source = "app") => ({
  harness: "claude-code",
  rows: [row("master", null, source), row("agent", agent, source), row("allowed", allowed, source)],
  candidates: [["opus", "catalog"], ["sonnet", "catalog"], ["team-haiku", "verified"], ["my-draft", "pending"]].map(([id, origin]) => ({ id, origin })),
  qualified: true,
  reason: null,
});
const harness = (name, role) => ({ harness: name, role, agents_max: 2, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true });
const status = { project_id: "chores", keep_warm: false, idle_stop_secs: 600, paused: false, min_agents: 1, max_agents: 3,
  harnesses: [harness("codex", "master"), harness("claude-code", "sub")], agents: [], tasks: [], messages: [], plan: [] };
const WORDS = {
  team_model_invalid: "That model id isn't valid. Pick a model from the list.",
  team_model_not_allowed: "That model isn't on this harness's allowed list. Pick another model, or Default.",
  team_model_not_supported: "This harness doesn't take a model from CrazyAgent. Hand the task over with Default.",
};

test("a hand-off offers the harness default, then each allowed model the harness offers once, in the TUI's words (DEC-150 M-8)", () => {
  const options = controls.delegateModelOptions(models("sonnet", ["sonnet", "opus", "team-haiku", "my-draft", "gone-model", "opus"]));
  assert.deepEqual(options, [
    { value: "", label: "Default (sonnet)" },
    { value: "opus", label: "opus" },
    { value: "team-haiku", label: "team-haiku" },
  ]);
});

test("a hand-off offers only Default for any sub-agent, before or without a read, and on a harness that takes no model (DEC-150 M-8)", () => {
  assert.deepEqual(controls.delegateModelOptions(null), [{ value: "", label: "Default" }]);
  assert.deepEqual(controls.delegateModelOptions(models(null, ["opus"], "not_applied")), [{ value: "", label: "Default (Account's own model)" }]);
  assert.deepEqual(controls.delegateModelOptions(models(null, ["opus"])), [{ value: "", label: "Default (Account's own model)" }, { value: "opus", label: "opus" }]);
});

test("the hand-off form has a Model choice on Default before the harness's models are read (DEC-150 M-8)", () => {
  const html = renderToStaticMarkup(createElement(controls.TeamActions, { projectId: "chores", status, reload: () => {} }));
  const select = html.match(/<select[^>]*id="tg-hand-model-chores"[^>]*>([\s\S]*?)<\/select>/);
  assert.ok(select, html);
  assert.deepEqual([...select[1].matchAll(/<option value="([^"]*)"[^>]*>([^<]*)<\/option>/g)].map((found) => [found[1], found[2]]), [["", "Default"]]);
  assert.ok(html.includes(">Model<"), html);
});

test("a refused model pick says why in the TUI's words, refused by the engine or by the bridge's check (DEC-150 M-8)", () => {
  const receipt = (rejection) => ({ outcome: "rejected", pending: false, rejection });
  for (const [code, sentence] of Object.entries(WORDS)) {
    assert.equal(projects.failureCopy(code), sentence);
    assert.equal(controls.changeNote({ kind: "ok", value: receipt(code) }, "x"), sentence);
  }
  assert.equal(controls.changeNote({ kind: "error", code: "team_action_invalid", cause: "team_model_invalid" }, "x"), WORDS.team_model_invalid);
});
