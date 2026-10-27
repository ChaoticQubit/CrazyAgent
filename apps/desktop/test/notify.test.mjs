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

test("each pending question is fresh once, and an app start notifies nothing already pending", () => {
  const start = notify.freshQuestions(null, [pending(3), pending(4, "p-2")]);
  assert.deepEqual(start.fresh, []);
  assert.deepEqual([...start.known].sort(), [3, 4]);
  const next = notify.freshQuestions(start.known, [pending(3), pending(6, "p-2"), pending(5)]);
  assert.deepEqual(next.fresh, [pending(5), pending(6, "p-2")]);
  const again = notify.freshQuestions(next.known, [pending(5), pending(6, "p-2")]);
  assert.deepEqual(again.fresh, [], "a question read again is not notified again");
  assert.deepEqual([...again.known].sort(), [5, 6], "a settled question is forgotten, so the known ids stay bounded by what is pending");
});

test("a question raised while another of the same project is answered is still notified", () => {
  const before = notify.freshQuestions(null, [pending(7)]);
  const after = notify.freshQuestions(before.known, [pending(8)]);
  assert.deepEqual(after.fresh, [pending(8)], "the pending count stayed 1, the question changed");
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
  assert.equal(source.match(/notifyQuestion\(question\.project_id, question\.id\)/g)?.length, 1, "one call, where the watch finds a fresh question");
  assert.match(source, /if \(shouldNotify\(inBackground\(\), await system\)\)/);
  assert.match(source, /navigate\(\{ name: "project", id: result\.value, tab: "questions" \}\)/);
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  assert.equal(app.match(/useQuestionNotifications\(session\.engine === "running", session\.showToast\);/g)?.length, 1);
  const settings = readFileSync(new URL("../src/screens/settings/team-alerts.tsx", import.meta.url), "utf8");
  assert.match(settings, /systemNotificationsLine\(/);
});
