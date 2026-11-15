import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const core = await import("../src/api.ts");
const api = await import("../src/api-keep-awake.ts");
const model = await import("../src/screens/keep-awake/model.ts");
const control = await import("../src/screens/keep-awake/control.tsx");
const panel = await import("../src/screens/keep-awake/panel.tsx");
const settings = await import("../src/screens/settings/keep-awake.tsx");
const bar = await import("../src/ui/top-bar.tsx");
const shell = await import("../src/ui/shell.tsx");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");
const { SETTINGS_TABS, parseRoute } = await import("../src/ui/router.ts");

const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const ENGINE_SETTINGS = source("../../../crates/crazyagent/src/keep_awake/settings.rs");
const ENGINE_STATE = source("../../../crates/crazyagent/src/keep_awake/state.rs");
const ENGINE = source("../../../crates/crazyagent/src/keep_awake.rs");
const LIB = source("../src-tauri/src/lib.rs");
const API = source("../src/api-keep-awake.ts");
const SHELL = source("../src/ui/shell.tsx");
const APP = source("../src/app.tsx");
const NOTICE_CSS = source("../src/screens/notifications/notifications.css");
const SETTINGS_PAGE = source("../src/screens/settings/keep-awake.tsx");

const NOW = 1_800_000_000_000;
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const LABEL_MAX = 32;
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ").trim();
const snake = (variant) => variant.replace(/(?<!^)([A-Z])/g, "_$1").toLowerCase();
const variants = (text, name) => {
  const body = text.slice(text.indexOf(`pub enum ${name} {`));
  return [...body.slice(0, body.indexOf("\n}")).matchAll(/^ {4}([A-Z]\w*)/gm)].map(([, variant]) => snake(variant));
};

const HELD = { kind: "idle", display: false };
const HELD_LID = { kind: "idle_and_lid", display: false };
const view = (state = {}, extra = {}) => ({
  settings: { ...api.KEEP_AWAKE_DEFAULTS, ...extra.settings },
  state: { want: { kind: "nothing" }, reason: "idle", ends_at_ms: null, ended: null, ...state },
  lid_support: extra.lid_support ?? { kind: "helper_missing" },
  platform: extra.platform ?? "mac_os",
  work: extra.work ?? 0,
});
const working = (count, extra = {}) => view({ want: HELD, reason: "work" }, { work: count, ...extra });
const timer = (left, extra = {}) => view({ want: HELD, reason: "timer", ends_at_ms: NOW + left }, extra);
const ended = (kind, extra = {}) => view({ ended: { kind, ...(kind === "battery" ? { pct: 9 } : {}), ...(kind === "cap" ? { hours: 24 } : {}) } }, extra);
const capped = (hours, cap) => view({ ended: { kind: "cap", hours } }, { settings: { cap } });
const OFF = view({ reason: "off" }, { settings: { mode: "off" } });
const ok = (value) => ({ kind: "ok", value });

const STATES = [
  ["Awake while 3 agents run", "hold", working(3)],
  ["Awake while 1 agent runs", "hold", working(1)],
  ["Awake, 2h 14m left", "hold", timer(2 * HOUR + 14 * MINUTE)],
  ["Awake, lid OK", "hold", view({ want: HELD_LID, reason: "work" }, { work: 3, lid_support: { kind: "held" } })],
  ["Awake 8m more, work done", "hold", view({ want: HELD, reason: "grace", ends_at_ms: NOW + 8 * MINUTE })],
  ["Idle only: on battery", "limit", working(3, { lid_support: { kind: "idle_only_on_battery" } })],
  ["Lid-closed hold needs macOS 13", "limit", working(3, { lid_support: { kind: "needs_macos13" } })],
  ["Needs approval", "limit", working(3, { lid_support: { kind: "needs_approval" } })],
  ["Off", "plain", view({ reason: "off" }, { settings: { mode: "off" } })],
  ["Auto, nothing running", "plain", view()],
  ["Ended: battery 9%", "ended", ended("battery", { work: 2 })],
  ["Ended: heat", "ended", ended("heat", { work: 2 })],
  ["Ended: 24 h limit", "ended", ended("cap", { work: 2 })],
  ["Ended: battery unreadable", "ended", ended("battery_unread", { work: 2 })],
  ["Ended: hold lost", "ended", ended("hold_lost", { work: 2 })],
  ["Manual, no timer", "plain", view({}, { settings: { mode: "manual" } })],
  ["Paused until work ends", "plain", view({}, { work: 2 })],
];

test("the window names every engine option and default of Keep awake as the engine does", () => {
  const enums = { mode: "Mode", grace: "Grace", cap: "Cap", lid: "LidRule", battery_floor: "BatteryFloor", heat_end: "HeatEnd", heat_notice: "HeatNotice", lid_display: "LidDisplay", display_awake: "DisplayAwake" };
  const fields = [...ENGINE_SETTINGS.slice(ENGINE_SETTINGS.indexOf("pub struct KeepAwakeSettings {")).split("\n}")[0].matchAll(/pub (\w+): (\w+),/g)].map(([, field, type]) => [field, type]);
  assert.deepEqual(fields, Object.entries(enums), "the settings fields in engine order");
  assert.deepEqual(api.KEEP_AWAKE_SETTINGS.map((setting) => setting.id), Object.keys(enums), "one line per engine setting, in engine order");
  for (const setting of api.KEEP_AWAKE_SETTINGS) {
    assert.deepEqual(setting.options.map((option) => option.value), variants(ENGINE_SETTINGS, enums[setting.id]), `${setting.id} lists the engine's options in order`);
    assert.ok(setting.label.length > 0 && setting.help.length > 0, `${setting.id} has a label and help`);
    assert.equal(new Set(setting.options.map((option) => option.label)).size, setting.options.length, `${setting.id} has one word per option`);
  }
  const defaults = Object.fromEntries([...ENGINE_SETTINGS.slice(ENGINE_SETTINGS.indexOf("pub const DEFAULT: Self = Self {")).split("};")[0].matchAll(/(\w+): \w+::(\w+),/g)].map(([, field, variant]) => [field, snake(variant)]));
  assert.deepEqual(api.KEEP_AWAKE_DEFAULTS, defaults, "the defaults are the engine's");
  assert.deepEqual(variants(ENGINE_STATE, "LidSupport"), ["held", "off", "idle_only_on_battery", "needs_macos13", "needs_approval", "helper_missing", "locked_by_policy", "not_available"]);
  assert.deepEqual(variants(ENGINE_STATE, "EndReason"), ["battery", "battery_unread", "heat", "cap", "manual", "off", "hold_lost"]);
  assert.deepEqual(variants(ENGINE_STATE, "Reason"), ["work", "timer", "grace", "idle", "off"]);
  assert.deepEqual(variants(ENGINE, "KeepAwakeAction"), ["start", "extend", "end", "set"]);
  assert.match(ENGINE, /minutes outside 1 to 1440/);
  assert.deepEqual([api.MINUTES_MIN, api.MINUTES_MAX], [1, 1440]);
});

test("the settings matrix words are the plan's, with the default of each", () => {
  const words = Object.fromEntries(api.KEEP_AWAKE_SETTINGS.map((setting) => [setting.label, setting.options.map((option) => option.label)]));
  assert.deepEqual(words, {
    "Mode": ["Auto", "Manual timer", "Off"],
    "Grace after work ends": ["0", "5 min", "10 min", "30 min"],
    "Longest single hold": ["4 h", "8 h", "24 h", "No limit"],
    "Lid-closed hold": ["On AC power only", "Always", "Off"],
    "End at battery": ["Off", "5 %", "10 %", "20 %"],
    "End at heat": ["Critical", "Serious", "Never"],
    "Heat notice": ["At serious heat", "Off"],
    "Display with the lid closed (macOS)": ["Sleep the display", "Leave on"],
    "Display awake with the lid open": ["Off", "While holding"],
  });
  assert.equal(api.KEEP_AWAKE_SETTINGS.reduce((count, setting) => count + setting.options.length, 0), 27);
});

test("every decision and lid support gives the approved label and its tone", () => {
  for (const [label, tone, state] of STATES) {
    assert.equal(model.keepAwakeLabel(state, NOW), label);
    assert.equal(model.keepAwakeTone(state), tone, `${label} is ${tone}`);
    assert.ok(label.length <= LABEL_MAX, `${label} fits ${LABEL_MAX} characters`);
  }
  assert.equal(new Set(STATES.map(([label]) => label)).size, STATES.length);
});

test("a lid state with no label of its own leaves the label as it is", () => {
  for (const lid of [{ kind: "helper_missing" }, { kind: "locked_by_policy" }, { kind: "not_available", reason: "Lid-closed hold is not set up on this computer" }, { kind: "off" }]) {
    assert.equal(model.keepAwakeLabel(working(3, { lid_support: lid }), NOW), "Awake while 3 agents run", lid.kind);
    assert.equal(model.keepAwakeTone(working(3, { lid_support: lid })), "hold", lid.kind);
    assert.equal(model.keepAwakeLabel(view({}, { lid_support: lid }), NOW), "Auto, nothing running", lid.kind);
  }
});

test("a lid limit shows only while something is held, and wins over the countdown", () => {
  const limit = { lid_support: { kind: "idle_only_on_battery" } };
  assert.equal(model.keepAwakeLabel(timer(HOUR, limit), NOW), "Idle only: on battery");
  assert.equal(model.keepAwakeLabel(view({}, limit), NOW), "Auto, nothing running");
  assert.equal(model.keepAwakeTone(view({}, limit)), "plain");
  assert.equal(model.keepAwakeLabel(timer(HOUR, { lid_support: { kind: "held" } }), NOW), "Awake, 1h left", "the countdown wins over lid OK");
  assert.equal(model.keepAwakeLabel(ended("heat", { work: 1, ...limit }), NOW), "Ended: heat", "an end wins over everything");
});

test("an end the owner made is not an end by itself", () => {
  assert.equal(model.keepAwakeLabel(ended("manual"), NOW), "Auto, nothing running");
  assert.equal(model.keepAwakeTone(ended("manual")), "plain");
  assert.equal(model.keepAwakeLabel(ended("manual", { settings: { mode: "manual" } }), NOW), "Manual, no timer");
  assert.equal(model.keepAwakeLabel(ended("off", { settings: { mode: "off" } }), NOW), "Off");
  assert.equal(model.keepAwakeTone(ended("off", { settings: { mode: "off" } })), "plain");
});

test("the cap's end names the cap the hold ran with, whatever the setting says now", () => {
  assert.match(ENGINE_STATE, /^ {4}Cap \{ hours: u8 \},$/m);
  assert.equal(model.keepAwakeLabel(capped(4, "h4"), NOW), "Ended: 4 h limit");
  assert.equal(model.keepAwakeLabel(capped(8, "h8"), NOW), "Ended: 8 h limit");
  assert.equal(model.keepAwakeLabel(capped(24, "h24"), NOW), "Ended: 24 h limit");
  assert.equal(model.keepAwakeLabel(capped(24, "h4"), NOW), "Ended: 24 h limit", "a later, shorter cap does not rename the end");
  assert.equal(model.keepAwakeLabel(capped(24, "none"), NOW), "Ended: 24 h limit", "No limit set later does not rename the end");
  assert.equal(model.keepAwakeTone(capped(24, "none")), "ended");
  for (const cap of ["h4", "h8", "h24", "none"]) assert.doesNotMatch(model.keepAwakeLabel(capped(8, cap), NOW), /time limit/);
});

test("every refusal of a Keep awake change has its own sentence, the engine's sentence", () => {
  const words = {
    keep_awake_off: "Keep awake is Off. Pick Auto or Manual first.",
    keep_awake_no_timer: "The timer already ended. Start a new one.",
    keep_awake_not_holding: "Nothing is held right now.",
    keep_awake_minutes_invalid: "Enter whole minutes from 1 to 1440.",
  };
  const refused = new Set([...ENGINE.matchAll(/return Some\("(keep_awake_[a-z_]+)"\);/g)].map(([, code]) => code));
  assert.deepEqual([...refused].sort(), Object.keys(words).sort(), "the engine refuses with these codes and no other");
  for (const [code, sentence] of Object.entries(words)) {
    assert.equal(core.plainError(code), sentence, code);
    assert.doesNotMatch(core.plainError(code), /Something went wrong/, code);
    assert.ok(ENGINE.includes(`"${sentence}"`), `the engine has the same words for ${code}`);
  }
});

test("time left rounds up to the minute and never reads zero", () => {
  const left = (ms) => model.keepAwakeLabel(timer(ms), NOW);
  assert.equal(left(1), "Awake, 1m left");
  assert.equal(left(MINUTE), "Awake, 1m left");
  assert.equal(left(MINUTE + 1), "Awake, 2m left");
  assert.equal(left(59 * MINUTE), "Awake, 59m left");
  assert.equal(left(HOUR), "Awake, 1h left");
  assert.equal(left(HOUR + 1), "Awake, 1h 1m left");
  assert.equal(left(24 * HOUR), "Awake, 24h left");
  assert.equal(model.keepAwakeLabel(timer(-5 * MINUTE), NOW), "Awake, 1m left", "a deadline just passed still reads one minute until the engine ends it");
  assert.equal(model.keepAwakeLabel(view({ want: HELD, reason: "grace", ends_at_ms: NOW + 30 * MINUTE }), NOW), "Awake 30m more, work done");
});

test("no label is longer than 32 characters at the extremes", () => {
  const extremes = [
    working(4_294_967_295), working(1000), working(999), timer(24 * HOUR - 1), timer(1440 * MINUTE * 7),
    view({ want: HELD, reason: "grace", ends_at_ms: NOW + 30 * MINUTE }), view({ ended: { kind: "battery", pct: 100 } }),
    ...STATES.map(([, , state]) => state),
  ];
  for (const state of extremes) {
    const label = model.keepAwakeLabel(state, NOW);
    assert.ok(label.length >= 1 && label.length <= LABEL_MAX, `${label} is 1 to ${LABEL_MAX} characters`);
  }
  assert.equal(model.keepAwakeLabel(working(1000), NOW), "Awake while 999+ agents run");
  assert.equal(model.keepAwakeLabel(working(999), NOW), "Awake while 999 agents run");
});

test("Ends by itself is built from the battery, heat and cap settings", () => {
  const line = (picked) => model.endsByItself({ ...api.KEEP_AWAKE_DEFAULTS, ...picked });
  assert.equal(line({}), "At 10% battery, critical heat, or 24 h");
  assert.equal(line({ battery_floor: "pct5", heat_end: "serious", cap: "h4" }), "At 5% battery, serious heat, or 4 h");
  assert.equal(line({ battery_floor: "off" }), "At critical heat or 24 h");
  assert.equal(line({ battery_floor: "pct20", heat_end: "never", cap: "none" }), "At 20% battery");
  assert.equal(line({ battery_floor: "off", heat_end: "never", cap: "h8" }), "At 8 h");
  assert.equal(line({ battery_floor: "off", heat_end: "never", cap: "none" }), "Never");
});

test("the Lid closed line says the rule, and why this computer cannot hold it", () => {
  assert.equal(model.lidClosedLine(view({}, { lid_support: { kind: "held" } })), "Hold on AC power only");
  assert.equal(model.lidClosedLine(view({}, { lid_support: { kind: "held" }, settings: { lid: "always" } })), "Hold always");
  assert.equal(model.lidClosedLine(view({}, { lid_support: { kind: "off" }, settings: { lid: "off" } })), "Sleep as usual");
  assert.equal(model.lidClosedLine(view({}, { lid_support: { kind: "idle_only_on_battery" } })), "Hold on AC power only");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "held" } })), null);
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "idle_only_on_battery" } })), null);
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "needs_macos13" } })), "Needs macOS 13 or newer");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "needs_approval" } })), "Waits for your approval in System Settings");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "helper_missing" } })), "Not set up on this computer yet");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "locked_by_policy" } })), "Locked by your organization");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "not_available", reason: "Lid-closed hold is not set up on this computer" } })), "Lid-closed hold is not set up on this computer");
  assert.equal(model.lidReason(view({}, { lid_support: { kind: "off" }, settings: { lid: "off" } })), null);
});

test("a setting this computer cannot honour carries its reason, and the others none", () => {
  const windows = view({}, { platform: "windows" });
  const linux = view({}, { platform: "linux" });
  const mac = view({}, { lid_support: { kind: "held" } });
  assert.equal(model.unavailable("heat_end", windows), "Not available on Windows");
  assert.equal(model.unavailable("heat_notice", windows), "Not available on Windows");
  assert.ok(source("../../../crates/crazyagent/src/keep_awake/power.rs").includes('Platform::Windows => Some("Not available on Windows")'), "the engine gives the TUI the same words");
  assert.equal(model.unavailable("lid_display", windows), "Only on macOS");
  assert.equal(model.unavailable("lid_display", linux), "Only on macOS");
  assert.equal(model.unavailable("lid", view()), "Not set up on this computer yet");
  for (const setting of api.KEEP_AWAKE_SETTINGS) assert.equal(model.unavailable(setting.id, mac), null, `${setting.id} works on a Mac with the lid hold`);
  for (const id of ["mode", "grace", "cap", "battery_floor", "display_awake"]) assert.equal(model.unavailable(id, windows), null, id);
});

test("custom minutes take whole numbers from 1 to 1440 and nothing else", () => {
  assert.equal(model.customMinutes("90"), 90);
  assert.equal(model.customMinutes(" 1 "), 1);
  assert.equal(model.customMinutes("1440"), 1440);
  for (const typed of ["", "0", "1441", "-5", "1.5", "1e2", "ten", "0x10", "99999999999999999999"]) assert.equal(model.customMinutes(typed), null, typed);
});

test("Extend works only while a timer runs, End only while something is held", () => {
  assert.equal(model.canExtend(timer(HOUR)), true);
  assert.equal(model.canExtend(working(3)), false);
  assert.equal(model.canExtend(view()), false);
  assert.equal(model.canEnd(timer(HOUR)), true);
  assert.equal(model.canEnd(working(3)), true);
  assert.equal(model.canEnd(view()), false);
  assert.equal(model.canEnd(ended("heat")), false);
});

const controlView = (loaded, extra = {}) => renderToStaticMarkup(createElement(control.KeepAwakeControlView, { view: loaded, open: false, now: NOW, onToggle: forbidden, ...extra }));

test("the control shows the label, its tone and its state for a screen reader", () => {
  for (const [label, tone, state] of STATES) {
    const markup = controlView(ok(state));
    assert.equal(text(markup), label);
    assert.match(markup, new RegExp(`<button type="button" class="keep-awake-control ${tone}"`), `${label} draws ${tone}`);
    assert.ok(markup.includes(`data-tone="${tone}"`));
  }
  const held = controlView(ok(working(3)), { open: true });
  assert.match(held, /aria-label="Keep awake: awake while 3 agents run"/);
  assert.match(held, /aria-expanded="true"/);
  assert.match(held, /aria-haspopup="dialog"/);
  assert.match(controlView(ok(working(3))), /aria-expanded="false"/);
  assert.match(controlView(ok(view({}, { settings: { mode: "off" } }))), /aria-label="Keep awake: off"/);
});

test("before the first read and without an engine the control reads Keep awake, plain", () => {
  for (const loaded of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", message: "The engine is not connected." }]) {
    const markup = controlView(loaded);
    assert.equal(text(markup), "Keep awake", loaded.kind);
    assert.match(markup, /class="keep-awake-control plain"/);
    assert.match(markup, /aria-label="Keep awake"/);
  }
  assert.match(controlView({ kind: "loading" }), /aria-busy="true"/);
});

const panelView = (loaded, extra = {}) => renderToStaticMarkup(createElement(panel.KeepAwakePanelView, {
  view: loaded, now: NOW, custom: false, typed: "", busy: false, problem: "",
  onStart: forbidden, onCustom: forbidden, onType: forbidden, onExtend: forbidden, onEnd: forbidden, onMode: forbidden, onRetry: forbidden, ...extra,
}));
const button = (markup, label) => [...markup.matchAll(/<button[^>]*>.*?<\/button>/g)].map(([tag]) => tag).find((tag) => text(tag) === label);
const disabled = (markup, label) => {
  const tag = button(markup, label);
  assert.ok(tag, `the panel has ${label}`);
  return / disabled=""/.test(tag);
};
const picked = (markup, name) => [...markup.matchAll(/<input[^>]*type="radio"[^>]*>/g)].map(([input]) => input)
  .filter((input) => input.includes(`name="${name}"`) && / checked=""/.test(input)).map((input) => input.match(/ value="([a-z0-9_]+)"/)[1]);
const offered = (markup, name) => [...markup.matchAll(/<input[^>]*type="radio"[^>]*>/g)].map(([input]) => input)
  .filter((input) => input.includes(`name="${name}"`)).map((input) => input.match(/ value="([a-z0-9_]+)"/)[1]);

test("the panel has the five actions, the mode, the two lines and the link, as approved", () => {
  const markup = panelView(ok(working(3)));
  assert.match(markup, /<section[^>]*role="dialog"[^>]*aria-label="Keep awake"/);
  assert.ok(text(markup).startsWith("Keep awake Awake while 3 agents run. Sleep comes back 10 minutes after they finish."));
  for (const label of ["Start 1 h", "Start 4 h", "Custom...", "Extend 1 h", "End now"]) assert.ok(button(markup, label), label);
  assert.equal(disabled(markup, "Extend 1 h"), true);
  assert.ok(text(markup).includes("Extend works while a timer runs. Start one first."));
  assert.equal(disabled(markup, "End now"), false);
  assert.deepEqual(offered(markup, "keep-awake-panel-mode"), ["auto", "manual", "off"]);
  assert.deepEqual(picked(markup, "keep-awake-panel-mode"), ["auto"]);
  assert.ok(text(markup).includes("Mode Auto Manual Off"));
  assert.ok(text(markup).includes("Lid closed Hold on AC power only"));
  assert.ok(text(markup).includes("Ends by itself At 10% battery, critical heat, or 24 h"));
  assert.match(markup, /<a href="#\/settings\/keep-awake">All Keep awake settings<\/a>/);
});

test("the panel's first line says what holds and when sleep comes back", () => {
  const line = (state) => model.panelLine(state, NOW);
  assert.equal(line(working(1)), "Awake while 1 agent runs. Sleep comes back 10 minutes after it finishes.");
  assert.equal(line(working(3, { settings: { grace: "min0" } })), "Awake while 3 agents run. Sleep comes back when they finish.");
  assert.equal(line(timer(HOUR)), "Awake, 1h left. Sleep comes back when the timer ends.");
  assert.equal(line(view({ want: HELD, reason: "grace", ends_at_ms: NOW + 8 * MINUTE })), "Awake 8m more, work done. Sleep comes back after that.");
  assert.equal(line(view()), "Auto, nothing running. Holds while agents work and 10 minutes after.");
  assert.equal(line(view({}, { settings: { grace: "min0" } })), "Auto, nothing running. Holds while agents work.");
  assert.equal(line(view({}, { settings: { mode: "manual" } })), "Manual, no timer. Holds only while a timer you start runs.");
  assert.equal(line(view({ reason: "off" }, { settings: { mode: "off" } })), "Off. The computer sleeps as usual.");
  assert.equal(line(view({}, { work: 2 })), "Paused until work ends. You ended the hold; it starts again with new work.");
  assert.equal(line(ended("heat", { work: 2 })), "Ended: heat. The computer sleeps as usual.");
  assert.equal(line(working(3, { lid_support: { kind: "idle_only_on_battery" } })), "Idle only: on battery. A closed lid sleeps the computer until it is on AC power.");
  assert.equal(line(working(3, { lid_support: { kind: "needs_macos13" } })), "Lid-closed hold needs macOS 13. A closed lid sleeps the computer.");
  assert.equal(line(working(3, { lid_support: { kind: "needs_approval" } })), "Needs approval. Allow the lid helper in System Settings to hold with the lid closed.");
  assert.equal(line(view({ want: HELD_LID, reason: "work" }, { work: 3, lid_support: { kind: "held" } })), "Awake, lid OK. 3 agents run; closing the lid keeps the computer awake.");
});

test("Extend is offered while a timer runs, End only while something is held, and nothing while a change is sent", () => {
  const running = panelView(ok(timer(HOUR)));
  assert.equal(disabled(running, "Extend 1 h"), false);
  assert.equal(text(running).includes("Extend works while a timer runs."), false);
  assert.equal(disabled(running, "End now"), false);
  const idle = panelView(ok(view()));
  assert.equal(disabled(idle, "Extend 1 h"), true);
  assert.equal(disabled(idle, "End now"), true);
  assert.equal(disabled(idle, "Start 1 h"), false);
  const busy = panelView(ok(timer(HOUR)), { busy: true });
  for (const label of ["Start 1 h", "Start 4 h", "Custom...", "Extend 1 h", "End now"]) assert.equal(disabled(busy, label), true, label);
  assert.match(busy, /<fieldset class="notice-choices" data-setting="mode" disabled="">/);
});

test("in mode Off the three Starts are off and the panel says how to start one", () => {
  const off = panelView(ok(OFF));
  for (const label of ["Start 1 h", "Start 4 h", "Custom..."]) assert.equal(disabled(off, label), true, label);
  assert.equal(disabled(off, "Extend 1 h"), true);
  assert.equal(disabled(off, "End now"), true);
  assert.ok(text(off).includes("Keep awake is Off. Pick Auto or Manual to start a timer."));
  assert.equal(text(off).includes("Extend works while a timer runs."), false, "one hint at a time");
  assert.equal(disabled(panelView(ok(OFF), { custom: true, typed: "90" }), "Start"), true, "an open custom field cannot start one either");
  assert.match(off, /<fieldset class="notice-choices" data-setting="mode"(?! disabled)/, "the mode stays pickable");
  for (const mode of ["auto", "manual"]) {
    const on = panelView(ok(view({}, { settings: { mode } })));
    for (const label of ["Start 1 h", "Start 4 h", "Custom..."]) assert.equal(disabled(on, label), false, `${mode} ${label}`);
    assert.equal(text(on).includes("Keep awake is Off."), false, mode);
  }
});

test("Custom opens a minutes field that starts only a whole number from 1 to 1440", () => {
  const closed = panelView(ok(view()));
  assert.equal(closed.includes('id="keep-awake-minutes"'), false);
  const open = panelView(ok(view()), { custom: true, typed: "90" });
  assert.match(open, /<input[^>]*id="keep-awake-minutes"[^>]*type="number"|<input[^>]*type="number"[^>]*id="keep-awake-minutes"/);
  assert.match(open, /<label[^>]*for="keep-awake-minutes"[^>]*>Minutes<\/label>/);
  assert.equal(disabled(open, "Start"), false);
  const bad = panelView(ok(view()), { custom: true, typed: "1441" });
  assert.equal(disabled(bad, "Start"), true);
  assert.match(bad, /aria-invalid="true"/);
  assert.match(bad, /role="alert"[^>]*>Enter whole minutes from 1 to 1440\./);
  const empty = panelView(ok(view()), { custom: true, typed: "" });
  assert.equal(disabled(empty, "Start"), true);
  assert.equal(empty.includes('role="alert"'), false, "an empty field is not an error yet");
});

test("the panel says what went wrong, and offers Retry when Keep awake cannot be read", () => {
  assert.match(panelView(ok(view()), { problem: "The engine refused the change." }), /role="alert"[^>]*>.*The engine refused the change\./);
  const failed = panelView({ kind: "error", message: "The engine is not connected." });
  assert.ok(text(failed).includes("Couldn't load Keep awake. The engine is not connected."));
  assert.ok(button(failed, "Retry") || button(failed, "Try again"));
  assert.equal(button(failed, "Start 1 h"), undefined);
  assert.equal(button(panelView({ kind: "loading" }), "Start 1 h"), undefined);
});

test("the top bar holds the page name, then Keep awake, then the bell", () => {
  const markup = renderToStaticMarkup(createElement(bar.TopBar, {
    title: "Tidepool Docs",
    keepAwake: createElement("button", { className: "keep-awake-control plain" }, "Auto, nothing running"),
    bell: createElement("button", { className: "notice-bell" }, "bell"),
  }));
  assert.match(markup, /^<header class="top-bar"/);
  assert.match(markup, /<span class="top-bar-title">Tidepool Docs<\/span>/);
  assert.equal(/<h[1-6]/.test(markup), false, "the page keeps its one heading");
  assert.ok(markup.indexOf("top-bar-title") < markup.indexOf("keep-awake-control"));
  assert.ok(markup.indexOf("keep-awake-control") < markup.indexOf("notice-bell"), "Keep awake sits left of the bell");
});

test("the shell puts the top bar above the scrolling page, outside it, and has no corner", () => {
  const markup = renderToStaticMarkup(createElement(shell.AppShell, {
    sidebar: createElement("aside", { className: "sidebar" }), top: createElement("header", { className: "top-bar" }, "bar"),
  }, createElement("p", null, "page")));
  assert.match(markup, /<div class="main-column"><header class="top-bar">bar<\/header><main id="main" class="main" tabindex="-1"><div class="page"><p>page<\/p><\/div><\/main><\/div>/);
  assert.equal(/\bcorner\b/.test(SHELL), false, "the shell has no corner slot");
  assert.equal(/corner=/.test(APP), false, "the app mounts no corner");
  assert.match(APP, /top=\{/);
  assert.equal(NOTICE_CSS.includes("--notice-room"), false, "no page keeps room for a floating bell");
  assert.equal(/\.notice-corner\s*\{[^}]*position:\s*sticky/.test(NOTICE_CSS), false, "the bell no longer floats over the page");
});

test("the page name in the bar follows the route", () => {
  const projects = [{ id: "p-1", name: "Tidepool Docs" }];
  const name = (hash) => bar.pageName(parseRoute(hash), projects);
  assert.equal(name("#/home"), "Home");
  assert.equal(name("#/welcome"), "Welcome");
  assert.equal(name("#/metrics"), "Metrics");
  assert.equal(name("#/settings/keep-awake"), "Settings");
  assert.equal(name("#/projects/new"), "New project");
  assert.equal(name("#/projects/p-1/ide"), "Tidepool Docs");
  assert.equal(name("#/projects/p-9/overview"), "Project");
});

test("Settings lists Keep awake in the App group, right after Notifications", () => {
  assert.ok(SETTINGS_TABS.includes("keep-awake"));
  const app = SETTINGS_GROUPS.find((group) => group.title === "App").items;
  const at = app.findIndex((item) => item.id === "notifications");
  assert.deepEqual(app[at + 1], { id: "keep-awake", label: "Keep awake" });
  assert.deepEqual(parseRoute("#/settings/keep-awake"), { name: "settings", tab: "keep-awake" });
});

const settingsView = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.KeepAwakeSettingsView, {
  view: loaded, picks: {}, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const MARK = '<span class="set-changed">changed</span>';
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);
const lineOf = (markup, id) => {
  const start = markup.indexOf(`<fieldset class="notice-choices" data-setting="${id}"`);
  assert.ok(start > 0, `the page has ${id}`);
  const from = markup.lastIndexOf('<div class="set-row', start);
  const next = markup.indexOf('<div class="set-row', start);
  return markup.slice(from, next === -1 ? undefined : next);
};

test("Settings, Keep awake lists every setting with every option and the stored one picked", () => {
  const markup = settingsView(ok(view({}, { lid_support: { kind: "held" } })));
  assert.match(markup, /<h2>Keep awake<\/h2>/);
  assert.match(markup, /<form[^>]*aria-label="Keep awake"/);
  for (const setting of api.KEEP_AWAKE_SETTINGS) {
    const name = `keep-awake-${setting.id}`;
    assert.deepEqual(offered(markup, name), setting.options.map((option) => option.value), setting.id);
    assert.deepEqual(picked(markup, name), [api.KEEP_AWAKE_DEFAULTS[setting.id]], `${setting.id} shows the default`);
    const line = lineOf(markup, setting.id);
    assert.ok(text(line).includes(setting.label), `${setting.id} has its label`);
    assert.ok(text(line).includes(setting.help), `${setting.id} has its help`);
    for (const option of setting.options) assert.ok(text(line).includes(option.label), `${setting.id} offers ${option.label}`);
    assert.equal(line.includes(MARK), false, `${setting.id} at its default has no changed mark`);
  }
  assert.equal(saveDisabled(markup), true, "nothing to save yet");
});

test("a stored or picked option away from the default carries the changed mark, and only a pick enables Save", () => {
  const stored = settingsView(ok(view({}, { settings: { mode: "manual", cap: "none" }, lid_support: { kind: "held" } })));
  assert.deepEqual(picked(stored, "keep-awake-mode"), ["manual"]);
  assert.ok(lineOf(stored, "mode").includes(MARK));
  assert.ok(lineOf(stored, "cap").includes(MARK));
  assert.equal(lineOf(stored, "grace").includes(MARK), false);
  assert.equal(saveDisabled(stored), true);
  const pickedOne = settingsView(ok(view({}, { lid_support: { kind: "held" } })), { picks: { grace: "min30" } });
  assert.deepEqual(picked(pickedOne, "keep-awake-grace"), ["min30"]);
  assert.ok(lineOf(pickedOne, "grace").includes(MARK));
  assert.equal(saveDisabled(pickedOne), false);
  const same = settingsView(ok(view({}, { lid_support: { kind: "held" } })), { picks: { grace: "min10" } });
  assert.equal(saveDisabled(same), true, "picking what is stored changes nothing");
  assert.match(settingsView(ok(view()), { picks: { grace: "min30" }, saving: true }), /<fieldset class="notice-choices" data-setting="grace" disabled="">/);
});

test("a save sends every setting with the picks applied, and refuses a name the engine does not have", () => {
  const stored = { ...api.KEEP_AWAKE_DEFAULTS, mode: "manual" };
  assert.deepEqual(settings.savedKeepAwake(stored, {}), stored);
  assert.deepEqual(settings.savedKeepAwake(stored, { cap: "none", heat_end: "never" }), { ...stored, cap: "none", heat_end: "never" });
  assert.deepEqual(Object.keys(settings.savedKeepAwake(stored, { cap: "h4" })), Object.keys(api.KEEP_AWAKE_DEFAULTS));
  assert.throws(() => settings.savedKeepAwake(stored, { cap: "h12" }), /cap cannot be h12/);
  assert.throws(() => settings.savedKeepAwake(stored, { volume: "loud" }), /no setting volume/);
});

test("a Settings save keeps what changed elsewhere while the page was open", () => {
  const changedElsewhere = { ...api.KEEP_AWAKE_DEFAULTS, mode: "off", lid: "always" };
  const sent = settings.savedKeepAwake(changedElsewhere, { grace: "min30" });
  assert.deepEqual(sent, { ...changedElsewhere, grace: "min30" }, "only the picked line differs from the engine's settings");
  assert.match(SETTINGS_PAGE, /window\.addEventListener\(KEEP_AWAKE_CHANGED, /, "the page reads again after a change made in the panel");
  assert.match(SETTINGS_PAGE, /window\.removeEventListener\(KEEP_AWAKE_CHANGED, /);
  assert.doesNotMatch(SETTINGS_PAGE, /\[view\]\)/, "a read never drops the picks");
  assert.match(SETTINGS_PAGE, /const fresh = await loadKeepAwake\(\);/, "a save reads the engine's settings first");
  assert.match(SETTINGS_PAGE, /savedKeepAwake\(fresh\.value\.settings, picks\)/, "and sends the picks over those");
});

test("Settings shows the reason on a setting this computer cannot honour and keeps it editable", () => {
  const windows = settingsView(ok(view({}, { platform: "windows", lid_support: { kind: "not_available", reason: "Lid-closed hold is not set up on this computer" } })));
  assert.ok(text(lineOf(windows, "heat_end")).includes("Not available on Windows"));
  assert.ok(text(lineOf(windows, "heat_notice")).includes("Not available on Windows"));
  assert.ok(text(lineOf(windows, "lid_display")).includes("Only on macOS"));
  assert.ok(text(lineOf(windows, "lid")).includes("Lid-closed hold is not set up on this computer"));
  assert.equal(text(lineOf(windows, "mode")).includes("Not available"), false);
  assert.equal(/<fieldset[^>]*disabled=""/.test(windows), false, "the choice stays editable and is stored");
  const mac = settingsView(ok(view({}, { lid_support: { kind: "held" } })));
  for (const words of ["Not available on Windows", "Only on macOS", "Not set up on this computer yet"]) assert.equal(text(mac).includes(words), false, words);
});

test("Settings, Keep awake keeps its title while it loads, is not ready or fails", () => {
  for (const loaded of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", message: "The engine is not connected." }]) {
    assert.match(settingsView(loaded), /<h2>Keep awake<\/h2>/, loaded.kind);
  }
  assert.ok(text(settingsView({ kind: "error", message: "The engine is not connected." })).includes("Couldn't load the Keep awake settings. The engine is not connected."));
  assert.match(settingsView(ok(view()), { problem: "The engine refused the change." }), /role="alert"[^>]*>.*The engine refused the change\./);
});

test("the window reads and changes Keep awake through two commands the app registers", () => {
  const registered = LIB.slice(LIB.indexOf("native_commands! {"));
  for (const command of ["desktop_keep_awake_read", "desktop_keep_awake_act"]) {
    assert.match(LIB, new RegExp(`async fn ${command}\\(`), command);
    assert.match(registered, new RegExp(`\\b${command},`), `${command} is registered`);
  }
  assert.equal(api.KEEP_AWAKE_POLL_MS, 5000);
  const sent = (action) => api.keepAwakeArgs(action, 7);
  assert.deepEqual(sent({ start: { minutes: 60 } }), { action: { start: { minutes: 60 } }, generation: 7 });
  assert.deepEqual(sent("end"), { action: "end", generation: 7 });
  assert.deepEqual(sent({ set: { settings: api.KEEP_AWAKE_DEFAULTS } }).action.set.settings, api.KEEP_AWAKE_DEFAULTS);
  assert.throws(() => api.keepAwakeArgs({ start: { minutes: 0 } }, 7), /minutes/);
  assert.throws(() => api.keepAwakeArgs({ extend: { minutes: 1441 } }, 7), /minutes/);
  assert.throws(() => api.keepAwakeArgs("end", 0), /generation/);
  assert.match(API, /call<KeepAwakeView>\("desktop_keep_awake_read"\)/);
  assert.match(API, /call<IntentReceipt>\("desktop_keep_awake_act", keepAwakeArgs\(/);
});
