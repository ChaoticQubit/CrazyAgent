import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const settings = await import("../src/screens/settings/finished-agents.tsx");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");
const { SETTINGS_TABS } = await import("../src/ui/router.ts");

const ok = (mode, days = 1) => ({ kind: "ok", value: { mode, days } });
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const view = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.FinishedAgentsSettingsView, {
  settings: loaded, picked: null, typed: null, saving: false, problem: "", onPick: forbidden, onType: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/\s+/gu, " ");
const checked = (markup) => [...markup.matchAll(/<input type="radio" name="finished-agents"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);
const daysDisabled = (markup) => /<input[^>]*id="finished-agents-days"[^>]*disabled=""/.test(markup) || /<input[^>]*disabled=""[^>]*id="finished-agents-days"/.test(markup);

test("Settings lists Finished agents in the Agents group with its three options and what stays", () => {
  assert.ok(SETTINGS_TABS.includes("finished"));
  const agents = SETTINGS_GROUPS.find((group) => group.title === "Agents");
  assert.ok(agents.items.some((item) => item.id === "finished" && item.label === "Finished agents"));
  const shown = text(view(ok("after_days")));
  assert.match(shown, /Finished agents/);
  assert.match(shown, /Remove finished agents' worktrees/);
  assert.match(shown, /1 day after the agent finishes/);
  assert.match(shown, /Once its work lands/);
  assert.match(shown, /Only when I click Clean up/);
  assert.match(shown, /Only the worktree goes: the code checkout and its build files\. The agent's conversations, history, logs and messages stay\. An agent whose work has not landed stays, and Clean up shows why\./);
  assert.deepEqual(settings.CLEANUP_MODES, ["after_days", "once_landed", "manual"]);
  assert.deepEqual(settings.CLEANUP_DEFAULT, { mode: "after_days", days: 1 });
});

test("After N days: the stored days name the option, the days field is open and Save waits for a change", () => {
  const markup = view(ok("after_days", 7));
  assert.deepEqual(checked(markup), ["after_days"]);
  assert.match(text(markup), /7 days after the agent finishes/);
  assert.ok(!daysDisabled(markup));
  assert.ok(saveDisabled(markup));
  const typed = view(ok("after_days", 7), { typed: "30" });
  assert.match(text(typed), /30 days after the agent finishes/);
  assert.ok(!saveDisabled(typed));
});

test("Once its work lands: picking it enables Save and keeps the days for later", () => {
  const markup = view(ok("after_days", 3), { picked: "once_landed" });
  assert.deepEqual(checked(markup), ["once_landed"]);
  assert.ok(!saveDisabled(markup));
  assert.ok(daysDisabled(markup), "the days only apply to the first option");
  assert.match(text(markup), /3 days after the agent finishes/);
});

test("Only when I click Clean up: picking it enables Save; a stored manual is checked", () => {
  assert.deepEqual(checked(view(ok("manual"))), ["manual"]);
  const markup = view(ok("once_landed"), { picked: "manual" });
  assert.deepEqual(checked(markup), ["manual"]);
  assert.ok(!saveDisabled(markup));
});

test("days outside 1 to 365 or not whole are refused before anything is sent", () => {
  for (const typed of ["0", "366", "1.5", "-2", "", "ten"]) {
    assert.equal(settings.cleanupDays(typed), null, typed);
    const markup = view(ok("after_days", 2), { typed });
    assert.ok(saveDisabled(markup), typed);
    assert.match(text(markup), /Enter a whole number of days from 1 to 365\./, typed);
  }
  assert.equal(settings.cleanupDays("365"), 365);
  assert.equal(settings.cleanupDays(" 1 "), 1);
});

test("loading, not ready, a failed read and a refused save each say so", () => {
  assert.doesNotMatch(view({ kind: "loading" }), /finished-agents-days/);
  assert.match(view({ kind: "not_ready" }), /finished agents setting/i);
  assert.match(text(view({ kind: "error", code: "owner_read_failed", message: "The engine is busy." })), /Couldn't load the finished agents setting\. The engine is busy\./);
  const refused = view(ok("after_days"), { picked: "manual", problem: "Another change is still running." });
  assert.match(refused, /role="alert"[^>]*>[\s\S]*Another change is still running/);
  const saving = view(ok("after_days"), { picked: "manual", saving: true });
  assert.match(saving, /<fieldset[^>]*disabled=""/);
  assert.match(saving, /Saving\.\.\./);
});

test("the options and the days bounds are the engine's", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/cleanup_settings.rs", import.meta.url), "utf8");
  const body = engine.match(/#\[serde\(rename_all = "snake_case"\)\]\s*pub enum CleanupMode \{([^}]*)\}/);
  assert.ok(body, "cleanup_settings.rs declares CleanupMode in snake case");
  const names = [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)]
    .map(([, name]) => name.replace(/(?<=[a-z])([A-Z])/g, "_$1").toLowerCase());
  assert.deepEqual([...names].sort(), [...settings.CLEANUP_MODES].sort());
  assert.match(engine, new RegExp(`pub const DAYS_MIN: u16 = ${settings.CLEANUP_DAYS_MIN};`));
  assert.match(engine, new RegExp(`pub const DAYS_MAX: u16 = ${settings.CLEANUP_DAYS_MAX};`));
});
