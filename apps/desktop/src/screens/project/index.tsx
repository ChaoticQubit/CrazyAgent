import { harnessCatalog } from "../../api";
import { useContext, useEffect, useRef, useState } from "react";
import { loadContext, loadPools, loadProjects, loadResearch, loadSetup, revealProjectFolder } from "../../api-projects";
import { SessionContext } from "../../state";
import { Menu, PageHeader, Tabs, formatRoute, type IconName } from "../../ui";
import { displayText } from "../conversation";
import { AgentsTab } from "./agents";
import { ContextTab } from "./context";
import { ConversationTab } from "./conversation";
import { go, takeFlash } from "./flash";
import { OverviewTab } from "./overview";
import { shouldPoll } from "./research";
import { valueOf } from "./shared";
import type { ProjectData } from "./shared";
import { SettingsTab } from "./settings";
import "./projects.css";

export const TABS = [
  { id: "overview", label: "Overview", icon: "house" },
  { id: "conversation", label: "Conversation", icon: "message-square" },
  { id: "agents", label: "Agents", icon: "bot" },
  { id: "context", label: "Context", icon: "folder-open" },
  { id: "settings", label: "Settings", icon: "settings" },
] as const satisfies readonly { id: string; label: string; icon: IconName }[];

export type TabId = typeof TABS[number]["id"];

function useProject(id: string, navigate: (path: string) => void, known = "") {
  const [name, setName] = useState(known);
  const [setup, setSetup] = useState<ProjectData["setup"]>();
  const [research, setResearch] = useState<ProjectData["research"]>();
  const [context, setContext] = useState<ProjectData["context"]>();
  const [pools, setPools] = useState<ProjectData["pools"]>();
  const [harnesses, setHarnesses] = useState<ProjectData["harnesses"]>();
  const [now, setNow] = useState(() => Date.now());
  const [toast, setToast] = useState<string | null>(null);
  const session = useContext(SessionContext);
  const notify = (message: string) => { if (session) session.showToast(message); else setToast(message); };
  const [round, setRound] = useState(0);
  const [poolRound, setPoolRound] = useState(0);
  const current = useRef(id);
  current.current = id;

  useEffect(() => {
    const message = takeFlash();
    if (message) notify(message);
  }, [id]);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 4000);
    return () => window.clearTimeout(timer);
  }, [toast]);

  useEffect(() => {
    const live = id;
    void loadSetup(id).then((result) => {
      if (current.current !== live) return;
      setSetup(result);
      if (result.kind === "ok" && result.value) setName(result.value.name);
    });
    void loadProjects().then((result) => {
      if (current.current !== live || result.kind !== "ok") return;
      const found = result.value.find((item) => item.id === live);
      if (found) setName((prior) => prior || found.name);
    });
    void loadResearch(id).then((result) => { if (current.current === live) setResearch(result); });
    void loadContext(id).then((result) => { if (current.current === live) setContext(result); });
  }, [id, round]);

  useEffect(() => {
    void harnessCatalog().then(setHarnesses);
  }, []);

  useEffect(() => {
    void loadPools().then(setPools);
  }, [poolRound]);

  const jobs = valueOf(research)?.jobs ?? [];
  const polling = shouldPoll(jobs);
  useEffect(() => {
    if (!polling) return;
    const live = id;
    const timer = window.setInterval(() => {
      setNow(Date.now());
      void loadResearch(live).then((result) => { if (current.current === live) setResearch(result); });
      void loadContext(live).then((result) => { if (current.current === live) setContext(result); });
    }, 3000);
    return () => window.clearInterval(timer);
  }, [id, polling]);

  const assigned = valueOf(pools)?.find((choice) => choice.pool.projects.includes(id)) ?? null;
  const data: ProjectData = {
    id, name, setup, research, context, pools, harnesses, assigned, now, navigate,
    notify,
    reload: () => { setNow(Date.now()); setRound((value) => value + 1); },
    reloadPools: () => setPoolRound((value) => value + 1),
  };
  return { data, toast, session };
}

function ProjectMenu({ data }: { data: ProjectData }) {
  async function reveal() {
    const result = await revealProjectFolder(data.id);
    if (result.kind !== "ok") data.notify(result.kind === "not_ready" ? "Not ready yet: Show folder in Finder isn't in this build." : "Couldn't open the folder.");
  }
  return <Menu label="Project actions" items={[
    { label: "Show folder in Finder", icon: "folder-open", onSelect: () => void reveal() },
    { label: "Remove from CrazyAgent", icon: "trash", danger: true, onSelect: () => data.navigate(`/projects/${encodeURIComponent(data.id)}/settings?section=remove`) },
  ]} />;
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
      if (!event.metaKey || event.altKey || event.ctrlKey) return;
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
      {active === "conversation" && <ConversationTab data={data} />}
      {active === "agents" && <AgentsTab data={data} />}
      {active === "context" && <ContextTab data={data} />}
      {active === "settings" && <SettingsTab data={data} section={settingsSection} />}
    </div>
    {!session && <div className="pj-toast-region" aria-live="polite">{toast && <p className="toast" role="status">{displayText(toast)}</p>}</div>}
  </div>;
}

