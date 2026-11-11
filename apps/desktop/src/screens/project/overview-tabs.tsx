import { useEffect, useState, type ReactNode } from "react";
import { nextGeneration, type Result } from "../../api";
import { loadRunMode } from "../../api-ide";
import { loadOverview, overviewCopy, refreshOverview, OVERVIEW_POLL_MS, type OverviewTabId, type ProjectOverview, type Readme, type TabState } from "../../api-overview";
import { failureCopy, openLink, receiptState, updateProjectSetup, type JobKind, type ResearchJob } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { displayText } from "../conversation";
import { draftOf, editRequest } from "../new-project/wizard";
import { MarkdownView } from "./markdown";
import { ago } from "./research";
import { RunCommand, type RunMode } from "./run-command";
import { valueOf, type ProjectData } from "./shared";
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

/** The most command lines of one text that get Copy and Run. */
export const COMMAND_LINES_MAX = 200;

const TAB_FILES: Record<OverviewTabId, string> = { quick_info: "quick-info", features: "features", competitors: "competitors", install_and_use: "install-and-use" };
const BLOCKED: Record<string, string> = { blank: "its file is empty", too_large: "its file is larger than 64 KB", not_utf8: "its file is not plain text" };
const COMPETITOR = "competitor_research";
const LINKS_SHOWN = 5;
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

/** How often to read the tabs again: only while a run is writing, or a run is due and can
 * come (none comes while the tabs are off or the project is outside the CrazyAgent folder).
 * Input: the overview or null. Output: the wait in milliseconds, or null to read once. */
export function pollEvery(overview: ProjectOverview | null): number | null {
  if (overview === null) return null;
  const due = overview.stale && overview.refresh !== "off" && !overview.needs_home;
  return overview.running || due ? OVERVIEW_POLL_MS : null;
}

/** Where "Move into CrazyAgent folder" lives: the Remove section of Project settings.
 * Input: the project. Output: the route. */
export function moveRoute(projectId: string): string {
  return `/projects/${encodeURIComponent(projectId)}/settings?section=remove`;
}

/** The command lines of a shell text as the page shows them: each line trimmed, one leading
 * prompt sign dropped, blank and comment lines left out, hidden characters as visible escapes,
 * so Copy takes exactly the shown text. Input: the text. Output: at most `COMMAND_LINES_MAX`. */
export function commandLines(source: string): string[] {
  const lines: string[] = [];
  for (const raw of source.split("\n")) {
    if (lines.length === COMMAND_LINES_MAX) break;
    const trimmed = raw.trim();
    const line = trimmed.startsWith("$ ") ? trimmed.slice(2).trim() : trimmed;
    if (line !== "" && !line.startsWith("#")) lines.push(displayText(line));
  }
  assert(lines.length <= COMMAND_LINES_MAX, "more command lines than the bound");
  return lines;
}

/** The action after one line of a shell block: Copy, and Run by the mode. A console block
 * gets it only on its prompt lines; a blank or comment line gets none.
 * Inputs: the project and the Run mode. Output: the per-line action for `MarkdownView`. */
function commandAction(projectId: string, mode: RunMode): (line: string, language: string) => ReactNode {
  return (line, language) => {
    if (language === "console" && !line.trimStart().startsWith("$ ")) return null;
    const [command] = commandLines(line);
    return command === undefined ? null : <RunCommand projectId={projectId} command={command} mode={mode} />;
  };
}

/** Turns competitor research on for a project through the project setup save the Research
 * switches use; the engine starts its agent when the project has a pool.
 * Inputs: the project data and, for tests, the save. Output: the note to show, empty once the
 * engine applied it, when the owner is told and the project is read again. */
export async function runResearch(data: Pick<ProjectData, "id" | "setup" | "notify" | "reload">, save?: (projectId: string, research: JobKind[]) => Promise<Result<IntentReceipt>>): Promise<string> {
  const setup = valueOf(data.setup);
  assert(setup !== null, "Run research was chosen for a project with no setup");
  assert(!setup.research.includes(COMPETITOR), "Run research was chosen though competitor research is already picked");
  const draft = draftOf(setup);
  const edit = editRequest(setup, { ...draft, research: [...draft.research, COMPETITOR] });
  assert(edit.setup.research.includes(COMPETITOR), "the setup to save does not pick competitor research");
  const send = save ?? ((projectId: string) => updateProjectSetup(projectId, edit.setup, nextGeneration(), null, setup.revision));
  const result = await send(data.id, edit.setup.research);
  if (result.kind === "not_ready") return "Not ready yet: changing a project's research isn't in this build.";
  if (result.kind === "error") return failureCopy(result);
  const state = receiptState(result.value);
  if (state.kind !== "applied") return state.kind === "rejected" ? failureCopy(state) : "We couldn't confirm your last change. Check it and try again.";
  data.notify("Competitor research is on. It starts when the project has a pool.");
  data.reload();
  return "";
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
  if (overview.checked_ms === null || overview.needs_home) return null;
  const said = ago(overview.checked_ms, now);
  return `Checked ${said === "Just now" ? "just now" : said}`;
}

/** The README tab: the project's own file rendered read only, with its path; its shell lines
 * get Copy only. Inputs: the project and the file or null. Output: the panel body. */
function ReadmePanel({ projectId, readme }: { projectId: string; readme: Readme | null }) {
  if (readme === null) return <p className="pj-muted">{NO_README}</p>;
  return <>
    <p className="pj-small pj-muted"><span className="pj-mono">{displayText(readme.path)}</span> {READ_ONLY}{readme.cut ? ` ${CUT}` : ""}</p>
    <MarkdownView source={readme.text} onOpenLink={(href) => void openLink(href)} codeAction={commandAction(projectId, "off")} />
  </>;
}

/** Why a tab's file can't be shown, where the file is and what to do about it.
 * Inputs: the tab and the engine's reason code. Output: the line. */
function Blocked({ tab, reason }: { tab: OverviewTabId; reason: string }) {
  const why = Object.hasOwn(BLOCKED, reason) ? BLOCKED[reason] : "its file can't be read";
  return <p className="pj-error" role="alert">CrazyAgent can't show this tab: {why}. The file
    is <span className="pj-mono">.crazyagent/overview/{TAB_FILES[tab]}.md</span> in the project. Fix or delete the file, then choose Refresh.</p>;
}

/** What the generated tabs of a project outside the CrazyAgent folder say, with the way to
 * the move. Input: the project data. Output: the line and its button. */
function NeedsHome({ data }: { data: ProjectData }) {
  return <div className="pj-need"><span>Overview tabs need this project in the CrazyAgent folder.</span>
    <button type="button" className="btn" onClick={() => data.navigate(moveRoute(data.id))}>Move into CrazyAgent folder</button></div>;
}

/** The newest competitor research job of a project, or null when it never had one.
 * Input: the project data. Output: the job. */
function competitorJob(data: ProjectData): ResearchJob | null {
  let newest: ResearchJob | null = null;
  for (const job of valueOf(data.research)?.jobs ?? []) if (job.kind === COMPETITOR && (newest === null || job.id > newest.id)) newest = job;
  return newest;
}

/** The Competitors tab with no competitor research to write it from: what the tab is, and by
 * the research's state that it runs, waits for a pool, did not finish, or the Run research
 * button. Nothing starts on its own. Inputs: the project data and the tab pick handler.
 * Output: the lines. */
function NoCompetitorResearch({ data, onSelect }: { data: ProjectData; onSelect: (tab: StripId) => void }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const setup = valueOf(data.setup);
  const job = competitorJob(data);
  if (job?.status === "queued" || job?.status === "running") return <p className="pj-muted" role="status">Competitor research is running. This tab is written from it once it finishes.</p>;
  if (job?.status === "needs_pool") return <p className="pj-muted" role="status">Competitor research is waiting for a pool. This tab is written from it once it finishes.</p>;
  if (job !== null || setup?.research.includes(COMPETITOR)) return <div className="pj-need"><span>Competitor research didn't finish. Try it again on the Activity tab.</span>
    <button type="button" className="btn" onClick={() => onSelect("activity")}>Open Activity</button></div>;
  const run = async () => {
    setBusy(true);
    setNote(await runResearch(data).catch(() => "CrazyAgent's engine isn't ready yet. Try again in a moment."));
    setBusy(false);
  };
  return <>
    <p>There is no competitor research for this project yet. This tab is written from it: who the competitors are, what differs, where this project is better and where it is not.</p>
    {setup !== null && <div><button type="button" className="btn primary" disabled={busy} onClick={() => void run()}>Run research</button></div>}
    {note && <p className="pj-error" role="alert">{note}</p>}
  </>;
}

/** The Competitors tab: what it is written from (nothing yet, linked research files or the
 * research report), then its text or what will write it.
 * Inputs: the project data, the overview, the tab's state and the tab pick handler.
 * Output: the panel body. */
function CompetitorsPanel({ data, overview, state, onSelect }: { data: ProjectData; overview: ProjectOverview; state: TabState; onSelect: (tab: StripId) => void }) {
  const source = overview.competitors;
  const text = state.text === null ? null : <MarkdownView source={state.text} onOpenLink={(href) => void openLink(href)} />;
  if (source.kind === "missing") return <><NoCompetitorResearch data={data} onSelect={onSelect} />{text}</>;
  const follow = overview.running ? `${WRITING}. This tab fills in when the run ends.`
    : overview.refresh === "off" ? "Overview tabs are off in Project settings." : "Choose Refresh to write this tab from it.";
  return <>
    {source.kind === "report" && <div className="pj-row"><span className="pj-small pj-muted">Written from the competitor research report.</span>
      <button type="button" className="btn" onClick={() => data.navigate(`/projects/${encodeURIComponent(data.id)}/context?file=${encodeURIComponent(source.path)}`)}>Open report</button></div>}
    {source.kind === "linked" && <p className="pj-small pj-muted">Written from the research linked to this project: {source.paths.slice(0, LINKS_SHOWN).map((path, index) =>
      <span key={path}>{index > 0 && ", "}<span className="pj-mono">{displayText(path)}</span></span>)}{source.paths.length > LINKS_SHOWN && ` and ${source.paths.length - LINKS_SHOWN} more`}</p>}
    {text ?? <p className="pj-muted">{follow}</p>}
  </>;
}

/** One generated tab: the move line for a project outside the CrazyAgent folder, why its file
 * can't be shown, or its Markdown or what will write it. Quick info keeps the project's own
 * description under any of those; Install & use gets Copy and Run on its command lines.
 * Inputs: the project data, the overview, the tab, the Run mode, the tab pick handler and how
 * Quick info renders a text. Output: the panel body. */
function GeneratedPanel({ data, overview, tab, runMode, onSelect, quickInfo }: {
  data: ProjectData;
  overview: ProjectOverview;
  tab: OverviewTabId;
  runMode: RunMode;
  onSelect: (tab: StripId) => void;
  quickInfo: (text: string | null) => ReactNode;
}) {
  const state = overview.tabs.find((item) => item.tab === tab);
  const facts = tab === "quick_info";
  if (overview.needs_home) return <><NeedsHome data={data} />{facts && quickInfo(null)}</>;
  if (state?.blocked) return <><Blocked tab={tab} reason={state.blocked} />{facts && quickInfo(null)}</>;
  if (state && tab === "competitors") return <CompetitorsPanel data={data} overview={overview} state={state} onSelect={onSelect} />;
  if (!state || state.text === null) return <><p className="pj-muted">{unwritten(overview)}</p>{facts && quickInfo(null)}</>;
  if (facts) return <>{quickInfo(state.text)}</>;
  return <MarkdownView source={state.text} onOpenLink={(href) => void openLink(href)} codeAction={tab === "install_and_use" ? commandAction(data.id, runMode) : undefined} />;
}

/** The Overview's tab strip and the picked tab: Quick info, Features, Competitors, Install &
 * use, README and Activity, with when the tabs were checked and the Refresh button. README and
 * Activity show even when the tabs could not be read.
 * Inputs: the project data, the overview or null, why it is null, now, the picked tab, the
 * pick handler, the actions, the Run in project terminal mode, how Quick info renders a text
 * and the Activity panel. Output: the strip and its panel. */
export function OverviewTabs({ data, overview, problem, now, selected, onSelect, actions, runMode, quickInfo, activity }: {
  data: ProjectData;
  overview: ProjectOverview | null;
  problem?: string;
  now: number;
  selected: StripId;
  onSelect: (tab: StripId) => void;
  actions: OverviewActions;
  runMode: RunMode;
  quickInfo: (text: string | null) => ReactNode;
  activity: ReactNode;
}) {
  const [asking, setAsking] = useState(false);
  assert(STRIP.some((item) => item.id === selected), "the picked Overview tab is not in the strip");
  const status = overview === null ? null : checked(overview, now);
  const off = overview === null || overview.running || overview.refresh === "off" || overview.needs_home;
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
      {overview === null && selected === "quick_info" && quickInfo(null)}
      {overview !== null && selected === "readme" && <ReadmePanel projectId={data.id} readme={overview.readme} />}
      {overview !== null && selected !== "readme" && selected !== "activity" && <GeneratedPanel data={data} overview={overview} tab={selected} runMode={runMode} onSelect={onSelect} quickInfo={quickInfo} />}
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
  const [runMode, setRunMode] = useState<RunMode>("off");
  useEffect(() => {
    let live = true;
    void load(data.id).then((result) => { if (live) setRead(result); });
    return () => { live = false; };
  }, [data.id, round, load]);
  useEffect(() => {
    let live = true;
    void loadRunMode(data.id).then((result) => { if (live) setRunMode(result.kind === "ok" ? result.value : "off"); });
    return () => { live = false; };
  }, [data.id]);
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
    runMode={runMode} quickInfo={quickInfo} activity={activity} />;
}
