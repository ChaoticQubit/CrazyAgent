import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const model = await import("../src/screens/first-run/harnesses.ts");
const api = await import("../src/api-harness-updates.ts");

/** Every string literal in one TUI source file, in source order, unescaped, so a desktop word
 * can be found in the TUI whatever way rustfmt splits the lines. Input: the file under the
 * TUI screens folder. Output: the literals. */
function rustLiterals(file) {
  const source = readFileSync(new URL(`../../../crates/crazyagent/src/tui/screens/${file}`, import.meta.url), "utf8");
  return [...source.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((match) => match[1].replace(/\\(["\\])/g, "$1"));
}

const STEP = rustLiterals("harness_setup.rs");

test("the TUI Harnesses step uses the desktop step's words", () => {
  const single = [
    model.HARNESS_STEP.label, model.HARNESS_STEP.title, model.HARNESS_STEP.intro,
    ...model.OFFERED.map((harness) => harness.id), ...model.OFFERED.map((harness) => harness.name),
    ...Object.values(model.HARNESS_WORDS),
  ];
  for (const word of single) assert.ok(STEP.includes(word), word);
  for (const kind of ["bundled", "installed", "offer", "refused", "reading", "waiting", "installing", "done", "failed"]) {
    const badge = model.rowBadge({ kind, version: null, line: "", reason: "", guide: null });
    assert.ok(STEP.includes(badge.word), `${kind}: ${badge.word}`);
  }
});

test("the TUI progress sentences are the desktop's, with the same counts filled in", () => {
  const tail = STEP.find((literal) => literal.includes("{failed}"));
  assert.ok(tail, "the failed tail");
  const filled = (literal, counts) => literal.replace("{done}", String(counts.done)).replace("{total}", String(counts.total))
    .replace("{tail}", counts.failed ? tail : "").replace("{failed}", String(counts.failed));
  const formats = STEP.filter((literal) => literal.includes("{done}"));
  for (const counts of [{ done: 1, total: 2, failed: 0, running: true }, { done: 2, total: 2, failed: 0, running: false }, { done: 2, total: 2, failed: 1, running: false }]) {
    assert.ok(formats.some((literal) => filled(literal, counts) === model.progressLine(counts)), model.progressLine(counts));
  }
});

test("the TUI says needs Node and a bad list in the desktop's words", () => {
  const copy = rustLiterals("settings_harnesses.rs");
  for (const code of ["harness_install_needs_node", "harness_install_list_invalid"]) {
    const at = copy.indexOf(code);
    assert.ok(at >= 0, code);
    assert.equal(copy[at + 1], api.updateCopy(code), code);
  }
});
