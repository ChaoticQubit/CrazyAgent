import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const stored = await import("../src/screens/project/session-messages.tsx");
const { consoleLines, turnLines } = await import("../src/screens/project/conversation.tsx");
const sessions = await import("../src/screens/project/sessions.tsx");

const turn = (patch) => ({ id: "t1", ordinal: 1, phase: "succeeded", reason: null, reply: null, ...patch });
const row = (seq, speaker, mode, text, harness = mode === "live" ? "claude-code" : "rcli") => ({ seq, speaker, mode, harness, text, at_ms: 1000 + seq });
const lines = (rows, turns, sent = {}) => stored.conversationLines(rows, turns, (one) => turnLines(one, sent, {}, 0));
const said = (list) => list.map((line) => `${line.kind}: ${line.text}`);
const SWITCHED = "Switched to Claude Code. The conversation continues: Claude Code got its earlier turns, newest first, and the open tasks.";
const BACK = "Switched to One-turn master (RCLI). The conversation continues.";
const CATALOG = [{ id: "claude-code", aliases: ["claude"], name: "Claude Code" }, { id: "codex", aliases: [], name: "Codex" }];

test("a message names the master that ran it, in both master modes", () => {
  assert.equal(stored.ranBy("live", "claude-code", CATALOG), "Claude Code");
  assert.equal(stored.ranBy("live", "codex", CATALOG), "Codex");
  assert.equal(stored.ranBy("live", "codex", null), "Codex", "before the catalog is read");
  assert.equal(stored.ranBy("live", null, CATALOG), "Live master");
  assert.equal(stored.ranBy("one_turn", "rcli", null), "One-turn master (RCLI)");
  assert.equal(stored.ranBy("one_turn", "codex", null), "One-turn master (Codex)");
  assert.equal(stored.ranBy("one_turn", "claude", null), "One-turn master (Claude)");
  assert.equal(stored.ranBy("one_turn", null, null), "One-turn master");
  assert.equal(stored.ranBy("live", "claude", CATALOG), "Claude Code", "an alias names the same harness");
});

test("a read with no page is a failed read, never an empty conversation", () => {
  assert.equal(stored.pageOf(undefined), null);
  assert.equal(stored.pageOf({}), null);
  assert.equal(stored.pageOf({ page: { messages: "none", next: null, count: 0 } }), null);
  assert.equal(stored.pageOf({ page: { messages: [], next: "3", count: 0 } }), null);
  const page = { project_id: "p", session_id: "s", messages: [], next: null, count: 0 };
  assert.equal(stored.pageOf({ ticket: "t", revision: "1", page }), page);
});

test("newer reads merge by sequence number and a terminal turn's owner row gains its words", () => {
  const held = [row(1, "owner", "live", "plan"), row(2, "master", "live", "planned"), row(3, "owner", "live", null)];
  const merged = stored.mergeMessages(held, [row(4, "master", "live", "shipped"), row(3, "owner", "live", "ship it")]);
  assert.deepEqual(merged.map((one) => one.seq), [1, 2, 3, 4]);
  assert.equal(merged[2].text, "ship it", "the row with the same sequence number replaces the held one");
  assert.deepEqual(stored.mergeMessages([], []), []);
});

test("stored messages alone read as the owner's words, replies, engine requests and dividers", () => {
  const rows = [
    row(1, "owner", "live", null), row(2, "master", "live", "launch shipped"), row(3, "engine", "live", "A result came in."),
    row(4, "switch", "one_turn", BACK), row(5, "master", "live", null),
  ];
  assert.deepEqual(said(stored.messageLines(rows)), [
    `engine: ${stored.NO_TEXT}`, "reply: launch shipped", "engine: A result came in.", `engine: ${BACK}`,
  ]);
  assert.deepEqual(stored.messageLines(rows).map((line) => line.id), ["m-1", "m-2", "m-3", "m-4-switch"]);
});

test("a move from the one-turn master to Claude Code keeps the earlier turns above the Switched to divider", () => {
  const rows = [
    row(1, "owner", "one_turn", "plan the launch"), row(2, "master", "one_turn", "launch planned"),
    row(3, "switch", "live", SWITCHED),
    row(4, "owner", "live", null), row(5, "master", "live", "launch shipped"),
  ];
  const turns = [turn({ id: "t1", reply: { text: "launch planned" }, tasks: [{ task: 8, note: null }] })];
  const shown = said(lines(rows, turns, { t1: "plan the launch" }));
  assert.equal(shown[0], "owner: plan the launch");
  assert.equal(shown[1], "reply: launch planned");
  assert.match(shown[2], /^delegate: /, "the turn keeps its task line, which no stored message holds");
  assert.deepEqual(shown.slice(3), [`engine: ${SWITCHED}`, `engine: ${stored.NO_TEXT}`, "reply: launch shipped"]);
  assert.equal(shown.filter((line) => line === "reply: launch planned").length, 1, "the one-turn reply shows once, through its turn");
});

test("a move from Claude Code to the one-turn master shows the terminal turns first, then the divider, then the turns", () => {
  const rows = [
    row(1, "owner", "live", "draft the page"), row(2, "master", "live", "page drafted"),
    row(3, "switch", "one_turn", BACK),
    row(4, "owner", "one_turn", "now publish it"), row(5, "master", "one_turn", "published"),
  ];
  const turns = [turn({ id: "t1", reply: { text: "published" } })];
  assert.deepEqual(said(lines(rows, turns, { t1: "now publish it" })), [
    "owner: draft the page", "reply: page drafted", `engine: ${BACK}`, "owner: now publish it", "reply: published",
  ]);
});

test("a Codex terminal session continued by a one-turn Claude master names both masters in the opened session", () => {
  const rows = [
    row(1, "owner", "live", "start", "codex"), row(2, "master", "live", "started", "codex"),
    row(3, "switch", "one_turn", "Switched to One-turn master (Claude).", "claude"),
    row(4, "owner", "one_turn", "go on", "claude"), row(5, "master", "one_turn", "going on", "claude"),
  ];
  const list = stored.messageRows(rows, CATALOG);
  assert.deepEqual(list.map((one) => one.kind), ["owner", "master", "switch", "owner", "master"]);
  assert.deepEqual(list.filter((one) => one.kind !== "switch").map((one) => one.who), ["You", "Codex", "You", "One-turn master (Claude)"]);
  const html = renderToStaticMarkup(createElement(stored.MessageList, { rows: list }));
  assert.match(html, /role="separator"[^>]*data-testid="message-switch"[^>]*>Switched to One-turn master \(Claude\)\./);
  assert.equal(html.match(/data-testid="session-message"/g).length, 4);
  assert.ok(html.indexOf("started") < html.indexOf("Switched to") && html.indexOf("Switched to") < html.indexOf("going on"), html);
});

test("a turn the store has no row for yet comes last and a stored row with no turn shows as stored", () => {
  const rows = [row(1, "owner", "one_turn", "first"), row(2, "master", "one_turn", "first done"), row(3, "switch", "live", SWITCHED)];
  const running = [turn({ id: "t1", reply: { text: "first done" } }), turn({ id: "t2", ordinal: 2, phase: "running" })];
  const shown = said(lines(rows, running, { t1: "first", t2: "second" }));
  assert.deepEqual(shown.slice(0, 4), ["owner: first", "reply: first done", `engine: ${SWITCHED}`, "owner: second"]);
  assert.equal(shown.length, 5, "the running turn's status line is the last line");

  const ahead = [...rows.slice(0, 2), row(3, "owner", "one_turn", "second"), row(4, "master", "one_turn", "second done")];
  assert.deepEqual(said(lines(ahead, [running[0]], { t1: "first" })), ["owner: first", "reply: first done", "owner: second", "reply: second done"]);
  assert.deepEqual(said(lines([row(1, "master", "one_turn", "orphan")], [])), ["reply: orphan"]);
});

test("a session only the one-turn master ran reads exactly as its turns do", () => {
  const turns = [
    turn({ id: "t1", reply: { text: "I handed it to Ada." }, tasks: [{ task: 8, note: null }], changes: "Its edits wait in Changes." }),
    turn({ id: "t2", ordinal: 2, phase: "failed", reason: "quota_exhausted" }),
    turn({ id: "t3", ordinal: 3, reply: { text: "Done." } }),
  ];
  const rows = [
    row(1, "owner", "one_turn", "a"), row(2, "master", "one_turn", "I handed it to Ada."),
    row(3, "owner", "one_turn", "b"),
    row(4, "engine", "one_turn", "c"), row(5, "master", "one_turn", "Done."),
  ];
  const sent = { t1: "a", t2: "b" };
  assert.deepEqual(lines(rows, turns, sent), consoleLines(turns, sent, {}, 0));
  assert.deepEqual(lines([], turns, sent), consoleLines(turns, sent, {}, 0), "with no stored message the view is the turns alone");
});

test("the live terminal session's conversation starts shown only when the store holds what the terminal does not", () => {
  assert.equal(stored.startsShown([row(1, "owner", "live", "a"), row(2, "master", "live", "b")]), false);
  assert.equal(stored.startsShown([row(1, "owner", "one_turn", "a")]), true);
  assert.equal(stored.startsShown([row(1, "owner", "live", "a"), row(2, "switch", "live", SWITCHED)]), true);
  assert.equal(stored.storedCount(1), "1 stored message");
  assert.equal(stored.storedCount(14), "14 stored messages");
  const empty = renderToStaticMarkup(createElement(stored.LiveConversation, { ticket: "t", sessionId: "s", mark: null, harnesses: null }));
  assert.equal(empty, "", "nothing shows before a read arrives");
  const opened = renderToStaticMarkup(createElement(stored.SessionMessages, { ticket: "t", sessionId: "s", harnesses: null }));
  assert.match(opened, /data-testid="session-messages"/);
  assert.match(opened, /Reading the messages\.\.\./);
});

test("a listed session shows its count, the master that ran it last and its first line under its newest", () => {
  const summary = (extra) => ({
    session_id: "a", phase: "parked", created: "3", last_activity: "5", started_ms: null, active_ms: null, turns: 0,
    first_line: null, last_line: null, agents: [], results_waiting: 0, ...extra,
  });
  assert.equal(sessions.countLabel(0), "no messages yet");
  assert.equal(sessions.countLabel(1), "1 message");
  assert.equal(sessions.countLabel(15), "15 messages");
  assert.equal(sessions.sessionHead(summary({ turns: 15, mode: "live", harness: "claude-code" }), 0, CATALOG), "Started earlier · 15 messages · Claude Code");
  assert.equal(sessions.sessionHead(summary({ turns: 14, mode: "one_turn", harness: "rcli" }), 0, null), "Started earlier · 14 messages · One-turn master (RCLI)");
  assert.equal(sessions.sessionHead(summary({ turns: 2 }), 0, null), "Started earlier · 2 messages", "an engine that names no master shows none");
  assert.equal(sessions.sessionHead(summary({ turns: 2, mode: null, harness: null }), 0, null), "Started earlier · 2 messages");
  assert.equal(sessions.firstLine(summary({})), null);
  assert.equal(sessions.firstLine(summary({ first_line: "Plan the page" })), null, "one line is shown once");
  assert.equal(sessions.firstLine(summary({ first_line: "Plan", last_line: "Plan" })), null);
  assert.equal(sessions.firstLine(summary({ first_line: "Plan the page", last_line: "Done, page planned" })), "First message: Plan the page");
});
