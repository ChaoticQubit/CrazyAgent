import assert from "node:assert/strict";

/** How long one team step of trial 3 (team tree, question, changes) may wait. The team steps
 * share one deadline that starts at the first of them, so a team that never forms costs one
 * team wait, and none runs once a step the team needs failed: add-project always, and
 * send-prompt in live mode, where the master builds the team from the prompt (a fixture seeds
 * its own team). Input: { mode, failed slugs, teamBy or null, now, teamMs, floorMs }.
 * Output: { wait, teamBy }; throws "not run: <steps> failed". */
export function teamWait({ mode, failed, teamBy, now, teamMs, floorMs }) {
  assert(mode === "live" || mode === "fixture", "the mode is live or fixture");
  assert(Array.isArray(failed), "the failed steps are a list");
  assert(Number.isFinite(now) && Number.isFinite(teamMs) && Number.isFinite(floorMs), "the times are numbers");
  assert(floorMs > 0 && floorMs <= teamMs, "the floor is positive and within the team time");
  const missing = (mode === "live" ? ["add-project", "send-prompt"] : ["add-project"]).filter((slug) => failed.includes(slug));
  if (missing.length > 0) throw new Error(`not run: ${missing.join(" and ")} failed`);
  const by = teamBy ?? now + teamMs;
  assert(by <= now + teamMs, "the shared deadline never moves later");
  return { wait: Math.max(by - now, floorMs), teamBy: by };
}
