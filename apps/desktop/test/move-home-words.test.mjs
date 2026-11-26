import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { moveUnreadLine } = await import("../src/screens/project/move-home-words.ts");

test("an unread folder names iCloud on macOS", () => {
  assert.equal(moveUnreadLine(true), "CrazyAgent couldn't read this folder. Anything in it only in iCloud is downloaded first, rebuildable folders too, which can be slow.");
});

test("an unread folder says only that on Windows and Linux", () => {
  const elsewhere = moveUnreadLine(false);
  assert.equal(elsewhere, "CrazyAgent couldn't read this folder.");
  assert.doesNotMatch(elsewhere, /iCloud/);
});
