import { useEffect, useState, type ReactNode } from "react";
import type { GraphNode, TeamGraph, TeamStatus } from "../../engine-client";
import { Button, Icon, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { TerminalPanel, useTeamGraph } from "../team";
import { cardTask, clipLines, graphSummary, nodeStatus, questionCount } from "../team/graph";
import { ago } from "./research";
import { valueOf, type ProjectData } from "./shared";
import "../team/team-graph.css";
import "./team-activity.css";

export const ACTIVITY_NAME_CHARS = 28;
export const ACTIVITY_TASK_CHARS = 34;
export const ACTIVITY_EMPTY = "No sub-agents yet. The master starts them when it delegates.";

/** One sub-agent row of the team activity strip: its graph node, its full task line, the
 * clipped name and task line, the questions it waits on and when it last changed, or null
 * before the team status is read. */
export interface ActivityRow {
  node: GraphNode;
  task: string;
  name: string;
  line: string;
  questions: number;
  changed: number | null;
}

const live = (node: GraphNode) => node.status === "starting" || node.status === "idle" || node.status === "working";
const plural = (count: number, word: string) => `${count} ${count === 1 ? word : `${word}s`}`;

/** When one sub-agent last changed: the newest of its start, stop and its tasks' created,
 * started and finished times. Inputs: the agent's graph node and the team status. Output:
 * milliseconds, or null when the status has no such agent. */
export function lastChange(node: GraphNode, status: TeamStatus | null): number | null {
  const agent = status?.agents.find((item) => `team-${item.id}` === node.id);
  if (!status || !agent) return null;
  const tasks = status.tasks.filter((item) => item.agent === agent.id);
  return Math.max(agent.created_ms, agent.stopped_ms ?? 0,
    ...tasks.flatMap((item) => [item.created_ms, item.started_ms ?? 0, item.finished_ms ?? 0]));
}

/** The rows of the team activity strip: one per sub-agent the graph shows (live or recently
 * stopped), those waiting on a question first, then live ones, then the latest change.
 * Inputs: the team graph and the team status, when read. Output: the rows. */
export function activityRows(graph: TeamGraph, status: TeamStatus | null): ActivityRow[] {
  const edges = graph.edges.map((edge) => ({ ...edge, text: displayText(edge.text) }));
  return graph.nodes.filter((node) => node.kind === "agent").map((node) => {
    const task = cardTask(node, edges);
    return {
      node,
      task,
      name: clipLines(displayText(node.label), ACTIVITY_NAME_CHARS, 1)[0] ?? "",
      line: clipLines(task, ACTIVITY_TASK_CHARS, 1)[0] ?? "",
      questions: questionCount(node),
      changed: lastChange(node, status),
    };
  }).sort((a, b) => Number(b.questions > 0) - Number(a.questions > 0) || Number(live(b.node)) - Number(live(a.node))
    || (b.changed ?? 0) - (a.changed ?? 0) || a.node.label.localeCompare(b.node.label, "en", { numeric: true }));
}

/** The agent a row click opens: the row's node when it has a live terminal. Inputs: the rows
 * and the chosen node id. Output: the node, or undefined. */
export function openTarget(rows: readonly ActivityRow[], id: string | null): GraphNode | undefined {
  const node = rows.find((row) => row.node.id === id)?.node;
  return node?.terminal ? node : undefined;
}

/** The strip's list: per sub-agent a status dot, its name and question badge, its harness,
 * status and age, and its clipped task line; hover shows the full task and a click opens a
 * live agent's terminal. Inputs: the rows, now in milliseconds, the open node id, a harness
 * name lookup and the open action. Output: the list, or the empty state. */
export function TeamActivityList({ rows, now, open, harness = (id) => id, onOpen }: {
  rows: readonly ActivityRow[];
  now: number;
  open: string | null;
  harness?: (id: string) => string;
  onOpen: (id: string) => void;
}) {
  if (rows.length === 0) return <p className="ta-empty">{ACTIVITY_EMPTY}</p>;
  return <ul className="ta-rows" aria-label="Sub-agents">{rows.map((row) => {
    const { node } = row;
    const status = nodeStatus(node);
    const opens = node.terminal !== null;
    const since = row.changed === null ? "" : ago(row.changed, now);
    const meta = [harness(node.harness), status?.word].filter(Boolean).join(", ");
    const waiting = row.questions > 0 ? `, ${plural(row.questions, "question")} waiting` : "";
    const label = `${displayText(node.label)}, ${meta}${waiting}. ${row.task}.${since ? ` Changed ${since.toLowerCase()}.` : ""}${opens ? " Opens its terminal." : ""}`;
    return <li key={node.id}>
      <button type="button" className={`ta-row${open === node.id ? " open" : ""}`} title={row.task} aria-label={label}
        aria-pressed={opens ? open === node.id : undefined} aria-disabled={opens ? undefined : true}
        onClick={() => { if (opens) onOpen(node.id); }}>
        <span className="ta-row-head">
          <span className={`tg-dot tone-${status?.tone ?? "neutral"}`} aria-hidden="true" />
          <span className="tg-clip ta-name">{row.name}</span>
          {row.questions > 0 && <span className="tg-badge question" aria-label={`${plural(row.questions, "question")} waiting`}>
            <Icon name="message-square" size={12} />{row.questions}</span>}
        </span>
        <span className="ta-row-meta"><span className="tg-clip">{displayText(meta)}</span>{since && <span className="ta-since">{since}</span>}</span>
        <span className="ta-row-task tg-clip">{row.line}</span>
      </button>
    </li>;
  })}</ul>;
}

/** The master view with the team activity strip beside it: the master's content on the left,
 * and on the right one row per sub-agent from the team graph the Agents tab reads, with the
 * age of each agent's last change from the project's team status. A click on a live agent
 * swaps the strip for its terminal until it is closed. The strip scrolls on its own. Inputs:
 * the project data and the master view. Output: the two-column layout. */
export function TeamActivity({ data, children }: { data: ProjectData; children: ReactNode }) {
  const [read, reload] = useTeamGraph(data.id);
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => setOpen(null), [data.id]);
  const harnesses = valueOf(data.harnesses);
  const harness = (id: string) => harnessName(harnesses, id);
  const graph = read.shown;
  const status = valueOf(data.team.result);
  const rows = graph?.kind === "ok" ? activityRows(graph.value, status) : [];
  const target = openTarget(rows, open);

  return <div className={`ta-split${target ? " with-terminal" : ""}`}>
    <div className="ta-main">{children}</div>
    <aside className="ta-side" aria-label="Team activity">
      <div className="ta-scroll">
        {target ? <TerminalPanel key={target.id} node={target} harness={harness} onClose={() => setOpen(null)} /> : <>
          <div className="ta-head">
            <h3>Team</h3>
            {graph?.kind === "ok" && <span className="ta-summary" role="status">{graphSummary(graph.value)}</span>}
          </div>
          {graph === undefined && <div aria-busy="true"><Skeleton shape="card" /></div>}
          {graph?.kind === "not_ready" && <p className="ta-empty">Not ready yet: team activity isn't in this build.</p>}
          {graph?.kind === "error" && <div className="ta-empty" role="alert">
            <p>{displayText(`Couldn't load the team. ${graph.message}`)}</p>
            <Button size="small" icon="refresh-cw" onClick={reload}>Try again</Button>
          </div>}
          {graph?.kind === "ok" && <TeamActivityList rows={rows} now={Date.now()} open={open} harness={harness} onOpen={setOpen} />}
          {read.stale && <p className="ta-stale" role="status">{displayText(`Couldn't refresh. ${read.stale}`)}</p>}
        </>}
      </div>
    </aside>
  </div>;
}
