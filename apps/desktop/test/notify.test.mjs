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
  const after = notify.freshQuestions(again.known, []);
  assert.deepEqual(notify.freshQuestions(after.known, [pending(5)]).fresh, [], "a question that left the read and came back is still known");
});

test("the watch keeps at most the newest 1024 question ids", () => {
  const many = Array.from({ length: notify.KNOWN_MAX + 10 }, (_, index) => pending(index + 1));
  const watched = notify.freshQuestions(new Set(), many);
  assert.equal(watched.fresh.length, notify.KNOWN_MAX + 10);
  assert.equal(watched.known.size, notify.KNOWN_MAX);
  assert.equal(watched.known.has(1), false);
  assert.equal(watched.known.has(notify.KNOWN_MAX + 10), true);
});

test("Settings says plainly how to allow notifications on macOS, and that other systems have none", () => {
  assert.equal(notify.systemNotificationsLine(undefined), null);
  assert.equal(notify.systemNotificationsLine({ kind: "not_ready" }), null);
  const mac = notify.systemNotificationsLine(available);
  assert.equal(mac, notify.AVAILABLE_LINE);
  assert.match(mac, /in the background/);
  assert.match(mac, /allow CrazyAgent in System Settings, Notifications\.$/);
  assert.equal(notify.systemNotificationsLine(unavailable), "System notifications are unavailable on this system.");
  assert.equal(notify.systemNotificationsLine({ kind: "error", code: "x", message: "" }), "System notifications are unavailable on this system.");
  assert.match(notify.FAILED_LINE, /^Couldn't show a macOS notification\. .*System Settings, Notifications\.$/);
});

test("the window names only the project and the question; the app writes the notification text", () => {
  const source = readFileSync(new URL("../src/notify.ts", import.meta.url), "utf8");
  assert.match(source, /call<void>\("desktop_notify_question", \{ projectId, questionId \}\)/);
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  assert.equal(app.match(/notifyQuestion\(/g)?.length, 1, "one call, where the alert read finds a fresh question");
  assert.match(app, /if \(shouldNotify\(inBackground\(\), await system\)\)/);
  const settings = readFileSync(new URL("../src/screens/settings/team-alerts.tsx", import.meta.url), "utf8");
  assert.match(settings, /systemNotificationsLine\(/);
});
