import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { listAccounts, loadTeamGraph, loadTerminalsPerWindow, nextGeneration, openTerminalWindows } from "../../api";
import type { Result } from "../../api";
import { useLoad } from "../../state";
import { loadChanges } from "../../api-changes";
import { AGENT_MESSAGE_BYTES_MAX, failureCopy, receiptState, sendAgentMessage, stopTeamAgent, stopTeammate } from "../../api-projects";
import { HOOKLESS_WORDS, type GraphEdge, type GraphNode, type TeamGraph } from "../../engine-client";
import { Button, ConfirmDialog, Dialog, EmptyState, Icon, LoadError, Menu, NotReady, Notice, Skeleton, TextArea, navigate, type IconName, type MenuItem } from "../../ui";
import { actions } from "../changes/model";
import { displayText } from "../conversation";
import { AgentSkillsDialog } from "../process";
import { accountName } from "../project/research";
import {
  BADGE_HEIGHT, BADGE_WIDTH, DRAG_SLOP, LIVE_LABEL_WIDTH_MAX, ZOOM_STEP, badgeLabel, cardTask, cardText, cardTip, dotTone, fitView, fromOwner,
  graphLive, graphSummary, layoutCards, layoutGraph, namedAccounts, nodeActions, nodeDetail, nodeStatus, panView,
  receiptTicks, teamLine, tipPlace, viewKey, waitingOnOwner, wheelZoom, zoomView, type Link, type PlacedCard, type Size, type View,
} from "./graph";
import { openAllHint, wallPages, type PerWindow } from "../terminals/pages";
import { AgentSide } from "./agent-instructions";
import { NodeMessages } from "./agent-messages";
import "./team-graph.css";
import { markLearned } from "../dashboard/learn";

export { TerminalPanel } from "./terminal-panel";

export const LIVE_READ_MS = 2_000;
export const IDLE_READ_MS = 10_000;

const NODE_ICONS: Record<GraphNode["kind"], IconName> = {
  master: "sparkles",
  lead: "boxes",
  sub: "bot",
  teammate: "users",
};

const GRAPH_HINT = "Drag or use the arrow keys to move around. Pinch or Ctrl+scroll to zoom, 0 fits. Click a card to open its terminal; its menu shows its skill counts, sends a message, stops it or merges its work.";

const BYTES = new TextEncoder();

/** What a screen reader hears for one card. Inputs: the node, a harness name lookup and the
 * graph's edges. Output: one sentence naming it, its detail, branch, status, why it waits on
 * the owner, its task and what activating it does. */
export function nodeLabel(node: GraphNode, harness?: (id: string) => string, edges: readonly GraphEdge[] = []): string {
  const status = nodeStatus(node);
  const branch = node.branch?.trim() ? `branch ${node.branch.trim()}` : null;
  const parts = [node.label, nodeDetail(node, harness), branch, status?.word].filter(Boolean).join(", ");
  const waiting = waitingOnOwner(node);
  const task = ` ${cardTask(node, edges).replace(/\s+/g, " ").trim().replace(/[.!?]$/, "")}.`;
  return `${parts}.${waiting ? ` ${waiting.sentence}` : ""}${task}${node.terminal ? " Opens its terminal." : ""}`;
}

/** One fixed-size card: a presence dot and the clipped title, the role line, the branch, the
 * clipped task lines, and amber "Waiting on you" and the owner's unread messages when they
 * apply. Activating it opens the agent's terminal; hover or focus shows its full text; its
 * menu holds what the owner can do to it. Inputs: the placed card, the graph, a harness name
 * lookup, whether its terminal is open, the id of its shown hover text, the menu items, and
 * the open and hover actions. Output: the card. */
export function GraphCard({ placed, graph, harness, open, tipId, items, onOpen, onTip }: {
  placed: PlacedCard;
  graph: TeamGraph;
  harness?: (id: string) => string;
  open: boolean;
  tipId: string | null;
  items: MenuItem[];
  onOpen: (id: string) => void;
  onTip: (id: string | null) => void;
}) {
  const node = placed.node;
  const opens = node.terminal !== null;
  const waiting = waitingOnOwner(node);
  const unread = fromOwner(node, graph);
  const text = cardText({ ...node, label: displayText(node.label) }, graph.edges.map((edge) => ({ ...edge, text: displayText(edge.text) })), graph.nodes, harness);
  const chips = waiting || unread || node.message_note;
  return <div className={`tg-card ${node.kind}${open ? " open" : ""}${opens ? "" : " static"}${waiting ? " waiting" : ""}`}
    style={{ left: placed.x, top: placed.y, width: placed.width, height: placed.height }}
    onPointerEnter={() => onTip(node.id)} onPointerLeave={() => onTip(null)}>
    <button type="button" className="tg-card-open"
      aria-label={displayText(nodeLabel(node, harness, graph.edges))} aria-pressed={opens ? open : undefined}
      aria-describedby={tipId ?? undefined}
      onClick={() => { if (opens) onOpen(node.id); }}
      onFocus={() => onTip(node.id)} onBlur={() => onTip(null)}>
      <span className="tg-card-title">
        <span className={`tg-dot tone-${dotTone(node)}`} aria-hidden="true" />
        <Icon name={NODE_ICONS[node.kind] ?? "bot"} size={14} />
        <span className="tg-clip">{text.title}</span>
      </span>
      {!chips && <span className="tg-card-detail tg-clip">{displayText(text.detail)}</span>}
      {chips && <span className="tg-chips">
        {node.message_note && <span className="tg-chip warn note"><span className="tg-clip">{displayText(node.message_note)}</span></span>}
        {waiting && <span className="tg-chip warn">{waiting.word}</span>}
        {unread && <span className="tg-chip">From you {unread.unread}{node.hookless ? "" : ` ${receiptTicks(unread.receipt).ticks}`}</span>}
      </span>}
      {text.branch && <span className="tg-card-branch"><Icon name="git-branch" size={12} /><span className="tg-clip">{displayText(text.branch)}</span></span>}
      <span className="tg-card-task">{text.task.map((line, index) => <span key={index} className="tg-clip">{line}</span>)}</span>
    </button>
    {items.length > 0 && <span className="tg-card-menu"><Menu label={`Actions for ${displayText(node.label)}`} items={items} /></span>}
  </div>;
}

/** The row on a link above the card it leads to: the pair's label, marked live while an action
 * happens, then the link's badges, with the whole row's words for hover and screen readers.
 * Input: the link. Output: the row, or nothing when it has nothing to show. */
export function LinkBadges({ link }: { link: Link }) {
  const shown = displayText(link.label);
  const label = [shown, badgeLabel(link)].filter(Boolean).join(", ");
  if (!label) return null;
  return <span className={`tg-badges${link.live ? " live" : ""}`} role="img" aria-label={label} title={label}
    style={{ left: link.badge.x - BADGE_WIDTH / 2, top: link.badge.y - BADGE_HEIGHT / 2, width: BADGE_WIDTH, height: BADGE_HEIGHT }}>
    {shown && <span className={`tg-edge-label${link.live ? " live" : ""}`} style={link.live ? { maxWidth: LIVE_LABEL_WIDTH_MAX } : undefined}><span className="tg-clip">{shown}</span></span>}
    {link.tasks > 0 && <span className="tg-badge"><Icon name="list-checks" size={12} />{link.tasks}</span>}
    {link.questions > 0 && <span className="tg-badge question"><Icon name="message-square" size={12} />{link.questions}</span>}
    {link.unread > 0 && <span className="tg-badge message"><Icon name="send" size={12} />{link.unread}{link.receipt && !link.hookless ? ` ${receiptTicks(link.receipt).ticks}` : ""}</span>}
  </span>;
}

/** The key under the graph: the label each relation's line shows, and the marked label of a
 * live action. Output: the legend. */
export function EdgeLegend() {
  return <p className="tg-legend">
    <span>Edges:</span>
    {["leads", "delegated to", "teammate of"].map((word) => <span key={word} className="tg-edge-label">{word}</span>)}
    <span className="tg-edge-label live">live action, while it happens</span>
  </p>;
}

/** What the owner is doing from a card's menu: reading its agent's skill counts, writing a
 * message to its agent, or confirming a stop of its agent or of a native teammate's pane. */
type Acting =
  | { kind: "skills"; node: GraphNode; agent: number }
  | { kind: "message"; node: GraphNode; agent: number }
  | { kind: "stop"; node: GraphNode; stop: NonNullable<ReturnType<typeof nodeActions>["stop"]> };

type Receipted = Awaited<ReturnType<typeof stopTeamAgent>>;

/** The note Open all terminals leaves: none once the windows opened, else why not.
 * Input: the bridge's answer. Output: the note, or null. */
export function wallNote(result: Awaited<ReturnType<typeof openTerminalWindows>>): string | null {
  if (result.kind === "ok") return null;
  if (result.kind === "not_ready") return "Not ready yet: terminal windows aren't in this build.";
  return result.code === "terminal_windows_full"
    ? "Terminal windows are full: at most 8 open at once. Close some, or pick 9 terminals per window in Coordination."
    : `Couldn't open the terminal windows. ${result.message}`;
}

/** The note an owner action leaves: none once the engine applied it, else why it did not.
 * Inputs: the engine's answer and what the action was, for a build without it. Output: the
 * note, or null when it applied. */
export function actionNote(result: Receipted, what: string): string | null {
  if (result.kind !== "ok") return result.kind === "not_ready" ? `Not ready yet: ${what} isn't in this build.` : failureCopy(result);
  const state = receiptState(result.value);
  if (state.kind === "applied") return null;
  return state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the graph before trying again.";
}

/** The helper under the Message box: where the message goes and how its receipt shows, that
 * its receipts are unavailable when its launch fires no hooks, and the engine's sentence when
 * the Delivery setting leaves the agent no channel while it works. Input: the node. Output:
 * the helper text. */
export function messageHelper(node: Pick<GraphNode, "hookless" | "message_note">): string {
  if (node.message_note) return `${node.message_note} It waits in the agent's mailbox until then.`;
  return node.hookless
    ? `It goes to the agent's mailbox. Its receipts are ${HOOKLESS_WORDS}.`
    : "It goes to the agent's mailbox. Its receipt shows on the graph: one tick sent, two delivered, then read and answered.";
}

/** Why the owner's message cannot go yet: empty, or more bytes than the coordination layer
 * takes. Input: the draft. Output: the reason, or null when it can be sent. */
export function messageProblem(draft: string): string | null {
  if (!draft.trim()) return "Write a message first.";
  return BYTES.encode(draft.trim()).length > AGENT_MESSAGE_BYTES_MAX ? `Keep the message under ${AGENT_MESSAGE_BYTES_MAX} bytes.` : null;
}

/** The identity the one terminal panel is kept by: the agent's engine number when it has one,
 * so the same agent opened from Questions and then drawn by the graph keeps one panel, its
 * terminal and its draft; else the node id. Input: the node. Output: the key. */
export function panelKey(node: Pick<GraphNode, "agent" | "id">): string {
  return typeof node.agent === "number" ? `agent:${node.agent}` : `node:${node.id}`;
}

/** What the graph view shows: the read it draws, why the latest read failed while an older
 * graph stays on screen, and whether the latest read found the project removed. */
export type GraphState = { shown: Result<TeamGraph> | undefined; stale: string | null; gone: boolean };

export const GRAPH_UNREAD: GraphState = { shown: undefined, stale: null, gone: false };

const PROJECT_GONE = ["project_missing", "project_setup_missing"];

const OPEN_ALL_GONE = "Nothing to open: this project was removed.";

/** What Open all terminals does with its fresh reads: no window and a note when the team read
 * finds the project removed, so no window opens only to shut again; else the wall windows the
 * team needs at the Terminals per window read now, or the given one when that read failed, and
 * the count shown when the team read failed. Inputs: the two reads, the given Terminals per
 * window and the count shown. Output: the windows to open and the note, or null. */
export function openAllPlan(graph: Result<TeamGraph>, setting: Result<PerWindow>, perWindow: PerWindow, shown: number): { windows: number; note: string | null } {
  if (graph.kind === "error" && PROJECT_GONE.includes(graph.code)) return { windows: 0, note: OPEN_ALL_GONE };
  const windows = graph.kind === "ok" ? wallPages(graph.value.nodes, setting.kind === "ok" ? setting.value : perWindow).length : shown;
  if (!(Number.isSafeInteger(windows) && windows >= 0)) throw new Error(`Open all needs a whole number of windows: ${windows}`);
  return { windows, note: null };
}

/** Takes one graph read into the view. A failed read after a good one keeps the good graph,
 * so an open terminal and its unsent prompt stay, and says why the refresh failed; before
 * any good read the failure is what the view shows. A read that says the project is no longer
 * there is marked gone. Inputs: the current state and the read. Output: the next state. */
export function graphRead(state: GraphState, result: Result<TeamGraph>): GraphState {
  const gone = result.kind === "error" && PROJECT_GONE.includes(result.code);
  if (result.kind === "ok" || state.shown?.kind !== "ok") return { shown: result, stale: null, gone };
  return { shown: state.shown, stale: result.kind === "error" ? result.message : "The team graph isn't ready in this build.", gone };
}

/** Puts each account's name in place of its id in a graph read, so every view of the team
 * graph, the master's console and each terminal head included, names accounts as Accounts
 * does. Inputs: the graph state and the owner's account names. Output: the state with a good
 * graph's accounts named; any other state as it was. */
export function namedRead(state: GraphState, accounts: { id: string; name: string }[]): GraphState {
  if (state.shown?.kind !== "ok") return state;
  const value = namedAccounts(state.shown.value, (id) => accountName(accounts, id));
  return { ...state, shown: { ...state.shown, value } };
}

/** Reads one project's team graph, again every 2 s while work is live and every 10 s
 * otherwise, with each node's account named. Input: the project id. Output: the graph state
 * and a reload action that keeps the graph on screen while it reads. */
export function useTeamGraph(projectId: string): [GraphState, () => void] {
  const [state, setState] = useState<GraphState>(GRAPH_UNREAD);
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const [round, setRound] = useState(0);
  useEffect(() => setState(GRAPH_UNREAD), [projectId]);
  useEffect(() => {
    let current = true;
    let timer = 0;
    const read = async () => {
      const result = await loadTeamGraph(projectId);
      if (!current) return;
      setState((previous) => graphRead(previous, result));
      if (result.kind === "not_ready") return;
      timer = window.setTimeout(read, result.kind === "ok" && graphLive(result.value) ? LIVE_READ_MS : IDLE_READ_MS);
    };
    void read();
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [projectId, round]);
  const named = useMemo(() => namedRead(state, accounts.kind === "ok" ? accounts.value : []), [state, accounts]);
  return [named, () => setRound((value) => value + 1)];
}

/** The Agents toolbar, one row as the mockup draws it above both Agents views: `lead`, then
 * the team line (`line`, how many of the team's agents work and tasks are live once it is
 * read and has agents, what `asks` the owner and the `pool`), then `tools` and Open all
 * terminals, which reads the team and its Terminals per window again, so an older read closes
 * no window the team still needs, opens the terminal wall windows, perWindow terminals each,
 * and says why under the row when they could not open or the project was removed. Inputs: the project id and name, the
 * terminals per window, the drawn team or null while it is not read, the team line and the
 * row's parts. Output: the row and its note. */
export function AgentsToolbar({ projectId, name, perWindow, team, line, lead, asks, pool, tools }: {
  projectId: string; name: string; perWindow: PerWindow; team: TeamGraph | null; line: string | null;
  lead?: ReactNode; asks?: ReactNode; pool?: string; tools?: ReactNode;
}) {
  const [note, setNote] = useState<string | null>(null);
  useEffect(() => { setNote(null); }, [projectId]);
  if (line !== null && team === null) throw new Error("a team line comes from a read team");
  const walls = team === null ? null : wallPages(team.nodes, perWindow).length;
  if (walls !== null && !(Number.isSafeInteger(walls) && walls >= 0)) throw new Error(`a team needs a whole number of windows: ${walls}`);

  async function openAll(shown: number) {
    const [graph, setting] = await Promise.all([loadTeamGraph(projectId), loadTerminalsPerWindow(projectId)]);
    const plan = openAllPlan(graph, setting, perWindow, shown);
    if (plan.windows === 0) {
      if (plan.note !== null) setNote(plan.note);
      return;
    }
    const opened = await openTerminalWindows(projectId, name, plan.windows, true);
    if (opened.kind === "ok") markLearned("terminal");
    setNote(wallNote(opened));
  }

  const working = line !== null && team !== null && team.nodes.some((node) => node.kind !== "master") ? graphSummary(team) : null;
  const parts = [line && <span key="team" role="status">{line}</span>, working && <span key="working">{working}</span>, asks && <span key="asks">{asks}</span>,
    pool && <span key="pool">{displayText(pool)}</span>]
    .filter((part) => Boolean(part));
  return <>
    <div className="tg-head">
      {lead}
      <span className="tg-summary">{parts.flatMap((part, index) => index === 0 ? [part] : [<span key={`dot-${index}`} aria-hidden="true"> · </span>, part])}</span>
      {tools}
      <Button size="small" icon="square-terminal" className="tg-open-all" title={openAllHint(walls ?? 0, perWindow)} disabled={!walls}
        onClick={() => void openAll(walls ?? 0)}>Open all terminals</Button>
    </div>
    {note && <Notice tone="warn" alert>
      <span className="grow">{displayText(note)}</span>
      <Button size="small" variant="quiet" onClick={() => setNote(null)}>Dismiss</Button>
    </Notice>}
  </>;
}

/** The live team graph of one project as a tree of who leads whom (DEC-104 Q16): the master
 * on top, each lead under it and each lead's agents under that lead, joined by solid
 * assignment lines whose badges count live tasks, waiting questions and unread messages with
 * their receipt. Unread messages between other agents arc dashed and claim overlaps arc red.
 * Drag or the arrow keys pan, a pinch, Ctrl or Cmd wheel or the buttons zoom, and Fit shows it
 * all. Activating a card opens its terminal in the one panel beside the graph, replacing the
 * agent shown there; its menu sends a message, stops it, or opens its changes to merge. An
 * agent opened from elsewhere shows in that panel even while the graph loads, fails or is empty,
 * and keeps its terminal and draft when the graph then draws it. Open all terminals opens the
 * terminal wall windows, perWindow terminals each; once clicked, a team that needs more or
 * fewer windows opens or closes them while any stays open. The open panel's Stop asks as the
 * card menu's does. The head is the Agents toolbar, one row as the mockup draws it: `lead`,
 * then the team line ("Master and 5 agents", how many of them work and tasks are live, what
 * `asks` the owner and the `pool`), then `tools` and Open all terminals; it shows while the
 * graph loads, fails or is empty too.
 * Inputs: the project id and name, the terminals per window, the agent whose terminal is open
 * and the open action, an optional harness name lookup, and the head's parts. Accounts come
 * named from the graph read. Output: the graph section. */
export function TeamGraphView({ projectId, name = "Project", perWindow = 6, open, onOpen, harness, lead, asks, pool, tools }: {
  projectId: string; name?: string; perWindow?: PerWindow; open: GraphNode | null; onOpen: (node: GraphNode | null) => void;
  harness?: (id: string) => string; lead?: ReactNode; asks?: ReactNode; pool?: string; tools?: ReactNode;
}) {
  const [read, reload] = useTeamGraph(projectId);
  const graph = read.shown;
  const [moved, setMoved] = useState<View | null>(null);
  const [tip, setTip] = useState<string | null>(null);
  const [element, setElement] = useState<HTMLDivElement | null>(null);
  const [port, setPort] = useState<Size>({ width: 0, height: 0 });
  const sizes = useRef<{ content: Size; port: Size }>({ content: { width: 0, height: 0 }, port });
  const drag = useRef<{ pointer: number; x: number; y: number; moved: boolean } | null>(null);
  const panned = useRef(false);
  const ids = useId();
  const [acting, setActing] = useState<Acting | null>(null);
  const [draft, setDraft] = useState("");
  const [sendNote, setSendNote] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setMoved(null);
    setTip(null);
    setActing(null);
    setDraft("");
    setSendNote(null);
    setNote(null);
  }, [projectId]);

  async function send() {
    if (acting?.kind !== "message" || busy) return;
    const problem = messageProblem(draft);
    if (problem) return setSendNote(problem);
    setBusy(true);
    const result = await sendAgentMessage(projectId, acting.agent, draft.trim(), nextGeneration());
    setBusy(false);
    const failed = actionNote(result, "messaging an agent");
    setSendNote(failed);
    if (failed === null) {
      setActing(null);
      setDraft("");
    }
    reload();
  }

  async function halt() {
    if (acting?.kind !== "stop" || busy) return;
    const { stop } = acting;
    setBusy(true);
    const result = stop.kind === "agent"
      ? await stopTeamAgent(projectId, stop.agent, nextGeneration())
      : await stopTeammate(projectId, stop.agent, stop.pane, nextGeneration());
    setBusy(false);
    setActing(null);
    setNote(actionNote(result, "stopping an agent"));
    reload();
  }

  async function merge(node: GraphNode, agent: number) {
    if (busy) return;
    setBusy(true);
    const result = await loadChanges(projectId);
    setBusy(false);
    if (result.kind === "not_ready") return setNote("Not ready yet: reviewing changes isn't in this build.");
    if (result.kind === "error") return setNote(`Couldn't read the changes. ${result.message}`);
    const now = Date.now();
    const waiting = result.value.some((set) => set.agent.kind === "sub" && set.agent.id === String(agent) && actions(set, now).accept);
    if (!waiting) return setNote(`No changes from ${node.label} wait to be merged.`);
    setNote(null);
    navigate({ name: "project", id: projectId, tab: "changes" });
  }

  function menuItems(node: GraphNode, nodes: readonly GraphNode[]): MenuItem[] {
    const can = nodeActions(node, nodes);
    const items: MenuItem[] = [];
    const agent = node.agent;
    if (typeof agent === "number") {
      items.push({ label: "Skills", icon: "list-checks", onSelect: () => setActing({ kind: "skills", node, agent }) });
    }
    const message = can.message;
    if (message !== null) {
      items.push({ label: "Message", icon: "send", onSelect: () => {
        setDraft("");
        setSendNote(null);
        setActing({ kind: "message", node, agent: message });
      } });
    }
    const stop = can.stop;
    if (stop !== null) items.push({ label: "Stop", icon: "circle-stop", danger: true, onSelect: () => setActing({ kind: "stop", node, stop }) });
    const work = can.merge;
    if (work !== null) items.push({ label: "Merge", icon: "git-branch", onSelect: () => void merge(node, work) });
    return items;
  }

  useEffect(() => {
    if (!element) return;
    const measure = () => setPort({ width: element.clientWidth, height: element.clientHeight });
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(element);
    const wheel = (event: WheelEvent) => {
      const factor = wheelZoom(event);
      if (factor === null) return;
      event.preventDefault();
      const box = element.getBoundingClientRect();
      const { content, port: viewport } = sizes.current;
      setMoved((current) => zoomView(current ?? fitView(content, viewport), factor,
        { x: event.clientX - box.left, y: event.clientY - box.top }, content, viewport));
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => {
      observer?.disconnect();
      element.removeEventListener("wheel", wheel);
    };
  }, [element]);

  const team = graph?.kind === "ok" ? graph.value : null;
  const side = (node: GraphNode) => {
    const stop = nodeActions(node, team?.nodes ?? []).stop;
    const onStop = stop === null ? undefined : () => setActing({ kind: "stop", node, stop });
    return <AgentSide key={panelKey(node)} projectId={projectId} node={node} harness={harness} onClose={() => onOpen(null)} onStop={onStop}>
      <NodeMessages projectId={projectId} node={node} refresh={graph} />
    </AgentSide>;
  };
  const head = (line: string | null) => <>
    <h3 id={`${ids}-title`} className="pj-sr">Team graph</h3>
    <AgentsToolbar projectId={projectId} name={name} perWindow={perWindow} team={team} line={line} lead={lead} asks={asks} pool={pool} tools={tools} />
  </>;
  const status: ReactNode = graph === undefined ? <Skeleton shape="card" />
    : graph.kind === "not_ready" ? <NotReady what="The team graph" compact />
    : graph.kind === "error" ? <LoadError message={`Couldn't load the team graph. ${graph.message}`} onRetry={reload} />
    : team === null || team.nodes.length === 0 ? <EmptyState icon="users" title="No team yet" compact>
      Add a team in the project settings, then ask the master to build something.
    </EmptyState>
    : null;
  const noted = note && <Notice tone="warn" alert>
    <span className="grow">{displayText(note)}</span>
    <Button size="small" variant="quiet" onClick={() => setNote(null)}>Dismiss</Button>
  </Notice>;
  const dialogs = <>
    {acting?.kind === "skills" && <AgentSkillsDialog projectId={projectId} agent={acting.agent}
      label={displayText(acting.node.label)} onClose={() => setActing(null)} />}
    {acting?.kind === "message" && <Dialog open title={`Message ${displayText(acting.node.label)}`} busy={busy}
      onClose={() => { setActing(null); setSendNote(null); }}
      footer={<><Button disabled={busy} onClick={() => { setActing(null); setSendNote(null); }}>Cancel</Button>
        <Button variant="primary" icon="send" pending={busy} disabled={messageProblem(draft) !== null} onClick={() => void send()}>Send</Button></>}>
      <TextArea id={`${ids}-message`} label="Message" value={draft} onValue={(value) => { setDraft(value); setSendNote(null); }} rows={5}
        maxLength={AGENT_MESSAGE_BYTES_MAX} counterAt={AGENT_MESSAGE_BYTES_MAX - 400} error={sendNote ?? undefined}
        helper={displayText(messageHelper(acting.node))} />
    </Dialog>}
    {acting?.kind === "stop" && <ConfirmDialog open title={`Stop ${displayText(acting.node.label)}?`} confirmLabel="Stop" pending={busy}
      onConfirm={() => void halt()} onCancel={() => setActing(null)}>
      {acting.stop.kind === "teammate"
        ? "This teammate's pane closes now, and its lead is told. Its lead keeps running."
        : acting.node.kind === "teammate"
          ? "This teammate's pane closes now. Its lead keeps running."
          : "Its harness stops now. Work it has not handed in stays in its worktree, and you can wake it later from Finished agents."}
    </ConfirmDialog>}
  </>;
  if (team === null || status !== null) {
    return <section className="tg" aria-busy={graph === undefined || undefined} aria-label="Team graph">
      {head(team !== null && team.nodes.length > 0 ? teamLine(team) : null)}
      {noted}
      <div key="split" className={`tg-split${open ? " with-terminal" : ""}`}>
        {status}
        {open && side(open)}
      </div>
      {dialogs}
    </section>;
  }
  const layout = layoutGraph(team);
  const content = { width: layout.width, height: layout.height };
  sizes.current = { content, port };
  const shown = moved ?? fitView(content, port);
  const cards = layoutCards(layout);
  const openNode = open === null ? undefined : (cards.find((item) => item.node.id === open.id)
    ?? cards.find((item) => item.node.kind !== "teammate" && item.node.terminal === open.terminal))?.node ?? open;
  const tipCard = cards.find((item) => item.node.id === tip);
  const tipId = `${ids}-tip`;
  const pan = (dx: number, dy: number) => setMoved((current) => panView(current ?? shown, dx, dy, content, port));
  const zoom = (factor: number) => setMoved((current) => zoomView(current ?? shown, factor, { x: port.width / 2, y: port.height / 2 }, content, port));
  const place = tipCard && tipPlace(tipCard, shown, port);

  return <section className="tg" aria-labelledby={`${ids}-title`}>
    {head(teamLine(team, layout.hidden))}
    {noted}
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh the team graph. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    <p className="pj-sr" id={`${ids}-hint`}>{GRAPH_HINT}</p>
    <div key="split" className={`tg-split${openNode ? " with-terminal" : ""}`}>
      <div className="tg-stage">
        <div className="tg-tools" role="toolbar" aria-label="Graph view">
          <Button size="small" icon="minus" iconOnly="Zoom out" onClick={() => zoom(1 / ZOOM_STEP)} />
          <span className="tg-zoom">{Math.round(shown.zoom * 100)}%</span>
          <Button size="small" icon="plus" iconOnly="Zoom in" onClick={() => zoom(ZOOM_STEP)} />
          <Button size="small" variant="quiet" icon="square" onClick={() => setMoved(null)}>Fit</Button>
          <span className="tg-help" title={GRAPH_HINT} aria-hidden="true"><Icon name="info" size={14} /></span>
        </div>
        <div ref={setElement} className="tg-viewport" tabIndex={0} role="group" aria-label="Team graph view" aria-describedby={`${ids}-hint`}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            drag.current = { pointer: event.pointerId, x: event.clientX, y: event.clientY, moved: false };
            panned.current = false;
          }}
          onPointerMove={(event) => {
            const current = drag.current;
            if (!current || current.pointer !== event.pointerId) return;
            const dx = event.clientX - current.x;
            const dy = event.clientY - current.y;
            if (!current.moved && Math.hypot(dx, dy) < DRAG_SLOP) return;
            if (!current.moved) {
              event.currentTarget.setPointerCapture(event.pointerId);
              current.moved = true;
              panned.current = true;
              setTip(null);
            }
            current.x = event.clientX;
            current.y = event.clientY;
            pan(dx, dy);
          }}
          onPointerUp={() => { drag.current = null; }}
          onClickCapture={(event) => {
            if (panned.current && event.detail !== 0) event.stopPropagation();
            panned.current = false;
          }}
          onPointerCancel={() => { drag.current = null; }}
          onKeyDown={(event) => {
            const action = viewKey(event.key);
            if (!action || event.altKey || event.ctrlKey || event.metaKey) return;
            event.preventDefault();
            if (action.kind === "pan") pan(action.dx, action.dy);
            else if (action.kind === "zoom") zoom(action.factor);
            else setMoved(null);
          }}>
          <div className="tg-canvas" style={{ width: layout.width, height: layout.height, transform: `translate(${shown.x}px, ${shown.y}px) scale(${shown.zoom})`,
            visibility: port.width > 0 ? "visible" : "hidden", ["--tg-unzoom" as string]: String(1 / shown.zoom) }}>
            <svg className="tg-lines" width={layout.width} height={layout.height} aria-hidden="true">
              {layout.links.map((link) => <path key={`link:${link.from}:${link.to}`} className={`tg-link${link.live ? " live" : ""}`} d={link.path} />)}
              {layout.messages.map((item) => <path key={`message:${item.from}:${item.to}`} className="tg-message" d={item.path} />)}
              {layout.overlaps.map((item) => <path key={`overlap:${item.from}:${item.to}`} className={`tg-overlap${item.edge.stale ? " stale" : ""}`} d={item.path} />)}
            </svg>
            {cards.map((item) => <GraphCard key={item.node.id} placed={item} graph={team} harness={harness} open={item.node.id === openNode?.id}
              tipId={item.node.id === tip ? tipId : null} onTip={setTip} items={menuItems(item.node, team.nodes)}
              onOpen={() => onOpen(item.node)} />)}
            {layout.links.map((link) => <LinkBadges key={`badge:${link.from}:${link.to}`} link={link} />)}
          </div>
          {tipCard && place && <div id={tipId} role="tooltip" className={`tg-tip${place.above ? " above" : ""}`} style={{ left: place.left, top: place.top }}>
            {cardTip(tipCard.node, team, harness).map((line, index) => <p key={index} className={index === 0 ? "tg-tip-title" : undefined}>{displayText(line)}</p>)}
          </div>}
        </div>
        <EdgeLegend />
      </div>
      {openNode && side(openNode)}
    </div>
    {dialogs}
  </section>;
}
