import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";

export type ChangeState = "pending" | "conflict" | "updating" | "accepted" | "discarded";
export type TurnOutcome = "succeeded" | "failed" | "cancelled";

export interface ChangeSet {
  id: string;
  agent: { kind: "master" | "sub"; id: string; label: string };
  state: ChangeState;
  base: string;
  head: string;
  files: number;
  added: number;
  removed: number;
  last_outcome: TurnOutcome;
  checked: boolean;
  created_ms: number;
  updated_ms: number;
  restorable_until_ms: number | null;
  conflicts: string[];
}

export type FileStatus = "added" | "modified" | "deleted" | "renamed";
export type FileKind = "text" | "binary" | "symlink" | "gitlink";

export interface ChangedFile {
  path: string;
  old_path?: string | null;
  status: FileStatus;
  kind: FileKind;
  marks: string[];
}

export interface ChangeDiff {
  files: ChangedFile[];
  text: string;
  next: number | null;
  truncated: boolean;
}

export type AcceptOutcome = { kind: "landed"; commit: string; branch: string } | { kind: "conflict"; files: string[] };

export interface ChangesRefusal {
  code: string;
  cause?: string;
  files?: string[];
}

const CONFLICT_SHOWN = 5;

const REFUSALS: Record<string, string> = {
  changes_owner_dirty: "Your project folder has uncommitted changes to tracked files. Commit or stash them, then accept again.",
  changes_detached_head: "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again.",
  changes_git_busy: "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again.",
  changes_conflict: "These changes conflict with your branch. Ask the agent to update, or discard.",
  changes_stale: "These changes moved on since you opened them. Review them again, then accept.",
  changes_branch_moved: "Your branch moved while these changes were landing, so nothing changed. Review and accept again.",
  changes_git_unsupported: "The Git on this computer can't land changes. Update Git, then try again.",
  changes_restore_expired: "These changes can no longer be restored. Discarded changes are kept for 7 days.",
  changes_agent_has_pending: "This agent has newer changes waiting. Accept or discard those first.",
  changes_agent_busy: "The agent is working right now. Try again when it finishes.",
  changes_unavailable: "Landing changes isn't ready in this engine yet.",
  changes_git_failed: "Git couldn't finish this step in your project folder. Try again; if it keeps failing, check the folder with Git.",
  changes_not_found: "These changes are no longer there. The list shows what is left.",
  changes_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the list before you try again.",
  changes_owner_identity_missing: "Git has no name and email for you in your project folder. Set user.name and user.email, then accept again.",
  changes_owner_unavailable: "Your project folder couldn't be read. Check that it is still there, then try again.",
  changes_too_large: "These changes are larger than CrazyAgent can show or land. Ask the agent to split them up.",
  changes_workspace_invalid: "The agent's working copy was moved or changed outside CrazyAgent, so these changes can't land from it.",
};

/** The refusal codes of the changes protocol that have their own sentence. */
export const CHANGES_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused Changes action; a conflict names up to five of its
 * files. Input: the refusal code, its engine cause and conflicting files. Output: the
 * sentence, or the plain fallback naming the code. */
export function changesRefusal(refusal: ChangesRefusal): string {
  const files = refusal.files ?? [];
  if (refusal.code === "changes_conflict" && files.length > 0) {
    const shown = files.slice(0, CONFLICT_SHOWN).join(", ");
    const more = files.length > CONFLICT_SHOWN ? ` and ${files.length - CONFLICT_SHOWN} more` : "";
    return `These files also changed on your branch: ${shown}${more}. Ask the agent to update, or discard.`;
  }
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** Tells whether a Changes answer means the view is not ready yet: this build has no
 * changes commands, or the engine answered changes_unavailable. Input: the answer. */
export function notReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "changes_unavailable");
}

/** Lists a project's change sets, newest first. Input: the project id. Output: each
 * change set with its agent, state and size. */
export function loadChanges(projectId: string): Promise<Result<ChangeSet[]>> {
  return call<ChangeSet[]>("desktop_changes", { projectId });
}

/** Reads one page of a change set's diff. Inputs: the change set id and the page, from 0.
 * Output: that page's files with marks, its plain diff text and the next page, if any. */
export function loadChangeDiff(id: string, page: number): Promise<Result<ChangeDiff>> {
  return call<ChangeDiff>("desktop_change_diff", { id, page });
}

/** Lands a change set as one commit on the owner's checked-out branch. Inputs: the id and
 * the head the owner reviewed. Output: the new commit and its branch, or the files that
 * conflict with the branch, in which case nothing changed. */
export function acceptChange(id: string, expectedHead: string): Promise<Result<AcceptOutcome>> {
  return call<AcceptOutcome>("desktop_accept_change", { id, expectedHead });
}

/** Discards a change set; it stays restorable for 7 days. Inputs: the id and the head the
 * owner reviewed. Output: when it can no longer be restored, in milliseconds. */
export function discardChange(id: string, expectedHead: string): Promise<Result<number>> {
  return call<number>("desktop_discard_change", { id, expectedHead });
}

/** Restores a discarded change set to its agent. Input: the id. */
export function restoreChange(id: string): Promise<Result<void>> {
  return call<void>("desktop_restore_change", { id });
}

/** Asks the agent to update its changes onto the owner's branch and resolve the
 * conflicts. Input: the id. */
export function updateChange(id: string): Promise<Result<void>> {
  return call<void>("desktop_update_change", { id });
}
