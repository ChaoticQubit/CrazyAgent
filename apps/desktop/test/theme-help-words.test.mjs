import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { themeHelp } = await import("../src/ui/keys.ts");

test("the Theme help names the Mac on macOS", () => {
  assert.equal(themeHelp(true), "System follows your Mac. This choice is kept in this app only.");
});

test("the Theme help names the computer's own setting on Windows and Linux", () => {
  const elsewhere = themeHelp(false);
  assert.equal(elsewhere, "System follows your computer's light or dark setting. This choice is kept in this app only.");
  assert.doesNotMatch(elsewhere, /Mac/);
});
