import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";

/** How often a project page with its IDE loaded tells the engine the owner still uses it, in
 * milliseconds. */
export const HEARTBEAT_MS = 60_000;

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

/** What the IDE tab shows: nothing while the `ide_tab` flag is off, else the engine's IDE. */
export type IdeView = { tab: "off" } | { tab: "on"; state: IdeTabState };

/** The panel the IDE webview covers, in the window's CSS pixels. */
export type IdeBounds = { x: number; y: number; width: number; height: number };

/** What the tab says about the build: its plain line, the one action it offers, and whether
 * the IDE can open now. */
export type IdeStatus = { line: string; action: "download" | "retry" | null; ready: boolean };

const PLATFORMS: Record<string, string> = { macos: "macOS", linux: "Linux", windows: "Windows" };
const MEGABYTE = 1024 * 1024;
const UNQUALIFIED = "The IDE is unavailable on this platform.";
const EXTENSIONS_FAILED = "The IDE's extensions couldn't be installed, so the IDE didn't start. Try again.";
const LINK_FAILED = "The IDE couldn't get its private link to CrazyAgent, so it didn't start. Try again.";

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
  ide_server_start_timeout: "The IDE took too long to start. Try again.",
  ide_server_start_failed: "The IDE couldn't start. Try again.",
  ide_entropy_unavailable: "The IDE couldn't start. Try again.",
  ide_capability_capacity: LINK_FAILED,
  ide_capability_unavailable: LINK_FAILED,
  ide_server_exited: "The IDE stopped while it started. Try again.",
  ide_server_port_taken: "Another program took the IDE's port. Try again.",
  ide_port_unavailable: "No free port was found for the IDE. Try again.",
  ide_token_unreadable: "The IDE's sign-in file couldn't be read. Try again.",
  ide_tab_off: "The IDE tab is turned off.",
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

/** Says why the IDE didn't open; every refusal of the extension install before the server
 * starts reads as one sentence. Input: the refusal code. Output: the plain sentence. */
export function openFailure(code: string): string {
  if (code === "ide_extensions_unavailable" || code.startsWith("ide_extension_")) return EXTENSIONS_FAILED;
  return OPEN_FAILURES[code] ?? failureCopy(code);
}

/** Rounds a panel's on-screen box to whole pixels for the IDE webview. Input: the box, as
 * `getBoundingClientRect` gives it. Output: the bounds, or null while the panel is hidden. */
export function bounds(box: { left: number; top: number; width: number; height: number }): IdeBounds | null {
  const width = Math.round(box.width);
  const height = Math.round(box.height);
  if (width < 1 || height < 1) return null;
  return { x: Math.max(0, Math.round(box.left)), y: Math.max(0, Math.round(box.top)), width, height };
}

/** Whether the panel kept the box the IDE webview was last placed at. Inputs: the box now
 * and the one last placed, null before the first. Output: true when nothing moved. */
export function sameBounds(box: IdeBounds, last: IdeBounds | null): boolean {
  return last !== null && box.x === last.x && box.y === last.y && box.width === last.width && box.height === last.height;
}

/** Reads what the IDE tab shows; with `retry` the engine first starts the build download
 * again. Input: whether to retry. Output: the view. */
export function ideView(retry = false): Promise<Result<IdeView>> {
  return call<IdeView>("desktop_ide_state", { retry });
}

/** Opens a project's IDE and shows it over the panel. Inputs: the project and the panel.
 * Output: done, or the refusal. */
export function openIde(projectId: string, panel: IdeBounds): Promise<Result<null>> {
  return call<null>("desktop_ide_open", { projectId, bounds: panel });
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
