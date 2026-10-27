import { harnessCatalog, nextGeneration, type Result } from "../../api";
import { useContext, useEffect, useState } from "react";
import { cancelTeamTask, failureCopy, loadContext, loadPools, loadResearch, loadSetup, receiptState, revealProjectFolder } from "../../api-projects";
import { SessionContext, useLoad, type Loaded } from "../../state";
import { ConfirmDialog, Menu, PageHeader, Tabs, formatRoute, shortcutKey, type IconName } from "../../ui";
import { displayText } from "../conversation";
import { ChangesTab } from "../changes";
import { AgentsTab } from "./agents";
import { ContextTab } from "./context";
import { MasterArea } from "./live-master";
import { go, takeFlash } from "./flash";
import { OverviewTab } from "./overview";
import { ProcessEditor } from "../process";
import { pauseSummary, shouldPoll } from "./research";
import { valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { SettingsTab } from "./settings";
import { TeamActivity } from "./team-activity";
import { useTeam } from "./team";
import "./projects.css";

export const TABS = [
  { id: "overview", label: "Overview", icon: "house" },
  { id: "conversation", label: "Conversation", icon: "message-square" },
  { id: "agents", label: "Agents", icon: "bot" },
  { id: "changes", label: "Changes", icon: "git-branch" },
  { id: "context", label: "Context", icon: "folder-open" },
  { id: "process", label: "Process", icon: "list-checks" },
  { id: "settings", label: "Settings", icon: "settings" },
] as const satisfies readonly { id: string; label: string; icon: IconName }[];

export type TabId = typeof TABS[number]["id"];

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

function ProjectMenu({ data }: { data: ProjectData }) {
  const [pausing, setPausing] = useState(false);
  const [busy, setBusy] = useState(false);
  const team = data.team.result;
  const summary = pauseSummary(team?.kind === "ok" ? team.value.tasks : []);
  const researching = (valueOf(data.research)?.jobs ?? []).some((job) => job.status === "running");

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
      { label: "Show folder in Finder", icon: "folder-open", onSelect: () => void reveal() },
      { label: "Pause all agents", icon: "circle-pause", onSelect: askPause },
      { label: "Remove from CrazyAgent", icon: "trash", danger: true, onSelect: () => data.navigate(`/projects/${encodeURIComponent(data.id)}/settings?section=remove`) },
    ]} />
    <ConfirmDialog open={pausing} title="Pause all agents?" confirmLabel="Pause all agents" cancelLabel="Keep working" pending={busy}
      onCancel={() => setPausing(false)} onConfirm={() => void pause()}>
      {summary.line} Work so far stays in each agent's copy of the project. Stopped tasks don't start again on their own; ask the master when you want to continue.
      {researching ? " Research agents keep running." : ""}
    </ConfirmDialog>
  </>;
}

function hashSection(): string | undefined {
  if (typeof window === "undefined") return undefined;
  return new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("section") ?? undefined;
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
  const active: TabId = TABS.some((item) => item.id === tab) ? tab as TabId : "overview";
  const setup = valueOf(data.setup);
  const settingsSection = section ?? hashSection();
  const select = (next: TabId) => navigate(`/projects/${encodeURIComponent(projectId)}/${next}`);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!shortcutKey(event)) return;
      const position = Number(event.key) - 1;
      if (Number.isInteger(position) && position >= 0 && position < TABS.length) {
        event.preventDefault();
        select(TABS[position].id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [projectId]);

  return <div className="pj pj-project">
    <PageHeader title={data.name ? displayText(data.name) : <span className="skeleton line pj-title-skeleton" role="status" aria-label="Loading project name" />}
      description={setup?.about ? <span className="pj-truncate" title={setup.about}>{displayText(setup.about.split("\n")[0])}</span> : undefined}
      actions={<ProjectMenu data={data} />} />
    <Tabs label="Project" current={active}
      tabs={TABS.map((item) => ({ id: item.id, label: item.label, icon: item.icon, href: formatRoute({ name: "project", id: projectId, tab: item.id }) }))} />
    <div className="pj-tab-panel" id="pj-tab-panel" role="tabpanel" aria-labelledby={`tab-${active}`}>
      {active === "overview" && <OverviewTab data={data} />}
      {active === "conversation" && <TeamActivity data={data}><MasterArea data={data} /></TeamActivity>}
      {active === "agents" && <AgentsTab data={data} />}
      {active === "changes" && <ChangesTab data={data} />}
      {active === "context" && <ContextTab data={data} />}
      {active === "process" && <ProcessEditor scope={{ kind: "project", id: data.id, name: data.name }} />}
      {active === "settings" && <SettingsTab data={data} section={settingsSection} />}
    </div>
    {!session && <div className="pj-toast-region" aria-live="polite">{toast && <p className="toast" role="status">{displayText(toast)}</p>}</div>}
  </div>;
}

