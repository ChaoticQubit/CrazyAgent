import { useEffect, useState, type ReactNode } from "react";
import { PROJECT_LIST_MAX, harnessCatalog, listAccounts, memorySuggestions, poolSummaries, type AccountView, type HarnessEntry, type PoolSummary, type Result, type Suggestion } from "../../api";
import { loadSetupStatus } from "../../api-intake";
import { loadResearch, loadTeamStatus, type TeamStatus } from "../../api-projects";
import type { ResearchJob, SetupStatus } from "../../engine-client";
import { useAppSession, useLoad, type Loaded, type ProjectRow } from "../../state";
import { Button, EmptyState, Icon, LoadError, NotReady, PageHeader, Panel, Skeleton, formatRoute, type IconName } from "../../ui";
import { displayText } from "../conversation";
import { FoundProjects } from "../intake/found";
import { SETUP_POLL_MS, setupBusy, toolsChecking, toolsStep } from "../intake/intake";
import { waitingLine } from "../project/research";

export interface SetupItem {
  id: "accounts" | "pools" | "harnesses" | "tools";
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
const NEEDS_SHOWN = 3;

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
  research: Result<{ jobs: ResearchJob[] }> | undefined): CardActivity {
  const crew = team?.kind === "ok" ? team.value : null;
  const jobs = research?.kind === "ok" ? research.value.jobs : null;
  const count = (status: string) => jobs?.filter((job) => job.status === status).length ?? 0;
  const waiting = count("needs_pool") > 0;
  let line: string | null = null;
  if (jobs && jobs.length > 0) {
    line = waiting ? waitingLine(jobs) : `Research ${count("done")} of ${jobs.length} done`;
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

/** Says what Add an account needs: which accounts are ready for agents, that none of the owned
 * ones is ready yet with a link to Accounts, or how to add the first one. Inputs: how many
 * accounts the owner has and the ready ones. Output: the step's body, action and link. */
function accountStep(owned: number, usable: AccountView[]): Pick<SetupItem, "body" | "action" | "href"> {
  const accountsPage = formatRoute({ name: "settings", tab: "accounts" });
  const open = (body: string) => ({ body, action: "Open accounts", href: accountsPage });
  if (usable.length === 1) return open(`${usable[0].name} is ready for agents.`);
  if (usable.length > 1) return open(`${usable.length} accounts are ready for agents.`);
  if (owned > 0) return open("No account is ready for agents yet. Sign one in or check its key or URL.");
  return { body: "Sign in with ChatGPT or Claude, or add an API key or local model.", action: "Add account",
    href: formatRoute({ name: "settings", tab: "accounts", action: "add" }) };
}

/** Works out the Get set up panel: Add an account is done when one account is ready for agents
 * (signed in, a saved key, or a reachable URL or local model) and names it, Create a pool when one
 * pool exists, Install tools when tmux, uv and graphify are installed; harnesses never block.
 * Accounts that exist but none ready send the owner to Accounts. The panel shows until those
 * three are done, and never while one of the four reads is still loading, so it cannot flash in
 * and out. Inputs: the four engine reads. Output: items and whether the panel shows. */
export function setupSteps(accounts: Loaded<AccountView[]>, pools: Loaded<PoolSummary[]>, harnesses: Loaded<HarnessEntry[]>,
  tools: Loaded<SetupStatus>): { items: SetupItem[]; visible: boolean } {
  const owned = accounts.kind === "ok" ? accounts.value : [];
  const usable = owned.filter((account) => READY_ACCOUNT.has(account.status));
  const accountsDone = usable.length > 0;
  const poolsDone = pools.kind === "ok" && pools.value.length > 0;
  const ready = harnesses.kind === "ok" ? harnesses.value.filter((entry) => entry.status.kind === "ready").length : undefined;
  const installTools = toolsStep(tools);
  const items: SetupItem[] = [
    { id: "accounts", icon: "key-round", title: "Add an account", ...accountStep(owned.length, usable), done: accountsDone },
    { id: "pools", icon: "layers", title: "Create a pool", body: "Group up to 15 accounts for your projects to use.",
      action: "Create pool", href: formatRoute({ name: "settings", tab: "pools", action: "new" }), done: poolsDone },
    { id: "harnesses", icon: "terminal", title: "Check your harnesses",
      body: ready === undefined || harnesses.kind !== "ok" ? "See which coding agents are installed on this Mac." : `${ready} of ${harnesses.value.length} harnesses ready on this Mac.`,
      action: "View harnesses", href: formatRoute({ name: "settings", tab: "harnesses" }), done: false },
    { id: "tools", icon: "wrench", title: "Install tools", ...installTools,
      action: "Set up tools", href: formatRoute({ name: "settings", tab: "pack" }) },
  ];
  const settled = [accounts, pools, harnesses, tools].every((read) => read.kind !== "loading");
  return { items, visible: settled && !(accountsDone && poolsDone && installTools.done) };
}

/** One Needs you row: the project it is about (none for an account), one sentence and one link. */
export interface NeedRow {
  key: string;
  icon: IconName;
  project: string | null;
  text: string;
  action: string;
  href: string;
}

/** Lists what needs the owner across projects, for the Needs you strip: projects whose team
 * left unseen notifications (one row each, opening its conversation), accounts that are not
 * signed in (one row each, with Sign in again opening that account's sign-in; accounts sharing
 * a name and provider fold into one row that opens Accounts), research waiting for a pool, then
 * suggested memories grouped by project. Inputs: the accounts read, the projects, each
 * project's card activity, the suggestions read and each project's unseen notification count.
 * Output: the first three rows, every row and the total. */
export function needsYou(accounts: Loaded<AccountView[]>, projects: ProjectRow[] = [], activity: Map<string, CardActivity> = new Map(),
  suggestions: Loaded<Suggestion[]> = { kind: "loading" }, alerts: Map<string, number> = new Map()): { rows: NeedRow[]; all: NeedRow[]; total: number } {
  const all: NeedRow[] = [];
  for (const project of projects) {
    const unseen = alerts.get(project.id) ?? 0;
    if (unseen <= 0) continue;
    all.push({ key: `alerts-${project.id}`, icon: "inbox", project: project.name,
      text: `${unseen} new team notification${unseen === 1 ? "" : "s"}.`,
      action: "Open conversation", href: formatRoute({ name: "project", id: project.id, tab: "conversation" }) });
  }
  const unsigned = new Map<string, AccountView[]>();
  for (const account of accounts.kind === "ok" ? accounts.value : []) {
    if (account.status !== "not_signed_in") continue;
    const same = `${account.provider}\n${account.name}`;
    unsigned.set(same, [...unsigned.get(same) ?? [], account]);
  }
  for (const [first, ...rest] of unsigned.values()) {
    const row = { key: `account-${first!.id}`, icon: "circle-alert" as const, project: null };
    if (rest.length === 0) {
      all.push({ ...row, text: `${first!.name} isn't signed in.`, action: "Sign in again",
        href: formatRoute({ name: "settings", tab: "accounts", action: "sign_in", account: first!.id }) });
    } else {
      all.push({ ...row, text: `${rest.length + 1} ${first!.provider_name} accounts named ${first!.name} aren't signed in.`,
        action: "Open accounts", href: formatRoute({ name: "settings", tab: "accounts" }) });
    }
  }
  for (const project of projects) {
    const work = activity.get(project.id);
    if (!work?.waiting || !work.research) continue;
    all.push({ key: `pool-${project.id}`, icon: "layers", project: project.name, text: work.research,
      action: "Choose pool", href: `${formatRoute({ name: "project", id: project.id, tab: "settings" })}?section=pool` });
  }
  const memories = new Map<string, { name: string; count: number }>();
  for (const item of suggestions.kind === "ok" ? suggestions.value : []) {
    const seen = memories.get(item.project_id);
    memories.set(item.project_id, { name: item.project_name, count: (seen?.count ?? 0) + 1 });
  }
  for (const [id, { name, count }] of memories) {
    all.push({ key: `memory-${id}`, icon: "brain", project: name, text: `${count} suggested ${count === 1 ? "memory" : "memories"} to review.`,
      action: "Review", href: formatRoute({ name: "settings", tab: "memory" }) });
  }
  return { rows: all.slice(0, NEEDS_SHOWN), all, total: all.length };
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
    <span className="title-row"><Icon name="folder" /><h3 className="ellipsis">{displayText(project.name)}</h3></span>
    {activity?.research && <span className={`activity${activity.waiting ? " warn" : ""}`}><Icon name="search" size={14} />{activity.research}</span>}
    {activity?.agents && <span className="activity"><Icon name="bot" size={14} />{activity.agents}</span>}
    <span className="meta"><span>Open project</span><Icon name="chevron-right" size={14} /></span>
  </a>;
}

function Projects({ projects, more, activity, onRetry }: { projects: Loaded<ProjectRow[]>; more: boolean; activity: Map<string, CardActivity> | undefined; onRetry: () => void }) {
  if (projects.kind === "loading") return <Skeleton shape="card" count={6} />;
  if (projects.kind === "not_ready") return <NotReady what="Projects" compact />;
  if (projects.kind === "error") return <LoadError message="Couldn't load your projects." onRetry={onRetry} />;
  if (projects.value.length === 0) return <EmptyState icon="folder-plus" title="No projects yet"
    action={<a className="btn" href={formatRoute({ name: "new-project", step: "basics" })}><Icon name="plus" size={15} />New project</a>}>
    Create a project and agents start the research you pick.
  </EmptyState>;
  return <>
    <div className="cards">{projects.value.map((project) => <ProjectCard key={project.id} project={project} activity={activity?.get(project.id)} />)}</div>
    {more && <p className="secondary" role="status">Showing the first {PROJECT_LIST_MAX} projects. The rest are not listed here, in the sidebar or in Go to.</p>}
  </>;
}

/** Home as a view of its engine reads: the Add actions (new, clone from GitHub, existing folder),
 * the Needs you strip (three rows, Show all for the rest), the projects folder section, project
 * cards with their live work or the empty state, a note when the list stops at its bound, and
 * the Get set up panel while setup is incomplete. Inputs: the five reads, whether the engine
 * holds more projects than listed, each card's activity and the suggested memories when read,
 * the projects folder section and a retry for the project list. */
export function HomeView({ projects, moreProjects = false, accounts, pools, harnesses, tools, activity, suggestions, alerts, found, onRetryProjects }: {
  projects: Loaded<ProjectRow[]>;
  moreProjects?: boolean;
  onRetryProjects: () => void;
  accounts: Loaded<AccountView[]>;
  pools: Loaded<PoolSummary[]>;
  harnesses: Loaded<HarnessEntry[]>;
  tools: Loaded<SetupStatus>;
  activity?: Map<string, CardActivity>;
  suggestions?: Loaded<Suggestion[]>;
  alerts?: Map<string, number>;
  found?: ReactNode;
}) {
  const setup = setupSteps(accounts, pools, harnesses, tools);
  const needs = needsYou(accounts, projects.kind === "ok" ? projects.value : [], activity, suggestions, alerts);
  const [expanded, setExpanded] = useState(false);
  return <>
    <PageHeader title="Projects" actions={<>
      <a className="btn" href={formatRoute({ name: "add-project", way: "clone" })}><Icon name="git-branch" size={15} />Clone from GitHub</a>
      <a className="btn" href={formatRoute({ name: "add-project", way: "folder" })}><Icon name="folder-input" size={15} />Add folder</a>
      <a className="btn primary" href={formatRoute({ name: "new-project", step: "basics" })}><Icon name="plus" size={15} />New project</a>
    </>} />
    {needs.total > 0 && <section className="needs" aria-label="Needs you">
      {(expanded ? needs.all : needs.rows).map((row) => <div className="need" key={row.key}>
        <span className="row"><Icon name={row.icon} /><span>{row.project && <><strong>{displayText(row.project)}</strong>{" "}</>}{row.text}</span></span>
        <a className="btn small" href={row.href}>{row.action}</a>
      </div>)}
      {needs.total > NEEDS_SHOWN && <Button variant="quiet" size="small" aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>
        {expanded ? "Show fewer" : `Show all (${needs.total})`}</Button>}
    </section>}
    {found}
    <div className={`home${setup.visible ? " with-setup" : ""}`}>
      <Projects projects={projects} more={moreProjects} activity={activity} onRetry={onRetryProjects} />
      {setup.visible && <SetupPanel items={setup.items} />}
    </div>
  </>;
}

/** Reads setup for the Install tools item, and reads it again every few seconds only while a
 * tool is still being checked or setup is installing. Output: the latest setup read. */
export function useToolsStatus(): Loaded<SetupStatus> {
  const [tools, reload] = useLoad(loadSetupStatus, [], "setup-status");
  const moving = tools.kind === "ok" && (toolsChecking(tools.value) || setupBusy(tools.value));
  useEffect(() => {
    if (!moving) return;
    const timer = window.setTimeout(reload, SETUP_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [moving, tools]);
  return tools;
}

/** Home: loads accounts, pools, harnesses and tool setup for the setup panel, each project's live
 * work and the suggested memories for Needs you, and renders HomeView with the session's projects.
 * Input: each project's unseen team notification count, when the app watches them. */
export function Home({ alerts }: { alerts?: Map<string, number> } = {}) {
  const session = useAppSession();
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const [pools] = useLoad(poolSummaries, [], "pool-summaries");
  const [harnesses] = useLoad(harnessCatalog, [], "harness-catalog");
  const tools = useToolsStatus();
  const projects = session.projects.kind === "ok" ? session.projects.value : [];
  const activity = useProjectActivity(projects.map((project) => project.id));
  const [suggestions, setSuggestions] = useState<Loaded<Suggestion[]>>({ kind: "loading" });
  const named = projects.map((project) => `${project.id}\t${project.name}`).join("\n");
  useEffect(() => {
    let live = true;
    void memorySuggestions(projects).then((found) => { if (live) setSuggestions(found); });
    return () => { live = false; };
  }, [named]);
  return <HomeView projects={session.projects} accounts={accounts} pools={pools} harnesses={harnesses} tools={tools} activity={activity} suggestions={suggestions}
    alerts={alerts} moreProjects={session.projectsMore} found={<FoundProjects />} onRetryProjects={() => void session.refreshProjects()} />;
}
