import { call, type Result } from "./api";
import { failureCopy } from "./api-projects";

/** The four Overview tabs an agent writes; README and Activity are not generated. */
export type OverviewTabId = "quick_info" | "features" | "competitors" | "install_and_use";

/** One generated tab: its Markdown or null before the first run, the version a save or an
 * accept must name, whether the owner edited it, a refresh's proposal waiting for review with
 * the proposal's own version ("" without one), which an accept sends back, and
 * why its file can't be shown (`blank`, `too_large`, `not_utf8`, `not_a_file` for a link or
 * folder in the file's place, `not_a_folder` for a tabs folder that is none), null when it can. */
export interface TabState { tab: OverviewTabId; text: string | null; version: string; edited: boolean; proposal: string | null; proposal_version: string; blocked: string | null }

/** The project's own README: its path in the project, its text and whether only the first
 * part of a long file is held. */
export interface Readme { path: string; text: string; cut: boolean }

/** What the Competitors tab is written from: nothing yet, research files the owner linked, or
 * the competitor research report. */
export type CompetitorState = { kind: "missing" } | { kind: "linked"; paths: string[] } | { kind: "report"; path: string };

/** When the tabs are written: after each landed change set and on Refresh, only on Refresh,
 * or never. */
export type OverviewRefresh = "after_landing" | "refresh_only" | "off";

/** What a refresh does with a tab: always propose, or replace a tab the owner has not edited. */
export type OverviewApply = "always_ask" | "when_unedited";

/** Everything the Overview tabs show for one project. `needs_home` is true for a project kept
 * outside the CrazyAgent folder, whose tabs cannot be written until it is moved there, and
 * `waiting_for_account` while `failure` is the engine's wait for an account. */
export interface ProjectOverview { readme: Readme | null; tabs: TabState[]; competitors: CompetitorState; running: boolean; stale: boolean; checked_ms: number | null; failure: string | null; waiting_for_account: boolean; refresh: OverviewRefresh; apply: OverviewApply; needs_home: boolean }

/** How often the tabs are read again while a run is going or a change set landed, in milliseconds. */
export const OVERVIEW_POLL_MS = 5_000;

const OVERVIEW_FAILURES: Record<string, string> = {
  overview_tab_changed: "Someone changed this tab since you opened it. Your text is kept below; compare and save again.",
  overview_tab_blocked: "CrazyAgent can't write this tab's file in the project. Check the project's .crazyagent folder.",
  overview_tab_too_large: "This text is too long for a tab. Shorten it and save again.",
  overview_tab_empty: "A tab can't be saved empty. Write something, or choose Cancel.",
  overview_no_proposal: "There is no proposal for this tab any more.",
  overview_proposal_changed: "A newer proposal replaced the one you opened. The diff now shows it; look it over and choose again.",
  overview_off: "Overview tabs are off for this project. Turn them on in Project settings.",
  overview_project_unknown: "CrazyAgent doesn't know this project any more.",
  overview_unavailable: "The Overview tabs can't be read right now. Try again in a moment.",
  overview_needs_project_home: "Overview tabs need this project in the CrazyAgent folder. Move it there in Project settings.",
};

/** Says why an Overview call was refused; every refusal has its own sentence. Input: the
 * refusal code, or the failure with the engine cause behind it. Output: the plain sentence. */
export function overviewCopy(failure: string | { code: string; cause?: string }): string {
  const code = typeof failure === "string" ? failure : failure.code;
  return Object.hasOwn(OVERVIEW_FAILURES, code) ? OVERVIEW_FAILURES[code] : failureCopy(failure);
}

/** Sends one Overview change that answers no data. Inputs: the bridge command and its
 * arguments. Output: done, or the refusal. */
async function done(command: string, args: Record<string, string>): Promise<Result<void>> {
  const result = await call<null>(command, args);
  return result.kind === "ok" ? { kind: "ok", value: undefined } : result;
}

/** Reads a project's Overview tabs, README and refresh state. Input: the project.
 * Output: the overview. */
export function loadOverview(projectId: string): Promise<Result<ProjectOverview>> {
  return call<ProjectOverview>("desktop_overview_read", { projectId });
}

/** Asks for one writing run of the tabs now. Input: the project. Output: done once the run
 * is queued, or the refusal. */
export function refreshOverview(projectId: string): Promise<Result<void>> {
  return done("desktop_overview_refresh", { projectId });
}

/** Saves the owner's text for one tab. Inputs: the project, the tab, the text and the version
 * the editor opened at. Output: done, or `overview_tab_changed` when the tab moved on. */
export function saveOverviewTab(projectId: string, tab: OverviewTabId, text: string, version: string): Promise<Result<void>> {
  return done("desktop_overview_save", { projectId, tab, text, version });
}

/** Accepts a refresh's proposal for one tab. Inputs: the project, the tab, the tab version the
 * shown diff was made from and the version of the proposal shown. Output: done, or the refusal,
 * `overview_proposal_changed` when a newer proposal took its place. */
export function acceptOverviewTab(projectId: string, tab: OverviewTabId, version: string, proposalVersion: string): Promise<Result<void>> {
  return done("desktop_overview_accept", { projectId, tab, version, proposalVersion });
}

/** Rejects a refresh's proposal for one tab, keeping the tab's text. Inputs: the project and
 * the tab. Output: done, or the refusal. */
export function rejectOverviewTab(projectId: string, tab: OverviewTabId): Promise<Result<void>> {
  return done("desktop_overview_reject", { projectId, tab });
}

/** Saves when a project's tabs are written. Inputs: the project and the setting.
 * Output: done, or the refusal. */
export function setOverviewRefresh(projectId: string, refresh: OverviewRefresh): Promise<Result<void>> {
  return done("desktop_overview_set_refresh", { projectId, refresh });
}

/** Saves what a refresh does with a tab. Inputs: the project and the setting.
 * Output: done, or the refusal. */
export function setOverviewApply(projectId: string, apply: OverviewApply): Promise<Result<void>> {
  return done("desktop_overview_set_apply", { projectId, apply });
}
