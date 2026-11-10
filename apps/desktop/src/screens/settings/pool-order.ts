import type { AccountStanding, AccountStrategy, OrderWhy } from "../../api";
import type { AccountLimits } from "../../api-usage";

/** The line above the held accounts. */
export const HELD_HEAD = "";

/** One usage bar on an account row. */
export interface UsageBar {
  name: "Weekly" | "5-hour";
  percent: number;
  text: string;
  tone: "ok" | "near" | "full";
}

/** Says why a ready account stands where it does. Inputs: the reason and now. Output: the line. */
export function whyLine(_why: OrderWhy, _nowMs: number): string {
  return "";
}

/** Says why an account is held. Inputs: its standing and now. Output: the line. */
export function heldLine(_standing: AccountStanding, _nowMs: number): string {
  return "";
}

/** Says where a pool's order comes from. Inputs: the strategy, whether the pool set it and how
 * many of its projects set their own. Output: the line. */
export function orderLine(_strategy: AccountStrategy, _own: boolean, _projects: number): string {
  return "";
}

/** Reads an account's usage bars. Inputs: its limits and now. Output: the bars. */
export function usageBars(_limits: AccountLimits, _nowMs: number): UsageBar[] {
  return [];
}

/** Says whether an account is near a limit. Inputs: its limits and now. Output: a boolean. */
export function isNear(_limits: AccountLimits, _nowMs: number): boolean {
  return false;
}
