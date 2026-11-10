import { useEffect, useState } from "react";
import { closeProjectWalls, extendTerminalWindows, focusTerminalWindow, loadTerminalsPerWindow, nextGeneration, type Result } from "../../api";
import { stopTeamAgent, stopTeammate } from "../../api-projects";
import type { GraphNode } from "../../engine-client";
import { Button, ConfirmDialog, EmptyState, Icon, LoadError, NotReady, Notice, Segmented, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { actionNote, panelKey, useTeamGraph, wallNote } from "../team";
import { nodeActions } from "../team/graph";
import { TerminalPanel } from "../team/terminal-panel";
import { wallPages, type PerWindow } from "./pages";
import "./terminals.css";

type Stopping = { node: GraphNode; stop: NonNullable<ReturnType<typeof nodeActions>["stop"]> };

/** The harness names a wall window shows: the built-in ones, as a wall window reads no
 * harness catalog. Input: the harness id. Output: its display name. */
function builtInHarness(id: string): string {
  return harnessName(null, id);
}

/** One terminal in a wall window, the same terminal as the Agents panel: its head with the
 * agent's name, harness, state, role and branch, Message, which moves the cursor to the
 * "Type to" box under the terminal, Stop for an agent that can be stopped, and the terminal
 * itself, typed into as the owner's Terminal input says, attached for its project. Inputs: the
 * project id, the node, whether it can be stopped and the stop action. Output: the tile. */
function WallTile({ projectId, node, stoppable, onStop }: { projectId: string; node: GraphNode; stoppable: boolean; onStop: () => void }) {
  return <div className="tw-tile">
    <TerminalPanel node={node} projectId={projectId} tile harness={builtInHarness} onStop={stoppable ? onStop : undefined} />
  </div>;
}

/** One terminal wall window's grid: its tiles, then a free slot for each place the window
 * still has, where the next new agent opens. Inputs: the project id, the tiles of this
 * window, the whole graph, the terminals per window and the stop action. Output: the grid. */
export function WallGrid({ projectId, tiles, nodes, perWindow, onStop }: { projectId: string; tiles: GraphNode[]; nodes: GraphNode[]; perWindow: PerWindow; onStop: (node: GraphNode) => void }) {
  if (tiles.length > perWindow) throw new Error("a wall window holds at most its terminals per window");
  return <div className="tw-grid" data-per={perWindow}>
    {tiles.map((node) => <WallTile key={panelKey(node)} projectId={projectId} node={node} stoppable={nodeActions(node, nodes).stop !== null} onStop={() => onStop(node)} />)}
    {Array.from({ length: perWindow - tiles.length }, (_, index) => <div key={`free-${index}`} className="tw-free" role="note">
      <Icon name="square-terminal" size={18} />
      <span className="tw-free-title">Free slot</span>
      <span>The next new agent opens here</span>
    </div>)}
  </div>;
}

/** A wall window's title, as the mockup's window bar says it. Inputs: the project name, the
 * window's page and the window count. Output: "Larkspur Booking · Terminals 1 of 2". */
export function wallTitle(name: string, page: number, pages: number): string {
  if (!(Number.isSafeInteger(page) && Number.isSafeInteger(pages) && page >= 1 && page <= pages)) {
    throw new Error(`a wall window is page ${page} of ${pages}`);
  }
  if (name.length === 0) throw new Error("a wall window names its project");
  return `${name} · Terminals ${page} of ${pages}`;
}

/** A wall window's tools row, as the mockup draws it: Back to Agents, which brings the main
 * window forward on the project's Agents tab, then the windows that hold terminals, which
 * bring another of the project's wall windows forward: a Window N of M switch at this
 * window's page, or a button each on a page the team no longer fills, and the hint. It shows
 * while the team loads too, so the owner can always leave. Inputs: this window's page, the
 * count of windows that hold terminals and the focus action. Output: the row. */
export function WallTools({ page, pages, onFocus }: { page: number; pages: number; onFocus: (target: "main" | number) => void }) {
  if (!(Number.isSafeInteger(page) && Number.isSafeInteger(pages) && page >= 1 && pages >= 0)) {
    throw new Error(`a wall window is page ${page} of ${pages}`);
  }
  const windows = Array.from({ length: pages }, (_, index) => ({ value: String(index + 1), label: `Window ${index + 1} of ${pages}` }));
  return <div className="tw-tools">
    <div className="tw-tools-row">
      <Button size="small" icon="bot" onClick={() => onFocus("main")}>Back to Agents</Button>
      {page <= pages
        ? <Segmented label="Terminal windows" value={String(page)} options={windows}
          onValue={(value) => { if (Number(value) !== page) onFocus(Number(value)); }} />
        : pages > 0 && <div className="tw-tools-row" role="group" aria-label="Terminal windows">
          {windows.map((item) => <Button key={item.value} size="small" onClick={() => onFocus(Number(item.value))}>{item.label}</Button>)}
        </div>}
    </div>
    <span className="tw-tools-hint">One click opens every window needed; new agents fill the last window or open a new one.</span>
  </div>;
}

/** A terminal wall window of one project: its page of the team's terminals, the master
 * first, side by side, read again with the team graph. It reaches only the terminal, graph,
 * Stop, Terminals per window and window growth commands, asks for the windows a growing
 * team needs itself, whatever the main window shows, and closes its project's windows once a
 * read finds the project removed, however it was removed. Its tools row, there on every page,
 * brings the main window forward on Agents or another of its wall windows forward; its
 * heading shows the project name through displayText. Inputs: the project id, the window's
 * page and the project name its address carries. Output: the window's page. */
export function TerminalWall({ projectId, page, name }: { projectId: string; page: number; name: string }) {
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

  useEffect(() => {
    if (read.gone) void closeProjectWalls(projectId);
  }, [projectId, read.gone]);

  async function focus(target: "main" | number) {
    const result = await focusTerminalWindow(projectId, target);
    if (result.kind === "ok") return;
    setNote(result.kind === "not_ready" ? "Not ready yet: moving between windows isn't in this build." : `Couldn't bring that window forward. ${result.message}`);
  }

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
  const extend = pages.length;
  useEffect(() => {
    if (extend === 0) return;
    let current = true;
    void extendTerminalWindows(projectId, pages.length).then((result) => {
      const failed = wallNote(result);
      if (current && failed !== null) setNote(failed);
    });
    return () => { current = false; };
  }, [projectId, extend]);
  const tiles = pages[page - 1];
  const body = graph === undefined || setting === undefined ? <Skeleton shape="card" />
    : graph.kind === "not_ready" || setting.kind === "not_ready" ? <NotReady what="Terminal windows" compact />
    : graph.kind === "error" ? <LoadError message={`Couldn't load the team. ${graph.message}`} onRetry={reload} />
    : setting.kind === "error" ? <LoadError message={`Couldn't read Terminals per window. ${setting.message}`} onRetry={reload} />
    : perWindow === null || tiles === undefined ? <EmptyState icon="square-terminal" title="No terminals for this window" compact>
      The team has fewer terminals now. Close this window, or use Open all terminals on Agents.
    </EmptyState>
    : <WallGrid projectId={projectId} tiles={tiles} nodes={nodes} perWindow={perWindow} onStop={(node) => {
      const stop = nodeActions(node, nodes).stop;
      if (stop !== null) setStopping({ node, stop });
    }} />;

  const title = displayText(name);
  return <main className="tw" aria-label="Terminals">
    <WallTools page={page} pages={pages.length} onFocus={(target) => void focus(target)} />
    <div className="tw-head">
      <h1>{tiles === undefined ? `${title} · Terminals` : wallTitle(title, page, pages.length)}</h1>
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
