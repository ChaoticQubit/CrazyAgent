import type { GraphEdge, GraphEdgeState, GraphNode, TeamAgentStatus, TeamGraph } from "../../engine-client";
import type { Tone } from "../../ui";

export const NODE_WIDTH = 184;
export const NODE_HEIGHT = 76;
export const COLUMN_GAP = 24;
export const ROW_GAP = 96;
export const MARGIN = 24;
export const NODES_MAX = 64;
export const EDGES_SHOWN_MAX = 24;
export const LABEL_STACK = 30;
export const ARROW_STEP = 16;
export const ARROW_STEP_LARGE = 64;
export const DRAG_SLOP = 4;
const OFFSET_MAX = 4000;

export interface Point { x: number; y: number }
export type Offsets = ReadonlyMap<string, Point>;

export interface PlacedNode { node: GraphNode; x: number; y: number }
export interface Link { from: string; to: string; path: string }
export interface PlacedEdge { edge: GraphEdge; path: string; label: Point }

/** A team graph laid out for drawing: node boxes, parent links, task edges with label
 * points, and the canvas size that holds them all. A task still waiting at the master has
 * no path; its label sits under the master. */
export interface GraphLayout {
  nodes: PlacedNode[];
  links: Link[];
  edges: PlacedEdge[];
  width: number;
  height: number;
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const byText = (a: string, b: string) => a.localeCompare(b, "en", { numeric: true });
const byNode = (a: GraphNode, b: GraphNode) => byText(a.label, b.label) || byText(a.id, b.id);
const column = (index: number) => MARGIN + index * (NODE_WIDTH + COLUMN_GAP);
const row = (index: number) => MARGIN + index * (NODE_HEIGHT + ROW_GAP);

function curve(from: Point, to: Point): string {
  const middle = (from.y + to.y) / 2;
  return `M ${from.x} ${from.y} C ${from.x} ${middle}, ${to.x} ${middle}, ${to.x} ${to.y}`;
}

function bottom(placed: PlacedNode): Point {
  return { x: placed.x + NODE_WIDTH / 2, y: placed.y + NODE_HEIGHT };
}

function top(placed: PlacedNode): Point {
  return { x: placed.x + NODE_WIDTH / 2, y: placed.y };
}

/** Picks the task edges the graph draws: every live one first, then the newest ended ones,
 * at most `EDGES_SHOWN_MAX`. Input: the graph's edges. Output: the edges, newest first. */
export function shownEdges(edges: readonly GraphEdge[]): GraphEdge[] {
  const live = (edge: GraphEdge) => edge.state === "sent" || edge.state === "running";
  return [...edges]
    .sort((a, b) => Number(live(b)) - Number(live(a)) || b.task_id - a.task_id)
    .slice(0, EDGES_SHOWN_MAX);
}

/** Lays out one project's team graph in three rows: the master on top, one box per harness
 * group under it, and each agent under its group. Dragged nodes move by their offset and the
 * canvas grows to hold them. Inputs: the graph and the drag offsets by node id. Output: the
 * layout, the same for the same graph and offsets. */
export function layoutGraph(graph: TeamGraph, offsets: Offsets = new Map()): GraphLayout {
  const nodes = graph.nodes.slice(0, NODES_MAX);
  const masters = nodes.filter((node) => node.kind === "master").sort(byNode);
  const groups = nodes.filter((node) => node.kind === "harness_group").sort(byNode);
  const agents = nodes.filter((node) => node.kind === "agent").sort((a, b) => byText(a.id, b.id));
  const groupIds = new Set(groups.map((group) => group.id));
  const placed: PlacedNode[] = [];
  let next = 0;
  for (const group of groups) {
    const children = agents.filter((agent) => agent.parent === group.id);
    const span = Math.max(1, children.length);
    placed.push({ node: group, x: (column(next) + column(next + span - 1)) / 2, y: row(1) });
    children.forEach((agent, index) => placed.push({ node: agent, x: column(next + index), y: row(2) }));
    next += span;
  }
  for (const agent of agents.filter((agent) => agent.parent === null || !groupIds.has(agent.parent))) {
    placed.push({ node: agent, x: column(next), y: row(2) });
    next += 1;
  }
  const columns = Math.max(next, masters.length, 1);
  const masterStart = (columns - masters.length) / 2;
  masters.forEach((master, index) => placed.push({ node: master, x: column(masterStart + index), y: row(0) }));
  assert(placed.length === masters.length + groups.length + agents.length, "every node is placed once");

  const moved = placed.map((item) => {
    const offset = offsets.get(item.node.id);
    return offset ? { ...item, x: Math.max(0, item.x + offset.x), y: Math.max(0, item.y + offset.y) } : item;
  });
  const at = new Map(moved.map((item) => [item.node.id, item]));
  const links: Link[] = [];
  for (const item of moved) {
    const parent = item.node.parent === null ? undefined : at.get(item.node.parent);
    if (parent) links.push({ from: parent.node.id, to: item.node.id, path: curve(bottom(parent), top(item)) });
  }
  const stacked = new Map<string, number>();
  const edges: PlacedEdge[] = [];
  for (const edge of shownEdges(graph.edges)) {
    const from = at.get(edge.from);
    const to = at.get(edge.to);
    if (!from || !to) continue;
    const start = bottom(from);
    const end = top(to);
    const pair = `${edge.from}\u0000${edge.to}`;
    const index = stacked.get(pair) ?? 0;
    stacked.set(pair, index + 1);
    if (from === to) {
      edges.push({ edge, path: "", label: { x: start.x, y: start.y + LABEL_STACK * (index + 1) } });
      continue;
    }
    edges.push({ edge, path: curve(start, end), label: { x: (start.x + end.x) / 2, y: (start.y + end.y) / 2 + index * LABEL_STACK } });
  }
  const rows = agents.length ? 3 : groups.length ? 2 : 1;
  const width = Math.max(column(columns - 1) + NODE_WIDTH + MARGIN, ...moved.map((item) => item.x + NODE_WIDTH + MARGIN));
  const height = Math.max(row(rows - 1) + NODE_HEIGHT + MARGIN, ...moved.map((item) => item.y + NODE_HEIGHT + MARGIN),
    ...edges.map((item) => item.label.y + LABEL_STACK));
  return { nodes: moved, links, edges, width, height };
}

/** Moves one node's drag offset, keeping it inside a fixed bound. Inputs: the offsets, the
 * node id and the move. Output: new offsets; the input is left unchanged. */
export function moveNode(offsets: Offsets, id: string, dx: number, dy: number): Offsets {
  const clamp = (value: number) => Math.min(OFFSET_MAX, Math.max(-OFFSET_MAX, Math.round(value)));
  const current = offsets.get(id) ?? { x: 0, y: 0 };
  const next = new Map(offsets);
  next.set(id, { x: clamp(current.x + dx), y: clamp(current.y + dy) });
  return next;
}

/** Drops drag offsets of nodes the graph no longer has. Inputs: the offsets and the graph.
 * Output: the offsets that still apply, the same object when nothing changed. */
export function keepOffsets(offsets: Offsets, graph: TeamGraph): Offsets {
  const ids = new Set(graph.nodes.map((node) => node.id));
  if ([...offsets.keys()].every((id) => ids.has(id))) return offsets;
  return new Map([...offsets].filter(([id]) => ids.has(id)));
}

/** Turns an arrow key into a node move. Inputs: the key and whether Shift is held. Output:
 * the move, or null for any other key. */
export function arrowMove(key: string, large: boolean): Point | null {
  const step = large ? ARROW_STEP_LARGE : ARROW_STEP;
  switch (key) {
    case "ArrowLeft": return { x: -step, y: 0 };
    case "ArrowRight": return { x: step, y: 0 };
    case "ArrowUp": return { x: 0, y: -step };
    case "ArrowDown": return { x: 0, y: step };
    default: return null;
  }
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
    || graph.edges.some((edge) => edge.state === "sent" || edge.state === "running");
}

/** The one line under the graph title: how many agents work and how many tasks are live.
 * Input: the graph. Output: the line. */
export function graphSummary(graph: TeamGraph): string {
  const agents = graph.nodes.filter((node) => node.kind === "agent");
  const working = agents.filter((node) => node.status === "working").length;
  const live = graph.edges.filter((edge) => edge.state === "sent" || edge.state === "running").length;
  return `${working} of ${agents.length} ${agents.length === 1 ? "agent" : "agents"} working. ${live} ${live === 1 ? "task" : "tasks"} live.`;
}
