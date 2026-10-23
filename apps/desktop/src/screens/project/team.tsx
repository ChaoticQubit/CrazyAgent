import { listAccounts, nextGeneration } from "../../api";
import type { HarnessEntry } from "../../api";
import { useEffect, useState } from "react";
import { cancelTeamTask, failureCopy, loadTeamStatus, receiptState } from "../../api-projects";
import { useLoad } from "../../state";
import { ConfirmDialog, Icon, StatusBadge } from "../../ui";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { useLinkOpener } from "./links";
import { MarkdownView } from "./markdown";
import { teamPanel } from "./research";
import type { TeamFilter, TeamTaskRow } from "./research";
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
    const read = () => void loadTeamStatus(id).then((result) => {
      if (!live) return;
      setTeam({ result, at: Date.now() });
      if (result.kind === "not_ready" && timer !== undefined) window.clearInterval(timer);
    });
    read();
    timer = window.setInterval(read, TEAM_POLL_MS);
    return () => { live = false; window.clearInterval(timer); };
  }, [id, round]);
  return [team, () => setRound((value) => value + 1)];
}

/** Team panel for the project page: each agent's role, working folder, harness, account, status and current task,
 * then the tasks the master or the owner handed to sub-agents with status and a result summary.
 * Inputs: the project data, the latest team read, the filter, the harness catalog and a reload.
 * Output: the panel, or a loading, Not ready yet or error line. */
export function TeamPanel({ data, team, filter, harnesses, reload }: {
  data: ProjectData; team: TeamRead; filter: TeamFilter; harnesses: HarnessEntry[] | null; reload: () => void;
}) {
  const [accounts] = useLoad(listAccounts, []);
  const [stopping, setStopping] = useState<TeamTaskRow | null>(null);
  const [busy, setBusy] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const openLink = useLinkOpener(data.notify);

  async function stop(row: TeamTaskRow) {
    if (!row.stop) return;
    setBusy(true);
    const result = await cancelTeamTask(data.id, row.stop.task, nextGeneration());
    setBusy(false);
    setStopping(null);
    if (result.kind !== "ok") return setNote(result.kind === "not_ready" ? "Not ready yet: stopping team work isn't in this build." : failureCopy(result.code));
    const state = receiptState(result.value);
    setNote(state.kind === "applied" ? "" : state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check the list before trying again.");
    reload();
  }

  if (team.result === undefined) return <p className="pj-muted" role="status">Loading the team...</p>;
  if (team.result.kind === "not_ready") return <NotReady what="the live team isn't in this build." />;
  if (team.result.kind === "error") return <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(team.result.code)}</p>;
  const status = team.result.value;
  const panel = teamPanel(status, accounts.kind === "ok" ? accounts.value : [], (id) => harnessName(harnesses, id), filter, team.at);
  const working = status.agents.filter((agent) => agent.status === "working").length;
  return <section className="pj-stack-s" aria-labelledby="pj-team-title">
    <div className="pj-row between">
      <h3 id="pj-team-title">Team</h3>
      <span className="pj-secondary pj-small">{working} of {status.max_agents} agents working.</span>
    </div>
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {panel.agents.length > 0 ? <div className="pj-table-wrap"><table className="pj-table">
      <caption className="pj-sr">Team agents</caption>
      <thead><tr><th scope="col">Agent</th><th scope="col">Harness</th><th scope="col">Account</th><th scope="col">Status</th><th scope="col">Current task</th></tr></thead>
      <tbody>{panel.agents.map((row) => <tr key={row.key}>
        <th scope="row"><span className="pj-kind"><Icon name={row.role === "Master" ? "sparkles" : "bot"} size={16} />{displayText(row.name)}</span>
          {row.role !== row.name && <span className="pj-small pj-muted pj-block">{row.role}</span>}
          {row.folder && <span className="pj-small pj-muted pj-block">{displayText(row.folder)}</span>}</th>
        <td>{displayText(row.harness)}</td>
        <td>{displayText(row.account)}</td>
        <td><StatusBadge word={row.word} /></td>
        <td className="pj-secondary">{displayText(row.task)}</td>
      </tr>)}</tbody>
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
  </section>;
}
