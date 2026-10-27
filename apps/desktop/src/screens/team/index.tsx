import { useEffect, useId, useRef, useState } from "react";
import { loadTeamGraph } from "../../api";
import type { Result } from "../../api";
import type { GraphNode, TeamGraph } from "../../engine-client";
import { Button, EmptyState, Icon, LoadError, NotReady, Notice, Skeleton, StatusBadge, type IconName } from "../../ui";
import { displayText } from "../conversation";
import {
  DRAG_SLOP, NODE_HEIGHT, NODE_WIDTH, arrowMove, edgeStatus, graphLive, graphSummary, keepOffsets,
  layoutGraph, moveNode, nodeDetail, nodeLine, nodeStatus, type Offsets, type PlacedEdge, type PlacedNode,
} from "./graph";
import { TerminalPanel } from "./terminal-panel";
import "./team-graph.css";

export { TerminalPanel } from "./terminal-panel";

export const LIVE_READ_MS = 2_000;
export const IDLE_READ_MS = 10_000;
const LABEL_CHARS = 28;

const NODE_ICONS: Record<GraphNode["kind"], IconName> = {
  master: "sparkles",
  harness_group: "boxes",
  agent: "bot",
};

/** A short label for a task edge: its status word and the start of the task text.
 * Input: the placed edge. Output: the label text. */
export function edgeLabel(placed: PlacedEdge): string {
  const text = placed.edge.text.replace(/\s+/g, " ").trim();
  const start = text.length > LABEL_CHARS ? `${text.slice(0, LABEL_CHARS - 1).trimEnd()}...` : text;
  return `${edgeStatus(placed.edge).word}: ${start}`;
}

/** What a screen reader hears for one node box. Inputs: the node and a harness name lookup.
 * Output: one sentence naming it, its detail, status and what activating it does. */
export function nodeLabel(node: GraphNode, harness?: (id: string) => string): string {
  const status = nodeStatus(node);
  const parts = [node.label, nodeDetail(node, harness), status?.word].filter(Boolean).join(", ");
  return `${parts}.${node.terminal ? " Opens its terminal." : ""} Arrow keys move it.`;
}

function NodeBox({ placed, harness, open, onOpen, onMove }: {
  placed: PlacedNode;
  harness?: (id: string) => string;
  open: boolean;
  onOpen: (id: string) => void;
  onMove: (id: string, dx: number, dy: number) => void;
}) {
  const drag = useRef<{ pointer: number; x: number; y: number; moved: boolean } | null>(null);
  const node = placed.node;
  const status = nodeStatus(node);
  const opens = node.terminal !== null;
  return <button type="button" className={`tg-node ${node.kind}${open ? " open" : ""}`}
    style={{ left: placed.x, top: placed.y, width: NODE_WIDTH, height: NODE_HEIGHT }}
    aria-label={displayText(nodeLabel(node, harness))} aria-pressed={opens ? open : undefined}
    title={node.kind === "harness_group" ? undefined : displayText(nodeLine(node, harness))}
    onPointerDown={(event) => {
      if (event.button !== 0) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      drag.current = { pointer: event.pointerId, x: event.clientX, y: event.clientY, moved: false };
    }}
    onPointerMove={(event) => {
      const current = drag.current;
      if (!current || current.pointer !== event.pointerId) return;
      const dx = event.clientX - current.x;
      const dy = event.clientY - current.y;
      if (!current.moved && Math.hypot(dx, dy) < DRAG_SLOP) return;
      current.moved = true;
      current.x = event.clientX;
      current.y = event.clientY;
      onMove(node.id, dx, dy);
    }}
    onPointerUp={(event) => {
      const current = drag.current;
      drag.current = null;
      if (current && current.pointer === event.pointerId && !current.moved && opens) onOpen(node.id);
    }}
    onPointerCancel={() => { drag.current = null; }}
    onClick={(event) => { if (event.detail === 0 && opens) onOpen(node.id); }}
    onKeyDown={(event) => {
      const move = arrowMove(event.key, event.shiftKey);
      if (!move) return;
      event.preventDefault();
      onMove(node.id, move.x, move.y);
    }}>
    <span className="tg-node-title"><Icon name={NODE_ICONS[node.kind]} size={14} />{displayText(node.label)}</span>
    <span className="tg-node-detail">{displayText(nodeDetail(node, harness))}</span>
    {status && <StatusBadge word={status.word} tone={status.tone} />}
  </button>;
}

function TaskDetail({ placed, nodes, id, onClose }: { placed: PlacedEdge; nodes: GraphNode[]; id: string; onClose: () => void }) {
  const { edge } = placed;
  const status = edgeStatus(edge);
  const to = nodes.find((node) => node.id === edge.to)?.label ?? edge.to;
  return <section id={id} className="tg-task" aria-label={`Task ${edge.task_id}`}>
    <div className="tg-task-head">
      <strong>{edge.source === "owner" ? "From you" : "From the master"} to {displayText(to)}</strong>
      <StatusBadge word={status.word} tone={status.tone} />
      <Button variant="quiet" size="small" icon="x" iconOnly="Close task" onClick={onClose} />
    </div>
    <p className="tg-task-text">{displayText(edge.text)}</p>
    {edge.reply
      ? <><h4 className="tg-task-label">{edge.state === "failed" ? "Problem" : "Reply"}</h4><p className="tg-task-reply">{displayText(edge.reply)}</p></>
      : <p className="tg-task-label">No reply yet.</p>}
  </section>;
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

/** The live team graph of one project: the master, one box per harness group with its agent
 * count, each agent with status and account, and the tasks the master handed out as live
 * edges. Boxes drag; a click on an edge shows the task and its reply; a click on the master
 * or an agent opens its terminal beside the graph. Inputs: the project id and an optional
 * harness name lookup. Output: the graph section. */
export function TeamGraphView({ projectId, harness }: { projectId: string; harness?: (id: string) => string }) {
  const [read, reload] = useTeamGraph(projectId);
  const graph = read.shown;
  const [offsets, setOffsets] = useState<Offsets>(() => new Map());
  const [open, setOpen] = useState<string | null>(null);
  const [task, setTask] = useState<number | null>(null);
  const ids = useId();
  const taskRegion = `${ids}-task`;

  useEffect(() => {
    setOffsets(new Map());
    setOpen(null);
    setTask(null);
  }, [projectId]);

  useEffect(() => {
    if (graph?.kind === "ok") setOffsets((current) => keepOffsets(current, graph.value));
  }, [graph]);

  if (graph === undefined) return <section className="tg" aria-busy="true" aria-label="Team graph"><Skeleton shape="card" /></section>;
  if (graph.kind === "not_ready") return <NotReady what="The team graph" compact />;
  if (graph.kind === "error") return <LoadError message={`Couldn't load the team graph. ${graph.message}`} onRetry={reload} />;
  if (graph.value.nodes.length === 0) {
    return <EmptyState icon="users" title="No team yet" compact>
      Add a team in the project settings, then ask the master to build something.
    </EmptyState>;
  }
  const layout = layoutGraph(graph.value, offsets);
  const openNode = layout.nodes.find((item) => item.node.id === open)?.node;
  const shownTask = layout.edges.find((item) => item.edge.task_id === task);
  const move = (id: string, dx: number, dy: number) => setOffsets((current) => moveNode(current, id, dx, dy));

  return <section className="tg" aria-labelledby={`${ids}-title`}>
    <div className="tg-head">
      <h3 id={`${ids}-title`}>Team graph</h3>
      <span className="tg-summary" role="status">{graphSummary(graph.value)}</span>
    </div>
    {read.stale && <Notice tone="warn" alert>
      <span className="grow">{displayText(`Couldn't refresh the team graph. ${read.stale} Showing the last read.`)}</span>
      <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
    </Notice>}
    <p className="tg-hint">Drag a box to arrange the graph, or focus it and use the arrow keys. Click the master or an agent to open its terminal.</p>
    <div className={`tg-split${openNode ? " with-terminal" : ""}`}>
      <div className="tg-canvas-scroll">
        <div className="tg-canvas" style={{ width: layout.width, height: layout.height }}>
          <svg className="tg-lines" width={layout.width} height={layout.height} aria-hidden="true">
            {layout.links.map((link) => <path key={`${link.from}>${link.to}`} className="tg-link" d={link.path} />)}
            {layout.edges.filter((item) => item.path).map((item) => <path key={item.edge.task_id} className={`tg-edge ${item.edge.state}`} d={item.path} />)}
          </svg>
          {layout.nodes.map((item) => <NodeBox key={item.node.id} placed={item} harness={harness} open={item.node.id === open}
            onOpen={(id) => setOpen(id)} onMove={move} />)}
          {layout.edges.map((item) => <button type="button" key={item.edge.task_id} className={`tg-edge-label ${item.edge.state}`}
            style={{ left: item.label.x, top: item.label.y }} aria-expanded={task === item.edge.task_id} aria-controls={taskRegion}
            aria-label={displayText(`Task ${item.edge.task_id}, ${edgeStatus(item.edge).word}: ${item.edge.text}`)}
            onClick={() => setTask(task === item.edge.task_id ? null : item.edge.task_id)}>
            {displayText(edgeLabel(item))}
          </button>)}
        </div>
      </div>
      {openNode && <TerminalPanel key={openNode.id} node={openNode} harness={harness} onClose={() => setOpen(null)} />}
    </div>
    {shownTask && <TaskDetail placed={shownTask} nodes={graph.value.nodes} id={taskRegion} onClose={() => setTask(null)} />}
  </section>;
}
