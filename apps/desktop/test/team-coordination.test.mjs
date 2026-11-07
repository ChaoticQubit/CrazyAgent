import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const coord = await import("../src/screens/team/coordination.tsx");

test("an engine that sends no coordination shows the plan defaults, native only for Claude Code", () => {
  const view = coord.coordination({ coordination: undefined });
  assert.deepEqual(view.settings, coord.COORD_DEFAULTS);
  assert.deepEqual(view.harnesses, [
    { harness: "claude-code", mode: "app_spawned", delivery: "auto", native: true },
    { harness: "codex", mode: "app_spawned", delivery: "auto", native: false },
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

test("Native is offered only where the harness kind has native teammates", () => {
  assert.deepEqual(coord.modeOptions(false).map((option) => option.value), ["app_spawned"]);
  assert.deepEqual(coord.modeOptions(true).map((option) => option.value), ["app_spawned", "native"]);
  assert.deepEqual(coord.DELIVERY_OPTIONS.map((option) => option.value), ["auto", "native_only", "typing_only"]);
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

test("Claude Code's row offers both terminal displays with Classic as the default (N-D9)", () => {
  assert.equal(coord.COORD_DEFAULTS.claude_display, "classic");
  assert.deepEqual(coord.DISPLAY_OPTIONS, [{ value: "classic", label: "Classic" }, { value: "fullscreen", label: "Fullscreen" }]);
  const status = (claude_display) => ({ project_id: "p", coordination: { settings: { ...coord.COORD_DEFAULTS, claude_display }, harnesses: [] } });
  for (const [display, helper] of [["classic", /the mouse wheel scrolls the terminal at once and a plain drag selects text/], ["fullscreen", /takes the mouse; the wheel scrolls 3 rows a notch and Option-drag selects text/]]) {
    const markup = renderToStaticMarkup(createElement(coord.CoordinationControls, { projectId: "p", status: status(display), reload: forbidden }));
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
