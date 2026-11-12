import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");

const label = (id) => ({ codex: "Codex", "claude-code": "Claude Code" })[id] ?? id;
const task = (id, status, extra = {}) => ({ id, source: "master", harness: "codex", tools: true, prompt: `Task ${id}`, status, status_label: "", agent: null, attempts: 0,
  result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 0, started_ms: null, finished_ms: null, ...extra });

test("a waiting task shows why it waits, the pool line, or the queue line before any reason (F11)", () => {
  const capped = "Waits for a free codex slot: the team plan runs 2 at once and 2 run now";
  const status = { agents: [], show_finished: false, tasks: [
    task(1, "queued", { wait: capped }),
    task(2, "needs_pool"),
    task(3, "queued"),
    task(4, "running", { wait: "stale" }),
  ] };
  const { tasks } = research.teamPanel(status, [], label, "all", 0);
  assert.deepEqual(tasks.map((row) => row.summary), [capped, "Waits for an account pool for this project", "Waits for its turn in the queue", ""]);
  assert.equal(research.waitLine({ status: "queued", wait: null }), "Waits for its turn in the queue");
});

test("the Team view names every agent limit, read only, with No limit for an unset cap (F11)", () => {
  const harness = (name, agents_max, role = "sub") => ({ harness: name, role, agents_max, tools: true, live: 0, delegation: null, delegation_label: null, runnable: true });
  assert.equal(
    research.teamLimitsLine({ max_agents: 6, harnesses: [harness("codex", 3, "master"), harness("codex", 3), harness("claude-code", 2)], max_harnesses: 4, max_teammates: null }, label),
    "Limits: 5 sub-agents at once (Codex 3, Claude Code 2). Max harnesses per project: 4. Max teammates per harness: No limit.",
  );
  assert.equal(
    research.teamLimitsLine({ max_agents: 21, harnesses: [], max_harnesses: undefined, max_teammates: 2 }, label),
    "Limits: 20 sub-agents at once (any harness that can run). Max harnesses per project: No limit. Max teammates per harness: 2.",
  );
});
