import { call, nextGeneration } from "../../api";
import type { Result } from "../../api";
import { useEffect, useRef, useState } from "react";
import { failureCopy, loadResearchLog } from "../../api-projects";
import type { ResearchJob } from "../../api-projects";
import { displayText } from "../conversation";
import { poolName } from "../new-project/wizard";
import { RESEARCH_ICONS, harnessName } from "../new-project/team-editor";
import { nearBottom, researchRow, teamPanel } from "./research";
import type { ResearchRow } from "./research";
import { TeamPanel } from "./team";
import { Badge, NotReady, ProjectEmpty, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { ConfirmDialog, Icon, Segmented, type Tone } from "../../ui";

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
    {log?.kind === "error" && <p className="pj-error" role="alert">Couldn't load the log. {failureCopy(log.code)}</p>}
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
  const jobs = valueOf(data.research)?.jobs ?? [];
  const setup = valueOf(data.setup);
  const harnesses = valueOf(data.harnesses);
  const running = jobs.filter((job) => job.status === "running" || job.status === "queued");
  const list = filter === "running" ? running : jobs;
  const accounts = data.assigned?.pool.members.length ?? 0;
  const team = data.team;
  const teamValue = team.result?.kind === "ok" ? team.result.value : { agents: [], tasks: [] };
  const teamCount = (shown: "running" | "all") => teamPanel(teamValue, [], (id) => id, shown, team.at).agents.length;
  const teamShown = teamPanel(teamValue, [], (id) => id, filter, team.at);

  async function stop(job: ResearchJob) {
    setStopping(null);
    const result = await call<unknown>("desktop_cancel_research", { id: data.id, job: job.id, generation: nextGeneration() });
    if (result.kind === "ok") {
      setNote("");
      data.reload();
    } else setNote(result.kind === "not_ready" ? "Not ready yet: stopping an agent isn't in this build." : failureCopy(result.code));
  }

  return <div className="pj-stack">
    <div className="pj-row between">
      <Segmented label="Show" value={filter} onValue={setFilter}
        options={[{ value: "running", label: `Running (${running.length + teamCount("running")})` }, { value: "all", label: `All (${jobs.length + teamCount("all")})` }]} />
      <span className="pj-secondary">{data.assigned
        ? `${jobs.filter((job) => job.status === "running").length} of ${setup?.team.max_agents ?? "?"} agents running. ${displayText(poolName(data.assigned.pool))}: ${accounts} ${accounts === 1 ? "account" : "accounts"}.`
        : "No pool. Agents wait until you choose one."}</span>
    </div>
    {data.research?.kind === "not_ready" && <NotReady what="live agent status isn't in this build." />}
    {data.research?.kind === "error" && <p className="pj-error" role="alert">Couldn't load agents. {failureCopy(data.research.code)}</p>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    <TeamPanel data={data} team={team} filter={filter} harnesses={harnesses} reload={data.reloadTeam} />
    {list.length > 0 && <h3>Research</h3>}
    {list.length > 0 ? <ul className="pj-rows" aria-label="Research agents">{list.map((job) => {
      const row = researchRow(job, data.now);
      const status = agentStatus(row);
      return <li className="pj-agent-row" key={job.id}>
        <strong className="pj-kind"><Icon name={RESEARCH_ICONS[job.kind]} size={16} />{displayText(row.title)}</strong>
        <span className="pj-secondary pj-small">{harnessName(harnesses, job.harness)}</span>
        <span><Badge word={status.word} tone={status.tone} /> <span className="pj-small pj-secondary">{displayText(row.progress)}</span></span>
        <span className="pj-small pj-muted">{row.elapsed}</span>
        <span className="pj-row">{job.started_ms !== null && <button type="button" className="btn" aria-expanded={logOpen === job.id} aria-controls={`pj-log-${job.id}`}
          onClick={() => setLogOpen(logOpen === job.id ? null : job.id)}>{logOpen === job.id ? "Hide log" : "View log"}</button>}
          {job.status === "running" && !job.cancel_requested && <button type="button" className="btn danger-text" onClick={() => setStopping(job)}>Stop</button>}</span>
        <span className="pj-wt pj-small pj-muted">Writes <span className="pj-mono">{displayText(job.report_path)}</span>. Tools {job.tools ? "on" : "off"}.{job.attempts > 1 ? ` Attempt ${job.attempts}.` : ""}</span>
        {logOpen === job.id && <AgentLog data={data} job={job} />}
      </li>;
    })}</ul> : data.research?.kind === "ok" && teamShown.agents.length + teamShown.tasks.length === 0 &&
      <ProjectEmpty title={filter === "running" && jobs.length ? "No agents running right now" : "No agents yet"}>
        Research agents start from the Work step, and the master starts agents when you ask it to build something.
      </ProjectEmpty>}
    <ConfirmDialog open={stopping !== null} title={`Stop ${displayText(stopping?.kind_label ?? "this agent")}?`} confirmLabel="Stop agent" cancelLabel="Keep running"
      onCancel={() => setStopping(null)} onConfirm={() => { if (stopping) void stop(stopping); }}>
      Work so far stays in its working copy.
    </ConfirmDialog>
  </div>;
}
