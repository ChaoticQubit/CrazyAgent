import { useEffect, useState, type ReactNode } from "react";
import type { Result } from "../../api";
import { loadOverview, overviewCopy, refreshOverview, OVERVIEW_POLL_MS, type OverviewTabId, type ProjectOverview, type Readme } from "../../api-overview";
import { openLink } from "../../api-projects";
import { displayText } from "../conversation";
import { MarkdownView } from "./markdown";
import { ago } from "./research";
import type { ProjectData } from "./shared";
import "./overview-tabs.css";

/** One tab of the Overview strip: a generated tab, the project's README or today's activity. */
export type StripId = OverviewTabId | "readme" | "activity";

/** The Overview strip in the order it is shown, Quick info first. */
export const STRIP: { id: StripId; label: string }[] = [
  { id: "quick_info", label: "Quick info" },
  { id: "features", label: "Features" },
  { id: "competitors", label: "Competitors" },
  { id: "install_and_use", label: "Install & use" },
  { id: "readme", label: "README" },
  { id: "activity", label: "Activity" },
];

/** What the strip can ask for: one writing run now, and a fresh read of the tabs. */
export interface OverviewActions {
  refresh: () => Promise<Result<void>>;
  reload: () => void;
}

/** Where the picked tab is kept: the two calls of the browser's session storage. */
export interface TabStore {
  getItem: (key: string) => string | null;
  setItem: (key: string, value: string) => void;
}

const TAB_KEY = "crazyagent.overview-tab.";
const FIRST: StripId = "quick_info";
const LOADING = "Loading the Overview tabs...";
const READ_ONLY = "This is the project's own file. Edit it in the project.";
const CUT = "Showing the first part of a long file.";
const NO_README = "This project has no README file.";
const WRITING = "Writing now";
const NOT_READY = "CrazyAgent's engine isn't ready yet. Try again in a moment.";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The tab an arrow, Home or End key moves to from the picked one; arrows wrap. Inputs: the
 * picked tab and the key. Output: the tab to pick, or null for any other key. */
export function stripMove(current: StripId, key: string): StripId | null {
  const index = STRIP.findIndex((item) => item.id === current);
  assert(index >= 0, "the picked Overview tab is not in the strip");
  const target = key === "ArrowRight" ? index + 1 : key === "ArrowLeft" ? index - 1 : key === "Home" ? 0 : key === "End" ? STRIP.length - 1 : null;
  if (target === null) return null;
  return STRIP[(target + STRIP.length) % STRIP.length].id;
}

/** Reads the tab the owner last picked for a project in this session. Inputs: the project
 * and, for tests, the store. Output: the tab, Quick info when none is kept, the kept value is
 * no tab, or the storage refuses. */
export function storedTab(projectId: string, store?: TabStore): StripId {
  try {
    const kept = (store ?? window.sessionStorage).getItem(TAB_KEY + projectId);
    return STRIP.find((item) => item.id === kept)?.id ?? FIRST;
  } catch {
    return FIRST;
  }
}

/** Keeps the tab the owner picked for a project for this session; a storage that refuses is
 * left alone. Inputs: the project, the tab and, for tests, the store. Output: none. */
export function storeTab(projectId: string, tab: StripId, store?: TabStore): void {
  assert(STRIP.some((item) => item.id === tab), "only a tab of the strip is kept");
  try {
    (store ?? window.sessionStorage).setItem(TAB_KEY + projectId, tab);
  } catch {
    return;
  }
}

/** How often to read the tabs again: only while a run is writing, or a run is due and the
 * tabs are not off (no run comes while they are off). Input: the overview or null.
 * Output: the wait in milliseconds, or null to read once. */
export function pollEvery(overview: ProjectOverview | null): number | null {
  if (overview === null) return null;
  return overview.running || (overview.stale && overview.refresh !== "off") ? OVERVIEW_POLL_MS : null;
}

/** What a generated tab with no text says, by what will write it. Input: the overview.
 * Output: the line. */
function unwritten(overview: ProjectOverview): string {
  if (overview.running) return `${WRITING}. This tab fills in when the run ends.`;
  if (overview.refresh === "off") return "Overview tabs are off in Project settings.";
  if (overview.refresh === "refresh_only") return "Not written yet. Choose Refresh to write it.";
  return "Not written yet. It is written after the next landed change set, or when you choose Refresh.";
}

/** The strip's status words: that a run is writing, else when the tabs were last checked.
 * Inputs: the overview and now in milliseconds. Output: the words, or null with nothing to say. */
function checked(overview: ProjectOverview, now: number): string | null {
  if (overview.running) return WRITING;
  if (overview.checked_ms === null) return null;
  const said = ago(overview.checked_ms, now);
  return `Checked ${said === "Just now" ? "just now" : said}`;
}

/** The README tab: the project's own file rendered read only, with its path. Input: the file
 * or null. Output: the panel body. */
function ReadmePanel({ readme }: { readme: Readme | null }) {
  if (readme === null) return <p className="pj-muted">{NO_README}</p>;
  return <>
    <p className="pj-small pj-muted"><span className="pj-mono">{displayText(readme.path)}</span> {READ_ONLY}{readme.cut ? ` ${CUT}` : ""}</p>
    <MarkdownView source={readme.text} onOpenLink={(href) => void openLink(href)} />
  </>;
}

/** One generated tab: its Markdown, or what will write it. Inputs: the overview and the tab.
 * Output: the panel body. */
function GeneratedPanel({ overview, tab }: { overview: ProjectOverview; tab: OverviewTabId }) {
  const state = overview.tabs.find((item) => item.tab === tab);
  if (!state || state.text === null) return <p className="pj-muted">{unwritten(overview)}</p>;
  return <MarkdownView source={state.text} onOpenLink={(href) => void openLink(href)} />;
}

/** The Overview's tab strip and the picked tab: Quick info, Features, Competitors, Install &
 * use, README and Activity, with when the tabs were checked and the Refresh button. README and
 * Activity show even when the tabs could not be read.
 * Inputs: the project data, the overview or null, why it is null, now, the picked tab, the
 * pick handler, the actions, how Quick info renders a text and the Activity panel.
 * Output: the strip and its panel. */
export function OverviewTabs({ data, overview, problem, now, selected, onSelect, actions, quickInfo, activity }: {
  data: ProjectData;
  overview: ProjectOverview | null;
  problem?: string;
  now: number;
  selected: StripId;
  onSelect: (tab: StripId) => void;
  actions: OverviewActions;
  quickInfo: (text: string | null) => ReactNode;
  activity: ReactNode;
}) {
  const [asking, setAsking] = useState(false);
  assert(STRIP.some((item) => item.id === selected), "the picked Overview tab is not in the strip");
  const status = overview === null ? null : checked(overview, now);
  const off = overview === null || overview.running || overview.refresh === "off";
  const refresh = async () => {
    assert(!off, "Refresh was chosen while it is disabled");
    setAsking(true);
    const result = await actions.refresh().catch((): Result<void> => ({ kind: "not_ready" }));
    setAsking(false);
    if (result.kind === "error") data.notify(`Couldn't refresh the tabs. ${overviewCopy(result)}`);
    if (result.kind === "not_ready") data.notify(NOT_READY);
    actions.reload();
  };
  const missing = overview === null && selected !== "activity" ? <p className={problem ? "pj-error" : "pj-muted"} role={problem ? "alert" : "status"}>{problem ?? LOADING}</p> : null;
  return <section className="pj-stack-s ov-tabs" aria-label="Overview tabs">
    <div className="ov-strip">
      <div className="tabs ov-tablist" role="tablist" aria-label="Overview" onKeyDown={(event) => {
        const next = stripMove(selected, event.key);
        if (next === null) return;
        event.preventDefault();
        onSelect(next);
        requestAnimationFrame(() => document.getElementById(`ov-tab-${next}`)?.focus());
      }}>
        {STRIP.map((item) => <button key={item.id} type="button" id={`ov-tab-${item.id}`} role="tab" className="tab" aria-controls="ov-panel"
          aria-selected={item.id === selected} tabIndex={item.id === selected ? 0 : -1} onClick={() => onSelect(item.id)}>{item.label}</button>)}
      </div>
      <div className="pj-row ov-meta">
        {status && <span className="pj-small pj-muted" role="status">{status}</span>}
        <button type="button" className="btn" disabled={off || asking} onClick={() => void refresh()}>Refresh</button>
      </div>
    </div>
    {overview?.failure && <p className="pj-error" role="alert">Couldn't refresh the tabs. {displayText(overview.failure)}</p>}
    <div className="pj-stack-s" id="ov-panel" role="tabpanel" aria-labelledby={`ov-tab-${selected}`}>
      {missing}
      {selected === "activity" && activity}
      {selected === "quick_info" && quickInfo(overview?.tabs.find((item) => item.tab === "quick_info")?.text ?? null)}
      {overview !== null && selected === "readme" && <ReadmePanel readme={overview.readme} />}
      {overview !== null && (selected === "features" || selected === "competitors" || selected === "install_and_use") && <GeneratedPanel overview={overview} tab={selected} />}
    </div>
  </section>;
}

/** The Overview tabs of one project, read from the engine: reads once per visit and after
 * each action, and again every `OVERVIEW_POLL_MS` only while a run is writing or a landed
 * change set waits for one; keeps the picked tab for the session.
 * Inputs: the project data, how Quick info renders a text, the Activity panel and the read.
 * Output: the strip and its panel. */
export function ProjectOverviewTabs({ data, quickInfo, activity, load = loadOverview }: {
  data: ProjectData;
  quickInfo: (text: string | null) => ReactNode;
  activity: ReactNode;
  load?: typeof loadOverview;
}) {
  const [selected, setSelected] = useState<StripId>(() => storedTab(data.id));
  const [read, setRead] = useState<Result<ProjectOverview> | undefined>(undefined);
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    void load(data.id).then((result) => { if (live) setRead(result); });
    return () => { live = false; };
  }, [data.id, round, load]);
  const overview = read?.kind === "ok" ? read.value : null;
  const every = pollEvery(overview);
  useEffect(() => {
    if (every === null) return;
    const timer = window.setInterval(() => setRound((count) => count + 1), every);
    return () => window.clearInterval(timer);
  }, [every]);
  const problem = read?.kind === "not_ready" ? "Not ready yet: the Overview tabs aren't in this build."
    : read?.kind === "error" ? `Couldn't load the Overview tabs. ${overviewCopy(read)}` : undefined;
  return <OverviewTabs data={data} overview={overview} problem={problem} now={data.now} selected={selected}
    onSelect={(tab) => { storeTab(data.id, tab); setSelected(tab); }}
    actions={{ refresh: () => refreshOverview(data.id), reload: () => setRound((count) => count + 1) }}
    quickInfo={quickInfo} activity={activity} />;
}
