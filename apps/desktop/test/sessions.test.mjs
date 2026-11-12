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

test("a session the current settings or engine run no longer admit says to start a new one", () => {
  assert.match(sessions.switchProblem("resume", "master_source_conflict"), /Start a new session/);
  assert.equal(sessions.switchProblem("new", "master_source_conflict"), api.failureCopy({ code: "master_source_conflict" }));
  assert.equal(sessions.switchProblem("resume", "master_session_live"), api.failureCopy({ code: "master_session_live" }));
});

test("the session header always shows New session and the Sessions list button", () => {
  const html = renderToStaticMarkup(createElement(sessions.SessionsBar, { ticket: "1", liveId: "live", onSwitched: async () => undefined }));
  assert.match(html, /data-testid="new-session"[^>]*>New session</);
  assert.match(html, /data-testid="sessions-toggle"[^>]*>Sessions</);
  assert.match(html, /No live session/);
});

test("a resumed session asks only for the owner lines it does not hold, at most 32 at once", () => {
  const turn = (id, cursor) => ({ id, ordinal: 1, phase: "succeeded", reason: null, reply: null, input_cursor: cursor });
  const turns = [turn("a", "11"), turn("b", "12"), turn("c", "13"), { id: "d", ordinal: 4, phase: "queued", reason: null, reply: null }];
  assert.deepEqual(sessions.missingInputs(turns, { b: "typed here" }, { "13": "Held" }), ["11"]);
  assert.deepEqual(sessions.missingInputs(turns, {}, { "11": "x", "12": "y", "13": "z" }), []);
  const many = Array.from({ length: 40 }, (_, index) => turn(`t${index}`, String(100 + index)));
  const asked = sessions.missingInputs(many, {}, {});
  assert.equal(asked.length, 32);
  assert.equal(asked[31], "139");
  assert.equal(sessions.ownerText(turns[0], {}, { "11": "Plan the first slice" }), "Plan the first slice");
  assert.equal(sessions.ownerText(turns[1], { b: "typed here" }, { "12": "older copy" }), "typed here");
  assert.equal(sessions.ownerText(turns[3], {}, {}), undefined);
});

test("a session whose master harness switched shows the engine's Switched to divider", async () => {
  const note = "Switched to Codex. The conversation continues: Codex got its earlier turns, newest first, and the open tasks.";
  const team = (extra) => ({ kind: "ok", value: { project_id: "p-1", ...extra } });
  assert.equal(sessions.switchDivider(undefined), null);
  assert.equal(sessions.switchDivider({ kind: "error", code: "x", message: "m" }), null);
  assert.equal(sessions.switchDivider(team({})), null);
  assert.equal(sessions.switchDivider(team({ live_switch: null })), null);
  assert.equal(sessions.switchDivider(team({ live_switch: { harness: "Codex", mode: "carry", note } })), note);
  assert.equal(renderToStaticMarkup(createElement(sessions.SwitchDivider, { team: team({}) })), "");
  const shown = renderToStaticMarkup(createElement(sessions.SwitchDivider, { team: team({ live_switch: { harness: "Codex", mode: "carry", note } }) }));
  assert.match(shown, /role="separator"/);
  assert.match(shown, /data-testid="harness-switch"/);
  assert.match(shown, /Switched to Codex\. The conversation continues/);

  const conversation = await import("../src/screens/project/conversation.tsx");
  const data = (live_switch) => ({
    id: "p-1", name: "Harbor Notes", setup: undefined, research: undefined, context: undefined,
    pools: undefined, harnesses: undefined, assigned: null, now: 61_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { result: team({ live_switch }), at: 1 }, reloadTeam: () => {},
  });
  const fresh = "Switched to Claude Code. Started fresh: Claude Code did not get the earlier conversation, as the On harness switch setting asks.";
  const earlier = renderToStaticMarkup(createElement(conversation.ConversationTab, { data: data({ harness: "Claude Code", mode: "fresh", note: fresh }), readOnly: true }));
  assert.match(earlier, /No earlier conversation\./);
  assert.match(earlier, /<p class="cs-line cs-engine"[^>]*>Switched to Claude Code\. Started fresh/);
  const editable = renderToStaticMarkup(createElement(conversation.ConversationTab, { data: data({ harness: "Claude Code", mode: "fresh", note: fresh }) }));
  assert.doesNotMatch(editable, /Switched to Claude Code/);
});

test("the live master area shows the switch divider under its header, above the terminal", async () => {
  const live = await import("../src/screens/project/live-master.tsx");
  const note = "Switched to Codex. Started fresh: Codex did not get the earlier conversation, as the On harness switch setting asks.";
  const data = (live_switch) => ({
    id: "p-1", name: "Harbor Notes", setup: undefined, research: undefined, context: undefined,
    pools: undefined, harnesses: undefined, assigned: null, now: 61_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { result: { kind: "ok", value: { project_id: "p-1", live_switch, messages: [] } }, at: 1 }, reloadTeam: () => {},
  });
  const view = (live_switch) => renderToStaticMarkup(createElement(live.LiveMasterView, {
    data: data(live_switch), harness: "codex", live: { kind: "running", node: null }, onSwitch() {},
  }));
  const shown = view({ harness: "Codex", mode: "fresh", note });
  const head = shown.indexOf("pj-console-head");
  const divider = shown.indexOf('data-testid="harness-switch"');
  const waiting = shown.indexOf("Waiting for the master");
  assert.ok(head >= 0 && head < divider && divider < waiting, shown);
  assert.match(shown, /Switched to Codex\. Started fresh/);
  assert.doesNotMatch(view(null), /harness-switch/);
});

test("a listed live session with no messages is new already, and Delete says why it kept a session", () => {
  const list = [summary("old", "parked", { turns: 2 })];
  assert.equal(sessions.liveUnused("live", list, true), true);
  assert.equal(sessions.liveUnused("live", [summary("live", "configured", { turns: 1 }), ...list], true), false);
  assert.equal(sessions.liveUnused("live", [], false), false, "not before the list is read");
  assert.equal(sessions.liveUnused(null, list, true), false);
  assert.equal(api.failureCopy("master_session_unused"), "This session has no messages yet, so it already is a new session.");
  assert.match(api.failureCopy("master_session_files_kept"), /nothing was deleted/);
});

const { bundle, withPage } = await import("./browser.mjs");
const counted = await bundle(new URL("./fixtures/sessions-count.mjs", import.meta.url).pathname);

/** Runs one Sessions bar count check in a page with the count fixture loaded. Input: the
 * check, given the page's evaluate. Output: what the check returns. */
const inCountPage = (check) => withPage(async ({ evaluate }) => {
  await evaluate(counted);
  return check(evaluate);
});

test("the live master's Sessions bar re-reads its count when the master's turn state changes, with no timer read", async () => {
  const seen = await inCountPage(async (evaluate) => {
    await evaluate('Fixture.mountLive("working")');
    const first = await evaluate('Fixture.liveLine("1 message", 3000)');
    await evaluate('Fixture.turnLive("idle")');
    const second = await evaluate('Fixture.liveLine("2 messages", 3000)');
    const quiet = await evaluate('Fixture.liveLine("never", 2500)');
    return { first, second, quiet };
  });
  assert.equal(seen.first.line, "Live session · Started earlier · 1 message");
  assert.equal(seen.second.line, "Live session · Started earlier · 2 messages", "the master's ended turn shows in the count");
  assert.equal(seen.quiet.reads, seen.second.reads, "the bar reads the list again only on a turn change");
});

test("the conversation's Sessions bar re-reads its count when a second turn shows, with no timer read", async () => {
  const seen = await inCountPage(async (evaluate) => {
    await evaluate("Fixture.mountConversation()");
    const first = await evaluate('Fixture.liveLine("1 message", 3000)');
    await evaluate("Fixture.turnConversation()");
    const second = await evaluate('Fixture.liveLine("2 messages", 5000)');
    const quiet = await evaluate('Fixture.liveLine("never", 4500)');
    return { first, second, quiet };
  });
  assert.equal(seen.first.line, "Live session · Started earlier · 1 message");
  assert.equal(seen.second.line, "Live session · Started earlier · 2 messages", "the second turn shows in the count");
  assert.equal(seen.quiet.reads, seen.second.reads, "the conversation's two-second turn reads leave the list alone");
});

test("a Sessions list read that works again clears the problem an earlier refused read showed", async () => {
  const seen = await inCountPage(async (evaluate) => {
    await evaluate('Fixture.mountLive("working", 1)');
    await evaluate('Fixture.liveLine("never", 500)');
    const refused = await evaluate("Fixture.problem()");
    await evaluate('Fixture.turnLive("idle")');
    const line = await evaluate('Fixture.liveLine("2 messages", 3000)');
    return { refused, line, after: await evaluate("Fixture.problem()") };
  });
  assert.equal(seen.refused, "CrazyAgent's engine isn't running.", "the refused read shows its problem");
  assert.equal(seen.line.line, "Live session · Started earlier · 2 messages");
  assert.equal(seen.after, null, "the read that worked clears the earlier problem");
});

test("a refused New session keeps its problem through the list read after it", async () => {
  const seen = await inCountPage(async (evaluate) => {
    await evaluate('Fixture.mountLive("idle")');
    const before = await evaluate('Fixture.liveLine("1 message", 3000)');
    const clicked = await evaluate('Fixture.click("New session")');
    const after = await evaluate('Fixture.liveLine("never", 500)');
    return { before, clicked, after, problem: await evaluate("Fixture.problem()") };
  });
  assert.equal(seen.clicked, true);
  assert.equal(seen.after.reads, seen.before.reads + 1, "the refused switch reads the list once");
  assert.equal(seen.problem, api.failureCopy({ code: "master_source_conflict" }), "the list read that worked keeps the switch's problem");
});
