import { call, type Result } from "./api";

export type UpdateMethod = { kind: "self_update" } | { kind: "homebrew"; cask: string } | { kind: "homebrew_formula"; formula: string } | { kind: "npm"; package: string } | { kind: "with_app" } | { kind: "unknown" };
export type LatestVersion = { kind: "not_checked" } | { kind: "no_check" } | { kind: "up_to_date" } | { kind: "newer"; version: string } | { kind: "failed"; code: string };
export interface LastUpdate { ok: boolean; code: string | null; output: string; from: string | null; to: string | null; ended_ms: number }
export interface HarnessVersions { id: string; method: UpdateMethod; installed: string | null; latest: LatestVersion; command: string | null; install: string | null; refusal: string | null; guide: string | null; running: boolean; last: LastUpdate | null }
export type UpdateMode = "check_and_update" | "check_only" | "off";
export type CheckWhen = "on_button" | "on_open";
export type OnlineLookup = "on" | "off";
export type InstallFromApp = "on" | "off";
export interface UpdateSettings { mode: UpdateMode; when: CheckWhen; online: OnlineLookup; install: InstallFromApp }

export const UPDATE_POLL_MS = 2_000;
export const UPDATE_POLLS_MAX = 600;
export const UPDATE_SETTINGS_DEFAULT: UpdateSettings = { mode: "check_and_update", when: "on_button", online: "on", install: "on" };

const COPY: Record<string, string> = {
  harness_update_unknown_install: "CrazyAgent can't tell how this was installed, so it won't update it.",
  harness_update_with_app: "Comes with CrazyAgent and updates with it.",
  harness_update_tool_missing: "The tool that updates it is no longer on this Mac.",
  harness_update_tool_unsafe: "The tool that updates it can be changed by another user, so CrazyAgent won't run it.",
  harness_update_needs_admin: "Updating it needs an administrator. Update it yourself.",
  harness_update_as_root: "CrazyAgent is running as the root user, so it won't run an update.",
  harness_update_off: "Harness updates are off in settings.",
  harness_update_check_only: "Harness updates are set to Check only.",
  harness_update_busy: "An update for this harness is already running.",
  harness_update_not_installed: "It isn't installed on this Mac.",
  harness_update_timeout: "The update took too long and was stopped.",
  harness_update_failed: "The update didn't finish.",
  harness_update_unsupported_here: "CrazyAgent can't update it on this computer.",
  harness_update_lookup_failed: "Couldn't check for a newer version.",
  harness_update_check_unreadable: "Couldn't read the newer version from the vendor's site.",
  harness_update_unknown_harness: "CrazyAgent doesn't know this harness.",
  harness_update_outcome_unknown: "The update ended in a way CrazyAgent doesn't know.",
  harness_install_no_tool: "CrazyAgent installs with Homebrew or npm and found no way to install this one.",
  harness_install_off: "Install from the app is off in settings.",
  harness_install_already: "It's already installed on this Mac.",
  harness_install_not_found: "The install finished, but CrazyAgent can't find the harness. Choose Check again.",
  harness_install_failed: "The install didn't finish.",
};

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Reads one harness's installed version, how it was installed and what Update would run, and
 * looks for a newer version when `look` is set. Inputs: the harness id and whether to look now.
 * Output: the versions, not ready, or a plain error. */
export function harnessVersions(id: string, look: boolean): Promise<Result<HarnessVersions>> {
  assert(id.length > 0, "harness id");
  return call<HarnessVersions>("desktop_harness_versions", { id, look });
}

/** Starts the update the confirm showed; the engine runs it in the background.
 * Input: the harness id. Output: ok once started, or a plain error with its code. */
export function updateHarness(id: string): Promise<Result<void>> {
  assert(id.length > 0, "harness id");
  return call<void>("desktop_harness_update", { id });
}

/** Starts the install the confirm showed, for a harness that is not on this Mac; the engine runs
 * it in the background. Input: the harness id. Output: ok once started, or a plain error with its code. */
export function installHarness(id: string): Promise<Result<void>> {
  assert(id.length > 0, "harness id");
  return call<void>("desktop_harness_install", { id });
}

/** Reads the harness update settings. Output: the four settings, not ready, or a plain error. */
export function loadHarnessUpdateSettings(): Promise<Result<UpdateSettings>> {
  return call<UpdateSettings>("desktop_harness_update_settings");
}

/** Saves all four harness update settings at once. Input: the settings. Output: ok or a plain error. */
export function setHarnessUpdateSettings(settings: UpdateSettings): Promise<Result<void>> {
  return call<void>("desktop_harness_set_update_settings", { settings });
}

/** The plain sentence for an update or check code. Input: the code. Output: one sentence, the
 * same for no two known codes. */
export function updateCopy(code: string): string {
  return COPY[code] ?? COPY.harness_update_outcome_unknown;
}

/** How a harness was installed, as the Update confirm says it. Input: the install method.
 * Output: one sentence naming the tool and package when there is one. */
export function installedWith(method: UpdateMethod): string {
  switch (method.kind) {
    case "homebrew": return `Installed with Homebrew (${method.cask}).`;
    case "homebrew_formula": return `Installed with Homebrew (${method.formula}).`;
    case "npm": return `Installed with npm (${method.package}).`;
    case "self_update": return "Installed with its own installer, which Update asks to update it.";
    case "with_app": return COPY.harness_update_with_app;
    case "unknown": return "Installed some way CrazyAgent can't tell.";
  }
}

/** The line under a harness: its version and what a check found. A failed check reads the same
 * whatever went wrong. Input: the versions. Output: one or two sentences. */
export function versionLine(versions: HarnessVersions): string {
  const installed = versions.installed === null ? "Version unknown." : `Version ${versions.installed}.`;
  const latest = versions.latest;
  if (latest.kind === "newer") return `${installed} Version ${latest.version} is available.`;
  if (latest.kind === "up_to_date") return `${installed} Up to date.`;
  if (latest.kind === "no_check") return `${installed} This install updates itself. Update asks it to look now.`;
  if (latest.kind === "failed") return `${installed} ${COPY.harness_update_lookup_failed}`;
  return installed;
}

/** Re-reads a harness while its update runs, every `UPDATE_POLL_MS`, at most `UPDATE_POLLS_MAX`
 * reads. Inputs: the id, the reader and the wait. Output: the last read, or its failure. */
export async function pollWhileRunning(id: string, read: (id: string, look: boolean) => Promise<Result<HarnessVersions>>, wait: (ms: number) => Promise<void>): Promise<Result<HarnessVersions>> {
  let last = await read(id, false);
  for (let polls = 1; polls < UPDATE_POLLS_MAX && last.kind === "ok" && last.value.running; polls += 1) {
    await wait(UPDATE_POLL_MS);
    last = await read(id, false);
  }
  assert(last.kind !== "ok" || last.value.id === id, "versions for another harness");
  return last;
}
