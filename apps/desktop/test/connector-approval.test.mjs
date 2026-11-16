import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-connectors.ts");
const approval = await import("../src/screens/connectors/approval.tsx");
const router = await import("../src/ui/router.ts");

const CREATED = 1_763_240_880_000;
const WAIT_MS = 600_000;
const WAITING = {
  id: 7, project: "alpha", project_name: "Alpha", agent: "agent-2", connector: "board", title: "Board", tool: "create_item",
  preview: "create_item title=Ship it", arguments: "{\n  \"title\": \"Ship it\"\n}", hidden: false,
  state: "waiting", created_ms: CREATED, expires_ms: CREATED + WAIT_MS, answered_ms: null,
};
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const actions = { onAnswer: forbidden, onRetry: forbidden };
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ").trim();
const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const render = (patch = {}, props = {}) => renderToStaticMarkup(createElement(approval.ApprovalPageView, {
  view: { kind: "ok", value: { ...WAITING, ...patch } }, now: CREATED + 1000, busy: false, failure: "", actions, ...props,
}));
const button = (markup, label) => markup.match(new RegExp(`<button[^>]*>${label}</button>`, "u"))?.[0] ?? null;

test("A waiting write's page says, in order, the connector, tool, project, agent, when it asked and what it sends, then the time left with Allow and Deny", () => {
  const lines = ["Connector: Board", "Tool: create_item", "Project: Alpha", "Agent: agent-2", `Asked: ${api.stampWords(CREATED)}`, "What it sends: create_item title=Ship it"];
  assert.deepEqual(api.approvalLines(WAITING), lines);
  const markup = render();
  const shown = text(markup);
  assert.ok(shown.startsWith(`${api.APPROVAL_PAGE.title} ${api.APPROVAL_PAGE.purpose}`), shown);
  let at = 0;
  for (const words of [...lines, "Time left: 9:59"]) {
    const found = shown.indexOf(words, at);
    assert.ok(found >= at, `${words} comes next: ${shown}`);
    at = found + words.length;
  }
  assert.ok(button(markup, "Allow") && button(markup, "Deny"), "Allow and Deny wait for the owner");
  assert.doesNotMatch(markup, /<a\b/u, "nothing an agent sent is a link");
  assert.ok(!shown.includes(api.APPROVAL_PAGE.hidden), "no hidden mark when the arguments hold none");
});

test("The full arguments sit in a closed All arguments fold as plain monospace text, and a write holding hidden characters says so", () => {
  const markup = render();
  const fold = markup.match(/<details[^>]*>(.*?)<\/details>/su);
  assert.ok(fold, "the arguments are in a fold");
  assert.doesNotMatch(fold[0], /<details[^>]*\bopen\b/u, "the fold starts closed");
  assert.match(fold[1], new RegExp(`<summary[^>]*>${api.APPROVAL_PAGE.fold}</summary>`, "u"));
  assert.match(fold[1], /<pre[^>]*class="[^"]*mono[^"]*"[^>]*>\{\n {2}&quot;title&quot;: &quot;Ship it&quot;\n\}<\/pre>/u, "line breaks kept, plain text");
  const marked = render({ hidden: true, arguments: "{\"title\": \"Ship\\u{202e}it\"}" });
  assert.ok(text(marked).includes(api.APPROVAL_PAGE.hidden), "the hidden mark shows");
  assert.match(marked, /<div[^>]*class="notice"[^>]*>(?:(?!<\/div>).)*<div>This write holds hidden/su, "the mark is a warning notice");
});

test("Time left counts whole seconds rounded up and never goes below 0:00", () => {
  assert.equal(api.timeLeft(0, 600_000), "10:00");
  assert.equal(api.timeLeft(1000, 600_000), "9:59");
  assert.equal(api.timeLeft(0, 59_001), "1:00");
  assert.equal(api.timeLeft(0, 61_000), "1:01");
  assert.equal(api.timeLeft(0, 1), "0:01");
  assert.equal(api.timeLeft(5, 5), "0:00");
  assert.equal(api.timeLeft(10_000, 5), "0:00");
});

test("A write still waiting when its wait runs out shows Expired and no buttons, before the engine marks it", () => {
  assert.equal(api.shownState(WAITING, CREATED + WAIT_MS - 1), "waiting");
  assert.equal(api.shownState(WAITING, CREATED + WAIT_MS), "expired", "the engine refuses an answer from its end on");
  for (const state of ["allowed", "denied", "expired"]) assert.equal(api.shownState({ ...WAITING, state, answered_ms: CREATED }, CREATED), state, state);
  for (const now of [CREATED + WAIT_MS, CREATED + WAIT_MS + 5000]) {
    const markup = render({}, { now });
    const shown = text(markup);
    assert.ok(shown.includes("Expired"), shown);
    assert.equal(button(markup, "Allow"), null, "no Allow once the wait ran out");
    assert.equal(button(markup, "Deny"), null, "no Deny once the wait ran out");
    assert.ok(!shown.includes("Time left"), shown);
  }
  assert.ok(text(render({}, { now: CREATED + WAIT_MS - 1 })).includes("Time left: 0:01"), "a millisecond before the end it still waits");
});

test("An answered or expired write shows where it stands and no buttons", () => {
  const answered = CREATED + 90_000;
  const cases = [
    [{ state: "allowed", answered_ms: answered }, `Allowed ${api.stampWords(answered)}`],
    [{ state: "denied", answered_ms: answered }, `Denied ${api.stampWords(answered)}`],
    [{ state: "expired", answered_ms: null }, "Expired"],
  ];
  for (const [patch, line] of cases) {
    assert.equal(api.answeredLine({ ...WAITING, ...patch }), line, patch.state);
    const markup = render(patch);
    assert.ok(text(markup).includes(line), `${patch.state}: ${text(markup)}`);
    assert.equal(button(markup, "Allow"), null, `${patch.state}: no Allow`);
    assert.equal(button(markup, "Deny"), null, `${patch.state}: no Deny`);
    assert.ok(!text(markup).includes("Time left"), `${patch.state}: no time left`);
  }
  assert.equal(api.answeredLine(WAITING), "");
  assert.throws(() => api.answeredLine({ ...WAITING, state: "allowed", answered_ms: null }), /answer time/u, "an answer always has its time");
});

test("While an answer is on its way both buttons wait, and a failed answer says why", () => {
  const markup = render({}, { busy: true });
  assert.match(button(markup, "Allow") ?? "", /disabled=""/u);
  assert.match(button(markup, "Deny") ?? "", /disabled=""/u);
  const failed = render({}, { failure: "That approval is gone." });
  assert.match(failed, /role="alert"[^>]*>.*That approval is gone\./su);
});

test("The page keeps its title while loading, when the engine is not ready and when the read fails", () => {
  for (const view of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", code: "connector_approval_unknown", message: "That approval is gone." }]) {
    assert.ok(text(render({}, { view })).startsWith(api.APPROVAL_PAGE.title), view.kind);
  }
  assert.ok(text(render({}, { view: { kind: "error", code: "connector_approval_unknown", message: "That approval is gone." } })).includes("That approval is gone."));
});

test("The approval route is #/connectors/approval/<id> for a whole id from 1 to 2^53-1; anything else is not that page", () => {
  for (const id of [1, 7, Number.MAX_SAFE_INTEGER]) {
    const hash = `#/connectors/approval/${id}`;
    assert.deepEqual(router.parseRoute(hash), { name: "connector-approval", id }, hash);
    assert.equal(router.formatRoute({ name: "connector-approval", id }), hash);
  }
  for (const bad of ["0", "-1", "07", "1.5", "1e3", "abc", "9007199254740992", "", "7/extra"]) {
    assert.notEqual(router.parseRoute(`#/connectors/approval/${bad}`).name, "connector-approval", bad);
  }
});

test("The app opens the approval page on its route, a fresh page per id", () => {
  const app = source("../src/app.tsx");
  assert.match(app, /case "connector-approval": return <ConnectorApprovalPage key=\{route\.id\} id=\{route\.id\} \/>;/u);
});

test("The approval page's four commands are built, registered and granted, and each call names only its own arguments", () => {
  const lib = source("../src-tauri/src/lib.rs");
  const built = source("../src-tauri/build.rs");
  const granted = JSON.parse(source("../src-tauri/capabilities/main-window.json")).permissions;
  const apiSource = source("../src/api-connectors.ts");
  const calls = {
    desktop_connector_approval: "{ id }",
    desktop_connector_answer: "{ id, allow }",
    desktop_connector_writes: "{ project }",
    desktop_connector_revoke_writes: "{ project, connector }",
  };
  for (const [command, args] of Object.entries(calls)) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`, "u"), command);
    assert.ok(lib.split(command).length >= 3, `${command} is registered with the window`);
    assert.ok(built.includes(`"${command}",`), `${command} is in the app's built commands`);
    assert.ok(granted.includes(`allow-${command.replaceAll("_", "-")}`), `${command} is granted to the main window`);
    assert.ok(apiSource.includes(`("${command}", ${args})`), `${command} is called with ${args}`);
  }
});

test("The approval refusals have their sentences", () => {
  assert.equal(api.connectorProblem({ code: "connector_approval_unknown", message: "Something went wrong. Try again." }), "That approval is gone.");
  assert.equal(api.connectorProblem({ code: "connector_approval_closed", message: "Something went wrong. Try again." }),
    "This write was already answered or its wait ran out. The page now shows where it stands.");
});

async function bridged(reply, run) {
  const sent = [];
  const before = globalThis.window;
  globalThis.window = { __TAURI_INTERNALS__: { async invoke(command, args) {
    sent.push([command, args]);
    return reply;
  } } };
  try {
    await run(sent);
  } finally {
    globalThis.window = before;
  }
}

test("An approval read shows every agent- and manifest-supplied string with its hidden characters as escapes, and sends only the id", async () => {
  const sneaky = "Bo‮ard​";
  const escaped = "Bo\\u202eard\\u200b";
  await bridged({ ...WAITING, title: sneaky, tool: sneaky, agent: sneaky, project_name: sneaky, preview: sneaky }, async (sent) => {
    const result = await api.loadApproval(7);
    assert.equal(result.kind, "ok");
    for (const field of ["title", "tool", "agent", "project_name", "preview"]) assert.equal(result.value[field], escaped, field);
    assert.deepEqual(sent, [["desktop_connector_approval", { id: 7 }]]);
    const markup = renderToStaticMarkup(createElement(approval.ApprovalPageView, { view: result, now: CREATED, busy: false, failure: "", actions }));
    for (const hidden of ["‮", "​"]) assert.ok(!markup.includes(hidden), JSON.stringify(hidden));
  });
});

test("An approval shows a typed backslash in its title, tool, agent and preview doubled, so a typed escape never looks like the hidden character it names", async () => {
  const typed = "Bo\\u202eard";
  const doubled = "Bo\\\\u202eard";
  assert.equal(api.approvalText(typed), doubled);
  assert.equal(api.approvalText("Bo‮ard"), typed, "a hidden character is shown as its escape");
  assert.notEqual(api.approvalText(typed), api.approvalText("Bo‮ard"), "a typed escape and the hidden character never show alike");
  await bridged({ ...WAITING, title: typed, tool: typed, agent: typed, preview: typed, project_name: "Al\\pha", arguments: "{\"t\": \"a\\\\b\"}" }, async () => {
    const result = await api.loadApproval(7);
    assert.equal(result.kind, "ok");
    for (const field of ["title", "tool", "agent", "preview"]) assert.equal(result.value[field], doubled, field);
    assert.equal(result.value.project_name, "Al\\pha", "the owner's own project name is shown as typed");
    assert.equal(result.value.arguments, "{\"t\": \"a\\\\b\"}", "the engine already doubled the arguments' backslashes");
    const markup = renderToStaticMarkup(createElement(approval.ApprovalPageView, { view: result, now: CREATED, busy: false, failure: "", actions }));
    assert.ok(text(markup).includes(`Connector: ${doubled}`), text(markup));
  });
  await bridged({ ...WAITING, state: "allowed", answered_ms: CREATED, title: typed }, async () => {
    const result = await api.answerApproval(7, true);
    assert.equal(result.kind, "ok");
    assert.equal(result.value.title, doubled, "an answer's reply is shown the same way");
  });
});
