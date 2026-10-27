import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const graph = await import("../src/screens/team/graph.ts");
const view = await import("../src/screens/team/index.tsx");

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: null, ...extra,
});
const edge = (task_id, from, to, state, extra = {}) => ({ from, to, task_id, source: "master", text: `Task ${task_id}`, state, reply: null, ...extra });

const team = {
  project_id: "chores",
  nodes: [
    node("team-2", "agent", { parent: "g-codex", label: "codex-2", status: "working", account: "Work ChatGPT", terminal: "team-2", questions: 2 }),
    node("g-codex", "harness_group", { parent: "m", label: "Codex", agents: 2 }),
    node("m", "master", { label: "Master", status: "idle", terminal: "team-9" }),
    node("team-1", "agent", { parent: "g-codex", label: "codex-1", status: "idle", terminal: "team-1" }),
    node("g-claude", "harness_group", { parent: "m", label: "Claude Code", harness: "claude", agents: 0 }),
  ],
  edges: [
    edge(3, "m", "team-1", "done", { reply: "Wrote chores.md" }),
    edge(7, "m", "team-2", "running"),
    edge(8, "m", "g-claude", "sent", { source: "owner" }),
    edge(9, "m", "m", "sent"),
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

function crowd(count, harnesses) {
  const nodes = [node("m", "master", { label: "Master", terminal: "team-0" })];
  for (const harness of harnesses) nodes.push(node(`g-${harness}`, "harness_group", { parent: "m", label: harness, harness }));
  for (let index = 0; index < count; index += 1) {
    const harness = harnesses[index % harnesses.length];
    nodes.push(node(`team-${index + 1}`, "agent", {
      parent: index % 7 === 6 ? null : `g-${harness}`, harness, status: "working", terminal: `team-${index + 1}`, questions: index % 3,
      label: `Persona${index} (@a-really-long-role-handle-${index})`,
    }));
  }
  const edges = Array.from({ length: count }, (_, index) => edge(index + 1, "m", `team-${index + 1}`, "running", { text: "word ".repeat(200) }));
  return { project_id: "p", nodes, edges };
}

test("the master sits on top and each harness is a frame of agents below it", () => {
  const layout = graph.layoutGraph(team);
  const master = card(layout, "m");
  assert.deepEqual([master.x, master.y], [graph.MARGIN + (layout.width - 2 * graph.MARGIN - graph.CARD_WIDTH) / 2, graph.MARGIN]);
  assert.deepEqual(layout.groups.map((group) => group.node?.id), ["g-claude", "g-codex"]);
  const [claude, codex] = layout.groups;
  const top = graph.MARGIN + graph.CARD_HEIGHT + graph.LEVEL_GAP;
  assert.deepEqual([claude.x, claude.y, claude.height], [graph.MARGIN, top, graph.GROUP_HEAD]);
  assert.deepEqual(codex.cards.map((item) => item.node.id), ["team-1", "team-2"]);
  assert.deepEqual(codex.cards.map((item) => [item.x, item.y]), [
    [codex.x + graph.GROUP_PAD, top + graph.GROUP_HEAD],
    [codex.x + graph.GROUP_PAD + graph.CARD_WIDTH + graph.CARD_GAP, top + graph.GROUP_HEAD],
  ]);
  assert.equal(codex.x, claude.x + claude.width + graph.GROUP_GAP);
  assert.equal(layout.width, codex.x + codex.width + graph.MARGIN);
  assert.equal(layout.height, top + codex.height + graph.MARGIN);
  assert.ok(graph.layoutCards(layout).every((item) => item.width === graph.CARD_WIDTH && item.height === graph.CARD_HEIGHT));
});

test("links run from the master to each harness and carry only live task and question counts", () => {
  const layout = graph.layoutGraph(team);
  const master = card(layout, "m");
  assert.deepEqual(layout.links.map((link) => [link.group?.id, link.tasks, link.questions]), [["g-claude", 1, 0], ["g-codex", 1, 2]]);
  const codex = layout.groups[1];
  const from = { x: master.x + graph.CARD_WIDTH / 2, y: master.y + graph.CARD_HEIGHT };
  const to = { x: codex.x + codex.width / 2, y: codex.y };
  const middle = (from.y + to.y) / 2;
  assert.equal(layout.links[1].path, `M ${from.x} ${from.y} C ${from.x} ${middle}, ${to.x} ${middle}, ${to.x} ${to.y}`);
  assert.deepEqual(layout.links[1].badge, { x: (from.x + to.x) / 2, y: middle });
  assert.equal(graph.badgeLabel(layout.links[1]), "1 live task, 2 questions waiting");
  assert.equal(graph.badgeLabel({ tasks: 3, questions: 1 }), "3 live tasks, 1 question waiting");
  assert.equal(graph.badgeLabel({ tasks: 0, questions: 0 }), "");
  const bad = graph.layoutGraph({ ...team, nodes: team.nodes.map((item) => ({ ...item, questions: item.id === "team-2" ? -4 : 1.5 })) });
  assert.deepEqual(bad.links.map((link) => link.questions), [0, 0]);
});

test("agents without a known harness get an Other agents frame, and duplicate ids are drawn once", () => {
  const nodes = [...team.nodes, node("research-1", "agent", { parent: null, label: "Market research" }), node("team-1", "agent", { parent: "g-claude" })];
  const layout = graph.layoutGraph({ ...team, nodes });
  const other = layout.groups[2];
  assert.equal(other.node, null);
  assert.deepEqual(other.cards.map((item) => item.node.id), ["research-1"]);
  assert.deepEqual(layout.groups[0].cards, []);
  assert.equal(graph.layoutCards(layout).length, 4);
  assert.equal(graph.groupTitle(other), "Other agents");
  assert.equal(graph.groupTitle(layout.groups[1], (id) => ({ codex: "Codex" })[id] ?? id), "Codex, 2 agents");
});

test("a master alone is one card, and at most NODES_MAX nodes are drawn", () => {
  const alone = graph.layoutGraph({ project_id: "p", nodes: [node("m", "master")], edges: [] });
  assert.deepEqual([card(alone, "m").x, card(alone, "m").y], [graph.MARGIN, graph.MARGIN]);
  assert.deepEqual([alone.width, alone.height], [graph.MARGIN * 2 + graph.CARD_WIDTH, graph.MARGIN * 2 + graph.CARD_HEIGHT]);
  assert.deepEqual(alone.links, []);
  const many = { project_id: "p", nodes: Array.from({ length: graph.NODES_MAX + 6 }, (_, index) => node(`team-${index + 1}`, "agent")), edges: [] };
  assert.equal(graph.layoutCards(graph.layoutGraph(many)).length, graph.NODES_MAX);
});

test("past NODES_MAX the master and frames stay, and running, waiting, failed then stopped agents are drawn newest first", () => {
  const harnesses = Array.from({ length: 8 }, (_, index) => `h${index + 1}`);
  const nodes = [node("m", "master", { label: "Master", status: "working", terminal: "team-0" })];
  for (const harness of harnesses) nodes.push(node(`g-${harness}`, "harness_group", { parent: "m", label: harness, harness }));
  const status = (index) => index <= 10 ? "stopped" : index <= 12 ? "failed" : index === 13 ? "stopped" : ["starting", "idle", "working"][index % 3];
  for (let index = 1; index <= 63; index += 1) {
    const harness = harnesses[index % harnesses.length];
    nodes.push(node(`team-${index}`, "agent", { parent: `g-${harness}`, harness, status: status(index), questions: index === 13 ? 1 : 0, terminal: `team-${index}` }));
  }
  const team = { project_id: "p", nodes, edges: [] };
  assert.equal(nodes.length, 72);
  const layout = graph.layoutGraph(team);
  const drawn = graph.layoutCards(layout).map((item) => item.node.id);
  assert.equal(drawn.length, graph.NODES_MAX - harnesses.length);
  assert.equal(layout.masters.length, 1);
  assert.equal(layout.groups.filter((group) => group.node).length, harnesses.length);
  assert.equal(layout.hidden, 8);
  const kept = new Set(drawn);
  for (let index = 1; index <= 63; index += 1) assert.equal(kept.has(`team-${index}`), index > 8, `team-${index}`);
  assert.equal(graph.graphSummary(team, layout.hidden), "17 of 63 agents working. 0 tasks live. 8 agents not shown.");
  assert.equal(graph.graphSummary(team), "17 of 63 agents working. 0 tasks live.");
  assert.equal(graph.layoutGraph({ project_id: "p", nodes: nodes.slice(0, 40), edges: [] }).hidden, 0);
});

for (const count of [10, 20, 40]) {
  for (const harnesses of [["codex"], ["claude", "codex", "gemini"]]) {
    test(`cards, frames and badges never overlap with ${count} agents on ${harnesses.length} harnesses`, () => {
      const layout = graph.layoutGraph(crowd(count, harnesses));
      const cards = graph.layoutCards(layout);
      assert.equal(cards.length, count + 1);
      assertApart(cards, "cards");
      assertApart(layout.groups, "frames");
      assertApart(layout.links.map(badgeBox), "badges");
      const canvas = { x: 0, y: 0, width: layout.width, height: layout.height };
      for (const group of layout.groups) {
        assert.ok(inside(group, canvas), "every frame is on the canvas");
        const body = { x: group.x, y: group.y + graph.GROUP_HEAD, width: group.width, height: group.height - graph.GROUP_HEAD };
        for (const item of group.cards) assert.ok(inside(item, body), `${item.node.id} sits in its frame under the header`);
        assert.ok(group.width <= graph.GROUP_PAD * 2 + graph.GROUP_COLUMNS_MAX * (graph.CARD_WIDTH + graph.CARD_GAP), "a frame holds at most GROUP_COLUMNS_MAX columns");
      }
      const master = card(layout, "m");
      assert.ok(inside(master, canvas));
      for (const box of [...layout.groups, ...layout.links.map(badgeBox)]) assert.ok(!overlaps(master, box), "the master stands apart");
      for (const box of layout.links.map(badgeBox)) {
        for (const group of layout.groups) assert.ok(!overlaps(box, group), "badges sit between the master and the frames");
      }
      assert.equal(layout.links.reduce((sum, link) => sum + link.tasks, 0), count);
      assert.ok(graph.fitView(layout, graph.REFERENCE_VIEW).zoom >= 0.55, "frames trade width for rows so the team fits a window");
    });
  }
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

test("a card shows its title, harness and newest task, and the full text stays for hover", () => {
  const name = (id) => ({ codex: "Codex" })[id] ?? id;
  const long = { ...team, edges: [...team.edges, edge(10, "m", "team-1", "running", { text: "Rename every helper in the chores list so the names match the house plan" })] };
  const text = graph.cardText(card(graph.layoutGraph(long), "team-1").node, long.edges, name);
  assert.deepEqual(text, { title: "codex-1", detail: "Codex", task: ["Rename every helper in the", "chores list so the names ma..."] });
  assert.equal(graph.cardTask(node("team-1", "agent"), team.edges), "Done: Task 3");
  assert.equal(graph.cardTask(node("team-5", "agent"), team.edges), "No task yet.");
  assert.equal(graph.cardTask(node("m", "master"), team.edges), "3 tasks live, 1 waiting for an agent.");
  assert.equal(graph.cardTask(node("m", "master"), [edge(1, "m", "team-1", "running")]), "1 task live.");
  const title = graph.cardText(node("a", "agent", { label: "Ada Lovelace (@onboarding-fix-for-reused-ids)" }), [], name).title;
  assert.equal(title, "Ada Lovelace (@onboardi...");
  assert.ok(Array.from(title).length <= graph.TITLE_CHARS);
  assert.deepEqual(graph.cardTip(team.nodes[3], team.edges, name), ["codex-1", "Codex", "Idle", "Done: Task 3", "Reply: Wrote chores.md"]);
  const huge = graph.cardTip(node("a", "agent"), [edge(1, "m", "a", "running", { text: "x".repeat(5000) })]);
  assert.equal(huge.length, 3);
  assert.equal(Array.from(huge[2]).length, graph.TIP_CHARS);
  assert.ok(huge[2].endsWith("..."));
});

test("a card renders clipped lines, a status dot and its full sentence for screen readers", () => {
  const html = renderToStaticMarkup(createElement(view.GraphCard, {
    placed: card(graph.layoutGraph(team), "team-2"), edges: team.edges, open: false, tipId: null, onOpen() {}, onTip() {},
  }));
  assert.match(html, /class="tg-dot tone-info"/);
  assert.match(html, /<span class="tg-clip">codex-2<\/span>/);
  assert.match(html, /codex, Work ChatGPT/);
  assert.match(html, /Task 7/);
  assert.match(html, /aria-label="codex-2, codex, Work ChatGPT, Working\. Task 7\. Opens its terminal\."/);
  assert.match(html, /aria-pressed="false"/);
  assert.equal(view.nodeLabel(team.nodes[1]), "Codex, codex, 2 agents.");
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
