import { HOOKLESS_WORDS, type GraphEdge, type GraphEdgeState, type GraphNode, type MessageEdge, type OverlapEdge, type Presence, type LiveAction, type Receipt, type RelationEdge, type TeamAgentStatus, type TeamGraph } from "../../engine-client";
import type { Tone } from "../../ui";

export const CARD_WIDTH = 216;
export const CARD_HEIGHT = 132;
export const CARD_GAP = 16;
export const LEVEL_GAP = 72;
export const ROW_GAP = 40;
export const LEAF_COLUMNS_MAX = 4;
export const ARC_RISE = 40;
export const MARGIN = 24;
export const NODES_MAX = 64;
export const BADGE_WIDTH = CARD_WIDTH;
export const BADGE_HEIGHT = 24;
export const TITLE_CHARS = 22;
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
export const OWNER_PARTY = "owner";
export const ENGINE_PARTY = "engine";
const WHEEL_ZOOM_RATE = 0.01;

export interface Point { x: number; y: number }
export interface Size { width: number; height: number }
export interface Box extends Point, Size {}

/** Where the canvas sits in the viewport: its offset in screen pixels and its zoom. */
export interface View { x: number; y: number; zoom: number }

export const VIEW_HOME: View = { x: 0, y: 0, zoom: 1 };

export interface PlacedCard extends Box { node: GraphNode }

/** The solid assignment line from a lead's card down to one card it leads, with its badge
 * row above that card: the pair's label, live while an action happens on it, then the live
 * tasks the card's agent runs, the questions it waits on, and the messages the lead sent it
 * that are still unread, with the newest one's receipt, unless the card's agent fires no hooks
 * and has none. Unread messages the other way, to the lead, arc like any other pair's. */
export interface Link {
  from: string;
  to: string;
  path: string;
  badge: Point;
  label: string;
  live: boolean;
  tasks: number;
  questions: number;
  unread: number;
  receipt: Receipt | null;
  hookless: boolean;
}

/** A line between two cards that are not lead and led: unread messages (dashed) or a claim
 * overlap (red). It arcs over both cards so it never runs under one. */
export interface Arc { from: string; to: string; path: string }

/** A team graph laid out for drawing: the cards of the who-leads-whom tree, the assignment
 * links, the message and overlap arcs, the canvas size that holds them all, and how many
 * agents the `NODES_MAX` cap left out. */
export interface GraphLayout extends Size {
  cards: PlacedCard[];
  links: Link[];
  messages: (Arc & { edge: MessageEdge })[];
  overlaps: (Arc & { edge: OverlapEdge })[];
  hidden: number;
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
const running = (node: GraphNode) => node.status === "starting" || node.status === "idle" || node.status === "working";

/** Which agents the graph keeps first when it cannot draw them all: waiting on the owner,
 * running, then waiting on questions, then failed, then stopped. Input: an agent node.
 * Output: the rank, lowest first. */
function agentRank(node: GraphNode): number {
  if (waitingOnOwner(node)) return 0;
  if (running(node)) return 1;
  if (questionCount(node) > 0) return 2;
  if (node.status === "failed") return 3;
  return node.status === "stopped" ? 4 : 5;
}

/** The questions one node's agent waits on, as the engine reports them; anything but a
 * positive whole number counts as none. Input: the node. Output: the count. */
export function questionCount(node: GraphNode): number {
  const count = node.questions;
  return typeof count === "number" && Number.isInteger(count) && count > 0 ? count : 0;
}

const count = (value: unknown) => typeof value === "number" && Number.isInteger(value) && value > 0 ? value : 0;

/** Why a node waits on the owner, shown in amber (DEC-104 Q16, Q17): a permission prompt or
 * a question in its own terminal, open questions for the owner, or a teammate working with
 * no worktree of its own. Input: the node. Output: the short words and the full sentence, or
 * null when it waits on nothing. */
export function waitingOnOwner(node: GraphNode): { word: string; sentence: string } | null {
  if (node.waiting === "permission") return { word: "Waiting on you", sentence: "Waiting on you: it asks for permission in its terminal." };
  if (node.waiting === "question") return { word: "Waiting on you", sentence: "Waiting on you: it asks a question in its terminal." };
  const asked = count(node.owner_questions);
  if (asked > 0) return { word: "Waiting on you", sentence: `Waiting on you: ${plural(asked, "question")} for you in Questions.` };
  if (node.kind === "teammate" && node.no_worktree === true) return { word: "No worktree", sentence: "This teammate works with no worktree of its own." };
  return null;
}

const PRESENCE: Record<Presence, { word: string; tone: Tone }> = {
  live: { word: "Live", tone: "ok" },
  idle: { word: "Idle", tone: "info" },
  away: { word: "Away", tone: "neutral" },
  stopped: { word: "Stopped", tone: "neutral" },
};

/** The colour of a card's dot: amber while it waits on the owner, else its presence, else the
 * tone of its status. Input: the node. Output: the tone. */
export function dotTone(node: GraphNode): Tone {
  if (waitingOnOwner(node)) return "warn";
  if (node.presence && Object.hasOwn(PRESENCE, node.presence)) return PRESENCE[node.presence].tone;
  return nodeStatus(node)?.tone ?? "neutral";
}

/** A node's presence word, such as "Live", or null before the engine reports one. Input:
 * the node. Output: the word. */
export function presenceWord(node: GraphNode): string | null {
  return node.presence && Object.hasOwn(PRESENCE, node.presence) ? PRESENCE[node.presence].word : null;
}

const TICKS: Record<Receipt, { ticks: string; word: string }> = {
  sent: { ticks: "\u2713", word: "sent" },
  delivered: { ticks: "\u2713\u2713", word: "delivered" },
  typed: { ticks: "\u2713\u2713", word: "typed into its terminal" },
  read: { ticks: "\u2713\u2713", word: "read" },
  replied: { ticks: "\u2713\u2713", word: "answered" },
  failed: { ticks: "!", word: "not delivered" },
  expired: { ticks: "!", word: "expired unread" },
};

/** The ticks and words of a message receipt, like message apps show. Input: the receipt.
 * Output: the tick marks and what they mean. */
export function receiptTicks(receipt: Receipt): { ticks: string; word: string } {
  return Object.hasOwn(TICKS, receipt) ? TICKS[receipt] : TICKS.sent;
}

/** What a receipt to one node shows: its ticks and words, or no ticks and the hookless words
 * when the node's launch fires no hooks (DEC-104 bare-hooks). Inputs: the receipt and whether
 * the node is hookless. Output: the tick marks and what they mean. */
export function receiptShown(receipt: Receipt, hookless: boolean): { ticks: string; word: string } {
  return hookless ? { ticks: "", word: `receipts ${HOOKLESS_WORDS}` } : receiptTicks(receipt);
}

/** The messages the owner sent one node that it has not read yet, with the newest one's
 * receipt. Inputs: the node and the graph. Output: the count and receipt, or null for none. */
export function fromOwner(node: GraphNode, graph: TeamGraph): { unread: number; receipt: Receipt } | null {
  const edge = (graph.messages ?? []).find((item) => item.from === OWNER_PARTY && item.to === node.id && count(item.unread) > 0);
  return edge ? { unread: count(edge.unread), receipt: edge.receipt } : null;
}

function elbow(from: Point, to: Point, bus: number): string {
  return `M ${from.x} ${from.y} V ${bus} H ${to.x} V ${to.y}`;
}

function trunk(from: Point, to: Point, bus: number, side: number, row: number): string {
  return `M ${from.x} ${from.y} V ${bus} H ${side} V ${row} H ${to.x} V ${to.y}`;
}

function arc(a: Box, b: Box): string {
  const from = { x: a.x + a.width / 2, y: a.y };
  const to = { x: b.x + b.width / 2, y: b.y };
  const top = Math.min(from.y, to.y) - ARC_RISE;
  return `M ${from.x} ${from.y} C ${from.x} ${top}, ${to.x} ${top}, ${to.x} ${to.y}`;
}

/** Lays out one project's who-leads-whom graph: the master on top, the agents it leads in a
 * row under it, and each lead's agents under that lead, every subtree centred over its own.
 * More than `LEAF_COLUMNS_MAX` agents that lead nobody wrap into rows under their lead, fed by
 * a line beside them. Cards never share space. Past `NODES_MAX` the master stays and agents
 * are kept by `agentRank`, newest first; one whose lead was left out hangs under the nearest
 * kept lead, else the master. Input: the graph. Output: the layout, the same for the same graph. */
export function layoutGraph(graph: TeamGraph): GraphLayout {
  const seen = new Set<string>();
  const unique = graph.nodes.filter((node) => !seen.has(node.id) && Boolean(seen.add(node.id)));
  const masters = unique.filter((node) => node.kind === "master").sort(byNode).slice(0, NODES_MAX);
  const ranked = unique.filter((node) => node.kind !== "master")
    .map((node, order) => ({ node, order }))
    .sort((a, b) => agentRank(a.node) - agentRank(b.node) || b.order - a.order)
    .map((item) => item.node);
  const kept = new Set(ranked.slice(0, NODES_MAX - masters.length).map((node) => node.id));
  const hidden = ranked.length - kept.size;
  const byId = new Map(unique.map((node) => [node.id, node]));
  const masterIds = new Set(masters.map((node) => node.id));
  const leadOf = (node: GraphNode): string | null => {
    const walked = new Set<string>([node.id]);
    let parent = node.parent;
    while (parent !== null && !walked.has(parent)) {
      if (kept.has(parent) || masterIds.has(parent)) return parent;
      walked.add(parent);
      parent = byId.get(parent)?.parent ?? null;
    }
    return masters[0]?.id ?? null;
  };
  const shown = unique.filter((node) => kept.has(node.id));
  const children = new Map<string | null, GraphNode[]>();
  for (const node of shown) {
    const lead = leadOf(node);
    children.set(lead, [...(children.get(lead) ?? []), node]);
  }
  const roots = masters.length ? masters : children.get(null) ?? [];
  const tree = new Map<string, GraphNode[]>();
  const reached = new Set(roots.map((node) => node.id));
  const queue = [...roots];
  for (let next = queue.shift(); next; next = queue.shift()) {
    const list = (children.get(next.id) ?? []).filter((child) => !reached.has(child.id));
    for (const child of list) reached.add(child.id);
    tree.set(next.id, list);
    queue.push(...list);
  }
  const stray = shown.filter((node) => !reached.has(node.id));
  if (roots[0]) tree.set(roots[0].id, [...(tree.get(roots[0].id) ?? []), ...stray]);
  else roots.push(...stray);
  for (const node of stray) {
    reached.add(node.id);
    tree.set(node.id, []);
  }
  const kids = (node: GraphNode) => tree.get(node.id) ?? [];
  const leaves = (list: GraphNode[]) => list.length > LEAF_COLUMNS_MAX && list.every((child) => kids(child).length === 0);
  const widths = new Map<string, number>();
  const measure = (node: GraphNode, depth: number): number => {
    assert(depth <= NODES_MAX, "the tree is no deeper than the nodes it holds");
    const list = kids(node);
    const inner = leaves(list)
      ? span(LEAF_COLUMNS_MAX, CARD_WIDTH)
      : list.reduce((sum, child) => sum + measure(child, depth + 1), 0) + Math.max(0, list.length - 1) * CARD_GAP;
    const width = Math.max(CARD_WIDTH, inner);
    widths.set(node.id, width);
    return width;
  };
  const rise = (graph.messages?.length || graph.overlaps?.length) ? ARC_RISE : 0;
  const cards: PlacedCard[] = [];
  const links: Link[] = [];
  const edges = graph.edges;
  const unread = (from: string, to: string) => (graph.messages ?? []).find((edge) => edge.from === from && edge.to === to && count(edge.unread) > 0);
  const link = (lead: PlacedCard, card: PlacedCard, path: string, badge: number) => {
    const message = unread(lead.node.id, card.node.id);
    const pair = (graph.relations ?? []).find((item) => item.from === lead.node.id && item.to === card.node.id);
    links.push({
      from: lead.node.id, to: card.node.id, path,
      badge: { x: card.x + CARD_WIDTH / 2, y: badge },
      label: pair ? edgeLabel(pair) : "",
      live: pair !== undefined && liveShown(pair.live),
      tasks: edges.filter((edge) => live(edge) && edge.to === card.node.id).length,
      questions: questionCount(card.node),
      unread: message ? count(message.unread) : 0,
      receipt: message?.receipt ?? null,
      hookless: card.node.hookless === true,
    });
  };
  const place = (node: GraphNode, left: number, top: number, depth: number): PlacedCard => {
    assert(depth <= NODES_MAX, "the tree is no deeper than the nodes it holds");
    const width = widths.get(node.id) ?? CARD_WIDTH;
    const card = { node, x: left + (width - CARD_WIDTH) / 2, y: top, width: CARD_WIDTH, height: CARD_HEIGHT };
    cards.push(card);
    const list = kids(node);
    const below = top + CARD_HEIGHT + LEVEL_GAP;
    const from = { x: card.x + CARD_WIDTH / 2, y: top + CARD_HEIGHT };
    const bus = top + CARD_HEIGHT + LEVEL_GAP / 2;
    if (leaves(list)) {
      const start = left + (width - span(LEAF_COLUMNS_MAX, CARD_WIDTH)) / 2;
      for (const [index, child] of list.entries()) {
        const row = Math.floor(index / LEAF_COLUMNS_MAX);
        const placed = place(child, start + (index % LEAF_COLUMNS_MAX) * (CARD_WIDTH + CARD_GAP), below + row * (CARD_HEIGHT + ROW_GAP), depth + 1);
        const to = { x: placed.x + CARD_WIDTH / 2, y: placed.y };
        if (row === 0) link(card, placed, elbow(from, to, bus), (bus + placed.y) / 2);
        else link(card, placed, trunk(from, to, bus, start - CARD_GAP / 2, placed.y - ROW_GAP / 2), placed.y - ROW_GAP / 2);
      }
    } else {
      let x = left + (width - (list.reduce((sum, child) => sum + (widths.get(child.id) ?? CARD_WIDTH), 0) + Math.max(0, list.length - 1) * CARD_GAP)) / 2;
      for (const child of list) {
        const placed = place(child, x, below, depth + 1);
        link(card, placed, elbow(from, { x: placed.x + CARD_WIDTH / 2, y: placed.y }, bus), (bus + placed.y) / 2);
        x += (widths.get(child.id) ?? CARD_WIDTH) + CARD_GAP;
      }
    }
    return card;
  };
  let left = MARGIN;
  for (const root of roots) {
    measure(root, 0);
    place(root, left, MARGIN + rise, 0);
    left += (widths.get(root.id) ?? CARD_WIDTH) + CARD_GAP * 2;
  }
  const placedIds = new Set(cards.map((card) => card.node.id));
  assert(placedIds.size === cards.length && cards.length === reached.size, "every kept node is placed once");
  const at = new Map(cards.map((card) => [card.node.id, card]));
  const linked = new Set(links.map((item) => `${item.from}>${item.to}`));
  const messages = (graph.messages ?? []).flatMap((edge) => {
    const a = at.get(edge.from);
    const b = at.get(edge.to);
    if (!a || !b || a === b || linked.has(`${edge.from}>${edge.to}`) || count(edge.unread) === 0) return [];
    return [{ from: edge.from, to: edge.to, path: arc(a, b), edge }];
  });
  const overlaps = (graph.overlaps ?? []).flatMap((edge) => {
    const a = at.get(edge.from);
    const b = at.get(edge.to);
    return a && b && a !== b ? [{ from: edge.from, to: edge.to, path: arc(a, b), edge }] : [];
  });
  const width = cards.length ? Math.max(...cards.map((card) => card.x + card.width)) + MARGIN : MARGIN * 2;
  const height = cards.length ? Math.max(...cards.map((card) => card.y + card.height)) + MARGIN : MARGIN * 2;
  return { cards, links, messages, overlaps, width, height, hidden };
}

/** Every card the layout draws, leads before the agents they lead. Input: the layout.
 * Output: the cards in drawing order. */
export function layoutCards(layout: GraphLayout): PlacedCard[] {
  return layout.cards;
}

const RELATION_WORDS = new Map([["leads", "leads"], ["delegated_to", "delegated to"], ["teammate_of", "teammate of"]]);
const LIVE_ACTIONS = new Set(["handing_off", "messaging", "waiting_on", "reviewing"]);

/** Whether a line shows a live action: one this build knows, so an action from a newer engine
 * leaves the line on its relation. Input: the engine's live action. Output: true when shown. */
function liveShown(live: LiveAction | null): live is LiveAction {
  return live !== null && LIVE_ACTIONS.has(live.action);
}

/** The words one graph line shows, as the TUI tree shows them: the live action while it
 * happens, a handoff naming its task's first line, a wait on the owner naming the owner's
 * answer and a review the agent whose change set it reviews, else the relation. Input: the
 * engine's relation edge. Output: the label, empty for a relation this build does not know. */
export function edgeLabel(edge: RelationEdge): string {
  const live = edge.live;
  if (!liveShown(live)) return RELATION_WORDS.get(edge.relation) ?? "";
  if (live.action === "messaging") return "messaging";
  if (live.action === "waiting_on") return live.owner === true ? "waiting on: your answer" : "waiting on";
  if (live.action === "reviewing") {
    const name = typeof live.agent === "string" ? live.agent.split(/\s+/).filter(Boolean).join(" ") : "";
    return name ? `reviewing: ${name}'s change set` : "reviewing: change set";
  }
  const task = live.task.split("\n").map((line) => line.split(/\s+/).filter(Boolean).join(" ")).find(Boolean);
  return `handing off: ${task ?? "Untitled task"}`;
}

/** What a link's badges say to a screen reader. Input: the counts. Output: one phrase, empty
 * when there is nothing to show. */
export function badgeLabel(link: Pick<Link, "tasks" | "questions"> & Partial<Pick<Link, "unread" | "receipt" | "hookless">>): string {
  const parts = [];
  if (link.tasks > 0) parts.push(plural(link.tasks, "live task"));
  if (link.questions > 0) parts.push(`${plural(link.questions, "question")} waiting`);
  if ((link.unread ?? 0) > 0) parts.push(`${plural(link.unread ?? 0, "unread message")}${link.receipt ? `, ${receiptShown(link.receipt, link.hookless === true).word}` : ""}`);
  return parts.join(", ");
}

/** What the agents a card's menu acts on: Message the node's own agent, Stop its agent or,
 * for a native teammate known only by its pane, that pane in its lead's terminal, and Merge
 * a lead's or sub-agent's work through Changes. The master is messaged but never stopped or
 * merged here. Inputs: the node and the graph's nodes. Output: what each item acts on, null
 * where the item does not apply. */
export function nodeActions(node: GraphNode, nodes: readonly GraphNode[]): {
  message: number | null;
  stop: { kind: "agent"; agent: number } | { kind: "teammate"; agent: number; pane: string } | null;
  merge: number | null;
} {
  const own = typeof node.agent === "number" && Number.isInteger(node.agent) && node.agent > 0 ? node.agent : null;
  const ended = node.status === "stopped" || node.status === "failed";
  const lead = nodes.find((item) => item.id === node.parent);
  const leadAgent = typeof lead?.agent === "number" && lead.agent > 0 ? lead.agent : null;
  let stop: ReturnType<typeof nodeActions>["stop"] = null;
  if (node.kind !== "master" && !ended && own !== null) stop = { kind: "agent", agent: own };
  else if (node.kind === "teammate" && !ended && own === null && node.pane && leadAgent !== null) stop = { kind: "teammate", agent: leadAgent, pane: node.pane };
  return {
    message: own,
    stop,
    merge: (node.kind === "lead" || node.kind === "sub") && own !== null ? own : null,
  };
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
 * agent; for an agent, its latest task, led by Done or Failed once it ended. Either is led by
 * the file the node waits for under Wait (OS-8). Inputs: the node and the edges. Output: the
 * full line, before clipping. */
export function cardTask(node: GraphNode, edges: readonly GraphEdge[]): string {
  const line = taskLine(node, edges);
  return node.waits_for ? `Waiting for ${node.waits_for}. ${line}` : line;
}

function taskLine(node: GraphNode, edges: readonly GraphEdge[]): string {
  if (node.kind === "master") {
    const count = edges.filter(live).length;
    const waiting = edges.filter((edge) => live(edge) && edge.to === node.id).length;
    return `${plural(count, "task")} live${waiting ? `, ${waiting} waiting for an agent` : ""}.`;
  }
  const task = latestTask(node, edges);
  if (!task) return node.kind === "teammate" ? "Works beside its lead, in its lead's terminal." : "No task yet.";
  return live(task) ? task.text : `${edgeStatus(task).word}: ${task.text}`;
}

/** The clipped text of one card: the title, the role, harness and account line, the branch
 * line when the agent has a branch, and up to `TASK_LINES` task lines. Inputs: the node, the
 * edges and a harness name lookup. Output: the card text. */
export function cardText(node: GraphNode, edges: readonly GraphEdge[], harness?: (id: string) => string):
  { title: string; detail: string; branch: string | null; task: string[] } {
  const branch = node.branch?.trim();
  return {
    title: clipLines(node.label, TITLE_CHARS, 1)[0] ?? "",
    detail: clipLines(nodeDetail(node, harness), LINE_CHARS, 1)[0] ?? "",
    branch: branch ? clipChars(branch, LINE_CHARS) : null,
    task: clipLines(cardTask(node, edges), LINE_CHARS, TASK_LINES),
  };
}

/** The full text a card shows on hover: title, role line, branch, status and presence, that
 * its receipts are unavailable when its launch fires no hooks, the engine's sentence when the
 * Delivery setting leaves it no way to get messages, why it waits on
 * the owner, the messages it has not read and who sent them, its claim overlaps, its task and
 * the reply once it came, each kept to `TIP_CHARS`. Inputs: the node, the graph and a harness
 * name lookup. Output: the lines. */
export function cardTip(node: GraphNode, graph: TeamGraph, harness?: (id: string) => string): string[] {
  const reply = node.kind === "master" ? null : latestTask(node, graph.edges)?.reply;
  const status = [nodeStatus(node)?.word, presenceWord(node)].filter(Boolean).join(", ");
  const unread = (graph.messages ?? []).filter((edge) => edge.to === node.id && count(edge.unread) > 0)
    .map((edge) => `${plural(count(edge.unread), "unread message")} from ${partyName(graph, edge.from)}, ${receiptShown(edge.receipt, node.hookless === true).word}.`);
  const overlaps = (graph.overlaps ?? []).filter((edge) => edge.from === node.id || edge.to === node.id)
    .map((edge) => `${edge.stale ? "Stale overlap" : "Overlap"} with ${labelOf(graph, edge.from === node.id ? edge.to : edge.from)}: ${edge.reason}`);
  return [
    node.label,
    nodeDetail(node, harness),
    node.branch?.trim() ? `Branch ${node.branch.trim()}` : undefined,
    status || undefined,
    node.hookless ? `Receipts ${HOOKLESS_WORDS}.` : undefined,
    node.message_note ?? undefined,
    waitingOnOwner(node)?.sentence,
    ...unread,
    ...overlaps,
    cardTask(node, graph.edges),
    reply ? `Reply: ${reply}` : undefined,
  ]
    .filter((line): line is string => Boolean(line))
    .map((line) => clipChars(line, TIP_CHARS));
}

function labelOf(graph: TeamGraph, id: string): string {
  return graph.nodes.find((node) => node.id === id)?.label ?? id;
}

function partyName(graph: TeamGraph, id: string): string {
  if (id === OWNER_PARTY) return "you";
  return id === ENGINE_PARTY ? "CrazyAgent" : labelOf(graph, id);
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
 * fixed word for the status. A native teammate known only by its pane has a label and no
 * status. Input: the node. Output: word and tone, or null for none. */
export function nodeStatus(node: GraphNode): { word: string; tone: Tone } | null {
  if (node.status === null) return node.status_label?.trim() ? { word: node.status_label.trim(), tone: "info" } : null;
  const fixed = AGENT_WORDS[node.status];
  return { word: node.status_label?.trim() || fixed.word, tone: fixed.tone };
}

/** The status word and tone of one task edge. Input: the edge. Output: word and tone. */
export function edgeStatus(edge: GraphEdge): { word: string; tone: Tone } {
  return EDGE_WORDS[edge.state];
}


const ROLE_WORDS: Record<GraphNode["kind"], string> = {
  master: "Master",
  lead: "Lead",
  sub: "Sub-agent",
  teammate: "Teammate",
};

/** A node's place in the tree as one word, such as "Sub-agent", and "Agent" for a kind an
 * older or newer engine sends that this build does not know. Input: the node. Output: the word. */
export function nodeRole(node: Pick<GraphNode, "kind">): string {
  return Object.hasOwn(ROLE_WORDS, node.kind) ? ROLE_WORDS[node.kind] : "Agent";
}

/** The second line of a node box: its place in the tree, its harness and its account, such
 * as "Lead, Claude Code, Work". Inputs: the node and a harness display name lookup. Output:
 * one short line; a node with no account leaves that part out. */
export function nodeDetail(node: GraphNode, harness: (id: string) => string = (id) => id): string {
  return [nodeRole(node), harness(node.harness), node.account?.trim() || null].filter(Boolean).join(", ");
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

/** The one line under the graph title: how many agents work, how many tasks are live and how
 * many agents the drawing leaves out. Inputs: the graph and the layout's hidden count.
 * Output: the line. */
export function graphSummary(graph: TeamGraph, hidden = 0): string {
  const agents = graph.nodes.filter((node) => node.kind !== "master");
  const working = agents.filter((node) => node.status === "working").length;
  const tasks = graph.edges.filter(live).length;
  const waiting = graph.nodes.filter((node) => waitingOnOwner(node)?.word === "Waiting on you").length;
  const asks = waiting > 0 ? ` ${waiting} waiting on you.` : "";
  const left = hidden > 0 ? ` ${plural(hidden, "agent")} not shown.` : "";
  return `${working} of ${plural(agents.length, "agent")} working. ${plural(tasks, "task")} live.${asks}${left}`;
}
