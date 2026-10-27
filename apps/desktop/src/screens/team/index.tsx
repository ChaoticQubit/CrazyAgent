import { useEffect, useId, useRef, useState } from "react";
import { loadTeamGraph } from "../../api";
import type { Result } from "../../api";
import type { GraphEdge, GraphNode, TeamGraph } from "../../engine-client";
import { Button, EmptyState, Icon, LoadError, NotReady, Notice, Skeleton, type IconName } from "../../ui";
import { displayText } from "../conversation";
import {
  BADGE_HEIGHT, BADGE_WIDTH, DRAG_SLOP, GROUP_HEAD, ZOOM_STEP, badgeLabel, cardTask, cardText, cardTip, fitView,
  graphLive, graphSummary, groupTitle, layoutCards, layoutGraph, namedAccounts, nodeDetail, nodeStatus, panView, questionCount,
  tipPlace, viewKey, wheelZoom, zoomView, type Link, type PlacedCard, type PlacedGroup, type Size, type View,
} from "./graph";
import { AgentInstructions } from "./agent-instructions";
import { TerminalPanel } from "./terminal-panel";
import "./team-graph.css";

export { TerminalPanel } from "./terminal-panel";

export const LIVE_READ_MS = 2_000;
export const IDLE_READ_MS = 10_000;

const NODE_ICONS: Record<GraphNode["kind"], IconName> = {
  master: "sparkles",
  harness_group: "boxes",
  agent: "bot",
};

/** What a screen reader hears for one card. Inputs: the node, a harness name lookup and the
 * graph's edges. Output: one sentence naming it, its detail, status, task and what activating
 * it does; a harness group has no task. */
export function nodeLabel(node: GraphNode, harness?: (id: string) => string, edges: readonly GraphEdge[] = []): string {
  const status = nodeStatus(node);
  const parts = [node.label, nodeDetail(node, harness), status?.word].filter(Boolean).join(", ");
  const task = node.kind === "harness_group" ? "" : ` ${cardTask(node, edges).replace(/\s+/g, " ").trim().replace(/[.!?]$/, "")}.`;
  return `${parts}.${task}${node.terminal ? " Opens its terminal." : ""}`;
}

/** One fixed-size card: a status dot and the clipped title, the harness line and the clipped
 * task lines. Activating it opens the agent's terminal; hover or focus shows its full text.
 * Inputs: the placed card, the edges, a harness name lookup, whether its terminal is open, the
 * id of its shown hover text, and the open and hover actions. Output: the card button. */
export function GraphCard({ placed, edges, harness, open, tipId, onOpen, onTip }: {
  placed: PlacedCard;
  edges: readonly GraphEdge[];
  harness?: (id: string) => string;
  open: boolean;
  tipId: string | null;
  onOpen: (id: string) => void;
  onTip: (id: string | null) => void;
}) {
  const node = placed.node;
  const status = nodeStatus(node);
  const opens = node.terminal !== null;
  const text = cardText({ ...node, label: displayText(node.label) }, edges.map((edge) => ({ ...edge, text: displayText(edge.text) })), harness);
  return <button type="button" className={`tg-card ${node.kind}${open ? " open" : ""}${opens ? "" : " static"}`}
    style={{ left: placed.x, top: placed.y, width: placed.width, height: placed.height }}
    aria-label={displayText(nodeLabel(node, harness, edges))} aria-pressed={opens ? open : undefined}
    aria-describedby={tipId ?? undefined}
    onClick={() => { if (opens) onOpen(node.id); }}
    onPointerEnter={() => onTip(node.id)} onPointerLeave={() => onTip(null)}
    onFocus={() => onTip(node.id)} onBlur={() => onTip(null)}>
    <span className="tg-card-title">
      <span className={`tg-dot tone-${status?.tone ?? "neutral"}`} aria-hidden="true" />
      <Icon name={NODE_ICONS[node.kind]} size={14} />
      <span className="tg-clip">{text.title}</span>
    </span>
    <span className="tg-card-detail tg-clip">{displayText(text.detail)}</span>
    <span className="tg-card-task">{text.task.map((line, index) => <span key={index} className="tg-clip">{line}</span>)}</span>
  </button>;
}

function GroupFrame({ group, harness }: { group: PlacedGroup; harness?: (id: string) => string }) {
  return <div className={`tg-group${group.node ? "" : " other"}`} style={{ left: group.x, top: group.y, width: group.width, height: group.height }}>
    <div className="tg-group-head" style={{ height: GROUP_HEAD }} title={displayText(groupTitle(group, harness))}>
      <Icon name={NODE_ICONS.harness_group} size={14} />
      <span className="tg-clip">{displayText(groupTitle(group, harness))}</span>
    </div>
  </div>;
}

function LinkBadges({ link }: { link: Link }) {
  const label = badgeLabel(link);
  if (!label) return null;
  return <span className="tg-badges" role="img" aria-label={label} title={label}
    style={{ left: link.badge.x - BADGE_WIDTH / 2, top: link.badge.y - BADGE_HEIGHT / 2, width: BADGE_WIDTH, height: BADGE_HEIGHT }}>
    {link.tasks > 0 && <span className="tg-badge"><Icon name="list-checks" size={12} />{link.tasks}</span>}
    {link.questions > 0 && <span className="tg-badge question"><Icon name="message-square" size={12} />{link.questions}</span>}
  </span>;
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

/** The live team graph of one project: the master card on top, one frame per harness with
 * its agent cards, and links that carry only live-task and question counts. Drag or the arrow
 * keys pan, a pinch, Ctrl or Cmd wheel or the buttons zoom, and Fit shows it all; the view fits
 * itself until the owner moves it. Hovering a card shows its full text; activating the master
 * or an agent opens its terminal beside the graph. Inputs: the project id and optional
 * harness and account name lookups. Output: the graph section. */
export function TeamGraphView({ projectId, harness, account }: {
  projectId: string; harness?: (id: string) => string; account?: (id: string) => string;
}) {
  const [read, reload] = useTeamGraph(projectId);
  const graph = read.shown;
  const [open, setOpen] = useState<string | null>(null);
  const [moved, setMoved] = useState<View | null>(null);
  const [tip, setTip] = useState<string | null>(null);
  const [element, setElement] = useState<HTMLDivElement | null>(null);
  const [port, setPort] = useState<Size>({ width: 0, height: 0 });
  const sizes = useRef<{ content: Size; port: Size }>({ content: { width: 0, height: 0 }, port });
  const drag = useRef<{ pointer: number; x: number; y: number; moved: boolean } | null>(null);
  const panned = useRef(false);
  const ids = useId();

  useEffect(() => {
    setOpen(null);
    setMoved(null);
    setTip(null);
  }, [projectId]);

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
  const edges = team.edges;
  const layout = layoutGraph(team);
  const content = { width: layout.width, height: layout.height };
  sizes.current = { content, port };
  const shown = moved ?? fitView(content, port);
  const cards = layoutCards(layout);
  const openNode = cards.find((item) => item.node.id === open)?.node;
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
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh the team graph. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    <p className="tg-hint" id={`${ids}-hint`}>Drag or use the arrow keys to move around. Pinch or Ctrl+scroll to zoom, 0 fits. Click the master or an agent to open its terminal.</p>
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
            visibility: port.width > 0 ? "visible" : "hidden" }}>
            {layout.groups.map((group, index) => <GroupFrame key={group.node?.id ?? `other-${index}`} group={group} harness={harness} />)}
            <svg className="tg-lines" width={layout.width} height={layout.height} aria-hidden="true">
              {layout.links.map((link, index) => <path key={link.group?.id ?? `other-${index}`} className="tg-link" d={link.path} />)}
            </svg>
            {cards.map((item) => <GraphCard key={item.node.id} placed={item} edges={edges} harness={harness} open={item.node.id === open}
              tipId={item.node.id === tip ? tipId : null} onTip={setTip}
              onOpen={setOpen} />)}
            {layout.links.map((link, index) => <LinkBadges key={link.group?.id ?? `other-${index}`} link={link} />)}
          </div>
          {tipCard && place && <div id={tipId} role="tooltip" className={`tg-tip${place.above ? " above" : ""}`} style={{ left: place.left, top: place.top }}>
            {cardTip(tipCard.node, edges, harness).map((line, index) => <p key={index} className={index === 0 ? "tg-tip-title" : undefined}>{displayText(line)}</p>)}
          </div>}
        </div>
      </div>
      {openNode && <div className="tg-side">
        <TerminalPanel key={openNode.id} node={openNode} harness={harness} onClose={() => setOpen(null)} />
        <AgentInstructions key={openNode.id} projectId={projectId} node={openNode} />
      </div>}
    </div>
  </section>;
}
