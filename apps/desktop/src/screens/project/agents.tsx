import { listAccounts, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { cancelResearch, cancelTeamTask, failureCopy, loadResearchLog, receiptState, retryResearch } from "../../api-projects";
import type { ResearchJob } from "../../api-projects";
import type { GraphNode, IntentReceipt, TeamStatus } from "../../engine-client";
import { displayText } from "../conversation";
import { poolName } from "../new-project/wizard";
import { RESEARCH_ICONS, harnessName } from "../new-project/team-editor";
import { accountName, changeOutcome, finishedRows, nearBottom, pauseSummary, researchActions, researchRow, teamPanel } from "./research";
import { AgentUsageCard } from "./usage";
import { FinishedAgentsCleanup } from "./cleanup";
import type { ResearchRow } from "./research";
import { FinishedAgentsPanel, TeamPanel } from "./team";
import { AgentsToolbar, TeamGraphView, useTeamGraph } from "../team";
import { teamLine } from "../team/graph";
import type { PerWindow } from "../terminals/pages";
import { SETTING_GROUPS, StopAll, TeamActions, TeamControls, type SettingGroupId } from "../team/controls";
import { TeamEventsPanel, WakeRulesPanel } from "../team/wake-rules";
import { QuestionsPanel, askCount, asksWords, questionsTitle, useQuestions } from "../team/questions";
import { Badge, NotReady, ProjectEmpty, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { Button, ConfirmDialog, Drawer, Icon, Segmented, navigate, type Tone } from "../../ui";
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

/** Which view of the Agents tab a route opens: Agent settings for "settings" or a setting
 * group's id, at that group, and the Team view for anything else. Input: the route's section.
 * Output: the view and the group to open at. */
export function agentsView(section: string | undefined): { view: "team" | "settings"; group: SettingGroupId | null } {
  if (section === "settings") return { view: "settings", group: null };
  const group = SETTING_GROUPS.find((item) => item.id === section);
  return group ? { view: "settings", group: group.id } : { view: "team", group: null };
}

const AGENTS_VIEWS: { value: "team" | "settings"; label: string }[] = [{ value: "team", label: "Team" }, { value: "settings", label: "Agent settings" }];

/** Pause all on Agents, beside Open all terminals: the project menu's Pause all agents, the
 * same confirm and the same engine call, which stops every running team task and cancels
 * every waiting one. Research agents keep running. Input: the project data. Output: the
 * button and its confirm. */
export function PauseAll({ data }: { data: ProjectData }) {
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const team = data.team.result;
  const summary = pauseSummary(team?.kind === "ok" ? team.value.tasks : []);
  const researching = (valueOf(data.research)?.jobs ?? []).some((job) => job.status === "running");

  function ask() {
    if (team === undefined) return data.notify("Still loading the team. Try again in a moment.");
    if (team.kind === "not_ready") return data.notify("Not ready yet: pausing agents isn't in this build.");
    if (team.kind === "error") return data.notify(`Couldn't load the team. ${failureCopy(team)}`);
    if (summary.count === 0) return data.notify(summary.line);
    setAsking(true);
  }
  async function pause() {
    setBusy(true);
    const result = await cancelTeamTask(data.id, null, nextGeneration());
    setBusy(false);
    setAsking(false);
    data.reloadTeam();
    if (result.kind !== "ok") return data.notify(result.kind === "not_ready" ? "Not ready yet: pausing agents isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind === "applied") return data.notify("Paused. Running tasks are stopping and waiting tasks are cancelled.");
    data.notify(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the Agents tab before trying again.");
  }
  return <>
    <Button size="small" icon="circle-pause" onClick={ask}>Pause all</Button>
    <ConfirmDialog open={asking} title="Pause all agents?" confirmLabel="Pause all agents" cancelLabel="Keep working" pending={busy}
      onCancel={() => setAsking(false)} onConfirm={() => void pause()}>
      {summary.line} Work so far stays in each agent's copy of the project. Stopped tasks don't start again on their own; ask the master when you want to continue.
      {researching ? " Research agents keep running." : ""}
    </ConfirmDialog>
  </>;
}

/** The Agents toolbar above Agent settings: the row the Team view draws over its graph, read
 * from the same team graph. Inputs: the project id and name, the terminals per window and
 * the row's parts. Output: the row. */
function SettingsToolbar({ projectId, name, perWindow, lead, asks, pool, tools }: {
  projectId: string; name: string; perWindow: PerWindow; lead: ReactNode; asks: ReactNode; pool: string; tools: ReactNode;
}) {
  const [read] = useTeamGraph(projectId);
  const team = read.shown?.kind === "ok" ? read.shown.value : null;
  return <AgentsToolbar projectId={projectId} name={name} perWindow={perWindow} team={team}
    line={team !== null && team.nodes.length > 0 ? teamLine(team) : null} lead={lead} asks={asks} pool={pool} tools={tools} />;
}

/** Agents tab: Team, as the approved mockup draws it: one toolbar row (Team and Agent
 * settings, the team line with the working line and the pool's account count, whose "N ask
 * you" opens the owner's questions and the sub-agent events in a drawer, Running and All with
 * their counts, Pause all, Open all terminals), a stopped team's line
 * with Start all again, the graph with its one terminal panel, then the closed Agents and
 * tasks fold (Stop all, Hand a task, the team's agents and tasks, wake rules, usage),
 * Finished agents and every research agent the project has started, with live progress from
 * the engine; or Agent settings under the same toolbar row, grouped as the mockup shows them.
 * Inputs: the project data and the route's section, which opens Agent settings, at a group
 * when it names one, or the questions drawer once on its questions or waiting agents, as a
 * toast's Open asks. Output: the tab; rows refresh while they poll. */
export function AgentsTab({ data, section }: { data: ProjectData; section?: string }) {
  const opened = agentsView(section);
  const [shownView, setShownView] = useState<"team" | "settings">(opened.view);
  useEffect(() => { setShownView(opened.view); }, [section]);
  const [filter, setFilter] = useState<"running" | "all">("running");
  const [stopping, setStopping] = useState<ResearchJob | null>(null);
  const [note, setNote] = useState("");
  const [logOpen, setLogOpen] = useState<number | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [terminal, setTerminal] = useState<GraphNode | null>(null);
  const [asking, setAsking] = useState(false);
  const [focus, setFocus] = useState<"questions" | "waiting" | null>(null);
  const [inbox, reloadInbox] = useQuestions(data.id);
  useEffect(() => { setTerminal(null); setAsking(false); setFocus(null); }, [data.id]);
  useEffect(() => {
    if (section !== "questions" && section !== "waiting") return;
    setAsking(true);
    setFocus(section);
    navigate({ name: "project", id: data.id, tab: "agents" }, true);
  }, [section, data.id]);
  const waits = stopping !== null && stopping.status !== "running";
  const jobs = valueOf(data.research)?.jobs ?? [];
  const harnesses = valueOf(data.harnesses);
  const [loadedAccounts] = useLoad(listAccounts, [], "accounts");
  const named = loadedAccounts.kind === "ok" ? loadedAccounts.value : [];
  const running = jobs.filter((job) => job.status === "running" || job.status === "queued");
  const list = filter === "running" ? running : jobs;
  const accounts = data.assigned?.pool.members.length ?? 0;
  const team = data.team;
  const teamValue: Pick<TeamStatus, "agents" | "tasks" | "terminals_per_window"> = team.result?.kind === "ok" ? team.result.value : { agents: [], tasks: [] };
  const teamCount = (shown: "running" | "all") => teamPanel(teamValue, [], (id) => id, shown, team.at).agents.length;
  const teamShown = teamPanel(teamValue, [], (id) => id, filter, team.at);
  const asks = inbox.shown?.kind === "ok" ? asksWords(askCount(inbox.shown.value)) : "Questions";
  const cleanup = <FinishedAgentsCleanup projectId={data.id} finished={teamValue.agents.filter((agent) => agent.status === "stopped" || agent.status === "failed").length} harness={(id) => harnessName(harnesses, id)} />;

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

  const viewSwitch = <Segmented label="Agents view" value={shownView} onValue={setShownView} options={AGENTS_VIEWS} />;
  const bar = {
    lead: viewSwitch,
    asks: <button type="button" className="tg-asks" aria-haspopup="dialog" onClick={() => { setFocus(null); setAsking(true); }}>{asks}</button>,
    pool: data.assigned ? `${poolName(data.assigned.pool)}: ${accounts} ${accounts === 1 ? "account" : "accounts"}` : "No pool. Pick a pool to start.",
    tools: <><Segmented label="Show" value={filter} onValue={setFilter}
      options={[{ value: "running", label: `Running (${running.length + teamCount("running")})` }, { value: "all", label: `All (${jobs.length + teamCount("all")})` }]} />
      <PauseAll data={data} /></>,
  };
  const perWindow = teamValue.terminals_per_window ?? 6;
  const drawer = <Drawer open={asking} title={questionsTitle(inbox)} onClose={() => { setAsking(false); setFocus(null); }}>
    <QuestionsPanel projectId={data.id} read={inbox} reload={reloadInbox} focus={focus} onOpen={(node) => { setAsking(false); setFocus(null); setShownView("team"); setTerminal(node); }} />
    {team.result?.kind === "ok" && <TeamEventsPanel projectId={data.id} status={team.result.value} reload={data.reloadTeam} />}
  </Drawer>;
  if (shownView === "settings") return <div className="pj-stack">
    <SettingsToolbar projectId={data.id} name={data.name} perWindow={perWindow} {...bar} />
    {team.result === undefined && <p className="pj-muted" role="status">Loading...</p>}
    {team.result?.kind === "not_ready" && <NotReady what="the live team isn't in this build." />}
    {team.result?.kind === "error" && <p className="pj-error" role="alert">Couldn't load the team. {failureCopy(team.result)}</p>}
    {team.result?.kind === "ok" && <TeamControls projectId={data.id} status={team.result.value} reload={data.reloadTeam} data={data} group={opened.group} />}
    {drawer}
  </div>;
  return <div className="pj-stack">
    {data.research?.kind === "not_ready" && <NotReady what="live agent status isn't in this build." />}
    {data.research?.kind === "error" && <p className="pj-error" role="alert">Couldn't load agents. {failureCopy(data.research)}</p>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {team.result?.kind === "ok" && team.result.value.paused && <StopAll projectId={data.id} status={team.result.value} reload={data.reloadTeam} />}
    <TeamGraphView projectId={data.id} perWindow={perWindow} name={data.name} open={terminal} onOpen={setTerminal} harness={(id) => harnessName(harnesses, id)} account={(id) => accountName(named, id)}
      {...bar} />
    <details className="pj-stack-s" aria-label="Agents and tasks">
      <summary><strong>Agents and tasks</strong> <span className="pj-small pj-muted">Stop all, Hand a task, the team's agents and tasks, wake rules and usage.</span></summary>
      {team.result?.kind === "ok" && <TeamActions projectId={data.id} status={team.result.value} harness={(id) => harnessName(harnesses, id)} reload={data.reloadTeam} />}
      <TeamPanel data={data} team={team} filter={filter} harnesses={harnesses} accounts={named} reload={data.reloadTeam} />
      {team.result?.kind === "ok" && <WakeRulesPanel projectId={data.id} status={team.result.value} reload={data.reloadTeam} />}
      <AgentUsageCard projectId={data.id} agents={teamValue.agents} nowMs={data.now} />
    </details>
    {finishedRows(teamValue, (id) => id, team.at).length > 0
      ? <FinishedAgentsPanel data={data} team={team} harnesses={harnesses} reload={data.reloadTeam}>{cleanup}</FinishedAgentsPanel>
      : cleanup}
    {list.length > 0 && <h3>Research</h3>}
    {list.length > 0 ? <ul className="pj-rows" aria-label="Research agents">{list.map((job) => {
      const row = researchRow(job, data.now);
      const status = agentStatus(row);
      const actions = researchActions(job);
      return <li className="pj-agent-row" key={job.id}>
        <strong className="pj-kind"><Icon name={job.kind === "roadmap_planning" ? "list-checks" : RESEARCH_ICONS[job.kind]} size={16} />{displayText(row.title)}</strong>
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
    {drawer}
  </div>;
}
