import { useEffect, useRef, useState } from "react";
import { loadTeamGraph, setLiveMaster } from "../../api";
import type { Result } from "../../api";
import { failureCopy, openMasterProject, readMaster, receiptState } from "../../api-projects";
import type { GraphNode, TeamStatus } from "../../engine-client";
import { Button, Icon } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { TerminalPanel } from "../team";
import "../team/team-graph.css";
import { readMasterInput } from "../../state";
import { ConversationTab } from "./conversation";
import { SessionsBar, SwitchDivider } from "./sessions";
import { NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

const LIVE_HARNESSES = ["codex", "claude-code"];
const POLL_MS = 2000;

/** Whether a master harness runs as its own app in a live terminal. Input: the team plan's
 * master harness id. Output: true for Codex and Claude Code, whose apps the engine reads. */
export function runsLive(harness: string | undefined): boolean {
  return harness !== undefined && LIVE_HARNESSES.includes(harness);
}

/** Finds the master's node in a team graph. Input: the graph nodes. Output: the master node,
 * or null before the engine lists one. */
export function masterNode(nodes: GraphNode[]): GraphNode | null {
  return nodes.find((node) => node.kind === "master") ?? null;
}

/** The live master's state as this area knows it: starting, not in this build, refused by
 * the engine with its code and plain copy, stopped with why the master last could not start,
 * or running with the master's terminal node once the team lists one. */
export type Live =
  | { kind: "starting" }
  | { kind: "not_ready" }
  | { kind: "refused"; code: string; copy: string }
  | { kind: "stopped"; problem: string | null }
  | { kind: "running"; node: GraphNode | null };

const NO_TERMINAL = "team_master_no_terminal";
const STOPPED = "The live master is stopped. It starts again when you open this tab or start a new session.";

/** The live master switch as the engine last told it. Inputs: the latest team status read
 * and the project shown. Output: undefined until a good read of this project arrives, since
 * a failed read says nothing; on null when the switch was never set; else the switch and why
 * the master last could not start. Opening the area turns a switch that is not on on. */
export function liveSwitch(team: Result<TeamStatus> | undefined, projectId: string): { on: boolean | null; problem: string | null } | undefined {
  if (team?.kind !== "ok" || team.value.project_id !== projectId) return undefined;
  return { on: team.value.live_master ?? null, problem: team.value.live_problem ?? null };
}

/** The owner's switch change while it is the switch the area shows: from the click until a
 * team read made after the engine accepted it. `after` is Infinity while the change waits. */
export type Want = { on: boolean; after: number };

/** Whether a team read now decides the switch again. Inputs: the owner's change, if any, and
 * when the latest team read arrived. Output: true once that read came after the engine
 * accepted the change, so an engine stop right after the owner's start shows. */
export function wantSettled(want: Want | null, readAt: number): boolean {
  return want !== null && readAt > want.after;
}

/** The project's master area. A Codex or Claude Code master is its own app in a live
 * terminal filling the area, where Enter sends and Shift+Enter starts a new line; every open
 * of the area starts it, and it stops only on the master idle stop or when the app quits.
 * Input: the project data. Output: the area;
 * any other master, a project without a team plan and a plan that couldn't be read keep the
 * engine-driven conversation. */
export function MasterArea({ data }: { data: ProjectData }) {
  if (data.setup === undefined) return <p className="pj-small pj-muted" role="status">Loading the master...</p>;
  const harness = valueOf(data.setup)?.team.master;
  if (harness === undefined || !runsLive(harness)) return <ConversationTab data={data} />;
  return <LiveMaster key={data.id} data={data} harness={harness} />;
}

function LiveMaster({ data, harness }: { data: ProjectData; harness: string }) {
  const [live, setLive] = useState<Live>({ kind: "starting" });
  const [want, setWant] = useState<Want | null>(null);
  const asked = useRef(false);
  const mounted = useRef(true);
  const engine = liveSwitch(data.team.result, data.id);
  const on = want?.on ?? engine?.on;

  const start = async () => {
    const id = data.id;
    asked.current = true;
    setWant({ on: true, after: Infinity });
    setLive({ kind: "starting" });
    const turned = await setLiveMaster(id, true);
    const state = turned.kind === "ok" ? receiptState(turned.value) : null;
    const failed: Live | null = turned.kind === "not_ready" ? { kind: "not_ready" }
      : turned.kind !== "ok" ? { kind: "refused", code: turned.code, copy: failureCopy(turned) }
      : state?.kind === "rejected" ? { kind: "refused", code: state.code, copy: failureCopy(state) }
      : null;
    if (!mounted.current) return;
    if (failed) {
      setWant(null);
      setLive(failed);
    } else setWant({ on: true, after: Date.now() });
    data.reloadTeam();
  };

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    if (wantSettled(want, data.team.at)) setWant(null);
  }, [data.team.at, want]);

  useEffect(() => {
    if (on !== undefined && on !== true && !asked.current) void start();
  }, [on]);

  useEffect(() => {
    if (on !== true) return;
    const id = data.id;
    let stopped = false;
    const read = async () => {
      const graph = await loadTeamGraph(id);
      if (stopped) return;
      if (graph.kind === "not_ready") return setLive({ kind: "not_ready" });
      if (graph.kind !== "ok") return;
      const node = masterNode(graph.value.nodes);
      setLive((prior) => prior.kind === "refused" || prior.kind === "not_ready" ? prior : { kind: "running", node: node?.terminal ? node : null });
    };
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [data.id, on === true]);

  const shown: Live = live.kind === "refused" || live.kind === "not_ready" ? live
    : on === false ? { kind: "stopped", problem: engine?.problem ?? null }
    : live;
  return <LiveMasterView data={data} harness={harness} live={shown} onStart={start} />;
}

/** The master ticket and live session the live master's New session and Sessions controls
 * act on, opened once per project as the conversation opens them. Input: the project id.
 * Output: the ticket, empty until opened, the live session id or null, and a re-read for
 * after a switch. */
export function useMasterSessions(projectId: string): { ticket: string; liveId: string | null; reread: () => Promise<void> } {
  const [ticket, setTicket] = useState("");
  const [liveId, setLiveId] = useState<string | null>(null);
  const current = useRef(projectId);
  current.current = projectId;

  const read = async (opened: string, id: string) => {
    const state = await readMaster(opened);
    if (state.kind !== "ok" || current.current !== id) return;
    setLiveId(state.value.session?.phase === "configured" ? state.value.session.session_id : null);
  };

  useEffect(() => {
    setTicket("");
    setLiveId(null);
    const id = projectId;
    void (async () => {
      const opened = await openMasterProject(id);
      if (opened.kind !== "ok" || opened.value.project_id !== id || current.current !== id) return;
      setTicket(opened.value.ticket);
      await read(opened.value.ticket, id);
    })();
  }, [projectId]);

  return { ticket, liveId, reread: () => ticket ? read(ticket, projectId) : Promise.resolve() };
}

/** The word and tone the console head shows for one live master state. Input: the state.
 * Output: a short word and the tone that colors it. */
export function liveWord(live: Live): { word: string; tone: "ok" | "muted" | "warn" | "danger" } {
  switch (live.kind) {
    case "running": return live.node ? { word: "Live", tone: "ok" } : { word: "Opening", tone: "muted" };
    case "starting": return { word: "Starting", tone: "muted" };
    case "stopped": return live.problem ? { word: "Stopped", tone: "warn" } : { word: "Stopped", tone: "muted" };
    case "refused": return { word: "Couldn't start", tone: "danger" };
    case "not_ready": return { word: "Not in this build", tone: "muted" };
  }
}

/** The live master area in one state: one console whose head names the master, its state
 * and account, then New session and Sessions, which restart the live terminal on a fresh or
 * resumed conversation and start the live master when it is stopped, above the master's
 * terminal filling the rest, typed into as the owner's Master input setting says, or a line
 * while it starts, can't or is stopped. An engine that says the master has no terminal app
 * gets today's conversation instead. Inputs: the project data, the master harness id, the
 * state and the start action. Output: the area. */
export function LiveMasterView({ data, harness, live, onStart }: { data: ProjectData; harness: string; live: Live; onStart: () => Promise<void> }) {
  const [input] = useState(readMasterInput);
  const sessions = useMasterSessions(data.id);
  const harnesses = valueOf(data.harnesses);
  const name = (id: string) => harnessName(harnesses, id);
  if (live.kind === "refused" && live.code === NO_TERMINAL) return <ConversationTab data={data} />;
  const off = live.kind === "stopped" || live.kind === "refused";
  const state = liveWord(live);
  const account = live.kind === "running" ? live.node?.account : null;
  return <div className="pj-master-live pj-console">
    <div className="pj-console-head">
      <span className="pj-console-title"><Icon name="square-terminal" size={15} />{displayText(name(harness))}</span>
      <span className={`pj-console-state ${state.tone}`}>{state.word}</span>
      {account && <span className="pj-console-detail">{displayText(account)}</span>}
    </div>
    {live.kind !== "not_ready" && <SessionsBar ticket={sessions.ticket} liveId={sessions.liveId} onSwitched={async () => {
      await sessions.reread();
      if (off) await onStart();
      data.reloadTeam();
    }} />}
    <SwitchDivider team={data.team.result} />
    {live.kind === "stopped" && <div className={`pj-note${live.problem ? " warn" : ""}`} role={live.problem ? "alert" : "status"}>
      <p>{displayText(live.problem ?? STOPPED)}</p>
    </div>}
    {live.kind === "not_ready" && <NotReady what="the live master terminal isn't in this build." />}
    {live.kind === "refused" && <div className="pj-note warn" role="alert">
      <p>The master terminal couldn't start. {live.copy}</p>
      <Button size="small" icon="refresh-cw" onClick={() => void onStart()}>Try again</Button>
    </div>}
    {live.kind === "starting" && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Starting the master ({displayText(name(harness))})...</p>}
    {live.kind === "running" && !live.node && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Waiting for the master's terminal. It opens once the master signs in and starts.</p>}
    {live.kind === "running" && live.node && <TerminalPanel key={`${live.node.terminal ?? live.node.id}:${sessions.liveId ?? ""}`} node={live.node} harness={name} embedded input={input} follow />}
  </div>;
}
