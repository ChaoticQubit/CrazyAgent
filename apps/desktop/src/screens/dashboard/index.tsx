import { useEffect, useState } from "react";
import { harnessCatalog, listAccounts, poolSummaries, type AccountView, type HarnessEntry, type PoolSummary, type Result } from "../../api";
import { loadResearch, loadTeamStatus, type TeamStatus } from "../../api-projects";
import type { ResearchJob } from "../../engine-client";
import { useAppSession, useLoad, type Loaded, type ProjectRow } from "../../state";
import { EmptyState, Icon, LoadError, NotReady, PageHeader, Panel, Skeleton, formatRoute, type IconName } from "../../ui";

export interface SetupItem {
  id: "accounts" | "pools" | "harnesses";
  icon: IconName;
  title: string;
  body: string;
  action: string;
  href: string;
  done: boolean;
}

const READY_ACCOUNT = new Set(["signed_in", "key_saved"]);
const LIVE_RESEARCH = new Set(["queued", "needs_pool", "running"]);
const LIVE_AGENT = new Set(["starting", "working"]);
const LIVE_TASK = new Set(["queued", "needs_pool", "running"]);
const ACTIVITY_POLL_MS = 10000;
const ACTIVITY_PROJECTS_MAX = 60;

/** What a project card says about its work: the research line, the working-agents line, whether
 * research waits for a pool, and whether anything is still moving so the dashboard keeps reading. */
export interface CardActivity {
  research: string | null;
  agents: string | null;
  waiting: boolean;
  live: boolean;
}

/** Sums up one project's live work for its card from the engine's team and research reads.
 * Working agents are team agents at work plus running research agents. A read that is missing,
 * not ready or failed adds nothing. Inputs: the two reads, undefined while loading.
 * Output: the card lines, null for a line with nothing to say. */
export function cardActivity(team: Result<Pick<TeamStatus, "agents" | "tasks">> | undefined,
  research: Result<{ jobs: Pick<ResearchJob, "status">[] }> | undefined): CardActivity {
  const crew = team?.kind === "ok" ? team.value : null;
  const jobs = research?.kind === "ok" ? research.value.jobs : null;
  const count = (status: string) => jobs?.filter((job) => job.status === status).length ?? 0;
  const waiting = count("needs_pool") > 0;
  let line: string | null = null;
  if (jobs && jobs.length > 0) {
    line = waiting ? "Research waits for a pool" : `Research ${count("done")} of ${jobs.length} done`;
    if (!waiting && count("failed") > 0) line += `, ${count("failed")} failed`;
    if (!waiting && count("cancelled") > 0) line += `, ${count("cancelled")} stopped`;
  }
  const working = (crew?.agents.filter((agent) => agent.status === "working").length ?? 0) + count("running");
  const agents = crew === null && jobs === null ? null : working === 0 ? "No agents working" : `${working} ${working === 1 ? "agent" : "agents"} working`;
  const live = Boolean(jobs?.some((job) => LIVE_RESEARCH.has(job.status)) || crew?.agents.some((agent) => LIVE_AGENT.has(agent.status)) ||
    crew?.tasks.some((task) => LIVE_TASK.has(task.status)));
  return { research: line, agents, waiting, live };
}

/** Reads each shown project's team and research status, one project at a time, and reads again
 * every few seconds only while some project still has work moving. Input: the project ids.
 * Output: the latest activity per project id. */
export function useProjectActivity(ids: string[]): Map<string, CardActivity> {
  const [activity, setActivity] = useState<Map<string, CardActivity>>(() => new Map());
  const key = ids.slice(0, ACTIVITY_PROJECTS_MAX).join("\n");
  useEffect(() => {
    if (!key) return;
    let live = true;
    let timer: number | undefined;
    const read = async () => {
      const rows: [string, CardActivity][] = [];
      for (const id of key.split("\n")) {
        const [team, research] = await Promise.all([loadTeamStatus(id), loadResearch(id)]);
        if (!live) return;
        rows.push([id, cardActivity(team, research)]);
      }
      setActivity(new Map(rows));
      if (rows.some(([, row]) => row.live)) timer = window.setTimeout(() => void read(), ACTIVITY_POLL_MS);
    };
    void read();
    return () => { live = false; window.clearTimeout(timer); };
  }, [key]);
  return activity;
}

/** Works out the Get set up panel: Add an account is done when one account is signed in or
 * has a saved key, Create a pool when one pool exists; harnesses never block. The panel
 * shows until the first two are done. Inputs: the three engine reads. Output: items and
 * whether the panel shows. */
export function setupSteps(accounts: Loaded<AccountView[]>, pools: Loaded<PoolSummary[]>, harnesses: Loaded<HarnessEntry[]>): { items: SetupItem[]; visible: boolean } {
  const accountsDone = accounts.kind === "ok" && accounts.value.some((account) => READY_ACCOUNT.has(account.status));
  const poolsDone = pools.kind === "ok" && pools.value.length > 0;
  const ready = harnesses.kind === "ok" ? harnesses.value.filter((entry) => entry.status.kind === "ready").length : undefined;
  const items: SetupItem[] = [
    { id: "accounts", icon: "key-round", title: "Add an account", body: "Sign in with ChatGPT or Claude, or add an API key or local model.",
      action: "Add account", href: formatRoute({ name: "settings", tab: "accounts", action: "add" }), done: accountsDone },
    { id: "pools", icon: "layers", title: "Create a pool", body: "Group up to 15 accounts for your projects to use.",
      action: "Create pool", href: formatRoute({ name: "settings", tab: "pools", action: "new" }), done: poolsDone },
    { id: "harnesses", icon: "terminal", title: "Check your harnesses",
      body: ready === undefined || harnesses.kind !== "ok" ? "See which coding agents are installed on this Mac." : `${ready} of ${harnesses.value.length} harnesses ready on this Mac.`,
      action: "View harnesses", href: formatRoute({ name: "settings", tab: "harnesses" }), done: false },
  ];
  return { items, visible: !(accountsDone && poolsDone) };
}

/** Lists accounts that need the owner, for the Needs you strip: at most three rows plus the
 * total. Input: the accounts read. Output: rows and the total count. */
export function needsYou(accounts: Loaded<AccountView[]>): { rows: AccountView[]; total: number } {
  const all = accounts.kind === "ok" ? accounts.value.filter((account) => account.status === "not_signed_in") : [];
  return { rows: all.slice(0, 3), total: all.length };
}

function SetupPanel({ items }: { items: SetupItem[] }) {
  return <Panel title="Get set up" padded={false} className="setup-panel">
    <ol>
      {items.map((item) => <li key={item.id}>
        <span className="row"><Icon name={item.icon} /><strong className="grow">{item.title}</strong>
          {item.done && <span className="done"><Icon name="circle-check" size={14} />Done</span>}</span>
        <p className="secondary">{item.body}</p>
        {!item.done && <span><a className={`btn${item.id === "harnesses" ? " quiet" : ""} small`} href={item.href}>{item.action}</a></span>}
      </li>)}
    </ol>
  </Panel>;
}

function ProjectCard({ project, activity }: { project: ProjectRow; activity: CardActivity | undefined }) {
  return <a className="project-card" href={formatRoute({ name: "project", id: project.id, tab: "overview" })} aria-label={project.name}>
    <span className="title-row"><Icon name="folder" /><h3 className="ellipsis">{project.name}</h3></span>
    {activity?.research && <span className={`activity${activity.waiting ? " warn" : ""}`}><Icon name="search" size={14} />{activity.research}</span>}
    {activity?.agents && <span className="activity"><Icon name="bot" size={14} />{activity.agents}</span>}
    <span className="meta"><span>Open project</span><Icon name="chevron-right" size={14} /></span>
  </a>;
}

function Projects({ projects, activity, onRetry }: { projects: Loaded<ProjectRow[]>; activity: Map<string, CardActivity> | undefined; onRetry: () => void }) {
  if (projects.kind === "loading") return <Skeleton shape="card" count={6} />;
  if (projects.kind === "not_ready") return <NotReady what="Projects" compact />;
  if (projects.kind === "error") return <LoadError message="Couldn't load your projects." onRetry={onRetry} />;
  if (projects.value.length === 0) return <EmptyState icon="folder-plus" title="No projects yet"
    action={<a className="btn" href={formatRoute({ name: "new-project", step: "basics" })}><Icon name="plus" size={15} />New project</a>}>
    Create a project and agents start the research you pick.
  </EmptyState>;
  return <div className="cards">{projects.value.map((project) => <ProjectCard key={project.id} project={project} activity={activity?.get(project.id)} />)}</div>;
}

/** Home as a pure view of its engine reads: the Needs you strip, project cards with their live
 * work or the empty state, and the Get set up panel while setup is incomplete. Inputs: the four
 * reads, each card's activity when read, and a retry for the project list. */
export function HomeView({ projects, accounts, pools, harnesses, activity, onRetryProjects }: {
  projects: Loaded<ProjectRow[]>;
  onRetryProjects: () => void;
  accounts: Loaded<AccountView[]>;
  pools: Loaded<PoolSummary[]>;
  harnesses: Loaded<HarnessEntry[]>;
  activity?: Map<string, CardActivity>;
}) {
  const setup = setupSteps(accounts, pools, harnesses);
  const needs = needsYou(accounts);
  return <>
    <PageHeader title="Projects" actions={<a className="btn primary" href={formatRoute({ name: "new-project", step: "basics" })}><Icon name="plus" size={15} />New project</a>} />
    {needs.total > 0 && <section className="needs" aria-label="Needs you">
      {needs.rows.map((account) => <div className="need" key={account.id}>
        <span className="row"><Icon name="circle-alert" /><span>{account.name} isn't signed in.</span></span>
        <a className="btn small" href={formatRoute({ name: "settings", tab: "accounts" })}>Sign in again</a>
      </div>)}
      {needs.total > 3 && <a className="btn quiet small" href={formatRoute({ name: "settings", tab: "accounts" })}>Show all ({needs.total})</a>}
    </section>}
    <div className={`home${setup.visible ? " with-setup" : ""}`}>
      <Projects projects={projects} activity={activity} onRetry={onRetryProjects} />
      {setup.visible && <SetupPanel items={setup.items} />}
    </div>
  </>;
}

/** Home: loads accounts, pools and harnesses for the setup panel and each project's live work,
 * and renders HomeView with the session's projects. */
export function Home() {
  const session = useAppSession();
  const [accounts] = useLoad(listAccounts);
  const [pools] = useLoad(poolSummaries);
  const [harnesses] = useLoad(harnessCatalog);
  const activity = useProjectActivity(session.projects.kind === "ok" ? session.projects.value.map((project) => project.id) : []);
  return <HomeView projects={session.projects} accounts={accounts} pools={pools} harnesses={harnesses} activity={activity}
    onRetryProjects={() => void session.refreshProjects()} />;
}
