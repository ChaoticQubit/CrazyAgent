import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/sidebar-layout.mjs", import.meta.url).pathname);
const BOTTOM = ["Home", "New project", "Settings", "Metrics"];
const LONG_NAME = "Maximiliana Featherstonehaugh-Cholmondeley";
const ONE_LINE_PX = 24;
const TARGET_PX = 24;

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

test("the footer shows the profile's initial beside its name, with the engine state under the name", async () => {
  const foot = await onSidebar("1", 800, ({ evaluate }) => evaluate("Fixture.foot()"));
  assert.equal(foot.initial, "M", JSON.stringify(foot));
  assert.equal(foot.hidden, "true", "the initial is decoration");
  assert.equal(foot.name, "Mira Okafor");
  assert.equal(foot.engine, "Engine running");
  assert.ok(foot.initialRight <= foot.nameLeft, JSON.stringify(foot));
  assert.ok(foot.engineTop >= foot.nameBottom - 1, JSON.stringify(foot));
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

test("every link in the sidebar opens its own page, and a flagged project shows its flag", async () => {
  const { hrefs } = await onSidebar('2, { name: "home" }, "running", "light", "Mira Okafor", "3"', 800, ({ evaluate }) => evaluate("Fixture.structure()"));
  assert.deepEqual(hrefs, {
    "Project 03": "#/projects/p0/overview", "Project 1": "#/projects/p1/overview", Home: "#/home", "New project": "#/projects/new?step=basics",
    Settings: "#/settings/accounts", Metrics: "#/metrics", "Mira Okafor": "#/settings/profile",
  });
});

test("a real Tab's focus ring on a project link stays inside the list that would clip it", async () => {
  const ring = await onSidebar("2", 800, async ({ evaluate, send }) => {
    for (let at = 0; at < 2; at += 1) {
      for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    }
    return evaluate("Fixture.ring()");
  });
  assert.equal(ring.text, "Project 0");
  assert.equal(ring.style, "solid");
  assert.ok(ring.width >= 2, JSON.stringify(ring));
  assert.deepEqual(ring.past.filter((reach) => reach > 0), [], JSON.stringify(ring));
});

for (const [what, profile] of [["a short", "Mira Okafor"], ["a very long", LONG_NAME]]) {
  test(`with ${what} profile name and the engine down the footer keeps its height, its state on one line and Try again in reach`, async () => {
    const seen = await onSidebar(`1, { name: "home" }, "running", "light", ${JSON.stringify(profile)}`, 800, async ({ evaluate }) => {
      const running = await evaluate("Fixture.foot()");
      await evaluate(`Fixture.mount(1, { name: "home" }, "down", "light", ${JSON.stringify(profile)})`);
      return { running, down: await evaluate("Fixture.foot()") };
    });
    const { running, down } = seen;
    assert.equal(down.footHeight, running.footHeight, JSON.stringify(seen));
    assert.ok(down.engineHeight <= ONE_LINE_PX, JSON.stringify(seen));
    assert.ok(down.retry.height >= TARGET_PX && down.retry.width >= TARGET_PX, JSON.stringify(seen));
    assert.ok(down.retry.right <= down.footRight, JSON.stringify(seen));
    assert.ok(down.nameRight <= down.retry.left, JSON.stringify(seen));
    assert.equal(down.nameCut, profile === LONG_NAME, JSON.stringify(seen));
    assert.equal(running.nameCut, profile === LONG_NAME, JSON.stringify(seen));
    assert.ok(running.nameRight <= running.footRight, JSON.stringify(seen));
  });
}

for (const [route, expected] of [
  ['{ name: "home" }', "Home"], ['{ name: "new-project", step: "basics" }', "New project"],
  ['{ name: "settings", tab: "pools" }', "Settings"], ['{ name: "settings", tab: "profile" }', "Settings"], ['{ name: "metrics" }', "Metrics"],
  ['{ name: "project", id: "p1", tab: "overview" }', "Project 1"],
]) {
  test(`the sidebar marks ${expected} as the current page and nothing else on ${route}`, async () => {
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
