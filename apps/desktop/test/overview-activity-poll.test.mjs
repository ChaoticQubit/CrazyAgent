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
