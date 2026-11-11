import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const notify = await import("../src/notify.ts");

const available = { kind: "ok", value: "available" };
const unavailable = { kind: "ok", value: "unavailable" };
const pending = (id, project_id = "p-1") => ({ id, project_id });

test("only a window in the background sends a system notification, and only where the system shows them", () => {
  assert.equal(notify.shouldNotify(true, available), true);
  assert.equal(notify.shouldNotify(false, available), false, "a focused window shows the toast only");
  assert.equal(notify.shouldNotify(true, unavailable), false);
  assert.equal(notify.shouldNotify(true, undefined), false);
  assert.equal(notify.shouldNotify(true, { kind: "not_ready" }), false);
  assert.equal(notify.shouldNotify(true, { kind: "error", code: "x", message: "" }), false);
});

test("each pending question is sent once, and an app start notifies nothing already pending", () => {
  const start = notify.freshQuestions(null, [pending(3), pending(4, "p-2")], true);
  assert.deepEqual(start.send, []);
  assert.deepEqual([...start.known].sort(), [3, 4]);
  const next = notify.freshQuestions(start.known, [pending(3), pending(6, "p-2"), pending(5)], true);
  assert.deepEqual(next.send, [pending(5), pending(6, "p-2")]);
  const again = notify.freshQuestions(next.known, [pending(5), pending(6, "p-2")], true);
  assert.deepEqual(again.send, [], "a question read again is not notified again");
  assert.deepEqual([...again.known].sort(), [5, 6], "a settled question is forgotten, so the known ids stay bounded by what is pending");
});

test("a question raised while another of the same project is answered is still notified", () => {
  const before = notify.freshQuestions(null, [pending(7)], true);
  const after = notify.freshQuestions(before.known, [pending(8)], true);
  assert.deepEqual(after.send, [pending(8)], "the pending count stayed 1, the question changed");
});

test("a burst goes out a few per read, oldest first, and every question is sent once", () => {
  const start = notify.freshQuestions(null, [], true);
  const burst = [9, 4, 8, 5, 7, 6].map((id) => pending(id));
  const first = notify.freshQuestions(start.known, burst, true);
  assert.equal(notify.NOTIFY_PER_READ_MAX, 3);
  assert.deepEqual(first.send.map((item) => item.id), [4, 5, 6]);
  const second = notify.freshQuestions(first.known, burst, true);
  assert.deepEqual(second.send.map((item) => item.id), [7, 8, 9], "the rest wait for the next read");
  assert.deepEqual(notify.freshQuestions(second.known, burst, true).send, []);
});

test("a focused window sends nothing and leaves nothing waiting", () => {
  const start = notify.freshQuestions(null, [], true);
  const focused = notify.freshQuestions(start.known, [pending(1), pending(2), pending(3), pending(4)], false);
  assert.deepEqual(focused.send, []);
  assert.deepEqual(notify.freshQuestions(focused.known, [pending(1), pending(2), pending(3), pending(4)], true).send, [], "a question seen while focused is never notified later");
});

test("only the plugin's refusal earns the System Settings toast", () => {
  const refused = (code) => ({ kind: "error", code, message: "" });
  assert.equal(notify.failureLine(refused("notifications_failed")), notify.FAILED_LINE);
  assert.equal(notify.failureLine(refused("questions_not_found")), null, "a question settled meanwhile");
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
  assert.match(mac, /If none appear, macOS is blocking them: allow CrazyAgent in System Settings, Notifications\.$/);
  assert.equal(notify.systemNotificationsLine(unavailable), "System notifications are unavailable on this system.");
  assert.equal(notify.systemNotificationsLine({ kind: "error", code: "x", message: "" }), "System notifications are unavailable on this system.");
  assert.match(notify.FAILED_LINE, /^Couldn't show a macOS notification\. .*System Settings, Notifications\.$/);
});

test("the window names only the project and the question; the app writes the notification text", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  assert.match(source, /call<void>\("desktop_notify_question", \{ projectId, questionId \}\)/);
  assert.equal(source.match(/notifyQuestion\(question\.project_id, question\.id\)/g)?.length, 1, "one call, where the watch picks a question to send");
  assert.match(source, /const notifying = shouldNotify\(inBackground\(\), await system\);/);
  assert.match(source, /failureLine\(await notifyQuestion\(question\.project_id, question\.id\)\)/);
  assert.match(source, /navigate\(\{ name: "project", id: result\.value, tab: "questions" \}\)/);
  const settings = readFileSync(new URL("../src/screens/settings/team-alerts.tsx", import.meta.url), "utf8");
  assert.match(settings, /systemNotificationsLine\(/);
});

test("one question-counts read every 10 s feeds the sidebar flags, the badge and the notification watch", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  const reads = (text) => text.match(/loadQuestionCounts\(\)/g)?.length ?? 0;
  assert.equal(reads(source) + reads(app), 1, "the counts are read once per look, not once per watcher");
  assert.match(app, /Promise\.all\(\[loadTeamEvents\(\), loadQuestionCounts\(\)\]\)/);
  assert.equal(app.match(/questionWatch\(\(line\) => toast\.current\(line\)\)/g)?.length, 1, "one watch, made by the alerts read");
  assert.match(app, /if \(questions\.kind === "ok"\) await watch\.look\(questions\.value\);/);
  assert.match(app, /watch\.stop\(\);/);
  assert.match(app, /if \(live && \(result\.kind !== "not_ready" \|\| questions\.kind !== "not_ready"\)\) timer = window\.setTimeout\(read, ALERT_READ_MS\);/, "a build without team events still watches questions");
  assert.doesNotMatch(source, /setTimeout|setInterval|QUESTION_READ_MS/, "the watch has no timer of its own");
  assert.doesNotMatch(app, /useQuestionNotifications/);
});
