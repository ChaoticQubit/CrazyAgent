import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/overview-activity.mjs", import.meta.url).pathname);

const item = (id, text, extra = {}) => ({ id, at_ms: 0, kind: "accepted", actor: "Owner", goal: "Build the service", text, change_set: null, undone: false, ...extra });
const ok = (items, more = false) => ({ kind: "ok", value: { project_id: "alpha", items, more } });
const failed = { kind: "error", code: "engine_unreachable", message: "The engine isn't running." };

/** Runs steps against a fresh page with the fixture loaded. Input: the steps, as source
 * that returns an object. Output: what the steps returned. */
function steps(source) {
  return withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    return evaluate(`(async () => { ${source} })()`);
  });
}

test("Recent activity says it is loading, shows the read, then keeps it with a note when a refresh fails", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    const loading = await Fixture.mount("alpha");
    const shown = await Fixture.reply(${JSON.stringify(ok([item(1, "Accepted the changes of Ada: login")]))});
    const polled = await Fixture.wait(Fixture.EVERY * 1.5);
    const stale = await Fixture.reply(${JSON.stringify(failed)});
    return { loading, shown, polled, stale };
  `);
  assert.match(seen.loading.text, /Loading activity\.\.\./);
  assert.equal(seen.shown.rows, 1);
  assert.equal(seen.shown.live, true, "a fresh read shows Live");
  assert.equal(seen.polled.asked.length, 2, "it reads again one interval after the reply");
  assert.equal(seen.stale.rows, 1, "a failed refresh keeps the last read");
  assert.match(seen.stale.text, /Couldn't refresh activity\. .*Showing the last read\./);
  assert.equal(seen.stale.live, false, "a stale read is not Live");
});

test("a first activity read that fails says so", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    return Fixture.reply(${JSON.stringify(failed)});
  `);
  assert.match(seen.alert ?? "", /Couldn't load activity\./);
  assert.equal(seen.rows, 0);
});

test("activity reads every interval while the window is visible, none while hidden, and none after unmount", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    await Fixture.reply(${JSON.stringify(ok([]))});
    const second = await Fixture.wait(Fixture.EVERY * 1.5);
    await Fixture.reply(${JSON.stringify(ok([]))});
    await Fixture.hide(true);
    const hidden = await Fixture.wait(Fixture.EVERY * 3);
    await Fixture.hide(false);
    const back = await Fixture.wait(Fixture.EVERY * 1.5);
    await Fixture.reply(${JSON.stringify(ok([]))});
    const gone = await Fixture.unmount(Fixture.EVERY * 3);
    return { second, hidden, back, gone };
  `);
  assert.equal(seen.second.asked.length, 2);
  assert.equal(seen.second.waiting, 1, "one read at a time");
  assert.equal(seen.hidden.asked.length, 2, "no read while the window is hidden");
  assert.equal(seen.back.asked.length, 3, "reads again once the window shows");
  assert.equal(seen.gone.asked.length, 3, "no read after the section is gone");
});

test("switching project reads the new one at once and drops the old project's reply", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    const moved = await Fixture.move("beta");
    const old = await Fixture.reply(${JSON.stringify(ok([item(1, "Alpha event")]))});
    const beta = await Fixture.reply(${JSON.stringify(ok([item(2, "Beta event")]))});
    return { moved, old, beta };
  `);
  assert.deepEqual(seen.moved.asked, ["alpha", "beta"]);
  assert.doesNotMatch(seen.old.text, /Alpha event/, "the project left behind is not shown");
  assert.match(seen.beta.text, /Beta event/);
});

test("a read the engine cut says older activity is not shown and counts with a plus", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    return Fixture.reply(${JSON.stringify(ok([item(1, "Newest"), item(2, "Older")], true))});
  `);
  assert.match(seen.text, /2\+ items/);
  assert.match(seen.text, /Older activity isn't shown here\./);
});

test("a read still waiting holds the next one, however long it takes", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    return Fixture.wait(Fixture.EVERY * 3);
  `);
  assert.equal(seen.asked.length, 1, "no second read starts while the first waits");
  assert.equal(seen.waiting, 1);
});

test("an empty read says there is nothing yet", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    return Fixture.reply(${JSON.stringify(ok([]))});
  `);
  assert.match(seen.text, /Nothing yet\./);
  assert.equal(seen.rows, 0);
  assert.equal(seen.live, true);
});

test("a build without the activity read says so and never reads again", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    const shown = await Fixture.reply({ kind: "not_ready" });
    const later = await Fixture.wait(Fixture.EVERY * 3);
    await Fixture.hide(true);
    const back = await Fixture.hide(false);
    return { shown, later, back };
  `);
  assert.match(seen.shown.text, /Not ready yet: project activity isn't in this build\./);
  assert.equal(seen.shown.live, false, "nothing was read, so it is not Live");
  assert.equal(seen.later.asked.length, 1, "the poll stops");
  assert.equal(seen.back.asked.length, 1, "the window showing again does not start it");
});

test("the window showing again reads at once, and the next read waits a full interval", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    await Fixture.reply(${JSON.stringify(ok([]))});
    await Fixture.hide(true);
    const hidden = await Fixture.wait(Fixture.EVERY * 1.5);
    const shown = await Fixture.hide(false);
    const held = await Fixture.wait(Fixture.EVERY * 1.5);
    await Fixture.reply(${JSON.stringify(ok([]))});
    const early = await Fixture.wait(Fixture.EVERY / 2);
    const next = await Fixture.wait(Fixture.EVERY);
    return { hidden, shown, held, early, next };
  `);
  assert.equal(seen.hidden.asked.length, 1, "no read while the window is hidden");
  assert.equal(seen.shown.asked.length, 2, "the read starts as the window shows, with no wait");
  assert.equal(seen.held.asked.length, 2, "the timer from before the window hid does not add a read");
  assert.equal(seen.early.asked.length, 2, "the next read waits for the interval");
  assert.equal(seen.next.asked.length, 3, "the poll goes on after that read");
});

test("the window showing again while a read is waiting starts no second read", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    await Fixture.hide(true);
    return Fixture.hide(false);
  `);
  assert.equal(seen.asked.length, 1);
  assert.equal(seen.waiting, 1);
});

test("the window showing again after the section is gone reads nothing", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount("alpha");
    await Fixture.reply(${JSON.stringify(ok([]))});
    await Fixture.hide(true);
    await Fixture.unmount(0);
    return Fixture.hide(false);
  `);
  assert.equal(seen.asked.length, 1);
});

test("Overview reads activity through the real bridge call and says when the engine cut it", { timeout: 60_000 }, async () => {
  const answer = { project_id: "alpha", items: [item(1, "Newest"), item(2, "Older")], more: true };
  const seen = await steps(`return Fixture.mountReal("alpha", ${JSON.stringify(answer)});`);
  assert.deepEqual(seen.sent, [{ command: "desktop_project_activity", args: { id: "alpha", limit: 200 } }]);
  assert.equal(seen.rows, 2);
  assert.match(seen.text, /2\+ items/);
  assert.match(seen.text, /Older activity isn't shown here\./);
});
