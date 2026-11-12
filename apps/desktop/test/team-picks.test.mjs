import assert from "node:assert/strict";
import test from "node:test";
import "./load-source.mjs";

const research = await import("../src/screens/project/research.ts");

const label = (id) => ({ codex: "Codex", "claude-code": "Claude Code" })[id] ?? id;
const task = (id, status, extra = {}) => ({ id, source: "master", harness: "claude-code", tools: true, prompt: `Task ${id}`, status, status_label: "", agent: null,
  attempts: 0, result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 0, started_ms: null, finished_ms: null, ...extra });

test("a task row says which model the master picked and why, in the engine's words, and nothing for the default (DEC-150 M-7)", () => {
  const pick = { model: "claude-opus-4-5", reason: "the charts need careful layout work" };
  const status = { agents: [], show_finished: false, tasks: [
    task(1, "running", { pick }),
    task(2, "queued", { pick: null }),
    task(3, "running"),
  ] };
  const { tasks } = research.teamPanel(status, [], label, "all", 0);
  assert.deepEqual(tasks.map((row) => row.pick), ["Model: claude-opus-4-5. Why: the charts need careful layout work", "", ""]);
  assert.equal(research.pickLine({ model: "gpt-a", reason: "" }), "Model: gpt-a");
});

test("a pick reason reads as one bounded line without control characters (DEC-150 M-7)", () => {
  const line = research.pickLine({ model: "gpt-a", reason: `${"r".repeat(400)}\nsecond\tline` });
  assert.ok(!/[\u0000-\u001f\u007f]/u.test(line), line);
  assert.ok(line.startsWith("Model: gpt-a. Why: rrr"), line);
  assert.ok(line.length <= "Model: gpt-a. Why: ".length + 240, String(line.length));
});
