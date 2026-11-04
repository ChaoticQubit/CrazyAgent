import assert from "node:assert/strict";
import test from "node:test";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";
import * as reply from "./trial4/n-d8-master-reply.mjs";
import * as enter from "./trial4/n-d10-terminal-enter.mjs";
import * as sessions from "./trial4/n-d11-sessions-bar.mjs";
import * as instructions from "./trial4/n-d14-old-session-instructions.mjs";
import * as swap from "./trial4/n-d26-master-switch.mjs";
import * as any from "./trial4/n-d27-any-harness.mjs";
import * as questions from "./trial4/n-d29-question-tags.mjs";

const MODULES = [reply, enter, sessions, instructions, swap, any, questions];
const DOT = "\u23fa";

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

test("the master has ended when it has no agent row or a stopped status; running agents are counted", () => {
  const live = reply.masterState([{ kind: "master", status: "working", harness: "claude-code" }, { kind: "lead", status: "idle" }, { kind: "sub", status: "stopped" }]);
  assert.equal(live.ended, false);
  assert.equal(live.working, 1);
  assert.equal(reply.masterState([{ kind: "master", status: null }]).ended, true);
  assert.equal(reply.masterState([{ kind: "master", status: "failed" }]).ended, true);
  assert.equal(reply.masterState([{ kind: "lead", status: "working" }]).ended, true);
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
