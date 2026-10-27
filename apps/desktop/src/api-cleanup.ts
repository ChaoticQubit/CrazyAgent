import { call, knownError, namedFailure } from "./api";
import type { Result } from "./api";

/** A finished agent whose worktree and agent folder the owner can remove, with the bytes
 * both hold on disk; `counted_all` is false when the engine stopped counting at its bound. */
export interface FinishedAgent {
  id: number;
  role: "master" | "sub";
  label: string;
  harness: string;
  stopped_ms: number | null;
  bytes: number;
  counted_all: boolean;
}

/** What one removal did: the agents it removed, in the order asked, and the code of the agent
 * it kept when it stopped part way, or null when it removed every one. */
export interface Removal {
  agents: number[];
  kept: string | null;
}

const REFUSALS: Record<string, string> = {
  cleanup_agent_has_task: "An agent has a task waiting for it again. CrazyAgent kept it and stopped there.",
  cleanup_agent_not_found: "An agent is no longer in this project. CrazyAgent kept it and stopped there.",
  cleanup_agent_running: "An agent started again. CrazyAgent kept it and stopped there.",
  cleanup_changes_open: "An agent has changes waiting for review. CrazyAgent kept it and stopped there.",
  cleanup_changes_restorable: "An agent has discarded changes you can still restore. CrazyAgent kept it and stopped there.",
  cleanup_commits_unlanded: "An agent has commits that haven't landed. CrazyAgent kept it and stopped there.",
  cleanup_folder_failed: "The worktree is gone but CrazyAgent couldn't remove the agent's folder. Try again.",
  cleanup_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  cleanup_request_invalid: "The engine didn't accept this cleanup request. Reload the page and try again.",
  cleanup_unavailable: "Cleanup isn't available in this engine yet.",
  cleanup_worktree_dirty: "An agent's working copy has changes that aren't saved anywhere else. CrazyAgent kept it and stopped there.",
  cleanup_worktree_invalid: "An agent's working copy isn't where the engine recorded it. CrazyAgent kept it and stopped there.",
};

/** The refusal codes of the cleanup protocol that have their own sentence. */
export const CLEANUP_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused cleanup request. Input: the refusal code and its engine
 * cause. Output: the sentence, or the plain fallback naming the code. */
export function cleanupRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** Tells whether a cleanup answer means there is nothing to offer: this build has no cleanup
 * commands, or the engine answered cleanup_unavailable. Input: the answer. */
export function cleanupNotReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "cleanup_unavailable");
}

/** Reads a project's finished agents that can be removed, newest first. Input: the project id.
 * Output: the agents with their size on disk. */
export function loadFinishedAgents(projectId: string): Promise<Result<FinishedAgent[]>> {
  return call<FinishedAgent[]>("desktop_finished_agents", { projectId });
}

/** Removes the worktrees and folders of finished agents the owner confirmed; the engine checks
 * each one again first and stops at the first one that changed. Inputs: the project id and the
 * agent ids. Output: what the removal did, or the code when it removed none. */
export function removeFinishedAgents(projectId: string, agents: number[]): Promise<Result<Removal>> {
  return call<Removal>("desktop_remove_finished_agents", { projectId, agents });
}
