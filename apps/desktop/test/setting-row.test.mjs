import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/setting-row.mjs", import.meta.url).pathname);
const CONTRAST_MIN = 4.5;
const WIDTH_PX = 760;
const NARROW_PX = 320;
const ACTION_GAP_MAX = 17;
const SEGMENT_COLUMN_PX = 110;

/** Runs a check on a mounted section. Inputs: the section's width, the check and the theme.
 * Output: the check's value. */
function onSection(width, check, theme = "light") {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1180, height: 800, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${width}, ${JSON.stringify(theme)})`);
    return check(page);
  });
}

const rows = ({ evaluate }) => evaluate("Fixture.rows()");

test("a row keeps its label and help on the left and its control on the right", async () => {
  const [plain] = await onSection(WIDTH_PX, rows);
  assert.ok(plain.control > plain.label, JSON.stringify(plain));
  assert.notEqual(plain.help, null, "the help is shown");
  assert.ok(plain.help >= plain.labelBottom - 1, JSON.stringify(plain));
  assert.ok(plain.helpRight <= plain.control, JSON.stringify(plain));
  assert.ok(Math.abs(plain.controlMiddle - plain.rowMiddle) <= 1, JSON.stringify(plain));
});

test("only a row whose value differs from its default carries the changed mark", async () => {
  const shown = await onSection(WIDTH_PX, rows);
  assert.deepEqual(shown.map((row) => row.marks), [[], ["changed"], [], [], []]);
});

test("a wide row stacks its control under the label at the label's left edge", async () => {
  const wide = (await onSection(WIDTH_PX, rows))[3];
  assert.ok(Math.abs(wide.control - wide.label) <= 1, JSON.stringify(wide));
  assert.ok(Math.abs(wide.childLeft - wide.label) <= 1, JSON.stringify(wide));
  assert.ok(wide.controlTop >= wide.labelBottom - 1, JSON.stringify(wide));
});

test("a real click on a named row's label focuses its control", async () => {
  const focused = await onSection(WIDTH_PX, async ({ evaluate, send }) => {
    const at = await evaluate(`Fixture.point('.set-row[data-setting="Pool name"] .set-label')`);
    for (const type of ["mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, x: at.x, y: at.y, button: "left", clickCount: 1 });
    return evaluate("Fixture.focused()");
  });
  assert.equal(focused, "named");
});

test("real Tab keys walk the section's action, then each row's control in order", async () => {
  const walked = await onSection(WIDTH_PX, async ({ evaluate, send }) => {
    const seen = [];
    for (let at = 0; at < 5; at += 1) {
      for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
      seen.push(await evaluate("Fixture.focused()"));
    }
    return seen;
  });
  assert.deepEqual(walked, ["act", "plain", "marked", "named", "wide"]);
});

test("at a narrow width nothing scrolls sideways and the control stays inside the section", async () => {
  const seen = await onSection(NARROW_PX, async (page) => ({ rows: await rows(page), section: await page.evaluate("Fixture.section()") }));
  assert.equal(seen.section.sideways, false, JSON.stringify(seen.section));
  assert.ok(seen.rows[0].right <= seen.section.right, JSON.stringify(seen));
});

test("at a narrow width two controls wrap inside their column and never cross the label", async () => {
  const two = (await onSection(NARROW_PX, rows)).at(-1);
  const [first, second] = two.parts;
  assert.ok(two.control >= two.labelRight, JSON.stringify(two));
  assert.ok(second.top >= first.bottom, JSON.stringify(two));
  assert.ok(first.left >= two.control - 1, JSON.stringify(two));
  assert.ok(second.right <= two.right + 1, JSON.stringify(two));
});

test("the section is named by its h3 and shows its purpose line and actions inside its box", async () => {
  const section = await onSection(WIDTH_PX, ({ evaluate }) => evaluate("Fixture.section()"));
  assert.equal(section.heading, "Your pools");
  assert.equal(section.name, "Your pools");
  assert.equal(section.about, "Sample words.");
  assert.equal(section.action, true, JSON.stringify(section));
  assert.ok(section.actionGap <= ACTION_GAP_MAX, JSON.stringify(section));
});

for (const theme of ["light", "dark"]) {
  test(`the changed mark and the help read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onSection(WIDTH_PX, async ({ evaluate }) => ({
      changed: await evaluate('Fixture.contrast(".set-changed")'),
      help: await evaluate('Fixture.contrast(".set-help")'),
    }), theme);
    for (const [name, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${name}: ${ratio}`);
  });
}

test("a segmented choice whose label wraps in a narrow column keeps every label inside its own pill (DESIGN-3-L2)", async () => {
  const pills = await withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1180, height: 800, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mountSegmented(${SEGMENT_COLUMN_PX})`);
    return page.evaluate("Fixture.pills()");
  });
  assert.ok(pills.some((pill) => pill.lines > 1), `a label wraps at ${SEGMENT_COLUMN_PX}px: ${JSON.stringify(pills)}`);
  for (const pill of pills) {
    assert.ok(pill.text.top >= pill.box.top - 0.5 && pill.text.bottom <= pill.box.bottom + 0.5, `the label stays inside its pill: ${JSON.stringify(pill)}`);
    assert.ok(pill.box.bottom <= pill.group.bottom + 0.5 && pill.box.right <= pill.group.right + 0.5, `the pill stays inside the group: ${JSON.stringify(pill)}`);
  }
});

test("no segmented button rule fixes a height a wrapped label can spill out of (DESIGN-3-L2 rule)", async () => {
  const { readFileSync } = await import("node:fs");
  const rules = ["../src/style.css", "../src/screens/settings/settings.css"].flatMap((path) => [...readFileSync(new URL(path, import.meta.url), "utf8").matchAll(/([^{}]*\.segmented button[^{}]*)\{([^}]*)\}/gu)]);
  assert.ok(rules.length > 0);
  for (const [, selector, body] of rules) assert.doesNotMatch(body, /(^|;)\s*height:/u, `${selector.trim()} grows with its label`);
  const shell = readFileSync(new URL("../src/style.css", import.meta.url), "utf8");
  assert.match(shell, /\.segmented button \{[^}]*min-height: 26px;/u, "the pill keeps its usual height as a floor");
});
