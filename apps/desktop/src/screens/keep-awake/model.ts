import { MINUTES_MAX, MINUTES_MIN, type KeepAwakeSettingId, type KeepAwakeSettings, type KeepAwakeView } from "../../api-keep-awake";

/** How the control draws a state: green holds, amber holds with a lid limit, red ended by
 * itself, plain is not holding (DEC-167 addendum 5). */
export type KeepAwakeTone = "hold" | "limit" | "ended" | "plain";

/** The longest label the control and the TUI header show. */
export const LABEL_MAX = 32;

const COUNT_MAX = 999;
const MINUTE_MS = 60_000;
const MINUTES_SHOWN_MAX = 999 * 60 + 59;
const REASON_MAX = 200;
const GRACE_MINUTES = { min0: 0, min5: 5, min10: 10, min30: 30 } as const;
const CAP_WORDS = { h4: "4 h", h8: "8 h", h24: "24 h", none: null } as const;
const BATTERY_WORDS = { off: null, pct5: "5% battery", pct10: "10% battery", pct20: "20% battery" } as const;
const HEAT_WORDS = { critical: "critical heat", serious: "serious heat", never: null } as const;
const LID_WORDS = { ac_only: "Hold on AC power only", always: "Hold always", off: "Sleep as usual" } as const;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function holding(view: KeepAwakeView): boolean {
  return view.state.want.kind !== "nothing";
}

/** Says how many agents run, with counts above 999 as "999+" so the label keeps its length.
 * Input: the count. Output: the words, such as "3 agents run". */
function agentsRun(work: number): string {
  const count = work > COUNT_MAX ? `${COUNT_MAX}+` : String(Math.max(0, Math.trunc(work)));
  return `${count} ${work === 1 ? "agent runs" : "agents run"}`;
}

/** The time until a hold ends, rounded up to the minute and never zero. Inputs: the end and
 * now, both in milliseconds. Output: "2h 14m", "3h" or "59m". */
export function timeLeft(endsAtMs: number | null, now: number): string {
  const left = endsAtMs === null ? 1 : Math.ceil((endsAtMs - now) / MINUTE_MS);
  const minutes = Math.min(MINUTES_SHOWN_MAX, Math.max(1, Number.isFinite(left) ? left : 1));
  assert(minutes >= 1 && minutes <= MINUTES_SHOWN_MAX, "the time left is 1 minute to 999 h 59 m");
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours === 0) return `${rest}m`;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}

/** The label of a hold that ended by itself, or null when nothing ended or the owner ended
 * it (the timer ran out, End, or mode Off). Input: the view. Output: the label or null. */
function endedLabel(view: KeepAwakeView): string | null {
  const ended = view.state.ended;
  if (ended === null) return null;
  switch (ended.kind) {
    case "battery": return `Ended: battery ${Math.min(100, Math.max(0, Math.trunc(ended.pct)))}%`;
    case "battery_unread": return "Ended: battery unreadable";
    case "heat": return "Ended: heat";
    case "cap": return `Ended: ${CAP_WORDS[view.settings.cap] ?? "time"} limit`;
    case "hold_lost": return "Ended: hold lost";
    case "manual":
    case "off": return null;
  }
}

/** The label of a running hold that a closed lid would end, or null. Input: the view.
 * Output: the label or null. */
function lidLimit(view: KeepAwakeView): string | null {
  if (!holding(view)) return null;
  switch (view.lid_support.kind) {
    case "idle_only_on_battery": return "Idle only: on battery";
    case "needs_macos13": return "Lid-closed hold needs macOS 13";
    case "needs_approval": return "Needs approval";
    default: return null;
  }
}

/** The label when nothing is held and nothing ended by itself. Input: the view. Output: the
 * label for mode Off, Manual without a timer, Auto standing down, or Auto at rest. */
function restingLabel(view: KeepAwakeView): string {
  if (view.settings.mode === "off") return "Off";
  if (view.settings.mode === "manual") return "Manual, no timer";
  return view.work > 0 ? "Paused until work ends" : "Auto, nothing running";
}

/** The control's label for one view, word for word from the approved States board, at most
 * 32 characters (DEC-167 addenda 5 and 6). An end by itself wins, then mode Off, then while
 * holding a lid limit, the timer, the grace, a held lid and the working agents. Inputs: the
 * view and now in milliseconds. Output: the label. */
export function keepAwakeLabel(view: KeepAwakeView, now: number = Date.now()): string {
  const label = pickLabel(view, now);
  assert(label.length >= 1 && label.length <= LABEL_MAX, `the Keep awake label is 1 to ${LABEL_MAX} characters: ${label}`);
  return label;
}

function pickLabel(view: KeepAwakeView, now: number): string {
  const ended = endedLabel(view);
  if (ended !== null) return ended;
  if (!holding(view) || view.settings.mode === "off") return restingLabel(view);
  const limit = lidLimit(view);
  if (limit !== null) return limit;
  if (view.state.reason === "timer") return `Awake, ${timeLeft(view.state.ends_at_ms, now)} left`;
  if (view.state.reason === "grace") return `Awake ${timeLeft(view.state.ends_at_ms, now)} more, work done`;
  if (view.state.want.kind === "idle_and_lid") return "Awake, lid OK";
  return `Awake while ${agentsRun(view.work)}`;
}

/** The tone the control draws one view in. Input: the view. Output: the tone. */
export function keepAwakeTone(view: KeepAwakeView): KeepAwakeTone {
  if (endedLabel(view) !== null) return "ended";
  if (!holding(view) || view.settings.mode === "off") return "plain";
  return lidLimit(view) === null ? "hold" : "limit";
}

/** What the panel says after the label about when sleep comes back. Input: the view.
 * Output: one sentence. */
function afterLabel(view: KeepAwakeView): string {
  const grace = GRACE_MINUTES[view.settings.grace];
  if (endedLabel(view) !== null || view.settings.mode === "off") return "The computer sleeps as usual.";
  if (!holding(view)) {
    if (view.settings.mode === "manual") return "Holds only while a timer you start runs.";
    if (view.work > 0) return "You ended the hold; it starts again with new work.";
    return grace === 0 ? "Holds while agents work." : `Holds while agents work and ${grace} minutes after.`;
  }
  switch (view.lid_support.kind) {
    case "idle_only_on_battery": return "A closed lid sleeps the computer until it is on AC power.";
    case "needs_macos13": return "A closed lid sleeps the computer.";
    case "needs_approval": return "Allow the lid helper in System Settings to hold with the lid closed.";
    default: break;
  }
  if (view.state.reason === "timer") return "Sleep comes back when the timer ends.";
  if (view.state.reason === "grace") return "Sleep comes back after that.";
  if (view.state.want.kind === "idle_and_lid") return `${agentsRun(view.work)}; closing the lid keeps the computer awake.`;
  const finish = view.work === 1 ? "it finishes" : "they finish";
  return grace === 0 ? `Sleep comes back when ${finish}.` : `Sleep comes back ${grace} minutes after ${finish}.`;
}

/** The panel's first line: the label, then when sleep comes back. Inputs: the view and now
 * in milliseconds. Output: the line. */
export function panelLine(view: KeepAwakeView, now: number = Date.now()): string {
  return `${keepAwakeLabel(view, now)}. ${afterLabel(view)}`;
}

/** What ends a hold by itself, from the battery, heat and cap settings; a part set to Off,
 * Never or No limit drops out. Input: the settings. Output: such as "At 10% battery,
 * critical heat, or 24 h", or "Never". */
export function endsByItself(settings: KeepAwakeSettings): string {
  const parts = [BATTERY_WORDS[settings.battery_floor], HEAT_WORDS[settings.heat_end], CAP_WORDS[settings.cap]].filter((part): part is NonNullable<typeof part> => part !== null);
  assert(parts.length <= 3, "three settings end a hold by itself");
  if (parts.length === 0) return "Never";
  if (parts.length === 1) return `At ${parts[0]}`;
  if (parts.length === 2) return `At ${parts[0]} or ${parts[1]}`;
  return `At ${parts[0]}, ${parts[1]}, or ${parts[2]}`;
}

/** What the lid rule does with a closed lid. Input: the view. Output: the words. */
export function lidClosedLine(view: KeepAwakeView): string {
  return LID_WORDS[view.settings.lid];
}

/** Why this computer cannot hold with the lid closed, or null when it can, the rule is Off
 * or only the power source stands in the way. Input: the view. Output: the reason or null. */
export function lidReason(view: KeepAwakeView): string | null {
  const lid = view.lid_support;
  switch (lid.kind) {
    case "needs_macos13": return "Needs macOS 13 or newer";
    case "needs_approval": return "Waits for your approval in System Settings";
    case "helper_missing": return "Not set up on this computer yet";
    case "locked_by_policy": return "Locked by your organization";
    case "not_available": return lid.reason.slice(0, REASON_MAX);
    case "held":
    case "off":
    case "idle_only_on_battery": return null;
  }
}

/** Why this computer cannot honour one setting, or null when it can. Inputs: the setting
 * and the view. Output: the reason shown on the setting's line, or null. */
export function unavailable(id: KeepAwakeSettingId, view: KeepAwakeView): string | null {
  if ((id === "heat_end" || id === "heat_notice") && view.platform === "windows") return "Not available on Windows";
  if (id === "lid_display" && view.platform !== "mac_os") return "Only on macOS";
  return id === "lid" ? lidReason(view) : null;
}

/** Reads the minutes typed into the custom field. Input: the typed text. Output: a whole
 * number from 1 to 1440, or null for anything else. */
export function customMinutes(typed: string): number | null {
  const text = typed.trim();
  if (!/^[0-9]{1,4}$/.test(text)) return null;
  const minutes = Number(text);
  return minutes >= MINUTES_MIN && minutes <= MINUTES_MAX ? minutes : null;
}

/** Whether Extend has a timer to move: something is held and the timer is the reason.
 * Input: the view. Output: the answer. */
export function canExtend(view: KeepAwakeView): boolean {
  return holding(view) && view.state.reason === "timer";
}

/** Whether End has a hold to end. Input: the view. Output: the answer. */
export function canEnd(view: KeepAwakeView): boolean {
  return holding(view);
}
