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
  const start = notify.freshNotices(null, page([3, 4], 9), true, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(start, { kept: 9, send: [] });
  assert.deepEqual(notify.bannerQuery(9), { filter: "all", project_id: null, kind: null, fold: false, before: 60, after: 9, limit: 50 }, "one page of ids just past the cursor, the oldest first");
});

test("each new item the engine marks for a banner is sent once, oldest first", () => {
  const next = notify.freshNotices(4, page([6, 5]), true, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(next, { kept: 6, send: [5, 6] });
  assert.deepEqual(notify.freshNotices(next.kept, page([6, 5]), true, notify.NOTIFY_PER_READ_MAX).send, [], "an item read again is not sent again");
});

test("an item without a banner, already read or already resolved is passed over, never sent later", () => {
  const read = notify.freshNotices(4, page([5, 6, 7, 8], 8, { 5: { banner: false }, 6: { read_ms: 1 }, 7: { resolved_ms: 1 } }), true, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(read, { kept: 8, send: [8] });
});

test("a burst goes out a few per read, oldest first, and the rest wait without being skipped", () => {
  assert.equal(notify.NOTIFY_PER_READ_MAX, 3);
  const burst = page([9, 4, 8, 5, 7, 6], 9, { 5: { banner: false } });
  const first = notify.freshNotices(3, burst, true, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(first, { kept: 7, send: [4, 6, 7] }, "the cursor stops at the last item sent");
  const second = notify.freshNotices(first.kept, page([9, 8], 9), true, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(second, { kept: 9, send: [8, 9] });
});

test("the cursor moves past the store's newest id once every listed item is handled", () => {
  assert.deepEqual(notify.freshNotices(4, page([5], 12), true, notify.NOTIFY_PER_READ_MAX), { kept: 12, send: [5] }, "items past the last listed are deleted ones");
  assert.deepEqual(notify.freshNotices(12, page([], 12), true, notify.NOTIFY_PER_READ_MAX), { kept: 12, send: [] });
});

test("a focused window sends nothing and leaves nothing waiting", () => {
  const focused = notify.freshNotices(0, page([1, 2, 3, 4]), false, notify.NOTIFY_PER_READ_MAX);
  assert.deepEqual(focused, { kept: 4, send: [] });
  assert.deepEqual(notify.freshNotices(focused.kept, page([1, 2, 3, 4]), true, notify.NOTIFY_PER_READ_MAX).send, [], "an item seen while focused is never sent later");
});

test("a store whose newest id fell below the cursor, purged or replaced, starts over from it and sends nothing old", () => {
  assert.deepEqual(notify.freshNotices(9, page([], 5), true, notify.NOTIFY_PER_READ_MAX), { kept: 5, send: [] });
  assert.deepEqual(notify.freshNotices(5, page([6]), true, notify.NOTIFY_PER_READ_MAX), { kept: 6, send: [6] }, "the next new item is sent");
});

const NEWEST_FIRST = (a, b) => b - a;
const store = (ids, banners) => {
  const reads = [];
  const load = async (query) => {
    reads.push(query);
    const inside = ids.filter((id) => (query.after === null || id > query.after) && (query.before === null || id < query.before)).sort(NEWEST_FIRST);
    const items = inside.slice(0, query.limit).map((id) => item(id, { banner: banners.includes(id) }));
    return { kind: "ok", value: { items, counts: { unread: ids.length, pending: 0 }, more: inside.length > query.limit, newest: Math.max(0, ...ids) } };
  };
  return { load, reads };
};
const upTo = (count) => Array.from({ length: count }, (_, at) => at + 1);

test("a backlog past one page goes out oldest first over windows and looks, and no item is skipped", async () => {
  const { load } = store(upTo(130), [7, 60, 61, 120]);
  const first = await notify.pickBanners(0, true, load);
  assert.deepEqual(first, { kept: 61, send: [7, 60, 61] }, "the oldest first, a read's worth, the cursor at the last sent");
  const second = await notify.pickBanners(first.kept, true, load);
  assert.deepEqual(second, { kept: 130, send: [120] });
  assert.deepEqual(await notify.pickBanners(second.kept, true, load), { kept: 130, send: [] });
});

test("one look reads at most a few windows of the store and the next look goes on from there", async () => {
  assert.equal(notify.BANNER_WINDOWS_PER_READ_MAX, 4);
  const { load, reads } = store(upTo(300), [290]);
  const first = await notify.pickBanners(0, true, load);
  assert.deepEqual(first, { kept: 200, send: [] });
  assert.equal(reads.length, 4, "a look is bounded");
  assert.deepEqual(reads.map((query) => [query.after, query.before, query.limit]), [[0, 51, 50], [50, 101, 50], [100, 151, 50], [150, 201, 50]]);
  assert.deepEqual(await notify.pickBanners(first.kept, true, load), { kept: 300, send: [290] });
});

test("the first look and a focused window read once and send nothing, and a failed read keeps what came before it", async () => {
  const { load, reads } = store(upTo(130), [7]);
  assert.deepEqual(await notify.pickBanners(null, true, load), { kept: 130, send: [] });
  assert.deepEqual(await notify.pickBanners(0, false, load), { kept: 130, send: [] });
  assert.equal(reads.length, 2);
  let served = 0;
  const failing = async (query) => (served++ === 0 ? load(query) : { kind: "error", code: "engine_unreachable", message: "" });
  assert.deepEqual(await notify.pickBanners(0, true, failing), { kept: 50, send: [7] }, "the next look starts after the first window");
  assert.deepEqual(await notify.pickBanners(0, true, async () => ({ kind: "not_ready" })), { kept: 0, send: [] });
});

test("a window lists only ids past the cursor, never more than one page of them", () => {
  assert.throws(() => notify.freshNotices(3, page([2], 9), true, notify.NOTIFY_PER_READ_MAX), /past the cursor/);
  assert.throws(() => notify.freshNotices(3, page([60]), true, notify.NOTIFY_PER_READ_MAX), /past the cursor/);
  assert.throws(() => notify.freshNotices(3, page([4]), true, 0), /at least one/);
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

test("the engine lost report goes once when the engine runs again after it ran and was lost, never on the first connect", () => {
  const start = { ran: false, lost: false };
  const connecting = notify.engineLostStep(start, "connecting");
  assert.deepEqual(connecting, { watch: start, report: false });
  const neverRan = notify.engineLostStep(start, "down");
  assert.deepEqual(neverRan, { watch: { ran: false, lost: false }, report: false }, "an engine that never ran was not lost");
  const first = notify.engineLostStep(neverRan.watch, "running");
  assert.deepEqual(first, { watch: { ran: true, lost: false }, report: false }, "the first connect reports nothing");
  const down = notify.engineLostStep(first.watch, "down");
  assert.deepEqual(down, { watch: { ran: true, lost: true }, report: false });
  const retrying = notify.engineLostStep(down.watch, "connecting");
  assert.deepEqual(retrying.watch, { ran: true, lost: true });
  const back = notify.engineLostStep(retrying.watch, "running");
  assert.deepEqual(back, { watch: { ran: true, lost: false }, report: true });
  assert.equal(notify.engineLostStep(back.watch, "running").report, false, "one report per loss");
  assert.deepEqual(notify.engineLostStep(first.watch, "outdated"), { watch: first.watch, report: false }, "another engine version is not a loss");
});

test("the app reports an update it found and an engine it lost and found again to the store", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  const updates = readFileSync(new URL("../src/screens/settings/updates.tsx", import.meta.url), "utf8");
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  assert.match(source, /sendNotice\(\{ report: \{ report \} \}\)/);
  assert.match(updates, /if \(result\.value\) void reportNotice\(\{ kind: "app_update_ready", version: result\.value\.version \}\);/, "every check that finds one; the engine keeps one item per version");
  assert.equal(app.match(/useEngineLostReport\(session\.engine\);/g)?.length, 1);
  assert.match(app, /if \(step\.report\) void reportNotice\(\{ kind: "engine_lost" \}\);/);
});
