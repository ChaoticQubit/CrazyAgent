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
  cost_nanos: number;
  runs: number;
  last_ms: number;
  not_reported_by?: string | null;
  project?: string | null;
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

export type UsageStep = "poll" | "team" | "runs" | "recount";

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

/** The kind of limit one free reset clears, when the provider says. */
export type ResetKind = "five_hour" | "weekly" | "model" | "all_limits";

/** How an account's free resets, credits and spend are read: by its harness, its key
 * provider's balance call, or not at all. */
export type LimitAdapter = "codex" | "claude" | "open_router" | "deep_seek" | "moonshot" | "not_reported";

/** An account's free resets: how many, the kinds the provider named and the next expiry. */
export interface FreeResets {
  count: number;
  kinds: ResetKind[];
  next_expiry_ms: number | null;
}

/** Where the owner spends a free reset or credit. */
export type SpendWay = { kind: "in_app" } | { kind: "elsewhere"; place: string } | { kind: "not_reported" };

/** The engine's limits contract for one account (DEC-154): null resets or credits mean not
 * reported, never 0. `resets_gained_ms` is when the count last rose, for the new-reset notice. */
export interface LimitExtras {
  harness: string;
  adapter: LimitAdapter;
  may_break: boolean;
  resets: FreeResets | null;
  credits: string | null;
  spend: SpendWay;
  not_reported: string;
  resets_checked_ms: number | null;
  credits_checked_ms: number | null;
  resets_gained_ms?: number | null;
}

/** One account's limits contract, by account id. */
export interface AccountExtras {
  account: string;
  extras: LimitExtras;
}

export interface MoneySettings {
  main_number: "dollars" | "tokens";
  prices: "bundled" | "off";
  project_limits?: "hidden" | "shown";
}

export interface MoneyView {
  settings: MoneySettings;
  shown: "dollars" | "tokens";
  as_of: string | null;
  cost_nanos: number;
  unpriced_tokens: number;
}

/** The one comparison the owner sees: totals as they stand and as they would be, and how many
 * sessions each side covers. */
export interface RecountStatus {
  old: Tokens;
  new: Tokens;
  new_cost_nanos: number;
  sessions_recounted: number;
  sessions_kept_old: number;
  homes_partial: number;
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
  money: MoneyView;
  recount?: RecountStatus | null;
  counting_notice?: string | null;
  spend?: SpendView;
}

/** How the app spends a free reset: after a confirm each time, at one click, or never. */
export type SpendConfirm = "confirm_each" | "one_click" | "off";

/** What the app does when every account in a pool is limited. */
export type AllLimited = "notice" | "no_hint" | "auto_spend";

/** The owner's two spend settings (DEC-154 slice 3). */
export interface SpendSettings {
  confirm: SpendConfirm;
  all_limited: AllLimited;
}

/** One account's latest spend: who asked, where it stands and its outcome code. */
export interface SpendAttempt {
  account: string;
  origin: "owner" | "auto";
  state: "queued" | "sent" | "settled";
  outcome: string | null;
  requested_ms: number;
  settled_ms: number | null;
}

/** A pool whose every account was read at its limit, with the accounts holding a free reset. */
export interface LimitedPool {
  pool: string;
  name: string | null;
  since_ms: number;
  with_resets: string[];
}

/** The report's spend part: the settings, each account's latest spend and the limited pools. */
export interface SpendView {
  settings: SpendSettings;
  attempts: SpendAttempt[];
  limited_pools: LimitedPool[];
}

/** The settings an engine that never stored any reads as. */
export const SPEND_DEFAULTS: SpendSettings = { confirm: "confirm_each", all_limited: "notice" };

export interface MetricsBucket {
  start_ms: number;
  tokens: Tokens;
  cost_nanos: number;
}

export interface MetricsRun {
  project: string;
  agent: string;
  label: string | null;
  session: string;
  task: number | null;
  title: string | null;
  status: string | null;
  last_ms: number;
  tokens: Tokens;
  cost_nanos: number;
}

export interface MetricsGroup {
  key: string;
  tokens: Tokens;
  cost_nanos: number;
  agents: number;
  runs: number;
  projects: string[];
  more_projects: number;
  merges: number;
}

export interface MetricsAgent {
  project: string;
  agent: string;
  label: string | null;
  harnesses: string[];
  more_harnesses: number;
  accounts: string[];
  more_accounts: number;
  tokens: Tokens;
  cost_nanos: number;
  turns?: number | null;
}

export interface ModelRow {
  model: string;
  tokens: Tokens;
  cost_nanos: number;
}

export interface ToolBucket {
  start_ms: number;
  tool: string;
  count: number;
}

export interface ActivityRow {
  activity: string;
  turns: number;
  edit_turns: number;
  first_try_turns: number;
}

export interface Metrics {
  running: number;
  tasks_waiting: number;
  tasks_done: number;
  tasks_failed: number;
  merges: number;
  merges_into_main: number;
  merges_undone: number;
  bucket_ms: number;
  series: MetricsBucket[];
  runs: MetricsRun[];
  projects: MetricsGroup[];
  agents: MetricsAgent[];
  accounts: MetricsGroup[];
  harnesses: MetricsGroup[];
  models: ModelRow[];
  tools: ToolBucket[];
  tools_since_ms: number | null;
  activity: ActivityRow[];
  activity_since_ms: number | null;
}

const REFUSALS: Record<string, string> = {
  usage_limits_invalid: "That reset time isn't valid. Pick a time in the future.",
  usage_recount_not_ready: "This recount was already decided. Reload the page.",
  usage_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the page before you try again.",
  usage_request_invalid: "The engine didn't accept this usage request. Reload the page and try again.",
  usage_spend_off: "Spending free resets is off. Turn it on in Settings, Pools.",
  usage_spend_unconfirmed: "Spend confirmation now asks first. Use a free reset again and confirm it.",
  usage_unavailable: "Usage isn't available in this engine yet.",
  owner_busy: "CrazyAgent is busy with other requests. Try again in a moment.",
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

const SPEND_OUTCOMES: Record<string, string> = {
  reset: "Free reset used. The limits update within a minute.",
  nothing_to_reset: "Nothing to reset: no limit on this account is full now. The free reset is kept.",
  no_credit: "This account has no free reset left.",
  already_redeemed: "This free reset was already used.",
  outcome_unknown: "The provider didn't say whether the reset was used. Check the limits after the next refresh.",
  usage_spend_off: "Spending free resets is off. Turn it on in Settings, Pools.",
  usage_spend_no_account: "This account is no longer in your settings.",
  usage_spend_not_in_app: "This account can't spend a free reset from CrazyAgent.",
  usage_spend_not_signed_in: "This account wasn't signed in or was busy. Sign in again in Accounts, then try again.",
  usage_spend_unavailable: "Spending this free reset from the app isn't available yet. Use the provider's own app.",
  usage_spend_failed: "The free reset couldn't be sent. Try again later.",
  usage_spend_refused: "The provider turned this free reset down. None was used.",
  usage_sign_in_lapsed: "The provider no longer accepts this sign-in. Sign in again in Accounts.",
  usage_sign_in_rejected: "This account's sign-in was rejected. Sign in again in Accounts.",
  usage_rate_limited: "The provider asked CrazyAgent to wait. Try again in a few minutes.",
  usage_request_invalid: "CrazyAgent couldn't ask the provider to use a free reset.",
  usage_reply_invalid: "The provider's answer wasn't one CrazyAgent can read. Check the limits after the next refresh.",
};

/** The spend outcome codes that have their own sentence. */
export const SPEND_OUTCOMES_KNOWN = Object.keys(SPEND_OUTCOMES);

/** One plain sentence for where an account's latest spend stands. Input: the attempt.
 * Output: the sentence, or the plain fallback naming the code. */
export function spendOutcome(attempt: SpendAttempt): string {
  if (attempt.state === "queued") return "Using a free reset now.";
  if (attempt.state === "sent") return "Sent. Waiting for the provider's answer.";
  const code = attempt.outcome ?? "outcome_unknown";
  return namedFailure(Object.hasOwn(SPEND_OUTCOMES, code) ? SPEND_OUTCOMES[code] : undefined, code);
}

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

/** Reads every account's free resets, credits and spend as the engine stored them. Pools
 * passes `detail` on each reread, which asks the engine to read the detail rows while Pools
 * is open; every other caller reads only what is stored. Input: whether Pools is open.
 * Output: one entry per account. */
export function loadExtras(detail: boolean): Promise<Result<AccountExtras[]>> {
  return call<AccountExtras[]>("desktop_usage_extras", { detail });
}

/** Asks the engine to read every signed-in account's limits now. */
export function refreshUsage(): Promise<Result<void>> {
  return call<void>("desktop_usage_refresh");
}

/** Stores the owner's main number and price source. Input: the settings. Output: done. */
export function setMoney(settings: MoneySettings): Promise<Result<void>> {
  return call<void>("desktop_usage_set_money", { settings });
}

/** Applies the owner's choice of the recount, once. Input: whether to use the new numbers.
 * Output: done. */
export function decideRecount(useNew: boolean): Promise<Result<void>> {
  return call<void>("desktop_usage_decide_recount", { useNew });
}

/** Sets or clears the owner's manual reset time of one account's window. Inputs: the
 * account id, the window and the time in milliseconds, null to clear it. */
export function setManualReset(account: string, windowKind: WindowKind, atMs: number | null): Promise<Result<void>> {
  return call<void>("desktop_usage_set_reset", { account, windowKind, atMs });
}

/** Asks the engine to spend one free reset on an account; the outcome shows in the report's
 * spend view within one usage step. Input: the account id. Output: done once queued. */
export function spendReset(account: string, confirmed: boolean): Promise<Result<void>> {
  return call<void>("desktop_usage_spend", { account, confirmed });
}

/** Stores the owner's spend settings. Input: the settings. Output: done. */
export function setSpendSettings(settings: SpendSettings): Promise<Result<void>> {
  return call<void>("desktop_usage_set_spend", { settings });
}

/** Reads the notices for the code the app adapts from others. Output: the text. */
export function loadNotices(): Promise<Result<string>> {
  return call<string>("desktop_usage_notices");
}
