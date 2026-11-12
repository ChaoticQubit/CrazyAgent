import type { AccountStanding, AccountStrategy, OrderWhy, ProjectHold } from "../../api";
import type { AccountLimits, LimitWindow } from "../../api-usage";
import { nearLimit } from "../metrics/limits";
import { windowNow } from "../metrics/usage-model";
import { atText, STRATEGIES } from "./account-choice";

/** The line above the held accounts. */
export const HELD_HEAD = "Held now, in pool order. Each one rejoins the order when its reason ends.";

const WHY: { [K in OrderWhy["kind"]]: (why: Extract<OrderWhy, { kind: K }>, nowMs: number) => string } = {
  use_last: () => "Pinned to Use last, so it goes after accounts that are not.",
  limit_reported: () => "A run just reported it at a limit, so it goes after accounts that have room.",
  no_reading: () => "Its weekly use or reset is not known yet, so it goes after accounts where it is.",
  resets_soonest: (why, nowMs) => `Its weekly limit resets soonest, ${atText(why.resets_ms, nowMs)}.`,
  resets_later: (why, nowMs) => `Its weekly limit resets later, ${atText(why.resets_ms, nowMs)}.`,
  least_used: () => "Least weekly use of the accounts ready now.",
  more_used: () => "More weekly use than the one above.",
  more_five_hour_used: () => "Same weekly use as the one above, more of its 5 hours used.",
  ranked: (why) => `You ranked it ${why.rank}.`,
  unranked: () => "No rank, so it goes after ranked accounts.",
  tied_rank: (why) => why.rank === null ? "Same as the one above, and it has no rank." : `Same as the one above, and you ranked it ${why.rank}.`,
  tied_order: () => "Same as the one above, so it keeps its pool order.",
};

const HOW_SET: Record<AccountStrategy, string> = {
  use_it_or_lose_it: "It follows live usage, so rows cannot be dragged.",
  manual_rank: "Set a rank under Schedule to move a row. Rows cannot be dragged.",
  balanced: "Rank only breaks ties. It follows live usage, so rows cannot be dragged.",
};

/** One usage bar on an account row: its name, how far it fills, its words and its tone. */
export interface UsageBar {
  name: "Weekly" | "5-hour";
  percent: number;
  text: string;
  tone: "ok" | "near" | "full";
}

/** Says why a ready account stands where it does. Inputs: the engine's reason and now.
 * Output: the sentence; throws for a reason with no words, so a row is never blank. */
export function whyLine(why: OrderWhy, nowMs: number): string {
  const line = WHY[why.kind] as ((why: OrderWhy, nowMs: number) => string) | undefined;
  if (line === undefined) throw new Error(`no words for the order reason ${why.kind}`);
  return line(why, nowMs);
}

/** Says why an account is held. Inputs: its standing and now. Output: the sentence; throws
 * for a ready account, which is not held. */
export function heldLine(standing: AccountStanding, nowMs: number): string {
  const at = (ms: number) => atText(ms, nowMs);
  switch (standing.kind) {
    case "next": throw new Error("a ready account is not held");
    case "not_signed_in": return "Not signed in. Agents skip it until you sign in again.";
    case "sign_in_unconfirmed": return "Its sign-in needs a check before agents can use it.";
    case "paused": return `Paused until ${at(standing.until_ms)}`;
    case "never_use": return "Never use. You pinned it.";
    case "five_hour_cap": return standing.until_ms === null ? "5-hour cap reached" : `5-hour cap until ${at(standing.until_ms)}`;
    case "reserve_reached":
      if (standing.reserve_percent > 0) return `Reserve reached (${standing.reserve_percent}%)`;
      return standing.until_ms === null ? "Weekly limit used up" : `Weekly limit used up until ${at(standing.until_ms)}`;
  }
}

/** Says which project holds an account after a limit its own agent reported, and until when.
 * Inputs: the hold and now. Output: the line; throws for a hold with no name, so a line never
 * reads "Held for  until". */
export function holdLine(hold: ProjectHold, nowMs: number): string {
  if (hold.name.length === 0) throw new Error(`the hold on ${hold.project} has no name`);
  return `Held for ${hold.name} until ${atText(hold.until_ms, nowMs)}`;
}

/** Says where a pool's order comes from and how a row can move. Inputs: the strategy, whether
 * the pool set it itself and how many of its projects set their own. Output: the line; throws
 * for a strategy with no words or a count that is not a whole number from 0. */
export function orderLine(strategy: AccountStrategy, own: boolean, projects: number): string {
  const choice = STRATEGIES.find((item) => item.value === strategy);
  if (choice === undefined) throw new Error(`no words for the order ${strategy}`);
  if (!Number.isInteger(projects) || projects < 0) throw new Error("a project count is a whole number from 0");
  const hint = choice.hint.charAt(0).toLowerCase() + choice.hint.slice(1);
  const line = `Order comes from ${choice.label}, ${own ? "this pool" : "the default for every pool"}: ${hint} ${HOW_SET[strategy]}`;
  if (projects === 0) return line;
  return `${line} ${projects === 1 ? "1 project using this pool sets its own order." : `${projects} projects using this pool set their own order.`}`;
}

/** Turns one window as it stands now into its bar. Inputs: the bar's name, the window, the
 * word before its reset time and now. Output: the bar. */
function bar(name: UsageBar["name"], window: LimitWindow, resets: string, nowMs: number): UsageBar {
  const used = `${Math.round(window.used_percent)}% used`;
  return {
    name,
    percent: Math.min(100, window.used_percent),
    text: window.resets_at_ms === null ? used : `${used}, ${resets} ${atText(window.resets_at_ms, nowMs)}`,
    tone: window.used_percent >= 100 ? "full" : nearLimit(window.used_percent) ? "near" : "ok",
  };
}

/** Reads the windows of an account as they stand now, with the owner's reset applied and a
 * passed reset counted as reset. Inputs: its limits and now. Output: weekly and 5-hour. */
function windows(limits: AccountLimits, nowMs: number): [LimitWindow | null, LimitWindow | null] {
  return [
    windowNow(limits.weekly, limits.weekly_reset_override_ms, nowMs),
    windowNow(limits.five_hour, limits.five_hour_reset_override_ms, nowMs),
  ];
}

/** Reads an account's usage bars, weekly first. Inputs: its limits and now. Output: one bar
 * per window it has a reading for. */
export function usageBars(limits: AccountLimits, nowMs: number): UsageBar[] {
  const [weekly, fiveHour] = windows(limits, nowMs);
  return [
    ...(weekly ? [bar("Weekly", weekly, "resets", nowMs)] : []),
    ...(fiveHour ? [bar("5-hour", fiveHour, "until", nowMs)] : []),
  ];
}

/** Says whether an account is near its limit: any window, as it stands now, at or past the
 * near line and not yet full. Inputs: its limits and now. Output: a boolean. */
export function isNear(limits: AccountLimits, nowMs: number): boolean {
  return windows(limits, nowMs).some((window) => window !== null && window.used_percent < 100 && nearLimit(window.used_percent));
}
