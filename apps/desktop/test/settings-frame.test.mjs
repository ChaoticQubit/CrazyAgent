import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, withPage } from "./browser.mjs";

const fixture = await bundle(new URL("./fixtures/settings-frame.mjs", import.meta.url).pathname);
const settings = await import("../src/screens/settings/index.tsx");
const router = await import("../src/ui/router.ts");
const CONTRAST_MIN = 4.5;

/** Runs a check on a mounted frame. Inputs: the tab, the window width, the check and the theme.
 * Output: the check's value. */
function onFrame(tab, width, check, theme = "light") {
  return withPage(async (page) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width, height: 800, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(fixture);
    await page.evaluate(`Fixture.mount(${JSON.stringify(tab)}, ${JSON.stringify(theme)})`);
    return check(page);
  });
}

test("a grouped list replaces the tab row: Access, Agents and App in the approved order", async () => {
  const shown = await onFrame("pools", 1180, ({ evaluate }) => evaluate("Fixture.structure()"));
  assert.deepEqual(shown.groups, [
    { name: "Access", links: ["Accounts", "Pools"] },
    { name: "Agents", links: ["Harnesses", "Plugins and skills", "Process", "Pack", "Memory", "Finished agents", "Quitting", "Master"] },
    { name: "App", links: ["Sandbox", "Team alerts"] },
  ]);
  assert.deepEqual(shown.current, ["Pools"]);
  assert.equal(shown.tabs, 0, "no tab roles are left");
  assert.equal(shown.heading, "Settings");
});

test("the list refuses a section left out, repeated or unknown, and names every section today", () => {
  const { LISTED_TABS } = settings;
  const every = LISTED_TABS.slice(0, -1);
  const list = (ids) => [{ title: "One", items: ids.map((id) => ({ id, label: id })) }];
  const refused = /must name every section once/;
  assert.doesNotThrow(() => settings.assertEverySection(settings.SETTINGS_GROUPS, LISTED_TABS));
  assert.throws(() => settings.assertEverySection(list(every), LISTED_TABS), refused);
  assert.throws(() => settings.assertEverySection(list([...every, every[0]]), LISTED_TABS), refused);
  assert.throws(() => settings.assertEverySection(list([...every, "nope"]), LISTED_TABS), refused);
});

test("Profile, Updates, GitHub and Projects folder leave the list for the user menu and keep their routes (DEC-151)", () => {
  const four = ["profile", "updates", "github", "home"];
  assert.deepEqual([...router.USER_MENU_TABS], four);
  const listed = settings.SETTINGS_GROUPS.flatMap((group) => group.items.map((item) => item.id));
  for (const tab of four) {
    assert.ok(!listed.includes(tab), `${tab} is not in the Settings list`);
    assert.ok(!settings.LISTED_TABS.includes(tab), `${tab} is not a listed section`);
    assert.ok(router.SETTINGS_TABS.includes(tab), `${tab} keeps its route for deep links`);
    assert.deepEqual(router.parseRoute(`#/settings/${tab}`), { name: "settings", tab });
  }
  assert.deepEqual([...settings.LISTED_TABS].sort(), router.SETTINGS_TABS.filter((tab) => !four.includes(tab)).sort());
});

test("the list stands left of the panel at the default size and stays in view while the panel scrolls", async () => {
  const shown = await onFrame("memory", 1180, async ({ evaluate }) => {
    const before = await evaluate("Fixture.layout()");
    await evaluate("document.querySelector('.main').scrollTop = 900");
    await evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
    return { before, after: await evaluate("Fixture.layout()") };
  });
  assert.ok(shown.before.navRight <= shown.before.panelLeft, JSON.stringify(shown));
  assert.ok(shown.after.navTop >= 0 && shown.after.navTop <= 40, JSON.stringify(shown));
  assert.ok(shown.after.panelTop < 0, `the panel did scroll: ${JSON.stringify(shown)}`);
});

test("at the 760 minimum the list stands above the panel and nothing scrolls sideways", async () => {
  const shown = await onFrame("memory", 760, ({ evaluate }) => evaluate("Fixture.layout()"));
  assert.ok(shown.panelTop > shown.navTop, JSON.stringify(shown));
  assert.equal(shown.sideways, false, JSON.stringify(shown));
});

for (const width of [1000, 1051, 1070]) {
  test(`at ${width}px, where the list wraps above the panel, it does not stick, so it never rides over the panel`, async () => {
    const shown = await onFrame("memory", width, async ({ evaluate }) => {
      await evaluate("document.querySelector('.main').scrollTop = 600");
      await evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
      return evaluate("Fixture.layout()");
    });
    assert.equal(shown.navPosition, "static", JSON.stringify(shown));
    assert.ok(shown.navBottom <= shown.panelTop, JSON.stringify(shown));
  });
}

test("the list stops sticking up to 1070px, past the 1052px wrap point with room for a scrollbar (DESIGN-2-L1)", async () => {
  const { readFileSync } = await import("node:fs");
  const css = readFileSync(new URL("../src/screens/settings/settings.css", import.meta.url), "utf8");
  const bound = css.match(/@media \(max-width: (\d+)px\) \{ \.settings-nav \{ position: static;/u);
  assert.ok(bound, "the settings list has a static breakpoint");
  assert.ok(Number(bound[1]) >= 1070, `the breakpoint covers the wrap point: ${bound[1]}`);
});

test("real Tab keys walk every Settings link in list order with a visible ring, then enter the panel", async () => {
  const walked = await onFrame("accounts", 1180, async ({ evaluate, send }) => {
    const seen = [];
    for (let at = 0; at < 18; at += 1) {
      for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
      seen.push(await evaluate("Fixture.focused()"));
    }
    return seen;
  });
  assert.deepEqual(walked.map((item) => item.text), ["Skip to content", "Accounts", "Pools", "GitHub", "Profile", "Harnesses", "Plugins and skills", "Process", "Pack", "Memory", "Finished agents", "Quitting", "Master",
    "Projects folder", "Sandbox", "Team alerts", "Updates", "In the panel"]);
  assert.deepEqual([...new Set(walked.slice(1, 17).map((item) => item.outline))], ["solid"]);
});

test("a real Enter on a focused link opens that section's address", async () => {
  const hash = await onFrame("accounts", 1180, async ({ evaluate, send }) => {
    await evaluate(`document.querySelector('.settings-nav a[href="#/settings/sandbox"]').focus()`);
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    await evaluate("new Promise((done) => setTimeout(done, 50))");
    return evaluate("location.hash");
  });
  assert.equal(hash, "#/settings/sandbox");
});

test("opening another section from a scrolled panel starts at its top and keeps focus on the link", async () => {
  const seen = await onFrame("memory", 1180, async ({ evaluate, send }) => {
    await evaluate("document.querySelector('.main').scrollTop = 900");
    await evaluate("new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))");
    await evaluate(`document.querySelector('.settings-nav a[href="#/settings/sandbox"]').focus()`);
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    await evaluate("new Promise((done) => setTimeout(done, 50))");
    return { layout: await evaluate("Fixture.layout()"), focused: await evaluate("Fixture.focused()"), current: (await evaluate("Fixture.structure()")).current };
  });
  assert.deepEqual(seen.current, ["Sandbox"]);
  assert.equal(seen.layout.scrollTop, 0, JSON.stringify(seen));
  assert.ok(seen.layout.panelTop >= 0, JSON.stringify(seen));
  assert.equal(seen.focused.text, "Sandbox");
});

for (const theme of ["light", "dark"]) {
  test(`the list's titles and links read at 4.5 to 1 in ${theme}`, async () => {
    const seen = await onFrame("pools", 1180, async ({ evaluate }) => {
      const ratios = {};
      for (const selector of [".settings-nav-title", ".settings-nav-item:not([aria-current])", ".settings-nav-item[aria-current]"]) {
        ratios[selector] = await evaluate(`Fixture.contrast(${JSON.stringify(selector)})`);
      }
      return ratios;
    }, theme);
    for (const [selector, ratio] of Object.entries(seen)) assert.ok(ratio >= CONTRAST_MIN, `${selector}: ${ratio}`);
  });
}
