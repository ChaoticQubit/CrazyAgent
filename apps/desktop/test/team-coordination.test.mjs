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
  const sent = { settings: { ...coord.COORD_DEFAULTS, nudges: false }, harnesses: [{ harness: "codex", mode: "app_spawned", delivery: "typing_only", native: false }] };
  const merged = coord.coordination({ coordination: sent });
  assert.equal(merged.settings.nudges, false);
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
