import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const sessions = await import("../src/screens/project/sessions.tsx");
const api = await import("../src/api-projects.ts");

const summary = (id, phase, extra = {}) => ({
  session_id: id, phase, created: "3", last_activity: "5", started_ms: null, active_ms: null, turns: 0,
  first_line: null, last_line: null, agents: [], results_waiting: 0, ...extra,
});

test("a session's start time reads in plain words", () => {
  const now = 10 * 24 * 3_600_000;
  assert.equal(sessions.startedLabel(null, now), "Started earlier");
  assert.equal(sessions.startedLabel(now - 20_000, now), "Started just now");
  assert.equal(sessions.startedLabel(now + 5_000, now), "Started just now");
  assert.equal(sessions.startedLabel(now - 5 * 60_000, now), "Started 5 min ago");
  assert.equal(sessions.startedLabel(now - 3 * 3_600_000, now), "Started 3 h ago");
  assert.equal(sessions.startedLabel(now - 25 * 3_600_000, now), "Started yesterday");
  assert.equal(sessions.startedLabel(now - 4 * 24 * 3_600_000, now), "Started 4 days ago");
});

test("a listed session shows its newest line, agents and waiting results", () => {
  assert.equal(sessions.sessionLine(summary("a", "parked")), "No messages yet");
  assert.equal(sessions.sessionLine(summary("a", "parked", { first_line: "Plan the page" })), "Plan the page");
  assert.equal(sessions.sessionLine(summary("a", "parked", { first_line: "Plan", last_line: "Done, page planned" })), "Done, page planned");
  assert.equal(sessions.agentsLine([]), null);
  assert.equal(sessions.agentsLine(["Ada (@page)", "Ben"]), "Agents: Ada (@page), Ben");
  assert.equal(sessions.waitingBadge(0), null);
  assert.equal(sessions.waitingBadge(1), "1 result waiting");
  assert.equal(sessions.waitingBadge(4), "4 results waiting");
});

test("only parked sessions can be resumed and the live one is found", () => {
  const list = [summary("live", "configured"), summary("old", "parked"), summary("gone", "closed"), summary("older", "parked")];
  assert.equal(sessions.liveSession(list)?.session_id, "live");
  assert.deepEqual(sessions.resumable(list).map((item) => item.session_id), ["old", "older"]);
  assert.equal(sessions.liveSession([summary("old", "parked")]), undefined);
});

test("a running reply asks the owner to confirm before a switch", () => {
  assert.deepEqual(sessions.switchOutcome({ kind: "applied" }), { kind: "done" });
  assert.deepEqual(sessions.switchOutcome({ kind: "rejected", code: "master_turn_running" }), { kind: "confirm" });
  assert.deepEqual(sessions.switchOutcome({ kind: "rejected", code: "master_session_live" }), { kind: "problem", code: "master_session_live" });
  assert.deepEqual(sessions.switchOutcome({ kind: "unconfirmed" }), { kind: "problem", code: "master_outcome_pending" });
  assert.match(api.failureCopy("master_turn_running"), /still answering/);
  assert.match(api.failureCopy("master_session_closed"), /can't be resumed/);
});

test("the session header always shows New session and the Sessions list button", () => {
  const html = renderToStaticMarkup(createElement(sessions.SessionsBar, { ticket: "1", liveId: "live", onSwitched: async () => undefined }));
  assert.match(html, /data-testid="new-session"[^>]*>New session</);
  assert.match(html, /data-testid="sessions-toggle"[^>]*>Sessions</);
  assert.match(html, /No live session/);
});
