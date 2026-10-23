import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const plugins = await import("../src/screens/project/plugins.tsx");

const skill = (id, scope, extra = {}) => ({
  id, name: id, description: "", scope, scope_label: "", enabled: true, on_here: null, source: "", folder: `/skills/${id}`, ...extra,
});

test("skills and servers split into every-project and this-project groups in engine order", () => {
  const items = [skill("a", { kind: "global" }), skill("b", { kind: "project", project_id: "p-1" }), skill("c", { kind: "global" })];
  const groups = plugins.pluginGroups(items);
  assert.deepEqual(groups.global.map((item) => item.id), ["a", "c"]);
  assert.deepEqual(groups.project.map((item) => item.id), ["b"]);
  assert.deepEqual(plugins.pluginGroups([]), { global: [], project: [] });
});

test("the per-project switch reads on_here and falls back to the global switch", () => {
  assert.equal(plugins.onHere({ on_here: false, enabled: true }), false);
  assert.equal(plugins.onHere({ on_here: true, enabled: false }), true);
  assert.equal(plugins.onHere({ on_here: null, enabled: true }), true);
  assert.equal(plugins.onHere({ on_here: null, enabled: false }), false);
});

test("MCP server status words get the design tones", () => {
  assert.equal(plugins.serverTone("approved"), "ok");
  assert.equal(plugins.serverTone("waiting_for_approval"), "warn");
  assert.equal(plugins.serverTone("cant_start"), "danger");
  assert.equal(plugins.serverTone("off"), "neutral");
});

test("a typed skill folder must be a full path", () => {
  assert.equal(plugins.skillFolderError("/Users/me/skills/review"), null);
  assert.equal(plugins.skillFolderError("  /Users/me/skills/review  "), null);
  assert.equal(plugins.skillFolderError(""), "Enter the full path of a folder that holds SKILL.md.");
  assert.equal(plugins.skillFolderError("skills/review"), "Enter the full path of a folder that holds SKILL.md.");
  assert.equal(plugins.skillFolderError("/a\0b"), "Enter the full path of a folder that holds SKILL.md.");
  assert.equal(plugins.skillFolderError(`/${"a".repeat(4096)}`), "This path is too long.");
});

test("both sections render their loading state without bridge effects", () => {
  const data = { id: "p-1", notify: () => {}, navigate: () => {} };
  assert.match(renderToStaticMarkup(createElement(plugins.ProjectSkills, { data })), /<h2>Skills<\/h2>[\s\S]*Loading skills/);
  assert.match(renderToStaticMarkup(createElement(plugins.ProjectServers, { data })), /<h2>MCP servers<\/h2>[\s\S]*Loading MCP servers/);
});

test("the Overview fact counts what is on in this project, or falls back to a plain link", () => {
  const on = (on_here, enabled = true) => ({ on_here, enabled });
  assert.equal(plugins.onFact({ kind: "ok", value: [on(true), on(false), on(null), on(null, false)] }, "skill", "skills"), "2 skills on");
  assert.equal(plugins.onFact({ kind: "ok", value: [on(true)] }, "skill", "skills"), "1 skill on");
  assert.equal(plugins.onFact({ kind: "ok", value: [] }, "skill", "skills"), "None on");
  assert.equal(plugins.onFact({ kind: "not_ready" }, "skill", "skills"), null);
  assert.equal(plugins.onFact(undefined, "skill", "skills"), null);
});
