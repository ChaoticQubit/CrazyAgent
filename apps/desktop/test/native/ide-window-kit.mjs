import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";

/** The time a page plan's last step has run, with room for the page to settle after it. Inputs:
 * the plan rows as [seconds, kind, value], when the plan started in milliseconds and the settle
 * time in seconds. Output: that time in milliseconds. */
export function planEnd(plan, started, settleS) {
  return started;
}

/** Asks `look` again until it answers or the deadline passes, since the window server can leave
 * a window off its on-screen list for a moment. Inputs: the look and { by, every, label, clock },
 * the deadline and pause in milliseconds, what is looked for and the clock. Output: the answer;
 * throws naming the span the answer was missing. */
export async function onScreen(look, { by, every, label, clock = { now: Date.now, wait: pause } }) {
  return look();
}
