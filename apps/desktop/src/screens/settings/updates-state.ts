import type { AvailableUpdate, UpdateProgress } from "../../api-updates";
import { formatBytes } from "../new-project/wizard";

export const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

export type UpdatePhase =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "current"; checkedAt: number }
  | { kind: "available"; update: AvailableUpdate; checkedAt: number }
  | { kind: "installing"; update: AvailableUpdate; step: UpdateProgress }
  | { kind: "installed"; update: AvailableUpdate; engineStarted: boolean }
  | { kind: "failed"; update: AvailableUpdate | null; message: string };

export type UpdateEvent =
  | { kind: "check_started" }
  | { kind: "check_done"; update: AvailableUpdate | null; at: number }
  | { kind: "check_failed"; code: string }
  | { kind: "install_started" }
  | { kind: "progress"; progress: UpdateProgress }
  | { kind: "install_done"; engineStarted: boolean }
  | { kind: "install_failed"; code: string }
  | { kind: "channel_changed" };

const COPY: Record<string, string> = {
  update_check_failed: "Couldn't reach the update server. Check your connection and try again.",
  update_download_failed: "The download didn't finish. Nothing changed. Try again.",
  update_too_large: "The download was larger than any CrazyAgent release, so it was stopped. Nothing changed.",
  update_signature_invalid: "The download didn't match CrazyAgent's release signature, so nothing was installed.",
  update_install_failed: "The update couldn't be installed. CrazyAgent is still on its current version and the engine was started again.",
  update_service_failed: "Couldn't stop the engine for the update, so nothing was installed. Try again.",
  update_not_checked: "Check for updates first.",
  update_channel_invalid: "The saved update channel isn't valid. Choose Stable or Dev again.",
  update_no_data_root: "CrazyAgent can't find its data folder on this computer.",
  update_io: "CrazyAgent couldn't save the update setting. Check that its data folder can be written.",
  sidecar_missing: "This copy of CrazyAgent has no bundled command line tool, so it can't update itself. Install it from the release page.",
  busy: "Another update step is still running. Wait for it to finish.",
  engine_unreachable: "CrazyAgent's updater isn't reachable. Restart CrazyAgent and try again.",
};

/** Turns an update error code into one plain sentence for the owner.
 * Input: the code. Output: the copy, or the generic update fallback. */
export function updateMessage(code: string): string {
  return COPY[code] ?? "Something went wrong with the update. Try again.";
}

const blocking = (state: UpdatePhase) => state.kind === "installing" || state.kind === "installed";

/** Applies one update event. Checks and channel switches never interrupt an install, and an
 * install starts only from a found release. Inputs: the phase and the event. Output: the next
 * phase, the same object when the event does not apply. */
export function reduceUpdate(state: UpdatePhase, event: UpdateEvent): UpdatePhase {
  switch (event.kind) {
    case "check_started":
      return blocking(state) ? state : { kind: "checking" };
    case "check_done":
      if (blocking(state)) return state;
      return event.update ? { kind: "available", update: event.update, checkedAt: event.at } : { kind: "current", checkedAt: event.at };
    case "check_failed":
      return blocking(state) ? state : { kind: "failed", update: null, message: updateMessage(event.code) };
    case "install_started":
      if (state.kind !== "available" && state.kind !== "failed" || !state.update) return state;
      return { kind: "installing", update: state.update, step: { kind: "downloading", downloaded: 0, total: null } };
    case "progress":
      return state.kind === "installing" ? { ...state, step: event.progress } : state;
    case "install_done":
      return state.kind === "installing" ? { kind: "installed", update: state.update, engineStarted: event.engineStarted } : state;
    case "install_failed":
      return state.kind === "installing" ? { kind: "failed", update: state.update, message: updateMessage(event.code) } : state;
    case "channel_changed":
      return blocking(state) ? state : { kind: "idle" };
  }
}

/** The share of the download done, 0 to 100. Input: the install step. Output: the percent, or
 * null when the step is not a download of known size. */
export function progressPercent(step: UpdateProgress): number | null {
  if (step.kind !== "downloading" || !step.total) return null;
  return Math.min(100, Math.floor(step.downloaded * 100 / step.total));
}

/** Says what the install is doing now. Input: the install step. Output: one line. */
export function progressLabel(step: UpdateProgress): string {
  switch (step.kind) {
    case "downloading": {
      const percent = progressPercent(step);
      return step.total && percent !== null
        ? `Downloading ${formatBytes(step.downloaded)} of ${formatBytes(step.total)} (${percent}%)`
        : `Downloading ${formatBytes(step.downloaded)}`;
    }
    case "verifying": return "Checking the signature...";
    case "stopping_engine": return "Stopping the engine. Your agents pick up where they were after the restart.";
    case "installing": return "Installing...";
  }
}

/** Whether a background check should tell the owner about a release. Inputs: the version last
 * announced or null, and the release found or null. Output: true once per new version. */
export function shouldAnnounce(announced: string | null, found: AvailableUpdate | null): boolean {
  return found !== null && found.version !== announced;
}

export interface UpdateStore {
  get: () => UpdatePhase;
  dispatch: (event: UpdateEvent) => void;
  subscribe: (listener: () => void) => () => void;
}

/** Creates the store the background check and Settings > Updates share, for
 * `useSyncExternalStore`. Output: get, dispatch and subscribe; listeners hear only changes. */
export function createUpdateStore(): UpdateStore {
  let phase: UpdatePhase = { kind: "idle" };
  const listeners = new Set<() => void>();
  return {
    get: () => phase,
    dispatch(event) {
      const next = reduceUpdate(phase, event);
      if (next === phase) return;
      phase = next;
      for (const listener of listeners) listener();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
  };
}

export const updateStore = createUpdateStore();
