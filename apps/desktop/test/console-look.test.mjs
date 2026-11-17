import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/console-look.mjs", import.meta.url).pathname);
const consoleCss = readFileSync(new URL("../src/screens/project/console.css", import.meta.url), "utf8");
const projects = readFileSync(new URL("../src/screens/project/projects.css", import.meta.url), "utf8");

/**
 * The WCAG 2 contrast ratio of two computed colors. Inputs: two rgb() strings.
 * Output: the ratio, at least 1.
 */
function ratio(first, second) {
  const lum = (color) => {
    const [red, green, blue] = color.match(/[\d.]+/g).slice(0, 3).map((value) => {
      const part = Number(value) / 255;
      return part <= 0.03928 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const [light, dark] = [lum(first), lum(second)].sort((a, b) => b - a);
  return (light + 0.05) / (dark + 0.05);
}

/**
 * Mounts the console in one theme and reads its look; with `menu` it first types "@" into the
 * composer so the tag menu opens. Inputs: the theme and whether to open the menu.
 * Output: what `Fixture.look()` returns, plus `Fixture.picked()` under `menu`.
 */
function looks(theme, menu = false) {
  return withPage(async ({ evaluate, send }) => {
    await evaluate(fixture);
    const id = await evaluate(`Fixture.mount(${JSON.stringify(theme)})`);
    if (!menu) return evaluate("Fixture.look()");
    await evaluate(`document.getElementById(${JSON.stringify(id)}).focus()`);
    await send("Input.insertText", { text: "ask @" });
    await evaluate("new Promise((done) => setTimeout(done, 0))");
    return { ...(await evaluate("Fixture.look()")), menu: await evaluate("Fixture.picked()") };
  });
}

for (const theme of ["light", "dark"]) {
  test(`${theme}: every console color comes from a terminal token`, async () => {
    const seen = await looks(theme);
    const t = seen.tokens;
    assert.deepEqual(seen.colors, {
      delegate: t["terminal-muted"],
      error: t["terminal-bright-red"],
      entry: t["terminal-border"],
      tag: t["terminal-selection"],
      code: t["terminal-bright-yellow"],
      block: { fill: t["terminal-surface"], edge: t["terminal-border"] },
      cell: t["terminal-border"],
      status: t["terminal-muted"],
    });
  });

  test(`${theme}: console type sits on the scale, a # heading at 600, a ## heading and table header at 500, links underlined`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.type, { body: "13px", status: "12px", title: "600", section: "500", header: "500", link: "underline" });
  });

  test(`${theme}: the master head title is 500, its detail 12px, and its notes sit on --terminal-surface with a yellow dot on a warning`, async () => {
    const seen = await looks(theme);
    assert.deepEqual(seen.head, { title: "500", detail: "12px" });
    const fill = seen.tokens["terminal-surface"];
    assert.deepEqual(seen.notes, [
      { fill, dot: { color: seen.tokens["terminal-yellow"], width: "6px" }, second: "none" },
      { fill, dot: null, second: null },
    ]);
  });

  test(`${theme}: the picked @ option carries an inset ring at 3:1 or more on the terminal, not only a fill`, async () => {
    const seen = await looks(theme, true);
    assert.ok(seen.menu.options > 0, JSON.stringify(seen.menu));
    assert.equal(seen.menu.fill, seen.tokens["terminal-selection"]);
    assert.equal(seen.menu.inset, true);
    assert.equal(seen.menu.ring, seen.tokens["terminal-fg"]);
    assert.ok(ratio(seen.menu.ring, seen.menu.ground) >= 3, `${seen.menu.ring} on ${seen.menu.ground}`);
  });
}

test("console.css names terminal tokens in place of its six local colors and sets no literal size or weight 700", () => {
  assert.doesNotMatch(consoleCss, /--cs-(?:dim|rule|tag-bg|error|code|box)\b/);
  assert.doesNotMatch(consoleCss, /#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i);
  assert.doesNotMatch(consoleCss, /font-size:\s*[0-9]|font-weight:\s*[67]00/);
  assert.match(consoleCss, /--cs-line: 20px;/, "the line height stays the console's own size");
});

test("the master console rules in projects.css name no literal color, weight 600 or literal size", () => {
  const rules = projects.split("\n").filter((line) => line.startsWith(".pj-console"));
  assert.ok(rules.length >= 16, `found ${rules.length} console rules`);
  assert.deepEqual(rules.filter((line) => /#[0-9a-f]{3,8}\b|rgba?\(|font-weight:\s*[67]00|font-size:\s*[0-9]/i.test(line)), []);
});
