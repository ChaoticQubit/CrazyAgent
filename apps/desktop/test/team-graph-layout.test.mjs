import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const graph = await import("../src/screens/team/graph.ts");
const view = await import("../src/screens/team/index.tsx");
const engineClient = await import("../src/engine-client.ts");

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, terminal: null, ...extra,
});
const edge = (task_id, from, to, state, extra = {}) => ({ from, to, task_id, source: "master", text: `Task ${task_id}`, state, reply: null, ...extra });

const team = {
  project_id: "chores",
  nodes: [
    node("m", "master", { label: "Master", status: "idle", terminal: "master", agent: 1 }),
    node("team-2", "lead", { parent: "m", label: "codex-2", status: "working", account: "Work ChatGPT", terminal: "team-2", questions: 2, agent: 2, branch: "crazyagent/agent-2" }),
    node("team-3", "sub", { parent: "team-2", label: "codex-3", status: "idle", terminal: "team-3", agent: 3 }),
    node("team-4", "lead", { parent: "m", label: "claude-4", harness: "claude", status: "idle", terminal: "team-4", agent: 4 }),
    node("teammate-4-12", "teammate", { parent: "team-4", label: "Teammate %12", harness: "claude", status_label: "Running", terminal: "team-4", agent: null, pane: "%12" }),
  ],
  edges: [
    edge(3, "m", "team-3", "done", { reply: "Wrote chores.md" }),
    edge(7, "m", "team-2", "running"),
    edge(8, "m", "m", "sent"),
  ],
};

const talk = {
  ...team,
  messages: [
    { from: "owner", to: "team-3", unread: 2, receipt: "delivered" },
    { from: "team-2", to: "team-3", unread: 1, receipt: "typed" },
    { from: "team-3", to: "team-4", unread: 1, receipt: "sent" },
    { from: "team-3", to: "team-2", unread: 1, receipt: "sent" },
    { from: "engine", to: "team-2", unread: 1, receipt: "delivered" },
    { from: "team-4", to: "team-3", unread: 0, receipt: "read" },
  ],
  overlaps: [
    { from: "team-3", to: "team-4", count: 2, reason: "both claim src/a.rs", stale: false },
    { from: "team-3", to: "gone", count: 1, reason: "x", stale: true },
  ],
};

const card = (layout, id) => {
  const found = graph.layoutCards(layout).find((item) => item.node.id === id);
  assert.ok(found, `${id} is placed`);
  return found;
};

const overlaps = (a, b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
const inside = (inner, outer) => inner.x >= outer.x && inner.y >= outer.y
  && inner.x + inner.width <= outer.x + outer.width && inner.y + inner.height <= outer.y + outer.height;
const badgeBox = (link) => ({ x: link.badge.x - graph.BADGE_WIDTH / 2, y: link.badge.y - graph.BADGE_HEIGHT / 2, width: graph.BADGE_WIDTH, height: graph.BADGE_HEIGHT });

function assertApart(boxes, what) {
  for (let a = 0; a < boxes.length; a += 1) {
    for (let b = a + 1; b < boxes.length; b += 1) assert.ok(!overlaps(boxes[a], boxes[b]), `${what} ${a} and ${b} overlap`);
  }
}

function crowd(leads, subs, deep) {
  const nodes = [node("m", "master", { label: "Master", terminal: "master", agent: 1 })];
  let next = 2;
  const add = (kind, parent, extra) => {
    const id = `team-${next}`;
    nodes.push(node(id, kind, { parent, agent: next, terminal: id, status: "working", ...extra }));
    next += 1;
    return id;
  };
  for (let l = 0; l < leads; l += 1) {
    const lead = add("lead", "m", { questions: l % 3, label: `Persona${l} (@a-really-long-role-handle-${l})` });
    for (let s = 0; s < subs; s += 1) {
      const sub = add("sub", lead, { questions: s % 2 });
      if (s < deep) add("sub", sub, { status: "idle" });
    }
  }
  const edges = nodes.slice(1).map((item, index) => edge(index + 1, "m", item.id, "running", { text: "word ".repeat(200) }));
  return { project_id: "p", nodes, edges };
}

test("the master sits on top and each agent hangs under the one who leads it", () => {
  const layout = graph.layoutGraph(team);
  assert.deepEqual(graph.layoutCards(layout).map((item) => item.node.id), ["m", "team-2", "team-3", "team-4", "teammate-4-12"], "leads come before those they lead");
  const row = (depth) => graph.MARGIN + depth * (graph.CARD_HEIGHT + graph.LEVEL_GAP);
  const column = (index) => graph.MARGIN + index * (graph.CARD_WIDTH + graph.CARD_GAP);
  assert.deepEqual([card(layout, "m").x, card(layout, "m").y], [graph.MARGIN + (graph.CARD_WIDTH + graph.CARD_GAP) / 2, row(0)]);
  assert.deepEqual([card(layout, "team-2").x, card(layout, "team-2").y], [column(0), row(1)]);
  assert.deepEqual([card(layout, "team-4").x, card(layout, "team-4").y], [column(1), row(1)]);
  assert.deepEqual([card(layout, "team-3").x, card(layout, "team-3").y], [column(0), row(2)]);
  assert.deepEqual([card(layout, "teammate-4-12").x, card(layout, "teammate-4-12").y], [column(1), row(2)]);
  assert.equal(layout.width, column(1) + graph.CARD_WIDTH + graph.MARGIN);
  assert.equal(layout.height, row(2) + graph.CARD_HEIGHT + graph.MARGIN);
  assert.equal(layout.hidden, 0);
  assert.deepEqual([layout.messages, layout.overlaps], [[], []]);
  assert.ok(graph.layoutCards(layout).every((item) => item.width === graph.CARD_WIDTH && item.height === graph.CARD_HEIGHT));
});

test("solid links run from each lead to those it leads and carry live tasks, questions and unread messages", () => {
  const layout = graph.layoutGraph(team);
  assert.deepEqual(layout.links.map((link) => [link.from, link.to, link.tasks, link.questions, link.unread, link.receipt]), [
    ["team-2", "team-3", 0, 0, 0, null],
    ["m", "team-2", 1, 2, 0, null],
    ["team-4", "teammate-4-12", 0, 0, 0, null],
    ["m", "team-4", 0, 0, 0, null],
  ]);
  const master = card(layout, "m");
  const lead = card(layout, "team-2");
  const from = { x: master.x + graph.CARD_WIDTH / 2, y: master.y + graph.CARD_HEIGHT };
  const bus = from.y + graph.LEVEL_GAP / 2;
  const to = { x: lead.x + graph.CARD_WIDTH / 2, y: lead.y };
  assert.equal(layout.links[1].path, `M ${from.x} ${from.y} V ${bus} H ${to.x} V ${to.y}`);
  assert.deepEqual(layout.links[1].badge, { x: to.x, y: (bus + to.y) / 2 });
  assert.equal(graph.badgeLabel(layout.links[1]), "1 live task, 2 questions waiting");
  assert.equal(graph.badgeLabel({ tasks: 3, questions: 1 }), "3 live tasks, 1 question waiting");
  assert.equal(graph.badgeLabel({ tasks: 0, questions: 0 }), "");
  assert.equal(graph.badgeLabel({ tasks: 0, questions: 0, unread: 2, receipt: "delivered" }), "2 unread messages, delivered");
  const bad = graph.layoutGraph({ ...team, nodes: team.nodes.map((item) => ({ ...item, questions: item.id === "team-2" ? -4 : 1.5 })) });
  assert.deepEqual(bad.links.map((link) => link.questions), [0, 0, 0, 0]);
});

test("each link shows its pair's label first in its badge row, live while an action happens", () => {
  const related = {
    ...team,
    relations: [
      { from: "m", to: "team-2", relation: "leads", live: { action: "handing_off", task: "Task 7" } },
      { from: "team-2", to: "team-3", relation: "teammate_of", live: null },
      { from: "team-4", to: "teammate-4-12", relation: "teammate_of", live: { action: "messaging" } },
      { from: "gone", to: "team-4", relation: "leads", live: null },
    ],
  };
  const layout = graph.layoutGraph(related);
  assert.deepEqual(layout.links.map((link) => [link.from, link.to, link.label, link.live]), [
    ["team-2", "team-3", "teammate of", false],
    ["m", "team-2", "handing off: Task 7", true],
    ["team-4", "teammate-4-12", "messaging", true],
    ["m", "team-4", "", false],
  ], "a pair the engine names from another lead has no label");
  assert.deepEqual(graph.layoutGraph(team).links.map((link) => [link.label, link.live]), [["", false], ["", false], ["", false], ["", false]],
    "an engine that sends no relations draws no labels");
  const live = renderToStaticMarkup(createElement(view.LinkBadges, { link: layout.links[1] }));
  assert.match(live, /aria-label="handing off: Task 7, 1 live task, 2 questions waiting"/);
  assert.match(live, /^<span class="tg-badges live" /);
  assert.match(live, /<span class="tg-edge-label live" style="max-width:352px"><span class="tg-clip">handing off: Task 7<\/span><\/span><span class="tg-badge">/);
  const standing = renderToStaticMarkup(createElement(view.LinkBadges, { link: layout.links[0] }));
  assert.match(standing, /aria-label="teammate of"/);
  assert.match(standing, /^<span class="tg-badges" /);
  assert.match(standing, /<span class="tg-edge-label"><span class="tg-clip">teammate of<\/span><\/span><\/span>$/);
  assert.equal(renderToStaticMarkup(createElement(view.LinkBadges, { link: layout.links[3] })), "", "a link with no label and no counts draws nothing");
  const hostile = graph.layoutGraph({ ...related, relations: [{ from: "m", to: "team-2", relation: "leads", live: { action: "handing_off", task: "Fix‮ it" } }] });
  assert.match(renderToStaticMarkup(createElement(view.LinkBadges, { link: hostile.links[1] })), /handing off: Fix\\u202e it/, "a direction mark in a task shows as an escape");
  assert.equal(graph.BADGE_WIDTH, graph.CARD_WIDTH, "a badge row is as wide as its card, so a standing label never reaches a sibling's row");
  assert.equal(graph.LIVE_LABEL_WIDTH_MAX, 2 * graph.CARD_WIDTH, "a live label may reach past its row, up to two cards wide, drawn above its neighbours while it lasts");
  assertApart(layout.links.map(badgeBox), "badge rows");
  for (const box of layout.links.map(badgeBox)) assert.ok(graph.layoutCards(layout).every((item) => !overlaps(box, item)), "a badge row never covers a card");
  const legend = renderToStaticMarkup(createElement(view.EdgeLegend));
  for (const word of ["leads", "delegated to", "teammate of", "live action, while it happens"]) assert.ok(legend.includes(word), `the legend names ${word}: ${legend}`);
});

test("a line's label pill takes the pointer so its full words show on hover, while the rest of its badge row lets a drag through", () => {
  const css = readFileSync(new URL("../src/screens/team/team-graph.css", import.meta.url), "utf8");
  const rule = (selector) => {
    const at = css.split("\n}").find((block) => block.trim().startsWith(`${selector} {`));
    return at === undefined ? "" : at.slice(at.indexOf("{") + 1);
  };
  assert.match(rule(".tg-badges"), /pointer-events: none;/, "the badge row lets a drag on the canvas through");
  assert.match(rule(".tg-badges .tg-edge-label"), /pointer-events: auto;/, "the label pill under the row takes the pointer, so the row's title shows its full words");
  const handing = { ...team, relations: [{ from: "m", to: "team-2", relation: "leads", live: { action: "handing_off", task: "Task 7" } }] };
  const markup = renderToStaticMarkup(createElement(view.LinkBadges, { link: graph.layoutGraph(handing).links.find((link) => link.to === "team-2") }));
  assert.match(markup, /^<span class="tg-badges live" role="img" aria-label="handing off: Task 7, [^"]+" title="handing off: Task 7, [^"]+"[^>]*><span class="tg-edge-label live" style="max-width:352px">/,
    "the full words stay in the row's aria label and title, over the pill");
});

test("unread messages ride their lead's link or arc dashed, and claim overlaps arc between the two cards", () => {
  const layout = graph.layoutGraph(talk);
  assert.equal(card(layout, "m").y, graph.MARGIN + graph.ARC_RISE, "arcs get room above the master");
  const down = layout.links.find((link) => link.to === "team-3");
  assert.deepEqual([down.unread, down.receipt], [1, "typed"]);
  assert.deepEqual(layout.messages.map((item) => [item.from, item.to]), [["team-3", "team-4"], ["team-3", "team-2"]],
    "the owner and the engine have no card, a lead's own message rides its link, and a read pair draws nothing");
  assert.deepEqual(layout.overlaps.map((item) => [item.from, item.to, item.edge.count]), [["team-3", "team-4", 2]], "an overlap with an agent not drawn has no arc");
  const a = card(layout, "team-3");
  const b = card(layout, "team-4");
  const top = Math.min(a.y, b.y) - graph.ARC_RISE;
  const path = `M ${a.x + graph.CARD_WIDTH / 2} ${a.y} C ${a.x + graph.CARD_WIDTH / 2} ${top}, ${b.x + graph.CARD_WIDTH / 2} ${top}, ${b.x + graph.CARD_WIDTH / 2} ${b.y}`;
  assert.equal(layout.messages[0].path, path);
  assert.equal(layout.overlaps[0].path, path);
  assert.ok(top >= 0);
  assert.deepEqual(graph.fromOwner(card(layout, "team-3").node, talk), { unread: 2, receipt: "delivered" });
  assert.equal(graph.fromOwner(card(layout, "team-2").node, talk), null);
  assert.equal(graph.fromOwner(card(layout, "team-2").node, team), null);
  assert.deepEqual(["sent", "delivered", "read", "failed", "bogus"].map((receipt) => graph.receiptTicks(receipt).ticks), ["\u2713", "\u2713\u2713", "\u2713\u2713", "!", "\u2713"]);
});

test("more than LEAF_COLUMNS_MAX agents that lead nobody wrap into rows fed by a line beside them", () => {
  const leaves = (count) => ({
    project_id: "p",
    nodes: [node("m", "master"), ...Array.from({ length: count }, (_, index) => node(`team-${index + 1}`, "lead", { parent: "m", agent: index + 1 }))],
    edges: [],
  });
  const layout = graph.layoutGraph(leaves(graph.LEAF_COLUMNS_MAX + 2));
  const grid = graph.LEAF_COLUMNS_MAX * graph.CARD_WIDTH + (graph.LEAF_COLUMNS_MAX - 1) * graph.CARD_GAP;
  const master = card(layout, "m");
  assert.equal(master.x, graph.MARGIN + (grid - graph.CARD_WIDTH) / 2);
  const first = graph.MARGIN + graph.CARD_HEIGHT + graph.LEVEL_GAP;
  const second = first + graph.CARD_HEIGHT + graph.ROW_GAP;
  const column = (index) => graph.MARGIN + index * (graph.CARD_WIDTH + graph.CARD_GAP);
  assert.deepEqual(graph.layoutCards(layout).slice(1).map((item) => [item.x, item.y]), [
    [column(0), first], [column(1), first], [column(2), first], [column(3), first], [column(0), second], [column(1), second],
  ]);
  const from = { x: master.x + graph.CARD_WIDTH / 2, y: master.y + graph.CARD_HEIGHT };
  const bus = from.y + graph.LEVEL_GAP / 2;
  const wrapped = layout.links.find((link) => link.to === `team-${graph.LEAF_COLUMNS_MAX + 1}`);
  const side = graph.MARGIN - graph.CARD_GAP / 2;
  const to = { x: column(0) + graph.CARD_WIDTH / 2, y: second };
  assert.equal(wrapped.path, `M ${from.x} ${from.y} V ${bus} H ${side} V ${second - graph.ROW_GAP / 2} H ${to.x} V ${to.y}`);
  assert.deepEqual(wrapped.badge, { x: to.x, y: second - graph.ROW_GAP / 2 });
  assert.equal(layout.width, column(3) + graph.CARD_WIDTH + graph.MARGIN);
  assert.equal(layout.height, second + graph.CARD_HEIGHT + graph.MARGIN);
  const row = graph.layoutGraph(leaves(graph.LEAF_COLUMNS_MAX));
  assert.ok(graph.layoutCards(row).slice(1).every((item) => item.y === first), "up to LEAF_COLUMNS_MAX agents stay in one row");
  assertApart(layout.links.map(badgeBox), "badges");
  for (const box of layout.links.map(badgeBox)) assert.ok(graph.layoutCards(layout).every((item) => !overlaps(box, item)), "a badge never covers a card");
});

test("a master alone is one card, and at most NODES_MAX nodes are drawn", () => {
  const alone = graph.layoutGraph({ project_id: "p", nodes: [node("m", "master")], edges: [] });
  assert.deepEqual([card(alone, "m").x, card(alone, "m").y], [graph.MARGIN, graph.MARGIN]);
  assert.deepEqual([alone.width, alone.height], [graph.MARGIN * 2 + graph.CARD_WIDTH, graph.MARGIN * 2 + graph.CARD_HEIGHT]);
  assert.deepEqual(alone.links, []);
  const many = { project_id: "p", nodes: Array.from({ length: graph.NODES_MAX + 6 }, (_, index) => node(`team-${index + 1}`, "lead")), edges: [] };
  assert.equal(graph.layoutCards(graph.layoutGraph(many)).length, graph.NODES_MAX);
  assert.equal(graph.layoutGraph(many).hidden, 6);
});

test("past NODES_MAX the master stays, and agents waiting on the owner, running, waiting, failed then stopped are drawn newest first", () => {
  const status = (index) => index <= 10 ? "stopped" : index <= 12 ? "failed" : index === 13 ? "stopped" : ["starting", "idle", "working"][index % 3];
  const nodes = [node("m", "master", { label: "Master", status: "working", terminal: "master", agent: 100 })];
  for (let index = 1; index <= 71; index += 1) {
    nodes.push(node(`team-${index}`, "lead", { parent: "m", status: status(index), questions: index === 13 ? 1 : 0, terminal: `team-${index}`, agent: index }));
  }
  const crowded = { project_id: "p", nodes, edges: [] };
  const layout = graph.layoutGraph(crowded);
  const drawn = graph.layoutCards(layout).map((item) => item.node.id);
  assert.equal(drawn.length, graph.NODES_MAX);
  assert.equal(drawn[0], "m");
  assert.equal(layout.hidden, 8);
  const kept = new Set(drawn);
  for (let index = 1; index <= 71; index += 1) assert.equal(kept.has(`team-${index}`), index > 8, `team-${index}`);
  assert.equal(graph.graphSummary(crowded, layout.hidden), "20 of 71 agents working. 0 tasks live. 8 agents not shown.");
  assert.equal(graph.graphSummary(crowded), "20 of 71 agents working. 0 tasks live.");
  const asking = { ...crowded, nodes: nodes.map((item) => item.id === "team-1" ? { ...item, waiting: "question" } : item) };
  const first = new Set(graph.layoutCards(graph.layoutGraph(asking)).map((item) => item.node.id));
  assert.ok(first.has("team-1"), "an agent waiting on the owner is never cut");
  assert.ok(!first.has("team-9") && first.has("team-10"));
});

test("an agent whose lead is not drawn hangs under the nearest drawn lead, and loops or repeats never break the tree", () => {
  const nodes = [node("m", "master", { agent: 1 }), node("team-1", "lead", { parent: "m", status: "stopped", agent: 2 }), node("team-2", "sub", { parent: "team-1", status: "working", agent: 3 })];
  for (let index = 3; index <= graph.NODES_MAX; index += 1) nodes.push(node(`team-${index}`, "lead", { parent: "m", status: "working", agent: index + 1 }));
  const cut = graph.layoutGraph({ project_id: "p", nodes, edges: [] });
  assert.equal(cut.hidden, 1);
  assert.ok(!graph.layoutCards(cut).some((item) => item.node.id === "team-1"));
  assert.equal(cut.links.find((link) => link.to === "team-2").from, "m");
  const loop = [node("team-5", "sub", { parent: "team-6" }), node("team-6", "sub", { parent: "team-5" })];
  const headless = graph.layoutGraph({ project_id: "p", nodes: loop, edges: [] });
  assert.deepEqual(graph.layoutCards(headless).map((item) => item.node.id).sort(), ["team-5", "team-6"]);
  assert.deepEqual(headless.links, []);
  const led = graph.layoutGraph({ project_id: "p", nodes: [node("m", "master"), ...loop, node("team-7", "sub", { parent: "gone" })], edges: [] });
  assert.deepEqual(led.links.map((link) => [link.from, link.to]).sort(), [["m", "team-5"], ["m", "team-6"], ["m", "team-7"]]);
  const twice = graph.layoutGraph({ ...team, nodes: [...team.nodes, node("team-3", "sub", { parent: "team-4" })] });
  assert.equal(graph.layoutCards(twice).length, team.nodes.length);
  assert.equal(twice.links.find((link) => link.to === "team-3").from, "team-2", "the first of two nodes with one id is drawn");
});

for (const [leads, subs, deep] of [[1, 9, 0], [3, 5, 1], [4, 8, 2], [6, 9, 0]]) {
  test(`cards and badges never overlap with ${leads} leads of ${subs} agents, ${deep} of them leading one more`, () => {
    const team = crowd(leads, subs, deep);
    const layout = graph.layoutGraph(team);
    const cards = graph.layoutCards(layout);
    assert.equal(cards.length, team.nodes.length);
    assert.equal(layout.links.length, cards.length - 1);
    assertApart(cards, "cards");
    const badges = layout.links.map(badgeBox);
    assertApart(badges, "badges");
    for (const box of badges) assert.ok(cards.every((item) => !overlaps(box, item)), "a badge never covers a card");
    const canvas = { x: 0, y: 0, width: layout.width, height: layout.height };
    for (const item of [...cards, ...badges]) assert.ok(inside(item, canvas), "everything is on the canvas");
    const master = card(layout, "m");
    assert.ok(cards.every((item) => item === master || item.y > master.y + graph.CARD_HEIGHT), "the master is above everyone");
    assert.equal(layout.links.reduce((sum, link) => sum + link.tasks, 0), team.nodes.length - 1);
  });
}

test("text is wrapped into card lines and clipped with an ellipsis", () => {
  assert.deepEqual(graph.clipLines("Write   the chores list\nfor every helper in the house", 12, 2), ["Write the", "chores li..."]);
  assert.deepEqual(graph.clipLines("Short task", 12, 2), ["Short task"]);
  assert.deepEqual(graph.clipLines("exactly twelve", 14, 1), ["exactly twelve"]);
  assert.deepEqual(graph.clipLines(" \n\t ", 12, 2), []);
  assert.deepEqual(graph.clipLines("abcdefghijklmnopqrstuvwxyz", 10, 2), ["abcdefghij", "klmnopq..."]);
  assert.deepEqual(graph.clipLines("abcdefghijklmnopqrst", 10, 2), ["abcdefghij", "klmnopqrst"]);
  assert.deepEqual(graph.clipLines("go 😀😀😀😀😀😀😀😀😀😀", 6, 2), ["go", "😀😀😀..."]);
  for (const line of graph.clipLines("word ".repeat(1000), graph.LINE_CHARS, graph.TASK_LINES)) {
    assert.ok(Array.from(line).length <= graph.LINE_CHARS);
  }
  assert.throws(() => graph.clipLines("text", 3, 1), /more than the ellipsis/);
  assert.throws(() => graph.clipLines("text", 10, 0), /at least one line/);
});

test("a card shows its title, harness and account, branch and one newest task line, and the full text stays for hover", () => {
  const name = (id) => ({ codex: "Codex", claude: "Claude Code" })[id] ?? id;
  const long = { ...team, edges: [...team.edges, edge(10, "m", "team-3", "running", { text: "Rename every helper in the chores list so the names match the house plan" })] };
  const text = graph.cardText(card(graph.layoutGraph(long), "team-3").node, long.edges, long.nodes, name);
  assert.deepEqual(text, { title: "codex-3", detail: "Codex", branch: null, task: ["Rename every helper in the..."] });
  assert.equal(graph.cardText(team.nodes[1], team.edges, team.nodes, name).branch, "crazyagent/agent-2");
  assert.deepEqual(team.nodes.map((item) => graph.cardText(item, team.edges, team.nodes, name).detail),
    ["Codex", "Codex lead · Work ChatGPT", "Codex", "Claude Code lead", "Teammate of claude-4"], "the mockup's second line: harness and account, a lead's harness says lead, a teammate names its lead");
  assert.equal(graph.cardText(node("t", "teammate", { parent: "gone" }), [], team.nodes, name).detail, "Codex teammate", "a teammate whose lead the graph does not draw names its harness");
  assert.equal(graph.cardTask(node("team-3", "sub"), team.edges), "Done: Task 3");
  assert.equal(graph.cardTask(node("team-5", "sub"), team.edges), "No task yet.");
  assert.equal(graph.cardTask(node("t", "teammate"), []), "Works beside its lead, in its lead's terminal.");
  assert.equal(graph.cardTask(node("m", "master"), team.edges), "2 tasks live, 1 waiting for an agent.");
  assert.equal(graph.cardTask(node("m", "master"), [edge(1, "m", "team-1", "running")]), "1 task live.");
  const title = graph.cardText(node("a", "sub", { label: "Ada Lovelace (@onboarding-fix-for-reused-ids)" }), [], [], name).title;
  assert.equal(title, "Ada Lovelace (@onbo...");
  assert.ok(Array.from(title).length <= graph.TITLE_CHARS);
  assert.deepEqual(graph.cardTip(talk.nodes[2], talk, name), [
    "codex-3", "Sub-agent, Codex", "Idle", "2 unread messages from you, delivered.", "1 unread message from codex-2, typed into its terminal.",
    "Overlap with claude-4: both claim src/a.rs", "Stale overlap with gone: x", "Done: Task 3", "Reply: Wrote chores.md",
  ]);
  assert.deepEqual(graph.cardTip({ ...talk.nodes[1], presence: "live", waiting: "permission" }, talk, name), [
    "codex-2", "Lead, Codex, Work ChatGPT", "Branch crazyagent/agent-2", "Working, Live", "Waiting on you: it asks for permission in its terminal.",
    "1 unread message from codex-3, sent.", "1 unread message from CrazyAgent, delivered.", "Task 7",
  ]);
  const huge = graph.cardTip(node("a", "sub"), { project_id: "p", nodes: [], edges: [edge(1, "m", "a", "running", { text: "x".repeat(5000) })] });
  assert.equal(huge.length, 3);
  assert.equal(Array.from(huge[2]).length, graph.TIP_CHARS);
  assert.ok(huge[2].endsWith("..."));
});

test("a task of an agent the graph does not draw stays its own and counts for no other card", () => {
  const edges = [
    edge(1, "team-2", "team-9", "running", { agent_label: "Alan (@add-fn)" }),
    edge(2, "m", "team-8", "done", { agent_label: "Ken (@readme)" }),
    edge(3, "m", "m", "sent"),
  ];
  const shown = graph.layoutGraph({ ...team, edges });
  assert.ok(shown.cards.every((item) => item.node.id !== "team-9" && item.node.id !== "team-8"));
  assert.equal(graph.cardTask(node("team-2", "lead"), edges), "No task yet.");
  assert.equal(graph.latestTask(node("m", "master"), edges)?.task_id, 3);
  assert.equal(graph.cardTask(node("m", "master"), edges), "2 tasks live, 1 waiting for an agent.");
  assert.equal(shown.links.reduce((sum, link) => sum + link.tasks, 0), 0, "no drawn link carries another agent's live task");
});

test("amber marks an agent waiting on the owner, and a teammate with no worktree of its own", () => {
  assert.equal(graph.waitingOnOwner(node("a", "sub", { waiting: "permission" })).word, "Waiting on you");
  assert.equal(graph.waitingOnOwner(node("a", "sub", { waiting: "question" })).sentence, "Waiting on you: it asks a question in its terminal.");
  assert.equal(graph.waitingOnOwner(node("m", "master", { owner_questions: 3 })).sentence, "Waiting on you: 3 questions for you in Questions.");
  assert.equal(graph.waitingOnOwner(node("m", "master", { owner_questions: -1 })), null);
  assert.equal(graph.waitingOnOwner(node("t", "teammate", { no_worktree: true })).word, "No worktree");
  assert.equal(graph.waitingOnOwner(node("a", "sub", { no_worktree: true })), null, "only a native teammate shares its lead's worktree");
  assert.equal(graph.dotTone(node("t", "teammate", { no_worktree: true, presence: "live" })), "warn");
  assert.equal(graph.dotTone(node("a", "sub", { presence: "live", status: "working" })), "ok");
  assert.equal(graph.dotTone(node("a", "sub", { presence: "away", status: "working" })), "neutral");
  assert.equal(graph.dotTone(node("a", "sub", { status: "failed" })), "danger");
  assert.equal(graph.dotTone(node("a", "sub", { presence: "nonsense", status: "working" })), "info");
  assert.equal(graph.presenceWord(node("a", "sub", { presence: "idle" })), "Idle");
  assert.equal(graph.presenceWord(node("a", "sub")), null);
});

test("a card's menu messages its agent, stops it or its teammate pane, and merges a lead's or sub-agent's work", () => {
  const acts = (target, nodes = team.nodes) => graph.nodeActions(target, nodes);
  const byId = (id) => team.nodes.find((item) => item.id === id);
  assert.deepEqual(acts(byId("m")), { message: 1, stop: null, merge: null });
  assert.deepEqual(acts(byId("team-2")), { message: 2, stop: { kind: "agent", agent: 2 }, merge: 2 });
  assert.deepEqual(acts(byId("team-3")), { message: 3, stop: { kind: "agent", agent: 3 }, merge: 3 });
  assert.deepEqual(acts(byId("teammate-4-12")), { message: null, stop: { kind: "teammate", agent: 4, pane: "%12" }, merge: null });
  assert.deepEqual(acts({ ...byId("team-3"), status: "stopped" }), { message: 3, stop: null, merge: 3 });
  assert.deepEqual(acts({ ...byId("teammate-4-12"), status: "stopped" }), { message: null, stop: null, merge: null });
  assert.deepEqual(acts(node("team-9", "teammate", { parent: "team-4", status: "working", agent: 9, pane: "%12" })), { message: 9, stop: { kind: "agent", agent: 9 }, merge: null });
  assert.deepEqual(acts(node("teammate-9-1", "teammate", { parent: "gone", agent: null, pane: "%1" })), { message: null, stop: null, merge: null });
  for (const agent of [0, -2, 1.5, "3", null]) assert.deepEqual(acts(node("team-3", "sub", { agent, status: "working" })), { message: null, stop: null, merge: null }, String(agent));
});

test("a card is the mockup's compact four lines, a presence dot, its menu and its full sentence for screen readers, and its chips take the harness line", () => {
  assert.deepEqual([graph.CARD_WIDTH, graph.CARD_HEIGHT, graph.TASK_LINES], [176, 96, 1], "the mockup's compact card: name, harness and account, branch, task");
  const html = renderToStaticMarkup(createElement(view.GraphCard, {
    placed: card(graph.layoutGraph(team), "team-2"), graph: team, items: [], open: false, tipId: null, onOpen() {}, onTip() {},
  }));
  assert.match(html, /class="tg-dot tone-info"/);
  assert.match(html, /<span class="tg-clip">codex-2<\/span>/);
  assert.match(html, /<span class="tg-card-detail tg-clip">codex lead · Work ChatGPT<\/span>/);
  assert.match(html, /<span class="tg-card-task"><span class="tg-clip">Task 7<\/span><\/span>/);
  assert.match(html, /crazyagent\/agent-2/);
  assert.match(html, /Task 7/);
  assert.match(html, /aria-label="codex-2, Lead, codex, Work ChatGPT, branch crazyagent\/agent-2, Working\. Task 7\. Opens its terminal\."/);
  assert.match(html, /aria-pressed="false"/);
  assert.doesNotMatch(html, /Actions for|tg-chip/);
  const placed = card(graph.layoutGraph(talk), "team-3");
  const asking = renderToStaticMarkup(createElement(view.GraphCard, {
    placed: { ...placed, node: { ...placed.node, waiting: "question", presence: "live" } }, graph: talk,
    items: [{ label: "Message", onSelect() {} }], open: false, tipId: null, onOpen() {}, onTip() {},
  }));
  assert.match(asking, /class="tg-card sub waiting"/);
  assert.match(asking, /class="tg-dot tone-warn"/);
  assert.match(asking, /<span class="tg-chip warn">Waiting on you<\/span>/);
  assert.match(asking, /From you 2 \u2713\u2713/);
  assert.doesNotMatch(asking, /tg-card-detail/, "the chips take the harness and account line, which stays in the hover text and the sentence");
  assert.match(asking, /<\/span><\/span><span class="tg-card-task"><span class="tg-clip">Done: Task 3<\/span><\/span><\/button>/, "a card always keeps its task line, last");
  assert.match(asking, /aria-label="Actions for codex-3"/);
  assert.match(asking, /Waiting on you: it asks a question in its terminal\./);
});

test("an owner message is checked against the coordination layer's byte limit before it is sent", () => {
  assert.equal(view.messageProblem("  \n "), "Write a message first.");
  assert.equal(view.messageProblem("x".repeat(4000)), null);
  assert.equal(view.messageProblem("x".repeat(4001)), "Keep the message under 4000 bytes.");
  assert.equal(view.messageProblem("\u00e9".repeat(2001)), "Keep the message under 4000 bytes.", "the limit counts bytes, not letters");
  assert.equal(view.actionNote({ kind: "not_ready" }, "messaging an agent"), "Not ready yet: messaging an agent isn't in this build.");
  assert.equal(view.actionNote({ kind: "ok", value: { outcome: "applied", rejection: null, pending: false } }, "x"), null);
});

test("fit to view centres the graph, never zooms past 100 percent and stays in the zoom range", () => {
  assert.deepEqual(graph.fitView({ width: 400, height: 200 }, { width: 800, height: 600 }), { x: 200, y: 200, zoom: 1 });
  assert.deepEqual(graph.fitView({ width: 1600, height: 600 }, { width: 800, height: 600 }), { x: 0, y: 150, zoom: 0.5 });
  const tiny = graph.fitView({ width: 100_000, height: 300 }, { width: 800, height: 600 });
  assert.equal(tiny.zoom, graph.ZOOM_MIN);
  assert.equal(tiny.x, (800 - 100_000 * graph.ZOOM_MIN) / 2);
  assert.deepEqual(graph.fitView({ width: 400, height: 200 }, { width: 0, height: 0 }), graph.VIEW_HOME);
});

test("zoom keeps the point under the pointer still, and pan keeps part of the graph in view", () => {
  const content = { width: 1000, height: 800 };
  const port = { width: 800, height: 600 };
  const zoomed = graph.zoomView({ x: 0, y: 0, zoom: 1 }, 2, { x: 100, y: 50 }, content, port);
  assert.deepEqual(zoomed, { x: -100, y: -50, zoom: 2 });
  assert.equal(graph.zoomView(zoomed, 100, { x: 0, y: 0 }, content, port).zoom, graph.ZOOM_MAX);
  assert.equal(graph.zoomView(zoomed, 0.0001, { x: 0, y: 0 }, content, port).zoom, graph.ZOOM_MIN);
  assert.deepEqual(graph.panView({ x: 0, y: 0, zoom: 1 }, 30, -20, content, port), { x: 30, y: -20, zoom: 1 });
  const far = graph.panView({ x: 0, y: 0, zoom: 1 }, 99_999, -99_999, content, port);
  assert.deepEqual(far, { x: port.width - graph.PAN_KEEP, y: graph.PAN_KEEP - content.height, zoom: 1 });
  assert.deepEqual(graph.panView(far, Number.NaN, 5, content, port), far);
  assert.deepEqual(graph.zoomView(far, Number.POSITIVE_INFINITY, { x: 0, y: 0 }, content, port), far);
});

test("view keys pan, zoom and fit, and only a pinch or Ctrl wheel zooms", () => {
  assert.deepEqual(graph.viewKey("ArrowLeft"), { kind: "pan", dx: graph.PAN_STEP, dy: 0 });
  assert.deepEqual(graph.viewKey("ArrowDown"), { kind: "pan", dx: 0, dy: -graph.PAN_STEP });
  assert.deepEqual(graph.viewKey("="), { kind: "zoom", factor: graph.ZOOM_STEP });
  assert.deepEqual(graph.viewKey("-"), { kind: "zoom", factor: 1 / graph.ZOOM_STEP });
  assert.deepEqual(graph.viewKey("0"), { kind: "fit" });
  assert.equal(graph.viewKey("Enter"), null);
  assert.equal(graph.wheelZoom({ deltaY: 40, ctrlKey: false, metaKey: false }), null);
  assert.equal(graph.wheelZoom({ deltaY: 0, ctrlKey: true, metaKey: false }), 1);
  assert.ok(graph.wheelZoom({ deltaY: 10, ctrlKey: true, metaKey: false }) < 1);
  assert.ok(graph.wheelZoom({ deltaY: -10, ctrlKey: false, metaKey: true }) > 1);
  assert.equal(graph.wheelZoom({ deltaY: Number.NaN, ctrlKey: true, metaKey: false }), null);
});

test("the hover card sits under a card near the top and over a card near the bottom, inside the view", () => {
  const port = { width: 800, height: 600 };
  const top = graph.tipPlace({ x: 100, y: 20, width: 200, height: 100 }, { x: 0, y: 0, zoom: 1 }, port);
  assert.deepEqual(top, { left: 200, top: 120 + graph.TIP_GAP, above: false });
  const low = graph.tipPlace({ x: 700, y: 450, width: 200, height: 100 }, { x: 0, y: 0, zoom: 1 }, port);
  assert.deepEqual(low, { left: port.width - graph.TIP_HALF, top: 450 - graph.TIP_GAP, above: true });
  const scaled = graph.tipPlace({ x: 100, y: 20, width: 200, height: 100 }, { x: 10, y: 10, zoom: 0.5 }, port);
  assert.deepEqual(scaled, { left: graph.TIP_HALF, top: 10 + 60 + graph.TIP_GAP, above: false });
});

test("a hookless card has no receipt ticks and says only its receipts are unavailable", () => {
  const client = engineClient;
  const bare = { ...talk, nodes: talk.nodes.map((item) => item.id === "team-3" ? { ...item, hookless: true } : item) };
  const layout = graph.layoutGraph(bare);
  const down = layout.links.find((link) => link.to === "team-3");
  assert.equal(down.hookless, true);
  assert.equal(graph.badgeLabel(down), `1 unread message, receipts ${client.HOOKLESS_WORDS}`);
  assert.equal(layout.links.find((link) => link.to === "team-2").hookless, false);
  assert.deepEqual(graph.receiptShown("delivered", true), { ticks: "", word: "receipts unavailable for this account kind" });
  assert.equal(graph.receiptShown("delivered", false).ticks, "✓✓");
  const tip = graph.cardTip(card(layout, "team-3").node, bare);
  assert.ok(tip.includes("Receipts unavailable for this account kind."), tip.join("\n"));
  assert.ok(!tip.some((line) => line.includes("adoption")), tip.join("\n"));
  assert.ok(tip.some((line) => /^2 unread messages from .+, receipts unavailable for this account kind\.$/.test(line)), tip.join("\n"));
  assert.ok(!graph.cardTip(card(graph.layoutGraph(talk), "team-3").node, talk).some((line) => line.includes("unavailable")));
  const html = renderToStaticMarkup(createElement(view.GraphCard, {
    placed: card(layout, "team-3"), graph: bare, items: [], open: false, tipId: null, onOpen() {}, onTip() {},
  }));
  assert.match(html, /<span class="tg-chip">From you 2<\/span>/);
  assert.doesNotMatch(html, /✓/);
});

test("a card whose agent the Delivery setting leaves without messages shows the engine's sentence on the card, in its tip and its Message helper", () => {
  const note = "Can't receive messages: Native only needs tools on for Codex.";
  const quiet = { ...talk, nodes: talk.nodes.map((item) => item.id === "team-2" ? { ...item, message_note: note } : item) };
  const layout = graph.layoutGraph(quiet);
  const tip = graph.cardTip(card(layout, "team-2").node, quiet);
  assert.ok(tip.includes(note), tip.join("\n"));
  const html = renderToStaticMarkup(createElement(view.GraphCard, {
    placed: card(layout, "team-2"), graph: quiet, items: [], open: false, tipId: null, onOpen() {}, onTip() {},
  }));
  assert.ok(html.includes(`<span class="tg-chip warn note"><span class="tg-clip">Can&#x27;t receive messages: Native only needs tools on for Codex.</span></span>`), html);
  assert.equal(view.messageHelper(card(layout, "team-2").node), `${note} It waits in the agent's mailbox until tools are on or message delivery changes.`);
  const plain = graph.layoutGraph(talk);
  assert.ok(!graph.cardTip(card(plain, "team-2").node, talk).includes(note));
  assert.doesNotMatch(renderToStaticMarkup(createElement(view.GraphCard, {
    placed: card(plain, "team-2"), graph: talk, items: [], open: false, tipId: null, onOpen() {}, onTip() {},
  })), /receive messages/);
  assert.equal(view.messageHelper({ hookless: false }), "It goes to the agent's mailbox. Its receipt shows on the graph: one tick sent, two delivered, then read and answered.");
  assert.equal(view.messageHelper({ hookless: true }), `It goes to the agent's mailbox. Its receipts are ${engineClient.HOOKLESS_WORDS}.`);
});
