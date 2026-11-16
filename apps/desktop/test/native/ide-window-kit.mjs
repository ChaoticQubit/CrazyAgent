import assert from "node:assert/strict";
import { setTimeout as pause } from "node:timers/promises";

/** The time a page plan's last step has run, with room for the page to settle after it. Inputs:
 * the plan rows as [seconds, kind, value], when the plan started in milliseconds and the settle
 * time in seconds. Output: that time in milliseconds. */
export function planEnd(plan, started, settleS) {
  assert.ok(Array.isArray(plan) && plan.length > 0, "a plan has steps");
  assert.ok(Number.isFinite(started) && Number.isFinite(settleS) && settleS >= 0, `a plan starts at a real time and settles for a real time: ${started}, ${settleS}`);
  const last = Math.max(...plan.map(([at]) => at));
  assert.ok(Number.isFinite(last) && last >= 0, `every plan step has a time: ${last}`);
  const end = started + (last + settleS) * 1000;
  assert.ok(end >= started, "a plan ends after it starts");
  return end;
}

/** Asks `look` again until it answers or the deadline passes, since the window server can leave
 * a window off its on-screen list for a moment. Inputs: the look and { by, every, label, clock },
 * the deadline and pause in milliseconds, what is looked for and the clock. Output: the answer;
 * throws naming the span the answer was missing. */
export async function onScreen(look, { by, every, label, clock = { now: Date.now, wait: pause } }) {
  assert.ok(Number.isFinite(by) && Number.isFinite(every) && every > 0, `a look has a deadline and a pause: ${by}, ${every}`);
  const first = clock.now();
  for (;;) {
    const answer = look();
    if (answer) return answer;
    const now = clock.now();
    assert.ok(now < by, `${label} is off the window server's on-screen list from ${new Date(first).toISOString()} to ${new Date(now).toISOString()}, as on another Space, hidden or in a locked session; the app's own rows say whether the app moved it`);
    const step = Math.min(every, by - now);
    assert.ok(step > 0, "each pause moves toward the deadline");
    await clock.wait(step);
  }
}
