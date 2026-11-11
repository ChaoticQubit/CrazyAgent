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

function node(id, kind, extra = {}) {
  return { id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, terminal: null, ...extra };
}

function panel(read) {
  return renderToStaticMarkup(createElement(view.AgentInstructionsView, { read, skills: SKILLS }));
}

test("the master and each agent card name the team agent the engine reads", () => {
  assert.equal(client.agentOf(node("master", "master", { agent: 4 })), null);
  assert.equal(client.agentOf(node("team-12", "lead", { agent: 12 })), 12);
  assert.equal(client.agentOf(node("team-12", "sub", { agent: 12 })), 12);
  assert.equal(client.agentOf(node("team-12", "sub", { agent: 7 })), 7, "the node's own agent id wins over its card id");
  assert.equal(client.agentOf(node("team-9", "teammate", { agent: 9, pane: "%12" })), 9);
  for (const shown of [node("team-12", "lead"), node("team-12", "sub"), node("teammate-3-12", "teammate", { agent: null, pane: "%12" }), node("team-3", "teammate"), node("team-0", "sub"), node("team--3", "sub"), node("team-1x", "lead"),
    node("team:12", "sub"), node("team-012", "sub"), node("team-99999999999999999999", "sub"), node("master", "lead"), node("team-5", "sub", { agent: 0 }),
    node("team-5x", "sub", { agent: 1.5 }), node("team-5y", "sub", { agent: -2 }), node("team-5", "sub", { agent: null })]) {
    assert.equal(client.agentOf(shown), undefined, shown.id);
  }
});

test("the view shows the instruction file with its size and each installed skill as a tag", () => {
  const markup = panel({ kind: "ok", value: { agent: 12, harness: "codex", launched: true, text: "Use tdd.\n", skills: ["tdd", "retired-skill"], sessions: 0, loaded: [], skills_used: [], files: [] } });
  assert.match(markup, /AGENTS\.md/);
  assert.match(markup, /9 bytes/);
  assert.match(markup, /Use <\/span><a[^>]*>tdd<\/a><span>\./);
  assert.match(markup, /<a[^>]*href="#\/settings\/plugins\?skill=skill-000000000000000000000001"[^>]*title="Write the failing test first\."[^>]*>tdd<\/a>/);
  assert.match(markup, /retired-skill/);
  assert.doesNotMatch(markup, /<a[^>]*>retired-skill<\/a>/, "a name the catalog no longer has is plain text");
  assert.doesNotMatch(markup, /<textarea|<input|<button[^>]*>Save/, "the view is read-only");

  const claude = panel({ kind: "ok", value: { agent: 3, harness: "claude-code", launched: true, text: "", skills: [], sessions: 0, loaded: [], skills_used: [], files: [] } });
  assert.match(claude, /CLAUDE\.md/);
  assert.match(claude, /No app or project instructions were given at its last launch\./);
  assert.match(claude, /No skills were installed at its last launch\./);
});

test("the view says which instruction files the harness loaded and which skills it invoked", () => {
  const value = { agent: 12, harness: "claude-code", launched: true, text: "", skills: [], sessions: 2,
    loaded: [{ name: "CLAUDE.md", path: "/w/agent-12/CLAUDE.md" }, { name: "AGENTS.md", path: "/w/agent-12/AGENTS.md" }, { name: "CLAUDE.md", path: "/h/agent-12/CLAUDE.md" }],
    skills_used: ["tdd", "systematic-debugging"], files: [] };
  const markup = panel({ kind: "ok", value });
  assert.match(markup, /Instructions: CLAUDE\.md, AGENTS\.md - Skills used: tdd, systematic-debugging/);
  for (const path of ["/w/agent-12/CLAUDE.md", "/w/agent-12/AGENTS.md", "/h/agent-12/CLAUDE.md"]) assert.ok(markup.includes(path), path);
  assert.ok(markup.indexOf("Skills used:") < markup.indexOf("No app or project instructions"), "the report leads the view");

  for (const harness of ["claude-code", "codex"]) {
    assert.match(panel({ kind: "ok", value: { ...value, harness, loaded: [], skills_used: [] } }),
      /Instructions: none loaded by the harness itself; CrazyAgent gives its instructions at launch - Skills used: none/,
      `DEC-141 PA-14: one line for every harness, ${harness}`);
    const unreported = panel({ kind: "ok", value: { ...value, harness, sessions: 0, loaded: [], skills_used: [] } });
    assert.match(unreported, /Instructions: not reported yet - Skills used: not reported yet/, harness);
    assert.match(unreported, /A headless run reports nothing here while it keeps no transcript\./, harness);
    assert.doesNotMatch(unreported, /Claude run/, harness);
  }
  const tui = readFileSync(new URL("../../../crates/crazyagent/src/tui/screens/team.rs", import.meta.url), "utf8");
  for (const line of ["none loaded by the harness itself; CrazyAgent gives its instructions at launch", "A headless run reports nothing here while it keeps no transcript."]) {
    assert.ok(tui.includes(line), `the TUI says the same: ${line}`);
  }
  assert.doesNotMatch(panel({ kind: "ok", value: { ...value, launched: false } }), /Skills used/, "an agent that never launched has no report");
});

test("the view lists each instruction file the engine recorded with its size and what became of it", () => {
  const files = [
    { name: "CLAUDE.md", bytes: 1229, state: "given" },
    { name: "AGENTS.md", bytes: 300, state: "same" },
    { name: "CLAUDE.local.md", bytes: 0, state: "unsafe" },
    { name: "~/.claude/CLAUDE.md", bytes: 40000, state: "too_large" },
    { name: "~/.codex/AGENTS.md", bytes: 12, state: "invalid" },
    { name: ".claude/CLAUDE.md", bytes: 5, state: "unreadable" },
  ];
  const markup = panel({ kind: "ok", value: { agent: 3, harness: "claude-code", launched: true, text: "Use tdd.\n", skills: [], sessions: 0, loaded: [], skills_used: [], files } });
  for (const shown of [
    /CLAUDE\.md<\/span> 1\.2 KB, given/,
    /AGENTS\.md<\/span> 300 bytes, left out: the same text as the instructions or a file above/,
    /CLAUDE\.local\.md<\/span> 0 bytes, left out: a link or not a plain file/,
    /~\/\.claude\/CLAUDE\.md<\/span> 39\.1 KB, left out: larger than CrazyAgent gives/,
    /~\/\.codex\/AGENTS\.md<\/span> 12 bytes, left out: not plain text/,
    /\.claude\/CLAUDE\.md<\/span> 5 bytes, left out: couldn&#x27;t be read/,
  ]) assert.match(markup, shown);
  assert.ok(markup.indexOf(">Instruction files</h4>") < markup.indexOf(">Skills</h4>"));
  const none = panel({ kind: "ok", value: { agent: 3, harness: "claude-code", launched: true, text: "", skills: [], sessions: 0, loaded: [], skills_used: [], files: [] } });
  assert.match(none, /No instruction files were found at its last launch\./);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/instruction_files.rs", import.meta.url), "utf8");
  for (const state of ["given", "same", "too_large", "unsafe", "invalid", "unreadable"]) {
    assert.match(engine, new RegExp(`=> "${state}"`), `${state} is a state the engine records`);
  }
});

test("an agent that never launched and a refused read say so plainly", () => {
  assert.match(panel({ kind: "ok", value: { agent: 4, harness: "codex", launched: false, text: "", skills: [], sessions: 0, loaded: [], skills_used: [], files: [] } }), /hasn&#x27;t launched yet/);
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
  assert.match(source, /<AgentSide key=\{panelKey\(node\)\} projectId=\{projectId\} node=\{node\} /);
  assert.match(source, /\{openNode && side\(openNode\)\}/);
});

test("the open agent's terminal head carries the reported line once the read is in", () => {
  const value = { agent: 12, harness: "codex", launched: true, text: "", skills: [], sessions: 1,
    loaded: [{ name: "AGENTS.md", path: "/w/agent-12/AGENTS.md" }], skills_used: ["tdd"], files: [] };
  assert.equal(view.headReport({ kind: "ok", value }), "Instructions: AGENTS.md - Skills used: tdd");
  assert.equal(view.headReport({ kind: "ok", value: { ...value, launched: false } }), undefined, "an agent that never launched has no report");
  assert.equal(view.headReport({ kind: "loading" }), undefined);
  const source = readFileSync(new URL("../src/screens/team/agent-instructions.tsx", import.meta.url), "utf8");
  assert.match(source, /<TerminalPanel [^>]*reported=\{agent === undefined \? undefined : headReport\(read\)\}/);
});
