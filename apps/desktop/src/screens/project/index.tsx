import { harnessCatalog, nextGeneration, type Result } from "../../api";
import { useContext, useEffect, useRef, useState } from "react";
import { cancelTeamTask, failureCopy, loadAgentPolicy, loadContext, loadPools, loadResearch, loadSetup, receiptState, revealProjectFolder } from "../../api-projects";
import { loadUncommittedImport } from "../../api-process";
import { loadQuestionCounts, pendingByProject } from "../../api-questions";
import { SessionContext, useLoad, type Loaded } from "../../state";
import { ConfirmDialog, Menu, formatRoute, shortcutKey, type IconName } from "../../ui";
import { displayText } from "../conversation";
import { HEARTBEAT_MS, closeIde, ideFetching, ideReplacesTabs, ideView, touchIde } from "../../api-ide";
import type { IdeView } from "../../api-ide";
import { ChangesTab } from "../changes";
import { MetricsView } from "../metrics/view";
import { WaitingChanges } from "../changes/waiting";
import { AgentsTab } from "./agents";
import { ContextTab } from "./context";
import { IdeExtensionConfirm } from "./ide-extensions";
import { IDE_POLL_MS, IdeTab, IdeUnavailable } from "./ide";
import { MasterArea } from "./live-master";
import { go, takeFlash } from "./flash";
import { ProjectHead, useProjectHead, type HeadMeta } from "./head";
import { accessSummary } from "./access";
import { poolName, poolSummary } from "../new-project/wizard";
import { IdeConfirm } from "./ide-confirm";
import { OverviewTab, folderLabel, projectFolder } from "./overview";
import { ImportMoveBanner, InstructionImportPrompt, ProcessEditor } from "../process";
import { QuestionsTab } from "./questions";
import { pauseSummary, shouldPoll } from "./research";
import { RecoveryBanner } from "./recovery-banner";
import { routeSection, valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { SettingsTab } from "./settings";
import { TeamActivity } from "./team-activity";
import { useTeam } from "./team";
import { useStopAll } from "../team/controls";
import "./projects.css";

export const TABS = [
  { id: "overview", label: "Overview", icon: "house" },
  { id: "conversation", label: "Conversation", icon: "message-square" },
  { id: "agents", label: "Agents", icon: "bot" },
  { id: "changes", label: "Changes", icon: "git-branch" },
  { id: "questions", label: "Questions", icon: "inbox" },
  { id: "context", label: "Context", icon: "folder-open" },
  { id: "process", label: "Process", icon: "list-checks" },
  { id: "metrics", label: "Metrics", icon: "trending-up" },
  { id: "settings", label: "Settings", icon: "settings" },
] as const satisfies readonly { id: string; label: string; icon: IconName }[];

const IDE_TAB = { id: "ide", label: "IDE", icon: "file-code" } as const satisfies { id: string; label: string; icon: IconName };

export type TabId = typeof TABS[number]["id"] | typeof IDE_TAB.id;

const REPLACED: readonly TabId[] = ["changes", "context"];

/** The project page's tabs: the fixed ones, with the IDE in place of Changes and Context
 * once the engine's IDE can open. A replaced tab the route opens stays listed after the IDE.
 * Inputs: the IDE view, undefined until it is read, and the route's tab. Output: the tabs
 * in order. */
export function projectTabs(view: IdeView | undefined, route = ""): readonly { id: TabId; label: string; icon: IconName }[] {
  if (!ideReplacesTabs(view)) return TABS;
  return TABS.flatMap<{ id: TabId; label: string; icon: IconName }>((item) => {
    const kept = !REPLACED.includes(item.id) || item.id === route ? [item] : [];
    return item.id === "changes" ? [IDE_TAB, ...kept] : kept;
  });
}

/** The tabs the IDE replaced, which the project menu still opens, since the IDE has not every
 * action they have. Input: the IDE view, undefined until read. Output: the tabs, or none. */
export function replacedTabs(view: IdeView | undefined): readonly { id: TabId; label: string; icon: IconName }[] {
  return ideReplacesTabs(view) ? TABS.filter((item) => REPLACED.includes(item.id)) : [];
}

/** The tab a route shows: itself when listed, the IDE route to Changes where the IDE can't
 * open, anything else to Overview. Inputs: the route's tab and the listed tabs. Output: the
 * tab to show. */
export function routedTab(tab: string, tabs: readonly { id: TabId }[]): TabId {
  const listed = (id: string) => tabs.some((item) => item.id === id);
  if (listed(tab)) return tab as TabId;
  if (tab === "ide" && listed("changes")) return "changes";
  return "overview";
}

function shown<T>(read: Loaded<T>): Result<T> | undefined {
  return read.kind === "loading" ? undefined : read;
}

function useProject(id: string, navigate: (path: string) => void, known = "") {
  const [setup, reloadSetup] = useLoad(() => loadSetup(id), [id], "project-setup");
  const [research, reloadResearch] = useLoad(() => loadResearch(id), [id], "project-research");
  const [context, reloadContext] = useLoad(() => loadContext(id), [id], "project-context");
  const [pools, reloadPools] = useLoad(loadPools, [], "project-pools");
  const [harnesses] = useLoad(harnessCatalog, [], "harness-catalog");
  const [now, setNow] = useState(() => Date.now());
  const [toast, setToast] = useState<string | null>(null);
  const session = useContext(SessionContext);
  const notify = (message: string) => { if (session) session.showToast(message); else setToast(message); };
  const [team, reloadTeam] = useTeam(id);
  const listed = session?.projects.kind === "ok" ? session.projects.value.find((item) => item.id === id)?.name : undefined;
  const name = (setup.kind === "ok" && setup.value ? setup.value.name : "") || known || listed || "";

  useEffect(() => {
    const message = takeFlash();
    if (message) notify(message);
  }, [id]);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 4000);
    return () => window.clearTimeout(timer);
  }, [toast]);

  const jobs = research.kind === "ok" ? research.value.jobs : [];
  const polling = shouldPoll(jobs);
  useEffect(() => {
    if (!polling) return;
    const timer = window.setInterval(() => {
      setNow(Date.now());
      reloadResearch();
      reloadContext();
    }, 3000);
    return () => window.clearInterval(timer);
  }, [id, polling]);

  const assigned = pools.kind === "ok" ? pools.value.find((choice) => choice.pool.projects.includes(id)) ?? null : null;
  const data: ProjectData = {
    id, name, setup: shown(setup), research: shown(research), context: shown(context), pools: shown(pools), harnesses: shown(harnesses),
    assigned, now, navigate, notify,
    reload: () => { setNow(Date.now()); reloadSetup(); reloadResearch(); reloadContext(); },
    reloadPools,
    team,
    reloadTeam,
  };
  return { data, toast, session };
}

function ProjectMenu({ data, replaced }: { data: ProjectData; replaced: readonly { id: TabId; label: string; icon: IconName }[] }) {
  const [pausing, setPausing] = useState(false);
  const [busy, setBusy] = useState(false);
  const team = data.team.result;
  const summary = pauseSummary(team?.kind === "ok" ? team.value.tasks : []);
  const researching = (valueOf(data.research)?.jobs ?? []).some((job) => job.status === "running");
  const stopped = team?.kind === "ok" && team.value.paused;
  const stopAll = useStopAll(data.id, data.reloadTeam, (note, stopping) => data.notify(note
    || (stopping ? "Stopped. Every agent of this project stays stopped until you start them again." : "Started again. The agents of this project run again.")));

  async function reveal() {
    const result = await revealProjectFolder(data.id);
    if (result.kind !== "ok") data.notify(result.kind === "not_ready" ? "Not ready yet: Show folder in Finder isn't in this build." : "Couldn't open the folder.");
  }
  function askPause() {
    if (team === undefined) return data.notify("Still loading the team. Try again in a moment.");
    if (team.kind === "not_ready") return data.notify("Not ready yet: pausing agents isn't in this build.");
    if (team.kind === "error") return data.notify(`Couldn't load the team. ${failureCopy(team)}`);
    if (summary.count === 0) return data.notify(summary.line);
    setPausing(true);
  }
  async function pause() {
    setBusy(true);
    const result = await cancelTeamTask(data.id, null, nextGeneration());
    setBusy(false);
    setPausing(false);
    data.reloadTeam();
    if (result.kind !== "ok") return data.notify(result.kind === "not_ready" ? "Not ready yet: pausing agents isn't in this build." : failureCopy(result));
    const state = receiptState(result.value);
    if (state.kind === "applied") return data.notify("Paused. Running tasks are stopping and waiting tasks are cancelled.");
    data.notify(state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check the Agents tab before trying again.");
  }
  return <>
    <Menu label="Project actions" items={[
      ...replaced.map((item) => ({ label: `Open ${item.label}`, icon: item.icon, onSelect: () => data.navigate(`/projects/${encodeURIComponent(data.id)}/${item.id}`) })),
      { label: "Show folder in Finder", icon: "folder-open", onSelect: () => void reveal() },
      { label: "Pause all agents", icon: "circle-pause", onSelect: askPause },
      stopped ? { label: "Start all again", icon: "refresh-cw", onSelect: stopAll.start } : { label: "Stop all", icon: "circle-stop", danger: true, onSelect: stopAll.ask },
      { label: "Remove from CrazyAgent", icon: "trash", danger: true, onSelect: () => data.navigate(`/projects/${encodeURIComponent(data.id)}/settings?section=remove&ask=remove`) },
    ]} />
    <ConfirmDialog open={pausing} title="Pause all agents?" confirmLabel="Pause all agents" cancelLabel="Keep working" pending={busy}
      onCancel={() => setPausing(false)} onConfirm={() => void pause()}>
      {summary.line} Work so far stays in each agent's copy of the project. Stopped tasks don't start again on their own; ask the master when you want to continue.
      {researching ? " Research agents keep running." : ""}
    </ConfirmDialog>
    {stopAll.dialog}
  </>;
}

/** The head's meta line for a project: its folder, the checked-out branch, the agents' access
 * and the pool with its size, each null until read. Inputs: the project data, the access
 * policy read and the checkout read. Output: the meta. */
function headMeta(data: ProjectData, access: string | null, branch: string | null): HeadMeta {
  const setup = valueOf(data.setup);
  return {
    folder: setup ? folderLabel(projectFolder(setup.folder)) : null,
    branch,
    access,
    pool: data.assigned ? `${poolName(data.assigned.pool)}, ${poolSummary(data.assigned.members)}` : null,
  };
}

/** How many questions of one project wait for the owner, read again with each team read.
 * Inputs: the project id and the time of the latest team read. Output: the count, 0 until read. */
function usePendingQuestions(projectId: string, at: number): number {
  const [count, setCount] = useState(0);
  useEffect(() => {
    let live = true;
    void loadQuestionCounts().then((result) => {
      if (live && result.kind === "ok") setCount(pendingByProject(result.value).get(projectId) ?? 0);
    });
    return () => { live = false; };
  }, [projectId, at]);
  return count;
}

function hashSection(): string | undefined {
  if (typeof window === "undefined") return undefined;
  return routeSection(window.location.hash);
}

/** The project page: header, tabs and the selected tab, read from the engine.
 * Inputs: the project id, the tab from the route, the name when the shell knows it, an
 * optional settings section and a navigate handler.
 * Output: the page; research progress polls while agents are queued or running. */
export function ProjectPage({ id: projectId, tab = "overview", name, section, navigate = go }: {
  id: string;
  tab?: string;
  name?: string;
  section?: string;
  navigate?: (path: string) => void;
}) {
  const { data, toast, session } = useProject(projectId, navigate, name);
  const [ide, setIde] = useState<Result<IdeView>>();
  const [busy, setBusy] = useState(false);
  const view = ide?.kind === "ok" ? ide.value : undefined;
  const failure = ide !== undefined && ide.kind !== "ok" ? ide : undefined;
  const fetching = ideFetching(view);
  const tabs = projectTabs(view, tab);
  const active = routedTab(tab, tabs);
  const setup = valueOf(data.setup);
  const settingsSection = section ?? hashSection();
  const [importMoves, setImportMoves] = useState(0);
  const select = (next: TabId) => navigate(`/projects/${encodeURIComponent(projectId)}/${next}`);

  useEffect(() => () => void closeIde(projectId), [projectId]);

  useEffect(() => {
    let live = true;
    const read = () => void ideView().then((result) => { if (live) setIde(result); });
    read();
    const timer = fetching ? window.setInterval(read, IDE_POLL_MS) : 0;
    return () => { live = false; window.clearInterval(timer); };
  }, [projectId, session?.revision, fetching]);

  async function readIde() {
    setBusy(true);
    setIde(await ideView());
    setBusy(false);
  }

  async function fetchIde() {
    setBusy(true);
    const result = await ideView(true);
    setBusy(false);
    if (result.kind === "ok") setIde(result);
    else data.notify(result.kind === "error" ? `The IDE couldn't be fetched. ${failureCopy(result)}` : "CrazyAgent's engine isn't ready yet. Try again in a moment.");
  }

  const replaces = ideReplacesTabs(view);
  useEffect(() => {
    if (!replaces) return;
    const beat = window.setInterval(() => void touchIde(projectId), HEARTBEAT_MS);
    return () => window.clearInterval(beat);
  }, [projectId, replaces]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!shortcutKey(event)) return;
      const position = Number(event.key) - 1;
      if (Number.isInteger(position) && position >= 0 && position < tabs.length) {
        event.preventDefault();
        select(tabs[position].id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [projectId, tabs.map((item) => item.id).join(" ")]);

  const root = useRef<HTMLDivElement>(null);
  const scrolled = useProjectHead(root, projectId);
  const [policy] = useLoad(() => loadAgentPolicy(projectId), [projectId], "agent-policy");
  const [checkout] = useLoad(() => loadUncommittedImport(projectId), [projectId, data.team.at]);
  const questions = usePendingQuestions(projectId, data.team.at);
  const team = data.team.result?.kind === "ok" ? data.team.result.value : null;
  const working = team ? team.agents.filter((agent) => agent.status === "working").length : 0;
  const meta = headMeta(data, policy.kind === "ok" ? accessSummary(policy.value) : null, checkout.kind === "ok" ? checkout.value.checked_out : null);

  return <div className="pj pj-project" ref={root}>
    <ProjectHead title={data.name ? displayText(data.name) : <span className="skeleton line pj-title-skeleton" role="status" aria-label="Loading project name" />}
      about={setup?.about ?? ""} meta={meta} working={working} questions={questions} actions={<ProjectMenu data={data} replaced={replacedTabs(view)} />} current={active} scrolled={scrolled}
      tabs={tabs.map((item) => ({ id: item.id, label: item.label, icon: item.icon, href: formatRoute({ name: "project", id: projectId, tab: item.id }) }))} />
    <RecoveryBanner key={projectId} project={projectId} />
    <IdeUnavailable view={view} failure={failure} busy={busy} onAction={() => void (failure ? readIde() : fetchIde())} />
    {active !== "changes" && <WaitingChanges projectId={projectId} />}
    <ImportMoveBanner projectId={projectId} version={importMoves} />
    <InstructionImportPrompt projectId={projectId} name={data.name ? displayText(data.name) : projectId} onMoved={() => setImportMoves((count) => count + 1)} />
    <div className="pj-tab-panel" id="pj-tab-panel" role="tabpanel" aria-labelledby={`tab-${active}`}>
      {active === "overview" && <OverviewTab data={data} />}
      {active === "conversation" && <TeamActivity data={data}><MasterArea data={data} /></TeamActivity>}
      {active === "agents" && <AgentsTab data={data} section={settingsSection} />}
      {active === "changes" && <ChangesTab data={data} />}
      {active === "questions" && <QuestionsTab data={data} />}
      {active === "context" && <ContextTab data={data} />}
      {active === "process" && <ProcessEditor key={data.id} scope={{ kind: "project", id: data.id, name: data.name }} />}
      {active === "metrics" && <MetricsView key={data.id} projectId={data.id} />}
      {active === "settings" && <SettingsTab data={data} section={settingsSection} />}
      {active === "ide" && <IdeTab key={data.id} projectId={projectId} onView={(value) => setIde({ kind: "ok", value })} />}
    </div>
    {replaces && <IdeConfirm data={data} />}
    {replaces && <IdeExtensionConfirm data={data} />}
    {!session && <div className="pj-toast-region" aria-live="polite">{toast && <p className="toast" role="status">{displayText(toast)}</p>}</div>}
  </div>;
}

