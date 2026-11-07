import { useEffect, useState } from "react";
import { Icon, type IconName } from "../../ui";
import { listMcpServers, listSkills, loadAllProjectMemories, nextGeneration, poolSummaries } from "../../api";
import { failureCopy, loadActivity, loadAgentPolicy, retryResearch } from "../../api-projects";
import type { ActivityItem, ActivityKind, ResearchJob } from "../../api-projects";
import type { Result } from "../../api";
import { displayText } from "../conversation";
import { RESEARCH_ICONS, harnessName } from "../new-project/team-editor";
import { anyHarnessTotal, poolName, poolSummary } from "../new-project/wizard";
import { poolReadiness } from "../settings/pools";
import { useLoad } from "../../state";
import { homePath } from "../settings/harnesses";
import { accessSummary } from "./access";
import { Badge, NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { ACTIVITY_ROWS, ago, changeOutcome, groupActivity, memoryGroups, researchJobs, researchRow, onFact, signInLine, suggestedLine, teamWaitingLine, waitingLine } from "./research";
import { PlanningLine } from "./planning";

const PLATFORM_LABELS: Record<string, string> = {
  website: "Website", mobile_app: "Mobile app", desktop_app: "Desktop app", cli: "CLI", api_backend: "API or backend", other: "Other",
};

/** Shows a project folder path with "~" for the macOS home folder and long engine ids
 * cut to eight characters. Input: the absolute path. Output: the display path. */
export function folderLabel(path: string): string {
  const short = path.split("/").map((part) => part.replace(/^(.*?[^0-9a-f]|)([0-9a-f]{8})[0-9a-f]{8,}$/u, "$1$2...")).join("/");
  return homePath(short);
}

/** The owner's project folder for a saved setup folder, which for a project set up in an
 * existing folder names the `.crazyagent` folder inside it.
 * Input: the saved folder path. Output: the project folder path. */
export function projectFolder(path: string): string {
  return path.replace(/[\\/]\.crazyagent[\\/]?$/u, "");
}

/** How often Overview reads the project's activity again while the window is visible. */
export const ACTIVITY_READ_MS = 5_000;
const ACTIVITY_READ_MAX = 200;

const KINDS: Record<ActivityKind, { icon: IconName; tone: "" | "ok" | "warn" | "bad" | "merge" }> = {
  created: { icon: "folder-plus", tone: "" },
  research_started: { icon: "search", tone: "" },
  research_saved: { icon: "file-text", tone: "ok" },
  research_stopped: { icon: "circle-stop", tone: "warn" },
  task_started: { icon: "circle-dashed", tone: "" },
  task_finished: { icon: "circle-check", tone: "ok" },
  changes_ready: { icon: "inbox", tone: "" },
  accepted: { icon: "check", tone: "ok" },
  discarded: { icon: "x", tone: "" },
  conflict: { icon: "triangle-alert", tone: "bad" },
  sent_back: { icon: "arrow-left", tone: "warn" },
  lead_merged: { icon: "git-branch", tone: "merge" },
  merged_into_main: { icon: "git-branch", tone: "merge" },
  undone: { icon: "refresh-cw", tone: "warn" },
  work_started: { icon: "zap", tone: "" },
  work_review_ready: { icon: "list-checks", tone: "" },
  work_complete: { icon: "circle-check", tone: "ok" },
  work_closed: { icon: "circle-x", tone: "" },
  phase: { icon: "layers", tone: "" },
  test: { icon: "shield-check", tone: "ok" },
};

/** The project's activity grouped by goal, newest goal first: up to eight rows per goal with
 * when, what, and who, and Show all (N) for a longer goal. Inputs: the events, the time they are
 * read against, the goals shown in full and the Show all action. Output: the groups. */
export function ActivityGroups({ items, now, expanded, onShowAll }: { items: ActivityItem[]; now: number; expanded: string[]; onShowAll: (goal: string) => void }) {
  return <div className="pj-stack-s">{groupActivity(items).map((group) => {
    const all = expanded.includes(group.goal);
    const count = group.items.length;
    return <section className="pj-panel pj-activity-goal" aria-label={group.goal} key={group.goal}>
      <div className="pj-activity-head">
        <strong className="ellipsis">{displayText(group.goal)}</strong>
        <span className="pj-small pj-muted">{count} {count === 1 ? "item" : "items"}, latest {ago(group.items[0].at_ms, now).toLowerCase()}</span>
      </div>
      <ul className="pj-activity-list" aria-label={`${group.goal} activity`}>{(all ? group.items : group.items.slice(0, ACTIVITY_ROWS)).map((item) => <li className="pj-activity-row" key={item.id}>
        <span className="pj-small pj-muted">{ago(item.at_ms, now)}</span>
        <span className={`pj-activity-kind ${KINDS[item.kind].tone}`}><Icon name={KINDS[item.kind].icon} size={14} /></span>
        <span className="pj-activity-text">{displayText(item.text)}{item.undone && <> <Badge word="Undone" tone="neutral" /></>}
          <span className="pj-small pj-muted pj-activity-by">{displayText(item.actor)}</span></span>
      </li>)}</ul>
      {!all && count > ACTIVITY_ROWS && <button type="button" className="pj-link pj-activity-more" onClick={() => onShowAll(group.goal)}>Show all ({count})</button>}
    </section>;
  })}</div>;
}

/** What Recent activity draws: the last good read, when it was read, and why the latest read
 * failed while an older one stays on screen. */
type ActivityRead = { shown: Result<ActivityItem[]> | undefined; at: number; stale: string | null };

/** Reads one project's activity now and again every 5 s while the window is visible. A failed
 * refresh keeps the last good read and says why. Input: the project id. Output: the read. */
function useActivity(projectId: string): ActivityRead {
  const [read, setRead] = useState<ActivityRead>({ shown: undefined, at: 0, stale: null });
  useEffect(() => setRead({ shown: undefined, at: 0, stale: null }), [projectId]);
  useEffect(() => {
    let current = true;
    let timer = 0;
    const tick = async () => {
      if (document.visibilityState === "visible") {
        const result = await loadActivity(projectId, ACTIVITY_READ_MAX);
        if (!current) return;
        const at = Date.now();
        setRead((previous) => result.kind === "ok" || previous.shown?.kind !== "ok"
          ? { shown: result.kind === "ok" ? { kind: "ok", value: result.value.items } : result, at, stale: null }
          : { ...previous, stale: result.kind === "error" ? failureCopy(result) : "Activity isn't in this build." });
        if (result.kind === "not_ready") return;
      }
      timer = window.setTimeout(() => void tick(), ACTIVITY_READ_MS);
    };
    void tick();
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [projectId]);
  return read;
}

export function RecentActivity({ projectId }: { projectId: string; every?: number; read?: unknown }) {
  const read = useActivity(projectId);
  const [expanded, setExpanded] = useState<string[]>([]);
  useEffect(() => setExpanded([]), [projectId]);
  const shown = read.shown;
  return <section className="pj-stack-s" aria-labelledby="pj-activity">
    <div className="pj-row"><h2 id="pj-activity">Recent activity</h2>{shown?.kind === "ok" && !read.stale && <span className="pj-live">Live</span>}</div>
    {shown === undefined && <p className="pj-muted" role="status">Loading activity...</p>}
    {shown?.kind === "not_ready" && <NotReady what="project activity isn't in this build." />}
    {shown?.kind === "error" && <p className="pj-error" role="alert">Couldn't load activity. {failureCopy(shown)}</p>}
    {read.stale && <p className="pj-note warn" role="status">{displayText(`Couldn't refresh activity. ${read.stale} Showing the last read.`)}</p>}
    {shown?.kind === "ok" && (shown.value.length
      ? <ActivityGroups items={shown.value} now={read.at} expanded={expanded} onShowAll={(goal) => setExpanded((open) => [...open, goal])} />
      : <p className="pj-muted">Nothing yet.</p>)}
  </section>;
}

/** Overview tab: what needs the owner, research progress, recent activity and project facts.
 * Input: the project data. Output: the tab. */
export function OverviewTab({ data }: { data: ProjectData }) {
  const setup = valueOf(data.setup);
  const jobs = valueOf(data.research)?.jobs ?? [];
  const harnesses = valueOf(data.harnesses);
  const base = `/projects/${encodeURIComponent(data.id)}`;
  const waiting = jobs.some((job) => job.status === "needs_pool") || (setup !== null && !data.assigned && setup.research.length > 0 && data.pools?.kind === "ok");
  const research = researchJobs(jobs);
  const rows = research.map((job) => ({ job, row: researchRow(job, data.now) }));
  const allDone = research.length > 0 && research.every((job) => job.status === "done");
  const [suggested, setSuggested] = useState(0);
  useEffect(() => {
    let live = true;
    void loadAllProjectMemories(data.id).then((result) => { if (live) setSuggested(result.kind === "ok" ? memoryGroups(result.value.items).suggested.length : 0); });
    return () => { live = false; };
  }, [data.id, data.now]);
  const [retrying, setRetrying] = useState<number | null>(null);
  const [skills] = useLoad(() => listSkills(data.id), [data.id], "skills");
  const [servers] = useLoad(() => listMcpServers(data.id), [data.id], "mcp-servers");
  const [summaries] = useLoad(poolSummaries, [data.id], "pool-summaries");
  const [policy] = useLoad(() => loadAgentPolicy(data.id), [data.id], "agent-policy");
  const assignedSummary = summaries.kind === "ok" ? summaries.value.find((pool) => pool.id === data.assigned?.pool.id) : undefined;
  const signIn = signInLine(assignedSummary);
  const teamWaiting = data.team.result?.kind === "ok" ? teamWaitingLine(data.team.result.value.tasks) : null;
  async function retry(job: ResearchJob) {
    setRetrying(job.id);
    const outcome = changeOutcome(await retryResearch(data.id, job.id, nextGeneration()), `${displayText(job.kind_label)} is queued again.`, "trying research again");
    setRetrying(null);
    data.notify(outcome.toast || outcome.note);
    if (outcome.reload) data.reload();
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
        {data.research?.kind === "error" && <p className="pj-error" role="alert">Couldn't load research. {failureCopy(data.research)}</p>}
        {data.research?.kind === "ok" && research.length === 0 && <p className="pj-muted">No research was picked for this project.</p>}
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
      <PlanningLine data={data} />
      <RecentActivity projectId={data.id} />
    </div>
    <aside className="pj-panel pj-facts" aria-label="Project facts">
      {data.setup?.kind === "not_ready" && <NotReady what="project details aren't in this build." />}
      {data.setup?.kind === "ok" && data.setup.value === null && <p className="pj-muted">This project was created before project setup. It has no folder, research or team details.</p>}
      <dl>
        {setup && <div><dt>Project folder</dt><dd className="pj-mono" title={projectFolder(setup.folder)}>{displayText(folderLabel(projectFolder(setup.folder)))}</dd></div>}
        {setup && <div><dt>Tech stack</dt><dd className="pj-chips">{setup.tech_stack.length ? setup.tech_stack.map((item) => <span className="pj-chip" key={item}>{displayText(item)}</span>) : <span className="pj-muted">None</span>}</dd></div>}
        {setup && <div><dt>Platforms</dt><dd>{setup.platforms.map((platform) => PLATFORM_LABELS[platform] ?? platform).join(", ") || "None"}</dd></div>}
        <div><dt>Pool</dt><dd>{data.assigned
          ? <><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=pool`)}>{displayText(assignedSummary?.name ?? poolName(data.assigned.pool))}</button> <span className="pj-small pj-muted">{assignedSummary ? `${poolSummary(data.assigned.members)}, ${poolReadiness(assignedSummary.members)}` : poolSummary(data.assigned.members)}</span></>
          : data.pools?.kind === "ok" ? <Badge word="Needs a pool" tone="warn" /> : <span className="pj-muted">{data.pools === undefined ? "Loading..." : "Unknown"}</span>}</dd></div>
        {setup && <div><dt>Team</dt><dd>Master: {harnessName(harnesses, setup.team.master)}</dd>
          <dd className="pj-secondary">{setup.team.sub_harnesses.length ? `Agents: ${setup.team.sub_harnesses.map((slot) => `${harnessName(harnesses, slot.harness)} x${slot.agents}`).join(", ")}` : anyHarnessTotal(setup.team)}</dd></div>}
        <div><dt>Agent access</dt><dd><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=access`)}>{policy.kind === "ok" ? accessSummary(policy.value) : policy.kind === "loading" ? "Loading..." : "Project settings"}</button></dd></div>
        {setup && setup.documents.length > 0 && <div><dt>Documents</dt><dd>{setup.documents.length} attached</dd></div>}
        <div><dt>Skills</dt><dd><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=plugins`)}>{onFact(skills, "skill", "skills") ?? "Project settings"}</button></dd></div>
        <div><dt>MCP servers</dt><dd><button type="button" className="pj-link" onClick={() => data.navigate(`${base}/settings?section=plugins`)}>{onFact(servers, "server", "servers") ?? "Project settings"}</button></dd></div>
      </dl>
    </aside>
  </div>;
}
