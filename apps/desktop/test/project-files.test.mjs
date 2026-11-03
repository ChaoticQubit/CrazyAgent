import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const files = await import("../src/screens/project/project-files.ts");
const model = await import("../src/screens/project/console-model.ts");
const fixture = await bundle(new URL("./fixtures/project-files.mjs", import.meta.url).pathname);

test("the file-list query is the tag text, without control characters, cut to the engine's 256 bytes", () => {
  assert.equal(files.filesQuery(null), null, "no tag typed, no read");
  assert.equal(files.filesQuery(""), "", "a bare @ lists the first files");
  assert.equal(files.filesQuery("src/Cart"), "src/Cart");
  assert.equal(files.filesQuery("a\u0007b\u0000c"), "abc");
  assert.equal(files.filesQuery("x".repeat(300)), "x".repeat(files.FILES_QUERY_BYTES_MAX));
  const wide = files.filesQuery("é".repeat(200));
  assert.equal(new TextEncoder().encode(wide).length, 256);
  assert.equal(wide, "é".repeat(128), "a two-byte character is never split");
  const emoji = files.filesQuery(`${"a".repeat(254)}😀`);
  assert.equal(emoji, "a".repeat(254), "a four-byte character that would cross the bound is left out whole");
});

test("listed files become @ tags the console bounds to 200 and that skip paths a tag cannot hold", () => {
  const listed = ["src/cart.rs", "docs/plant care.md", ...Array.from({ length: 250 }, (_, index) => `f${index}.rs`)];
  const mentions = model.fileMentions(listed);
  assert.equal(mentions.length, model.FILES_MAX);
  assert.equal(model.FILES_MAX, 200, "the engine's FILES_MAX");
  assert.deepEqual(mentions.slice(0, 2).map((mention) => mention.handle), ["src/cart.rs", "f0.rs"]);
  assert.ok(files.FILES_DEBOUNCE_MS >= 100 && files.FILES_DEBOUNCE_MS <= 300);
});

test("typing an @ tag keeps one file read in flight and sends only the latest tag text once it returns", async () => {
  const wait = files.FILES_DEBOUNCE_MS * 3;
  const seen = await withPage(async ({ evaluate }) => {
    await evaluate(fixture);
    const first = await evaluate(`Fixture.mount("alpha", "s")`);
    const typing = await evaluate(`Fixture.type("sr", ${wait})`);
    const more = await evaluate(`Fixture.type("src/C", ${wait})`);
    const back = await evaluate(`Fixture.reply(["src/a.rs"], ${wait})`);
    const latest = await evaluate(`Fixture.reply(["src/Cart.rs"], ${wait})`);
    const idle = await evaluate(`Fixture.type("src/C", ${wait})`);
    return { first, typing, more, back, latest, idle };
  });
  assert.deepEqual(seen.first.asked, ["alpha:s"], "the first tag text is read at once");
  assert.deepEqual(seen.typing.asked, ["alpha:s"], "no second read starts while the first is out");
  assert.deepEqual(seen.more.asked, ["alpha:s"]);
  assert.deepEqual(seen.back.asked, ["alpha:s", "alpha:src/C"], "only the latest text is sent after the reply; sr never is");
  assert.deepEqual(seen.back.shown, ["src/a.rs"], "the earlier list shows while the latest text loads");
  assert.deepEqual(seen.latest.shown, ["src/Cart.rs"]);
  assert.deepEqual(seen.idle.asked, ["alpha:s", "alpha:src/C"], "unchanged text reads nothing more");
  assert.equal(seen.idle.open, 0);
  assert.equal(seen.idle.most, 1, "at most one file read was ever in flight");
});
