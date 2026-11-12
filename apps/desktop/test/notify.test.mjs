import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const notify = await import("../src/notify.ts");

const available = { kind: "ok", value: "available" };
const unavailable = { kind: "ok", value: "unavailable" };
const item = (id, extra = {}) => ({
  id, kind: "agent_finished", class: "info", project_id: "p-1", text: "Sub-agent 2 finished Add the login form",
  target: { page: "agents", project_id: "p-1" }, created_ms: 1_800_000_000_000, read_ms: null, resolved_ms: null,
  folded: 1, unread: 1, pending: 0, ids: [id], banner: true, ...extra,
});
const page = (ids, newest = Math.max(0, ...ids), extra = {}) => ({
  items: [...ids].sort((a, b) => b - a).map((id) => item(id, extra[id] ?? {})), counts: { unread: ids.length, pending: 0 }, more: false, newest,
});

test("only a window in the background sends a system notification, and only where the system shows them", () => {
  assert.equal(notify.shouldNotify(true, available), true);
  assert.equal(notify.shouldNotify(false, available), false, "a focused window shows the bell only");
  assert.equal(notify.shouldNotify(true, unavailable), false);
  assert.equal(notify.shouldNotify(true, undefined), false);
  assert.equal(notify.shouldNotify(true, { kind: "not_ready" }), false);
  assert.equal(notify.shouldNotify(true, { kind: "error", code: "x", message: "" }), false);
});

test("an app start notifies nothing already in the store and starts after its newest item", () => {
  assert.deepEqual(notify.bannerQuery(null), { filter: "all", project_id: null, kind: null, fold: false, before: null, after: null, limit: 1 });
  const start = notify.freshNotices(null, page([3, 4], 9), true);
  assert.deepEqual(start, { kept: 9, send: [] });
  assert.deepEqual(notify.bannerQuery(9), { filter: "all", project_id: null, kind: null, fold: false, before: null, after: 9, limit: 50 });
});

test("each new item the engine marks for a banner is sent once, oldest first", () => {
  const next = notify.freshNotices(4, page([6, 5]), true);
  assert.deepEqual(next, { kept: 6, send: [5, 6] });
  assert.deepEqual(notify.freshNotices(next.kept, page([6, 5]), true).send, [], "an item read again is not sent again");
});

test("an item without a banner, already read or already resolved is passed over, never sent later", () => {
  const read = notify.freshNotices(4, page([5, 6, 7, 8], 8, { 5: { banner: false }, 6: { read_ms: 1 }, 7: { resolved_ms: 1 } }), true);
  assert.deepEqual(read, { kept: 8, send: [8] });
});

test("a burst goes out a few per read, oldest first, and the rest wait without being skipped", () => {
  assert.equal(notify.NOTIFY_PER_READ_MAX, 3);
  const burst = page([9, 4, 8, 5, 7, 6], 9, { 5: { banner: false } });
  const first = notify.freshNotices(3, burst, true);
  assert.deepEqual(first, { kept: 7, send: [4, 6, 7] }, "the cursor stops at the last item sent");
  const second = notify.freshNotices(first.kept, page([9, 8], 9), true);
  assert.deepEqual(second, { kept: 9, send: [8, 9] });
});

test("the cursor moves past the store's newest id once every listed item is handled", () => {
  assert.deepEqual(notify.freshNotices(4, page([5], 12), true), { kept: 12, send: [5] }, "items past the last listed are deleted ones");
  assert.deepEqual(notify.freshNotices(12, page([], 12), true), { kept: 12, send: [] });
});

test("a focused window sends nothing and leaves nothing waiting", () => {
  const focused = notify.freshNotices(0, page([1, 2, 3, 4]), false);
  assert.deepEqual(focused, { kept: 4, send: [] });
  assert.deepEqual(notify.freshNotices(focused.kept, page([1, 2, 3, 4]), true).send, [], "an item seen while focused is never sent later");
});

test("a store whose newest id fell below the cursor, purged or replaced, starts over from it and sends nothing old", () => {
  assert.deepEqual(notify.freshNotices(9, page([], 5), true), { kept: 5, send: [] });
  assert.deepEqual(notify.freshNotices(5, page([6]), true), { kept: 6, send: [6] }, "the next new item is sent");
});

test("a click on a banner opens the item's exact page and marks only that item read", async () => {
  const went = [];
  const sent = [];
  const anchor = await notify.openClicked({ id: 7, target: { page: "questions", project_id: "p-1", question: 12, agent: null } }, {
    go: (hash) => went.push(hash),
    send: async (action) => { sent.push(action); return { kind: "ok", value: undefined }; },
  });
  assert.deepEqual(went, ["#/projects/p-1/questions"]);
  assert.deepEqual(sent, [{ read: { ids: [7] } }], "never a fold read, which would reach older items of the kind");
  assert.equal(anchor, "question-12");
});

test("only the plugin's refusal earns the System Settings toast", () => {
  const refused = (code) => ({ kind: "error", code, message: "" });
  assert.equal(notify.failureLine(refused("notifications_failed")), notify.FAILED_LINE);
  assert.equal(notify.failureLine(refused("notification_ids_invalid")), null);
  assert.equal(notify.failureLine(refused("notifications_full")), null, "a run that showed its most");
  assert.equal(notify.failureLine({ kind: "ok", value: undefined }), null);
  assert.equal(notify.failureLine({ kind: "not_ready" }), null);
});

test("Settings says plainly how to allow notifications on macOS, and that other systems have none", () => {
  assert.equal(notify.systemNotificationsLine(undefined), null);
  assert.equal(notify.systemNotificationsLine({ kind: "not_ready" }), null);
  const mac = notify.systemNotificationsLine(available);
  assert.equal(mac, notify.AVAILABLE_LINE);
  assert.match(mac, /in the background/);
  assert.match(mac, /Settings, Notifications/);
  assert.doesNotMatch(mac, /question/, "every kind can send one now, not only questions");
  assert.match(mac, /If none appear, macOS is blocking them: allow CrazyAgent in System Settings, Notifications\.$/);
  assert.equal(notify.systemNotificationsLine(unavailable), "System notifications are unavailable on this system.");
  assert.equal(notify.systemNotificationsLine({ kind: "error", code: "x", message: "" }), "System notifications are unavailable on this system.");
  assert.match(notify.FAILED_LINE, /^Couldn't show a macOS notification\. .*System Settings, Notifications\.$/);
});

test("the window names only the store item; the app reads it and writes the notification text", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  assert.match(source, /call<void>\("desktop_notify_notice", \{ id \}\)/);
  assert.equal(source.match(/failureLine\(await notifyNotice\(id\)\)/g)?.length, 1, "one call, where the watch picks an item to send");
  assert.match(source, /const notifying = shouldNotify\(inBackground\(\), await system\);/);
  assert.doesNotMatch(source, /desktop_notify_question|questionWatch|loadQuestions/, "questions send their banner as store items, never twice");
  const settings = readFileSync(new URL("../src/screens/settings/team-alerts.tsx", import.meta.url), "utf8");
  assert.match(settings, /systemNotificationsLine\(/);
});

test("the alerts read every 10 s also drives the banner watch, which has no timer of its own", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  assert.match(app, /Promise\.all\(\[loadTeamEvents\(\), loadQuestionCounts\(\)\]\)/);
  assert.equal(app.match(/noticeWatch\(\(line\) => toast\.current\(line\)\)/g)?.length, 1, "one watch, made by the alerts read");
  assert.match(app, /await watch\.look\(\);/);
  assert.match(app, /watch\.stop\(\);/);
  assert.doesNotMatch(app, /questionWatch/);
  assert.doesNotMatch(source, /setTimeout|setInterval/, "the watch has no timer of its own");
});
