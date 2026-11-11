import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const coord = await import("../src/screens/team/coordination.tsx");

test("Overlapping edits lists every engine choice in the mockup's order, then an unknown value as it is", async () => {
  const { OVERLAP_CHOICES } = await import("../src/engine-client.ts");
  const values = (current) => coord.overlapOptions(current).map((option) => option.value);
  assert.deepEqual(values("warn"), ["warn", "wait", "block", "off"]);
  assert.deepEqual([...values("warn")].sort(), [...OVERLAP_CHOICES].sort(), "no engine choice drops from the list");
  assert.deepEqual(values("later"), ["warn", "wait", "block", "off", "later"], "a value this app does not know stays selectable");
});

test("an engine that sends no coordination shows the plan defaults, native for Claude Code and Codex", () => {
  const view = coord.coordination({ coordination: undefined });
  assert.deepEqual(view.settings, coord.COORD_DEFAULTS);
  assert.deepEqual(view.harnesses, [
    { harness: "claude-code", mode: "app_spawned", delivery: "auto", native: true },
    { harness: "codex", mode: "app_spawned", delivery: "auto", native: true },
    { harness: "rcli", mode: "app_spawned", delivery: "auto", native: false },
  ]);
  const sent = { settings: { ...coord.COORD_DEFAULTS, nudges: "first_edit" }, harnesses: [{ harness: "codex", mode: "app_spawned", delivery: "typing_only", native: false }] };
  const merged = coord.coordination({ coordination: sent });
  assert.equal(merged.settings.nudges, "first_edit");
  assert.equal(merged.harnesses[1].delivery, "typing_only");
  assert.equal(merged.harnesses[0].harness, "claude-code");
});

test("the desktop defaults are the engine's counted tools", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/coord/settings.rs", import.meta.url), "utf8");
  const block = source.slice(source.indexOf("pub const COUNTED_TOOLS_DEFAULT"), source.indexOf("];", source.indexOf("pub const COUNTED_TOOLS_DEFAULT")));
  const engine = [...block.matchAll(/"([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(coord.COORD_DEFAULTS.counted_tools, engine);
});

test("Native is listed for every harness kind and chosen only where it has native sub-agents", () => {
  for (const harness of ["claude-code", "codex", "rcli"]) {
    const native = coord.NATIVE_UNAVAILABLE[harness] === null;
    const options = coord.modeOptions(harness, native);
    assert.deepEqual(options.map((option) => option.value), ["native", "app_spawned"], `${harness}: native comes first, as the mockup lists it`);
    assert.equal(options[0].disabledReason, native ? undefined : coord.NATIVE_UNAVAILABLE[harness], harness);
    assert.equal(options[1].disabledReason, undefined, `${harness}: CrazyAgent workers are always open`);
    assert.ok(coord.modeHelper(harness, native).endsWith("."), harness);
  }
  assert.match(coord.modeHelper("codex", true), /Codex sub-agents run inside the lead's own session and worktree, on its model\./);
  assert.ok(coord.modeHelper("codex", true).includes(coord.CODEX_NATIVE_SCOPE), "the Codex helper says what the app cannot show");
  assert.ok(!coord.modeHelper("claude-code", true).includes(coord.CODEX_NATIVE_SCOPE));
  assert.equal(coord.modeHelper("rcli", false), coord.NATIVE_UNAVAILABLE.rcli);
  assert.deepEqual(coord.DELIVERY_OPTIONS.map((option) => option.value), ["auto", "native_only", "typing_only"]);
});

test("the desktop says why RCLI has no native sub-agents in the engine's own words", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/coord/settings.rs", import.meta.url), "utf8");
  const engine = /pub const RCLI_NATIVE_UNAVAILABLE: &str =\s*"([^"]+)";/u.exec(source)?.[1];
  assert.ok(engine, "the engine states RCLI's reason");
  assert.equal(coord.NATIVE_UNAVAILABLE.rcli, engine);
});

test("the desktop says what the app cannot show of Codex sub-agents in the engine's own words", () => {
  const source = readFileSync(new URL("../../../crates/crazyagent/src/coord/settings.rs", import.meta.url), "utf8");
  const engine = /pub const CODEX_NATIVE_SCOPE: &str =\s*"([^"]+)";/u.exec(source)?.[1];
  assert.ok(engine, "the engine states what Codex sub-agents leave out");
  assert.equal(coord.CODEX_NATIVE_SCOPE, engine);
});

test("counted tools are read with the engine's rule: once each, at most 32, wildcards allowed", () => {
  assert.deepEqual(coord.countedToolsInput("start_work, ctx_*\nstart_work,, graphify"), { kind: "ok", tools: ["start_work", "ctx_*", "graphify"] });
  const many = Array.from({ length: 33 }, (_, index) => `tool_${index}`).join(",");
  assert.deepEqual(coord.countedToolsInput(many), { kind: "invalid", reason: "Count at most 32 tools." });
  const thirtyTwo = Array.from({ length: 32 }, (_, index) => `tool_${index}`).join(",");
  assert.equal(coord.countedToolsInput(thirtyTwo).kind, "ok");
  assert.deepEqual(coord.countedToolsInput("bad name!"), { kind: "invalid", reason: "\"bad name!\" isn't a tool name. Use letters, digits, _, - or * only." });
  assert.equal(coord.countedToolsInput("x".repeat(65)).kind, "invalid");
  assert.deepEqual(coord.countedToolsInput(""), { kind: "ok", tools: [] });
});

const forbidden = () => { throw new Error("static rendering must not save"); };
const skill = (name, patch = {}) => ({ id: `skill-${name}`, name, description: "d", line: "d", scope: { kind: "global" }, scope_label: "For every project",
  enabled: true, on_here: true, source: `/Users/owner/skills/${name}`, folder: `/x/${name}`, refused: null, ...patch });
const projectSkills = [skill("style-guide"), skill("release-notes", { scope: { kind: "project", project_id: "p" }, on_here: null }), skill("switched-off", { enabled: false }),
  skill("off-here", { on_here: false }), skill("unreadable", { refused: "This skill's SKILL.md can't be read." })];
const roleProps = (settings, patch = {}) => ({ settings: { ...coord.COORD_DEFAULTS, ...settings }, skills: { kind: "ok", value: projectSkills }, busy: false, onChoose: forbidden, ...patch });

test("every role starts on All approved skills, the engine's default", () => {
  assert.deepEqual(coord.COORD_DEFAULTS.role_skills, { master: null, lead: null, worker: null });
  const markup = renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps({})));
  for (const role of ["Master", "Lead", "Worker"]) assert.match(markup, new RegExp(`role="radiogroup" aria-label="${role} skills"`), role);
  assert.deepEqual([...markup.matchAll(/role="radio" aria-checked="true"[^>]*>([^<]*)</g)].map((match) => match[1]), ["All approved skills", "All approved skills", "All approved skills"]);
  assert.doesNotMatch(markup, /type="checkbox"/);
});

test("a role on Chosen skills ticks its approved skills and names chosen ones that are not approved", () => {
  const markup = renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps({ role_skills: { master: null, lead: [], worker: ["style-guide", "gone"] } })));
  assert.deepEqual([...markup.matchAll(/role="radio" aria-checked="true"[^>]*>([^<]*)</g)].map((match) => match[1]), ["All approved skills", "Chosen skills", "Chosen skills"]);
  const boxes = [...markup.matchAll(/<input type="checkbox"([^>]*)\/><span>([^<]*)<\/span>/g)].map((match) => [match[2], match[1].includes("checked")]);
  assert.deepEqual(boxes, [["release-notes", false], ["style-guide", false], ["release-notes", false], ["style-guide", true]]);
  assert.match(markup, /Chosen but not approved here, so no agent gets them: gone\./);
  assert.doesNotMatch(markup, /disabled/);
  const saving = renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps({ role_skills: { master: null, lead: null, worker: [] } }, { busy: true })));
  assert.equal(saving.match(/disabled=""/g).length, 2);
});

test("a role on Chosen skills says when the project's skills are loading, unreadable or none are approved", () => {
  const chosen = { role_skills: { master: [], lead: null, worker: null } };
  assert.match(renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps(chosen, { skills: { kind: "loading" } }))), /Loading this project&#x27;s skills\.\.\./);
  assert.match(renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps(chosen, { skills: { kind: "error", code: "x", message: "m" } }))), /Couldn&#x27;t load this project&#x27;s skills\./);
  assert.match(renderToStaticMarkup(createElement(coord.RoleSkillsControls, roleProps(chosen, { skills: { kind: "ok", value: [] } }))), /No approved skills for this project yet\./);
});

test("approved skills are the ones on here and readable, and a tick keeps the names in order", () => {
  assert.deepEqual(coord.approvedSkills(projectSkills), ["release-notes", "style-guide"]);
  assert.deepEqual(coord.approvedSkills([skill("a"), skill("a", { scope: { kind: "project", project_id: "p" }, on_here: null })]), ["a"]);
  const approved = ["release-notes", "style-guide"];
  assert.deepEqual(coord.tickedSkills(null, approved, "release-notes", false), ["style-guide"]);
  assert.deepEqual(coord.tickedSkills([], approved, "style-guide", true), ["style-guide"]);
  assert.deepEqual(coord.tickedSkills(["style-guide"], approved, "release-notes", true), approved);
  assert.deepEqual(coord.tickedSkills(["style-guide", "gone"], approved, "style-guide", false), ["gone"]);
});

test("Coordination leads with the mockup's rows, each harness's sub-agents together, and keeps the per-harness rows in their own harness groups", () => {
  assert.equal(typeof coord.PerHarnessControls, "function", "the per-harness rows have their own part");
  const status = { project_id: "p", coordination: { settings: coord.COORD_DEFAULTS, harnesses: [] } };
  const rows = (markup) => [...markup.matchAll(/data-setting="([^"]*)"/g)].map((match) => match[1]);
  const lead = renderToStaticMarkup(createElement(coord.CoordinationControls, { projectId: "p", status, reload: forbidden }));
  assert.deepEqual(rows(lead), ["Claude Code sub-agents", "Codex sub-agents", "rcli sub-agents", "Sub-agent questions go to", "Overlapping edits", "Terminals per window"]);
  const per = renderToStaticMarkup(createElement(coord.PerHarnessControls, { projectId: "p", status, reload: forbidden }));
  assert.deepEqual(rows(per), ["Messages to Claude Code agents", "Claude Code terminal display", "Messages to Codex agents", "Messages to rcli agents"]);
  assert.deepEqual([...per.matchAll(/role="group" aria-label="([^"]*)"/g)].map((match) => match[1]), ["Claude Code", "Codex", "rcli"]);
});

test("Claude Code's row offers both terminal displays with Classic as the default (N-D9)", () => {
  assert.equal(coord.COORD_DEFAULTS.claude_display, "classic");
  assert.deepEqual(coord.DISPLAY_OPTIONS, [{ value: "classic", label: "Classic" }, { value: "fullscreen", label: "Fullscreen" }]);
  const status = (claude_display) => ({ project_id: "p", coordination: { settings: { ...coord.COORD_DEFAULTS, claude_display }, harnesses: [] } });
  for (const [display, helper] of [["classic", /the mouse wheel scrolls the terminal at once and a plain drag selects text/], ["fullscreen", /takes the mouse; the wheel scrolls 3 rows a notch and Option-drag selects text/]]) {
    assert.equal(typeof coord.PerHarnessControls, "function", "the display row sits in the per-harness part");
    const markup = renderToStaticMarkup(createElement(coord.PerHarnessControls, { projectId: "p", status: status(display), reload: forbidden }));
    const claude = markup.slice(markup.indexOf('aria-label="Claude Code"'), markup.indexOf('aria-label="Codex"'));
    assert.match(claude, /<label for="tg-display-claude-code-p">Claude Code terminal display<\/label>/, display);
    assert.match(claude, new RegExp(`<option value="${display}" selected="">`), display);
    assert.match(claude, helper, display);
    assert.match(claude, /applies when a Claude Code terminal next starts/, display);
    assert.doesNotMatch(markup.slice(markup.indexOf('aria-label="Codex"')), /terminal display/, "only Claude Code has the display");
  }
});

test("the Task-start reminder row follows Workflow nudges with On as the default and both options (T3-15)", () => {
  assert.equal(coord.COORD_DEFAULTS.task_start, "on");
  for (const [option, checked] of [["on", "On"], ["off", "Off"]]) {
    const status = { project_id: "p", coordination: { settings: { ...coord.COORD_DEFAULTS, task_start: option }, harnesses: [] } };
    const markup = renderToStaticMarkup(createElement(coord.ToolsControls, { projectId: "p", status, reload: forbidden }));
    const nudges = markup.indexOf('aria-label="Workflow nudges"');
    const reminder = markup.indexOf('aria-label="Task-start reminder"');
    const counted = markup.indexOf('aria-label="Counted tools"');
    assert.ok(nudges >= 0 && nudges < reminder && reminder < counted, `the reminder row sits between Workflow nudges and Counted tools: ${option}`);
    const row = markup.slice(reminder, counted);
    assert.deepEqual([...row.matchAll(/role="radio" aria-checked="(true|false)"[^>]*>([^<]+)</gu)].map((match) => [match[2], match[1]]),
      [["On", String(checked === "On")], ["Off", String(checked === "Off")]], option);
    assert.match(row, /ToolSearch/, option);
    assert.match(row, /first turn of each task/, option);
  }
});

/** Installs a fake engine holding each project's coordination settings: a team read answers
 * with them, and a save stores what it sends unless it is refused, counting how many saves run
 * at once. Input: the settings by project id. Output: the saves sent and that most. */
function coordEngine(stored) {
  const seen = { sent: [], most: 0 };
  let running = 0;
  globalThis.window = { __TAURI_INTERNALS__: { async invoke(command, args) {
    if (command === "desktop_team_status") {
      if (!(args.id in stored)) throw `Command ${command} not found`;
      return { project_id: args.id === "wrong" ? "other" : args.id, coordination: { settings: { ...stored[args.id] }, harnesses: [] } };
    }
    assert.equal(command, "desktop_team_coordination");
    running += 1;
    seen.most = Math.max(seen.most, running);
    seen.sent.push(args.settings);
    await new Promise((done) => setTimeout(done, 20));
    running -= 1;
    const refused = args.settings.claude_display === "fullscreen";
    if (!refused) stored[args.id] = { ...args.settings };
    return { outcome: "applied", pending: false, rejection: refused ? "team_settings_invalid" : null };
  } } };
  return seen;
}

const pick = (settings) => [settings.overlaps, settings.nudges, settings.task_start, settings.claude_display];

test("quick coordination saves from any row keep every change: one save at a time, each laid on the engine's newest settings", async () => {
  assert.equal(typeof coord.saveCoordSettings, "function", "coordination.tsx has one save path for coordination settings");
  const engine = coordEngine({ p1: { ...coord.COORD_DEFAULTS }, p2: { ...coord.COORD_DEFAULTS } });
  const both = await Promise.all([coord.saveCoordSettings("p1", { overlaps: "block" }), coord.saveCoordSettings("p1", { nudges: "ten_minutes" })]);
  assert.deepEqual(both.map((result) => result.kind), ["ok", "ok"]);
  assert.equal(engine.most, 1, "one coordination save runs at a time");
  assert.deepEqual(engine.sent.map(pick), [["block", "off", "on", "classic"], ["block", "ten_minutes", "on", "classic"]], "the second save keeps Block");
  await coord.saveCoordSettings("p1", { claude_display: "fullscreen" });
  await coord.saveCoordSettings("p1", { nudges: "first_edit" });
  assert.deepEqual(pick(engine.sent[3]), ["block", "first_edit", "on", "classic"], "a refused save is not built on");
  await coord.saveCoordSettings("p2", { task_start: "off" });
  assert.deepEqual(pick(engine.sent[4]), ["warn", "off", "off", "classic"], "each project keeps its own saves");
});

test("a coordination save keeps the value the TUI or another window set since, even one equal to an older read, and sends nothing when the team read fails", async () => {
  const stored = { p: { ...coord.COORD_DEFAULTS } };
  const engine = coordEngine(stored);
  await coord.saveCoordSettings("p", { overlaps: "block" });
  stored.p = { ...stored.p, overlaps: "warn" };
  await coord.saveCoordSettings("p", { nudges: "ten_minutes" });
  assert.deepEqual(pick(engine.sent[1]), ["warn", "ten_minutes", "on", "classic"], "Warn set back elsewhere stays");
  stored.p = { ...stored.p, overlaps: "block" };
  await coord.saveCoordSettings("p", { task_start: "off" });
  assert.deepEqual(pick(engine.sent[2]), ["block", "ten_minutes", "off", "classic"], "Block set elsewhere stays");
  assert.deepEqual((await coord.saveCoordSettings("missing", { nudges: "off" })).kind, "not_ready", "a failed team read is the save's result");
  assert.equal(engine.sent.length, 3, "nothing is sent without the engine's settings");
  assert.throws(() => coord.saveCoordSettings("p", {}), /changes at least one setting/);
  stored.wrong = { ...coord.COORD_DEFAULTS };
  await assert.rejects(coord.saveCoordSettings("wrong", { nudges: "off" }), /its own project's team read/);
  assert.equal(engine.sent.length, 3, "a save on another project's read sends nothing");
});
