import { nextGeneration } from "../../api";
import type { HarnessEntry } from "../../api";
import { Fragment, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { cancelTeamTask, failureCopy, loadTeamStatus, receiptState, renameTeamAgent, stopTeamAgent, stopTeammate, wakeTeamAgent } from "../../api-projects";
import { ConfirmDialog, Icon, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { AgentMessages } from "../team/agent-messages";
import { useLinkOpener } from "./links";
import { MarkdownView } from "./markdown";
import { agentKey, finishedLabel, finishedRows, liveAgent, teamLimitsLine, teamPanel } from "./research";
import type { FinishedRow, TeamAgentRow, TeamFilter, TeamTaskRow, TeammateRow } from "./research";
import { startRead, stampRead } from "../../state";
import { NotReady } from "./shared";
import type { ProjectData, TeamRead } from "./shared";

const TEAM_POLL_MS = 3000;

/** Reads the project's live team now and every few seconds while shown; stops asking when the
 * engine has no team command. Input: the project id. Output: the latest read with its time, and a reload. */
export function useTeam(id: string): [TeamRead, () => void] {
  const [team, setTeam] = useState<TeamRead>({ result: undefined, at: 0 });
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const read = () => {
      const started = startRead();
      void loadTeamStatus(id).then((result) => {
        stampRead(result, started);
        if (!live) return;
        setTeam({ result, at: Date.now() });
        if (result.kind === "not_ready" && timer !== undefined) window.clearInterval(timer);
      });
    };
    read();
    timer = window.setInterval(read, TEAM_POLL_MS);
    return () => { live = false; window.clearInterval(timer); };
  }, [id, round]);
  return [team, () => setRound((value) => value + 1)];
}

/** Team panel for the project page: each agent's role, working folder, harness, account, status and current task,
 * then the tasks the master or the owner handed to sub-agents with status and a result summary.
 * Inputs: the project data, the latest team read, the filter, the harness catalog, the owner's
 * account names and a reload.
 * Output: the panel, or a loading, Not ready yet or error line. */
export function TeamPanel({ data, team, filter, harnesses, accounts, reload }: {
  data: ProjectData; team: TeamRead; filter: TeamFilter; harnesses: HarnessEntry[] | null;
  accounts: { id: string; name: string }[]; reload: () => void;
}) {
  const [stopping, setStopping] = useState<TeamTaskRow | null>(null);
  const [busy, setBusy] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [renaming, setRenaming] = useState<{ agent: number; text: string } | null>(null);
  const [halting, setHalting] = useState<TeamAgentRow | null>(null);
  const [messages, setMessages] = useState<string | null>(null);
  const openLink = useLinkOpener(data.notify);

  async function rename() {
    if (!renaming) return;
    setBusy(true);
    const result = await renameTeamAgent(data.id, renaming.agent, renaming.text.trim(), nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? "Not ready yet: renaming agents isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind === "applied") setRenaming(null);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the list before trying again.");
    reload();
  }

  async function stop(row: TeamTaskRow) {
    if (!row.stop) return;
    setBusy(true);
    const result = await cancelTeamTask(data.id, row.stop.task, nextGeneration());
    setBusy(false);
    setStopping(null);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? "Not ready yet: stopping team work isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the list before trying again.");
    reload();
  }

  async function haltHarness(row: TeamAgentRow) {
    if (!row.stop) return;
    setBusy(true);
    const result = await stopTeamAgent(data.id, row.stop.agent, nextGeneration());
    setBusy(false);
    setHalting(null);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? "Not ready yet: stopping a harness isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the list before trying again.");
    reload();
  }

  async function haltTeammate(row: TeammateRow) {
    if (!row.stop) return;
    setBusy(true);
    const result = await stopTeammate(data.id, row.stop.agent, row.stop.pane, nextGeneration());
    setBusy(false);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? "Not ready yet: stopping a teammate isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the list before trying again.");
    reload();
  }

  if (team.result === undefined) return <p className="pj-muted" role="status">Loading the team...</p>;
  if (team.result.kind === "not_ready") return <NotReady what="the live team isn't in this build." />;
  if (team.result.kind === "error") return <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(team.result)}</p>;
  const status = team.result.value;
  const panel = teamPanel(status, accounts, (id) => harnessName(harnesses, id), filter, team.at);
  const working = status.agents.filter((agent) => agent.status === "working").length;
  const subs = new Map(status.agents.filter((agent) => agent.role === "sub").map((agent) => [agentKey(agent.id), agent]));
  return <section className="pj-stack-s" aria-labelledby="pj-team-title">
    <div className="pj-row between">
      <h3 id="pj-team-title">Team</h3>
      <span className="pj-secondary pj-small">{working} of {status.max_agents} agents working.</span>
    </div>
    <p className="pj-small pj-muted" aria-label="Agent limits">{displayText(teamLimitsLine(status, (id) => harnessName(harnesses, id)))} Change them in Agent settings.</p>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {panel.agents.length > 0 ? <div className="pj-table-wrap"><table className="pj-table">
      <caption className="pj-sr">Team agents</caption>
      <thead><tr><th scope="col">Agent</th><th scope="col">Harness</th><th scope="col">Account</th><th scope="col">Status</th><th scope="col">Current task</th></tr></thead>
      <tbody>{panel.agents.map((row) => { const sub = subs.get(row.key); return <Fragment key={row.key}><tr>
        <th scope="row" style={row.depth > 0 ? { paddingInlineStart: `${row.depth * 1.5}rem` } : undefined}>{renaming && row.rename?.agent === renaming.agent
          ? <form className="pj-row" onSubmit={(event) => { event.preventDefault(); void rename(); }}>
            <label htmlFor={`pj-rename-${row.key}`} className="pj-sr">New @name for {displayText(row.name)}</label>
            <span aria-hidden="true">@</span>
            <input id={`pj-rename-${row.key}`} className="pj-input" value={renaming.text} maxLength={33} spellCheck={false}
              onChange={(event) => setRenaming({ agent: renaming.agent, text: event.target.value })} />
            <button type="submit" className="btn primary small" disabled={busy || renaming.text.trim() === ""}>Save</button>
            <button type="button" className="btn quiet small" onClick={() => setRenaming(null)}>Cancel</button>
          </form>
          : <span className="pj-kind"><Icon name={row.role === "Master" ? "sparkles" : "bot"} size={16} />{displayText(row.name)}
            {row.rename && <button type="button" className="btn quiet small" aria-label={`Rename ${displayText(row.name)}`}
              onClick={() => { const target = row.rename; if (target) setRenaming({ agent: target.agent, text: target.handle }); }}>Rename</button>}</span>}
          {row.role !== row.name && <span className="pj-small pj-muted pj-block">{row.role}</span>}
          {row.lead && <span className="pj-small pj-muted pj-block">Worker of {displayText(row.lead)}</span>}
          {row.folder && <span className="pj-small pj-muted pj-block">{displayText(row.folder)}</span>}
          {row.messageNote && <span className="pj-small pj-block">{displayText(row.messageNote)}</span>}
          {row.teammates.length > 0 && <ul className="pj-rows" aria-label={`Teammates of ${displayText(row.name)}`}>{row.teammates.map((mate) => <li key={mate.key} className="pj-row">
            <span className="pj-small pj-secondary">Teammate in pane {displayText(mate.pane)}</span>
            <StatusBadge word={mate.word} />
            {mate.stop && <button type="button" className="btn danger-text" disabled={busy} aria-label={`Stop the teammate in pane ${displayText(mate.pane)}`}
              onClick={() => void haltTeammate(mate)}>Stop</button>}
          </li>)}</ul>}
          {sub && <span className="pj-block"><button type="button" className="btn quiet small" aria-label={`Messages for ${displayText(row.name)}`}
            aria-expanded={messages === row.key} onClick={() => setMessages(messages === row.key ? null : row.key)}>Messages</button></span>}</th>
        <td>{displayText(row.harness)}</td>
        <td>{displayText(row.account)}</td>
        <td><StatusBadge word={row.word} />
          {row.stoppedByYou && <span className="pj-small pj-muted pj-block">Stopped by you. Its task waits until you wake it.</span>}
          {row.stop && <button type="button" className="btn danger-text" disabled={busy} aria-label={row.lead ? `Stop ${displayText(row.name)}` : `Stop ${displayText(row.name)} and every agent it leads`}
            onClick={() => setHalting(row)}>Stop</button>}</td>
        <td className="pj-secondary">{displayText(row.task)}</td>
      </tr>
      {sub && messages === row.key && <tr><td colSpan={5}>
        <AgentMessages projectId={data.id} agent={sub.id} name={sub.label} refresh={liveAgent(sub.status) ? team.at : 0} notify={data.notify} />
      </td></tr>}</Fragment>; })}</tbody>
    </table></div> : <p className="pj-muted">{filter === "running" ? "No team agents are working. The master starts them when you ask it to build something." : "No team agents yet."}</p>}
    <h4>Tasks</h4>
    {panel.tasks.length > 0 ? <ul className="pj-rows" aria-label="Team tasks">{panel.tasks.map((row) => <li key={row.key} className="pj-task">
      <span className="pj-row between">
        <span className="pj-small pj-secondary">{row.from} to {displayText(row.to)}</span>
        <span className="pj-row"><StatusBadge word={row.word} />
          {row.elapsed && <span className="pj-small pj-muted">{row.elapsed}</span>}
          {row.stop && <button type="button" className="btn danger-text" onClick={() => setStopping(row)}>{row.stop.label}</button>}</span>
      </span>
      <strong>{displayText(row.prompt)}</strong>
      {row.summary && <span className="pj-secondary">{displayText(row.summary)}</span>}
      {row.result !== null && <span>
        <button type="button" className="btn quiet small" aria-expanded={shown === row.key} aria-controls={`pj-result-${row.key}`}
          onClick={() => setShown(shown === row.key ? null : row.key)}>{shown === row.key ? "Hide result" : "Show result"}</button>
      </span>}
      {row.result !== null && shown === row.key && <div className="pj-md" id={`pj-result-${row.key}`}><MarkdownView source={row.result} onOpenLink={openLink} /></div>}
    </li>)}</ul> : <p className="pj-muted">{filter === "running" ? "No tasks are waiting or running." : "No tasks yet."}</p>}
    <ConfirmDialog open={stopping !== null} title={stopping?.stop?.label === "Cancel" ? "Cancel this task?" : "Stop this task?"}
      confirmLabel={stopping?.stop?.label === "Cancel" ? "Cancel task" : "Stop task"} cancelLabel="Keep it" pending={busy}
      onCancel={() => setStopping(null)} onConfirm={() => { if (stopping) void stop(stopping); }}>
      {displayText(stopping?.prompt ?? "")}
    </ConfirmDialog>
    <ConfirmDialog open={halting !== null} title={`Stop ${displayText(halting?.name ?? "")}?`} confirmLabel={halting?.lead ? "Stop sub-agent" : "Stop harness"} cancelLabel="Keep it running" pending={busy}
      onCancel={() => setHalting(null)} onConfirm={() => { if (halting) void haltHarness(halting); }}>
      {halting?.lead
        ? `This sub-agent of ${displayText(halting.lead)} stops now, app-spawned or a native teammate, and stays stopped across app and engine restarts. Its lead is told. Its task waits for it until you wake it.`
        : "This harness, every sub-agent it leads and every teammate in its terminal stop now, and stay stopped across app and engine restarts. Its task waits for it, in its own worktree, until you wake it."}
    </ConfirmDialog>
  </section>;
}

/** The Finished fold-out under the Team panel (DEC-102): finished sub-agents, newest first, each
 * with its last task, that task's full result, how many tasks it ran and a Wake button that
 * wakes the same agent by its team id, or says why it can't; the worktree cleanup sits inside.
 * Nothing shows while no agent is finished.
 * Inputs: the project data, the latest team read, the harness catalog, a reload and the cleanup.
 * Output: the fold-out, or nothing. */
export function FinishedAgentsPanel({ data, team, harnesses, reload, children }: {
  data: ProjectData; team: TeamRead; harnesses: HarnessEntry[] | null; reload: () => void; children?: ReactNode;
}) {
  const [shown, setShown] = useState<string | null>(null);
  const [waking, setWaking] = useState<number | null>(null);
  const [messages, setMessages] = useState<number | null>(null);
  const [note, setNote] = useState("");
  const openLink = useLinkOpener(data.notify);
  if (team.result?.kind !== "ok") return null;
  const rows = finishedRows(team.result.value, (id) => harnessName(harnesses, id), team.at || data.now);
  if (rows.length === 0) return null;

  async function wake(row: FinishedRow) {
    if (!row.wakeable) return;
    setWaking(row.agent);
    const result = await wakeTeamAgent(data.id, row.agent, nextGeneration());
    setWaking(null);
    if (result.kind !== "ok") {
      setNote(result.kind === "not_ready" ? "Not ready yet: waking agents isn't in this build." : failureCopy(result));
      return reload();
    }
    const state = receiptState(result.value);
    if (state.kind === "applied") {
      setNote("");
      data.notify(`Waking ${displayText(row.name)}.`);
    } else setNote(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm the wake. Check the list before trying again.");
    reload();
  }

  return <details className="pj-stack-s" aria-label="Finished agents">
    <summary><strong>{finishedLabel(rows.length)}</strong> <span className="pj-small pj-muted">Stopped and failed agents. Their results and worktrees stay until you clean them up.</span></summary>
    {note && <p className="pj-note warn" role="alert">{displayText(note)}</p>}
    <ul className="pj-rows" aria-label="Finished agents">{rows.map((row) => <li key={row.key} className="pj-task">
      <span className="pj-row between">
        <span className="pj-kind"><Icon name="bot" size={16} />{displayText(row.name)} <span className="pj-small pj-secondary">{displayText(row.harness)}</span></span>
        <span className="pj-row"><StatusBadge word={row.word} />
          {row.ended && <span className="pj-small pj-muted">{row.ended}</span>}
          <button type="button" className="btn small" disabled={!row.wakeable || waking !== null}
            title={row.wakeNote ?? undefined}
            onClick={() => void wake(row)}>{waking === row.agent ? "Waking..." : "Wake"}</button></span>
      </span>
      {row.wakeNote !== null && <span className="pj-small pj-muted">{displayText(row.wakeNote)}</span>}
      <span className="pj-secondary">{displayText(row.task)}</span>
      <span className="pj-small pj-muted">{row.tasks} {row.tasks === 1 ? "task" : "tasks"} run.</span>
      {row.result !== null && <span>
        <button type="button" className="btn quiet small" aria-expanded={shown === row.key} aria-controls={`pj-finished-${row.key}`}
          onClick={() => setShown(shown === row.key ? null : row.key)}>{shown === row.key ? "Hide result" : "Show result"}</button>
      </span>}
      {row.result !== null && shown === row.key && <div className="pj-md" id={`pj-finished-${row.key}`}><MarkdownView source={row.result} onOpenLink={openLink} /></div>}
      <span><button type="button" className="btn quiet small" aria-label={`Messages for ${displayText(row.name)}`} aria-expanded={messages === row.agent}
        onClick={() => setMessages(messages === row.agent ? null : row.agent)}>Messages</button></span>
      {messages === row.agent && <AgentMessages projectId={data.id} agent={row.agent} name={row.name} refresh={0} notify={data.notify} />}
    </li>)}</ul>
    {children}
  </details>;
}
