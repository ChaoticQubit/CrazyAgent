import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const settings = await import("../src/screens/settings/quitting.tsx");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");
const { SETTINGS_TABS } = await import("../src/ui/router.ts");

const ok = (on_quit, left_running = "after_hours", hours = 24) => ({ kind: "ok", value: { on_quit, left_running, hours } });
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const view = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.QuittingSettingsView, {
  settings: loaded, onQuit: null, left: null, typed: null, saving: false, problem: "",
  onPickQuit: forbidden, onPickLeft: forbidden, onType: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/\s+/gu, " ");
const checked = (markup, name) => [...markup.matchAll(new RegExp(`<input type="radio" name="${name}"[^>]*>`, "g"))]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);
const hoursDisabled = (markup) => /<input[^>]*id="left-running-hours"[^>]*disabled=""/.test(markup) || /<input[^>]*disabled=""[^>]*id="left-running-hours"/.test(markup);

test("Settings lists Quitting in the Agents group after Finished agents with both choices and what the limit covers", () => {
  assert.ok(SETTINGS_TABS.includes("quit"));
  const agents = SETTINGS_GROUPS.find((group) => group.title === "Agents");
  const ids = agents.items.map((item) => item.id);
  assert.equal(ids.indexOf("quit"), ids.indexOf("finished") + 1);
  assert.ok(agents.items.some((item) => item.id === "quit" && item.label === "Quitting"));
  const shown = text(view(ok("keep_running")));
  assert.match(shown, /What happens to team agents when CrazyAgent quits\./);
  assert.match(shown, /When CrazyAgent quits/);
  assert.match(shown, /Keep team agents running They go on with their work, and CrazyAgent takes them over when it opens again\./);
  assert.match(shown, /Stop team agents Each agent stops as Stop would, and its task goes back to the queue\./);
  assert.match(shown, /End agents left running/);
  assert.match(shown, /24 hours after CrazyAgent quits/);
  assert.match(shown, /No limit, until CrazyAgent opens again/);
  assert.match(shown, /The limit also covers agents left behind when CrazyAgent did not quit cleanly\./);
  assert.deepEqual(settings.ON_QUIT_CHOICES.map((option) => option.choice), ["keep_running", "stop"]);
  assert.deepEqual(settings.LEFT_RUNNING_CHOICES, ["after_hours", "never"]);
  assert.deepEqual(settings.QUIT_DEFAULT, { on_quit: "keep_running", left_running: "after_hours", hours: 24 });
});

test("Keep team agents running: the stored choice is checked, the hours are open and Save waits for a change", () => {
  const markup = view(ok("keep_running", "after_hours", 5));
  assert.deepEqual(checked(markup, "on-quit"), ["keep_running"]);
  assert.deepEqual(checked(markup, "left-running"), ["after_hours"]);
  assert.match(text(markup), /5 hours after CrazyAgent quits/);
  assert.ok(!hoursDisabled(markup));
  assert.ok(saveDisabled(markup));
  const typed = view(ok("keep_running", "after_hours", 5), { typed: "1" });
  assert.match(text(typed), /1 hour after CrazyAgent quits/);
  assert.ok(!saveDisabled(typed));
});

test("Stop team agents: picking it enables Save; a stored stop is checked", () => {
  assert.deepEqual(checked(view(ok("stop")), "on-quit"), ["stop"]);
  const markup = view(ok("keep_running"), { onQuit: "stop" });
  assert.deepEqual(checked(markup, "on-quit"), ["stop"]);
  assert.ok(!saveDisabled(markup));
});

test("No limit: picking it enables Save, closes the hours and keeps them for later", () => {
  const markup = view(ok("keep_running", "after_hours", 48), { left: "never" });
  assert.deepEqual(checked(markup, "left-running"), ["never"]);
  assert.ok(!saveDisabled(markup));
  assert.ok(hoursDisabled(markup), "the hours only apply to the first limit");
  assert.match(text(markup), /48 hours after CrazyAgent quits/);
  assert.deepEqual(checked(view(ok("stop", "never", 3)), "left-running"), ["never"]);
});

test("hours outside 1 to 168 or not whole are refused before anything is sent", () => {
  for (const typed of ["0", "169", "1.5", "-2", "", "ten", "1000"]) {
    assert.equal(settings.quitHours(typed), null, typed);
    const markup = view(ok("keep_running", "after_hours", 2), { typed });
    assert.ok(saveDisabled(markup), typed);
    assert.match(text(markup), /Enter a whole number of hours from 1 to 168\./, typed);
  }
  assert.equal(settings.quitHours("168"), 168);
  assert.equal(settings.quitHours(" 1 "), 1);
});

test("loading, not ready, a failed read and a refused save each say so", () => {
  assert.doesNotMatch(view({ kind: "loading" }), /left-running-hours/);
  assert.match(view({ kind: "not_ready" }), /quitting setting/i);
  assert.match(text(view({ kind: "error", code: "owner_read_failed", message: "The engine is busy." })), /Couldn't load the quitting setting\. The engine is busy\./);
  const refused = view(ok("keep_running"), { onQuit: "stop", problem: "Another change is still running." });
  assert.match(refused, /role="alert"[^>]*>[\s\S]*Another change is still running/);
  const saving = view(ok("keep_running"), { onQuit: "stop", saving: true });
  assert.match(saving, /<fieldset[^>]*disabled=""/);
  assert.match(saving, /Saving\.\.\./);
});

test("the choices and the hours bounds are the engine's", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/quit_settings.rs", import.meta.url), "utf8");
  const names = (name) => {
    const body = engine.match(new RegExp(`#\\[serde\\(rename_all = "snake_case"\\)\\]\\s*pub enum ${name} \\{([^}]*)\\}`));
    assert.ok(body, `quit_settings.rs declares ${name} in snake case`);
    return [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)].map(([, found]) => found.replace(/(?<=[a-z])([A-Z])/g, "_$1").toLowerCase());
  };
  assert.deepEqual(names("OnQuit").sort(), settings.ON_QUIT_CHOICES.map((option) => option.choice).sort());
  assert.deepEqual(names("LeftRunning").sort(), [...settings.LEFT_RUNNING_CHOICES].sort());
  assert.match(engine, new RegExp(`pub const HOURS_MIN: u16 = ${settings.QUIT_HOURS_MIN};`));
  assert.match(engine, new RegExp(`pub const HOURS_MAX: u16 = ${settings.QUIT_HOURS_MAX};`));
  assert.match(engine, new RegExp(`pub const HOURS_DEFAULT: u16 = ${settings.QUIT_DEFAULT.hours};`));
});
