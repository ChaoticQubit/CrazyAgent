import { listAccounts, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState } from "react";
import { cancelResearch, failureCopy, loadResearchLog, retryResearch } from "../../api-projects";
import type { ResearchJob } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { displayText } from "../conversation";
import { poolName } from "../new-project/wizard";
import { RESEARCH_ICONS, harnessName } from "../new-project/team-editor";
import { accountName, changeOutcome, nearBottom, researchActions, researchRow, teamPanel } from "./research";
import { AgentUsageCard } from "./usage";
import type { ResearchRow } from "./research";
import { TeamPanel } from "./team";
import { TeamGraphView } from "../team";
import { TeamControls } from "../team/controls";
import { cardActivity } from "../dashboard";
import { Badge, NotReady, ProjectEmpty, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { ConfirmDialog, Icon, Segmented, type Tone } from "../../ui";
import { useLoad } from "../../state";

/** Words an agent row uses for a research job, from the design's fixed agent set.
 * Input: the research row. Output: the agent status word and tone. */
export function agentStatus(row: ResearchRow): { word: string; tone: Tone } {
  switch (row.status) {
    case "Queued": return { word: "Starting", tone: "info" };
    case "Needs a pool": return { word: "Needs a pool", tone: "warn" };
    default: return { word: row.status, tone: row.tone };
  }
}

function AgentLog({ data, job }: { data: ProjectData; job: ResearchJob }) {
  const [log, setLog] = useState<Result<string[]>>();
  const [follow, setFollow] = useState(true);
  const list = useRef<HTMLOListElement>(null);
  const live = job.status === "running";
  const latest = () => {
    if (list.current) list.current.scrollTop = list.current.scrollHeight;
    setFollow(true);
  };
  useEffect(() => {
    let current = true;
    void loadResearchLog(data.id, job.id).then((result) => { if (current) setLog(result); });
    return () => { current = false; };
  }, [data.id, job.id, live ? data.now : 0]);
  useEffect(() => {
    if (follow && list.current) list.current.scrollTop = list.current.scrollHeight;
  }, [log]);
  return <div className="pj-log" id={`pj-log-${job.id}`}>
    {log === undefined && <p className="pj-muted pj-small" role="status">Loading log...</p>}
    {log?.kind === "not_ready" && <NotReady what="the agent log isn't in this build." />}
    {log?.kind === "error" && <p className="pj-error" role="alert">Couldn't load the log. {failureCopy(log)}</p>}
    {log?.kind === "ok" && (log.value.length
      ? <>
        <ol className="pj-log-lines" ref={list} aria-label={`${displayText(job.kind_label)} log`} onScroll={(event) => setFollow(nearBottom(event.currentTarget))}>
          {log.value.map((line, index) => <li key={index}>{displayText(line)}</li>)}</ol>
        {!follow && <button type="button" className="btn quiet" onClick={latest}>Jump to latest</button>}
      </>
      : <p className="pj-muted pj-small">No log lines yet.</p>)}
  </div>;
}

/** Agents tab: the live team panel, then every research agent the project has started, with live
 * progress from the engine. Input: the project data. Output: the tab; rows refresh while they poll. */
export function AgentsTab({ data }: { data: ProjectData }) {
  const [filter, setFilter] = useState<"running" | "all">("running");
  const [stopping, setStopping] = useState<ResearchJob | null>(null);
  const [note, setNote] = useState("");
  const [logOpen, setLogOpen] = useState<number | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const waits = stopping !== null && stopping.status !== "running";
  const jobs = valueOf(data.research)?.jobs ?? [];
  const harnesses = valueOf(data.harnesses);
  const [loadedAccounts] = useLoad(listAccounts, []);
  const named = loadedAccounts.kind === "ok" ? loadedAccounts.value : [];
  const running = jobs.filter((job) => job.status === "running" || job.status === "queued");
  const list = filter === "running" ? running : jobs;
  const accounts = data.assigned?.pool.members.length ?? 0;
  const team = data.team;
  const teamValue = team.result?.kind === "ok" ? team.result.value : { agents: [], tasks: [] };
  const teamCount = (shown: "running" | "all") => teamPanel(teamValue, [], (id) => id, shown, team.at).agents.length;
  const teamShown = teamPanel(teamValue, [], (id) => id, filter, team.at);
  const working = cardActivity(team.result, data.research).agents;

  function settle(result: Result<IntentReceipt>, done: string, missing: string) {
    setBusy(null);
    const outcome = changeOutcome(result, done, missing);
    setNote(outcome.note);
    if (outcome.toast) data.notify(outcome.toast);
    if (outcome.reload) data.reload();
  }

  async function stop(job: ResearchJob) {
    setStopping(null);
    setBusy(job.id);
    const label = displayText(job.kind_label);
    settle(await cancelResearch(data.id, job.id, nextGeneration()), job.status === "running" ? `Stopping ${label}.` : `${label} cancelled.`, "stopping research");
  }

  async function retry(job: ResearchJob) {
    setBusy(job.id);
    settle(await retryResearch(data.id, job.id, nextGeneration()), `${displayText(job.kind_label)} is queued again.`, "trying research again");
  }

  return <div className="pj-stack">
    <div className="pj-row between">
      <Segmented label="Show" value={filter} onValue={setFilter}
        options={[{ value: "running", label: `Running (${running.length + teamCount("running")})` }, { value: "all", label: `All (${jobs.length + teamCount("all")})` }]} />
      <span className="pj-secondary">{data.assigned
        ? `${working ? `${working}. ` : ""}${displayText(poolName(data.assigned.pool))}: ${accounts} ${accounts === 1 ? "account" : "accounts"}.`
        : "No pool. Pick a pool to start."}</span>
    </div>
    {data.research?.kind === "not_ready" && <NotReady what="live agent status isn't in this build." />}
    {data.research?.kind === "error" && <p className="pj-error" role="alert">Couldn't load agents. {failureCopy(data.research)}</p>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <TeamGraphView projectId={data.id} harness={(id) => harnessName(harnesses, id)} account={(id) => accountName(named, id)} />
    {team.result?.kind === "ok" && <TeamControls projectId={data.id} status={team.result.value} harness={(id) => harnessName(harnesses, id)} reload={data.reloadTeam} />}
    <TeamPanel data={data} team={team} filter={filter} harnesses={harnesses} accounts={named} reload={data.reloadTeam} />
    <AgentUsageCard projectId={data.id} agents={teamValue.agents} nowMs={data.now} />
    {list.length > 0 && <h3>Research</h3>}
    {list.length > 0 ? <ul className="pj-rows" aria-label="Research agents">{list.map((job) => {
      const row = researchRow(job, data.now);
      const status = agentStatus(row);
      const actions = researchActions(job);
      return <li className="pj-agent-row" key={job.id}>
        <strong className="pj-kind"><Icon name={RESEARCH_ICONS[job.kind]} size={16} />{displayText(row.title)}</strong>
        <span className="pj-secondary pj-small">{harnessName(harnesses, job.harness)}</span>
        <span><Badge word={status.word} tone={status.tone} /> <span className="pj-small pj-secondary">{displayText(row.progress)}</span></span>
        <span className="pj-small pj-muted">{row.elapsed}</span>
        <span className="pj-row">{actions.log && <button type="button" className="btn" aria-expanded={logOpen === job.id} aria-controls={`pj-log-${job.id}`}
          onClick={() => setLogOpen(logOpen === job.id ? null : job.id)}>{logOpen === job.id ? "Hide log" : "View log"}</button>}
          {actions.retry && <button type="button" className="btn" disabled={busy === job.id} onClick={() => void retry(job)}>Try again</button>}
          {actions.stop && <button type="button" className="btn danger-text" disabled={busy === job.id} onClick={() => setStopping(job)}>{actions.stop}</button>}</span>
        <span className="pj-wt pj-small pj-muted">Writes <span className="pj-mono">{displayText(job.report_path)}</span>. Tools {job.tools ? "on" : "off"}.{job.attempts > 1 ? ` Attempt ${job.attempts}.` : ""}</span>
        {logOpen === job.id && <AgentLog data={data} job={job} />}
      </li>;
    })}</ul> : data.research?.kind === "ok" && teamShown.agents.length + teamShown.tasks.length === 0 &&
      <ProjectEmpty title={filter === "running" && jobs.length ? "No agents running right now" : "No agents yet"}>
        Research agents start from the Work step, and the master starts agents when you ask it to build something.
      </ProjectEmpty>}
    <ConfirmDialog open={stopping !== null} title={`${waits ? "Cancel" : "Stop"} ${displayText(stopping?.kind_label ?? "this agent")}?`}
      confirmLabel={waits ? "Cancel research" : "Stop agent"} cancelLabel={waits ? "Keep it" : "Keep running"}
      onCancel={() => setStopping(null)} onConfirm={() => { if (stopping) void stop(stopping); }}>
      {waits ? "It won't start. Try again queues it later." : "Work so far stays in its working copy. Try again starts it over."}
    </ConfirmDialog>
  </div>;
}
