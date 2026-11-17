import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/settings-look.mjs", import.meta.url).pathname);
const settings = readFileSync(new URL("../src/screens/settings/settings.css", import.meta.url), "utf8");
const projects = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");
const LOUD = /var\(--(?:accent|on-accent)|font-weight:\s*[67]00|font:\s*[67]00|font-size:\s*[0-9.]+(?:px|rem|em)|\bfont:[^;]*\b[0-9.]+px/;

/**
 * Mounts the Settings frame in one theme and reads its look. Input: the theme.
 * Output: what `Fixture.look()` returns.
 */
function looks(theme) {
  return withPage(async (page) => {
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    return page.evaluate("Fixture.look()");
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: the chosen Settings nav row is ink on --surface-3 like the main nav's, every row at 500, under muted group captions`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.nav.current, { fill: seen.tokens["surface-3"], color: seen.tokens.text, weight: "500" });
    assert.equal(seen.nav.other, "500");
    assert.deepEqual(seen.nav.group, { size: "12px", line: "16px", weight: "400", color: seen.tokens["text-3"] });
  });

  test(`${theme}: the page title is 16/24 at 600 and a section title is a small muted caption at 500`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.top, { title: { size: "16px", line: "24px", weight: "600", color: seen.tokens.text }, about: "13px" });
    assert.deepEqual(seen.section.title, { size: "12px", line: "16px", weight: "500", color: seen.tokens["text-3"] });
    assert.deepEqual(seen.section.about, { size: "12px", color: seen.tokens["text-2"] });
    assert.deepEqual(seen.section.sub, { size: "12px", line: "16px", weight: "500", color: seen.tokens["text-3"] });
  });

  test(`${theme}: a setting's label is 500, its help 12px, and its changed mark is ink with an ink dot`, async () => {
    const seen = await looks(theme);
    assert.equal(seen.section.label, "500");
    assert.equal(seen.section.help, "12px");
    assert.deepEqual(seen.section.changed, { color: seen.tokens.text, dot: seen.tokens.primary });
  });
}

test("settings.css names no accent token, no weight 600 or 700 and no literal size, and a chosen pool reads like a chosen list row", () => {
  assert.deepEqual(settings.split("\n").filter((line) => LOUD.test(line)), []);
  assert.ok(settings.includes(".pool-item.on { background: var(--surface-3); box-shadow: inset 3px 0 0 var(--primary); }"), "chosen pool");
});

test("the shared settings rules in projects.css name no accent token, no weight 600 or 700 and no literal size", () => {
  const rules = projects.split("\n").filter((line) => line.startsWith(".set-") || line.startsWith(".set-row"));
  assert.ok(rules.length >= 30, `found ${rules.length} settings rules`);
  assert.deepEqual(rules.filter((line) => LOUD.test(line)), []);
});
