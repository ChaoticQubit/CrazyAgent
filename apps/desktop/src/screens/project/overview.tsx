import { useEffect, useState } from "react";
import { Icon, Segmented, type IconName } from "../../ui";
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
import { ACTIVITY_FILTERS, ACTIVITY_ROWS, RESEARCH_KEY, activityFilter, activityGoal, ago, changeOutcome, groupActivity, memoryGroups, newestActivity, type ActivityFilter, researchJobs, researchRow, onFact, signInLine, suggestedLine, teamWaitingLine, waitingLine } from "./research";
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

/** One activity row: when, the kind's icon, what happened, Undone when it was, and who, with
 * the goal when the row stands outside its group. Inputs: the event, the time it is read
 * against and the goal to tag it with. Output: the row. */
function ActivityRow({ item, now, goal }: { item: ActivityItem; now: number; goal?: string }) {
  return <li className="pj-activity-row">
    <span className="pj-small pj-muted">{ago(item.at_ms, now)}</span>
    <span className={`pj-activity-kind ${KINDS[item.kind].tone}`}><Icon name={KINDS[item.kind].icon} size={14} /></span>
    <span className="pj-activity-text">{displayText(item.text)}{item.undone && <> <Badge word="Undone" tone="neutral" /></>}
      <span className="pj-small pj-muted pj-activity-by">{displayText(item.actor)}{goal !== undefined && <> · <span className="pj-activity-tag">{displayText(goal)}</span></>}</span></span>
  </li>;
}

/** The project's activity grouped by goal, newest goal first: a header that closes and opens the
 * goal, up to eight rows per open goal with when, what, and who, and Show all (N) for a longer
 * goal. A read the engine cut counts with a plus. Inputs: the events, whether older ones were
 * left out, the time they are read against, the group keys shown in full, the closed group keys
 * and the Show all and close or open actions. Output: the groups. */
export function ActivityGroups({ items, more, now, expanded, closed, onShowAll, onToggle }: {
  items: ActivityItem[]; more: boolean; now: number; expanded: string[]; closed: string[]; onShowAll: (key: string) => void; onToggle: (key: string) => void;
}) {
  return <div className="pj-stack-s">{groupActivity(items).map((group) => {
    const open = !closed.includes(group.key);
    const all = expanded.includes(group.key);
    const count = `${group.items.length}${more ? "+" : ""}`;
    return <section className="pj-panel pj-activity-goal" aria-label={group.goal} key={group.key}>
      <button type="button" className="pj-activity-head" aria-expanded={open} onClick={() => onToggle(group.key)}>
        <span className="pj-activity-name"><Icon name={open ? "chevron-down" : "chevron-right"} size={16} /><strong className="ellipsis">{displayText(group.goal)}</strong></span>
        <span className="pj-small pj-muted">{count} {count === "1" ? "item" : "items"}, latest {ago(group.items[0].at_ms, now).toLowerCase()}</span>
      </button>
      {open && <ul className="pj-activity-list" aria-label={`${group.goal} activity`}>{(all ? group.items : group.items.slice(0, ACTIVITY_ROWS)).map((item) => <ActivityRow key={item.id} item={item} now={now} />)}</ul>}
      {open && !all && group.items.length > ACTIVITY_ROWS && <button type="button" className="pj-link pj-activity-more" onClick={() => onShowAll(group.key)}>Show all ({count})</button>}
    </section>;
  })}</div>;
}

/** The project's activity as one list, newest first, each row tagged with its goal. Inputs: the
 * events and the time they are read against. Output: the list. */
export function ActivityTimeline({ items, now }: { items: ActivityItem[]; now: number }) {
  return <section className="pj-panel pj-activity-timeline" aria-label="Activity timeline">
    <ul className="pj-activity-list">{newestActivity(items).map((item) => <ActivityRow key={item.id} item={item} now={now} goal={activityGoal(item).goal} />)}</ul>
  </section>;
}

const VIEWS: { value: "goal" | "time"; label: string }[] = [{ value: "goal", label: "By goal" }, { value: "time", label: "Timeline" }];

/** What Recent activity draws: the last good read, whether the engine left older events out of
 * it, when it was read, and why the latest read failed while an older one stays on screen. */
type ActivityRead = { shown: Result<ActivityItem[]> | undefined; more: boolean; at: number; stale: string | null };

const UNREAD: ActivityRead = { shown: undefined, more: false, at: 0, stale: null };

/** Reads one project's activity now and again each interval while the window is visible, one
 * read at a time. A failed refresh keeps the last good read and says why. Inputs: the project
 * id, the interval and the read. Output: the read. */
function useActivity(projectId: string, every: number, load: typeof loadActivity): ActivityRead {
  const [read, setRead] = useState<ActivityRead>(UNREAD);
  useEffect(() => setRead(UNREAD), [projectId]);
  useEffect(() => {
    let current = true;
    let timer = 0;
    const tick = async () => {
      if (document.visibilityState === "visible") {
        const result = await load(projectId, ACTIVITY_READ_MAX);
        if (!current) return;
        const at = Date.now();
        setRead((previous) => result.kind === "ok" || previous.shown?.kind !== "ok"
          ? { shown: result.kind === "ok" ? { kind: "ok", value: result.value.items } : result, more: result.kind === "ok" && result.value.more === true, at, stale: null }
          : { ...previous, stale: result.kind === "error" ? failureCopy(result) : "Activity isn't in this build." });
        if (result.kind === "not_ready") return;
      }
      timer = window.setTimeout(() => void tick(), every);
    };
    void tick();
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [projectId, every, load]);
  return read;
}

/** Recent activity on Overview: the project's events by goal, read again while the window is
 * visible, with loading, failure and stale states. Inputs: the project id, and for tests the
 * interval and the read. Output: the section. */
export function RecentActivity({ projectId, every = ACTIVITY_READ_MS, read: load = loadActivity }: { projectId: string; every?: number; read?: typeof loadActivity }) {
  const read = useActivity(projectId, every, load);
  const [expanded, setExpanded] = useState<string[]>([]);
  const [closed, setClosed] = useState<string[]>([RESEARCH_KEY]);
  const [view, setView] = useState<"goal" | "time">("goal");
  const [filter, setFilter] = useState<ActivityFilter>("all");
  useEffect(() => {
    setExpanded([]);
    setClosed([RESEARCH_KEY]);
  }, [projectId]);
  const shown = read.shown;
  const items = shown?.kind === "ok" ? shown.value : [];
  const visible = filter === "all" ? items : items.filter((item) => activityFilter(item.kind) === filter);
  const chip = ACTIVITY_FILTERS.find((option) => option.value === filter)?.label ?? "";
  return <section className="pj-stack-s" aria-labelledby="pj-activity">
    <div className="pj-row between">
      <div className="pj-row"><h2 id="pj-activity">Recent activity</h2>{shown?.kind === "ok" && !read.stale && <span className="pj-live">Live</span>}</div>
      {items.length > 0 && <Segmented label="Show activity" value={view} options={VIEWS} onValue={setView} />}
    </div>
    {shown === undefined && <p className="pj-muted" role="status">Loading activity...</p>}
    {shown?.kind === "not_ready" && <NotReady what="project activity isn't in this build." />}
    {shown?.kind === "error" && <p className="pj-error" role="alert">Couldn't load activity. {failureCopy(shown)}</p>}
    {read.stale && <p className="pj-note warn" role="status">{displayText(`Couldn't refresh activity. ${read.stale} Showing the last read.`)}</p>}
    {shown?.kind === "ok" && items.length === 0 && <p className="pj-muted">Nothing yet.</p>}
    {items.length > 0 && <div className="pj-row" role="group" aria-label="Show only">{ACTIVITY_FILTERS.map((option) => <button type="button" key={option.value}
      className={`pj-chip-toggle${filter === option.value ? " on" : ""}`} aria-pressed={filter === option.value} onClick={() => setFilter(option.value)}>{option.label}</button>)}</div>}
    {items.length > 0 && visible.length === 0 && <p className="pj-muted">Nothing under {chip} yet.</p>}
    {visible.length > 0 && (view === "goal"
      ? <ActivityGroups items={visible} more={read.more} now={read.at} expanded={expanded} closed={closed}
        onShowAll={(key) => setExpanded((open) => [...open, key])}
        onToggle={(key) => setClosed((keys) => keys.includes(key) ? keys.filter((other) => other !== key) : [...keys, key])} />
      : <ActivityTimeline items={visible} now={read.at} />)}
    {shown?.kind === "ok" && read.more && <p className="pj-small pj-muted">Older activity isn't shown here.</p>}
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
