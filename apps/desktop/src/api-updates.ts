import { Channel } from "@tauri-apps/api/core";
import { call, type Result } from "./api";

export type UpdateChannel = "stable" | "dev";

export interface UpdateSettings {
  current_version: string;
  channel: UpdateChannel;
}

export interface AvailableUpdate {
  version: string;
  current_version: string;
  notes: string | null;
}

export type UpdateProgress =
  | { kind: "downloading"; downloaded: number; total: number | null }
  | { kind: "verifying" }
  | { kind: "stopping_engine" }
  | { kind: "installing" };

export interface InstallOutcome {
  engine_started: boolean;
}

/** Reads this app's version and the saved update channel. Output: the settings, not ready, or
 * a plain error. */
export function loadUpdateSettings(): Promise<Result<UpdateSettings>> {
  return call<UpdateSettings>("desktop_update_settings");
}

/** Saves the update channel, shared with `crazyagent update`. Input: stable or dev.
 * Output: the new settings. */
export function setUpdateChannel(channel: UpdateChannel): Promise<Result<UpdateSettings>> {
  return call<UpdateSettings>("desktop_set_update_channel", { channel });
}

/** Checks the saved channel for a newer signed release. Output: the release, or null when this
 * app is current. */
export function checkForUpdate(): Promise<Result<AvailableUpdate | null>> {
  return call<AvailableUpdate | null>("desktop_check_update");
}

/** Downloads, verifies and installs the checked release, stopping the engine first.
 * Input: a handler for each step. Output: whether the engine started on the new version. */
export function installUpdate(onProgress: (progress: UpdateProgress) => void): Promise<Result<InstallOutcome>> {
  const progress = new Channel<UpdateProgress>();
  progress.onmessage = onProgress;
  return call<InstallOutcome>("desktop_install_update", { progress });
}

/** Restarts CrazyAgent so the installed version runs. */
export function restartApp(): Promise<Result<void>> {
  return call<void>("desktop_restart_app");
}

/** Puts the bundled `crazyagent` on the owner's PATH. Output: the tool's one-line report. */
export function linkCommandLineTool(): Promise<Result<string>> {
  return call<string>("desktop_link_cli");
}
