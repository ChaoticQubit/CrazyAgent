import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const settings = await import("../src/screens/settings/master.tsx");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");
const { SETTINGS_TABS } = await import("../src/ui/router.ts");

const LIMITS = ["no_limit", "steps50", "steps100", "steps200"];
const ok = (limit) => ({ kind: "ok", value: { limit } });
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const view = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.MasterSettingsView, {
  settings: loaded, picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/\s+/gu, " ");
const checked = (markup) => [...markup.matchAll(/<input type="radio" name="step-limit"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z0-9_]+)"/)[1]);
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/.test(markup);
const marked = (markup) => /class="set-changed"/.test(markup);

test("Settings lists Master in the Agents group after Quitting with the four step limits and what a stopped turn says", () => {
  assert.ok(SETTINGS_TABS.includes("master"));
  const agents = SETTINGS_GROUPS.find((group) => group.title === "Agents");
  const ids = agents.items.map((item) => item.id);
  assert.equal(ids.indexOf("master"), ids.indexOf("quit") + 1);
  assert.ok(agents.items.some((item) => item.id === "master" && item.label === "Master"));
  const shown = text(view(ok("no_limit")));
  assert.match(shown, /How far a project's master goes in one turn\./);
  assert.match(shown, /Master step limit/);
  assert.match(shown, /Most steps in one turn/);
  assert.match(shown, /No limit A turn runs until the master replies or its time is up\./);
  for (const steps of [50, 100, 200]) assert.ok(shown.includes(`${steps} steps A turn stops after ${steps} steps.`), `${steps} steps is listed`);
  assert.match(shown, /Stopped after N steps; reply with 'continue' to go on\./);
  assert.match(shown, /The limit covers a Claude master with tools on; a Codex master counts no steps\./);
  assert.doesNotMatch(shown, /cut off/);
  assert.deepEqual(settings.STEP_LIMIT_CHOICES.map((option) => option.choice), LIMITS);
  assert.deepEqual(settings.STEP_LIMIT_DEFAULT, { limit: "no_limit" });
});

test("No limit is the default: it is checked before any save, unmarked, and Save waits for a change", () => {
  const markup = view(ok("no_limit"));
  assert.deepEqual(checked(markup), ["no_limit"]);
  assert.ok(saveDisabled(markup));
  assert.ok(!marked(markup), "the default carries no changed mark");
});

for (const limit of LIMITS) {
  test(`${limit}: a stored one is checked with Save resting, and picking it over another enables Save`, () => {
    const stored = view(ok(limit));
    assert.deepEqual(checked(stored), [limit]);
    assert.ok(saveDisabled(stored));
    assert.equal(marked(stored), limit !== "no_limit", "only a limit away from the default is marked changed");
    const other = LIMITS.find((choice) => choice !== limit);
    const picked = view(ok(other), { picked: limit });
    assert.deepEqual(checked(picked), [limit]);
    assert.ok(!saveDisabled(picked));
    assert.ok(saveDisabled(view(ok(limit), { picked: limit })), "picking the stored limit is no change");
  });
}

test("the Master tab names what failed: a read that fails offers a retry, and a failed save shows its problem", () => {
  const failed = text(view({ kind: "error", message: "engine_failed" }));
  assert.match(failed, /Couldn't load the master setting\. engine_failed/);
  assert.match(text(view({ kind: "not_ready" })), /Master/);
  assert.match(text(view({ kind: "loading" })), /How far a project's master goes in one turn\./);
  const refused = view(ok("no_limit"), { picked: "steps50", problem: "The engine refused the save." });
  assert.match(text(refused), /The engine refused the save\./);
  assert.match(refused, /role="alert"/);
});
