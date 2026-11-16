import { call, nextGeneration, receiptResult, type Result } from "./api";
import type { IntentReceipt } from "./engine-client";

export type KeepAwakeMode = "auto" | "manual" | "off";
export type KeepAwakeGrace = "min0" | "min5" | "min10" | "min30";
export type KeepAwakeCap = "h4" | "h8" | "h24" | "none";
export type LidRule = "ac_only" | "always" | "off";
export type BatteryFloor = "off" | "pct5" | "pct10" | "pct20";
export type HeatEnd = "critical" | "serious" | "never";
export type HeatNotice = "serious" | "off";
export type LidDisplay = "sleep" | "leave_on";
export type DisplayAwake = "off" | "while_holding";
export type LowPower = "battery_below40" | "on_battery" | "off";
export type Freeze = "off" | "on";
export type Mute = "off" | "on";

/** The Keep awake settings as the engine stores them (DEC-167), one field per setting. */
export interface KeepAwakeSettings {
  mode: KeepAwakeMode;
  grace: KeepAwakeGrace;
  cap: KeepAwakeCap;
  lid: LidRule;
  battery_floor: BatteryFloor;
  heat_end: HeatEnd;
  heat_notice: HeatNotice;
  lid_display: LidDisplay;
  display_awake: DisplayAwake;
  low_power: LowPower;
  freeze: Freeze;
  mute: Mute;
}

/** One app the owner may freeze while the lid is closed (Task 17): the name shown and its
 * id, the bundle id on macOS and the executable path on Linux. */
export interface FreezeApp {
  name: string;
  id: string;
}

export type KeepAwakeSettingId = keyof KeepAwakeSettings;

/** What the engine holds: nothing, the idle hold, or the idle hold with the closed lid. */
export type KeepAwakeWant = { kind: "nothing" } | { kind: "idle"; display: boolean } | { kind: "idle_and_lid"; display: boolean };

export type KeepAwakeReason = "work" | "timer" | "grace" | "idle" | "off";

/** Why a hold ended or was refused on the engine's last evaluation; a cap end carries the
 * hours of the cap the hold ran under. */
export type KeepAwakeEnd =
  | { kind: "battery"; pct: number }
  | { kind: "cap"; hours: number }
  | { kind: "battery_unread" | "heat" | "manual" | "off" | "hold_lost" };

/** Whether a hold covers the closed lid, or the reason it does not: on macOS also a lid
 * helper not turned on, one to remove with why, and a helper journal that cannot be read. */
export type LidSupport =
  | { kind: "held" | "off" | "idle_only_on_battery" | "needs_macos13" | "needs_approval" | "helper_missing" | "not_turned_on" | "sleep_not_restored" | "locked_by_policy" }
  | { kind: "not_available" | "remove_helper"; reason: string };

export type KeepAwakePlatform = "mac_os" | "linux" | "windows";

/** Keep awake as the engine last evaluated it: the settings, the settings version a save
 * sends back, the decision, the lid support, what this computer itself says of the lid
 * whatever the rule, the platform, how many agents work and the apps frozen while the lid
 * is closed. */
export interface KeepAwakeView {
  settings: KeepAwakeSettings;
  version: number;
  state: { want: KeepAwakeWant; reason: KeepAwakeReason; ends_at_ms: number | null; ended: KeepAwakeEnd | null };
  lid_support: LidSupport;
  lid_platform: LidSupport;
  platform: KeepAwakePlatform;
  work: number;
  freeze_apps: FreezeApp[];
}

/** One change the owner makes: start a timer, move the running one later, end the hold,
 * save the settings over the settings version read, on macOS turn on the lid-closed hold or
 * remove the lid helper, on Windows put the lid setting back now, or save the apps frozen
 * while the lid is closed. */
export type KeepAwakeAction =
  | { start: { minutes: number } }
  | { extend: { minutes: number } }
  | "end"
  | { set: { settings: KeepAwakeSettings; version: number } }
  | LidAction
  | { freeze_apps: { apps: FreezeApp[] } };

/** The lid commands, sent by their engine names: the two macOS lid helper ones and the
 * Windows Put the lid setting back now. */
export type LidAction = "turn_on_lid_hold" | "remove_lid_helper" | "put_lid_back";

export const MINUTES_MIN = 1;
export const MINUTES_MAX = 1440;
export const FREEZE_APPS_MAX = 16;
export const KEEP_AWAKE_POLL_MS = 5000;

/** How many times a save reads and lays its picks, the first time included, while another
 * window keeps saving first. */
export const SAVE_ATTEMPTS = 3;

/** The engine's sentence once another window saved first on every attempt, as the TUI says it. */
export const KEEP_AWAKE_STALE = "Keep awake changed in another window meanwhile. Try again.";

const STALE = "stale_revision";

/** The window event that says Keep awake changed here, so the control reads at once. */
export const KEEP_AWAKE_CHANGED = "crazyagent:keep-awake-changed";

/** The window event that opens the Keep awake panel over the page, sent by a click on a
 * Keep awake notification. */
export const KEEP_AWAKE_OPEN = "crazyagent:keep-awake-open";

/** The engine's settings before the first save. */
export const KEEP_AWAKE_DEFAULTS: KeepAwakeSettings = {
  mode: "auto", grace: "min10", cap: "h24", lid: "ac_only", battery_floor: "pct10",
  heat_end: "critical", heat_notice: "serious", lid_display: "sleep", display_awake: "off",
  low_power: "battery_below40",
  freeze: "off", mute: "off",
};

interface SettingWords { id: KeepAwakeSettingId; label: string; help: string; options: readonly { value: string; label: string }[] }

/** Every Keep awake setting in engine order with its words and options, the same words the
 * TUI shows. */
export const KEEP_AWAKE_SETTINGS: readonly SettingWords[] = [
  { id: "mode", label: "Mode", help: "Auto holds while agents work. Manual timer holds only while a timer you start runs. Off never holds.",
    options: [{ value: "auto", label: "Auto" }, { value: "manual", label: "Manual timer" }, { value: "off", label: "Off" }] },
  { id: "grace", label: "Grace after work ends", help: "How long the computer stays awake after the last agent finishes.",
    options: [{ value: "min0", label: "0" }, { value: "min5", label: "5 min" }, { value: "min10", label: "10 min" }, { value: "min30", label: "30 min" }] },
  { id: "cap", label: "Longest single hold", help: "One hold ends by itself after this long, even while agents still work.",
    options: [{ value: "h4", label: "4 h" }, { value: "h8", label: "8 h" }, { value: "h24", label: "24 h" }, { value: "none", label: "No limit" }] },
  { id: "lid", label: "Lid-closed hold", help: "Whether a hold also keeps the computer awake with the lid closed.",
    options: [{ value: "ac_only", label: "On AC power only" }, { value: "always", label: "Always" }, { value: "off", label: "Off" }] },
  { id: "battery_floor", label: "End at battery", help: "On battery, a hold ends when the charge falls to this level.",
    options: [{ value: "off", label: "Off" }, { value: "pct5", label: "5 %" }, { value: "pct10", label: "10 %" }, { value: "pct20", label: "20 %" }] },
  { id: "heat_end", label: "End at heat", help: "A hold ends when the computer gets this hot.",
    options: [{ value: "critical", label: "Critical" }, { value: "serious", label: "Serious" }, { value: "never", label: "Never" }] },
  { id: "heat_notice", label: "Heat notice", help: "Tells you when the computer gets hot during a hold.",
    options: [{ value: "serious", label: "At serious heat" }, { value: "off", label: "Off" }] },
  { id: "lid_display", label: "Display with the lid closed (macOS)", help: "What an outside display does while a hold runs with the lid closed.",
    options: [{ value: "sleep", label: "Sleep the display" }, { value: "leave_on", label: "Leave on" }] },
  { id: "display_awake", label: "Display awake with the lid open", help: "Whether a hold also keeps the display on. While it does, the screen stays unlocked.",
    options: [{ value: "off", label: "Off" }, { value: "while_holding", label: "While holding" }] },
  { id: "low_power", label: "Low Power Mode", help: "During a hold on battery, turns on the computer's low power mode to save charge, and puts it back when the hold ends.",
    options: [{ value: "battery_below40", label: "On battery below 40 %" }, { value: "on_battery", label: "Whenever on battery" }, { value: "off", label: "Off" }] },
  { id: "freeze", label: "Freeze apps while the lid is closed", help: "The listed apps pause while a hold runs with the lid closed and go on when the lid opens. Not while an external display is in use. If one stays paused, run crazyagent keep-awake restore; the next start also puts it back.",
    options: [{ value: "off", label: "Off" }, { value: "on", label: "On for the listed apps" }] },
  { id: "mute", label: "Mute audio while the lid is closed", help: "The sound goes off while a hold runs with the lid closed and comes back as it was when the lid opens. Not while an external display is in use.",
    options: [{ value: "off", label: "Off" }, { value: "on", label: "On" }] },
];

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The arguments one change sends to the bridge. Inputs: the change and its draft
 * generation. Output: the arguments; minutes outside 1 to 1440, or more than 16 apps or
 * one app twice, are a caller's mistake and throw. */
export function keepAwakeArgs(action: KeepAwakeAction, generation: number): { action: KeepAwakeAction; generation: number } {
  assert(Number.isSafeInteger(generation) && generation > 0, "a Keep awake change needs a draft generation");
  if (typeof action === "object" && ("start" in action || "extend" in action)) {
    const minutes = "start" in action ? action.start.minutes : action.extend.minutes;
    assert(Number.isInteger(minutes) && minutes >= MINUTES_MIN && minutes <= MINUTES_MAX, `Keep awake minutes must be ${MINUTES_MIN} to ${MINUTES_MAX}`);
  }
  if (typeof action === "object" && "freeze_apps" in action) {
    const ids = action.freeze_apps.apps.map((app) => app.id);
    assert(ids.length <= FREEZE_APPS_MAX && new Set(ids).size === ids.length, `Keep awake freezes at most ${FREEZE_APPS_MAX} apps, each once`);
  }
  return { action, generation };
}

/** Reads Keep awake as the engine last evaluated it. Output: the view. */
export function loadKeepAwake(): Promise<Result<KeepAwakeView>> {
  return call<KeepAwakeView>("desktop_keep_awake_read");
}

/** Reads the running apps the owner may freeze while the lid is closed: never CrazyAgent,
 * its engine, agents or system processes. Output: the apps by name. */
export function loadKeepAwakeApps(): Promise<Result<FreezeApp[]>> {
  return call<FreezeApp[]>("desktop_keep_awake_apps");
}

/** Sends one Keep awake change and, once the engine took it, tells the window so the
 * control reads at once. Input: the change. Output: nothing, or why the engine refused. */
export async function sendKeepAwake(action: KeepAwakeAction): Promise<Result<void>> {
  const result = await call<IntentReceipt>("desktop_keep_awake_act", keepAwakeArgs(action, nextGeneration()));
  if (result.kind !== "ok") return result;
  const taken = receiptResult(result.value);
  if (taken.kind === "ok" && typeof window !== "undefined") window.dispatchEvent(new Event(KEEP_AWAKE_CHANGED));
  return taken;
}

/** Saves picked settings over the settings the engine holds at that moment: reads, lays the
 * picks over that read, keeps every field this window does not list as read, and sends them
 * with the read's version. When another window saved first it reads and lays them again, at
 * most `SAVE_ATTEMPTS` times, then says so. Input: the lay from the read settings to the sent
 * ones. Output: nothing, or why it was not saved. */
export async function saveKeepAwake(lay: (stored: KeepAwakeSettings) => KeepAwakeSettings): Promise<Result<void>> {
  for (let attempt = 1; attempt <= SAVE_ATTEMPTS; attempt += 1) {
    const fresh = await loadKeepAwake();
    if (fresh.kind !== "ok") return fresh;
    const settings = { ...fresh.value.settings, ...lay(fresh.value.settings) };
    const result = await sendKeepAwake({ set: { settings, version: fresh.value.version } });
    if (result.kind !== "error" || result.code !== STALE) return result;
  }
  return { kind: "error", code: STALE, message: KEEP_AWAKE_STALE };
}
