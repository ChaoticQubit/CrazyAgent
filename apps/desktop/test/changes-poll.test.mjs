import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/changes-poll.mjs", import.meta.url).pathname);

test("a change list read slower than the poll interval is never overlapped by another", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    const mounted = await evaluate(`Fixture.mount("alpha")`);
    const slow = await evaluate(`Fixture.wait(Fixture.EVERY * 4)`);
    return { mounted, slow };
  });
  assert.deepEqual(seen.mounted.asked, ["alpha"]);
  assert.deepEqual(seen.slow.asked, ["alpha"], "no second read starts while the first is in flight");
  assert.equal(seen.slow.most, 1);
});

test("a change list reply slower than the poll interval is shown, not dropped", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount("alpha")`);
    await evaluate(`Fixture.wait(Fixture.EVERY * 4)`);
    return evaluate(`Fixture.reply(["cs-1"])`);
  });
  assert.equal(seen.shown, "cs-1", "the slow reply is the list shown");
});

test("the change list rereads one interval after each reply, and a reread asked mid-read runs once it settles", { timeout: 60_000 }, async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    await evaluate(`Fixture.mount("alpha")`);
    const first = await evaluate(`Fixture.reply(["cs-1"])`);
    const polled = await evaluate(`Fixture.wait(Fixture.EVERY * 2)`);
    const asked = await evaluate(`Fixture.again()`);
    const queued = await evaluate(`Fixture.reply(["cs-2"])`);
    const third = await evaluate(`Fixture.reply(["cs-3"])`);
    const idle = await evaluate(`Fixture.again()`);
    const fourth = await evaluate(`Fixture.reply(["cs-4"])`);
    const reading = await evaluate(`Fixture.wait(Fixture.EVERY * 2)`);
    const moved = await evaluate(`Fixture.move("beta")`);
    const stale = await evaluate(`Fixture.reply(["cs-5"])`);
    const beta = await evaluate(`Fixture.reply(["cs-b"])`);
    const gone = await evaluate(`Fixture.unmount(Fixture.EVERY * 3)`);
    return { first, polled, asked, queued, third, idle, fourth, reading, moved, stale, beta, gone };
  });
  assert.equal(seen.first.shown, "cs-1");
  assert.equal(seen.first.asked.length, 1, "the next read waits an interval after the reply");
  assert.equal(seen.polled.asked.length, 2, "one interval after the reply the list reads again");
  assert.equal(seen.asked.asked.length, 2, "a reread asked while a read is in flight does not start another");
  assert.equal(seen.queued.shown, "cs-2");
  assert.equal(seen.queued.asked.length, 3, "the reread asked mid-read starts as soon as that read settles");
  assert.equal(seen.third.shown, "cs-3");
  assert.equal(seen.idle.asked.length, 4, "a reread asked between reads starts at once");
  assert.equal(seen.fourth.shown, "cs-4");
  assert.equal(seen.fourth.most, 1, "never more than one read in flight for a project");
  assert.equal(seen.reading.open, 1, "a read of alpha is in flight when the project changes");
  assert.deepEqual(seen.moved.asked.slice(-1), ["beta"], "another project reads at once");
  assert.equal(seen.stale.shown, "cs-4", "a reply for the project left behind is not shown");
  assert.equal(seen.beta.shown, "cs-b");
  assert.equal(seen.gone.asked.length, seen.beta.asked.length, "nothing is read after the list is closed");
});
