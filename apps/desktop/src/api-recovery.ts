import { call } from "./api";
import type { Failure, Result } from "./api";
import { failureCopy } from "./api-projects";

/** Where the resume of one interrupted run stands. */
export type RunShown = "interrupted" | "resuming" | "resumed" | "failed";

/** The ledger actions a notice can be about; only the last three are shown today. */
export type ActionKind = "message" | "dispatch" | "worktree_create" | "agent_spawn" | "landing_accept" | "project_delete";

/** One agent run an engine stop cut short, as the banner lists it. */
export interface RecoveryRun {
  agent: number;
  label: string;
  role: "master" | "sub";
  harness: string;
  at_ms: number;
  turn: number;
  asked: string | null;
  state: RunShown;
  reason: string | null;
  paused: boolean;
}

/** One ledger notice the owner dismisses: what it is about and what it shows. */
export interface RecoveryNotice {
  action_kind: ActionKind;
  id: number;
  agent: number | null;
  label: string | null;
  change_set: string | null;
  shown: "unknown" | "landed" | "dropped";
  started_ms: number;
}

/** One project's interrupted work, and how many open items did not fit. */
export interface RecoveryBanner {
  runs: RecoveryRun[];
  notices: RecoveryNotice[];
  more: number;
}

const REFUSALS: Record<string, string> = {
  recovery_not_failed: "That resume is no longer failed. The list is read again.",
  ledger_finished: "That notice was already dismissed. The list is read again.",
  ledger_missing: "That notice was already dismissed. The list is read again.",
  ledger_not_notice: "That start is still being checked; it settles on its own.",
  recovery_unavailable: "CrazyAgent couldn't read its records. Try again.",
  ledger_unavailable: "CrazyAgent couldn't read its records. Try again.",
  recovery_record_invalid: "A recovery record is damaged, so this list can't be shown.",
  recovery_outcome_unknown: "CrazyAgent couldn't confirm what happened. The list is read again.",
  owner_busy: "CrazyAgent is busy with other requests. Try again in a moment.",
};

/** One plain sentence for a refused Recovery request. Input: the failed result.
 * Output: the sentence, or the project screens' fallback naming the code. */
export function recoveryMessage(failure: Failure): string {
  if (failure.kind === "not_ready") return "Recovery isn't ready in this engine yet.";
  return Object.hasOwn(REFUSALS, failure.code) ? REFUSALS[failure.code] : failureCopy(failure);
}

/** Reads one project's interrupted work. Input: the project id. Output: the banner. */
export function loadRecovery(projectId: string): Promise<Result<RecoveryBanner>> {
  return call<RecoveryBanner>("desktop_recovery_banner", { projectId });
}

/** Starts an agent's failed resume again. Input: the team agent. Output: done once it waits. */
export function retryRecovery(agent: number): Promise<Result<null>> {
  return call<null>("desktop_retry_recovery", { agent });
}

/** Dismisses one notice the owner was shown. Input: the notice. Output: done once dismissed. */
export function dismissRecovery(notice: Pick<RecoveryNotice, "action_kind" | "id">): Promise<Result<null>> {
  return call<null>("desktop_dismiss_recovery", { actionKind: notice.action_kind, id: notice.id });
}
