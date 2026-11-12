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
  assert.match(shown, /The limit covers Claude and Codex masters with tools on\. A master with tools off, and an rcli master, which has no tools, answers each turn in one step, so the limit never stops it\./);
  assert.doesNotMatch(shown, /counts no steps/);
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

const TIMES = ["no_limit", "minutes10", "minutes30", "minutes60"];
const timeView = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.MasterTimeView, {
  settings: loaded, picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const timeChecked = (markup) => [...markup.matchAll(/<input type="radio" name="time-limit"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z0-9_]+)"/)[1]);

test("Settings, Master lists the master time limit with its four options, what a stopped turn says and which masters it covers", () => {
  const markup = timeView(ok("no_limit"));
  const shown = text(markup);
  assert.match(shown, /Master time limit/);
  assert.match(shown, /Longest one turn may run/);
  assert.match(shown, /No limit A turn runs until the master replies or you stop it\./);
  for (const minutes of [10, 30, 60]) assert.ok(shown.includes(`${minutes} min A turn stops after ${minutes} minutes.`), `${minutes} min is listed`);
  assert.match(shown, /Stopped after N minutes; reply with 'continue' to go on\./);
  assert.match(shown, /The limit covers every master: Claude, Codex and rcli\./);
  assert.doesNotMatch(shown, /still stops after/, "no master keeps a clock of its own (OB-5d)");
  assert.doesNotMatch(shown, /ran out of time|cut off/);
  assert.match(markup, /<form[^>]*aria-label="Master time limit"/);
  assert.doesNotMatch(markup, /<h2>Master<\/h2>/, "the tab has one title, above the step limit");
  assert.deepEqual(settings.TIME_LIMIT_CHOICES.map((option) => option.choice), TIMES);
  assert.deepEqual(settings.TIME_LIMIT_DEFAULT, { limit: "no_limit" });
});

test("No time limit is the default: it is checked before any save, unmarked, and Save waits for a change", () => {
  const markup = timeView(ok("no_limit"));
  assert.deepEqual(timeChecked(markup), ["no_limit"]);
  assert.ok(saveDisabled(markup));
  assert.ok(!marked(markup), "the default carries no changed mark");
});

for (const limit of TIMES) {
  test(`time limit ${limit}: a stored one is checked with Save resting, and picking it over another enables Save`, () => {
    const stored = timeView(ok(limit));
    assert.deepEqual(timeChecked(stored), [limit]);
    assert.ok(saveDisabled(stored));
    assert.equal(marked(stored), limit !== "no_limit", "only a limit away from the default is marked changed");
    const other = TIMES.find((choice) => choice !== limit);
    const picked = timeView(ok(other), { picked: limit });
    assert.deepEqual(timeChecked(picked), [limit]);
    assert.ok(!saveDisabled(picked));
    assert.ok(saveDisabled(timeView(ok(limit), { picked: limit })), "picking the stored limit is no change");
  });
}

test("the master time limit names what failed: a read that fails offers a retry, and a failed save shows its problem", () => {
  const failed = text(timeView({ kind: "error", message: "engine_failed" }));
  assert.match(failed, /Couldn't load the master time limit\. engine_failed/);
  assert.doesNotMatch(timeView({ kind: "loading" }), /<form/);
  const refused = timeView(ok("no_limit"), { picked: "minutes10", problem: "The engine refused the save." });
  assert.match(text(refused), /The engine refused the save\./);
  assert.match(refused, /role="alert"/);
});

const FRAMES = ["kib64", "kib256", "mib1"];
const frameView = (loaded, extra = {}) => renderToStaticMarkup(createElement(settings.MasterFrameView, {
  settings: loaded, picked: null, saving: false, problem: "", onPick: forbidden, onSave: forbidden, onRetry: forbidden, ...extra,
}));
const frameChecked = (markup) => [...markup.matchAll(/<input type="radio" name="frame-limit"[^>]*>/g)]
  .filter(([input]) => / checked=""/.test(input)).map(([input]) => input.match(/ value="([a-z0-9_]+)"/)[1]);

test("Settings, Master lists the master frame size limit with its three options, what a stopped turn says and which masters it covers (F39)", () => {
  const markup = frameView(ok("kib64"));
  const shown = text(markup);
  assert.match(shown, /Master frame size limit/);
  assert.match(shown, /Largest one message to the master/);
  assert.match(shown, /64 KiB The engine's own message size; enough for most turns\./);
  assert.match(shown, /256 KiB Room for long instructions and large pasted text\./);
  assert.match(shown, /1 MiB The largest; for a turn that still stops at 256 KiB\./);
  assert.match(shown, /One message was larger than the master frame size limit\. Raise it here, or at 1 MiB shorten the message or the instructions, then send again\./);
  assert.match(shown, /The limit covers Claude and Codex masters; rcli keeps 64 KiB\./);
  assert.match(markup, /<form[^>]*aria-label="Master frame size limit"/);
  assert.doesNotMatch(markup, /<h2>Master<\/h2>/, "the tab has one title, above the step limit");
  assert.deepEqual(settings.FRAME_LIMIT_CHOICES.map((option) => option.choice), FRAMES);
  assert.deepEqual(settings.FRAME_LIMIT_DEFAULT, { limit: "kib64" });
});

test("64 KiB is the default frame size: it is checked before any save, unmarked, and Save waits for a change", () => {
  const markup = frameView(ok("kib64"));
  assert.deepEqual(frameChecked(markup), ["kib64"]);
  assert.ok(saveDisabled(markup));
  assert.ok(!marked(markup), "the default carries no changed mark");
});

for (const limit of FRAMES) {
  test(`frame size ${limit}: a stored one is checked with Save resting, and picking it over another enables Save`, () => {
    const stored = frameView(ok(limit));
    assert.deepEqual(frameChecked(stored), [limit]);
    assert.ok(saveDisabled(stored));
    assert.equal(marked(stored), limit !== "kib64", "only a size away from the default is marked changed");
    const other = FRAMES.find((choice) => choice !== limit);
    const picked = frameView(ok(other), { picked: limit });
    assert.deepEqual(frameChecked(picked), [limit]);
    assert.ok(!saveDisabled(picked));
    assert.ok(saveDisabled(frameView(ok(limit), { picked: limit })), "picking the stored size is no change");
  });
}

test("the master frame size limit names what failed: a read that fails offers a retry, and a failed save shows its problem", () => {
  const failed = text(frameView({ kind: "error", message: "engine_failed" }));
  assert.match(failed, /Couldn't load the master frame size limit\. engine_failed/);
  assert.doesNotMatch(frameView({ kind: "loading" }), /<form/);
  const refused = frameView(ok("kib64"), { picked: "kib256", problem: "The engine refused the save." });
  assert.match(text(refused), /The engine refused the save\./);
  assert.match(refused, /role="alert"/);
});
