import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { SkillTag, skillEntries } = await import("../src/screens/skill-tag.tsx");
const plugins = await import("../src/screens/settings/plugins.tsx");
const pack = await import("../src/screens/settings/pack.tsx");
const process = await import("../src/screens/process.tsx");

const CATALOG = [
  { id: "skill-000000000000000000000001", name: "grill-me", description: "Asks one question at a time." },
  { id: "skill-000000000000000000000002", name: "graphify", description: "" },
];
const html = (component, props) => renderToStaticMarkup(createElement(component, props));
const forbidden = () => { throw new Error("static rendering must not call the engine"); };

test("a known skill name is a tag: hover shows its one-line description and it links to the skill", () => {
  const markup = html(SkillTag, { name: "grill-me", skills: CATALOG });
  assert.equal(markup, "<a class=\"skill-tag\" href=\"#/settings/plugins?skill=skill-000000000000000000000001\" title=\"Asks one question at a time.\" aria-label=\"Skill grill-me: Asks one question at a time.\">grill-me</a>");
  const bare = html(SkillTag, { name: "graphify", skills: CATALOG, label: "Graphify skill for agents" });
  assert.match(bare, /title="graphify"/);
  assert.match(bare, /aria-label="Skill Graphify skill for agents"/);
  assert.match(bare, />Graphify skill for agents<\/a>/);
});

test("an unknown skill name stays plain text", () => {
  assert.equal(html(SkillTag, { name: "superpowers", skills: CATALOG }), "superpowers");
  assert.equal(html(SkillTag, { name: "grill-me", skills: [] }), "grill-me");
  assert.equal(html(SkillTag, { name: "superpowers", skills: CATALOG, label: "Superpowers" }), "Superpowers");
});

test("clicking a tag follows its link, or hands the skill to the screen that opens it in place", () => {
  assert.equal(SkillTag({ name: "grill-me", skills: CATALOG }).props.onClick, undefined, "without a handler the link opens the Settings route");
  let prevented = false;
  let opened;
  const inPlace = SkillTag({ name: "grill-me", skills: CATALOG, onOpen: (skill) => { opened = skill; } });
  inPlace.props.onClick({ preventDefault: () => { prevented = true; } });
  assert.equal(prevented, true);
  assert.deepEqual(opened, CATALOG[0]);
});

test("the catalog uses the engine's one-line description and keeps the first skill of a name", () => {
  const listed = [
    { id: "skill-1", name: "notes", description: "Writes release notes. Use after merges.", line: "Writes release notes." },
    { id: "skill-2", name: "notes", description: "Project notes.", line: "" },
  ];
  assert.deepEqual(skillEntries(listed), [
    { id: "skill-1", name: "notes", description: "Writes release notes." },
    { id: "skill-2", name: "notes", description: "Project notes." },
  ]);
  assert.match(html(SkillTag, { name: "notes", skills: skillEntries(listed) }), /skill=skill-1/);
});

test("the skills list, the default pack and the Process page all draw skill names with the shared tag", () => {
  const skill = { id: "skill-000000000000000000000001", name: "grill-me", description: "Asks one question at a time. More.", line: "Asks one question at a time.",
    scope: { kind: "global" }, scope_label: "For every project", enabled: true, on_here: null, source: "/Users/owner/.claude/skills/grill-me", folder: "/x" };
  const list = html(plugins.SkillsView, { skills: { kind: "ok", value: [skill] }, failure: "", onAdd: forbidden, onOpen: forbidden, onToggle: forbidden, onRemove: forbidden, onRetry: forbidden });
  assert.match(list, /<a class="skill-tag" href="#\/settings\/plugins\?skill=skill-000000000000000000000001" title="Asks one question at a time\."[^>]*>grill-me<\/a>/);

  const entry = (id, kind = "skill") => ({ id, kind, group: "workflow", source: "github.com/x", license: "MIT", on_by_default: true, state: { state: "on" } });
  const status = { kind: "ok", value: { dependencies: [], pack: [entry("grill-me"), entry("superpowers"), entry("serena", "mcp_server"), entry("graphify")], running: false } };
  const packed = html(pack.PackSettingsView, { status, busy: null, problem: "", skills: CATALOG, onSwitch: forbidden, onAsk: forbidden, onInstallTools: forbidden, onRetry: forbidden });
  assert.match(packed, /<a class="skill-tag"[^>]*>grill-me<\/a>/);
  assert.match(packed, /<a class="skill-tag" href="#\/settings\/plugins\?skill=skill-000000000000000000000002"[^>]*>Graphify skill for agents<\/a>/);
  assert.match(packed, /<strong>superpowers<\/strong>/);
  assert.doesNotMatch(packed, /<a class="skill-tag"[^>]*>serena<\/a>/);

  const text = html(process.TaggedText, { text: "Run grill-me first.", skills: CATALOG });
  assert.match(text, /<span>Run <\/span><a class="skill-tag"[^>]*>grill-me<\/a><span> first\.<\/span>/);
});
