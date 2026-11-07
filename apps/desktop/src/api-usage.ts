import { call, knownError, namedFailure } from "./api";
import type { Result } from "./api";

export interface Tokens {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
}

export interface UsageRow {
  key: string;
  tokens: Tokens;
  runs: number;
  last_ms: number;
  not_reported_by?: string | null;
}

export interface DayTokens {
  day_ms: number;
  tokens: Tokens;
}

export interface LinesRow {
  project: string;
  change_sets: number;
  added: number;
  removed: number;
  commits: number;
  committed_added: number;
  committed_removed: number;
}

export interface AgentLinesRow {
  agent: string;
  change_sets: number;
  added: number;
  removed: number;
  commits: number;
  committed_added: number;
  committed_removed: number;
}

export interface LimitWindow {
  used_percent: number;
  resets_at_ms: number | null;
}

export type WindowKind = "five_hour" | "weekly";

export type UsageStep = "poll" | "team" | "runs";

export interface StepHealth {
  step: UsageStep;
  panics: number;
  last_ms: number;
  recovered_ms: number | null;
}

export interface AccountLimits {
  account: string;
  plan: string | null;
  five_hour: LimitWindow | null;
  weekly: LimitWindow | null;
  five_hour_reset_override_ms: number | null;
  weekly_reset_override_ms: number | null;
  source: "poll" | "transcript" | null;
  observed_ms: number | null;
  checked_ms: number | null;
  failure: string | null;
  five_hour_left_out: "claude" | "codex" | null;
  weekly_left_out: "claude" | "codex" | null;
  five_hour_left_out_label: string | null;
  weekly_left_out_label: string | null;
}

export interface UsageReport {
  since_ms: number | null;
  total: Tokens;
  runs: number;
  projects: UsageRow[];
  pools: UsageRow[];
  accounts: UsageRow[];
  agents: UsageRow[];
  days: DayTokens[];
  lines: LinesRow[];
  agent_lines: AgentLinesRow[];
  limits: AccountLimits[];
  not_reported_by?: string[];
  health?: StepHealth[];
  partial_homes: number;
}

export interface MetricsBucket {
  start_ms: number;
  tokens: Tokens;
}

export interface MetricsRun {
  project: string;
  agent: string;
  session: string;
  task: number | null;
  title: string | null;
  status: string | null;
  last_ms: number;
  tokens: Tokens;
}

export interface MetricsGroup {
  key: string;
  tokens: Tokens;
  agents: number;
  runs: number;
}

export interface Metrics {
  running: number;
  tasks_waiting: number;
  tasks_done: number;
  tasks_failed: number;
  bucket_ms: number;
  series: MetricsBucket[];
  runs: MetricsRun[];
  accounts: MetricsGroup[];
  harnesses: MetricsGroup[];
}

const REFUSALS: Record<string, string> = {
  usage_limits_invalid: "That reset time isn't valid. Pick a time in the future.",
  usage_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the page before you try again.",
  usage_request_invalid: "The engine didn't accept this usage request. Reload the page and try again.",
  usage_unavailable: "Usage isn't available in this engine yet.",
};

const POLL_FAILURES: Record<string, string> = {
  usage_sign_in_lapsed: "The provider no longer accepts this sign-in. Sign in again in Accounts.",
  usage_sign_in_rejected: "This account's sign-in was rejected. Sign in again in Accounts.",
  usage_rate_limited: "The provider asked CrazyAgent to wait before reading limits again.",
  usage_poll_failed: "The provider didn't return limits. CrazyAgent tries again in 10 minutes.",
  usage_unreachable: "CrazyAgent couldn't reach the provider. It tries again in 10 minutes.",
  usage_reply_invalid: "The provider's answer wasn't one CrazyAgent can read.",
  usage_request_invalid: "CrazyAgent couldn't ask the provider for this account's limits.",
  usage_step_panicked: "CrazyAgent's usage check hit an internal error. It tries again on its own.",
  native_profile_codex_api_key: "An API-key account has no 5-hour or weekly window.",
  native_profile_codex_token_unreadable: "CrazyAgent can't read this Codex sign-in. Sign in to Codex again in Accounts.",
  native_profile_codex_sign_in_expired: "This Codex sign-in expired and CrazyAgent couldn't renew it. Sign in to Codex again in Accounts.",
  native_profile_credential_unavailable: "CrazyAgent can't read this account's sign-in. Sign in again in Accounts.",
  native_profile_renewal_failed: "CrazyAgent couldn't renew this account's sign-in. It tries again in 10 minutes.",
  native_profile_renewal_busy: "Another check of this account's sign-in is running. CrazyAgent tries again in 10 minutes.",
  native_profile_sign_in_revoked: "The provider ended this sign-in. Sign in again in Accounts.",
};

/** The refusal codes of the usage protocol that have their own sentence. */
export const USAGE_REFUSALS = Object.keys(REFUSALS);

/** The failed-poll codes an account's limits can carry that have their own sentence. */
export const USAGE_POLL_FAILURES = Object.keys(POLL_FAILURES);

/** One plain sentence for a refused usage request. Input: the refusal code and its engine
 * cause. Output: the sentence, or the plain fallback naming the code. */
export function usageRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** One plain sentence for the last failed read of an account's limits. Input: the stored
 * code. Output: the sentence, or the plain fallback naming the code. */
export function pollFailure(code: string): string {
  return namedFailure(Object.hasOwn(POLL_FAILURES, code) ? POLL_FAILURES[code] : undefined, code);
}

/** Tells whether a usage answer means the page is not ready: this build has no usage
 * commands, or the engine answered usage_unavailable. Input: the answer. */
export function usageNotReady<T>(result: Result<T>): boolean {
  return result.kind === "not_ready" || (result.kind === "error" && result.code === "usage_unavailable");
}

/** Reads the usage page's numbers for every project, or for one project when its id is
 * given. Inputs: the range start in milliseconds, null for all time, the viewer's offset east
 * of UTC in minutes and the project id. Output: the report. */
export function loadUsage(sinceMs: number | null, offsetMinutes: number, projectId: string | null = null): Promise<Result<UsageReport>> {
  return call<UsageReport>("desktop_usage_report", { projectId, sinceMs, offsetMinutes });
}

/** Reads the Metrics page's task counts, tokens chart and run, account and harness rows for
 * one project, or every project when the id is null. Inputs: the range start in milliseconds
 * and the project id. Output: the metrics. */
export function loadMetrics(sinceMs: number, projectId: string | null): Promise<Result<Metrics>> {
  return call<Metrics>("desktop_usage_metrics", { projectId, sinceMs });
}

/** Asks the engine to read every signed-in account's limits now. */
export function refreshUsage(): Promise<Result<void>> {
  return call<void>("desktop_usage_refresh");
}

/** Sets or clears the owner's manual reset time of one account's window. Inputs: the
 * account id, the window and the time in milliseconds, null to clear it. */
export function setManualReset(account: string, windowKind: WindowKind, atMs: number | null): Promise<Result<void>> {
  return call<void>("desktop_usage_set_reset", { account, windowKind, atMs });
}
