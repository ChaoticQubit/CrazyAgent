import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/sidebar-layout.mjs", import.meta.url).pathname);
const BOTTOM = ["Home", "New project", "Settings", "Metrics"];

/** Runs a check on a mounted sidebar in a window of a given height. Inputs: the mount arguments
 * as source text, the window height and the check. Output: the check's value. */
function onSidebar(mount, height, check) {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1180, height, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${mount})`);
    return check(page);
  });
}

test("the project list stands alone on top and Home, New project, Settings and Metrics stand below it", async () => {
  const shown = await onSidebar("3", 800, ({ evaluate }) => evaluate("Fixture.structure()"));
  assert.deepEqual(shown.navs, [
    { name: "Projects", links: ["Project 0", "Project 1", "Project 2"] },
    { name: "App", links: BOTTOM },
  ]);
  assert.match(shown.children[0], /brand-mark/u);
  assert.equal(shown.children.at(-1), "sidebar-foot");
});

test("a dividing line stands above the bottom group, which stays at the bottom with few projects", async () => {
  const shown = await onSidebar("2", 800, ({ evaluate }) => evaluate("Fixture.layout()"));
  assert.deepEqual(shown.divider, ["1px", "solid"]);
  assert.ok(shown.app.bottom <= shown.foot.top + 1, JSON.stringify(shown));
  assert.ok(shown.foot.bottom >= shown.window - 40, `the foot sits at the bottom: ${JSON.stringify(shown)}`);
});

test("a long project list scrolls by itself and the bottom group and footer stay in the window", async () => {
  const shown = await onSidebar("60", 640, ({ evaluate }) => evaluate("Fixture.layout()"));
  assert.equal(shown.listScrolls, true, JSON.stringify(shown));
  assert.equal(shown.asideScrolls, false, JSON.stringify(shown));
  assert.ok(shown.list.bottom <= shown.app.top + 1, JSON.stringify(shown));
  assert.ok(shown.foot.bottom <= shown.window, JSON.stringify(shown));
});

test("a very short window still reaches the footer through the sidebar's own scroll", async () => {
  const shown = await onSidebar("60", 260, ({ evaluate }) => evaluate("Fixture.layout()"));
  assert.equal(shown.asideScrolls, true, JSON.stringify(shown));
  assert.ok(shown.list.bottom - shown.list.top >= 60, `the list keeps some room: ${JSON.stringify(shown)}`);
});

test("real Tab keys walk the projects, then the bottom group, then the profile and Try again, and Shift+Tab walks back", async () => {
  const walked = await onSidebar('2, { name: "home" }, "down"', 800, async ({ evaluate, send }) => {
    const press = async (modifiers) => {
      for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9, modifiers });
      return evaluate("Fixture.focused()");
    };
    const forward = [];
    for (let at = 0; at < 9; at += 1) forward.push(await press(0));
    return { forward, back: await press(8) };
  });
  assert.deepEqual(walked.forward, ["Skip to content", "Project 0", "Project 1", ...BOTTOM, "Mira Okafor", "Try again"]);
  assert.equal(walked.back, "Mira Okafor");
});

for (const [route, expected] of [
  ['{ name: "home" }', "Home"], ['{ name: "new-project", step: "basics" }', "New project"],
  ['{ name: "settings", tab: "pools" }', "Settings"], ['{ name: "metrics" }', "Metrics"],
  ['{ name: "project", id: "p1", tab: "overview" }', "Project 1"],
]) {
  test(`the sidebar marks ${expected} as the current page and nothing else`, async () => {
    const shown = await onSidebar(`3, ${route}`, 800, ({ evaluate }) => evaluate("Fixture.structure()"));
    assert.deepEqual(shown.current, [expected]);
  });
}

for (const theme of ["light", "dark"]) {
  test(`the sidebar's words keep 4.5:1 contrast in ${theme}`, async () => {
    const ratios = await onSidebar(`3, { name: "metrics" }, "running", "${theme}"`, 800, async ({ evaluate }) => ({
      project: await evaluate('Fixture.contrast(".nav-group .nav-item")'),
      label: await evaluate('Fixture.contrast(".nav-label")'),
      link: await evaluate('Fixture.contrast(".app-nav .nav-item:not([aria-current])")'),
      current: await evaluate('Fixture.contrast(".app-nav .nav-item[aria-current]")'),
      engine: await evaluate('Fixture.contrast(".engine-state")'),
    }));
    for (const [name, ratio] of Object.entries(ratios)) assert.ok(ratio >= 4.5, `${name} ${ratio}`);
  });
}
