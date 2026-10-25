import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const graph = await import("../src/screens/team/graph.ts");
const io = await import("../src/screens/team/terminal-io.ts");
const view = await import("../src/screens/team/index.tsx");

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: null, ...extra,
});
const edge = (task_id, from, to, state, extra = {}) => ({ from, to, task_id, source: "master", text: `Task ${task_id}`, state, reply: null, ...extra });

const team = {
  project_id: "chores",
  nodes: [
    node("team-2", "agent", { parent: "g-codex", label: "codex-2", status: "working", status_label: "Working", account: "Work ChatGPT", terminal: "team-2" }),
    node("g-codex", "harness_group", { parent: "m", label: "Codex", agents: 2 }),
    node("m", "master", { label: "Master", status: "idle", terminal: "team-9" }),
    node("team-1", "agent", { parent: "g-codex", label: "codex-1", status: "idle", terminal: "team-1" }),
    node("g-claude", "harness_group", { parent: "m", label: "Claude Code", harness: "claude", agents: 0 }),
  ],
  edges: [
    edge(3, "m", "team-1", "done", { reply: "Wrote chores.md" }),
    edge(7, "m", "team-2", "running"),
    edge(8, "m", "g-claude", "sent", { source: "owner" }),
  ],
};

const at = (layout, id) => {
  const placed = layout.nodes.find((item) => item.node.id === id);
  assert.ok(placed, `${id} is placed`);
  return { x: placed.x, y: placed.y };
};

test("the graph lays out the master on top, harness groups under it and agents under their group", () => {
  const layout = graph.layoutGraph(team);
  const column = (index) => graph.MARGIN + index * (graph.NODE_WIDTH + graph.COLUMN_GAP);
  const row = (index) => graph.MARGIN + index * (graph.NODE_HEIGHT + graph.ROW_GAP);
  assert.deepEqual(at(layout, "g-claude"), { x: column(0), y: row(1) });
  assert.deepEqual(at(layout, "team-1"), { x: column(1), y: row(2) });
  assert.deepEqual(at(layout, "team-2"), { x: column(2), y: row(2) });
  assert.deepEqual(at(layout, "g-codex"), { x: (column(1) + column(2)) / 2, y: row(1) });
  assert.deepEqual(at(layout, "m"), { x: column(1), y: row(0) });
  assert.equal(layout.nodes.length, 5);
  assert.equal(layout.width, column(2) + graph.NODE_WIDTH + graph.MARGIN);
  assert.equal(layout.height, row(2) + graph.NODE_HEIGHT + graph.MARGIN);
  assert.deepEqual(layout.links.map((link) => `${link.from}>${link.to}`).sort(), ["g-codex>team-1", "g-codex>team-2", "m>g-claude", "m>g-codex"]);
  const masterBottom = { x: column(1) + graph.NODE_WIDTH / 2, y: row(0) + graph.NODE_HEIGHT };
  const claudeTop = { x: column(0) + graph.NODE_WIDTH / 2, y: row(1) };
  const middle = (masterBottom.y + claudeTop.y) / 2;
  assert.equal(layout.links.find((link) => link.to === "g-claude").path,
    `M ${masterBottom.x} ${masterBottom.y} C ${masterBottom.x} ${middle}, ${claudeTop.x} ${middle}, ${claudeTop.x} ${claudeTop.y}`);
});

test("live task edges come first, newest first, with labels halfway along each edge", () => {
  const layout = graph.layoutGraph(team);
  assert.deepEqual(layout.edges.map((item) => item.edge.task_id), [8, 7, 3]);
  const sent = layout.edges[0];
  const column = (index) => graph.MARGIN + index * (graph.NODE_WIDTH + graph.COLUMN_GAP);
  assert.deepEqual(sent.label, {
    x: (column(1) + column(0)) / 2 + graph.NODE_WIDTH / 2,
    y: (graph.MARGIN + graph.NODE_HEIGHT + graph.MARGIN + graph.NODE_HEIGHT + graph.ROW_GAP) / 2,
  });
  const twice = graph.layoutGraph({ ...team, edges: [edge(11, "m", "team-1", "running"), edge(12, "m", "team-1", "sent")] });
  assert.equal(twice.edges[1].label.y - twice.edges[0].label.y, graph.LABEL_STACK);
  const lost = graph.layoutGraph({ ...team, edges: [edge(13, "m", "team-404", "running")] });
  assert.deepEqual(lost.edges, []);
  const waiting = graph.layoutGraph({ ...team, edges: [edge(14, "m", "m", "sent"), edge(15, "m", "m", "sent")] });
  const master = at(waiting, "m");
  assert.deepEqual(waiting.edges.map((item) => item.path), ["", ""]);
  assert.deepEqual(waiting.edges.map((item) => item.label), [
    { x: master.x + graph.NODE_WIDTH / 2, y: master.y + graph.NODE_HEIGHT + graph.LABEL_STACK },
    { x: master.x + graph.NODE_WIDTH / 2, y: master.y + graph.NODE_HEIGHT + graph.LABEL_STACK * 2 },
  ]);
});

test("only a bounded number of edges and nodes are drawn", () => {
  const many = Array.from({ length: graph.EDGES_SHOWN_MAX + 6 }, (_, index) => edge(index + 1, "m", "team-1", index % 2 ? "done" : "running"));
  const shown = graph.shownEdges(many);
  assert.equal(shown.length, graph.EDGES_SHOWN_MAX);
  assert.deepEqual(shown.map((item) => item.state), [...Array(15).fill("running"), ...Array(9).fill("done")]);
  assert.deepEqual(shown.slice(0, 2).map((item) => item.task_id), [29, 27]);
  assert.deepEqual(shown.slice(15, 17).map((item) => item.task_id), [30, 28]);
  const crowd = { project_id: "p", nodes: Array.from({ length: graph.NODES_MAX + 6 }, (_, index) => node(`team-${index + 1}`, "agent")), edges: [] };
  assert.equal(graph.layoutGraph(crowd).nodes.length, graph.NODES_MAX);
});

test("agents without a known group get their own column after the groups", () => {
  const layout = graph.layoutGraph({ ...team, nodes: [...team.nodes, node("research-1", "agent", { parent: null, label: "Market research" })] });
  const column = (index) => graph.MARGIN + index * (graph.NODE_WIDTH + graph.COLUMN_GAP);
  assert.equal(at(layout, "research-1").x, column(3));
  assert.equal(at(layout, "m").x, column(1.5));
});

test("a master alone is laid out in one row", () => {
  const layout = graph.layoutGraph({ project_id: "p", nodes: [node("m", "master")], edges: [] });
  assert.deepEqual(at(layout, "m"), { x: graph.MARGIN, y: graph.MARGIN });
  assert.equal(layout.height, graph.MARGIN * 2 + graph.NODE_HEIGHT);
  assert.equal(layout.width, graph.MARGIN * 2 + graph.NODE_WIDTH);
});

test("dragged nodes move by their offset, stay on the canvas, and the canvas grows to hold them", () => {
  let offsets = new Map();
  offsets = graph.moveNode(offsets, "team-1", -500, 10);
  offsets = graph.moveNode(offsets, "team-2", 300.4, 400.6);
  const layout = graph.layoutGraph(team, offsets);
  const before = graph.layoutGraph(team);
  assert.deepEqual(at(layout, "team-1"), { x: 0, y: at(before, "team-1").y + 10 });
  assert.deepEqual(at(layout, "team-2"), { x: at(before, "team-2").x + 300, y: at(before, "team-2").y + 401 });
  assert.equal(layout.width, at(layout, "team-2").x + graph.NODE_WIDTH + graph.MARGIN);
  assert.equal(layout.height, at(layout, "team-2").y + graph.NODE_HEIGHT + graph.MARGIN);
  assert.ok(layout.links.find((link) => link.to === "team-1").path.endsWith(`${graph.NODE_WIDTH / 2} ${at(layout, "team-1").y}`));
  const far = graph.moveNode(new Map(), "m", 99_999, -99_999);
  assert.deepEqual(far.get("m"), { x: 4000, y: -4000 });
  const again = graph.moveNode(offsets, "team-1", 4, 0);
  assert.deepEqual(offsets.get("team-1"), { x: -500, y: 10 });
  assert.deepEqual(again.get("team-1"), { x: -496, y: 10 });
});

test("offsets of nodes that left the graph are dropped", () => {
  const offsets = new Map([["team-1", { x: 5, y: 5 }]]);
  assert.equal(graph.keepOffsets(offsets, team), offsets);
  const gone = graph.keepOffsets(new Map([["team-1", { x: 5, y: 5 }], ["team-77", { x: 1, y: 1 }]]), team);
  assert.deepEqual([...gone.keys()], ["team-1"]);
});

test("arrow keys move a focused node, Shift moves it further", () => {
  assert.deepEqual(graph.arrowMove("ArrowLeft", false), { x: -graph.ARROW_STEP, y: 0 });
  assert.deepEqual(graph.arrowMove("ArrowDown", true), { x: 0, y: graph.ARROW_STEP_LARGE });
  assert.equal(graph.arrowMove("Enter", false), null);
});

test("nodes and edges use the engine's status words with fixed tones", () => {
  assert.deepEqual(graph.nodeStatus(team.nodes[0]), { word: "Working", tone: "info" });
  assert.deepEqual(graph.nodeStatus(node("a", "agent", { status: "failed", status_label: " " })), { word: "Failed", tone: "danger" });
  assert.equal(graph.nodeStatus(node("g", "harness_group")), null);
  assert.deepEqual(graph.edgeStatus(team.edges[0]), { word: "Done", tone: "ok" });
  assert.deepEqual(graph.edgeStatus(team.edges[1]), { word: "Running", tone: "info" });
  const name = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  assert.equal(graph.nodeDetail(team.nodes[2], name), "Master, Codex");
  assert.equal(graph.nodeDetail(team.nodes[1], name), "Codex, 2 agents");
  assert.equal(graph.nodeDetail(node("g", "harness_group", { agents: 1 })), "codex, 1 agent");
  assert.equal(graph.nodeDetail(node("g", "harness_group", { agents: 1, status_label: "1 of 2 agents" }), name), "Codex, 1 of 2 agents");
  assert.equal(graph.nodeDetail(team.nodes[0], name), "Codex, Work ChatGPT");
  assert.equal(graph.nodeDetail(team.nodes[3], name), "Codex");
});

test("the graph reads again quickly only while work is live", () => {
  assert.equal(graph.graphLive(team), true);
  const quiet = { ...team, nodes: team.nodes.map((item) => ({ ...item, status: item.status && "idle" })), edges: [edge(3, "m", "team-1", "done")] };
  assert.equal(graph.graphLive(quiet), false);
  assert.equal(graph.graphSummary(team), "1 of 2 agents working. 2 tasks live.");
  assert.equal(graph.graphSummary(quiet), "0 of 2 agents working. 0 tasks live.");
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
  const send = async (chunk) => {
    inFlight += 1;
    assert.equal(inFlight, 1);
    await new Promise((resolve) => setTimeout(resolve, chunk.length > 1 ? 5 : 0));
    sent.push(chunk.length === 1 ? String.fromCharCode(chunk[0]) : chunk.length);
    inFlight -= 1;
    return true;
  };
  let stops = 0;
  const type = io.inputQueue(send, () => { stops += 1; });
  type(io.encodeTyped("x".repeat(5000)));
  type(io.encodeTyped("b"));
  type(io.encodeTyped("\r"));
  await new Promise((resolve) => setTimeout(resolve, 60));
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

test("edge and node labels read as plain sentences", () => {
  const layout = graph.layoutGraph({ ...team, edges: [edge(4, "m", "team-1", "running", { text: "Write   the chores list\nfor every helper in the house" })] });
  assert.equal(view.edgeLabel(layout.edges[0]), "Running: Write the chores list for e...");
  assert.equal(view.edgeLabel(graph.layoutGraph(team).edges[2]), "Done: Task 3");
  assert.equal(view.nodeLabel(team.nodes[0]), "codex-2, codex, Work ChatGPT, Working. Opens its terminal. Arrow keys move it.");
  assert.equal(view.nodeLabel(team.nodes[1]), "Codex, codex, 2 agents. Arrow keys move it.");
});

test("the terminal panel names the agent, says it is connecting and offers Close", () => {
  const html = renderToStaticMarkup(createElement(view.TerminalPanel, { node: team.nodes[0], onClose() {} }));
  assert.match(html, /aria-label="codex-2 terminal"/);
  assert.match(html, /aria-label="Close terminal"/);
  assert.match(html, /Connecting to the terminal\.\.\./);
  assert.match(html, /Working/);
  const none = renderToStaticMarkup(createElement(view.TerminalPanel, { node: node("team-5", "agent"), onClose() {} }));
  assert.match(none, /This agent has no terminal yet/);
  assert.doesNotMatch(none, /Connecting/);
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
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.deepEqual(sent, [["keys", 4096], ["keys", 904], ["prompt", "run the tests"], ["keys", 1]]);
  assert.equal(stops, 0);
  const halted = io.jobQueue(() => { stops += 1; });
  assert.equal(halted(async () => false), true);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(halted(async () => true), false);
  assert.equal(stops, 1);
});

test("the note under a live terminal says typed keys are not kept or told to the master", async () => {
  const panel = await import("../src/screens/team/terminal-panel.tsx");
  assert.equal(panel.typingNote(team.nodes[2]), "Typing goes straight to the master.");
  assert.equal(panel.typingNote(team.nodes[0]),
    "Typing goes straight to this agent and is not kept. Only a prompt you send below is also told to its master.");
});

test("the composer names who reads the prompt and says a sub-agent's master is told", () => {
  const panel = (target) => renderToStaticMarkup(createElement(view.TerminalPanel, { node: target, onClose() {} }));
  const sub = panel(team.nodes[0]);
  assert.match(sub, /<label for="tg-prompt-team-2" class="pj-sr">Prompt codex-2<\/label>/);
  assert.match(sub, /placeholder="Tell codex-2 what to do\. Its master is told what you send\."/);
  assert.match(sub, /<textarea id="tg-prompt-team-2"[^>]* disabled=""/);
  assert.match(sub, /Opens once the terminal is live\./);
  assert.match(sub, /<button type="submit" class="btn primary small" disabled=""/);
  const master = panel(team.nodes[2]);
  assert.match(master, /Prompt the master/);
  assert.doesNotMatch(master, /Its master is told/);
  assert.doesNotMatch(panel(node("team-5", "agent")), /tg-composer/);
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
