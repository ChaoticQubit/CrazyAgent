import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const words = await import("../src/screens/demo/words.ts");

/** Every string literal in the TUI demo screen, in source order, unescaped, so a word the
 * desktop shows can be found in the TUI whatever way rustfmt splits the lines. Input: none.
 * Output: the literals. */
function rustLiterals() {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/demo.rs", import.meta.url), "utf8");
  return [...source.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((match) => match[1].replace(/\\(["\\])/g, "$1"));
}

/** True when the literals hold the given words one right after another. Inputs: the
 * literals and the words. Output: the answer. */
function consecutive(literals, run) {
  return literals.some((_, start) => run.every((word, offset) => literals[start + offset] === word));
}

const LATER = new Set([words.DEMO_ENTRY.body, words.DEMO_MARK, words.DEMO_COPY_GOAL, words.DEMO_NOT_READY]);

test("every demo word the TUI shows is the desktop's word", () => {
  const literals = rustLiterals();
  const single = [
    words.DEMO_ENTRY.title,
    words.DEMO_ENTRY.start,
    words.DEMO_ENTRY.open,
    words.DEMO_COST_NOTE,
    words.DEMO_ABOUT,
    words.DEMO_PICK,
    words.DEMO_CREATE,
    words.DEMO_NO_POOL,
    words.DEMO_NO_POOL_ACTION,
    words.DEMO_EXISTS,
    words.DEMO_POOL_UNKNOWN,
    words.DEMO_UNCONFIRMED,
    words.DEMO_POOL_UNASSIGNED,
    words.DEMO_GUIDE_TITLE,
    words.DEMO_GOAL_LABEL,
  ];
  for (const word of single) {
    assert.ok(!LATER.has(word), word);
    assert.ok(literals.includes(word), `the TUI lacks: ${word}`);
  }
  for (const [code, sentence] of Object.entries(words.DEMO_POOL_REASONS)) {
    assert.ok(consecutive(literals, [code, sentence]), `pool reason ${code}`);
  }
  assert.equal(Object.keys(words.DEMO_REFUSALS).length, 8);
  for (const [code, sentence] of Object.entries(words.DEMO_REFUSALS)) {
    assert.ok(consecutive(literals, [code, sentence]), `refusal ${code}`);
  }
  const at = words.DEMO_STEPS.map((step) => {
    assert.ok(consecutive(literals, [step.id, step.title, step.body]), `step ${step.id}`);
    return literals.indexOf(step.id);
  });
  assert.deepEqual([...at].sort((a, b) => a - b), at, "the TUI keeps the desktop's step order");
});

test("a desktop word the TUI does not show yet is named here, not dropped silently", () => {
  const literals = rustLiterals();
  for (const word of LATER) {
    assert.ok(!literals.includes(word), `the TUI shows it now; check it against the desktop: ${word}`);
  }
});
