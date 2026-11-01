/** The shipped wake rule, as the engine's `WakeRule::default` sends it (team/events.rs). */
const SHIPPED_WAKE = { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 };

/** A team status read of project alpha in the shape the live master view reads: every field
 * the desktop TeamStatus type requires, `events` with the engine's shipped wake rule included,
 * for a team that has run nothing yet. Input: the fields to change. Output: the status. */
export function teamStatus(patch = {}) {
  return {
    project_id: "alpha", keep_warm: false, paused: false, min_agents: 0, max_agents: 1,
    harnesses: [], agents: [], tasks: [], messages: [], plan: [],
    events: { recent: [], waiting: [], notifications: [], paused: false, questions: [], wake: { app: null, project: null, agents: [], effective: SHIPPED_WAKE, level: "shipped" } },
    ...patch,
  };
}
