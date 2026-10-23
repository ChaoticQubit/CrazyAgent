import { useEffect, useState } from "react";
import { Icon } from "../../ui";
import { listMcpServers, listSkills, nextGeneration, poolSummaries } from "../../api";
import { failureCopy, loadProjectMemories, receiptState, retryResearch } from "../../api-projects";
import type { ResearchJob } from "../../api-projects";
import { displayText } from "../conversation";
import { RESEARCH_ICONS, harnessName } from "../new-project/team-editor";
import { poolName, poolSummary } from "../new-project/wizard";
import { poolReadiness } from "../settings/pools";
import { useLoad } from "../../state";
import { homePath } from "../settings/harnesses";
import { Badge, NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { ACTIVITY_ROWS, activity, ago, memoryGroups, researchRow, onFact, signInLine, suggestedLine, teamWaitingLine, waitingLine } from "./research";

const PLATFORM_LABELS: Record<string, string> = {
  website: "Website", mobile_app: "Mobile app", desktop_app: "Desktop app", cli: "CLI", api_backend: "API or backend", other: "Other",
};

/** Shows a project folder path with "~" for the macOS home folder and long engine ids
 * cut to eight characters. Input: the absolute path. Output: the display path. */
export function folderLabel(path: string): string {
  const short = path.split("/").map((part) => part.replace(/^(.*?[^0-9a-f]|)([0-9a-f]{8})[0-9a-f]{8,}$/u, "$1$2...")).join("/");
  return homePath(short);
}

/** Overview tab: what needs the owner, research progress, recent activity and project facts.
 * Input: the project data. Output: the tab. */
export function OverviewTab({ data }: { data: ProjectData }) {
  const setup = valueOf(data.setup);
  const jobs = valueOf(data.research)?.jobs ?? [];
  const harnesses = valueOf(data.harnesses);
  const base = `/projects/${encodeURIComponent(data.id)}`;
  const waiting = jobs.some((job) => job.status === "needs_pool") || (setup !== null && !data.assigned && setup.research.length > 0 && data.pools?.kind === "ok");
  const rows = jobs.map((job) => ({ job, row: researchRow(job, data.now) }));
  const allDone = jobs.length > 0 && jobs.every((job) => job.status === "done");
  const events = activity(jobs, data.name, setup?.created_ms ?? null);
  const [suggested, setSuggested] = useState(0);
  useEffect(() => {
    let live = true;
    void loadProjectMemories(data.id, null).then((result) => { if (live) setSuggested(result.kind === "ok" ? memoryGroups(result.value).suggested.length : 0); });
    return () => { live = false; };
  }, [data.id, data.now]);
  const [retrying, setRetrying] = useState<number | null>(null);
  const [allActivity, setAllActivity] = useState(false);
  const [skills] = useLoad(() => listSkills(data.id), [data.id]);
  const [servers] = useLoad(() => listMcpServers(data.id), [data.id]);
  const [summaries] = useLoad(poolSummaries, [data.id]);
  const assignedSummary = summaries.kind === "ok" ? summaries.value.find((pool) => pool.id === data.assigned?.pool.id) : undefined;
  const signIn = signInLine(assignedSummary);
  const teamWaiting = data.team.result?.kind === "ok" ? teamWaitingLine(data.team.result.value.tasks) : null;
  async function retry(job: ResearchJob) {
    setRetrying(job.id);
    const result = await retryResearch(data.id, job.id, nextGeneration());
    setRetrying(null);
    if (result.kind === "not_ready") return data.notify("Not ready yet: trying research again isn't in this build.");
    if (result.kind === "error") return data.notify(failureCopy(result.code));
    const state = receiptState(result.value);
    if (state.kind === "rejected") return data.notify(failureCopy(state.code));
    data.reload();
  }
  return <div className="pj-overview">
    <div className="pj-stack">
      {(waiting || suggested > 0 || signIn || teamWaiting) && <section className="pj-stack-s" aria-labelledby="pj-needs"><h2 id="pj-needs">Needs you</h2>
        {waiting && <div className="pj-need"><span>{displayText(waitingLine(jobs))}</span>
          <button type="button" className="btn primary" onClick={() => data.navigate(`${base}/settings?section=pool`)}>Choose pool</button></div>}
        {teamWaiting && <div className="pj-need"><span>{displayText(teamWaiting)}</span>
          <button type="button" className="btn" onClick={() => data.navigate(`${base}/settings?section=pool`)}>{data.assigned ? "Change pool" : "Choose pool"}</button></div>}
        {signIn && <div className="pj-need"><span>{displayText(signIn)}</span>
          <button type="button" className="btn" onClick={() => data.navigate("/settings/accounts")}>Sign in again</button></div>}
        {suggested > 0 && <div className="pj-need"><span>{suggestedLine(suggested)}</span>
          <button type="button" className="btn" onClick={() => data.navigate(`${base}/context?file=%40memory`)}>Review</button></div>}
      </section>}
      <section className="pj-stack-s" aria-labelledby="pj-research"><h2 id="pj-research">Research</h2>
        {data.research === undefined && <p className="pj-muted" role="status">Loading research...</p>}
        {data.research?.kind === "not_ready" && <NotReady what="research progress isn't in this build." />}
        {data.research?.kind === "error" && <p className="pj-error" role="alert">Couldn't load research. {failureCopy(data.research.code)}</p>}
        {data.research?.kind === "ok" && jobs.length === 0 && <p className="pj-muted">No research was picked for this project.</p>}
        {rows.length > 0 && <ul className="pj-rows" aria-label="Research">{rows.map(({ job, row }) => <li className="pj-research-row" key={job.id}>
          <strong className="pj-kind"><Icon name={RESEARCH_ICONS[job.kind]} size={16} />{displayText(row.title)}</strong>
          <Badge word={row.status} tone={row.tone} />
          <span className="pj-secondary">{displayText(row.progress)}</span>
          <span className="pj-small pj-muted">{row.elapsed}</span>
          <span>{row.report && <button type="button" className="btn" onClick={() => data.navigate(`${base}/context?file=${encodeURIComponent(job.report_path)}`)}>Open report</button>}
            {job.status === "failed" && <button type="button" className="btn" disabled={retrying === job.id} onClick={() => void retry(job)}>Try again</button>}</span>
        </li>)}</ul>}
        {allDone && <p>Research finished. <button type="button" className="pj-link" onClick={() => data.navigate(`${base}/conversation`)}>Ask the master what to build first.</button></p>}
      </section>
      <section className="pj-stack-s" aria-labelledby="pj-activity"><h2 id="pj-activity">Recent activity</h2>
        {events.length ? <ul className="pj-rows" aria-label="Recent activity">{(allActivity ? events : events.slice(0, ACTIVITY_ROWS)).map((event, index) => <li className="pj-activity-row" key={index}>
          <span className="pj-small pj-muted">{ago(event.at, data.now)}</span><span>{displayText(event.text)}</span></li>)}</ul>
          : <p className="pj-muted">Nothing yet.</p>}
        {!allActivity && events.length > ACTIVITY_ROWS && <button type="button" className="pj-link" onClick={() => setAllActivity(true)}>Show all ({events.length})</button>}
      </section>
    </div>
    <aside className="pj-panel pj-facts" aria-label="Project facts">
      {data.setup?.kind === "not_ready" && <NotReady what="project details aren't in this build." />}
      {data.setup?.kind === "ok" && data.setup.value === null && <p className="pj-muted">This project was created before project setup. It has no folder, research or team details.</p>}
      <dl>
        {setup && <div><dt>Project folder</dt><dd className="pj-mono" title={setup.folder}>{displayText(folderLabel(setup.folder))}</dd></div>}
        {setup && <div><dt>Tech stack</dt><dd className="pj-chips">{setup.tech_stack.length ? setup.tech_stack.map((item) => <span className="pj-chip" key={item}>{displayText(item)}</span>) : <span className="pj-muted">None</span>}</dd></div>}
        {setup && <div><dt>Platforms</dt><dd>{setup.platforms.map((platform) => PLATFORM_LABELS[platform] ?? platform).join(", ") || "None"}</dd></div>}
        <div><dt>Pool</dt><dd>{data.assigned
          ? <><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=pool`)}>{displayText(assignedSummary?.name ?? poolName(data.assigned.pool))}</button> <span className="pj-small pj-muted">{assignedSummary ? `${poolSummary(data.assigned.members)}, ${poolReadiness(assignedSummary.members)}` : poolSummary(data.assigned.members)}</span></>
          : data.pools?.kind === "ok" ? <Badge word="Needs a pool" tone="warn" /> : <span className="pj-muted">{data.pools === undefined ? "Loading..." : "Unknown"}</span>}</dd></div>
        {setup && <div><dt>Team</dt><dd>Master: {harnessName(harnesses, setup.team.master)}</dd>
          <dd className="pj-secondary">{setup.team.sub_harnesses.length ? `Agents: ${setup.team.sub_harnesses.map((slot) => `${harnessName(harnesses, slot.harness)} x${slot.agents}`).join(", ")}` : "No agents beside the master"}</dd></div>}
        {setup && setup.documents.length > 0 && <div><dt>Documents</dt><dd>{setup.documents.length} attached</dd></div>}
        <div><dt>Skills</dt><dd><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=plugins`)}>{onFact(skills, "skill", "skills") ?? "Project settings"}</button></dd></div>
        <div><dt>MCP servers</dt><dd><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=plugins`)}>{onFact(servers, "server", "servers") ?? "Project settings"}</button></dd></div>
      </dl>
    </aside>
  </div>;
}
