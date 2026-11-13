import { useEffect, useRef, useState } from "react";
import { listAccounts, loadTeamGraph, nextGeneration, setLiveMaster, setTeamStopped } from "../../api";
import type { Result } from "../../api";
import { useLoad } from "../../state";
import { failureCopy, openMasterProject, readMaster, receiptState, startMaster } from "../../api-projects";
import type { GraphNode, IntentReceipt, MasterState, TeamStatus } from "../../engine-client";
import { Button, Icon } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { entry } from "../new-project/wizard";
import { TerminalPanel } from "../team";
import { changeNote } from "../team/controls";
import "../team/team-graph.css";
import { ConversationTab, MasterSettingsButton, MasterSettingsDrawer } from "./conversation";
import { accountName } from "./research";
import { LiveConversation } from "./session-messages";
import { SessionsBar, SwitchDivider } from "./sessions";
import { savedStale } from "./settings";
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

/** The master area's head (N-D26): the harness the running master uses, or the saved one
 * while none runs, and the saved harness it switches to next while the two differ. Inputs:
 * the saved harness id, the running master's harness id or null, and the name lookup.
 * Output: the title and the waiting switch's words, or null for none. */
export function masterHead(saved: string, running: string | null, name: (id: string) => string): { title: string; waits: string | null } {
  if (running === null || running === "") return { title: name(saved), waits: null };
  const title = name(running);
  return { title, waits: title === name(saved) ? null : `${name(saved)} from the next start` };
}

const LIVE_STATUSES: readonly string[] = ["starting", "idle", "working"];

/** The harness the running master uses (N-D26), from the team status agents, since the
 * master card names the saved plan: the agent behind the card, else the newest live master.
 * Inputs: the team status read, the project shown and the card's agent id. Output: the
 * harness id, or null without a good read of this project or a live master. */
export function runningHarness(team: Result<TeamStatus> | undefined, projectId: string, agent: number | null | undefined): string | null {
  if (team?.kind !== "ok" || team.value.project_id !== projectId) return null;
  const masters = team.value.agents.filter((row) => row.role === "master");
  const behind = agent === null || agent === undefined ? undefined : masters.find((row) => row.id === agent);
  const found = behind ?? [...masters].reverse().find((row) => LIVE_STATUSES.includes(row.status));
  return found?.harness ?? null;
}

/** The live master's state as this area knows it: starting, not in this build, refused by
 * the engine with its code and plain copy, stopped with why the master last could not start,
 * or running with the master's terminal node once the team lists one, and while it lists
 * none, since when the area has waited for that terminal. */
export type Live =
  | { kind: "starting" }
  | { kind: "not_ready" }
  | { kind: "refused"; code: string; copy: string }
  | { kind: "stopped"; problem: string | null; idle: boolean }
  | { kind: "paused" }
  | { kind: "running"; node: GraphNode | null; since: number | null };

const NO_TERMINAL = "team_master_no_terminal";
const STOPPED = "The live master is stopped. It starts again when you open this tab or start a new session.";
const PAUSED = "The team is stopped (Stop all). Start the team to run the master.";
const WAITING = "Waiting for the master's terminal. It opens once the master signs in and starts.";
const NO_SESSION = "No master session opened, so this conversation won't be in Sessions.";
const OVERDUE = "The master's terminal hasn't opened after a minute.";

/** How long the area waits for the master's terminal before it stops waiting and offers Try
 * again with the engine's line, in milliseconds, so no wait for the terminal is endless. */
export const OPEN_LIMIT_MS = 60_000;

/** Whether the wait for the master's terminal is over its limit. Inputs: when the area first
 * saw the master on with no terminal, null while a terminal is open, and the time now in
 * milliseconds. Output: true once `OPEN_LIMIT_MS` passed. */
export function openOverdue(since: number | null, now: number): boolean {
  return since !== null && now - since >= OPEN_LIMIT_MS;
}

/** The area's state after one team graph read while the switch is on: running with the
 * master's terminal node, or with none while it has no terminal, keeping when that wait
 * began so a start or a closed terminal restarts the clock; a refused or missing start stays.
 * Inputs: the state before, the master node read and the time now. Output: the state. */
export function runningState(prior: Live, node: GraphNode | null, now: number): Live {
  if (prior.kind === "refused" || prior.kind === "not_ready") return prior;
  if (node?.terminal) return { kind: "running", node, since: null };
  const since = prior.kind === "running" && prior.since !== null ? prior.since : now;
  return { kind: "running", node: null, since };
}

/** Whether the latest good team read of this project says the team is stopped (Stop all),
 * when the engine runs none of its agents and the live master cannot start. Inputs: the team
 * status read and the project shown. Output: true only for a good read of this project. */
export function teamPaused(team: Result<TeamStatus> | undefined, projectId: string): boolean {
  return team?.kind === "ok" && team.value.project_id === projectId && team.value.paused;
}

/** The live master switch as the engine last told it. Inputs: the latest team status read
 * and the project shown. Output: undefined until a good read of this project arrives, since
 * a failed read says nothing; on null when the switch was never set; else the switch, why
 * the master last could not start and the team settings version a switch change must name.
 * Opening the area turns a switch that is not on on. */
export function liveSwitch(team: Result<TeamStatus> | undefined, projectId: string): { on: boolean | null; problem: string | null; version: number } | undefined {
  if (team?.kind !== "ok" || team.value.project_id !== projectId) return undefined;
  return { on: team.value.live_master ?? null, problem: team.value.live_problem ?? null, version: team.value.settings_version ?? 0 };
}

/** Whether the team read says an idle stop ended the live master. Inputs: the read and the
 * project. Output: the answer. */
export function idleStopped(team: Result<TeamStatus> | undefined, projectId: string): boolean {
  return team?.kind === "ok" && team.value.project_id === projectId && team.value.live_idle_stopped === true;
}

/** How the engine's note starts once it moved a live master to its pool's top account under
 * Live master account: Move when idle (DEC-139). */
const MOVED_START = "The live master moved to the account";

/** How long the move line stays after the engine wrote its note, in milliseconds: the engine's
 * own wait before a project's next move, so the line ends before another move can follow. */
const MOVED_SHOWN_MS = 15 * 60 * 1000;

/** How often the master area reads the clock again, in milliseconds, so the move line ends
 * with no new team read. */
const CLOCK_MS = 30_000;

/** The line the master area shows after the engine moved the live master to another account:
 * the engine's own note while it is the newest thing the engine said to the team and younger
 * than `MOVED_SHOWN_MS`. A note written after the clock was last read shows. Inputs: the latest
 * team status read, the project shown and the time now in milliseconds. Output: the note, or
 * null. */
export function movedNote(team: Result<TeamStatus> | undefined, projectId: string, now: number): string | null {
  if (team?.kind !== "ok" || team.value.project_id !== projectId) return null;
  const last = team.value.messages.filter((message) => message.author === "engine" && message.state === "note").at(-1);
  if (last === undefined || !last.text.startsWith(MOVED_START)) return null;
  return now - last.created_ms < MOVED_SHOWN_MS ? last.text : null;
}

/** Words the time a live master has left before its Master run time (F01). Input: the
 * milliseconds left. Output: such as "6 d 23 h left", "5 h 10 min left" or "Time is up". */
export function timeLeftWords(left: number): string {
  if (left <= 0) return "Time is up; it stops if it works on without you";
  const minutes = Math.ceil(left / 60_000);
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  if (days > 0) return `${days} d ${hours} h left`;
  return hours > 0 ? `${hours} h ${minutes % 60} min left` : `${minutes} min left`;
}

/** The line the master area shows about its limits: the time the live master has left under
 * Master run time and the sub-agent rounds of your current goal (F01, F09). Inputs: the latest
 * team read, the project shown and the time now in milliseconds. Output: the line, or null
 * without a good read of this project. */
export function masterLimitsLine(team: Result<TeamStatus> | undefined, projectId: string, now: number): string | null {
  if (team?.kind !== "ok" || team.value.project_id !== projectId) return null;
  const status = team.value;
  const used = status.rounds_used ?? 0;
  const max = status.master_rounds ?? null;
  const rounds = status.rounds_kept_going === true && max !== null
    ? `Sub-agent rounds: ${used}, no limit for this goal`
    : max === null ? `Sub-agent rounds: ${used}` : `Sub-agent rounds: ${used} of ${max}`;
  if (status.master_run_secs === null) return `No time limit. ${rounds}.`;
  const deadline = status.master_deadline_ms ?? null;
  return deadline === null ? `${rounds}.` : `${timeLeftWords(deadline - now)}. ${rounds}.`;
}

/** Whether the engine says this project's Live terminal setting is Off (DEC-117 Q4). Inputs:
 * the team read and the project shown. Output: true only for a good read of this project
 * showing Off, so a failed read never closes a running terminal. */
export function liveTerminalOff(team: Result<TeamStatus> | undefined, projectId: string): boolean {
  return team?.kind === "ok" && team.value.project_id === projectId && team.value.master_live_terminal === false;
}

/** Whether opening the area turns the live master on now: only once a team read shows a
 * switch that is not on, nothing was asked and the read is newer than a start the engine
 * refused as stale, so a refused start waits for the fresh read instead of retrying the old
 * one. A stopped team (Stop all) starts nothing, so opening its area leaves the switch alone
 * until a read shows the team running. Inputs: the switch shown, whether a change was asked,
 * the read's settings version, the version last refused, -1 when none, and whether the team
 * is stopped. Output: true to start. */
export function autoStart(on: boolean | null | undefined, asked: boolean, read: number, refused: number, paused: boolean): boolean {
  return on !== undefined && on !== true && !asked && read > refused && !paused;
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

/** Whether starting the live master first sends the owner's default Open: only after a good
 * master read that holds no live session, so the live master runs in a session its turns
 * mark used and the Sessions list shows (LMS-1); a session still closing is left to finish.
 * Input: the master read. Output: true when the Open goes out. */
export function openBeforeLive(state: Result<MasterState>): boolean {
  if (state.kind !== "ok") return false;
  const phase = state.value.session?.phase;
  return phase === undefined || phase === "parked" || phase === "closed";
}

/** Why the owner's default Open sent before the live switch opened no session, so the area
 * names it while the live master still starts (LMS-M1). Input: the Open's result. Output:
 * the plain copy for a refused or failed Open; null when it applied, is still unconfirmed,
 * or the build has no Open, which the switch itself then shows. */
export function openProblem(opened: Result<IntentReceipt>): string | null {
  if (opened.kind === "not_ready") return null;
  if (opened.kind !== "ok") return failureCopy(opened);
  const state = receiptState(opened.value);
  return state.kind === "rejected" ? failureCopy(state) : null;
}

/** Why the owner's Open could not even be sent before the live switch: the project's master
 * could not be opened or read (LMS2-L1). Input: the failed open, or the master read when the
 * open went through. Output: the plain copy for a failure; null for a good read or a build
 * without it, which the switch itself then shows. */
export function readProblem(master: Result<unknown>): string | null {
  return master.kind === "error" ? failureCopy(master) : null;
}

/** What one open of the area does with the live master switch as last read: turn a switch
 * not on on, sending the owner's Open first, or only send that Open for a switch already on,
 * so an existing project's master gets its session too (LMS-1); nothing before the switch is
 * read or once this open did either. Inputs: the switch, whether this open turned it on and
 * whether it sent the Open check. Output: "start", "session" or null. */
export function onOpen(on: boolean | null | undefined, asked: boolean, checked: boolean): "start" | "session" | null {
  if (on === undefined) return null;
  if (on !== true) return asked ? null : "start";
  return asked || checked ? null : "session";
}

/** The toast for a live master start refused because the team settings changed in another
 * window since the area read them; the area then shows the saved switch again. Input: the
 * project name. Output: the text. */
export function liveStaleNote(name: string): string {
  return `The team settings of ${displayText(name || "this project")} changed in another window, so your live master switch was not saved. The master area now shows the saved switch.`;
}

/** The project's master area. A Codex or Claude Code master is its own app in a live
 * terminal filling the area, where Enter sends and Shift+Enter starts a new line; every open
 * of the area starts it, and it stops only on the master idle stop or when the app quits.
 * The plan's master is read through the harness catalog, so an alias such as "claude" is the
 * same Claude Code master (OP-28), and nothing is shown before the plan and catalog are read.
 * Input: the project data. Output: the area;
 * any other master, a project with Live terminal off, a project without a team plan and a plan
 * that couldn't be read keep the engine-driven conversation. */
export function MasterArea({ data }: { data: ProjectData }) {
  if (data.setup === undefined || data.harnesses === undefined) return <p className="pj-small pj-muted" role="status">Loading the master...</p>;
  const stored = valueOf(data.setup)?.team.master;
  const harness = stored === undefined ? undefined : entry(valueOf(data.harnesses), stored)?.id ?? stored;
  const area = harness === undefined || !runsLive(harness) || liveTerminalOff(data.team.result, data.id)
    ? <ConversationTab data={data} />
    : <LiveMaster key={data.id} data={data} harness={harness} />;
  return <MasterSettingsDrawer data={data} liveHarness={harness === undefined || runsLive(harness)}>{area}</MasterSettingsDrawer>;
}

function LiveMaster({ data, harness }: { data: ProjectData; harness: string }) {
  const [live, setLive] = useState<Live>({ kind: "starting" });
  const [want, setWant] = useState<Want | null>(null);
  const [openNote, setOpenNote] = useState<string | null>(null);
  const asked = useRef(false);
  const checked = useRef(false);
  const mounted = useRef(true);
  const sessions = useMasterSessions(data.id);
  const saved = useRef(0);
  const refused = useRef(-1);
  const engine = liveSwitch(data.team.result, data.id);
  const on = want?.on ?? engine?.on;

  const start = async (turn = true, clicked = true) => {
    const id = data.id;
    if (turn) {
      asked.current = true;
      setWant({ on: true, after: Infinity });
      setLive({ kind: "starting" });
    }
    setOpenNote(null);
    const opened = await sessions.opened();
    const master = opened.kind === "ok" ? await readMaster(opened.value) : opened;
    if (mounted.current) setOpenNote(readProblem(master));
    if (opened.kind === "ok" && openBeforeLive(master)) {
      const sent = await startMaster(opened.value, nextGeneration());
      if (mounted.current) setOpenNote(openProblem(sent));
      await sessions.reread();
    }
    if (!turn) return;
    const version = Math.max(engine?.version ?? 0, saved.current);
    const turned = await setLiveMaster(id, true, version);
    if (savedStale(turned)) {
      if (clicked) data.notify(liveStaleNote(data.name));
      if (!mounted.current) return;
      asked.current = false;
      refused.current = version;
      setWant(null);
      setLive({ kind: "starting" });
      data.reloadTeam();
      return;
    }
    const state = turned.kind === "ok" ? receiptState(turned.value) : null;
    const failed: Live | null = turned.kind === "not_ready" ? { kind: "not_ready" }
      : turned.kind !== "ok" ? { kind: "refused", code: turned.code, copy: failureCopy(turned) }
      : state?.kind === "rejected" ? { kind: "refused", code: state.code, copy: failureCopy(state) }
      : null;
    if (!mounted.current) return;
    if (failed) {
      setWant(null);
      setLive(failed);
    } else {
      saved.current = version + 1;
      setWant({ on: true, after: Date.now() });
    }
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
    const next = onOpen(on, asked.current, checked.current);
    if (next === "session") {
      checked.current = true;
      void start(false, false);
    }
    if (autoStart(on, asked.current, engine?.version ?? 0, refused.current, teamPaused(data.team.result, data.id))) void start(true, false);
  }, [on, data.team.at]);

  useEffect(() => {
    if (on === true) asked.current = true;
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
      setLive((prior) => runningState(prior, node, Date.now()));
    };
    void read();
    const timer = window.setInterval(() => void read(), POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [data.id, on === true]);

  const shown: Live = live.kind === "not_ready" ? live
    : teamPaused(data.team.result, data.id) ? { kind: "paused" }
    : live.kind === "refused" ? live
    : on === false ? { kind: "stopped", problem: engine?.problem ?? null, idle: idleStopped(data.team.result, data.id) }
    : live;
  return <LiveMasterView data={data} harness={harness} live={shown} onStart={() => start()} onSession={() => start(false)} sessions={sessions} openNote={openNote} />;
}

/** The master ticket and live session the live master's New session and Sessions controls
 * act on, with `opened` resolving to the ticket once the project's open settles, the failure
 * when it failed, or not ready when it never ran or the area moved to another project. */
export type MasterSessions = { ticket: string; liveId: string | null; reread: () => Promise<void>; opened: () => Promise<Result<string>> };

/** The master ticket and live session of one project, opened once per project as the
 * conversation opens them; a project open makes every earlier ticket stale, so one area
 * holds one. Inputs: the project id and whether this hook opens the project at all.
 * Output: the ticket, empty until opened, the live session id or null, a re-read for after
 * a switch and the settled open. */
export function useMasterSessions(projectId: string, enabled = true): MasterSessions {
  const [ticket, setTicket] = useState("");
  const [liveId, setLiveId] = useState<string | null>(null);
  const current = useRef(projectId);
  const opening = useRef<Promise<Result<string>>>(Promise.resolve({ kind: "not_ready" }));
  current.current = projectId;

  const read = async (opened: string, id: string) => {
    const state = await readMaster(opened);
    if (state.kind !== "ok" || current.current !== id) return;
    setLiveId(state.value.session?.phase === "configured" ? state.value.session.session_id : null);
  };

  useEffect(() => {
    setTicket("");
    setLiveId(null);
    if (!enabled) return;
    const id = projectId;
    opening.current = (async (): Promise<Result<string>> => {
      const opened = await openMasterProject(id);
      if (opened.kind !== "ok") return opened;
      if (opened.value.project_id !== id || current.current !== id) return { kind: "not_ready" };
      setTicket(opened.value.ticket);
      await read(opened.value.ticket, id);
      return { kind: "ok", value: opened.value.ticket };
    })();
  }, [projectId, enabled]);

  return {
    ticket,
    liveId,
    reread: async () => {
      const held = ticket || await opening.current.then((opened) => opened.kind === "ok" ? opened.value : "");
      if (held) await read(held, projectId);
    },
    opened: () => opening.current,
  };
}

/** The word and tone the console head shows for one live master state. Input: the state.
 * Output: a short word and the tone that colors it. */
export function liveWord(live: Live): { word: string; tone: "ok" | "muted" | "warn" | "danger" } {
  switch (live.kind) {
    case "running": return live.node ? { word: "Live", tone: "ok" } : { word: "Opening", tone: "muted" };
    case "starting": return { word: "Starting", tone: "muted" };
    case "stopped": return live.problem ? { word: "Stopped", tone: "warn" } : { word: "Stopped", tone: "muted" };
    case "paused": return { word: "Team stopped", tone: "warn" };
    case "refused": return { word: "Couldn't start", tone: "danger" };
    case "not_ready": return { word: "Not in this build", tone: "muted" };
  }
}

/** The live master area in one state: one console whose head names the master, its state
 * and account, then New session and Sessions, which restart the live terminal on a fresh or
 * resumed conversation and start the live master when it is stopped, above the master's
 * terminal filling the rest, typed into as the owner's Terminal input setting says, under the
 * engine's line while that terminal waits to start, or a line while it starts, can't or is
 * stopped, with Start again, the start of coming back to the tab, after an idle stop; a
 * stopped team (Stop all) says so and offers Start all. An engine that says the master has no terminal app
 * gets today's conversation instead. Under the Sessions bar the session's stored
 * conversation can be shown, so what an earlier master said stays readable. Inputs: the project data, the master harness id, the
 * state, the start action, the session check Start all sends once the team runs again, the
 * area's sessions when its owner already holds them and why the owner's Open opened no
 * session, if it did not. Output: the area. */
export function LiveMasterView({ data, harness, live, onStart, onSession, sessions: held, openNote = null }: { data: ProjectData; harness: string; live: Live; onStart: () => Promise<void>; onSession?: () => Promise<void>; sessions?: MasterSessions; openNote?: string | null }) {
  const [resuming, setResuming] = useState(false);
  const [resumeNote, setResumeNote] = useState("");
  const own = useMasterSessions(data.id, held === undefined);
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const sessions = held ?? own;
  const harnesses = valueOf(data.harnesses);
  const name = (id: string) => harnessName(harnesses, id);
  const waits = liveSwitch(data.team.result, data.id)?.problem ?? null;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), CLOCK_MS);
    return () => window.clearInterval(timer);
  }, []);
  const moved = live.kind === "running" ? movedNote(data.team.result, data.id, now) : null;
  const limits = live.kind === "running" ? masterLimitsLine(data.team.result, data.id, now) : null;
  const overdue = live.kind === "running" && !live.node && openOverdue(live.since, now);
  if (live.kind === "refused" && live.code === NO_TERMINAL) return <ConversationTab data={data} />;
  const off = live.kind === "stopped" || live.kind === "refused";
  const startTeam = async () => {
    setResuming(true);
    setResumeNote("");
    const note = changeNote(await setTeamStopped(data.id, false), "starting the team again");
    setResuming(false);
    setResumeNote(note);
    await onSession?.();
    data.reloadTeam();
  };
  const state = liveWord(live);
  const mark = live.kind === "running" ? live.node?.status ?? null : null;
  const accountId = live.kind === "running" ? live.node?.account ?? null : null;
  const account = accountId === null ? null : accountName(accounts.kind === "ok" ? accounts.value : [], accountId);
  const head = masterHead(harness, live.kind === "running" && live.node ? runningHarness(data.team.result, data.id, live.node.agent) : null, name);
  return <div className="pj-master-live pj-console">
    <div className="pj-console-head">
      <span className="pj-console-title"><Icon name="square-terminal" size={15} />{displayText(head.title)}</span>
      <span className={`pj-console-state ${state.tone}`}>{state.word}</span>
      {account && <span className="pj-console-detail">{displayText(account)}</span>}
      {head.waits && <span className="pj-console-detail" data-testid="master-switch-waits">{displayText(head.waits)}</span>}
      {limits && <span className="pj-console-detail" data-testid="master-limits">{displayText(limits)}</span>}
      <span className="pj-console-actions"><MasterSettingsButton /></span>
    </div>
    {live.kind !== "not_ready" && <SessionsBar ticket={sessions.ticket} liveId={sessions.liveId} mark={mark} harnesses={harnesses} onSwitched={async () => {
      await sessions.reread();
      if (off) await onStart();
      data.reloadTeam();
    }} />}
    <SwitchDivider team={data.team.result} />
    {moved && <div className="pj-row pj-small pj-muted" role="status" data-testid="master-moved">
      <span>{displayText(moved)}</span>
    </div>}
    {live.kind !== "not_ready" && <LiveConversation ticket={sessions.ticket} sessionId={sessions.liveId} mark={mark} harnesses={harnesses} />}
    {openNote && <div className="pj-note warn" role="alert">
      <p>{`${NO_SESSION} ${displayText(openNote)}`}</p>
    </div>}
    {live.kind === "stopped" && <div className={`pj-note${live.problem ? " warn" : ""}`} role={live.problem ? "alert" : "status"}>
      <p>{displayText(live.problem ?? STOPPED)}</p>
      {live.idle && <Button size="small" variant="primary" onClick={() => void onStart()}>Start again</Button>}
      {!live.idle && live.problem && <Button size="small" icon="refresh-cw" onClick={() => void onStart()}>Try again</Button>}
    </div>}
    {live.kind === "paused" && <div className="pj-note warn" role="status">
      <p>{PAUSED}</p>
      {resumeNote && <p role="alert">{displayText(resumeNote)}</p>}
      <Button size="small" variant="primary" pending={resuming} pendingLabel="Starting..." onClick={() => void startTeam()}>Start all</Button>
    </div>}
    {live.kind === "not_ready" && <NotReady what="the live master terminal isn't in this build." />}
    {live.kind === "refused" && <div className="pj-note warn" role="alert">
      <p>The master terminal couldn't start. {live.copy}</p>
      <Button size="small" icon="refresh-cw" onClick={() => void onStart()}>Try again</Button>
    </div>}
    {live.kind === "starting" && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />Starting the master ({displayText(name(harness))})...</p>}
    {overdue && <div className="pj-note warn" role="alert">
      <p>{waits === null ? OVERDUE : `${OVERDUE} ${displayText(waits)}`}</p>
      <Button size="small" icon="refresh-cw" onClick={() => void onStart()}>Try again</Button>
    </div>}
    {live.kind === "running" && !overdue && (!live.node || waits !== null) && <p className="tg-terminal-state" role="status"><Icon name="loader-circle" size={14} spin />{displayText(waits ?? WAITING)}</p>}
    {live.kind === "running" && live.node && <TerminalPanel key={`${live.node.terminal ?? live.node.id}:${sessions.liveId ?? ""}`} node={live.node} harness={name} embedded follow />}
  </div>;
}
