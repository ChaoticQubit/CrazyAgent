import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";
import { pasteTerminal } from "./trial-kit.mjs";
import * as reply from "./trial4/n-d8-master-reply.mjs";
import * as enter from "./trial4/n-d10-terminal-enter.mjs";
import * as sessions from "./trial4/n-d11-sessions-bar.mjs";
import * as instructions from "./trial4/n-d14-old-session-instructions.mjs";
import * as swap from "./trial4/n-d26-master-switch.mjs";
import * as any from "./trial4/n-d27-any-harness.mjs";
import * as questions from "./trial4/n-d29-question-tags.mjs";

const MODULES = [reply, enter, sessions, instructions, swap, any, questions];
const DOT = "\u23fa";
const MESSAGE = "Build the whole trial plan now, then report.";
const RULE = "\u2500".repeat(20);
const PLACEHOLDER = "> [Pasted text #1 +17 lines]";

/** A harness's ruled input box. Inputs: the input line and the hint under the box. Output: its rows. */
const box = (line = "> ", hint = "? for shortcuts") => [RULE, line, RULE, hint];
const IDLE = box();
const PASTED = box(PLACEHOLDER);
const SENT = [`> ${MESSAGE}`, `${DOT} Working on it`, ...box()];

/** A terminal a send runs against, with no window. Input: the screen, a function of what the
 * terminal has seen so far. Output: the terminal, with what it saw under `seen`. */
function fakeTerminal(screen) {
  const seen = { pasted: false, presses: 0, reads: 0, readsAtPaste: 0, readsAtEnter: [] };
  return {
    seen,
    rows: async () => { seen.reads += 1; return screen(seen); },
    paste: async () => { seen.pasted = true; seen.readsAtPaste = seen.reads; },
    enter: async () => { seen.presses += 1; seen.readsAtEnter.push(seen.reads); },
    pause: async () => {},
  };
}

/** Runs a body with a window that has no message box under the master and whose terminal
 * rows come from a function. Inputs: the rows function, given what was seen, whether the
 * terminal keeps the focus, and the body. Output: the body's result; the globals are removed. */
async function withBoxlessWindow(rows, focused, body) {
  const seen = { pasted: false, enters: 0, reads: 0, readsAtPaste: 0, readsAtEnter: 0, clicks: 0 };
  globalThis.$$ = async () => [];
  globalThis.$ = () => ({ click: async () => { seen.clicks += 1; } });
  globalThis.browser = {
    execute: async (fn) => {
      if (fn === pasteTerminal) { seen.pasted = focused; seen.readsAtPaste = seen.reads; return focused; }
      if (!fn.toString().includes("xterm-rows")) return undefined;
      seen.reads += 1;
      return rows(seen);
    },
    keys: async () => { seen.enters += 1; seen.readsAtEnter = seen.reads; },
  };
  try {
    return await body(seen);
  } finally {
    delete globalThis.$$;
    delete globalThis.$;
    delete globalThis.browser;
  }
}

test("every master check module names one check of the plan, runs after a trial 3 step or at the end, and quotes the owner", () => {
  const slugs = new Set([...TRIAL3_STEPS.map((step) => step.slug), "end"]);
  const ids = MODULES.flatMap((module) => module.checks.map((check) => check.id));
  assert.deepEqual(ids, ["N-D8", "N-D10", "N-D11", "N-D14", "N-D26", "N-D27", "N-D29"]);
  for (const check of MODULES.flatMap((module) => module.checks)) {
    assert.ok(check.op.length > 0 && check.op.every((op) => /^OP-\d+$/.test(op)), check.id);
    assert.ok(slugs.has(check.after), `${check.id} runs after ${check.after}`);
    assert.match(check.why, /^owner: /, check.id);
    assert.equal(typeof check.run, "function", check.id);
  }
});

test("a reply line is the harness's mark followed by text; the owner's prompt and a bare mark are not", () => {
  const rows = ["> Trial prompt", `${DOT} Starting the team now.`, `  ${DOT}   `, "", `  ${DOT} Bash(git status)`];
  assert.equal(reply.replyLines(rows, "claude-code"), 2);
  assert.equal(reply.replyLines(rows, "claude"), 2);
  assert.equal(reply.replyLines(["\u2022 Ran the plan"], "codex"), 1);
  assert.equal(reply.replyLines(rows, "codex"), 0);
  assert.throws(() => reply.replyLines(rows, "rcli"), /no reply mark is known for the harness rcli/);
});

test("a message is known by its first characters, and one with no text is refused", () => {
  assert.equal(reply.messageStart(`  ${MESSAGE}\nthe second line`), "Build the whole tria");
  assert.equal(reply.messageStart("One line\nthe second line"), "One line");
  assert.throws(() => reply.messageStart(" \n "), /a message to send has text/);
});

test("the input area is between the last two rules, or with no box from the last prompt to the bottom", () => {
  assert.deepEqual(reply.inputRows([RULE, "an old pair", RULE, "transcript", RULE, "> typed", "  more", RULE, "hint"]), ["> typed", "  more"]);
  assert.deepEqual(reply.inputRows([`  ${RULE}  `, "> a", `${RULE} note`, RULE]), ["> a", `${RULE} note`]);
  assert.deepEqual(reply.inputRows(["\u203a old message", `${DOT} reply`, "\u203a typed", "  more", "footer"]), ["\u203a typed", "  more", "footer"]);
  assert.deepEqual(reply.inputRows(["nothing to type into"]), []);
});

test("a paste waits while the input area holds its start or the placeholder, wherever that area has moved", () => {
  const start = reply.messageStart(MESSAGE);
  assert.equal(reply.pasteWaiting(start, box(`> ${MESSAGE.slice(0, 30)}`)), true);
  assert.equal(reply.pasteWaiting(start, ["pushed", "down", ...PASTED]), true);
  assert.equal(reply.pasteWaiting(start, IDLE), false);
  assert.equal(reply.pasteWaiting(start, [`> ${MESSAGE}`, ...IDLE]), false);
});

test("a pasted message gets one Enter, and only after it has drawn and stopped changing", async () => {
  const terminal = fakeTerminal((seen) => {
    if (seen.presses > 0) return SENT;
    const since = seen.pasted ? seen.reads - seen.readsAtPaste : 0;
    if (since < 3) return IDLE;
    if (since === 3) return box("> [Pasted text");
    return since === 4 ? box("> [Pasted text #1") : PASTED;
  });
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE), 1);
  assert.equal(terminal.seen.presses, 1);
  assert.equal(terminal.seen.readsAtEnter[0], terminal.seen.readsAtPaste + 6);
});

test("a first Enter that only clears a notice is followed by a second that sends", async () => {
  const terminal = fakeTerminal((seen) => (seen.presses >= 2 ? SENT : seen.pasted ? PASTED : IDLE));
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE), 2);
});

test("a paste moved up a row by the notice its first Enter cleared still reads as unsent", async () => {
  const terminal = fakeTerminal((seen) => {
    if (seen.presses >= 2) return SENT;
    if (seen.presses === 1) return PASTED;
    return ["Update available", ...(seen.pasted ? PASTED : IDLE)];
  });
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE), 2);
  assert.equal(terminal.seen.presses, 2);
});

test("a status row ticking once a second is not taken for the paste", async () => {
  const terminal = fakeTerminal((seen) => [...IDLE, `running hooks ${Math.floor(seen.reads / 4)}s`]);
  await assert.rejects(reply.pasteAndSend(terminal, MESSAGE), /the paste never drew/);
  assert.equal(terminal.seen.presses, 0);
});

test("a sent message echoed on the row it was typed on, with the box moved below, reads as sent", async () => {
  const typed = box(`> ${MESSAGE}`);
  const terminal = fakeTerminal((seen) => {
    if (seen.presses > 0) return ["", `> ${MESSAGE}`, "", ...IDLE];
    return seen.pasted ? typed : IDLE;
  });
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE), 1);
});

test("a dialog drawn in place of the input box is not read as sent", async () => {
  const DIALOG = ["Do you trust the files in this folder?", "  1. Yes, continue", "  2. No, exit", "Enter to confirm"];
  const terminal = fakeTerminal((seen) => (seen.presses > 0 ? DIALOG : seen.pasted ? PASTED : IDLE));
  await assert.rejects(reply.pasteAndSend(terminal, MESSAGE), /after 4 Enter presses: it was not sent/);
  assert.equal(terminal.seen.presses, reply.ENTER_PRESSES_MAX);
});

test("an input box hidden for a moment after Enter and drawn empty again reads as sent on one press", async () => {
  const terminal = fakeTerminal((seen) => {
    if (seen.presses === 0) return seen.pasted ? PASTED : IDLE;
    return seen.reads - seen.readsAtEnter[0] <= 3 ? ["Working"] : IDLE;
  });
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE), 1);
  assert.equal(terminal.seen.presses, 1);
});

test("a paste that never leaves the input area fails after the last Enter instead of reading as sent", async () => {
  const terminal = fakeTerminal((seen) => (seen.pasted ? PASTED : IDLE));
  await assert.rejects(reply.pasteAndSend(terminal, MESSAGE), /after 4 Enter presses: it was not sent/);
  assert.equal(terminal.seen.presses, reply.ENTER_PRESSES_MAX);
  assert.equal(terminal.seen.reads, 2 + reply.ENTER_PRESSES_MAX * reply.ENTER_READS_MAX);
});

test("a paste that never draws fails with no Enter pressed", async () => {
  const terminal = fakeTerminal(() => IDLE);
  await assert.rejects(reply.pasteAndSend(terminal, MESSAGE), /the paste never drew/);
  assert.equal(terminal.seen.presses, 0);
  assert.equal(terminal.seen.reads, reply.DRAW_READS_MAX);
});

test("an animated row neither counts as the paste nor as the send", async () => {
  const spinner = (seen) => `Thinking ${seen.reads}`;
  const silent = fakeTerminal((seen) => [...IDLE, spinner(seen)]);
  await assert.rejects(reply.pasteAndSend(silent, MESSAGE), /the paste never drew/);
  assert.equal(silent.seen.presses, 0);
  const typed = fakeTerminal((seen) => [...box(seen.presses >= 2 ? "> " : seen.pasted ? PLACEHOLDER : "> "), spinner(seen)]);
  assert.equal(await reply.pasteAndSend(typed, MESSAGE), 2);
});

test("a message with no text is refused before anything is pasted", async () => {
  const terminal = fakeTerminal(() => IDLE);
  await assert.rejects(reply.pasteAndSend(terminal, "  "), /a message to send has text/);
  assert.equal(terminal.seen.pasted, false);
});

test("a terminal that draws no echo is sent on the caller's read of a started turn, with no draw to wait for", async () => {
  const terminal = fakeTerminal(() => IDLE);
  assert.equal(await reply.pasteAndSend(terminal, MESSAGE, async () => terminal.seen.presses >= 2), 2);
  assert.equal(terminal.seen.readsAtEnter[0], 0);
  const stuck = fakeTerminal(() => IDLE);
  await assert.rejects(reply.pasteAndSend(stuck, MESSAGE, async () => false), /it was not sent/);
  assert.equal(stuck.seen.presses, reply.ENTER_PRESSES_MAX);
});

test("sendToMaster with no box proves the paste on the real rows before Enter", async () => {
  const rows = (seen) => (seen.enters > 0 ? SENT : seen.pasted ? PASTED : IDLE);
  await withBoxlessWindow(rows, true, async (seen) => {
    assert.equal(await reply.sendToMaster(null, "The whole prompt"), "terminal");
    assert.equal(seen.clicks, 1);
    assert.equal(seen.enters, 1);
    assert.equal(seen.readsAtEnter, seen.readsAtPaste + 2);
  });
});

test("sendToMaster stops with no Enter when the terminal lost the focus, and hands a caller's read to the send", async () => {
  await withBoxlessWindow(() => IDLE, false, async (seen) => {
    await assert.rejects(reply.sendToMaster(null, "The whole prompt"), /lost the focus before the paste/);
    assert.equal(seen.enters, 0);
  });
  await withBoxlessWindow(() => IDLE, true, async (seen) => {
    assert.equal(await reply.sendToMaster(null, "A line", async () => seen.enters > 0), "terminal");
    assert.equal(seen.enters, 1);
  });
});

test("the master has ended when it has no agent row or a stopped status; running agents are counted", () => {
  const live = reply.masterState([{ kind: "master", status: "working", harness: "claude-code" }, { kind: "lead", status: "idle" }, { kind: "sub", status: "stopped" }]);
  assert.equal(live.ended, false);
  assert.equal(live.working, 1);
  assert.equal(reply.masterState([{ kind: "master", status: null }]).ended, true);
  assert.equal(reply.masterState([{ kind: "master", status: "failed" }]).ended, true);
  assert.equal(reply.masterState([{ kind: "lead", status: "working" }]).ended, true);
});

test("Start all is decided from the engine's team read of this project, never from no read", () => {
  assert.equal(reply.pausedOf({ project_id: "ca-trial-4", paused: true, keep_warm: true }, "ca-trial-4"), true);
  assert.equal(reply.pausedOf({ project_id: "ca-trial-4", paused: false, keep_warm: true }, "ca-trial-4"), false);
  assert.throws(() => reply.pausedOf({ failed: "engine_unavailable" }, "ca-trial-4"), /desktop_team_status failed: engine_unavailable/);
  assert.throws(() => reply.pausedOf({ project_id: "ca-trial-4b", paused: true }, "ca-trial-4"), /answered for "ca-trial-4b", not ca-trial-4/);
  assert.throws(() => reply.pausedOf({ project_id: "ca-trial-4" }, "ca-trial-4"), /no paused flag/);
  assert.throws(() => reply.pausedOf(null, "ca-trial-4"), /answered for undefined/);
});

test("the Sessions bar line gives the live session's messages, 0 for a new one and null for none", () => {
  assert.equal(reply.messageCount("Live session \u00b7 started 3 min ago \u00b7 12 messages"), 12);
  assert.equal(reply.messageCount("Live session \u00b7 started now \u00b7 1 message"), 1);
  assert.equal(reply.messageCount("Live session \u00b7 no messages yet"), 0);
  assert.equal(reply.messageCount("No live session"), null);
});

test("two typed lines are one message only when no reply sits between them, and done once a reply follows", () => {
  const [first, second] = enter.LINES;
  const typing = [`> ${first}`, `  ${second}`];
  assert.deepEqual(enter.oneMessage(typing, enter.LINES, "claude-code"), { done: false, ok: false, seen: "both lines are one message; no reply under it yet" });
  const answered = ["older", `> ${first}`, `  ${second}`, "", `${DOT} ok`, "> "];
  assert.equal(enter.oneMessage(answered, enter.LINES, "claude-code").ok, true);
  const split = [`> ${first}`, `${DOT} Waiting for more.`, `> ${second}`, `${DOT} ok`];
  assert.deepEqual(enter.oneMessage(split, enter.LINES, "claude-code"), { done: true, ok: false, seen: "a reply sits between the two lines: they arrived as two messages" });
  const early = [`> ${first}`, `${DOT} What else?`];
  assert.equal(enter.oneMessage(early, enter.LINES, "claude-code").seen, "the harness answered the first line alone: Shift+Enter sent it");
  assert.equal(enter.oneMessage(["> nothing"], enter.LINES, "claude-code").done, false);
  assert.ok(enter.LINES.every((line) => line.length < 60), "each typed line fits one terminal row");
});

test("the Sessions list picks the one session that used agents as the trial and the newest without agents as scratch", () => {
  assert.equal(sessions.listedCount("Sessions (3)"), 3);
  assert.equal(sessions.listedCount("Sessions"), 0);
  const items = ["Today 14:02 ok", "Today 13:40 Trial prompt Agents: Ada (@lead-a), Bo (@lead-b)", "Yesterday hello"];
  assert.equal(sessions.trialItem(items), 1);
  assert.equal(sessions.scratchItem(items), 0);
  assert.throws(() => sessions.trialItem(["a", "b"]), /expected one resumable session that used agents, found 0 of 2/);
  assert.throws(() => sessions.scratchItem([items[1]]), /no resumable session without agents among 1/);
});

test("the marked instruction line is added after a blank line and only carries a plain token", () => {
  assert.deepEqual(instructions.addMarkedLine("# Rules\nBe brief.\n\n", "t4-nd14-0a1b"), {
    line: "Trial 4 marker line: t4-nd14-0a1b",
    edited: "# Rules\nBe brief.\n\nTrial 4 marker line: t4-nd14-0a1b\n",
  });
  assert.equal(instructions.addMarkedLine("", "t4-x").edited, "Trial 4 marker line: t4-x\n");
  assert.throws(() => instructions.addMarkedLine("x", "Bad Token"), /lowercase letters, digits and dashes/);
  assert.ok(!instructions.QUESTION.includes("t4-nd14"), "the question never gives the token away");
});

test("a master switch settles on one divider, one message box and the session's messages", () => {
  const good = { console: false, boxes: 1, divider: "Switched to Codex. The conversation carried over.", title: "Codex", rows: 12, messages: 4 };
  assert.equal(swap.switchSeen(good, "Codex", 4).ok, true);
  assert.deepEqual(swap.switchSeen({ ...good, boxes: 2 }, "Codex", 4), { done: true, ok: false, seen: "the master area shows 2 message boxes" });
  assert.equal(swap.switchSeen({ ...good, console: true }, "Codex", 4).ok, false);
  assert.equal(swap.switchSeen({ ...good, divider: "" }, "Codex", 4).done, false);
  assert.equal(swap.switchSeen({ ...good, rows: 0 }, "Codex", 4).done, false);
  assert.equal(swap.switchSeen({ ...good, messages: 0 }, "Codex", 4).seen, "the live session reads 0 messages, not the 4 it had");
  assert.deepEqual(swap.SWITCHES.map((item) => item.id), ["codex", "claude-code"]);
});

test("an empty plan lists every runnable agent harness but the master's own row", () => {
  const status = { harnesses: [
    { harness: "claude-code", role: "master", runnable: true },
    { harness: "claude-code", role: "sub", runnable: true },
    { harness: "codex", role: "sub", runnable: true },
    { harness: "codex", role: "teammate", runnable: true },
    { harness: "rcli", role: "sub", runnable: false },
  ] };
  assert.deepEqual(any.agentHarnesses(status), ["claude-code", "codex"]);
  assert.throws(() => any.agentHarnesses({}), /the team status has no harness list/);
  assert.match("Any harness, up to 63 agents", any.ANY_TOTAL);
  assert.match("Any harness, up to 1 agent", any.ANY_TOTAL);
  assert.doesNotMatch("No sub-agents (team maximum is 1)", any.ANY_TOTAL);
});

test("a question row's tags are the badge it opens with and its For: tag", () => {
  assert.deepEqual(questions.questionTags("WaitingAda (@lead-a)The agent waits for your answer.Which branch?"), { severity: "Waiting", forWhom: null });
  assert.deepEqual(questions.questionTags("UrgentFor: MeAda (@lead-a)The agent is paused until you answer."), { severity: "Urgent", forWhom: "Me" });
  assert.deepEqual(questions.questionTags("Carrying on For: @lead-a Is the default fine?"), { severity: "Carrying on", forWhom: "@lead-a" });
  assert.equal(questions.questionTags("Ada asks: Urgent? For:").severity, null);
});

test("N-D14 resumes the master session the 18.14 record step wrote in ca-trial-4c, and its title names that hop", () => {
  assert.equal(instructions.OLD_PROJECT, "ca-trial-4c");
  const [check] = instructions.checks;
  assert.equal(check.id, "N-D14");
  assert.match(check.title, /^A master session recorded on 18\.14 resumes on 18\.15 /u);
});

test("N-D14 passes only on the live session the 18.14 record step wrote, never on another or on no read", () => {
  const RECORDED = "master-215080c51594223929c2c5f79d08b6ef09e86c82b1c8bba4a1d895cc1f91da33";
  assert.equal(instructions.RECORDED_SESSION, RECORDED);
  const state = (session) => ({ project_id: "ca-trial-4c", ticket: "ticket-1", session });
  assert.equal(instructions.liveSessionOf(state({ session_id: RECORDED, phase: "configured", current_turn: null }), "ca-trial-4c"), RECORDED);
  assert.equal(instructions.liveSessionOf(state({ session_id: "master-new", phase: "configured", current_turn: null }), "ca-trial-4c"), "master-new");
  assert.throws(() => instructions.liveSessionOf(state({ session_id: RECORDED, phase: "parked", current_turn: null }), "ca-trial-4c"), /ca-trial-4c has no live master session/);
  assert.throws(() => instructions.liveSessionOf(state(null), "ca-trial-4c"), /ca-trial-4c has no live master session/);
  assert.throws(() => instructions.liveSessionOf({ failed: "engine_unavailable" }, "ca-trial-4c"), /desktop_master_state failed: engine_unavailable/);
  assert.throws(() => instructions.liveSessionOf({ ...state(null), project_id: "ca-trial-4b" }, "ca-trial-4c"), /answered for "ca-trial-4b", not ca-trial-4c/);
});

test("N-D14 fails on either note the engine posts when a session goes on in a new harness conversation", () => {
  const source = readFileSync(new URL("../../../../crates/crazyagent/src/master_sessions.rs", import.meta.url), "utf8");
  assert.ok(source.includes(`pub const FRESH_NOTE: &str = ${JSON.stringify(instructions.FRESH_NOTE)};`), "FRESH_NOTE is the engine's text");
  assert.ok(source.includes(`pub const LOST_NOTE: &str = ${JSON.stringify(instructions.LOST_NOTE)};`), "LOST_NOTE is the engine's text");
  const status = (messages) => ({ project_id: "ca-trial-4c", paused: false, messages });
  const said = { id: 1, author: "master", text: instructions.FRESH_NOTE, state: "reply", created_ms: 1 };
  assert.deepEqual(instructions.resumeNotes(status([said, { id: 2, author: "engine", text: "Team started.", state: "note", created_ms: 2 }]), "ca-trial-4c"), []);
  assert.deepEqual(instructions.resumeNotes(status([{ ...said, state: "note" }, { ...said, id: 3, text: instructions.LOST_NOTE, state: "note" }]), "ca-trial-4c"), [instructions.FRESH_NOTE, instructions.LOST_NOTE]);
  assert.throws(() => instructions.resumeNotes({ project_id: "ca-trial-4c", paused: false }, "ca-trial-4c"), /desktop_team_status answered no messages for ca-trial-4c/);
  assert.throws(() => instructions.resumeNotes({ failed: "engine_unavailable" }, "ca-trial-4c"), /desktop_team_status failed: engine_unavailable/);
  assert.throws(() => instructions.resumeNotes(status([]), "ca-trial-4b"), /answered for "ca-trial-4c", not ca-trial-4b/);
});
