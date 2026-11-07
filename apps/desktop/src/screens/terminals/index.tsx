import { useEffect, useRef, useState } from "react";
import { loadTerminalsPerWindow, nextGeneration, type Result } from "../../api";
import { stopTeamAgent, stopTeammate } from "../../api-projects";
import type { GraphNode } from "../../engine-client";
import { Button, ConfirmDialog, EmptyState, Icon, LoadError, NotReady, Notice, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { actionNote, panelKey, useTeamGraph } from "../team";
import { nodeActions, nodeLine } from "../team/graph";
import { TerminalPanel } from "../team/terminal-panel";
import { wallPages, type PerWindow } from "./pages";
import "./terminals.css";

type Stopping = { node: GraphNode; stop: NonNullable<ReturnType<typeof nodeActions>["stop"]> };

/** One terminal in a wall window: its agent's line, Message, which moves the cursor to the
 * message box under the terminal, Stop for an agent that can be stopped, and the terminal
 * itself, typed into directly or through the box. Inputs: the node, whether it can be
 * stopped and the stop action. Output: the tile. */
function WallTile({ node, stoppable, onStop }: { node: GraphNode; stoppable: boolean; onStop: () => void }) {
  const tile = useRef<HTMLDivElement>(null);
  const label = displayText(node.label);
  return <div className="tw-tile" ref={tile}>
    <div className="tw-tile-head">
      <span className="tg-terminal-title"><Icon name="square-terminal" size={15} /><span className="tw-tile-name">{displayText(nodeLine(node))}</span></span>
      <Button variant="quiet" size="small" icon="send" iconOnly={`Message ${label}`}
        onClick={() => tile.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus()} />
      {stoppable && <Button variant="quiet" size="small" icon="circle-stop" iconOnly={`Stop ${label}`} onClick={onStop} />}
    </div>
    <TerminalPanel node={node} embedded input="both" />
  </div>;
}

/** One terminal wall window's grid: its tiles, then a free slot for each place the window
 * still has, where the next new agent opens. Inputs: the tiles of this window, the whole
 * graph, the terminals per window and the stop action. Output: the grid. */
export function WallGrid({ tiles, nodes, perWindow, onStop }: { tiles: GraphNode[]; nodes: GraphNode[]; perWindow: PerWindow; onStop: (node: GraphNode) => void }) {
  if (tiles.length > perWindow) throw new Error("a wall window holds at most its terminals per window");
  return <div className="tw-grid" data-per={perWindow}>
    {tiles.map((node) => <WallTile key={panelKey(node)} node={node} stoppable={nodeActions(node, nodes).stop !== null} onStop={() => onStop(node)} />)}
    {Array.from({ length: perWindow - tiles.length }, (_, index) => <div key={`free-${index}`} className="tw-free" role="note">
      <Icon name="square-terminal" size={18} />
      <span className="tw-free-title">Free slot</span>
      <span>The next new agent opens here</span>
    </div>)}
  </div>;
}

/** A terminal wall window of one project: its page of the team's terminals, the master
 * first, side by side, read again with the team graph. It reaches only the terminal, graph,
 * Stop and Terminals per window commands. Inputs: the project id and the window's page.
 * Output: the window's page. */
export function TerminalWall({ projectId, page }: { projectId: string; page: number }) {
  const [read, reload] = useTeamGraph(projectId);
  const graph = read.shown;
  const [setting, setSetting] = useState<Result<PerWindow> | undefined>(undefined);
  const [stopping, setStopping] = useState<Stopping | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    void loadTerminalsPerWindow(projectId).then((result) => { if (current) setSetting(result); });
    return () => { current = false; };
  }, [projectId, graph]);

  async function halt() {
    if (stopping === null || busy) return;
    const { stop } = stopping;
    setBusy(true);
    const result = stop.kind === "agent"
      ? await stopTeamAgent(projectId, stop.agent, nextGeneration())
      : await stopTeammate(projectId, stop.agent, stop.pane, nextGeneration());
    setBusy(false);
    setStopping(null);
    setNote(actionNote(result, "stopping an agent"));
    reload();
  }

  const nodes = graph?.kind === "ok" ? graph.value.nodes : [];
  const perWindow = setting?.kind === "ok" ? setting.value : null;
  const pages = perWindow === null ? [] : wallPages(nodes, perWindow);
  const tiles = pages[page - 1];
  const body = graph === undefined || setting === undefined ? <Skeleton shape="card" />
    : graph.kind === "not_ready" || setting.kind === "not_ready" ? <NotReady what="Terminal windows" compact />
    : graph.kind === "error" ? <LoadError message={`Couldn't load the team. ${graph.message}`} onRetry={reload} />
    : setting.kind === "error" ? <LoadError message={`Couldn't read Terminals per window. ${setting.message}`} onRetry={reload} />
    : perWindow === null || tiles === undefined ? <EmptyState icon="square-terminal" title="No terminals for this window" compact>
      The team has fewer terminals now. Close this window, or use Open all terminals on Agents.
    </EmptyState>
    : <WallGrid tiles={tiles} nodes={nodes} perWindow={perWindow} onStop={(node) => {
      const stop = nodeActions(node, nodes).stop;
      if (stop !== null) setStopping({ node, stop });
    }} />;

  return <main className="tw" aria-label="Terminals">
    <div className="tw-head">
      <h1>{tiles === undefined ? "Terminals" : `Terminals ${page} of ${pages.length}`}</h1>
      {read.stale && <span className="tw-stale" role="status">{displayText(`Couldn't refresh the team. ${read.stale} Showing the last read.`)}</span>}
    </div>
    {note && <Notice tone="warn" alert>
      <span className="grow">{displayText(note)}</span>
      <Button size="small" variant="quiet" onClick={() => setNote(null)}>Dismiss</Button>
    </Notice>}
    {body}
    {stopping && <ConfirmDialog open title={`Stop ${displayText(stopping.node.label)}?`} confirmLabel="Stop" pending={busy}
      onConfirm={() => void halt()} onCancel={() => setStopping(null)}>
      {stopping.stop.kind === "teammate"
        ? "This teammate's pane closes now, and its lead is told. Its lead keeps running."
        : stopping.node.kind === "teammate"
          ? "This teammate's pane closes now. Its lead keeps running."
          : "Its harness stops now. Work it has not handed in stays in its worktree, and you can wake it later from Finished agents."}
    </ConfirmDialog>}
  </main>;
}
