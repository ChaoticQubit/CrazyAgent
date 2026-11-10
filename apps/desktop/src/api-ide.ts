import { call } from "./api";
import type { Failure, Result } from "./api";
import { failureCopy } from "./api-projects";
import { isMac } from "./ui/keys";

/** How often a project page with its IDE loaded tells the engine the owner still uses it, in
 * milliseconds. */
export const HEARTBEAT_MS = 60_000;

/** What the owner reads when the IDE webview didn't move with its panel. */
export const IDE_NOT_PLACED = "The IDE couldn't move to fit its panel. Resize the window to try again.";

/** What the owner reads when the IDE webview didn't hide, so it may still cover the page. */
export const IDE_NOT_HIDDEN = "The IDE couldn't be hidden, so it may still cover this page. Restart CrazyAgent if it stays.";

/** What the owner reads when a project's IDE didn't close with its page. */
export const IDE_NOT_CLOSED = "The IDE didn't close cleanly. Restart CrazyAgent if it stays on screen.";

/** What the owner reads when the IDE couldn't move to a window of its own. */
export const IDE_NOT_MOVED = "The IDE couldn't open in its own window. Try again.";

/** What the owner reads when the IDE's own window didn't come forward. */
export const IDE_NOT_FORWARD = "The IDE's window couldn't be brought forward. Try again.";

/** Where the engine's pinned IDE build stands. */
export type BuildState =
  | { state: "missing" }
  | { state: "downloading"; bytes: number; total: number }
  | { state: "verifying" }
  | { state: "ready"; version: string; root: string }
  | { state: "failed"; code: string };

/** Where the engine's IDE stands: the build, and the platform's name when the IDE server
 * cannot run on it. */
export type IdeTabState = { build: BuildState; unqualified: string | null };

/** What the project page shows for the IDE: the engine's IDE state and the platform the
 * desktop runs on. */
export type IdeView = { state: IdeTabState; platform: string };

/** The panel the IDE webview covers, in the window's CSS pixels. */
export type IdeBounds = { x: number; y: number; width: number; height: number; viewport: number };

/** What the tab says about the build: its plain line, the one action it offers, and whether
 * the IDE can open now. */
export type IdeStatus = { line: string; action: "download" | "retry" | null; ready: boolean };

const PLATFORMS: Record<string, string> = { macos: "macOS", linux: "Linux", windows: "Windows" };
const MEGABYTE = 1024 * 1024;
const UNQUALIFIED = "The IDE is unavailable on this platform.";
const EXTENSIONS_FAILED = "The IDE's extensions couldn't be installed, so the IDE didn't start. Try again.";
const LINK_FAILED = "The IDE couldn't get its private link to CrazyAgent, so it didn't start. Try again.";
const NOT_SUPPORTED = "The IDE isn't supported on this platform yet.";
const NO_BUILD = "This CrazyAgent version has no IDE build for this platform.";
const ENGINE_NOT_READY = "CrazyAgent's engine isn't ready yet. Try again in a moment.";

const BUILD_FAILURES: Record<string, string> = {
  ide_build_hash_mismatch: "The IDE download didn't match its published checksum, so it was deleted and never run.",
  ide_build_download_failed: "The IDE couldn't be downloaded. Check the connection, then try again.",
  ide_build_too_large: "The IDE download was larger than expected, so it was stopped and deleted.",
  ide_build_archive_invalid: "The IDE download held files outside its own folder, so it was deleted and never run.",
  ide_build_manifest_invalid: "This CrazyAgent version lists no valid IDE build. Update CrazyAgent.",
  ide_build_url_refused: "This CrazyAgent version lists an IDE address it won't download from. Update CrazyAgent.",
  ide_build_storage: "The IDE couldn't be saved in CrazyAgent's data folder. Check free space, then try again.",
};

const OPEN_FAILURES: Record<string, string> = {
  ide_platform_unqualified: UNQUALIFIED,
  ide_build_not_ready: "The IDE isn't ready yet. Wait for the download to finish.",
  ide_servers_full: "Too many project IDEs are open. Close another project, then try again.",
  ide_project_deleting: "This project is being deleted, so the IDE can't open.",
  ide_server_start_timeout: "The IDE took too long to start. Try again.",
  ide_server_start_failed: "The IDE couldn't start. Try again.",
  ide_entropy_unavailable: "The IDE couldn't start. Try again.",
  ide_capability_capacity: LINK_FAILED,
  ide_capability_unavailable: LINK_FAILED,
  ide_server_exited: "The IDE stopped while it started. Try again.",
  ide_server_port_taken: "Another program took the IDE's port. Try again.",
  ide_port_unavailable: "No free port was found for the IDE. Try again.",
  ide_token_unreadable: "The IDE's sign-in file couldn't be read. Try again.",
  ide_project_folder_invalid: "The IDE can't open this project's folder: it's missing or unreadable, or its path holds a colon (:), which the IDE's git can't use. Check the folder is still there; for a colon, add the project again from a folder whose path has none.",
  git_exclude_invalid: "The IDE didn't start: it couldn't add the agents folder to the project's .git/info/exclude, which keeps agent files out of your commits. Make .git/info a plain folder and .git/info/exclude a plain file under 1 MiB, not links, then open the IDE again.",
};

/** Says what the IDE tab shows for the engine's IDE state. Input: the state. Output: the
 * plain line, the one action offered (Download when missing, Retry when failed) and whether
 * the IDE can open; an unqualified platform offers nothing and never opens. */
export function ideStatus(state: IdeTabState): IdeStatus {
  if (state.unqualified !== null) {
    return { line: `The IDE is unavailable on ${PLATFORMS[state.unqualified] ?? state.unqualified}.`, action: null, ready: false };
  }
  const build = state.build;
  switch (build.state) {
    case "missing":
      return { line: "The IDE isn't downloaded yet.", action: "download", ready: false };
    case "downloading":
      return {
        line: build.total > 0 ? `Downloading the IDE: ${Math.round(build.bytes / MEGABYTE)} of ${Math.round(build.total / MEGABYTE)} MB.` : "Downloading the IDE.",
        action: null,
        ready: false,
      };
    case "verifying":
      return { line: "Checking the IDE download.", action: null, ready: false };
    case "ready":
      return { line: "Opening the IDE.", action: null, ready: true };
    case "failed":
      if (build.code === "ide_build_unsupported_platform") return { line: UNQUALIFIED, action: null, ready: false };
      return { line: BUILD_FAILURES[build.code] ?? failureCopy(build.code), action: "retry", ready: false };
  }
}

/** Whether the IDE takes the place of Context and Changes: the platform is qualified and the
 * build is ready, so the IDE can open. Input: the view, undefined until read. Output: true then. */
export function ideReplacesTabs(view: IdeView | undefined): boolean {
  return view !== undefined && ideStatus(view.state).ready;
}

/** Whether a qualified platform's build is downloading or being checked, so the page keeps
 * reading until it is Ready or Failed. Input: the view, undefined until read. Output: true then. */
export function ideFetching(view: IdeView | undefined): boolean {
  if (view === undefined || view.state.unqualified !== null) return false;
  return view.state.build.state === "downloading" || view.state.build.state === "verifying";
}

/** The page's line when the IDE state couldn't be read: the platform, the reason and Retry.
 * Inputs: the failed read and this desktop's platform. Output: the line and its action. */
export function ideReadFailed(failure: Failure, platform: string): { line: string; action: "retry" } {
  const reason = failure.kind === "error" ? failureCopy(failure) : ENGINE_NOT_READY;
  return { line: `IDE unavailable on ${PLATFORMS[platform] ?? platform}: ${reason}`, action: "retry" };
}

/** This desktop's platform in the engine's names, for a line the engine couldn't give one.
 * Input: the browser's platform string, by default the window's. Output: macos, windows,
 * linux, or the string itself. */
export function desktopPlatform(platform: string = typeof navigator === "undefined" ? "" : navigator.platform): string {
  if (isMac(platform)) return "macos";
  if (/^Win/i.test(platform)) return "windows";
  if (/^Linux/i.test(platform)) return "linux";
  return platform;
}

/** The page's line while Context and Changes stay: the build's progress while it downloads
 * or is checked, else why the IDE can't open on this platform and the build's one action.
 * Input: the view, undefined until read. Output: the line and action, or null while unread
 * or when the IDE replaces the tabs. */
export function ideUnavailable(view: IdeView | undefined): { line: string; action: IdeStatus["action"] } | null {
  if (view === undefined || ideReplacesTabs(view)) return null;
  const { build, unqualified } = view.state;
  const platform = unqualified ?? view.platform;
  const unavailable = `IDE unavailable on ${PLATFORMS[platform] ?? platform}:`;
  if (unqualified !== null) return { line: `${unavailable} ${NOT_SUPPORTED}`, action: null };
  if (build.state === "failed" && build.code === "ide_build_unsupported_platform") return { line: `${unavailable} ${NO_BUILD}`, action: null };
  const status = ideStatus(view.state);
  if (ideFetching(view)) return { line: status.line, action: null };
  return { line: `${unavailable} ${status.line}`, action: status.action };
}

/** Says why the IDE didn't open; every refusal of the extension install before the server
 * starts reads as one sentence. Input: the refusal code. Output: the plain sentence. */
export function openFailure(code: string): string {
  if (code === "ide_extensions_unavailable" || code.startsWith("ide_extension_")) return EXTENSIONS_FAILED;
  return OPEN_FAILURES[code] ?? failureCopy(code);
}

/** Rounds a panel's on-screen box to whole pixels for the IDE webview. Inputs: the box, as
 * `getBoundingClientRect` gives it, and the height of the page's view, which the app needs to
 * place the box in its window. Output: the bounds, or null while the panel is hidden. */
export function bounds(box: { left: number; top: number; width: number; height: number }, viewport: number): IdeBounds | null {
  const width = Math.round(box.width);
  const height = Math.round(box.height);
  if (width < 1 || height < 1) return null;
  return { x: Math.max(0, Math.round(box.left)), y: Math.max(0, Math.round(box.top)), width, height, viewport: Math.round(viewport) };
}

/** Whether the panel kept the box the IDE webview was last placed at. Inputs: the box now
 * and the one last placed, null before the first. Output: true when nothing moved. */
export function sameBounds(box: IdeBounds, last: IdeBounds | null): boolean {
  return last !== null && box.x === last.x && box.y === last.y && box.width === last.width && box.height === last.height && box.viewport === last.viewport;
}

/** Reads what the project page shows for the IDE; with `retry` the engine first starts the
 * build download again. Input: whether to retry. Output: the view. */
export function ideView(retry = false): Promise<Result<IdeView>> {
  return call<IdeView>("desktop_ide_state", { retry });
}

/** Opens a project's IDE and shows it over the panel. Inputs: the project and the panel.
 * Output: true when the IDE stays in its own window instead, or the refusal. */
export function openIde(projectId: string, panel: IdeBounds): Promise<Result<boolean | null>> {
  return call<boolean | null>("desktop_ide_open", { projectId, bounds: panel });
}

/** Moves a project's loaded IDE into a window of its own. Inputs: the project and the height of
 * this page, which measures the title bar. Output: done, or the refusal. */
export function openIdeWindow(projectId: string, viewport: number): Promise<Result<null>> {
  return call<null>("desktop_ide_window_open", { projectId, viewport });
}

/** Brings a project's own IDE window forward. Input: the project. Output: done, or the refusal. */
export function focusIdeWindow(projectId: string): Promise<Result<null>> {
  return call<null>("desktop_ide_window_focus", { projectId });
}

/** Asks whether a project's IDE sits in its own window. Input: the project. Output: true then. */
export function ideWindowOut(projectId: string): Promise<Result<boolean>> {
  return call<boolean>("desktop_ide_window_out", { projectId });
}

/** Moves the shown IDE over its panel. Inputs: the project and the panel. */
export function placeIde(projectId: string, panel: IdeBounds): Promise<Result<null>> {
  return call<null>("desktop_ide_place", { projectId, bounds: panel });
}

/** Hides a project's IDE while its tab is not shown, keeping it loaded. Input: the project. */
export function hideIde(projectId: string): Promise<Result<null>> {
  return call<null>("desktop_ide_hide", { projectId });
}

/** Closes a project's IDE when its project page closes. Input: the project. */
export function closeIde(projectId: string): Promise<Result<null>> {
  return call<null>("desktop_ide_close", { projectId });
}

/** Keeps a project's IDE server from stopping for idleness. Input: the project. Output:
 * whether its server still runs. */
export function touchIde(projectId: string): Promise<Result<boolean>> {
  return call<boolean>("desktop_ide_touch", { projectId });
}
