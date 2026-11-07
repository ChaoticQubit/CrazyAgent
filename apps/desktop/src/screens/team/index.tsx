import { useEffect, useId, useRef, useState } from "react";
import { loadTeamGraph, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { loadChanges } from "../../api-changes";
import { AGENT_MESSAGE_BYTES_MAX, failureCopy, receiptState, sendAgentMessage, stopTeamAgent, stopTeammate } from "../../api-projects";
import { HOOKLESS_WORDS, type GraphEdge, type GraphNode, type TeamGraph } from "../../engine-client";
import { Button, ConfirmDialog, Dialog, EmptyState, Icon, LoadError, Menu, NotReady, Notice, Skeleton, TextArea, navigate, type IconName, type MenuItem } from "../../ui";
import { actions } from "../changes/model";
import { displayText } from "../conversation";
import { AgentSkillsDialog } from "../process";
import {
  BADGE_HEIGHT, BADGE_WIDTH, DRAG_SLOP, ZOOM_STEP, badgeLabel, cardTask, cardText, cardTip, dotTone, fitView, fromOwner,
  graphLive, graphSummary, layoutCards, layoutGraph, namedAccounts, nodeActions, nodeDetail, nodeStatus, panView, questionCount,
  receiptTicks, tipPlace, viewKey, waitingOnOwner, wheelZoom, zoomView, type Link, type PlacedCard, type Size, type View,
} from "./graph";
import { AgentSide } from "./agent-instructions";
import { NodeMessages } from "./agent-messages";
import "./team-graph.css";

export { TerminalPanel } from "./terminal-panel";

export const LIVE_READ_MS = 2_000;
export const IDLE_READ_MS = 10_000;

const NODE_ICONS: Record<GraphNode["kind"], IconName> = {
  master: "sparkles",
  lead: "boxes",
  sub: "bot",
  teammate: "users",
};

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
  const text = cardText({ ...node, label: displayText(node.label) }, graph.edges.map((edge) => ({ ...edge, text: displayText(edge.text) })), harness);
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
      <span className="tg-card-detail tg-clip">{displayText(text.detail)}</span>
      {text.branch && <span className="tg-card-branch"><Icon name="git-branch" size={12} /><span className="tg-clip">{displayText(text.branch)}</span></span>}
      <span className="tg-card-task">{text.task.map((line, index) => <span key={index} className="tg-clip">{line}</span>)}</span>
      {(waiting || unread || node.message_note) && <span className="tg-chips">
        {node.message_note && <span className="tg-chip warn note"><span className="tg-clip">{displayText(node.message_note)}</span></span>}
        {waiting && <span className="tg-chip warn">{waiting.word}</span>}
        {unread && <span className="tg-chip">From you {unread.unread}{node.hookless ? "" : ` ${receiptTicks(unread.receipt).ticks}`}</span>}
      </span>}
    </button>
    {items.length > 0 && <span className="tg-card-menu"><Menu label={`Actions for ${displayText(node.label)}`} items={items} /></span>}
  </div>;
}

function LinkBadges({ link }: { link: Link }) {
  const label = badgeLabel(link);
  if (!label) return null;
  return <span className="tg-badges" role="img" aria-label={label} title={label}
    style={{ left: link.badge.x - BADGE_WIDTH / 2, top: link.badge.y - BADGE_HEIGHT / 2, width: BADGE_WIDTH, height: BADGE_HEIGHT }}>
    {link.tasks > 0 && <span className="tg-badge"><Icon name="list-checks" size={12} />{link.tasks}</span>}
    {link.questions > 0 && <span className="tg-badge question"><Icon name="message-square" size={12} />{link.questions}</span>}
    {link.unread > 0 && <span className="tg-badge message"><Icon name="send" size={12} />{link.unread}{link.receipt && !link.hookless ? ` ${receiptTicks(link.receipt).ticks}` : ""}</span>}
  </span>;
}

/** What the owner is doing from a card's menu: reading its agent's skill counts, writing a
 * message to its agent, or confirming a stop of its agent or of a native teammate's pane. */
type Acting =
  | { kind: "skills"; node: GraphNode; agent: number }
  | { kind: "message"; node: GraphNode; agent: number }
  | { kind: "stop"; node: GraphNode; stop: NonNullable<ReturnType<typeof nodeActions>["stop"]> };

type Receipted = Awaited<ReturnType<typeof stopTeamAgent>>;

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
 * the Delivery setting leaves the agent no way to get it yet. Input: the node. Output: the
 * helper text. */
export function messageHelper(node: Pick<GraphNode, "hookless" | "message_note">): string {
  if (node.message_note) return `${node.message_note} It waits in the agent's mailbox until tools are on or message delivery changes.`;
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

/** What the graph view shows: the read it draws, and why the latest read failed while an
 * older graph stays on screen. */
export type GraphState = { shown: Result<TeamGraph> | undefined; stale: string | null };

export const GRAPH_UNREAD: GraphState = { shown: undefined, stale: null };

/** Takes one graph read into the view. A failed read after a good one keeps the good graph,
 * so an open terminal and its unsent prompt stay, and says why the refresh failed; before
 * any good read the failure is what the view shows. Inputs: the current state and the read.
 * Output: the next state. */
export function graphRead(state: GraphState, result: Result<TeamGraph>): GraphState {
  if (result.kind === "ok" || state.shown?.kind !== "ok") return { shown: result, stale: null };
  return { shown: state.shown, stale: result.kind === "error" ? result.message : "The team graph isn't ready in this build." };
}

/** Reads one project's team graph, again every 2 s while work is live and every 10 s
 * otherwise. Input: the project id. Output: the graph state and a reload action that keeps
 * the graph on screen while it reads. */
export function useTeamGraph(projectId: string): [GraphState, () => void] {
  const [state, setState] = useState<GraphState>(GRAPH_UNREAD);
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
  return [state, () => setRound((value) => value + 1)];
}

/** The live team graph of one project as a tree of who leads whom (DEC-104 Q16): the master
 * on top, each lead under it and each lead's agents under that lead, joined by solid
 * assignment lines whose badges count live tasks, waiting questions and unread messages with
 * their receipt. Unread messages between other agents arc dashed and claim overlaps arc red.
 * Drag or the arrow keys pan, a pinch, Ctrl or Cmd wheel or the buttons zoom, and Fit shows it
 * all. Activating a card opens its terminal in the one panel beside the graph, replacing the
 * agent shown there; its menu sends a message, stops it, or opens its changes to merge.
 * Inputs: the project id, the agent whose terminal is open and the open action, and optional
 * harness and account name lookups. Output: the graph section. */
export function TeamGraphView({ projectId, open, onOpen, harness, account }: {
  projectId: string; open: GraphNode | null; onOpen: (node: GraphNode | null) => void;
  harness?: (id: string) => string; account?: (id: string) => string;
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

  if (graph === undefined) return <section className="tg" aria-busy="true" aria-label="Team graph"><Skeleton shape="card" /></section>;
  if (graph.kind === "not_ready") return <NotReady what="The team graph" compact />;
  if (graph.kind === "error") return <LoadError message={`Couldn't load the team graph. ${graph.message}`} onRetry={reload} />;
  const team = account ? namedAccounts(graph.value, account) : graph.value;
  if (team.nodes.length === 0) {
    return <EmptyState icon="users" title="No team yet" compact>
      Add a team in the project settings, then ask the master to build something.
    </EmptyState>;
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
  const questions = cards.reduce((sum, item) => sum + questionCount(item.node), 0);

  return <section className="tg" aria-labelledby={`${ids}-title`}>
    <div className="tg-head">
      <h3 id={`${ids}-title`}>Team graph</h3>
      <span className="tg-summary" role="status">
        {graphSummary(team, layout.hidden)}{questions > 0 ? ` ${questions} ${questions === 1 ? "question" : "questions"} waiting.` : ""}
      </span>
    </div>
    {note && <Notice tone="warn" alert>
      <span className="grow">{displayText(note)}</span>
      <Button size="small" variant="quiet" onClick={() => setNote(null)}>Dismiss</Button>
    </Notice>}
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh the team graph. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    <p className="tg-hint" id={`${ids}-hint`}>Drag or use the arrow keys to move around. Pinch or Ctrl+scroll to zoom, 0 fits. Click a card to open its terminal; its menu shows its skill counts, sends a message, stops it or merges its work.</p>
    <div className={`tg-split${openNode ? " with-terminal" : ""}`}>
      <div className="tg-stage">
        <div className="tg-tools" role="toolbar" aria-label="Graph view">
          <Button size="small" icon="minus" iconOnly="Zoom out" onClick={() => zoom(1 / ZOOM_STEP)} />
          <span className="tg-zoom">{Math.round(shown.zoom * 100)}%</span>
          <Button size="small" icon="plus" iconOnly="Zoom in" onClick={() => zoom(ZOOM_STEP)} />
          <Button size="small" variant="quiet" icon="square" onClick={() => setMoved(null)}>Fit</Button>
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
              {layout.links.map((link) => <path key={`link:${link.from}:${link.to}`} className="tg-link" d={link.path} />)}
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
      </div>
      {openNode && <AgentSide key={openNode.id} projectId={projectId} node={openNode} harness={harness} onClose={() => onOpen(null)}>
        <NodeMessages projectId={projectId} node={openNode} refresh={graph} />
      </AgentSide>}
    </div>
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
  </section>;
}
