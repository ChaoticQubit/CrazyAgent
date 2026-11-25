import { COMPUTERS, eachComputer } from "./load-source.mjs";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const model = await import("../src/screens/first-run/harnesses.ts");

/** Every string literal in one TUI source file, in source order, unescaped, so a desktop word
 * can be found in the TUI whatever way rustfmt splits the lines. A sentence joined around the
 * computer's name reads as one literal with the given computer's words. Inputs: the file under
 * the TUI screens folder and the computer's words. Output: the literals. */
function rustLiterals(file, computer) {
  const source = readFileSync(new URL(`../../../crates/crazyagent/src/tui/screens/${file}`, import.meta.url), "utf8")
    .replace(/on_this_computer!\(\s*"((?:[^"\\]|\\.)*)",\s*"((?:[^"\\]|\\.)*)",?\s*\)/g, (_, before, after) => `"${before}${computer}${after}"`);
  return [...source.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((match) => match[1].replace(/\\(["\\])/g, "$1"));
}

const STEP = rustLiterals("harness_setup.rs", COMPUTERS[2].computer);

test("the TUI Harnesses step uses the desktop step's words", async () => {
  await eachComputer("../src/screens/first-run/harnesses.ts", (step, { os, computer }) => {
    const words = rustLiterals("harness_setup.rs", computer);
    const single = [
      step.HARNESS_STEP.label, step.HARNESS_STEP.title, step.HARNESS_STEP.intro,
      ...step.OFFERED.map((harness) => harness.id), ...step.OFFERED.map((harness) => harness.name),
      ...Object.values(step.HARNESS_WORDS),
    ];
    for (const word of single) assert.ok(words.includes(word), `${os}: ${word}`);
    for (const kind of ["bundled", "installed", "offer", "refused", "reading", "waiting", "installing", "done", "failed", "blocked"]) {
      const badge = step.rowBadge({ kind, version: null, line: "", reason: "", guide: null });
      assert.ok(words.includes(badge.word), `${os} ${kind}: ${badge.word}`);
    }
  });
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

test("the TUI says needs Node and a bad list in the desktop's words", async () => {
  await eachComputer("../src/api-harness-updates.ts", (updates, { os, computer }) => {
    const copy = rustLiterals("settings_harnesses/words.rs", computer);
    for (const code of ["harness_install_needs_node", "harness_install_list_invalid"]) {
      const at = copy.indexOf(code);
      assert.ok(at >= 0, `${os} ${code}`);
      assert.equal(copy[at + 1], updates.updateCopy(code), `${os} ${code}`);
    }
  });
});

test("Task 21 (Q3): every Limited control reason the bundled manifests give reads word for word in Settings, Harnesses and in the pickers' team notes", async () => {
  const { createElement } = await import("react");
  const { renderToStaticMarkup } = await import("react-dom/server");
  const view = await import("../src/screens/settings/harnesses.tsx");
  const wizard = await import("../src/screens/new-project/wizard.ts");
  const folder = new URL("../../../crates/crazyagent/harness-manifests/", import.meta.url);
  const field = (text, key) => new RegExp(`^${key}:\\s*"([^"]*)"`, "mu").exec(text)?.[1] ?? null;
  const quiet = () => undefined;
  let limited = 0;
  for (const file of readdirSync(folder).filter((name) => name.endsWith(".yaml"))) {
    const text = readFileSync(new URL(file, folder), "utf8");
    const [id, name, tools, start] = ["id", "name", "tools_limited", "cannot_start"].map((key) => field(text, key));
    if (tools === null && start === null) continue;
    limited += 1;
    const entry = { id, name, status: { kind: "limited", reason: start ?? tools }, status_label: "", executable: `/opt/homebrew/bin/${id}`, headless: `${id} -p`,
      tools: tools === null ? { kind: "switchable" } : { kind: "limited", reason: tools }, model_selection: true, account_kinds: ["api_key"], team: { kind: "lead" }, aliases: [] };
    const row = renderToStaticMarkup(createElement(view.HarnessRowView, { entry, versions: null, mode: "check_and_update", onCheck: quiet, onUpdate: quiet, onInstall: quiet, onGuide: quiet }))
      .replace(/&#x27;/g, "'");
    assert.ok(row.includes(`Limited control: ${start ?? tools}`), `${id}: ${row}`);
    if (tools !== null) assert.ok(row.includes(tools), `${id}: ${row}`);
    const notes = wizard.teamNotes({ master: id, sub_harnesses: [] }, [entry]);
    for (const reason of [tools, start].filter((reason) => reason !== null)) assert.ok(notes.includes(`${name}: ${reason.replace(/\.$/u, "")}.`), `${id}: ${notes}`);
  }
  assert.ok(limited >= 10, "the bundled manifests limit most harnesses");
});
