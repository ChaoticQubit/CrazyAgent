import "../load-source.mjs";
import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { CONTROL, FORM, PANEL, SETTINGS_TITLE, controlLabel, engineAct, engineTry, engineView, minutesLeft, onEngine, openPanel, panelText, until } from "./keep-awake-engine.mjs";

const { KEEP_AWAKE_DEFAULTS, KEEP_AWAKE_SETTINGS } = await import("../../src/api-keep-awake.ts");

const SAVE = `${FORM} button[type=submit]`;
const SAVED = "Keep awake settings saved.";
const IDLE = { auto: "Auto, nothing running", manual: "Manual, no timer", off: "Off" };
const LID_LINES = { ac_only: "Lid closed Hold on AC power only", always: "Lid closed Hold always", off: "Lid closed Sleep as usual" };
const GRACE_LINES = {
  min0: "Auto, nothing running. Holds while agents work.",
  min5: "Auto, nothing running. Holds while agents work and 5 minutes after.",
  min10: "Auto, nothing running. Holds while agents work and 10 minutes after.",
  min30: "Auto, nothing running. Holds while agents work and 30 minutes after.",
};
const ENDS_LINES = {
  cap: { h4: "At 10% battery, critical heat, or 4 h", h8: "At 10% battery, critical heat, or 8 h", h24: "At 10% battery, critical heat, or 24 h", none: "At 10% battery or critical heat" },
  battery_floor: { off: "At critical heat or 24 h", pct5: "At 5% battery, critical heat, or 24 h", pct10: "At 10% battery, critical heat, or 24 h", pct20: "At 20% battery, critical heat, or 24 h" },
  heat_end: { critical: "At 10% battery, critical heat, or 24 h", serious: "At 10% battery, serious heat, or 24 h", never: "At 10% battery or 24 h" },
};

/** The option the settings page shows picked for one setting. Inputs: the page and the
 * setting. Output: the option's engine name, or null when none is picked. */
async function shown(page, setting) {
  return page.evaluate(`document.querySelector('input[type=radio][name="keep-awake-${setting}"]:checked')?.value ?? null`);
}

/** Opens Settings, Keep awake and waits for every setting. Inputs: the owner and the route
 * opener. Output: none. */
async function settingsPage(me, open) {
  await open("#/settings/keep-awake", "the Keep awake settings", (text) => text.includes(SETTINGS_TITLE));
  await me.waitFor("every Keep awake setting", (text) => KEEP_AWAKE_SETTINGS.every((setting) => text.includes(setting.label)));
}

/** Picks one option on the settings page as the owner does, saves it, and proves the engine
 * stored it and the page still shows it after a reload. Inputs: the row's tools, the setting
 * and the option. Output: none. */
async function save({ bridge, me, page, reload }, setting, option) {
  await me.click(option.label, `fieldset[data-setting="${setting.id}"] label`);
  await until(`${option.label} picked`, option.value, () => shown(page, setting.id));
  await me.follow(SAVE);
  await me.waitFor("the saved note", (text) => text.includes(SAVED));
  await until(`the engine stores ${setting.id} ${option.value}`, option.value, async () => (await engineView(bridge)).settings[setting.id]);
  await reload(SETTINGS_TITLE);
  await until(`${option.label} still picked after a reload`, option.value, () => shown(page, setting.id));
  const stored = (await engineView(bridge)).settings;
  for (const other of KEEP_AWAKE_SETTINGS) {
    if (other.id !== setting.id) assert.equal(stored[other.id], KEEP_AWAKE_DEFAULTS[other.id], `saving ${setting.id} leaves ${other.id} alone`);
  }
}

/** The visible effect one option has in the control or its panel, checked on the page.
 * Inputs: the row's tools, the setting id and the option's engine name. Output: none. */
async function effect({ bridge, me, page }, id, value) {
  if (id === "mode") return until(`the control for mode ${value}`, IDLE[value], () => controlLabel(page));
  await openPanel(me, page);
  if (id === "grace") return until("the panel's first line", true, async () => (await panelText(page)).includes(GRACE_LINES[value]));
  if (id === "lid") return until("the Lid closed line", true, async () => (await panelText(page)).includes(LID_LINES[value]));
  if (id in ENDS_LINES) return until("the Ends by itself line", true, async () => (await panelText(page)).includes(`Ends by itself ${ENDS_LINES[id][value]} `));
  if (id === "display_awake") {
    await me.click("Start 1 h", `${PANEL} button`);
    await until(`the hold with display ${value}`, { kind: "idle", display: value === "while_holding" }, async () => (await engineView(bridge)).state.want);
    await me.click("End now", `${PANEL} button`);
    return until("the hold ended", "nothing", async () => (await engineView(bridge)).state.want.kind);
  }
  assert.ok(["heat_notice", "lid_display"].includes(id), `${id} has an effect row`);
  const view = await engineView(bridge);
  assert.equal(view.settings[id], value, `${id} is stored; its effect lands with its own task and is proven there`);
}

/** The options of one setting in the order the rows pick them: the default last, so every
 * pick is a change. Input: the setting. Output: the options. */
function order(setting) {
  const start = setting.options.findIndex((option) => option.value === KEEP_AWAKE_DEFAULTS[setting.id]);
  assert.ok(start >= 0, `${setting.id} has its default among its options`);
  return [...setting.options.slice(start + 1), ...setting.options.slice(0, start + 1)];
}

for (const setting of KEEP_AWAKE_SETTINGS) {
  test(`Keep awake: every option of ${setting.label} saves from Settings, stays after a reload and shows in the control`, { timeout: 900000 }, (t) => onEngine(t, `ca-ka-${setting.id.replaceAll("_", "-")}-`, async (tools) => {
    const { me, open, page } = tools;
    await settingsPage(me, open);
    assert.equal(await shown(page, setting.id), KEEP_AWAKE_DEFAULTS[setting.id], "a new engine shows the default");
    for (const option of order(setting)) {
      await tools.t.test(`ka ${setting.label.toLowerCase()} ${option.label.toLowerCase()}`, async () => {
        await save(tools, setting, option);
        await effect(tools, setting.id, option.value);
      });
    }
  }));
}

test("Keep awake: Start 1 h, Start 4 h, Custom, Extend and End in the panel each change what the engine holds", { timeout: 900000 }, (t) => onEngine(t, "ca-ka-panel-", async ({ t: row, bridge, me, page, open }) => {
  const held = async () => {
    const view = await engineView(bridge);
    return [view.state.want.kind, view.state.reason, minutesLeft(view)];
  };
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await until("the control on a new engine", IDLE.auto, () => controlLabel(page));
  assert.deepEqual(await held(), ["nothing", "idle", null]);
  await openPanel(me, page);
  assert.equal(await page.evaluate(`[...document.querySelectorAll(${JSON.stringify(`${PANEL} button`)})].filter((button) => ["Extend 1 h", "End now"].includes(button.innerText.trim())).every((button) => button.disabled)`), true, "Extend and End wait for a hold");
  assert.ok((await panelText(page)).includes("Extend works while a timer runs. Start one first."));

  await row.test("ka panel start 1 h", async () => {
    await me.click("Start 1 h", `${PANEL} button`);
    await until("a one hour timer in the engine", ["idle", "timer", 60], held);
    await until("the control counting down", true, async () => /^Awake, (1h|59m) left$/.test(await controlLabel(page)));
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(CONTROL)}).dataset.tone`), "hold");
  });
  await row.test("ka panel extend", async () => {
    await me.click("Extend 1 h", `${PANEL} button`);
    await until("the timer an hour later", ["idle", "timer", 120], held);
    await until("the control counting down", true, async () => /^Awake, (2h|1h 59m) left$/.test(await controlLabel(page)));
  });
  await row.test("ka panel end", async () => {
    await me.click("End now", `${PANEL} button`);
    await until("nothing held in the engine", ["nothing", "idle", null], held);
    await until("the control at rest", IDLE.auto, () => controlLabel(page));
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(CONTROL)}).dataset.tone`), "plain");
  });
  await row.test("ka panel start 4 h", async () => {
    await me.click("Start 4 h", `${PANEL} button`);
    await until("a four hour timer in the engine", ["idle", "timer", 240], held);
    await until("the control counting down", true, async () => /^Awake, (4h|3h 59m) left$/.test(await controlLabel(page)));
  });
  await row.test("ka panel custom", async () => {
    await me.click("Custom...", `${PANEL} button`);
    await me.type("#keep-awake-minutes", "1441");
    await me.waitFor("the bounds", (text) => text.includes("Enter whole minutes from 1 to 1440."));
    assert.deepEqual(await held(), ["idle", "timer", 240], "minutes out of bounds start nothing");
    await me.type("#keep-awake-minutes", "90");
    await me.click("Start", `${PANEL} button`);
    await until("a ninety minute timer in the engine", ["idle", "timer", 90], held);
    await until("the control counting down", true, async () => /^Awake, 1h (30|29)m left$/.test(await controlLabel(page)));
  });
  await engineAct(bridge, "end");
  await until("the control at rest after the rows", IDLE.auto, () => controlLabel(page));
}));

test("Keep awake: the panel's Mode switch saves at once, closes on Escape and a click outside, and its link opens the settings", { timeout: 600000 }, (t) => onEngine(t, "ca-ka-panel-mode-", async ({ bridge, me, page, open }) => {
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await openPanel(me, page);
  for (const [label, mode] of [["Manual", "manual"], ["Off", "off"], ["Auto", "auto"]]) {
    await me.click(label, `${PANEL} fieldset[data-setting="mode"] label`);
    await until(`the engine stores mode ${mode}`, mode, async () => (await engineView(bridge)).settings.mode);
    await until(`the control for mode ${mode}`, IDLE[mode], () => controlLabel(page));
  }
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await until("the panel closed by Escape", false, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`));
  assert.equal(await page.evaluate(`document.activeElement === document.querySelector(${JSON.stringify(CONTROL)})`), true, "focus returns to the control");
  await openPanel(me, page);
  await me.follow(".top-bar-title");
  await until("the panel closed by a click outside", false, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`));
  await openPanel(me, page);
  await me.click("All Keep awake settings", `${PANEL} a`);
  await me.waitFor("the Keep awake settings", (text) => text.includes(SETTINGS_TITLE));
  assert.equal(await page.evaluate("location.hash"), "#/settings/keep-awake");
  await until("the panel closed after its link", false, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`));
}));

/** Whether a button in the open panel is disabled. Inputs: the page and the button's words.
 * Output: true or false, or null when the panel has no such button. */
async function panelButtonOff(page, label) {
  return page.evaluate(`(() => {
    const button = [...document.querySelectorAll(${JSON.stringify(`${PANEL} button`)})].find((entry) => entry.innerText.trim() === ${JSON.stringify(label)});
    return button ? button.disabled : null;
  })()`);
}

test("Keep awake: in mode Off the panel's Starts are off and it says how to start a timer", { timeout: 600000 }, (t) => onEngine(t, "ca-ka-panel-off-", async ({ bridge, me, page, open }) => {
  await engineAct(bridge, { set: { settings: { ...KEEP_AWAKE_DEFAULTS, mode: "off" } } });
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await until("the control for mode off", IDLE.off, () => controlLabel(page));
  await openPanel(me, page);
  for (const label of ["Start 1 h", "Start 4 h", "Custom...", "Extend 1 h", "End now"]) assert.equal(await panelButtonOff(page, label), true, label);
  assert.ok((await panelText(page)).includes("Keep awake is Off. Pick Auto or Manual to start a timer."));
  assert.equal((await panelText(page)).includes("Something went wrong"), false);
  await me.click("Manual", `${PANEL} fieldset[data-setting="mode"] label`);
  await until("the engine stores mode manual", "manual", async () => (await engineView(bridge)).settings.mode);
  await until("Start 1 h comes back", false, () => panelButtonOff(page, "Start 1 h"));
  assert.equal((await panelText(page)).includes("Keep awake is Off."), false);
}));

test("Keep awake: a Settings save keeps a mode changed in the panel or by another client while the page was open", { timeout: 600000 }, (t) => onEngine(t, "ca-ka-settings-stale-", async ({ bridge, me, page, open }) => {
  const grace = KEEP_AWAKE_SETTINGS.find((setting) => setting.id === "grace").options.find((option) => option.value === "min30");
  const cap = KEEP_AWAKE_SETTINGS.find((setting) => setting.id === "cap").options.find((option) => option.value === "h4");
  await settingsPage(me, open);
  await until("the page shows mode auto", "auto", () => shown(page, "mode"));
  await openPanel(me, page);
  await me.click("Off", `${PANEL} fieldset[data-setting="mode"] label`);
  await until("the engine stores mode off", "off", async () => (await engineView(bridge)).settings.mode);
  await until("the page shows the panel's mode without a reload", "off", () => shown(page, "mode"));
  for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
  await until("the panel closed", false, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`));
  await me.click(grace.label, `fieldset[data-setting="grace"] label`);
  await until("30 min picked", "min30", () => shown(page, "grace"));
  await me.follow(SAVE);
  await me.waitFor("the saved note", (text) => text.includes(SAVED));
  await until("the save kept the panel's mode", { mode: "off", grace: "min30" }, async () => {
    const stored = (await engineView(bridge)).settings;
    return { mode: stored.mode, grace: stored.grace };
  });

  await me.click(cap.label, `fieldset[data-setting="cap"] label`);
  await until("4 h picked", "h4", () => shown(page, "cap"));
  await engineAct(bridge, { set: { settings: { ...(await engineView(bridge)).settings, mode: "manual" } } });
  assert.equal(await shown(page, "cap"), "h4", "the pick is still on the page");
  await me.follow(SAVE);
  await until("the save kept the other client's mode", { mode: "manual", grace: "min30", cap: "h4" }, async () => {
    const stored = (await engineView(bridge)).settings;
    return { mode: stored.mode, grace: stored.grace, cap: stored.cap };
  });
}));

const LID_BUTTONS = ["Turn on lid-closed hold", "Open Login Items", "Remove lid helper"];
const LID_HELD_STILL_MS = 5000;

/** The lid helper buttons the page shows now, by their words. Input: the page. Output: the
 * words of each one shown. */
async function lidButtons(page) {
  return page.evaluate(`[...document.querySelectorAll("button")].map((entry) => entry.innerText.trim()).filter((words) => ${JSON.stringify(LID_BUTTONS)}.includes(words))`);
}

test("Keep awake: an engine with no lid helper in its bundle offers no lid helper button, and a lid command sent anyway reaches nothing", { timeout: 600000 }, (t) => onEngine(t, "ca-ka-lid-helper-", async ({ bridge, me, page, open }) => {
  const first = await engineView(bridge);
  if (first.platform !== "mac_os") {
    for (const action of ["turn_on_lid_hold", "remove_lid_helper"]) {
      const receipt = await engineTry(bridge, action);
      assert.deepEqual([receipt.outcome, receipt.rejection], ["rejected", "keep_awake_no_lid_helper"], action);
    }
    return;
  }
  await until("the lid line read this engine's bundle first (QUESTION 35k)", "helper_missing", async () => (await engineView(bridge)).lid_platform.kind);
  await settingsPage(me, open);
  await me.waitFor("the lid helper line", (text) => text.includes("Lid helper") && text.includes("Not set up on this computer yet"));
  assert.deepEqual(await lidButtons(page), [], "Settings offers no lid helper button");
  await openPanel(me, page);
  assert.deepEqual(await lidButtons(page), [], "the panel offers none either");
  for (const action of ["turn_on_lid_hold", "remove_lid_helper"]) await engineAct(bridge, action);
  for (const deadline = Date.now() + LID_HELD_STILL_MS; Date.now() < deadline; await pause(500)) {
    assert.equal((await engineView(bridge)).lid_platform.kind, "helper_missing", "a lid command stops at the bundle check");
  }
  assert.deepEqual(await lidButtons(page), []);
}));
