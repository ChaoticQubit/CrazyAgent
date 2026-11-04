import assert from "node:assert/strict";
import test from "node:test";
import { cardOf, copyRow, finishedRow, fromRows, moved, notShown, overlapping, redrawShift, roleName, showsTask, spilled, statusWord } from "./agents-cards.mjs";

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

/** A terminal view of an agent that does not read the mouse, as terminalView reads it.
 * Inputs: the rows text, the scroll offset and whether Jump to latest shows. Output: the view. */
function plain(rows, top, jump) {
  return { rows, lines: rows.split("\n"), top, jump, mouse: false };
}

test("a wheel turn scrolled a terminal when its offset changed or Jump to latest appeared, never for new rows alone", () => {
  assert.equal(moved(plain("a\nb", 0, false), plain("a\nb", 0, false), true), false);
  assert.equal(moved(plain("a\nb", 0, false), plain("z\na", 0, false), true), false);
  assert.equal(moved(plain("a\nb", 40, false), plain("a\nb", 0, false), true), true);
  assert.equal(moved(plain("a\nb", 0, false), plain("z\na", 0, true), true), true);
  assert.equal(moved(plain("a\nb", 0, true), plain("z\na", 0, true), true), false);
});

const RULE = "────────────────────────────────────────────────────────────";
const FOOTER = ["", RULE, "❯ ", RULE, "  ▸▸ bypass permissions on (shift+tab to cycle) · esc to interrupt"];
const TRANSCRIPT = [
  "  Do not push.",
  "  Acceptance: python3 -c \"import calc; assert calc.add(2, 3) == 5\" passes",
  "  in your worktree.",
  "  Commit your work on the branch checked out there with conventional",
  "  commit messages before you finish: only commits on that branch land.",
  "  Never switch, rename, delete or push it, create no other branch but your",
  "  teammates' branches named below, never write inside the project",
  "  folder's .git, and never retry a refused command outside the sandbox.",
  "  CrazyAgent fetches your branch when you finish, and the owner's Accept",
  "  in Changes or the master's merge lands your commits.",
  "  When you finish, reply with a short summary of what you did.",
  "  You are a worker of a lead agent: your lead merges your work into its",
  "  task branch.",
  "● Listing 2 directories",
];

/** A Claude Code fullscreen view as terminalView reads it: the agent reads the mouse, the
 * alternate screen has no scrollback, so the offset stays 0 and Jump to latest never shows.
 * Input: its rows. Output: the view. */
function fullscreen(lines) {
  return { rows: lines.join("\n"), lines, top: 0, jump: false, mouse: true };
}

const BOTTOM = fullscreen([...TRANSCRIPT.slice(6, 14), ...FOOTER]);
const UP_THREE = fullscreen([...TRANSCRIPT.slice(3, 10), "● Listing 2 directories, 1 new message (click) ↓", ...FOOTER]);
const NEW_OUTPUT = fullscreen([...TRANSCRIPT.slice(8, 14), "● Read(calc.py)", "  ⎿  Read 4 lines", ...FOOTER]);

test("a redraw shift is how far a block of rows the screen holds once moved down, or up when negative", () => {
  assert.equal(redrawShift(BOTTOM.lines, UP_THREE.lines), 3);
  assert.equal(redrawShift(UP_THREE.lines, BOTTOM.lines), -3);
  assert.equal(redrawShift(BOTTOM.lines, NEW_OUTPUT.lines), -2);
  assert.equal(redrawShift(BOTTOM.lines, BOTTOM.lines), 0);
  assert.equal(redrawShift(["ab", "cd", "ab", "cd", "ab", "cd"], ["cd", "ab", "cd", "ab", "cd", "ab"]), 0);
  assert.equal(redrawShift(["x", "y", "z", "x", "y", "z"], ["x", "y", "z", "q", "r", "s"]), 0);
  assert.equal(redrawShift(["a", "", "b", "c", "d"], ["x", "a", "", "b", "c"]), 0);
  assert.equal(redrawShift(["a   ", "b", "c", "d"], ["z", "a", "b ", "c"]), 1);
});

test("a fullscreen agent that reads the mouse scrolled when its own redraw moved its rows the wheel's way", () => {
  assert.equal(moved(BOTTOM, UP_THREE, true), true);
  assert.equal(moved(BOTTOM, NEW_OUTPUT, true), false);
  assert.equal(moved(BOTTOM, BOTTOM, true), false);
  assert.equal(moved(UP_THREE, BOTTOM, false), true);
  assert.equal(moved(BOTTOM, UP_THREE, false), false);
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
