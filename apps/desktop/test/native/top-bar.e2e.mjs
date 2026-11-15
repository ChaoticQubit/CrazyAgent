import "../load-source.mjs";
import assert from "node:assert/strict";
import { test } from "node:test";
import { BAR, BELL, CONTROL, PANEL, controlLabel, engineAct, onEngine, openPanel, until } from "./keep-awake-engine.mjs";

const { ADD_PROJECT_WAYS, NEW_PROJECT_STEPS, PROJECT_TABS, SETTINGS_TABS, formatRoute } = await import("../../src/ui/router.ts");

const BAR_HEIGHT = 56;
const CONTRAST_MIN = 4.5;
const TONES = ["plain", "hold", "limit", "ended"];

const PLACED = `(() => {
  const bar = document.querySelector(${JSON.stringify(BAR)});
  const control = bar?.querySelector(${JSON.stringify(CONTROL)});
  const bell = bar?.querySelector(${JSON.stringify(BELL)});
  if (!bar || !control || !bell) return { missing: [!bar && "bar", !control && "control", !bell && "bell"].filter(Boolean) };
  const on = (element) => {
    const box = element.getBoundingClientRect();
    return document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2)?.closest(element.tagName.toLowerCase()) === element;
  };
  const [frame, pill, ring] = [bar, control, bell].map((element) => element.getBoundingClientRect());
  const main = document.getElementById("main");
  return {
    bars: document.querySelectorAll(${JSON.stringify(BAR)}).length,
    top: Math.round(frame.top),
    height: Math.round(frame.height),
    outside: main === null || !main.contains(bar),
    below: main === null || Math.round(main.getBoundingClientRect().top) >= Math.round(frame.bottom),
    left: pill.right <= ring.left,
    inside: pill.left >= frame.left && ring.right <= Math.min(frame.right, innerWidth) && pill.top >= frame.top && pill.bottom <= frame.bottom,
    sizes: [Math.round(pill.height), Math.round(ring.width), Math.round(ring.height)],
    onTop: document.querySelector("dialog[open]") !== null || (on(control) && on(bell)),
    labelled: control.innerText.trim().length > 0 && control.scrollWidth <= control.clientWidth,
  };
})()`;
const WANTED = { bars: 1, top: 0, height: BAR_HEIGHT, outside: true, below: true, left: true, inside: true, sizes: [36, 36, 36], onTop: true, labelled: true };

const CONTRAST = `(() => {
  const channel = (value) => { const part = value / 255; return part <= 0.03928 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4; };
  const parse = (color) => (color.match(/[\\d.]+/g) ?? []).map(Number);
  const light = ([red, green, blue]) => 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue);
  const behind = (element) => {
    for (let at = element; at; at = at.parentElement) {
      const [red, green, blue, alpha = 1] = parse(getComputedStyle(at).backgroundColor);
      if (alpha === 1) return [red, green, blue];
    }
    return [255, 255, 255];
  };
  const control = document.querySelector(${JSON.stringify(CONTROL)});
  const kept = control.className;
  const ratios = {};
  for (const tone of ${JSON.stringify(TONES)}) {
    control.className = "keep-awake-control " + tone;
    const [text, back] = [light(parse(getComputedStyle(control).color)), light(behind(control))];
    ratios[tone] = Math.round(((Math.max(text, back) + 0.05) / (Math.min(text, back) + 0.05)) * 100) / 100;
  }
  control.className = kept;
  return ratios;
})()`;

/** Every route `formatRoute` can write for this engine's one project, each once. Input: the
 * project id. Output: the location hashes. */
function routes(projectId) {
  const all = [
    { name: "welcome" }, { name: "home" }, { name: "metrics" }, { name: "demo" },
    ...NEW_PROJECT_STEPS.map((step) => ({ name: "new-project", step })),
    ...ADD_PROJECT_WAYS.map((way) => ({ name: "add-project", way })),
    ...SETTINGS_TABS.map((tab) => ({ name: "settings", tab })),
    ...PROJECT_TABS.map((tab) => ({ name: "project", id: projectId, tab })),
    { name: "onboarding", id: projectId },
  ].map(formatRoute);
  assert.equal(new Set(all).size, all.length, "each route once");
  assert.ok(all.length >= 9 + NEW_PROJECT_STEPS.length + SETTINGS_TABS.length + PROJECT_TABS.length);
  return all;
}

test("the_top_bar_with_bell_and_keep_awake_is_on_every_route", { timeout: 1200000 }, (t) => onEngine(t, "ca-top-bar-routes-", async ({ me, page, open, projectId }) => {
  assert.ok(projectId, "the walk has a project");
  for (const hash of routes(projectId)) {
    await open(hash, hash, () => true);
    await until(`the bar with Keep awake left of the bell on ${hash}`, WANTED, () => page.evaluate(PLACED));
    await until(`Keep awake read on ${hash}`, "Auto, nothing running", () => controlLabel(page));
  }
  const project = `#/projects/${encodeURIComponent(projectId)}`;
  for (const tab of ["overview", "conversation", "ide"]) {
    await open(`${project}/${tab}`, `the ${tab} tab`, () => true);
    await page.evaluate(`(() => { const main = document.getElementById("main"); if (main) main.scrollTop = main.scrollHeight; })()`);
    await until(`the bar stays over a scrolled ${tab} tab`, WANTED, () => page.evaluate(PLACED));
  }
  await open(`${project}/conversation`, "the Conversation tab", () => true);
  await until("the whole master view inside the window, under the bar", true, () => page.evaluate(`(() => {
    const main = document.querySelector(".main")?.getBoundingClientRect();
    const view = document.querySelector(".pj-fixed")?.getBoundingClientRect();
    return !!main && !!view && view.top >= ${BAR_HEIGHT} && view.bottom <= main.bottom + 0.5 && main.bottom <= innerHeight + 0.5;
  })()`));
  await me.capture("top-bar-routes");
}, { withProject: true }));

test("the top bar is there on Welcome, where no sidebar exists", { timeout: 600000 }, (t) => onEngine(t, "ca-top-bar-welcome-", async ({ me, page, open }) => {
  await open("#/welcome", "Welcome", (text) => text.includes("Welcome"));
  await until("the bar on Welcome", WANTED, () => page.evaluate(PLACED));
  assert.equal(await page.evaluate(`document.querySelector("aside.sidebar") === null`), true, "first run has no sidebar");
  assert.equal(await page.evaluate(`document.querySelector(".top-bar-title")?.innerText.trim()`), "Welcome");
  await openPanel(me, page);
  await until("the panel under the bar, inside the window", true, () => page.evaluate(`(() => {
    const box = document.querySelector(${JSON.stringify(PANEL)})?.getBoundingClientRect();
    return !!box && box.top >= ${BAR_HEIGHT} - 8 && box.left >= 0 && box.right <= innerWidth && box.bottom <= innerHeight;
  })()`));
  await me.capture("top-bar-welcome");
}));

test("the Keep awake control reads clearly in light and dark, in every tone", { timeout: 600000 }, (t) => onEngine(t, "ca-top-bar-contrast-", async ({ bridge, me, page, open }) => {
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await until("Keep awake read", "Auto, nothing running", () => controlLabel(page));
  for (const theme of ["light", "dark"]) {
    await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
    const ratios = await page.evaluate(CONTRAST);
    for (const tone of TONES) assert.ok(ratios[tone] >= CONTRAST_MIN, `${tone} in ${theme} has contrast ${ratios[tone]}, wanted ${CONTRAST_MIN} or more`);
    await me.capture(`top-bar-${theme}`);
  }
  await engineAct(bridge, { start: { minutes: 60 } });
  await until("the control holding", true, async () => /^Awake, (1h|59m) left$/.test(await controlLabel(page)));
  for (const theme of ["light", "dark"]) {
    await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(CONTROL)}).dataset.tone`), "hold");
    const [text, edge] = await page.evaluate(`(() => { const style = getComputedStyle(document.querySelector(${JSON.stringify(CONTROL)})); return [style.color, style.borderTopColor]; })()`);
    assert.equal(text, edge, `a hold draws its edge in its text colour in ${theme}`);
    await me.capture(`top-bar-holding-${theme}`);
  }
  await page.send("Emulation.setEmulatedMedia", { features: [{ name: "forced-colors", value: "active" }] });
  assert.equal(await page.evaluate(`getComputedStyle(document.querySelector(${JSON.stringify(CONTROL)})).borderTopStyle`), "solid", "forced colours keep the control's edge");
  await page.send("Emulation.setEmulatedMedia", { features: [] });
  await engineAct(bridge, "end");
}));

test("at 760 px the bell and the Keep awake control stay whole and visible, in dark as in light", { timeout: 600000 }, (t) => onEngine(t, "ca-top-bar-760-", async ({ bridge, me, page, open, projectId }) => {
  await page.send("Emulation.setDeviceMetricsOverride", { width: 760, height: 900, deviceScaleFactor: 1, mobile: false });
  await engineAct(bridge, { start: { minutes: 134 } });
  for (const hash of ["#/home", `#/projects/${encodeURIComponent(projectId)}/conversation`, "#/settings/keep-awake"]) {
    await open(hash, hash, () => true);
    await until(`the control holding on ${hash}`, true, async () => /^Awake, 2h 1[34]m left$/.test(await controlLabel(page)));
    for (const theme of ["dark", "light"]) {
      await page.evaluate(`document.documentElement.setAttribute("data-theme", ${JSON.stringify(theme)})`);
      await until(`the bar at 760 px in ${theme} on ${hash}`, WANTED, () => page.evaluate(PLACED));
    }
    assert.equal(await page.evaluate("document.documentElement.scrollWidth <= innerWidth"), true, `${hash} does not scroll sideways`);
  }
  await page.evaluate(`document.documentElement.setAttribute("data-theme", "dark")`);
  await openPanel(me, page);
  await until("the panel inside a 760 px window", true, () => page.evaluate(`(() => {
    const box = document.querySelector(${JSON.stringify(PANEL)})?.getBoundingClientRect();
    return !!box && box.left >= 0 && box.right <= innerWidth && box.bottom <= innerHeight;
  })()`));
  await me.capture("top-bar-760-dark");
  await engineAct(bridge, "end");
}, { withProject: true }));
