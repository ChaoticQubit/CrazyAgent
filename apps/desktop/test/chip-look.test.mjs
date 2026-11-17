import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/chip-look.mjs", import.meta.url).pathname);
const sheet = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const rules = [sheet("../src/style.css"), sheet("../src/screens/project/projects.css"), sheet("../src/screens/team/team-graph.css")].join("\n");
const CLEAR = "rgba(0, 0, 0, 0)";
const DOT = { width: "6px", height: "6px" };
const BADGE_TONE = { ok: "ok", warn: "warn", danger: "danger", info: "running", neutral: "text-2" };
const CONSOLE_TONE = { ok: "terminal-green", warn: "terminal-yellow", danger: "terminal-red", muted: "terminal-muted" };

/**
 * Mounts the chips in one theme and reads their look. Input: the theme.
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
  test(`${theme}: a status badge is a dot plus its word, no fill, both in its tone, the info tone in --running`, async () => {
    const seen = await looks(theme);
    for (const [tone, name] of Object.entries(BADGE_TONE)) {
      const color = seen.tokens[name];
      assert.deepEqual(seen.badges[tone], { background: CLEAR, color, size: "12px", weight: "500", padding: "0px", dot: { color, ...DOT } }, tone);
    }
  });

  test(`${theme}: the master console state is a dot plus its word in terminal colors, with no tinted fill`, async () => {
    const seen = await looks(theme);
    for (const [tone, name] of Object.entries(CONSOLE_TONE)) {
      const color = seen.tokens[name];
      assert.deepEqual(seen.console[tone], { background: CLEAR, color, size: "11px", weight: seen.console[tone].weight, padding: "0px", dot: { color, ...DOT } }, tone);
    }
  });

  test(`${theme}: a team graph dot takes its state color, the info dot --running`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.dots, { info: seen.tokens.running, ok: seen.tokens.ok, warn: seen.tokens.warn, danger: seen.tokens.danger });
  });
}

test("the badge, tone, console state and graph dot rules name no accent token, no literal color, fill or font size", () => {
  const chips = rules.split("\n").filter((line) => /^\.(?:badge|tone-|pj-console-state|tg-dot)/.test(line));
  assert.ok(chips.length >= 11, `found ${chips.length} chip rules`);
  assert.deepEqual(chips.filter((line) => /var\(--(?:accent|on-accent)|rgba?\(|#[0-9a-fA-F]{3,8}\b|font-size:\s*[0-9]/.test(line)), []);
  assert.deepEqual(chips.filter((line) => /^\.(?:tone-|pj-console-state\.)/.test(line) && /background/.test(line)), []);
  assert.match(rules, /\.tg-dot\.tone-info \{\s*background: var\(--running\);\s*\}/);
});

test("the working badge's dot pulses only when motion is allowed, and no other badge rule animates", () => {
  const style = sheet("../src/style.css");
  assert.ok(style.includes("@media (prefers-reduced-motion: no-preference) { .badge.tone-info::before { animation: badge-pulse 1.2s ease-in-out infinite; } }"), "the pulse sits behind the motion preference");
  assert.ok(style.includes("@keyframes badge-pulse { 50% { opacity: 0.4; } }"), "the dot dims to 40 percent at mid-pulse");
  assert.deepEqual(style.split("\n").filter((line) => /^\.(?:badge|tone-)/.test(line) && /animation/.test(line)), []);
});
