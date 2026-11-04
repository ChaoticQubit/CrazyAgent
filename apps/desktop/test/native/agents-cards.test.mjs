import assert from "node:assert/strict";
import test from "node:test";
import { cardOf, copyRow, finishedRow, fromRows, moved, notShown, offTab, overlapping, roleName, showsTask, spilled, statusWord } from "./agents-cards.mjs";

/** A card as readCards reads it, at a place and size, with one text box inside it unless
 * given others. Inputs: the title, the box and the text boxes. Output: the card. */
function card(title, x, y, w = 200, h = 80, clips = [{ text: title, x: x + 8, y: y + 8, w: 120, h: 16 }]) {
  return { kind: "sub", label: `${title}, Sub-agent, Codex, Working. Write a.md. Opens its terminal.`, title, task: ["Write a.md"], opens: true, x, y, w, h, clips };
}

test("a card names the engine's status label, else the fixed word for the status", () => {
  assert.equal(statusWord({ status: "working", status_label: null }), "Working");
  assert.equal(statusWord({ status: "working", status_label: " Working - instructions changed " }), "Working - instructions changed");
  assert.equal(statusWord({ status: null, status_label: null }), "");
});

test("a node is matched to the card whose button label starts with its label", () => {
  const cards = [card("Ada (@lead-a)", 0, 0), card("Ada (@lead-ab)", 300, 0)];
  assert.equal(cardOf(cards, { label: "Ada (@lead-ab)" }), cards[1]);
  assert.equal(cardOf(cards, { label: "Bo (@worker)" }), undefined);
});

test("a card shows a task only when its first task line is a real task", () => {
  assert.equal(showsTask(card("Ada (@lead-a)", 0, 0)), true);
  assert.equal(showsTask({ ...card("Ada (@lead-a)", 0, 0), task: ["No task yet."] }), false);
  assert.equal(showsTask({ ...card("Ada (@lead-a)", 0, 0), task: ["  "] }), false);
  assert.equal(showsTask({ ...card("Ada (@lead-a)", 0, 0), task: [] }), false);
});

test("cards that touch do not overlap and cards that share area do", () => {
  assert.deepEqual(overlapping([card("A", 0, 0), card("B", 200, 0), card("C", 0, 80)]), []);
  assert.deepEqual(overlapping([card("A", 0, 0), card("B", 150, 40)]), [["A", "B"]]);
});

test("text that leaves its card is reported and text inside it is not", () => {
  assert.deepEqual(spilled([card("A", 0, 0)]), []);
  const wide = card("A", 0, 0, 200, 80, [{ text: "A long task line", x: 8, y: 60, w: 230, h: 16 }]);
  assert.deepEqual(spilled([wide]), [{ card: "A", text: "A long task line" }]);
  const low = card("A", 0, 0, 200, 80, [{ text: "Below", x: 8, y: 70, w: 50, h: 16 }]);
  assert.deepEqual(spilled([low]), [{ card: "A", text: "Below" }]);
});

test("an agent name is a persona with its @role name, never a harness name with a number", () => {
  assert.equal(roleName("Ada (@lead-a)"), true);
  assert.equal(roleName("Grace Hopper (@onboarding-fix)"), true);
  assert.equal(roleName("Claude Code 1"), false);
  assert.equal(roleName("Codex 2"), false);
  assert.equal(roleName("Claude Code (@lead-a)"), false);
  assert.equal(roleName("Ada"), false);
  assert.equal(roleName("Ada (@)"), false);
});

test("the graph line's left-out count is read, and none means zero", () => {
  assert.equal(notShown("3 of 7 agents working. 2 tasks live. 4 agents not shown."), 4);
  assert.equal(notShown("1 of 2 agents working. 1 task live. 1 agent not shown."), 1);
  assert.equal(notShown("1 of 2 agents working. 1 task live."), 0);
});

test("a wheel turn scrolled a terminal when its offset changed or Jump to latest appeared, never for new rows alone", () => {
  assert.equal(moved({ rows: "a\nb", top: 0, jump: false }, { rows: "a\nb", top: 0, jump: false }), false);
  assert.equal(moved({ rows: "a\nb", top: 0, jump: false }, { rows: "z\na", top: 0, jump: false }), false);
  assert.equal(moved({ rows: "a\nb", top: 40, jump: false }, { rows: "a\nb", top: 0, jump: false }), true);
  assert.equal(moved({ rows: "a\nb", top: 0, jump: false }, { rows: "z\na", top: 0, jump: true }), true);
  assert.equal(moved({ rows: "a\nb", top: 0, jump: true }, { rows: "z\na", top: 0, jump: true }), false);
});

test("copied text counts only when it is part of the terminal's rows", () => {
  assert.equal(fromRows("cargo  test\n  ok", "$ cargo test\n ok\n$"), true);
  assert.equal(fromRows("   ", "$ cargo test"), false);
  assert.equal(fromRows("something else", "$ cargo test"), false);
});

test("the finished list row of an agent is the one named after it, never a longer name", () => {
  const rows = [{ name: "Ada (@lead-ab)", harness: "Codex", ended: "Stopped 2 minutes ago" }, { name: "Ada (@lead-a)", harness: "Claude Code", ended: "Stopped by you 1 minute ago" }];
  assert.equal(finishedRow(rows, "Ada (@lead-a)"), rows[1]);
  assert.equal(finishedRow(rows, " Ada (@lead-ab) "), rows[0]);
  assert.equal(finishedRow(rows, "Ada"), undefined);
  assert.equal(finishedRow(rows, "Ada (@lead"), undefined);
  assert.equal(finishedRow(rows, "Bo (@worker)"), undefined);
});

test("the row to copy is the newest one with at least three visible characters", () => {
  assert.equal(copyRow(["$ cargo test", "test ok", "  ", "> ", ""]), 1);
  assert.equal(copyRow(["abc"]), 0);
  assert.equal(copyRow([" a b ", "", "  "]), -1);
  assert.equal(copyRow([]), -1);
});

test("a window is off a project tab when its hash shows another page, another tab or another project", () => {
  assert.equal(offTab("#/projects/ca-trial-4b/agents", "ca-trial-4b", "agents"), null);
  assert.equal(offTab("#/projects/ca-trial-4b/agents?section=finished", "ca-trial-4b", "agents"), null);
  assert.equal(offTab("#/projects/ca%20trial/agents", "ca trial", "agents"), null);
  assert.equal(offTab("#/projects/ca-trial-4b/conversation", "ca-trial-4b", "agents"), "#/projects/ca-trial-4b/conversation");
  assert.equal(offTab("#/settings/memory", "ca-trial-4b", "agents"), "#/settings/memory");
  assert.equal(offTab("#/projects/ca-trial-4/agents", "ca-trial-4b", "agents"), "#/projects/ca-trial-4/agents");
  assert.equal(offTab("#/projects/ca-trial-4b/agents-old", "ca-trial-4b", "agents"), "#/projects/ca-trial-4b/agents-old");
  assert.equal(offTab("", "ca-trial-4b", "agents"), "");
});
