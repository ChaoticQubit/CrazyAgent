import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { loadContext, loadHarnesses, loadPools, loadProjects, loadResearch, loadSetup, revealProjectFolder } from "../../api-projects";
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
  { id: "overview", label: "Overview" },
  { id: "conversation", label: "Conversation" },
  { id: "agents", label: "Agents" },
  { id: "context", label: "Context" },
  { id: "settings", label: "Settings" },
] as const;

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
  const [round, setRound] = useState(0);
  const [poolRound, setPoolRound] = useState(0);
  const current = useRef(id);
  current.current = id;

  useEffect(() => {
    const message = takeFlash();
    if (message) setToast(message);
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
    void loadHarnesses().then(setHarnesses);
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
    notify: (message) => setToast(message),
    reload: () => { setNow(Date.now()); setRound((value) => value + 1); },
    reloadPools: () => setPoolRound((value) => value + 1),
  };
  return { data, toast };
}

function ProjectMenu({ data }: { data: ProjectData }) {
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState("");
  async function reveal() {
    setOpen(false);
    const result = await revealProjectFolder(data.id);
    setNote(result.kind === "ok" ? "" : result.kind === "not_ready" ? "Not ready yet: Show folder in Finder isn't in this build." : "Couldn't open the folder.");
  }
  return <div className="pj-menu">
    <button type="button" className="pj-btn" aria-haspopup="menu" aria-expanded={open} aria-label="Project actions" onClick={() => setOpen(!open)}>...</button>
    {open && <div className="pj-menu-list" role="menu" onKeyDown={(event) => { if (event.key === "Escape") setOpen(false); }}>
      <button type="button" role="menuitem" onClick={() => void reveal()}>Show folder in Finder</button>
      <button type="button" role="menuitem" onClick={() => { setOpen(false); data.navigate(`/projects/${encodeURIComponent(data.id)}/settings?section=remove`); }}>Remove from CrazyAgent</button>
    </div>}
    {note && <p className="pj-small pj-muted" role="status">{note}</p>}
  </div>;
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
  const { data, toast } = useProject(projectId, navigate, name);
  const active: TabId = TABS.some((item) => item.id === tab) ? tab as TabId : "overview";
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const setup = valueOf(data.setup);
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

  function arrows(event: ReactKeyboardEvent<HTMLDivElement>) {
    const position = TABS.findIndex((item) => item.id === active);
    const delta = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (!delta) return;
    event.preventDefault();
    const next = (position + delta + TABS.length) % TABS.length;
    select(TABS[next].id);
    tabs.current[next]?.focus();
  }

  return <div className="pj pj-project">
    <header className="pj-page-header">
      <div className="pj-title">
        <h1>{data.name ? displayText(data.name) : <span className="pj-skeleton" aria-label="Loading project name" />}</h1>
        {setup?.about && <p className="pj-secondary pj-truncate" title={setup.about}>{displayText(setup.about.split("\n")[0])}</p>}
      </div>
      <ProjectMenu data={data} />
    </header>
    <div className="pj-tabs" role="tablist" aria-label="Project" onKeyDown={arrows}>
      {TABS.map((item, index) => <button type="button" role="tab" key={item.id} id={`pj-tab-${item.id}`} aria-controls="pj-tab-panel"
        aria-selected={item.id === active} tabIndex={item.id === active ? 0 : -1} ref={(element) => { tabs.current[index] = element; }}
        onClick={() => select(item.id)}>{item.label}</button>)}
    </div>
    <div className="pj-tab-panel" id="pj-tab-panel" role="tabpanel" aria-labelledby={`pj-tab-${active}`}>
      {active === "overview" && <OverviewTab data={data} />}
      {active === "conversation" && <ConversationTab data={data} />}
      {active === "agents" && <AgentsTab data={data} />}
      {active === "context" && <ContextTab data={data} />}
      {active === "settings" && <SettingsTab data={data} section={section} />}
    </div>
    <div className="pj-toast-region" aria-live="polite">{toast && <p className="pj-toast" role="status">{displayText(toast)}</p>}</div>
  </div>;
}

