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
export type HarnessJob = "update" | "install";
export type ReadVersions = (id: string, look: boolean) => Promise<Result<HarnessVersions>>;

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
  harness_update_busy: "Another harness update or install is running.",
  harness_update_not_installed: "It isn't installed on this Mac.",
  harness_update_timeout: "The update took too long and was stopped.",
  harness_update_failed: "The update didn't finish.",
  harness_update_unsupported_here: "CrazyAgent can't update it on this computer.",
  harness_update_lookup_failed: "Couldn't check for a newer version.",
  harness_update_check_unreadable: "Couldn't read the newer version from the vendor's site.",
  harness_update_unknown_harness: "CrazyAgent doesn't know this harness.",
  harness_update_outcome_unknown: "The update ended in a way CrazyAgent doesn't know.",
  harness_update_check_failed: "The check for a newer version didn't finish.",
  harness_update_start_failed: "CrazyAgent couldn't start the tool that updates or installs it.",
  harness_update_settings_unreadable: "CrazyAgent couldn't read the harness update settings.",
  harness_update_unavailable: "The engine couldn't open its update records. Try again.",
  harness_install_no_tool: "CrazyAgent installs with Homebrew or npm and found no way to install this one.",
  harness_install_off: "Install from the app is off in settings.",
  harness_install_already: "It's already installed on this Mac.",
  harness_install_not_found: "The install finished, but CrazyAgent can't find the harness. Choose Check again.",
  harness_install_failed: "The install didn't finish.",
  harness_install_needs_node: "CrazyAgent installs this one with npm, and Node.js isn't on this Mac yet.",
  harness_install_list_invalid: "CrazyAgent couldn't install that list of harnesses. Pick them again.",
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

/** Starts installing several harnesses one after another; the engine keeps going if the step
 * closes, and a failure does not stop the rest. Input: the harness ids, RCLI left out.
 * Output: ok once queued, or a plain error with its code. */
export function installHarnesses(ids: readonly string[]): Promise<Result<void>> {
  assert(ids.length > 0 && ids.every((id) => id.length > 0), "harness ids");
  return call<void>("desktop_harness_install_many", { ids });
}

/** Reads the harness ids still waiting to install, in the order they will run.
 * Output: the ids, not ready, or a plain error. */
export function queuedHarnesses(): Promise<Result<string[]>> {
  return call<string[]>("desktop_harness_queued");
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

/** The line a job would run for a harness. Inputs: the versions and the job. Output: the line,
 * or null when the engine offers none. */
export function jobLine(versions: HarnessVersions, job: HarnessJob): string | null {
  return job === "update" ? versions.command : versions.install;
}

/** Reads a harness again right before its confirm opens, so the dialog shows the line the engine
 * has now and not the one read when the page opened. Inputs: the id, the job and the reader.
 * Output: the read, and whether the confirm may open: the read is good, no job runs and the
 * engine still offers a line for the job. */
export async function readForConfirm(id: string, job: HarnessJob, read: ReadVersions): Promise<{ read: Result<HarnessVersions>; open: boolean }> {
  assert(id.length > 0, "harness id");
  const fresh = await read(id, false);
  return { read: fresh, open: fresh.kind === "ok" && !fresh.value.running && jobLine(fresh.value, job) !== null };
}

/** Re-reads a harness while its update or install runs, every `UPDATE_POLL_MS`, at most
 * `UPDATE_POLLS_MAX` reads, and stops once `live` says the row is gone. Inputs: the id, the
 * reader, the wait and whether the row still shows. Output: the last read, or its failure. */
export async function pollWhileRunning(id: string, read: ReadVersions, wait: (ms: number) => Promise<void>, live: () => boolean = () => true): Promise<Result<HarnessVersions>> {
  let last = await read(id, false);
  for (let polls = 1; polls < UPDATE_POLLS_MAX && live() && last.kind === "ok" && last.value.running; polls += 1) {
    await wait(UPDATE_POLL_MS);
    if (!live()) break;
    last = await read(id, false);
  }
  assert(last.kind !== "ok" || last.value.id === id, "versions for another harness");
  return last;
}
