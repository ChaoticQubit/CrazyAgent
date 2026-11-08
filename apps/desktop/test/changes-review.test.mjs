import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/changes-detail.mjs", import.meta.url).pathname);

const NOW = Date.UTC(2026, 10, 8, 9, 0);
const set = (extra = {}) => ({
  id: "cs-1", agent: { kind: "sub", id: "team-1", label: "agent-1" }, state: "pending", base: "a".repeat(40), head: "b".repeat(40),
  files: 1, added: 400, removed: 0, last_outcome: "succeeded", checked: false, created_ms: NOW - 60_000, updated_ms: NOW - 1000,
  restorable_until_ms: null, conflicts: [], message: "Add lines", waiting: false, ...extra,
});
const page = (text, next = null) => ({ kind: "ok", value: { files: [], text, next, truncated: false } });
const LONG = Array.from({ length: 400 }, (_, at) => `+line ${at + 1}\n`).join("");

/** Runs steps on one mounted change set's details in a 1280 x 900 page. Input: the steps, as
 * source that returns an object. Output: what the steps returned. */
function steps(source) {
  return withPage(async ({ evaluate, send }) => {
    await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    await evaluate(fixture);
    return evaluate(`(async () => { ${source} })()`);
  });
}

test("after a move, Accept and Discard come back only once the end of the last page was on screen in the diff box, not when it arrived (RVA-1)", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount(${JSON.stringify(set())}, ${NOW});
    const opened = await Fixture.reply(${JSON.stringify(page("+one\n"))});
    const moved = await Fixture.show(${JSON.stringify(set({ head: "c".repeat(40) }))});
    const arrived = await Fixture.reply(${JSON.stringify(page(LONG))});
    const middle = await Fixture.scrollTo(0.5);
    const end = await Fixture.scrollTo(1);
    const back = await Fixture.scrollTo(0);
    await Fixture.show(${JSON.stringify(set({ head: "d".repeat(40) }))});
    await Fixture.reply(${JSON.stringify(page("+short\n"))});
    const short = await Fixture.scrollTo(0);
    return { opened, moved, arrived, middle, end, back, short };
  `);
  assert.equal(seen.opened.accept, true, "the head the owner opened can be accepted");
  assert.deepEqual(seen.moved.asked.at(-1), { id: "cs-1", head: "c".repeat(40), page: 0 }, "a move reads the new head from the top");
  assert.equal(seen.arrived.scrollable, true, "400 lines need the diff box's own scroll");
  assert.equal(seen.arrived.accept, false, "the last page arrived but its end was never on screen");
  assert.equal(seen.arrived.discard, false);
  assert.equal(seen.middle.accept, false, "half way down is not the end");
  assert.equal(seen.end.accept, true, "scrolled to its end, the moved diff was read");
  assert.equal(seen.end.discard, true);
  assert.equal(seen.back.accept, true, "the end was on screen once, so Accept stays offered");
  assert.equal(seen.short.scrollable, false);
  assert.equal(seen.short.accept, true, "a last page that fits is on screen whole");
});

test("a page refused as moved on is read again from page 0 when the list shows the same head, with Accept held until then (RVA-L2)", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mount(${JSON.stringify(set())}, ${NOW});
    const first = await Fixture.reply(${JSON.stringify(page("+one\n", 1))});
    const next = await Fixture.click("Next page");
    const refused = await Fixture.reply({ kind: "error", code: "changes_stale" });
    const listed = await Fixture.show(${JSON.stringify(set())});
    const again = listed.asked.length > next.asked.length ? await Fixture.reply(${JSON.stringify(page("+one again\n", 1))}) : listed;
    const shown = await Fixture.scrollTo(1);
    if (!shown.paging) return { first, next, refused, listed, again, shown };
    const paged = await Fixture.click("Next page");
    await Fixture.reply(${JSON.stringify(page("+two\n"))});
    const read = await Fixture.scrollTo(1);
    return { first, next, refused, listed, again, shown, paged, read };
  `);
  assert.equal(seen.first.paging, true);
  assert.deepEqual(seen.next.asked.at(-1), { id: "cs-1", head: "b".repeat(40), page: 1 });
  assert.equal(seen.refused.moved, 1, "a page refused as moved on asks for the list again");
  assert.equal(seen.refused.accept, false, "Accept waits while the moved diff is read again");
  assert.equal(seen.refused.discard, false);
  assert.deepEqual(seen.listed.asked.at(-1), { id: "cs-1", head: "b".repeat(40), page: 0 }, "the list back at the same head reads the diff again from page 0");
  assert.equal(seen.again.alert, null, "no dead error is left on screen");
  assert.match(seen.again.diff ?? "", /\+one again/);
  assert.equal(seen.again.paging, true, "the owner can page again");
  assert.equal(seen.shown.accept, false, "the reread diff has another page, so it is not read yet");
  assert.deepEqual(seen.paged.asked.at(-1), { id: "cs-1", head: "b".repeat(40), page: 1 });
  assert.equal(seen.read.accept, true, "its last page read to the end, Accept comes back");
});

test("a list read that moves the head never lets the old diff's end marker unlock the new head, without act as the app runs (RVA-1 r1 M1)", { timeout: 60_000 }, async () => {
  const seen = await steps(`
    await Fixture.mountPlain(${JSON.stringify(set())}, ${NOW});
    const opened = await Fixture.answer(${JSON.stringify(page("+one\n"))});
    const markers = await Fixture.showInFrame(${JSON.stringify(set({ head: "c".repeat(40) }))});
    await Fixture.frames();
    const arrived = await Fixture.answer(${JSON.stringify(page(LONG))});
    return { opened, markers, arrived };
  `);
  assert.equal(seen.opened.accept, true, "the head the owner opened can be accepted");
  assert.match(seen.opened.diff ?? "", /\+one/);
  assert.deepEqual(seen.arrived.asked.at(-1), { id: "cs-1", head: "c".repeat(40), page: 0 }, "a move reads the new head from the top");
  assert.equal(seen.arrived.scrollable, true, "400 lines need the diff box's own scroll");
  assert.equal(seen.arrived.scrolled, 0, "the new diff is at its top");
  assert.equal(seen.arrived.accept, false, "the old head's end marker, read before the move, unlocked the new head");
  assert.equal(seen.arrived.discard, false);
  assert.equal(seen.markers, 0, "the render that moves to the new head shows no diff read at the old head");
});
