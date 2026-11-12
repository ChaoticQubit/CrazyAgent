import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { resetNotice, NOTICES_PER_LOOK_MAX } = await import("../src/screens/metrics/extras.ts");
const watchPage = await bundle(new URL("./fixtures/reset-notice-read.mjs", import.meta.url).pathname);

const POOLS = { name: "settings", tab: "pools" };
const labels = new Map([["acct-a", "Work"], ["acct-b", "Home"]]);
const extrasReads = (asked) => asked.filter((item) => item.startsWith("desktop_usage_extras"));

test("the notice names each account by its label, joins one look's arrivals and says nothing when none arrived", () => {
  assert.equal(resetNotice([], labels), null);
  assert.equal(resetNotice([{ account: "acct-a", count: 1 }], labels), "Work has a new free reset. 1 free reset now.");
  assert.equal(resetNotice([{ account: "acct-a", count: 2 }, { account: "acct-b", count: 1 }], labels),
    "Work has a new free reset. 2 free resets now. Home has a new free reset. 1 free reset now.");
  assert.equal(resetNotice([{ account: "acct-gone", count: 1 }], labels), "An account has a new free reset. 1 free reset now.",
    "an account the list no longer has is never named by its id");
  const many = Array.from({ length: NOTICES_PER_LOOK_MAX + 1 }, (_, index) => ({ account: `acct-${index}`, count: 1 }));
  assert.throws(() => resetNotice(many, labels));
});

test("the app watches for new free resets while the engine runs", () => {
  const app = readFileSync(new URL("../src/app.tsx", import.meta.url), "utf8");
  assert.match(app, /useResetNotice\(session\.engine === "running", session\.showToast\);/);
});

test("a new free reset is told once as a toast that opens Pools, from what is stored and never the detail rows", async () => {
  await withPage(async ({ evaluate }) => {
    await evaluate(watchPage);
    const first = await evaluate(`Fixture.start([["acct-a", 2, 60000], ["acct-b", 1, null]])`);
    assert.deepEqual(first.toasts, [{ message: "Work has a new free reset. 2 free resets now.", open: POOLS }]);
    assert.equal(first.armed, true, "the next look is armed");
    const again = await evaluate(`Fixture.look([["acct-a", 2, 60000], ["acct-b", 1, null]])`);
    assert.equal(again.toasts.length, 1, "the same arrival is never told twice");
    const both = await evaluate(`Fixture.look([["acct-a", 3, 0], ["acct-b", 1, 0]])`);
    assert.deepEqual(both.toasts.slice(1), [{ message: "Work has a new free reset. 3 free resets now. Home has a new free reset. 1 free reset now.", open: POOLS }]);
    assert.deepEqual([...new Set(extrasReads(both.asked))], ["desktop_usage_extras:false"], "the watch never asks for the detail rows");
    assert.equal(both.asked.filter((item) => item === "desktop_list_accounts").length, 2, "accounts are read only for a look that tells something");
  });
});

test("an old arrival or a failed read tells nothing, and a stopped engine ends the reads", async () => {
  await withPage(async ({ evaluate }) => {
    await evaluate(watchPage);
    const old = await evaluate(`Fixture.start([["acct-a", 2, ${2 * 3_600_000}]])`);
    assert.deepEqual(old.toasts, [], "a reset that arrived long before the app looked is not new");
    assert.equal(old.asked.includes("desktop_list_accounts"), false);
    const failed = await evaluate("Fixture.look(null)");
    assert.deepEqual(failed.toasts, []);
    assert.equal(failed.armed, true, "a failed read still arms the next look");
    const stopped = await evaluate("Fixture.engineStops()");
    assert.equal(extrasReads(stopped.asked).length, extrasReads(failed.asked).length, "no read once the engine stops");
    assert.deepEqual(stopped.toasts, []);
  });
});
