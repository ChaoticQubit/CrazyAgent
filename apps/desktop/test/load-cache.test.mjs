import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const { LOAD_CACHE_MAX, cacheLoad, cachedLoad, loadKey } = await import("../src/state.ts");
const fixture = await bundle(new URL("./fixtures/load-cache.mjs", import.meta.url).pathname);

test("the read cache keeps only good replies, newest last, and at most LOAD_CACHE_MAX of them", () => {
  const key = (index) => loadKey("bound", [index]);
  assert.notEqual(loadKey("pools", ["a"]), loadKey("pools", ["b"]));
  assert.notEqual(loadKey("pools", []), loadKey("accounts", []));
  cacheLoad(key(0), { kind: "ok", value: "zero" });
  cacheLoad(key(0), { kind: "error", message: "busy" });
  assert.deepEqual(cachedLoad(key(0)), { kind: "ok", value: "zero" });
  cacheLoad(key(1), { kind: "not_ready" });
  assert.equal(cachedLoad(key(1)), undefined);
  for (let index = 1; index <= LOAD_CACHE_MAX; index += 1) cacheLoad(key(index), { kind: "ok", value: index });
  assert.equal(cachedLoad(key(0)), undefined, "the oldest entry is dropped past the bound");
  assert.deepEqual(cachedLoad(key(LOAD_CACHE_MAX)), { kind: "ok", value: LOAD_CACHE_MAX });
  assert.deepEqual(cachedLoad(key(1)), { kind: "ok", value: 1 });
  assert.equal(cachedLoad(null), undefined);
});

test("a second mount of a cached read shows its last value at once, then the new reply; an error still shows", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    const first = await evaluate(`Fixture.mount("a", "probe")`);
    const firstReply = await evaluate(`Fixture.reply({ kind: "ok", value: "first" })`);
    const second = await evaluate(`Fixture.mount("b", "probe")`);
    const secondReply = await evaluate(`Fixture.reply({ kind: "ok", value: "second" })`);
    const third = await evaluate(`Fixture.mount("c", "probe")`);
    const failed = await evaluate(`Fixture.reply({ kind: "error", message: "The engine is busy." })`);
    const fourth = await evaluate(`Fixture.mount("d", "probe")`);
    const uncached = await evaluate(`Fixture.mount("e")`);
    return { first, firstReply, second, secondReply, third, failed, fourth, uncached };
  });
  assert.deepEqual(seen.first, ["a:loading"]);
  assert.equal(seen.firstReply.text, "first");
  assert.deepEqual(seen.second, ["b:first"], "the remount renders the cached value on its first render");
  assert.equal(seen.secondReply.text, "second");
  assert.ok(!seen.secondReply.renders.includes("b:loading"), `no loading state on a cached remount: ${seen.secondReply.renders}`);
  assert.deepEqual(seen.third, ["c:second"]);
  assert.equal(seen.failed.text, "error", "a failed refresh shows its error instead of the cached value");
  assert.deepEqual(seen.fourth, ["d:second"], "the failed refresh left the last good value cached");
  assert.deepEqual(seen.uncached, ["e:loading"], "a read without a cache key still starts loading");
});

test("screens mounting together share one cached read in flight; a reload reads fresh; uncached reads stay separate", async () => {
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    const mounted = await evaluate(`Fixture.mountTwins("twin")`);
    const answered = await evaluate(`Fixture.answerTwin(0, "one")`);
    const reloaded = await evaluate(`Fixture.reloadTwin("x")`);
    const fresh = await evaluate(`Fixture.answerTwin(1, "two")`);
    const busy = await evaluate(`Fixture.mountTwins("busy")`);
    const reloadedBusy = await evaluate(`Fixture.reloadTwin("x")`);
    const newer = await evaluate(`Fixture.answerTwin(1, "newer")`);
    const uncached = await evaluate(`Fixture.mountTwins()`);
    return { mounted, answered, reloaded, fresh, busy, reloadedBusy, newer, uncached };
  });
  assert.deepEqual(seen.mounted, { reads: 1, x: "loading", y: "loading" }, "two mounts of one cached read ask the engine once");
  assert.deepEqual(seen.answered, { reads: 1, x: "one", y: "one" }, "both screens get the shared reply");
  assert.equal(seen.reloaded.reads, 2, "a reload asks the engine again");
  assert.deepEqual(seen.fresh, { reads: 2, x: "two", y: "one" });
  assert.equal(seen.busy.reads, 1);
  assert.equal(seen.reloadedBusy.reads, 2, "a reload never joins a read that started before it");
  assert.equal(seen.newer.x, "newer");
  assert.equal(seen.uncached.reads, 2, "reads without a cache key are not shared");
});
