import type { AccountLimits, LimitWindow, StepHealth, Tokens } from "../../api-usage";
import { Notice } from "../../ui";

export const REMOVED_PROJECTS = "<removed>";
export const REMOVED_LABEL = "Removed projects";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
export const API_KEY_ACCOUNT = "native_profile_codex_api_key";
const COMPACT = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

export type UsageRange = "today" | "7d" | "30d" | "all";

/** The range switch's choices, shortest first. */
export const RANGES: { value: UsageRange; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "all", label: "All" },
];

/** Where a range starts: midnight today at the viewer's offset now, the one fixed offset the
 * engine cuts every day with, six or twenty-nine days before it so the range holds whole
 * engine days even across a daylight saving change, or null for all time. Inputs: the range
 * and now. Output: milliseconds since the epoch, or null. */
export function rangeStart(range: UsageRange, now: Date): number | null {
  if (range === "all") return null;
  const shift = offsetMinutes(now) * MINUTE;
  const today = Math.floor((now.getTime() + shift) / DAY) * DAY - shift;
  return today - (range === "today" ? 0 : range === "7d" ? 6 : 29) * DAY;
}

/** A By day row's date as the engine cut it: the day's start moved by the same fixed offset
 * and read in UTC, so a day before a daylight saving change keeps its own date. Inputs: the
 * day's start in milliseconds and the offset in minutes east of UTC. Output: the label. */
export function dayLabel(dayMs: number, offset: number): string {
  return new Date(dayMs + offset * MINUTE).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" });
}

/** The viewer's offset east of UTC in minutes, as the engine cuts local days. Input: now. */
export function offsetMinutes(now: Date): number {
  const west = now.getTimezoneOffset();
  return west === 0 ? 0 : -west;
}

/** A token count in a few characters: 950, 1.2K, 3.4M. Input: the count. */
export function compactCount(value: number): string {
  return COMPACT.format(value);
}

/** The headline count usage shows and budgets spend (DEC-117 Q6): new input, cache writes and output, never cache re-reads. Input: the tokens. */
export function headlineTokens(tokens: Tokens): number {
  return tokens.input + tokens.output + tokens.cache_write;
}

/** The share of prompt tokens served from the provider's cache, as a whole percent.
 * Input: the tokens. Output: 0 to 100, or null when no prompt tokens were counted. */
export function cacheShare(tokens: Tokens): number | null {
  const prompt = tokens.input + tokens.cache_read + tokens.cache_write;
  return prompt === 0 ? null : Math.round((tokens.cache_read * 100) / prompt);
}

/** An observed limit window as it stands now, as the engine reads it: the owner's manual
 * reset replaces the observed one, and a window whose reset passed counts as unused with
 * its next reset unknown. Inputs: the window, the manual reset and now. Output: the window,
 * or null when none was observed. */
export function windowNow(observed: LimitWindow | null, overrideMs: number | null, nowMs: number): LimitWindow | null {
  if (!observed) return null;
  const resets = overrideMs ?? observed.resets_at_ms;
  if (resets !== null && resets <= nowMs) return { used_percent: 0, resets_at_ms: null };
  return { used_percent: observed.used_percent, resets_at_ms: resets };
}

/** How long until a time comes. Inputs: the time and now. Output: "in under a minute",
 * "in 14 min", "in 2 h 14 min" or "in 3 d 4 h". */
export function untilText(atMs: number, nowMs: number): string {
  const minutes = Math.floor((atMs - nowMs) / MINUTE);
  if (minutes < 1) return "in under a minute";
  const hours = Math.floor(minutes / 60);
  if (hours < 1) return `in ${minutes} min`;
  if (hours < 24) return `in ${hours} h ${minutes % 60} min`;
  return `in ${Math.floor(hours / 24)} d ${hours % 24} h`;
}

/** How long ago something happened. Inputs: the time and now. Output: "just now",
 * "5 min ago", "3 h ago" or "2 d ago". */
export function agoText(atMs: number, nowMs: number): string {
  const passed = Math.max(0, nowMs - atMs);
  if (passed < MINUTE) return "just now";
  if (passed < HOUR) return `${Math.floor(passed / MINUTE)} min ago`;
  if (passed < DAY) return `${Math.floor(passed / HOUR)} h ago`;
  return `${Math.floor(passed / DAY)} d ago`;
}

const STEP_WORDS: Record<StepHealth["step"], [string, string]> = {
  poll: ["Checking account limits", "limits shown may be out of date"],
  team: ["Counting agent tokens", "agent and research run token totals may be low"],
  runs: ["Counting research run tokens", "research token totals may be low"],
  recount: ["Recounting history", "the comparison may arrive later"],
};

/** One plain sentence for a usage step kind that panicked since the engine started: what
 * may be missing while it keeps failing, or when it worked again. Inputs: the step's health
 * and now. Output: the sentence. */
export function healthText(health: StepHealth, nowMs: number): string {
  const [what, missing] = STEP_WORDS[health.step];
  const times = health.panics === 1 ? "once" : `${health.panics} times`;
  const said = `${what} hit an internal error ${times} since the engine started, last ${agoText(health.last_ms, nowMs)}.`;
  return health.recovered_ms === null
    ? `${said} CrazyAgent keeps retrying; ${missing}.`
    : `${said} It worked again ${agoText(health.recovered_ms, nowMs)}.`;
}

/** Where and when an account's limits were last read, and whether the last check failed.
 * Inputs: the limits and now. Output: one line, or "Not read yet". */
export function readLine(limits: AccountLimits, nowMs: number): string {
  if (limits.observed_ms === null || limits.source === null) return "Not read yet";
  const from = limits.source === "poll" ? "Read from the provider" : "Read from a run's own record";
  return `${from} ${agoText(limits.observed_ms, nowMs)}`;
}

/** What a window with no current reading says: not read when the last read failed, whose
 * cause the card's notice above gives, or not reported by the provider, as for an API-key
 * account, which has no such window. Input: the account's last failure. Output: the
 * window's state and its line. */
export function missingWindow(failure: string | null): [string, string] {
  return failure && failure !== API_KEY_ACCOUNT
    ? ["Not read", "The last read failed, for the reason above."]
    : ["Not reported", "The provider hasn't reported this window."];
}

/** An agent's name in the tables. Inputs: the engine's agent key and the project namer.
 * Output: "Agent 3", "Research harbor", "Master, Harbor Notes", "Onboarding" or the key. */
export function agentLabel(key: string, project: (key: string) => string = (key) => key): string {
  const team = /^team-(\d+)$/.exec(key);
  if (team) return `Agent ${team[1]}`;
  const research = /^research-(.+)$/.exec(key);
  if (research) return `Research ${research[1]}`;
  const master = /^master-(.+)$/.exec(key);
  if (master) return `Master, ${project(master[1])}`;
  if (key === "master") return "Master";
  return key === "onboarding" ? "Onboarding" : key;
}

/** Names a project or account row, the empty key being runs tied to none. Inputs: the
 * names by key and the key. Output: the name, "Unattributed" or the key itself. */
export function rowName(names: Map<string, string>, key: string): string {
  return key === "" ? "Unattributed" : names.get(key) ?? key;
}

/** Reads a manual reset typed as local date and time. Inputs: the field's value and now.
 * Output: the time in milliseconds, or the reason it can't be used. */
export function manualReset(value: string, nowMs: number): { kind: "ok"; at: number } | { kind: "invalid"; reason: string } {
  const at = new Date(value).getTime();
  if (!value || !Number.isFinite(at)) return { kind: "invalid", reason: "Pick a date and time." };
  if (at <= nowMs) return { kind: "invalid", reason: "Pick a time in the future." };
  return { kind: "ok", at };
}

/** The plain line a reading shows when a scan left records out. Input: how many agent homes
 * held more records than one scan reads. Output: the line, or null when every home was read
 * whole. */
export function partialLine(homes: number): string | null {
  if (!(homes > 0)) return null;
  return homes === 1
    ? "Some usage may be missing: 1 agent home holds more records than one scan reads, so its least recently changed records are not counted."
    : `Some usage may be missing: ${homes} agent homes hold more records than one scan reads, so their least recently changed records are not counted.`;
}

/** A warning that a reading is partial, shown only when a scan left records out. Input: how
 * many agent homes held more records than one scan reads. Output: the notice, or nothing. */
export function PartialNotice({ homes }: { homes: number }) {
  const line = partialLine(homes);
  return line === null ? null : <Notice tone="warn">{line}</Notice>;
}

/** The tokens one row shows: the count, the count and the harness that also ran for it
 * without reporting usage, or only that harness when nothing was counted, never a bare zero.
 * Inputs: the token count and that harness, if any. Output: the text. */
export function tokenText(total: number, notReportedBy?: string | null): string {
  if (!notReportedBy) return compactCount(total);
  if (total === 0) return `Usage not reported by ${notReportedBy}`;
  return `${compactCount(total)} + usage not reported by ${notReportedBy}`;
}

