import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const graph = await import("../src/screens/team/graph.ts");
const io = await import("../src/screens/team/terminal-io.ts");
const view = await import("../src/screens/team/index.tsx");

/** Renders with the owner's Terminal input saved as Both, as a render with no window reads the
 * default, Terminal only. Input: the render. Output: what it returns. */
function withBox(render) {
  globalThis.window = { localStorage: { getItem: () => "both" } };
  try {
    return render();
  } finally {
    delete globalThis.window;
  }
}

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, terminal: null, ...extra,
});
const edge = (task_id, from, to, state, extra = {}) => ({ from, to, task_id, source: "master", text: `Task ${task_id}`, state, reply: null, ...extra });

const team = {
  project_id: "chores",
  nodes: [
    node("team-2", "sub", { parent: "team-4", label: "codex-2", status: "working", status_label: "Working", account: "acct-work", terminal: "team-2", agent: 2 }),
    node("teammate-4-12", "teammate", { parent: "team-4", label: "Teammate %12", harness: "claude", status_label: "Running", terminal: "team-4", agent: null, pane: "%12" }),
    node("m", "master", { label: "Master", status: "idle", terminal: "team-9", agent: 9 }),
    node("team-1", "lead", { parent: "m", label: "codex-1", status: "idle", terminal: "team-1", agent: 1 }),
    node("team-4", "lead", { parent: "m", label: "claude-4", harness: "claude", status: "idle", terminal: "team-4", agent: 4 }),
  ],
  edges: [
    edge(3, "m", "team-1", "done", { reply: "Wrote chores.md" }),
    edge(7, "m", "team-2", "running"),
    edge(8, "m", "m", "sent", { source: "owner" }),
  ],
};

const named = graph.namedAccounts(team, (id) => ({ "acct-work": "Work ChatGPT" })[id] ?? "An account in the pool");

test("graph nodes name their account as the Team panel does, never by its id", () => {
  assert.deepEqual(named.nodes.map((item) => item.account), ["Work ChatGPT", null, null, null, null]);
  assert.equal(team.nodes[0].account, "acct-work", "the engine's graph is left as it was");
  assert.deepEqual(graph.namedAccounts(team, () => "x").edges, team.edges);
  assert.equal(view.nodeLabel(named.nodes[0]), "codex-2, Sub-agent, codex, Work ChatGPT, Working. No task yet. Opens its terminal.");
});

test("nodes and edges use the engine's status words with fixed tones", () => {
  assert.deepEqual(graph.nodeStatus(team.nodes[0]), { word: "Working", tone: "info" });
  assert.deepEqual(graph.nodeStatus(node("a", "sub", { status: "failed", status_label: " " })), { word: "Failed", tone: "danger" });
  assert.deepEqual(graph.nodeStatus(team.nodes[1]), { word: "Running", tone: "info" }, "a pane teammate has a label and no status");
  assert.equal(graph.nodeStatus(node("t", "teammate", { status_label: "  " })), null);
  assert.deepEqual(graph.edgeStatus(team.edges[0]), { word: "Done", tone: "ok" });
  assert.deepEqual(graph.edgeStatus(team.edges[1]), { word: "Running", tone: "info" });
  const name = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  assert.equal(graph.nodeDetail(team.nodes[2], name), "Master, Codex");
  assert.equal(graph.nodeDetail(team.nodes[1], name), "Teammate, Claude Code");
  assert.equal(graph.nodeDetail(team.nodes[0], name), "Sub-agent, Codex, acct-work");
  assert.equal(graph.nodeDetail(named.nodes[0], name), "Sub-agent, Codex, Work ChatGPT");
  assert.equal(graph.nodeDetail(team.nodes[3], name), "Lead, Codex");
  assert.equal(graph.nodeDetail(team.nodes[4]), "Lead, claude");
  assert.equal(graph.nodeDetail(node("x", "someday", { account: "  " })), "Agent, codex", "an unknown kind from a newer engine still reads");
  const ada = node("team-7", "sub", { label: "Ada (@onboarding-fix)", harness: "claude", status: "working", status_label: "Working" });
  assert.equal(graph.nodeRole(ada), "Sub-agent");
  assert.equal(graph.cardText(ada, [], [], name).title, "Ada (@onboarding-fix)");
  assert.equal(graph.cardText(ada, [], [], name).branch, null);
  assert.equal(graph.cardText({ ...ada, branch: " crazyagent/agent-7 " }, [], [], name).branch, "crazyagent/agent-7");
  assert.equal(graph.cardTip(ada, { project_id: "p", nodes: [ada], edges: [] }, name)[0], "Ada (@onboarding-fix)");
  assert.equal(graph.nodeRole(team.nodes[2]), "Master");
  assert.equal(graph.nodeRole(node("x", "someday")), "Agent", "an unknown kind from a newer engine still reads");
  const now = 1_700_000_000_000;
  assert.equal(graph.nodeRunning({ started_ms: now - 45_000 }, now), "running 45 s");
  assert.equal(graph.nodeRunning({ started_ms: now - 125 * 60_000 }, now), "running 2 h 5 min");
  assert.equal(graph.nodeRunning({ started_ms: now + 5_000 }, now), "running 0 s", "a clock behind the engine's never says a negative time");
  assert.equal(graph.nodeRunning({ started_ms: null }, now), null);
  assert.equal(graph.nodeRunning({}, now), null);
  assert.throws(() => graph.nodeRunning({ started_ms: 1.5 }, now), /whole ms/);
  assert.throws(() => graph.nodeRunning({ started_ms: now }, Number.NaN), /now is a time/);
});

test("each graph line names its relation, and the live action while it happens", () => {
  const pair = (relation, live = null) => ({ from: "m", to: "team-1", relation, live });
  assert.equal(graph.edgeLabel(pair("leads")), "leads");
  assert.equal(graph.edgeLabel(pair("delegated_to")), "delegated to");
  assert.equal(graph.edgeLabel(pair("teammate_of")), "teammate of");
  assert.equal(graph.edgeLabel(pair("delegated_to", { action: "handing_off", task: "\n  Fix\tthe   login bug\nThe token expires early." })), "handing off: Fix the login bug",
    "a handoff names the first line of its task, as the TUI does");
  assert.equal(graph.edgeLabel(pair("leads", { action: "handing_off", task: " \n\t " })), "handing off: Untitled task");
  assert.equal(graph.edgeLabel(pair("leads", { action: "messaging" })), "messaging");
  assert.equal(graph.edgeLabel(pair("delegated_to", { action: "waiting_on", owner: false })), "waiting on");
  assert.equal(graph.edgeLabel(pair("delegated_to", { action: "waiting_on", owner: true })), "waiting on: your answer");
  assert.equal(graph.edgeLabel(pair("leads", { action: "reviewing", agent: "Wren (@checkout)" })), "reviewing: Wren (@checkout)'s change set",
    "a review names the agent whose change set it reviews, as its card names it");
  assert.equal(graph.edgeLabel(pair("leads", { action: "reviewing", agent: " Wren\n\t(@checkout) " })), "reviewing: Wren (@checkout)'s change set");
  assert.equal(graph.edgeLabel(pair("leads", { action: "reviewing", agent: " \n " })), "reviewing: change set", "a blank name is not named");
  assert.equal(graph.edgeLabel(pair("leads", { action: "reviewing", change_set: 16 })), "reviewing: change set", "a number from an older engine is never shown");
  assert.equal(graph.edgeLabel(pair("leads", { action: "someday" })), "leads", "a live action from a newer engine falls back to the relation");
  assert.equal(graph.edgeLabel(pair("someday")), "", "a relation from a newer engine draws no label");
});

test("the graph reads again quickly only while work is live", () => {
  assert.equal(graph.graphLive(team), true);
  const quiet = { ...team, nodes: team.nodes.map((item) => ({ ...item, status: item.status && "idle" })), edges: [edge(3, "m", "team-1", "done")] };
  assert.equal(graph.graphLive(quiet), false);
  assert.equal(graph.graphSummary(team), "1 of 4 agents working. 2 tasks live.");
  assert.equal(graph.graphSummary(quiet), "0 of 4 agents working. 0 tasks live.");
  const asking = { ...team, nodes: team.nodes.map((item) => item.id === "team-1" ? { ...item, waiting: "permission" } : item.id === "m" ? { ...item, owner_questions: 2 } : item) };
  assert.equal(graph.graphSummary(asking, 3), "1 of 4 agents working. 2 tasks live. 2 waiting on you. 3 agents not shown.");
});

test("the Agents toolbar's team line counts the master's agents as the mockup words it", () => {
  assert.equal(graph.teamLine(team), "Master and 4 agents");
  assert.equal(graph.teamLine(team, 3), "Master and 4 agents · 3 not shown");
  assert.equal(graph.teamLine({ ...team, nodes: team.nodes.filter((item) => item.kind === "master" || item.id === "team-1") }), "Master and 1 agent");
  assert.equal(graph.teamLine({ ...team, nodes: team.nodes.filter((item) => item.kind === "master") }), "Master, no agents yet");
  assert.throws(() => graph.teamLine(team, 5), /hides 5 of 4 agents/);
  assert.throws(() => graph.teamLine(team, -1), /whole number/);
});

test("the Agents toolbar says how many agents work and how many tasks are live after the team line, and nothing while the team is not read", () => {
  const row = (props) => {
    const markup = renderToStaticMarkup(createElement(view.AgentsToolbar, { projectId: "chores", name: "Chores", perWindow: 6, pool: "Work: 2 accounts", ...props }));
    return markup.slice(markup.indexOf('class="tg-summary"'), markup.indexOf("Open all terminals"));
  };
  const dot = '<span aria-hidden="true"> · </span>';
  assert.match(row({ team, line: graph.teamLine(team) }),
    new RegExp(`<span role="status">Master and 4 agents</span>${dot}<span>1 of 4 agents working\\. 2 tasks live\\.</span>${dot}<span>Work: 2 accounts</span>`));
  assert.match(row({ team, line: graph.teamLine(team, 3) }), /Master and 4 agents · 3 not shown<\/span><span aria-hidden="true"> · <\/span><span>1 of 4 agents working\. 2 tasks live\.<\/span>/,
    "the left-out count stays on the team line only");
  assert.doesNotMatch(row({ team: null, line: null }), /working|live/);
  const alone = { ...team, nodes: team.nodes.filter((item) => item.kind === "master") };
  assert.doesNotMatch(row({ team: alone, line: graph.teamLine(alone) }), /working/, "a team of only the master has no agents to count");
});

test("typed keys reach the agent as the UTF-8 bytes a terminal reads", () => {
  assert.deepEqual([...io.encodeTyped("ls\r")], [108, 115, 13]);
  assert.deepEqual([...io.encodeTyped("\x1b[A")], [27, 91, 65]);
  assert.deepEqual([...io.encodeTyped("\x03")], [3]);
  assert.deepEqual([...io.encodeTyped("é")], [0xc3, 0xa9]);
  assert.deepEqual([...io.encodeTyped("😀")], [0xf0, 0x9f, 0x98, 0x80]);
  assert.deepEqual([...io.encodeBinary("\x1b[M \x80\xff")], [27, 91, 77, 32, 0x80, 0xff]);
  assert.deepEqual([...io.encodeBinary("ā")], [0x01]);
});

test("long input is split into bridge-sized chunks without cutting a character", () => {
  assert.deepEqual(io.inputChunks(new Uint8Array()), []);
  const paste = io.encodeTyped(`${"a".repeat(io.INPUT_BYTES_MAX - 1)}é`);
  const chunks = io.inputChunks(paste);
  assert.deepEqual(chunks.map((chunk) => chunk.length), [io.INPUT_BYTES_MAX - 1, 2]);
  assert.deepEqual(chunks[1], [0xc3, 0xa9]);
  assert.deepEqual(io.inputChunks(io.encodeTyped("a😀"), 4), [[0x61], [0xf0, 0x9f, 0x98, 0x80]]);
  assert.deepEqual(io.inputChunks(io.encodeTyped("😀😀"), 4).map((chunk) => chunk.length), [4, 4]);
  const big = io.inputChunks(io.encodeTyped("x".repeat(10_000)));
  assert.deepEqual(big.map((chunk) => chunk.length), [4096, 4096, 1808]);
  assert.ok(big.every((chunk) => Array.isArray(chunk) && chunk.length <= io.INPUT_BYTES_MAX));
  assert.throws(() => io.inputChunks(paste, 3), /whole UTF-8 character/);
});

test("terminal sizes stay inside the engine limits", () => {
  assert.deepEqual(io.terminalSize(80.9, 24.2), { cols: 80, rows: 24 });
  assert.deepEqual(io.terminalSize(0.5, 5000), { cols: io.SIZE_MIN, rows: io.SIZE_MAX });
  assert.equal(io.terminalSize(Number.NaN, 24), null);
  assert.equal(io.terminalSize(80, Number.POSITIVE_INFINITY), null);
});

test("queued input reaches the agent in order, one chunk at a time", async () => {
  const sent = [];
  let inFlight = 0;
  let allSent;
  const finished = new Promise((resolve) => { allSent = resolve; });
  const send = async (chunk) => {
    inFlight += 1;
    assert.equal(inFlight, 1);
    await new Promise((resolve) => setTimeout(resolve, chunk.length > 1 ? 5 : 0));
    sent.push(chunk.length === 1 ? String.fromCharCode(chunk[0]) : chunk.length);
    inFlight -= 1;
    if (sent.length === 4) allSent();
    return true;
  };
  let stops = 0;
  const type = io.inputQueue(send, () => { stops += 1; });
  type(io.encodeTyped("x".repeat(5000)));
  type(io.encodeTyped("b"));
  type(io.encodeTyped("\r"));
  let timer;
  await Promise.race([finished, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`only ${JSON.stringify(sent)} reached the agent`)), 5000);
  })]).finally(() => clearTimeout(timer));
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(sent, [4096, 904, "b", "\r"]);
  assert.equal(stops, 0);
});

test("input stops once the bridge stops taking it, and the stop is reported once", async () => {
  const sent = [];
  let stops = 0;
  const type = io.inputQueue(async (chunk) => { sent.push(chunk[0]); return false; }, () => { stops += 1; });
  type(io.encodeTyped("a"));
  type(io.encodeTyped("b"));
  await new Promise((resolve) => setTimeout(resolve, 10));
  type(io.encodeTyped("c"));
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(sent, [97]);
  assert.equal(stops, 1);
  let thrown = 0;
  const failing = io.inputQueue(async () => { throw new Error("bridge gone"); }, () => { thrown += 1; });
  failing(io.encodeTyped("a"));
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(thrown, 1);
});

test("terminal channel messages become output, the attached size, or a plain end", () => {
  const bytes = new TextEncoder().encode("\x1b[1mready\x1b[0m").buffer;
  const output = io.terminalEvent(bytes);
  assert.equal(output.kind, "output");
  assert.equal(new TextDecoder().decode(output.bytes), "\x1b[1mready\x1b[0m");
  assert.deepEqual(io.terminalEvent({ kind: "attached", agent_id: "team-2", cols: 120, rows: 32 }), { kind: "attached", cols: 120, rows: 32 });
  assert.deepEqual(io.terminalEvent({ kind: "rejected", code: "terminal_agent_unknown" }),
    { kind: "ended", copy: "This agent's terminal isn't running. It may have finished or not started yet." });
  assert.deepEqual(io.terminalEvent({ kind: "closed", reason: "exited" }), { kind: "ended", copy: "The agent's app exited." });
  assert.deepEqual(io.terminalEvent({ kind: "closed", reason: "something_new" }), { kind: "ended", copy: "This terminal closed." });
});

test("the terminal panel names the agent, says it is connecting and offers Close", () => {
  const html = renderToStaticMarkup(createElement(view.TerminalPanel, { node: team.nodes[0], onClose() {} }));
  assert.match(html, /aria-label="codex-2 terminal"/);
  assert.match(html, /aria-label="Close terminal"/);
  assert.match(html, /Connecting to the terminal\.\.\./);
  assert.match(html, /<span class="tg-terminal-name">codex-2 \u00b7 codex<\/span><span class="badge tone-info">(?:(?!<\/span>)[^])*Working<\/span>/);
  assert.match(html, /<span class="tg-terminal-detail">acct-work<\/span>/);
  const reported = renderToStaticMarkup(createElement(view.TerminalPanel, { node: team.nodes[0], onClose() {}, reported: "Instructions: AGENTS.md - Skills used: tdd" }));
  assert.match(reported, /<span class="tg-terminal-detail" title="Instructions: AGENTS.md - Skills used: tdd">Instructions: AGENTS\.md - Skills used: tdd<\/span>/,
    "the head says what the agent's harness reported, not only the Instructions panel");
  const none = renderToStaticMarkup(createElement(view.TerminalPanel, { node: node("team-5", "sub"), onClose() {} }));
  assert.match(none, /This agent has no terminal yet/);
  assert.doesNotMatch(none, /Connecting/);
});

test("the terminal panel head is the mockup's: name, harness and state, Message, Stop and Close, then role, branch and the swap hint", () => {
  const harness = (id) => (id === "codex" ? "Codex" : id);
  const sub = { ...team.nodes[0], branch: "crazyagent/calendar-api" };
  const html = withBox(() => renderToStaticMarkup(createElement(view.TerminalPanel, { node: sub, harness, onClose() {}, onStop() {}, swaps: true })));
  const head = html.slice(html.indexOf('class="tg-terminal-head"'), html.indexOf('class="tg-terminal-state"'));
  assert.match(head, /<span class="tg-terminal-name">codex-2 · Codex<\/span><span class="badge tone-info">(?:(?!<\/span>)[^])*Working<\/span>/);
  const order = [">Message</button>", ">Stop</button>", 'aria-label="Close terminal"'].map((part) => head.indexOf(part));
  assert.ok(order.every((at) => at > 0), `Message, Stop and Close are in the head: ${JSON.stringify(order)}`);
  assert.deepEqual([...order].sort((a, b) => a - b), order, "Message, then Stop, then Close");
  const metaAt = head.indexOf('class="tg-terminal-meta"');
  assert.ok(metaAt > order[2], "the meta row follows the actions");
  const meta = head.slice(metaAt);
  assert.match(meta, />Sub-agent<\/span>/);
  assert.match(meta, /<span class="tg-terminal-branch">crazyagent\/calendar-api<\/span>/);
  assert.match(meta, /<span class="tg-terminal-detail">acct-work<\/span>/, "the account stays in the head");
  assert.match(meta, />Click another agent to show it here<\/span>/);
  const master = renderToStaticMarkup(createElement(view.TerminalPanel, { node: team.nodes[2], harness, onClose() {} }));
  assert.match(master, /<span class="tg-terminal-name">Master · Codex<\/span>/);
  assert.match(master, />Master<\/span>/);
  assert.doesNotMatch(master, />Stop<\/button>/, "no Stop without a stop action");
  assert.doesNotMatch(master, /Click another agent/, "only the graph's one panel says it swaps");
  assert.doesNotMatch(master, /tg-terminal-branch/, "no branch, no branch part");
  const none = withBox(() => renderToStaticMarkup(createElement(view.TerminalPanel, { node: node("team-5", "sub"), onClose() {} })));
  assert.match(none, /<button type="button" class="btn small" disabled="">(?:(?!<\/button>)[^])*Message<\/button>/, "Message waits for a box to type in");
  assert.doesNotMatch(renderToStaticMarkup(createElement(view.TerminalPanel, { node: sub, harness, onClose() {}, onStop() {}, swaps: true })), />Message<\/button>/, "with no box there is no Message at all");
});

test("the terminal head says how long the agent's task has run, after the branch, only while one runs", async () => {
  const { Icon } = await import("../src/ui/icon.tsx");
  const clock = renderToStaticMarkup(createElement(Icon, { name: "clock", size: 12 }));
  const meta = (extra) => {
    const html = renderToStaticMarkup(createElement(view.TerminalPanel, { node: { ...team.nodes[0], branch: "crazyagent/calendar-api", ...extra }, onClose() {} }));
    return html.slice(html.indexOf('class="tg-terminal-meta"'), html.indexOf('class="tg-terminal-state"'));
  };
  const running = meta({ started_ms: Date.now() - (14 * 60 + 5) * 1000 });
  assert.ok(running.includes(`<span class="tg-terminal-fact">${clock}running 14 min</span>`), running);
  assert.ok(running.indexOf("running 14 min") > running.indexOf("tg-terminal-branch"), "after the branch, as the mockup has it");
  assert.doesNotMatch(meta({ started_ms: null }), /running/, "an idle agent says nothing");
  assert.doesNotMatch(meta({}), /running/, "an engine without started_ms says nothing");
});

test("a terminal embedded in a console never also draws a wall tile's head or the swap hint", () => {
  const render = (props) => renderToStaticMarkup(createElement(view.TerminalPanel, { node: team.nodes[0], ...props }));
  assert.throws(() => render({ embedded: true, tile: true }), /either embedded in a console or a wall tile/);
  assert.throws(() => render({ embedded: true, swaps: true }), /only a terminal with its own head says it swaps/);
  assert.doesNotMatch(render({ embedded: true }), /tg-terminal-head/);
});

test("a failed background read keeps the last graph, so an open terminal stays mounted", () => {
  const ok = { kind: "ok", value: team };
  const busy = { kind: "error", code: "busy", message: "The engine is busy. Try again." };
  let state = view.graphRead(view.GRAPH_UNREAD, ok);
  assert.deepEqual(state, { shown: ok, stale: null, gone: false });
  state = view.graphRead(state, busy);
  assert.equal(state.shown, ok, "the graph that holds the open terminal is still shown");
  assert.equal(state.stale, "The engine is busy. Try again.");
  state = view.graphRead(state, { kind: "not_ready" });
  assert.equal(state.shown, ok);
  assert.equal(state.stale, "The team graph isn't ready in this build.");
  state = view.graphRead(state, ok);
  assert.deepEqual(state, { shown: ok, stale: null, gone: false });
  assert.deepEqual(view.graphRead(view.GRAPH_UNREAD, busy), { shown: busy, stale: null, gone: false }, "with no graph yet the error is the view");
});

test("a read that finds the project removed is marked gone, and no other read is", () => {
  const ok = { kind: "ok", value: team };
  for (const code of ["project_missing", "project_setup_missing"]) {
    const removed = { kind: "error", code, message: "Gone." };
    assert.equal(view.graphRead(view.GRAPH_UNREAD, removed).gone, true, code);
    const after = view.graphRead(view.graphRead(view.GRAPH_UNREAD, ok), removed);
    assert.equal(after.gone, true, `${code} after a good read`);
    assert.equal(after.shown, ok, "the last graph stays on screen");
  }
  assert.equal(view.graphRead(view.GRAPH_UNREAD, ok).gone, false);
  assert.equal(view.graphRead(view.GRAPH_UNREAD, { kind: "not_ready" }).gone, false);
  assert.equal(view.graphRead(view.GRAPH_UNREAD, { kind: "error", code: "owner_read_failed", message: "No." }).gone, false);
});

test("the one terminal panel is kept by the agent's number, so a Questions node and its graph card share it", () => {
  assert.equal(view.panelKey({ id: "team-1", agent: 1 }), "agent:1");
  assert.equal(view.panelKey({ id: "team-1", agent: 1 }), view.panelKey({ id: "card-1", agent: 1 }));
  assert.notEqual(view.panelKey({ id: "team-1", agent: 1 }), view.panelKey({ id: "team-2", agent: 2 }));
  assert.equal(view.panelKey({ id: "pane-3", agent: null }), "node:pane-3");
});

test("the graph view waits for its first read before drawing", () => {
  const html = renderToStaticMarkup(createElement(view.TeamGraphView, { projectId: "chores" }));
  assert.match(html, /aria-busy="true"/);
  assert.match(html, /aria-label="Team graph"/);
});

test("a composer prompt is checked against the engine's limits before it is sent", () => {
  assert.equal(io.PROMPT_BYTES_MAX, 4096);
  assert.deepEqual(io.promptText("  run the tests \n\n"), { kind: "ok", text: "run the tests" });
  assert.deepEqual(io.promptText("first\r\nsecond"), { kind: "ok", text: "first\r\nsecond" });
  assert.deepEqual(io.promptText(" \n "), { kind: "blank" });
  assert.deepEqual(io.promptText("x".repeat(4096)), { kind: "ok", text: "x".repeat(4096) });
  assert.deepEqual(io.promptText("x".repeat(4097)), { kind: "too_long", reason: "Too long: 4097 of 4096 bytes." });
  assert.deepEqual(io.promptText("\u00e9".repeat(2049)), { kind: "too_long", reason: "Too long: 4098 of 4096 bytes." });
});

test("typed keys and a sent prompt go out in order on one queue, and only the prompt is a prompt", async () => {
  const sent = [];
  let stops = 0;
  const run = io.jobQueue(() => { stops += 1; });
  const type = io.keyQueue(run, async (chunk) => {
    await new Promise((resolve) => setTimeout(resolve, chunk.length > 1 ? 5 : 0));
    sent.push(["keys", chunk.length]);
    return true;
  });
  type(io.encodeTyped("x".repeat(5000)));
  assert.equal(run(async () => { sent.push(["prompt", "run the tests"]); return true; }), true);
  type(io.encodeTyped("\r"));
  const settle = async (done) => {
    for (let waited = 0; !done() && waited < 2000; waited += 5) await new Promise((resolve) => setTimeout(resolve, 5));
  };
  await settle(() => sent.length >= 4);
  assert.deepEqual(sent, [["keys", 4096], ["keys", 904], ["prompt", "run the tests"], ["keys", 1]]);
  assert.equal(stops, 0);
  const halted = io.jobQueue(() => { stops += 1; });
  assert.equal(halted(async () => false), true);
  await settle(() => stops === 1);
  assert.equal(halted(async () => true), false);
  assert.equal(stops, 1);
});

test("the note under a live terminal says typed keys are not kept or told to the master", async () => {
  const panel = await import("../src/screens/team/terminal-panel.tsx");
  assert.equal(panel.typingNote(team.nodes[2], true), "Typing goes straight to the master.");
  assert.equal(panel.typingNote(team.nodes[2], false), "Typing goes straight to the master.");
  assert.equal(panel.typingNote(team.nodes[0], true),
    "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master.");
  assert.equal(panel.typingNote(team.nodes[0], false), "Typing goes straight to this agent and is not kept.", "no box, so no word of one");
  assert.equal(panel.typingNote(team.nodes[1], false), "Typing goes straight to this teammate's pane and is not kept or told to the master.");
  assert.equal(panel.teammatePane(team.nodes[1]), "%12");
  assert.equal(panel.teammatePane(team.nodes[0]), null);
  assert.equal(panel.promptCopy(team.nodes[1]).placeholder, "Tell Teammate %12 what to do. It goes to its pane only.");
  assert.equal(panel.typingNote({ ...team.nodes[1], agent: 12 }, true),
    "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master.");
});

test("the composer names who reads the prompt and says a sub-agent's master is told", () => {
  const panel = (target) => withBox(() => renderToStaticMarkup(createElement(view.TerminalPanel, { node: target, onClose() {} })));
  const sub = panel(team.nodes[0]);
  assert.match(sub, /<label for="tg-prompt-team-2" class="pj-sr">Prompt codex-2<\/label>/);
  assert.match(sub, /placeholder="Type to codex-2\.\.\."/);
  assert.match(sub, /Tell codex-2 what to do\. Its master is told what you send\./, "where the prompt goes stays in the box's description");
  assert.doesNotMatch(sub, /<textarea id="tg-prompt-team-2"[^>]* disabled=""/, "the draft box takes typing before the terminal is live");
  assert.match(sub, /Sends once the terminal is live\./);
  assert.match(sub, /<button type="submit" class="btn primary small" disabled=""/);
  const master = panel(team.nodes[2]);
  assert.match(master, /Prompt the master/);
  assert.match(master, /placeholder="Type to Master\.\.\."/);
  assert.doesNotMatch(master, /Its master is told/);
  assert.doesNotMatch(panel(node("team-5", "sub")), /tg-composer/);
});

test("the Agents tab shows the team graph above the team panel", async () => {
  const agents = await import("../src/screens/project/agents.tsx");
  const tab = {
    id: "p-1", name: "Harbor Notes", setup: undefined, research: { kind: "ok", value: { project_id: "p-1", jobs: [] } }, context: undefined,
    pools: undefined, harnesses: undefined, assigned: null, now: 61_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { result: undefined, at: 0 }, reloadTeam: () => {},
  };
  const html = renderToStaticMarkup(createElement(agents.AgentsTab, { data: tab }));
  assert.match(html, /<section class="tg" aria-busy="true" aria-label="Team graph">/);
});

test("an agent waiting for a file under Wait leads its card's task line and its hover with the file", () => {
  const waiting = node("team-5", "sub", { parent: "m", label: "Ada", status: "idle", terminal: "team-5", agent: 5, waits_for: "src/ui/chart.ts" });
  const edges = [edge(11, "m", "team-5", "running", { text: "Draw the chart" })];
  assert.equal(graph.cardTask(waiting, edges), "Waiting for src/ui/chart.ts. Draw the chart");
  assert.equal(graph.cardTask(waiting, []), "Waiting for src/ui/chart.ts. No task yet.");
  assert.equal(graph.cardTask({ ...waiting, waits_for: null }, edges), "Draw the chart", "an agent waiting for nothing shows its task alone");
  assert.equal(graph.cardTask({ ...waiting, waits_for: undefined }, edges), "Draw the chart", "an older engine leaves the field out");
  assert.ok(graph.cardTip(waiting, { ...team, nodes: [...team.nodes, waiting], edges }).includes("Waiting for src/ui/chart.ts. Draw the chart"));
  assert.ok(graph.cardText(waiting, edges, []).task[0].startsWith("Waiting for src/ui/chart.ts."), "the card shows it on its first task line");
});

test("Open all terminals opens nothing and says why when its fresh read finds the project removed (UI-5-F2)", () => {
  assert.equal(typeof view.openAllPlan, "function", "Open all terminals plans its windows from the fresh reads");
  const nodes = Array.from({ length: 13 }, (_, at) => ({ id: `a${at}`, kind: at === 0 ? "master" : "sub", parent: at === 0 ? null : "a0", label: `a${at}`, harness: "codex", status: "working", status_label: "Working", account: null, terminal: `team-${at}` }));
  const read = { kind: "ok", value: { project_id: "alpha", nodes, edges: [] } };
  const six = { kind: "ok", value: 6 };
  for (const code of ["project_missing", "project_setup_missing"]) {
    const plan = view.openAllPlan({ kind: "error", code, message: code }, six, 6, 2);
    assert.equal(plan.windows, 0, `${code}: a removed project opens no window`);
    assert.match(plan.note ?? "", /removed/u, `${code}: the note says why nothing opened`);
  }
  assert.deepEqual(view.openAllPlan({ kind: "error", code: "owner_read_failed", message: "No." }, six, 6, 2), { windows: 2, note: null }, "another failure keeps the shown count");
  assert.deepEqual(view.openAllPlan(read, six, 9, 2), { windows: 3, note: null }, "a good read counts from the team and the setting read now");
  assert.deepEqual(view.openAllPlan(read, { kind: "error", code: "owner_read_failed", message: "No." }, 4, 2), { windows: 4, note: null }, "a failed setting read keeps the given Terminals per window");
});
