import type { GraphEdge, GraphEdgeState, GraphNode, TeamAgentStatus, TeamGraph } from "../../engine-client";
import type { Tone } from "../../ui";

export const CARD_WIDTH = 216;
export const CARD_HEIGHT = 96;
export const CARD_GAP = 16;
export const GROUP_HEAD = 44;
export const GROUP_PAD = 12;
export const GROUP_GAP = 32;
export const GROUP_COLUMNS_MAX = 6;
export const LEVEL_GAP = 72;
export const MARGIN = 24;
export const NODES_MAX = 64;
export const REFERENCE_VIEW: Size = { width: 1200, height: 560 };
export const BADGE_WIDTH = 96;
export const BADGE_HEIGHT = 24;
export const TITLE_CHARS = 26;
export const LINE_CHARS = 30;
export const TASK_LINES = 2;
export const TIP_CHARS = 1000;
export const TIP_HALF = 160;
export const TIP_GAP = 8;
export const ELLIPSIS = "...";
export const ZOOM_MIN = 0.2;
export const ZOOM_MAX = 2;
export const ZOOM_STEP = 1.25;
export const PAN_STEP = 48;
export const PAN_KEEP = 48;
export const DRAG_SLOP = 4;
const WHEEL_ZOOM_RATE = 0.01;

export interface Point { x: number; y: number }
export interface Size { width: number; height: number }
export interface Box extends Point, Size {}

/** Where the canvas sits in the viewport: its offset in screen pixels and its zoom. */
export interface View { x: number; y: number; zoom: number }

export const VIEW_HOME: View = { x: 0, y: 0, zoom: 1 };

export interface PlacedCard extends Box { node: GraphNode }

/** One harness frame: its group node, or null for agents with no known harness, and the
 * agent cards inside it under its header. */
export interface PlacedGroup extends Box { node: GraphNode | null; cards: PlacedCard[] }

/** The link from the master to one harness frame. It carries no text, only the number of live
 * tasks in that harness and the number of questions its agents wait on, at `badge`. */
export interface Link { group: GraphNode | null; path: string; badge: Point; tasks: number; questions: number }

/** A team graph laid out for drawing: master cards on top, one frame per harness under them
 * with agent cards in a grid, the links between, and the canvas size that holds them all. */
export interface GraphLayout extends Size {
  masters: PlacedCard[];
  groups: PlacedGroup[];
  links: Link[];
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const byText = (a: string, b: string) => a.localeCompare(b, "en", { numeric: true });
const byNode = (a: GraphNode, b: GraphNode) => byText(a.label, b.label) || byText(a.id, b.id);
const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), Math.max(low, high));
const span = (count: number, size: number) => count * size + Math.max(0, count - 1) * CARD_GAP;
const live = (edge: GraphEdge) => edge.state === "sent" || edge.state === "running";
const plural = (count: number, word: string) => `${count} ${count === 1 ? word : `${word}s`}`;

function curve(from: Point, to: Point): string {
  const middle = (from.y + to.y) / 2;
  return `M ${from.x} ${from.y} C ${from.x} ${middle}, ${to.x} ${middle}, ${to.x} ${to.y}`;
}

const columnsFor = (agents: number, rows: number) => Math.min(GROUP_COLUMNS_MAX, Math.max(1, Math.ceil(agents / rows)));

/** How many card columns each harness frame uses. Every row count up to the largest frame is
 * tried, and the one whose whole layout fits `REFERENCE_VIEW` at the largest zoom wins, fewer
 * rows first on a tie; no frame uses more than `GROUP_COLUMNS_MAX` columns. Inputs: the agent
 * count of each frame and whether a master row sits above. Output: columns per frame. */
export function frameColumns(counts: readonly number[], master: boolean): number[] {
  let best = counts.map((count) => columnsFor(count, 1));
  let bestZoom = -1;
  for (let rows = 1; rows <= Math.max(1, ...counts); rows += 1) {
    const columns = counts.map((count) => columnsFor(count, rows));
    const width = MARGIN * 2 + columns.reduce((sum, item) => sum + GROUP_PAD * 2 + span(item, CARD_WIDTH), 0)
      + Math.max(0, columns.length - 1) * GROUP_GAP;
    const tallest = Math.max(0, ...counts.map((count, index) => Math.ceil(count / columns[index])));
    const height = MARGIN * 2 + (master ? CARD_HEIGHT + LEVEL_GAP : 0) + GROUP_HEAD + (tallest ? span(tallest, CARD_HEIGHT) + GROUP_PAD : 0);
    const zoom = Math.min(REFERENCE_VIEW.width / width, REFERENCE_VIEW.height / height);
    if (zoom > bestZoom) {
      best = columns;
      bestZoom = zoom;
    }
  }
  assert(best.length === counts.length && best.every((item) => item >= 1 && item <= GROUP_COLUMNS_MAX), "every frame has 1 to GROUP_COLUMNS_MAX columns");
  return best;
}

/** The questions one node's agent waits on, as the engine reports them; anything but a
 * positive whole number counts as none. Input: the node. Output: the count. */
export function questionCount(node: GraphNode): number {
  const count = node.questions;
  return typeof count === "number" && Number.isInteger(count) && count > 0 ? count : 0;
}

/** Lays out one project's team graph: the master on top, then one frame per harness side by
 * side with its agents in a grid of fixed-size cards (shaped by `frameColumns`), and agents
 * with no known harness in a last frame. Cells never share space, so cards never overlap at any agent count. Input: the
 * graph. Output: the layout, the same for the same graph. */
export function layoutGraph(graph: TeamGraph): GraphLayout {
  const seen = new Set<string>();
  const nodes = graph.nodes.filter((node) => !seen.has(node.id) && Boolean(seen.add(node.id))).slice(0, NODES_MAX);
  const masterNodes = nodes.filter((node) => node.kind === "master").sort(byNode);
  const groupNodes = nodes.filter((node) => node.kind === "harness_group").sort(byNode);
  const agents = nodes.filter((node) => node.kind === "agent").sort((a, b) => byText(a.id, b.id));
  const groupIds = new Set(groupNodes.map((group) => group.id));
  const frames: { node: GraphNode | null; members: GraphNode[] }[] =
    groupNodes.map((node) => ({ node, members: agents.filter((agent) => agent.parent === node.id) }));
  const orphans = agents.filter((agent) => agent.parent === null || !groupIds.has(agent.parent));
  if (orphans.length) frames.push({ node: null, members: orphans });

  const top = MARGIN + (masterNodes.length ? CARD_HEIGHT + LEVEL_GAP : 0);
  const shape = frameColumns(frames.map((frame) => frame.members.length), masterNodes.length > 0);
  const groups: PlacedGroup[] = [];
  let left = MARGIN;
  for (const [order, frame] of frames.entries()) {
    const columns = shape[order];
    const rows = Math.ceil(frame.members.length / columns);
    const cards = frame.members.map((node, index) => ({
      node,
      x: left + GROUP_PAD + (index % columns) * (CARD_WIDTH + CARD_GAP),
      y: top + GROUP_HEAD + Math.floor(index / columns) * (CARD_HEIGHT + CARD_GAP),
      width: CARD_WIDTH,
      height: CARD_HEIGHT,
    }));
    const width = GROUP_PAD * 2 + span(columns, CARD_WIDTH);
    const height = GROUP_HEAD + (rows ? span(rows, CARD_HEIGHT) + GROUP_PAD : 0);
    groups.push({ node: frame.node, cards, x: left, y: top, width, height });
    left += width + GROUP_GAP;
  }
  const inner = Math.max(groups.length ? left - GROUP_GAP - MARGIN : 0, span(masterNodes.length, CARD_WIDTH));
  const start = MARGIN + (inner - span(masterNodes.length, CARD_WIDTH)) / 2;
  const masters = masterNodes.map((node, index) => ({
    node, x: start + index * (CARD_WIDTH + CARD_GAP), y: MARGIN, width: CARD_WIDTH, height: CARD_HEIGHT,
  }));
  const placed = masters.length + groups.filter((group) => group.node).length + groups.reduce((sum, group) => sum + group.cards.length, 0);
  assert(placed === masterNodes.length + groupNodes.length + agents.length, "every node is placed once");

  const links: Link[] = [];
  for (const group of groups) {
    const parent = masters.find((master) => master.node.id === group.node?.parent) ?? masters[0];
    if (!parent) continue;
    const ids = new Set(group.cards.map((item) => item.node.id));
    if (group.node) ids.add(group.node.id);
    const from = { x: parent.x + CARD_WIDTH / 2, y: parent.y + CARD_HEIGHT };
    const to = { x: group.x + group.width / 2, y: group.y };
    links.push({
      group: group.node,
      path: curve(from, to),
      badge: { x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 },
      tasks: graph.edges.filter((edge) => live(edge) && ids.has(edge.to)).length,
      questions: group.cards.reduce((sum, item) => sum + questionCount(item.node), 0),
    });
  }
  const height = groups.length
    ? top + Math.max(...groups.map((group) => group.height)) + MARGIN
    : MARGIN * 2 + (masters.length ? CARD_HEIGHT : 0);
  return { masters, groups, links, width: MARGIN * 2 + inner, height };
}

/** Every card the layout draws: the masters, then each frame's agents. Input: the layout.
 * Output: the cards in drawing order. */
export function layoutCards(layout: GraphLayout): PlacedCard[] {
  return [...layout.masters, ...layout.groups.flatMap((group) => group.cards)];
}

/** What a link's badges say to a screen reader. Input: the counts. Output: one phrase, empty
 * when there is nothing to show. */
export function badgeLabel(link: Pick<Link, "tasks" | "questions">): string {
  const parts = [];
  if (link.tasks > 0) parts.push(plural(link.tasks, "live task"));
  if (link.questions > 0) parts.push(`${plural(link.questions, "question")} waiting`);
  return parts.join(", ");
}

/** Wraps text into at most `lines` lines of at most `chars` characters, breaking at spaces
 * and splitting a word longer than a line. Whitespace runs read as one space. When text is
 * left over, the last line ends in "...". Inputs: text, line length and line count. Output:
 * the lines, none for blank text. */
export function clipLines(text: string, chars: number, lines: number): string[] {
  assert(Number.isInteger(chars) && chars > ELLIPSIS.length, "a line holds more than the ellipsis");
  assert(Number.isInteger(lines) && lines > 0, "at least one line");
  const letters = Array.from(text.split(/\s+/).filter(Boolean).join(" "));
  const out: string[] = [];
  let start = 0;
  while (start < letters.length && out.length < lines) {
    let end = Math.min(start + chars, letters.length);
    if (end < letters.length) {
      const space = letters.lastIndexOf(" ", end);
      if (space > start) end = space;
    }
    if (out.length === lines - 1 && end < letters.length) {
      out.push(`${letters.slice(start, start + chars - ELLIPSIS.length).join("").trimEnd()}${ELLIPSIS}`);
      break;
    }
    out.push(letters.slice(start, end).join(""));
    start = end;
    while (letters[start] === " ") start += 1;
  }
  assert(out.length <= lines && out.every((line) => Array.from(line).length <= chars), "clipped lines fit");
  return out;
}

function clipChars(text: string, chars: number): string {
  const letters = Array.from(text);
  return letters.length <= chars ? text : `${letters.slice(0, chars - ELLIPSIS.length).join("")}${ELLIPSIS}`;
}

/** The task one agent card is about: its newest live task, else its newest ended one. Inputs:
 * the node and the graph's edges. Output: the edge, or undefined when it has none. */
export function latestTask(node: GraphNode, edges: readonly GraphEdge[]): GraphEdge | undefined {
  return edges.filter((edge) => edge.to === node.id)
    .sort((a, b) => Number(live(b)) - Number(live(a)) || b.task_id - a.task_id)[0];
}

/** The task line of a card: for the master, how many tasks are live and how many wait for an
 * agent; for an agent, its latest task, led by Done or Failed once it ended. Inputs: the node
 * and the edges. Output: the full line, before clipping. */
export function cardTask(node: GraphNode, edges: readonly GraphEdge[]): string {
  if (node.kind === "master") {
    const count = edges.filter(live).length;
    const waiting = edges.filter((edge) => live(edge) && edge.to === node.id).length;
    return `${plural(count, "task")} live${waiting ? `, ${waiting} waiting for an agent` : ""}.`;
  }
  const task = latestTask(node, edges);
  if (!task) return "No task yet.";
  return live(task) ? task.text : `${edgeStatus(task).word}: ${task.text}`;
}

/** The clipped text of one card: the title, the harness line and up to `TASK_LINES` task
 * lines. Inputs: the node, the edges and a harness name lookup. Output: the card text. */
export function cardText(node: GraphNode, edges: readonly GraphEdge[], harness?: (id: string) => string):
  { title: string; detail: string; task: string[] } {
  return {
    title: clipLines(node.label, TITLE_CHARS, 1)[0] ?? "",
    detail: clipLines(nodeDetail(node, harness), LINE_CHARS, 1)[0] ?? "",
    task: clipLines(cardTask(node, edges), LINE_CHARS, TASK_LINES),
  };
}

/** The full text a card shows on hover: title, harness line, status, task and the reply once
 * it came, each kept to `TIP_CHARS`. Inputs: the node, the edges and a harness name lookup.
 * Output: the lines. */
export function cardTip(node: GraphNode, edges: readonly GraphEdge[], harness?: (id: string) => string): string[] {
  const reply = node.kind === "agent" ? latestTask(node, edges)?.reply : null;
  return [node.label, nodeDetail(node, harness), nodeStatus(node)?.word, cardTask(node, edges), reply ? `Reply: ${reply}` : undefined]
    .filter((line): line is string => Boolean(line))
    .map((line) => clipChars(line, TIP_CHARS));
}

/** The header of a harness frame: the group's name and agent count, or "Other agents".
 * Inputs: the frame and a harness name lookup. Output: one line. */
export function groupTitle(group: PlacedGroup, harness?: (id: string) => string): string {
  return group.node ? nodeDetail(group.node, harness) : "Other agents";
}

/** The view that shows the whole graph centred, never zoomed past 100%. Inputs: the canvas and
 * viewport sizes. Output: the view, or the home view before the viewport has a size. */
export function fitView(content: Size, viewport: Size): View {
  if (!(viewport.width > 0 && viewport.height > 0 && content.width > 0 && content.height > 0)) return VIEW_HOME;
  const zoom = clamp(Math.min(viewport.width / content.width, viewport.height / content.height, 1), ZOOM_MIN, ZOOM_MAX);
  return { x: (viewport.width - content.width * zoom) / 2, y: Math.max(0, (viewport.height - content.height * zoom) / 2), zoom };
}

function keepInView(view: View, content: Size, viewport: Size): View {
  const zoom = clamp(view.zoom, ZOOM_MIN, ZOOM_MAX);
  return {
    x: clamp(view.x, PAN_KEEP - content.width * zoom, viewport.width - PAN_KEEP),
    y: clamp(view.y, PAN_KEEP - content.height * zoom, viewport.height - PAN_KEEP),
    zoom,
  };
}

/** Moves the view, keeping at least `PAN_KEEP` pixels of the graph on screen. Inputs: the view,
 * the move, and the canvas and viewport sizes. Output: the new view; a non-finite move keeps it. */
export function panView(view: View, dx: number, dy: number, content: Size, viewport: Size): View {
  if (!Number.isFinite(dx) || !Number.isFinite(dy)) return view;
  return keepInView({ ...view, x: view.x + dx, y: view.y + dy }, content, viewport);
}

/** Zooms the view by a factor about a viewport point, which stays still, within the zoom
 * range. Inputs: the view, the factor, the point, and the canvas and viewport sizes. Output:
 * the new view; a non-finite or non-positive factor keeps it. */
export function zoomView(view: View, factor: number, at: Point, content: Size, viewport: Size): View {
  if (!Number.isFinite(factor) || factor <= 0 || !Number.isFinite(at.x) || !Number.isFinite(at.y)) return view;
  assert(view.zoom > 0, "a view always has a positive zoom");
  const zoom = clamp(view.zoom * factor, ZOOM_MIN, ZOOM_MAX);
  const scale = zoom / view.zoom;
  return keepInView({ x: at.x - (at.x - view.x) * scale, y: at.y - (at.y - view.y) * scale, zoom }, content, viewport);
}

/** What a key does to the graph view: arrows pan, plus and minus zoom, 0 fits. Input: the
 * key. Output: the action, or null for any other key. */
export function viewKey(key: string): { kind: "pan"; dx: number; dy: number } | { kind: "zoom"; factor: number } | { kind: "fit" } | null {
  switch (key) {
    case "ArrowLeft": return { kind: "pan", dx: PAN_STEP, dy: 0 };
    case "ArrowRight": return { kind: "pan", dx: -PAN_STEP, dy: 0 };
    case "ArrowUp": return { kind: "pan", dx: 0, dy: PAN_STEP };
    case "ArrowDown": return { kind: "pan", dx: 0, dy: -PAN_STEP };
    case "+": case "=": return { kind: "zoom", factor: ZOOM_STEP };
    case "-": case "_": return { kind: "zoom", factor: 1 / ZOOM_STEP };
    case "0": return { kind: "fit" };
    default: return null;
  }
}

/** The zoom factor of a wheel event: a pinch or a Ctrl or Cmd wheel zooms, a plain wheel is
 * left to scroll the page. Input: the event. Output: the factor, or null to leave it. */
export function wheelZoom(event: { deltaY: number; ctrlKey: boolean; metaKey: boolean }): number | null {
  if (!(event.ctrlKey || event.metaKey) || !Number.isFinite(event.deltaY)) return null;
  return Math.exp(-event.deltaY * WHEEL_ZOOM_RATE);
}

/** Where a card's hover text sits in the viewport: centred under the card, or over it when the
 * card is in the lower half, kept inside the viewport's width. Inputs: the card, the view and
 * the viewport size. Output: the anchor point and whether the text sits above it. */
export function tipPlace(card: Box, view: View, viewport: Size): { left: number; top: number; above: boolean } {
  const left = clamp(view.x + (card.x + card.width / 2) * view.zoom, TIP_HALF, viewport.width - TIP_HALF);
  const above = view.y + (card.y + card.height / 2) * view.zoom > viewport.height / 2;
  const top = above ? view.y + card.y * view.zoom - TIP_GAP : view.y + (card.y + card.height) * view.zoom + TIP_GAP;
  return { left, top, above };
}

const AGENT_WORDS: Record<TeamAgentStatus, { word: string; tone: Tone }> = {
  starting: { word: "Starting", tone: "info" },
  idle: { word: "Idle", tone: "neutral" },
  working: { word: "Working", tone: "info" },
  stopped: { word: "Stopped", tone: "neutral" },
  failed: { word: "Failed", tone: "danger" },
};

const EDGE_WORDS: Record<GraphEdgeState, { word: string; tone: Tone }> = {
  sent: { word: "Sent", tone: "neutral" },
  running: { word: "Running", tone: "info" },
  done: { word: "Done", tone: "ok" },
  failed: { word: "Failed", tone: "danger" },
};

/** The status word and tone a node shows: the engine's label when it gives one, else the
 * fixed word for the status. Input: the node. Output: word and tone, or null for none. */
export function nodeStatus(node: GraphNode): { word: string; tone: Tone } | null {
  if (node.status === null) return null;
  const fixed = AGENT_WORDS[node.status];
  return { word: node.status_label?.trim() || fixed.word, tone: fixed.tone };
}

/** The status word and tone of one task edge. Input: the edge. Output: word and tone. */
export function edgeStatus(edge: GraphEdge): { word: string; tone: Tone } {
  return EDGE_WORDS[edge.state];
}

/** One line naming an agent or the master with its harness and status, such as
 * "Ada (@onboarding-fix) · Claude Code · working". Inputs: the node and a harness display
 * name lookup. Output: the line; a node without a status leaves that part out. */
export function nodeLine(node: GraphNode, harness: (id: string) => string = (id) => id): string {
  const status = nodeStatus(node);
  return [node.label, harness(node.harness), status?.word.toLowerCase()].filter(Boolean).join(" · ");
}

/** The second line of a node box: the master's harness, a group's agent count, or an
 * agent's harness and account. Inputs: the node and a harness display name lookup.
 * Output: one short line. */
export function nodeDetail(node: GraphNode, harness: (id: string) => string = (id) => id): string {
  const name = harness(node.harness);
  switch (node.kind) {
    case "master": return `Master, ${name}`;
    case "harness_group": {
      const count = node.agents ?? 0;
      return `${name}, ${node.status_label?.trim() || `${count} ${count === 1 ? "agent" : "agents"}`}`;
    }
    case "agent": return node.account ? `${name}, ${node.account}` : name;
  }
}

/** Puts each node's account name in place of the engine's account id, so the graph names
 * accounts as the Team panel does. Inputs: the graph and an account namer. Output: a new graph;
 * the input is left unchanged. */
export function namedAccounts(graph: TeamGraph, account: (id: string) => string): TeamGraph {
  return { ...graph, nodes: graph.nodes.map((node) => node.account === null ? node : { ...node, account: account(node.account) }) };
}

/** Whether the graph can still change soon: an agent starting or working, or a task sent or
 * running. Input: the graph. Output: true to read it again quickly. */
export function graphLive(graph: TeamGraph): boolean {
  return graph.nodes.some((node) => node.status === "starting" || node.status === "working")
    || graph.edges.some(live);
}

/** The one line under the graph title: how many agents work and how many tasks are live.
 * Input: the graph. Output: the line. */
export function graphSummary(graph: TeamGraph): string {
  const agents = graph.nodes.filter((node) => node.kind === "agent");
  const working = agents.filter((node) => node.status === "working").length;
  const count = graph.edges.filter(live).length;
  return `${working} of ${plural(agents.length, "agent")} working. ${plural(count, "task")} live.`;
}
