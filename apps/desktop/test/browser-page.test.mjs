import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { withPage } from "./browser.mjs";

test("a check's page is in standards mode, as the app's page is", async () => {
  const page = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  assert.match(page, /^<!doctype html>/i, "the app's page opens with a doctype");
  const mode = await withPage(({ evaluate }) => evaluate("document.compatMode"));
  assert.equal(mode, "CSS1Compat");
});
