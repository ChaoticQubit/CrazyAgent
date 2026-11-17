import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/base-weights.mjs", import.meta.url).pathname);
const sheet = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("DEC-192 on a bare page: h1 and h2 stay at 600 with their bold words, h3, strong and b sit at 500, inside a project page too", async () => {
  const seen = await withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate("Fixture.mount()");
    return page.evaluate("Fixture.look()");
  });
  assert.deepEqual(seen, {
    h1: "600", "h1-strong": "600", h2: "600", "h2-b": "600", h3: "500", strong: "500", b: "500",
    "pj-h2": "600", "pj-h2-strong": "600", "pj-h3": "500", "pj-strong": "500",
  });
});

test("no screen rule restates the h3 or strong weight the base rules now set", () => {
  const css = [sheet("../src/style.css"), sheet("../src/screens/changes/changes.css"), sheet("../src/screens/project/projects.css")].join("\n");
  for (const gone of [".ch-row > strong {", ".ch-detail h3 {", ".setup-panel li strong {", ".need strong {", ".pj-agents strong"]) {
    assert.ok(!css.includes(gone), gone);
  }
  assert.match(css, /^\.project-card \.title-row h3 \{ white-space: normal; overflow-wrap: anywhere; \}$/m);
  assert.match(css, /^\.pj-agents \.pj-kind \{ font-weight: var\(--weight-ui\); \}$/m, "agent names are spans, so they keep their own rule");
});
