import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const job = await import("../src/screens/project/publish-job.ts");

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const snake = (name) => name.replace(/[A-Z]/g, (letter, at) => `${at ? "_" : ""}${letter.toLowerCase()}`);

test("every engine rule has its own plain label, the same words as the TUI", () => {
  const rules = read("../../crates/crazyagent/src/github/rules.rs");
  const body = rules.slice(rules.indexOf("pub enum GitHubRule {"));
  const variants = [...body.slice(0, body.indexOf("}")).matchAll(/^\s+([A-Z]\w+),$/gm)].map((match) => snake(match[1]));
  assert.equal(variants.length, 25);
  assert.deepEqual(Object.keys(job.RULE_LABELS).sort(), [...variants].sort());
  const labels = Object.values(job.RULE_LABELS);
  assert.equal(new Set(labels).size, labels.length);
  for (const label of labels) {
    assert.ok(label.length > 0 && !label.includes("_") && !label.endsWith("."), label);
  }
  const tui = read("../../crates/crazyagent/src/tui/screens/project_publish.rs");
  const shown = Object.fromEntries([...tui.matchAll(/GitHubRule::(\w+) => "([^"]+)"/g)].map((match) => [snake(match[1]), match[2]]));
  assert.deepEqual(shown, job.RULE_LABELS);
});
