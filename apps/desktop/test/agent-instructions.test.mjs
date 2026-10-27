import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-process.ts");
const view = await import("../src/screens/team/agent-instructions.tsx");

const SKILLS = [
  { id: "skill-000000000000000000000001", name: "tdd", description: "Write the failing test first." },
  { id: "skill-000000000000000000000002", name: "code-review", description: "Review a diff for bugs." },
];

function node(id, kind) {
  return { id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: null };
}

function panel(read) {
  return renderToStaticMarkup(createElement(view.AgentInstructionsView, { read, skills: SKILLS }));
}

test("the master and each agent card name the team agent the engine reads", () => {
  assert.equal(client.agentOf(node("master", "master")), null);
  assert.equal(client.agentOf(node("team-12", "agent")), 12);
  for (const shown of [node("group:codex", "harness_group"), node("team-0", "agent"), node("team--3", "agent"), node("team-1x", "agent"), node("team:12", "agent"), node("team-012", "agent"),
    node("team-99999999999999999999", "agent"), node("master", "agent")]) {
    assert.equal(client.agentOf(shown), undefined, shown.id);
  }
});

test("the view shows the instruction file with its size and each installed skill as a tag", () => {
  const markup = panel({ kind: "ok", value: { agent: 12, harness: "codex", launched: true, text: "Use tdd.\n", skills: ["tdd", "retired-skill"] } });
  assert.match(markup, /AGENTS\.md/);
  assert.match(markup, /9 bytes/);
  assert.match(markup, /Use <\/span><a[^>]*>tdd<\/a><span>\./);
  assert.match(markup, /<a[^>]*href="#\/settings\/plugins\?skill=skill-000000000000000000000001"[^>]*title="Write the failing test first\."[^>]*>tdd<\/a>/);
  assert.match(markup, /retired-skill/);
  assert.doesNotMatch(markup, /<a[^>]*>retired-skill<\/a>/, "a name the catalog no longer has is plain text");
  assert.doesNotMatch(markup, /<textarea|<input|<button[^>]*>Save/, "the view is read-only");

  const claude = panel({ kind: "ok", value: { agent: 3, harness: "claude-code", launched: true, text: "", skills: [] } });
  assert.match(claude, /CLAUDE\.md/);
  assert.match(claude, /No app or project instructions were given at its last launch\./);
  assert.match(claude, /No skills were installed at its last launch\./);
});

test("an agent that never launched and a refused read say so plainly", () => {
  assert.match(panel({ kind: "ok", value: { agent: 4, harness: "codex", launched: false, text: "", skills: [] } }), /hasn&#x27;t launched yet/);
  const refused = panel({ kind: "error", code: "agent_instructions_unsafe", message: "" });
  assert.match(refused, /link or not a plain file/);
  assert.doesNotMatch(refused, /agent_instructions_/);
  assert.match(panel({ kind: "loading" }), /Reading what this agent received/);
  assert.match(panel({ kind: "not_ready" }), /aren&#x27;t ready in this build/);
});

test("every agent view refusal has a plain sentence and is an engine code", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/process.rs", import.meta.url), "utf8");
  const listed = engine.slice(engine.indexOf("PROCESS_CODES: &[&str] = &["), engine.indexOf("];"));
  for (const code of ["agent_instructions_unsafe", "agent_instructions_too_large", "agent_instructions_invalid", "agent_instructions_unreadable",
    "process_agent_unknown", "process_agent_unsupported"]) {
    assert.ok(listed.includes(`"${code}"`), `${code} is not an engine code`);
    assert.ok(client.PROCESS_REFUSALS.includes(code), code);
    assert.doesNotMatch(client.processMessage({ kind: "error", code, message: "" }), /agent_instructions_|process_/);
  }
});

test("the agent panel reads the view for the open card only", () => {
  const source = readFileSync(new URL("../src/screens/team/index.tsx", import.meta.url), "utf8");
  assert.match(source, /<AgentInstructions key=\{openNode\.id\} projectId=\{projectId\} node=\{openNode\} \/>/);
});
