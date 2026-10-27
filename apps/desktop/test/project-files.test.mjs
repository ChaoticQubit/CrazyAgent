import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const files = await import("../src/screens/project/project-files.ts");
const model = await import("../src/screens/project/console-model.ts");

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
