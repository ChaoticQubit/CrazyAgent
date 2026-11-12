import { useEffect, useRef, useState, type ReactNode } from "react";
import { nextGeneration, type Result } from "../../api";
import { loadRunMode } from "../../api-ide";
import { acceptOverviewTab, loadOverview, overviewCopy, refreshOverview, rejectOverviewTab, saveOverviewTab, OVERVIEW_POLL_MS, type OverviewTabId, type ProjectOverview, type Readme, type TabState } from "../../api-overview";
import { lineDiff } from "../../api-process";
import { failureCopy, openLink, receiptState, updateProjectSetup, type JobKind, type ResearchJob } from "../../api-projects";
import type { IntentReceipt } from "../../engine-client";
import { displayText } from "../conversation";
import { draftOf, editRequest } from "../new-project/wizard";
import { MarkdownView } from "./markdown";
import { ago } from "./research";
import { RunCommand, type RunMode } from "./run-command";
import { valueOf, type ProjectData } from "./shared";
import "../process.css";
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

/** What the strip can ask for: one writing run now, a save of the owner's text for a tab, an
 * accept or a reject of a refresh's proposal, and a fresh read of the tabs. */
export interface OverviewActions {
  refresh: () => Promise<Result<void>>;
  save: typeof saveOverviewTab;
  accept: typeof acceptOverviewTab;
  reject: typeof rejectOverviewTab;
  reload: () => void;
}

/** What one diff is made from: the tab version and the proposal version. */
export interface Shown { version: string; proposal_version: string }

/** An open review of one tab's proposal: whether a choice is being sent, why the last one was
 * refused or held, what the diff the owner was last told about is made from, and whether the
 * engine said that diff is gone and the read that shows the new one has not come yet. */
export interface TabReview { busy: boolean; note: string; seen: Shown; told: boolean }

/** What one generated tab's panel shows for the keyboard's sake: its mode, and whether a
 * refresh proposal's banner is on it. */
export interface PanelState { mode: "idle" | "editing" | "reviewing"; proposal: boolean }

/** How an accept or a reject ended: done, or refused and why; `stale` marks the refusal of an
 * accept whose diff is gone, because a newer proposal took its place or the tab changed. */
export interface ChoiceOutcome { done: boolean; note: string; stale?: true }

/** An open editor of one tab: the tab version the owner last compared the text with (the one
 * it opened at, then each changed one the page showed), the typed text, whether a save is
 * being sent, why the last save was refused or held, whether the tab changed since it opened,
 * and whether the engine said it changed and the read that shows the change has not come yet. */
export interface TabDraft { version: string; text: string; busy: boolean; note: string; changed: boolean; told: boolean }

/** The open editors of one project, by tab. */
export type OpenDrafts = Partial<Record<OverviewTabId, TabDraft>>;

/** How a save ended: saved, or refused with the typed text kept, why, and whether the tab
 * changed since the editor opened. */
export interface SaveOutcome { saved: boolean; changed: boolean; kept: string; note: string }

/** Where the picked tab is kept: the two calls of the browser's session storage. */
export interface TabStore {
  getItem: (key: string) => string | null;
  setItem: (key: string, value: string) => void;
}

/** The most UTF-8 bytes of one tab's text: the engine's `TAB_BYTES_MAX`, which a test compares
 * with the engine source. The limit an owner reads is this number in KB. */
export const TAB_BYTES_MAX = 24 * 1024;

/** The most characters of one open editor's text that are kept for the session; a longer text
 * is not kept. */
export const DRAFT_CHARS_MAX = 8 * TAB_BYTES_MAX;

/** The most command lines of one text that get Copy and Run. */
export const COMMAND_LINES_MAX = 200;

const TAB_FILES: Record<OverviewTabId, string> = { quick_info: "quick-info", features: "features", competitors: "competitors", install_and_use: "install-and-use" };
const TAB_IDS = Object.keys(TAB_FILES) as OverviewTabId[];
const BLOCKED: Record<string, string> = {
  blank: "its file is empty", too_large: `its file is larger than ${TAB_BYTES_MAX / 1024} KB`, not_utf8: "its file is not plain text",
  not_a_file: "a link or folder is where its file goes", not_a_folder: "the folder for the tabs is a link or a file",
};
const BLOCKED_BY_ENTRY = new Set(["not_a_file", "not_a_folder"]);
const COMPETITOR = "competitor_research";
const LINKS_SHOWN = 5;
const TAB_CHANGED = "overview_tab_changed";
const PROPOSAL_CHANGED = "overview_proposal_changed";
const EDIT_OPEN = "ov-edit-open";
const REVIEW_OPEN = "ov-review-open";
const REVIEW_PANEL = "ov-review";
const TAB_KEY = "crazyagent.overview-tab.";
const DRAFTS_KEY = "crazyagent.overview-drafts.";
const DIFF_CHANGED = "This tab changed since this diff was made. The diff now shows the change against the tab as it is; choose again.";
const FIRST: StripId = "quick_info";
const LOADING = "Loading the Overview tabs...";
const READ_ONLY = "This is the project's own file. Edit it in the project.";
const CUT = "Showing the first part of a long file.";
const NO_README = "This project has no README file.";
const WRITING = "Writing now";
const QUEUED = "Refresh waiting to run";
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

/** Reads the editors the owner left open for a project in this session, so typed text
 * survives leaving the Overview. Each comes back unlocked, with no note, at the tab version it
 * was last compared with. Inputs: the project and, for tests, the store. Output: the editors
 * by tab; none when nothing is kept or the storage refuses, and a kept entry that is no editor
 * or is over `DRAFT_CHARS_MAX` is left out. */
export function storedDrafts(projectId: string, store?: TabStore): OpenDrafts {
  const drafts: OpenDrafts = {};
  let kept: unknown;
  try {
    kept = JSON.parse((store ?? window.sessionStorage).getItem(DRAFTS_KEY + projectId) ?? "null");
  } catch {
    return drafts;
  }
  if (typeof kept !== "object" || kept === null) return drafts;
  for (const tab of TAB_IDS) {
    const entry = (kept as Record<string, unknown>)[tab];
    if (typeof entry !== "object" || entry === null) continue;
    const { version, text, changed } = entry as Record<string, unknown>;
    if (typeof version !== "string" || typeof text !== "string" || typeof changed !== "boolean" || text.length > DRAFT_CHARS_MAX) continue;
    drafts[tab] = { version, text, busy: false, note: "", changed, told: false };
  }
  assert(Object.keys(drafts).length <= TAB_IDS.length, "more kept editors than generated tabs");
  return drafts;
}

/** Keeps a project's open editors for this session: per tab the typed text, the tab version it
 * was last compared with and whether the tab changed. A closed editor leaves the kept set; a
 * text over `DRAFT_CHARS_MAX` is not kept, and a storage that refuses is left alone.
 * Inputs: the project, its open editors and, for tests, the store. Output: none. */
export function storeDrafts(projectId: string, drafts: OpenDrafts, store?: TabStore): void {
  const kept: Partial<Record<OverviewTabId, Pick<TabDraft, "version" | "text" | "changed">>> = {};
  for (const tab of TAB_IDS) {
    const draft = drafts[tab];
    if (draft !== undefined && draft.text.length <= DRAFT_CHARS_MAX) kept[tab] = { version: draft.version, text: draft.text, changed: draft.changed };
  }
  assert(Object.keys(kept).length <= TAB_IDS.length, "more editors to keep than generated tabs");
  try {
    (store ?? window.sessionStorage).setItem(DRAFTS_KEY + projectId, JSON.stringify(kept));
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

/** Whether the engine holds a writing run that has not started yet, after a Refresh, a
 * landed change set or a wait for an account: engine state, so it survives leaving the
 * view. Input: the overview. Output: the answer. */
function queued(overview: ProjectOverview): boolean {
  return overview.stale && !overview.running && overview.refresh !== "off" && !overview.needs_home;
}

/** What a generated tab with no text says, by what will write it. Input: the overview.
 * Output: the line. */
function unwritten(overview: ProjectOverview): string {
  if (overview.running) return `${WRITING}. This tab fills in when the run ends.`;
  if (overview.refresh === "off") return "Overview tabs are off in Project settings.";
  if (queued(overview)) return "A refresh is waiting to run. This tab fills in when it ends.";
  if (overview.refresh === "refresh_only") return "Not written yet. Choose Refresh to write it.";
  return "Not written yet. It is written after the next landed change set, or when you choose Refresh.";
}

/** The strip's status words: that a run is writing or waiting to run, else when the tabs
 * were last checked. Inputs: the overview and now in milliseconds. Output: the words, or null
 * with nothing to say. */
function checked(overview: ProjectOverview, now: number): string | null {
  if (overview.running) return WRITING;
  if (queued(overview)) return QUEUED;
  if (overview.checked_ms === null || overview.needs_home) return null;
  const said = ago(overview.checked_ms, now);
  return `Checked ${said === "Just now" ? "just now" : said}`;
}

/** Saves the owner's text for a tab at the version the editor opened at. A text over the tab
 * bound or an empty one is refused here, before anything is sent; a refused save keeps the
 * typed text. Inputs: the project, the tab with that version, the text and the save call.
 * Output: how the save ended. */
export async function saveTab(projectId: string, opened: { tab: OverviewTabId; version: string }, text: string, save: typeof saveOverviewTab = saveOverviewTab): Promise<SaveOutcome> {
  const refused = (note: string, changed = false): SaveOutcome => ({ saved: false, changed, kept: text, note });
  const bytes = new TextEncoder().encode(text).length;
  if (bytes > TAB_BYTES_MAX) return refused(overviewCopy("overview_tab_too_large"));
  if (text.trim() === "") return refused(overviewCopy("overview_tab_empty"));
  assert(bytes > 0, "an empty text was about to be sent");
  assert(bytes <= TAB_BYTES_MAX, "a text over the tab bound was about to be sent");
  const result = await save(projectId, opened.tab, text, opened.version).catch((): Result<void> => ({ kind: "not_ready" }));
  if (result.kind === "ok") return { saved: true, changed: false, kept: "", note: "" };
  if (result.kind === "not_ready") return refused(`${NOT_READY} Your text is kept.`);
  return refused(overviewCopy(result), result.code === TAB_CHANGED);
}

/** The editor after a read of the tabs: once the engine said the tab changed, the first read
 * that shows another version is the changed tab the owner was told to compare with; any later
 * change is found at Save. Inputs: the open editor and the tab's state.
 * Output: the editor, the same one when nothing is taken. */
export function draftOnRead(draft: TabDraft, state: TabState): TabDraft {
  if (!draft.told || draft.version === state.version) return draft;
  return { ...draft, version: state.version, told: false };
}

/** What a Save does: sends the tab version the owner compared the text with, or, when a read
 * put another version under the open editor, holds the Save, says so and counts that version
 * as compared, so the next Save is made against it. Every such change holds a Save again.
 * Inputs: the open editor and the tab's state. Output: the version to send, or the editor to
 * show instead. */
export function saveStep(draft: TabDraft, state: TabState): { send: string } | { hold: TabDraft } {
  assert(!draft.busy, "a save was started while another was still sent");
  const compared = draftOnRead(draft, state).version;
  if (compared === state.version) return { send: compared };
  return { hold: { ...draft, version: state.version, note: overviewCopy(TAB_CHANGED), changed: true } };
}

/** The editor after a save came back: closed when it was saved, else open with the typed text
 * and the reason; after the engine said the tab changed, it waits for the read that shows the
 * change. Inputs: the editor the save was made from and the outcome.
 * Output: the editor, or null to close it. */
export function afterSave(draft: TabDraft, outcome: SaveOutcome): TabDraft | null {
  if (outcome.saved) return null;
  assert(outcome.note !== "", "a refused save says why");
  return { ...draft, busy: false, note: outcome.note, changed: draft.changed || outcome.changed, told: draft.told || outcome.changed };
}

/** Where the keyboard goes when a panel changes and the control it was on left the page:
 * into the review when it opens, back to Review when it closes with the proposal still
 * waiting, else back to Edit after an editor or a review closes or a banner leaves. The
 * editor's text box takes the keyboard itself.
 * Inputs: the panel before and now. Output: the id of the element to focus, or null. */
export function focusAfter(previous: PanelState, current: PanelState): string | null {
  if (current.mode === "editing") return null;
  if (current.mode === "reviewing") return previous.mode === "reviewing" ? null : REVIEW_PANEL;
  if (previous.mode === "editing") return EDIT_OPEN;
  if (previous.mode === "reviewing") return current.proposal ? REVIEW_OPEN : EDIT_OPEN;
  return previous.proposal && !current.proposal ? EDIT_OPEN : null;
}

/** Moves the keyboard as `focusAfter` says whenever a panel changes. A banner that left on a
 * plain read moves it only when nothing else holds it, so a read never takes the keyboard
 * from where the owner is working. Input: the panel now. Output: none. */
function useReturnFocus(current: PanelState): void {
  const previous = useRef(current);
  useEffect(() => {
    const before = previous.current;
    previous.current = current;
    const target = focusAfter(before, current);
    if (target === null) return;
    const lost = document.activeElement === null || document.activeElement === document.body;
    if (before.mode !== current.mode || lost) document.getElementById(target)?.focus({ preventScroll: before.mode === current.mode });
  }, [current.mode, current.proposal]);
}

/** Says how an accept or a reject ended. A tab changed since the diff was made gets its own
 * sentence, since the page then shows a new diff; that refusal and a replaced proposal are
 * marked stale. Input: the result. Output: the outcome. */
function choiceOutcome(result: Result<void>): ChoiceOutcome {
  if (result.kind === "ok") return { done: true, note: "" };
  if (result.kind === "not_ready") return { done: false, note: `${NOT_READY} Nothing changed.` };
  if (result.code === PROPOSAL_CHANGED) return { done: false, note: overviewCopy(PROPOSAL_CHANGED), stale: true };
  if (result.code === TAB_CHANGED) return { done: false, note: DIFF_CHANGED, stale: true };
  return { done: false, note: overviewCopy(result) };
}

/** What the diff of a tab's state is made from. Input: the state. Output: the two versions. */
function shownOf(state: TabState): Shown {
  return { version: state.version, proposal_version: state.proposal_version };
}

/** Opens the review of a tab's proposal on the diff shown. Input: the tab's state with a
 * proposal. Output: the review. */
export function openReview(state: TabState): TabReview {
  assert(state.proposal !== null, "a review was opened for a tab with no proposal");
  return { busy: false, note: "", seen: shownOf(state), told: false };
}

/** The review after a read of the tabs: once the engine said the reviewed diff is gone, the
 * first read that shows another one is the diff the owner was told about; any later change is
 * found at Accept. Inputs: the open review and the tab's state with a proposal.
 * Output: the review, the same one when nothing is taken. */
export function reviewOnRead(review: TabReview, state: TabState): TabReview {
  assert(state.proposal !== null, "a review was read against a tab with no proposal");
  const shown = shownOf(state);
  if (!review.told || (review.seen.version === shown.version && review.seen.proposal_version === shown.proposal_version)) return review;
  return { ...review, seen: shown, told: false };
}

/** What an Accept does: sends the two versions of the diff the owner was told about, or, when
 * a read put a newer proposal or a changed tab under an open review, holds the Accept, says
 * which and counts the diff now shown as told, so the next Accept sends it. Every such change
 * holds an Accept again. Inputs: the open review and the tab's state.
 * Output: the versions to send, or the review to show instead. */
export function acceptStep(review: TabReview, state: TabState): { send: Shown } | { hold: TabReview } {
  assert(state.proposal !== null, "an Accept was chosen for a tab with no proposal");
  assert(!review.busy, "an Accept was chosen while another choice was still sent");
  const seen = reviewOnRead(review, state).seen;
  const shown = shownOf(state);
  if (seen.proposal_version !== shown.proposal_version) return { hold: { busy: false, note: overviewCopy(PROPOSAL_CHANGED), seen: shown, told: false } };
  if (seen.version !== shown.version) return { hold: { busy: false, note: DIFF_CHANGED, seen: shown, told: false } };
  return { send: shown };
}

/** The review after a choice came back: closed when it was done, else open with the reason on
 * the diff the choice was made for; after the engine said that diff is gone, it waits for the
 * read that shows the new one. Inputs: the review the choice was made in and the outcome.
 * Output: the review, or null to close it. */
export function afterChoice(review: TabReview, outcome: ChoiceOutcome): TabReview | null {
  if (outcome.done) return null;
  assert(outcome.note !== "", "a refused choice says why");
  return { busy: false, note: outcome.note, seen: review.seen, told: review.told || outcome.stale === true };
}

/** Accepts a refresh's proposal for a tab at the tab version and the proposal version the
 * shown diff was made from. Inputs: the project, the tab with both versions and the accept
 * call. Output: the outcome. */
export async function acceptTab(projectId: string, shown: { tab: OverviewTabId; version: string; proposal_version: string }, accept: typeof acceptOverviewTab = acceptOverviewTab): Promise<ChoiceOutcome> {
  return choiceOutcome(await accept(projectId, shown.tab, shown.version, shown.proposal_version).catch((): Result<void> => ({ kind: "not_ready" })));
}

/** Rejects a refresh's proposal for a tab; the tab's text stays. Inputs: the project, the tab
 * and the reject call. Output: the outcome. */
export async function rejectTab(projectId: string, tab: OverviewTabId, reject: typeof rejectOverviewTab = rejectOverviewTab): Promise<ChoiceOutcome> {
  return choiceOutcome(await reject(projectId, tab).catch((): Result<void> => ({ kind: "not_ready" })));
}

/** The review of a refresh's proposal for one tab: the proposal beside the tab's text as a
 * line diff (both texts whole past the diff's line bound), with Accept, Reject and Close.
 * Nothing changes until Accept; hidden characters show as visible escapes.
 * Inputs: the tab's label, its state with a proposal, whether a choice is being sent, the
 * refusal note and the three handlers. Output: the review. */
export function ProposalReview({ label, state, busy, note, onAccept, onReject, onClose }: {
  label: string;
  state: TabState;
  busy: boolean;
  note: string;
  onAccept: () => void;
  onReject: () => void;
  onClose: () => void;
}) {
  assert(state.proposal !== null, "a review was opened for a tab with no proposal");
  const current = state.text ?? "";
  const rows = lineDiff(current, state.proposal);
  const removed = rows?.filter((row) => row.kind === "removed").length ?? 0;
  const added = rows?.filter((row) => row.kind === "added").length ?? 0;
  return <section className="pj-stack-s ov-review" id={REVIEW_PANEL} tabIndex={-1} aria-label={`Proposed changes to ${label}`}>
    {state.edited && <p className="pj-note warn">You edited this tab. Accept replaces your text with the proposal; Reject keeps yours.</p>}
    {rows !== null ? <>
      <p className="pj-secondary">{`${removed} ${removed === 1 ? "line" : "lines"} removed, ${added} added. Nothing changes until you accept.`}</p>
      <div className="process-text pr-diff" role="group" aria-label="Proposed changes">
        {rows.map((row, index) => row.kind === "same" ? <span key={index} className="pr-diff-row">{displayText(row.text)}</span>
          : row.kind === "removed" ? <del key={index} className="pr-diff-row removed"><span className="visually-hidden">Removed: </span>{displayText(row.text)}</del>
            : <ins key={index} className="pr-diff-row added"><span className="visually-hidden">Added: </span>{displayText(row.text)}</ins>)}
      </div>
    </> : <>
      <p className="pj-secondary">Too many lines to mark each change, so both texts are shown whole. Nothing changes until you accept.</p>
      <div className="pr-sides">
        <div><h3>The tab now</h3><pre className="process-text">{displayText(current)}</pre></div>
        <div><h3>Proposed</h3><pre className="process-text">{displayText(state.proposal)}</pre></div>
      </div>
    </>}
    {note && <p className="pj-error" role="alert">{note}</p>}
    <div className="pj-row"><button type="button" className="btn primary" disabled={busy} onClick={onAccept}>Accept</button>
      <button type="button" className="btn" disabled={busy} onClick={onReject}>Reject</button>
      <button type="button" className="btn" disabled={busy} onClick={onClose}>Close</button></div>
  </section>;
}

/** The editor of one tab: a labelled text box with Save and Cancel; Cmd or Ctrl with Enter
 * saves. It is locked while a save is sent and shows why the last one was refused.
 * Inputs: the tab's label, the typed text, whether a save is being sent, the refusal note and
 * the text, save and cancel handlers. Output: the editor. */
export function TabEditor({ label, text, busy, note, onText, onSave, onCancel }: {
  label: string;
  text: string;
  busy: boolean;
  note: string;
  onText: (text: string) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return <form className="pj-stack-s ov-editor" onSubmit={(event) => { event.preventDefault(); if (!busy) onSave(); }}>
    <label className="pj-label" htmlFor="ov-edit">Edit {label}</label>
    <textarea id="ov-edit" className="pj-input ov-edit-text" rows={18} value={text} readOnly={busy} spellCheck={false} autoFocus aria-describedby="ov-edit-help"
      onChange={(event) => onText(event.target.value)}
      onKeyDown={(event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && !busy) { event.preventDefault(); onSave(); } }} />
    <p className="pj-help" id="ov-edit-help">Markdown. The tab is saved to this project's .crazyagent folder; a later refresh proposes changes instead of replacing your text.</p>
    {note && <p className="pj-error" role="alert">{note}</p>}
    <div className="pj-row"><button type="submit" className="btn primary" disabled={busy}>Save</button>
      <button type="button" className="btn" disabled={busy} onClick={onCancel}>Cancel</button></div>
  </form>;
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

/** Why a tab's file can't be shown, where the trouble is and what to do about it: the tab's
 * file, or the tabs' folder when that is what is wrong; an entry that is no file is removed,
 * a file is fixed or deleted. Inputs: the tab and the engine's reason code. Output: the line. */
function Blocked({ tab, reason }: { tab: OverviewTabId; reason: string }) {
  const why = Object.hasOwn(BLOCKED, reason) ? BLOCKED[reason] : "its file can't be read";
  const path = reason === "not_a_folder" ? ".crazyagent/overview" : `.crazyagent/overview/${TAB_FILES[tab]}.md`;
  const place = reason === "not_a_folder" ? "The folder" : reason === "not_a_file" ? "The path" : "The file";
  const fix = BLOCKED_BY_ENTRY.has(reason) ? "Remove it, then choose Refresh." : "Fix or delete the file, then choose Refresh.";
  return <p className="pj-error" role="alert">CrazyAgent can't show this tab: {why}. {place} is <span className="pj-mono">{path}</span> in
    the project. {fix}</p>;
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
    : overview.refresh === "off" ? "Overview tabs are off in Project settings."
    : queued(overview) ? "A refresh is waiting to run. This tab fills in when it ends." : "Choose Refresh to write this tab from it.";
  return <>
    {source.kind === "report" && <div className="pj-row"><span className="pj-small pj-muted">Written from the competitor research report.</span>
      <button type="button" className="btn" onClick={() => data.navigate(`/projects/${encodeURIComponent(data.id)}/context?file=${encodeURIComponent(source.path)}`)}>Open report</button></div>}
    {source.kind === "linked" && <p className="pj-small pj-muted">Written from the research linked to this project: {source.paths.slice(0, LINKS_SHOWN).map((path, index) =>
      <span key={path}>{index > 0 && ", "}<span className="pj-mono">{displayText(path)}</span></span>)}{source.paths.length > LINKS_SHOWN && ` and ${source.paths.length - LINKS_SHOWN} more`}</p>}
    {text ?? <p className="pj-muted">{follow}</p>}
  </>;
}

/** The body of one generated tab that can be shown: Competitors with its source, Quick info
 * through the project's own description, else its Markdown or what will write it; Install &
 * use gets Copy and Run on its command lines.
 * Inputs: the project data, the overview, the tab's state, the Run mode, the tab pick handler
 * and how Quick info renders a text. Output: the body. */
function TabBody({ data, overview, state, runMode, onSelect, quickInfo }: {
  data: ProjectData;
  overview: ProjectOverview;
  state: TabState;
  runMode: RunMode;
  onSelect: (tab: StripId) => void;
  quickInfo: (text: string | null) => ReactNode;
}) {
  const facts = state.tab === "quick_info";
  if (state.tab === "competitors") return <CompetitorsPanel data={data} overview={overview} state={state} onSelect={onSelect} />;
  if (state.text === null) return <><p className="pj-muted">{unwritten(overview)}</p>{facts && quickInfo(null)}</>;
  if (facts) return <>{quickInfo(state.text)}</>;
  return <MarkdownView source={state.text} onOpenLink={(href) => void openLink(href)} codeAction={state.tab === "install_and_use" ? commandAction(data.id, runMode) : undefined} />;
}

/** The banner of a tab whose refresh proposal waits, or the open review of it. Accept sends
 * the two versions of the diff the owner was told about and is held whenever a read changed
 * the diff since; either choice reads the tabs again, and a refused one keeps the review open
 * on what the engine holds now.
 * Inputs: the project data, the tab's label and state, the open review or null, the review
 * handler and the actions. Output: the banner or the review. */
function Proposal({ data, label, state, review, onReview, actions }: {
  data: ProjectData;
  label: string;
  state: TabState;
  review: TabReview | null;
  onReview: (review: TabReview | null) => void;
  actions: OverviewActions;
}) {
  assert(state.proposal !== null, "a proposal banner was shown for a tab with none");
  useEffect(() => {
    if (review === null) return;
    const read = reviewOnRead(review, state);
    if (read !== review) onReview(read);
  }, [review, state.version, state.proposal_version]);
  if (review === null) return <div className="pj-need"><span>A refresh proposes changes to this tab.</span>
    <button type="button" className="btn" id={REVIEW_OPEN} onClick={() => onReview(openReview(state))}>Review</button></div>;
  const choose = async (send: () => Promise<ChoiceOutcome>, told: string) => {
    assert(!review.busy, "a choice was made while another was still sent");
    onReview({ ...review, busy: true, note: "" });
    const outcome = await send();
    onReview(afterChoice(review, outcome));
    if (outcome.done) data.notify(told);
    actions.reload();
  };
  return <ProposalReview label={label} state={state} busy={review.busy} note={review.note} onClose={() => onReview(null)}
    onAccept={() => {
      const step = acceptStep(review, state);
      if ("hold" in step) return onReview(step.hold);
      void choose(() => acceptTab(data.id, { tab: state.tab, ...step.send }, actions.accept), `${label} now shows the refreshed text.`);
    }}
    onReject={() => void choose(() => rejectTab(data.id, state.tab, actions.reject), `The proposal for ${label} is rejected. The tab is unchanged.`)} />;
}

/** One generated tab: the move line for a project outside the CrazyAgent folder, why its file
 * can't be shown, or its body with Edit and, when a refresh proposal waits, its banner or
 * review. An open editor takes the body's place; once the tab changed under it, the changed
 * tab shows above the editor so the owner can compare, and a Save is made only against the
 * version the owner was told about: each later change holds the Save and says so again.
 * Inputs: the project data, the overview, the tab, its open editor and review or null with
 * their handlers, the actions, the Run mode, the tab pick handler and how Quick info renders
 * a text. Output: the panel body. */
function GeneratedPanel({ data, overview, tab, draft, onDraft, review, onReview, actions, runMode, onSelect, quickInfo }: {
  data: ProjectData;
  overview: ProjectOverview;
  tab: OverviewTabId;
  draft: TabDraft | null;
  onDraft: (draft: TabDraft | null) => void;
  review: TabReview | null;
  onReview: (review: TabReview | null) => void;
  actions: OverviewActions;
  runMode: RunMode;
  onSelect: (tab: StripId) => void;
  quickInfo: (text: string | null) => ReactNode;
}) {
  const state = overview.tabs.find((item) => item.tab === tab);
  const facts = tab === "quick_info";
  const shown = state !== undefined && !overview.needs_home && !state.blocked;
  useReturnFocus({ mode: draft !== null ? "editing" : review !== null && shown && state.proposal !== null ? "reviewing" : "idle", proposal: shown && draft === null && state.proposal !== null });
  useEffect(() => {
    if (draft === null || state === undefined) return;
    const read = draftOnRead(draft, state);
    if (read !== draft) onDraft(read);
  }, [draft, state?.version]);
  if (overview.needs_home) return <><NeedsHome data={data} />{facts && quickInfo(null)}</>;
  if (!state) return <><p className="pj-muted">{unwritten(overview)}</p>{facts && quickInfo(null)}</>;
  if (state.blocked) return <><Blocked tab={tab} reason={state.blocked} />{facts && quickInfo(null)}</>;
  const body = <TabBody data={data} overview={overview} state={state} runMode={runMode} onSelect={onSelect} quickInfo={quickInfo} />;
  const label = STRIP.find((item) => item.id === tab)?.label ?? "";
  if (draft === null) return <>
    {state.proposal !== null && <Proposal data={data} label={label} state={state} review={review} onReview={onReview} actions={actions} />}
    <div className="ov-edit-row"><button type="button" className="btn" id={EDIT_OPEN} onClick={() => onDraft({ version: state.version, text: state.text ?? "", busy: false, note: "", changed: false, told: false })}>Edit</button></div>
    {body}
  </>;
  const save = async () => {
    const step = saveStep(draft, state);
    if ("hold" in step) return onDraft(step.hold);
    onDraft({ ...draft, busy: true, note: "" });
    const outcome = await saveTab(data.id, { tab, version: step.send }, draft.text, actions.save);
    onDraft(afterSave(draft, outcome));
    actions.reload();
  };
  return <>
    {draft.changed && <section className="pj-stack-s ov-current" aria-label={`${label} as it is now`}><p className="pj-small pj-muted">The tab as it is now:</p>{body}</section>}
    <TabEditor label={label} text={draft.text} busy={draft.busy} note={draft.note} onText={(text) => onDraft({ ...draft, text })} onSave={() => void save()} onCancel={() => onDraft(null)} />
  </>;
}

/** The Overview's tab strip and the picked tab: Quick info, Features, Competitors, Install &
 * use, README and Activity, with when the tabs were checked and the Refresh button. README and
 * Activity show even when the tabs could not be read. Open editors are kept for the session,
 * so typed text survives leaving the Overview until it is saved or cancelled.
 * Inputs: the project data, the overview or null, why it is null, now, the picked tab, the
 * pick handler, the actions, the Run in project terminal mode, how Quick info renders a text,
 * the Activity panel and, for tests, the store of the open editors.
 * Output: the strip and its panel. */
export function OverviewTabs({ data, overview, problem, now, selected, onSelect, actions, runMode, quickInfo, activity, store }: {
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
  store?: TabStore;
}) {
  const [asking, setAsking] = useState(false);
  const [drafts, setDrafts] = useState<OpenDrafts>(() => storedDrafts(data.id, store));
  const open = useRef(drafts);
  const [reviews, setReviews] = useState<Partial<Record<OverviewTabId, TabReview>>>({});
  assert(STRIP.some((item) => item.id === selected), "the picked Overview tab is not in the strip");
  const keepDraft = (tab: StripId, draft: TabDraft | null) => {
    assert(tab !== "readme" && tab !== "activity", "only a generated tab has an editor");
    const next: OpenDrafts = { ...open.current, [tab]: draft ?? undefined };
    open.current = next;
    setDrafts(next);
    storeDrafts(data.id, next, store);
  };
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
      {overview !== null && selected !== "readme" && selected !== "activity" && <GeneratedPanel key={selected} data={data} overview={overview} tab={selected} draft={drafts[selected] ?? null}
        onDraft={(draft) => keepDraft(selected, draft)} review={reviews[selected] ?? null}
        onReview={(review) => setReviews((open) => ({ ...open, [selected]: review ?? undefined }))} actions={actions} runMode={runMode} onSelect={onSelect} quickInfo={quickInfo} />}
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
  return <OverviewTabs key={data.id} data={data} overview={overview} problem={problem} now={data.now} selected={selected}
    onSelect={(tab) => { storeTab(data.id, tab); setSelected(tab); }}
    actions={{ refresh: () => refreshOverview(data.id), save: saveOverviewTab, accept: acceptOverviewTab, reject: rejectOverviewTab, reload: () => setRound((count) => count + 1) }}
    runMode={runMode} quickInfo={quickInfo} activity={activity} />;
}
