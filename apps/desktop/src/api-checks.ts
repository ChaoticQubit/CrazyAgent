import { call, type Failure, type Result } from "./api";
import { questionsRefusal } from "./api-questions";

/** How a finished run came out, as the trackwright core rules it. */
export const VERDICTS = ["passed", "failed", "no_tests", "build_failed", "unknown", "unavailable"] as const;
export type Verdict = typeof VERDICTS[number];

/** Where a run stands. */
export const RUN_STATES = ["running", "finished", "interrupted"] as const;
export type RunState = typeof RUN_STATES[number];

/** Why an interrupted run stopped. */
export const STOP_REASONS = ["cancelled", "timed_out"] as const;

/** How one result came out. */
export const OUTCOMES = ["passed", "failed", "skipped", "errored", "not_run"] as const;
export type Outcome = typeof OUTCOMES[number];

/** A test case's kind. */
export const CASE_KINDS = ["unit", "integration", "e2e", "smoke", "scenario", "manual"] as const;
export type CaseKind = typeof CASE_KINDS[number];

/** A test case's status. */
export const CASE_STATUSES = ["draft", "ready", "deprecated"] as const;
export type CaseStatus = typeof CASE_STATUSES[number];

/** A test plan's status. */
export const PLAN_STATUSES = ["draft", "active", "closed"] as const;
export type PlanStatus = typeof PLAN_STATUSES[number];

/** A check's network mode. */
export const NETWORK_MODES = ["off", "localhost", "remotes"] as const;
export type NetworkMode = typeof NETWORK_MODES[number];

/** A toolchain family a check runs with. */
export const TOOLCHAIN_FAMILIES = ["rust", "node", "python", "none"] as const;
export type ToolchainFamily = typeof TOOLCHAIN_FAMILIES[number];

/** The source a run covered: a git commit with its tree, base and change set, or a
 * reference declared by a standalone writer. */
export type SourceRef =
  | { kind: "git"; repository: string; commit: string; tree: string; base: string | null; change_set: string | null }
  | { kind: "declared"; ref: string };

/** The policy a run ran under: the approved profile and when, the check, its command,
 * toolchain, sandbox and environment digests, network and timeout. */
export interface PolicyRef {
  profile: string;
  profile_approved_ms: number;
  check: string;
  kind: CaseKind;
  argv: string[];
  toolchain: { family: ToolchainFamily; version: string };
  sandbox: string;
  network: NetworkMode;
  timeout_s: number;
  environment: string;
}

/** What the engine saw about the tests themselves: how many ran, the tests the baseline ran
 * that this run lost, the skip markers the change added, the protected paths it touched,
 * whether holdouts ran and whether the build succeeded. */
export interface IntegrityRef {
  executed: number;
  inventory: { baseline_run: string | null; removed: string[]; removed_total: number };
  new_skips: { path: string; line: number; marker: string }[];
  protected_touched: string[];
  holdouts_ran: boolean;
  build_ok: boolean;
}

/** A run's result counts; `total` includes results that did not run. */
export interface Totals { total: number; passed: number; failed: number; skipped: number; errored: number }

/** A run's own fields, as the first `run_show` page carries them beside its binding. */
export interface RunHeader {
  id: string;
  plan: string | null;
  batch: string | null;
  title: string;
  state: RunState;
  verdict: Verdict | null;
  reason: string | null;
  started_ms: number;
  finished_ms: number | null;
  recorded_by: string;
  totals: Totals;
  version: number;
}

/** A part of the run's binding that was too large for one reply. */
export type Cut = "cut";

/** One run as its detail page shows it: the run, what it covered (`cut` when too large to
 * show), its integrity evidence (null when none was recorded) and its unlinked names. */
export interface RunDetail {
  run: RunHeader;
  source: SourceRef | Cut;
  policy: PolicyRef | Cut;
  integrity: IntegrityRef | null | Cut;
  unlinked: string[];
  unlinked_total: number;
}

/** One stored result; `withheld` marks a protected case's message the reader may not see. */
export interface ResultView {
  position: number;
  name: string;
  outcome: Outcome;
  case: string | null;
  duration_ms: number | null;
  message: string | null;
  artifact: string | null;
  withheld?: boolean;
}

/** One page of results with the cursor for the next one. */
export interface ResultsPage { results: ResultView[]; cursor: string | null }

/** The first `run_show` page: the run's detail, its first results and the cursor. */
export interface RunPage extends ResultsPage { detail: RunDetail }

/** One row of `run_list`. */
export interface RunRow { id: string; title: string; elided: boolean; state: RunState; verdict: Verdict | null; started_ms: number; finished_ms: number | null; totals: Totals }

/** One row of `case_list`. */
export interface CaseRow { id: string; title: string; elided: boolean; protected: boolean; kind: CaseKind | null; status: CaseStatus | null; latest: Outcome | null }

/** One row of `plan_list`. */
export interface PlanRow { id: string; name: string; status: PlanStatus; items: number }

/** One page of a list with the cursor for the next one. */
export interface ListPage<T> { rows: T[]; cursor: string | null }

/** One criterion of a coverage page with its linked cases and their latest results. */
export interface CoverageRow {
  criterion: { ticket: string; position: number };
  text: string;
  state: "open" | "met" | "waived";
  cases: { case: string; title: string; proves: boolean; latest: { outcome: Outcome; run: string; verdict: Verdict } | null }[];
  cases_total: number;
}

/** One page of `coverage`. */
export interface CoveragePage { rows: CoverageRow[]; criteria_total: number; uncovered_total: number; cursor: string | null }

/** The `case_list` filters the Tests tab offers. */
export interface CaseFilter { kind: CaseKind | null; status: CaseStatus | null; set: string | null; unlinked: boolean }

/** The `run_list` filters the Tests tab offers: verdict, plan, batch and source commit. */
export interface RunFilter { verdict: Verdict | null; plan: string | null; batch: string | null; commit: string | null }

/** The reads the Tests tab makes through the owner tracker: the eight test reads and the
 * roadmap for the version names coverage is shown per. */
export const TEST_READS = ["case_list", "case_show", "set_show", "plan_list", "plan_show", "run_list", "run_show", "coverage", "roadmap"] as const;
export type TestRead = typeof TEST_READS[number];

/** What the bridge gives back for one test read: the page's data and cursor, or the code
 * the tracker refused it with. */
export type TestsReply = { outcome: "page"; data: unknown; cursor: string | null } | { outcome: "refused"; code: string };

/** The most results one `run_show` page holds. */
export const RUN_SHOW_PAGE_ROWS = 50;
/** The most rows one list page holds. */
export const LIST_ROWS_MAX = 50;
/** The most bytes of profile JSON the editor sends, the engine's input bound. */
export const PROFILE_INPUT_BYTES_MAX = 32 * 1024;
/** The value a Covered row shows for a binding part too large for one reply. */
export const CUT_TEXT = "Too large to show in one reply";

const TEST_ID = /^[A-Z]{2,8}-[TSPR][1-9][0-9]{0,9}$/;
const COMMIT = /^[0-9a-f]{40}$/;
const DIGEST = /^[0-9a-f]{64}$/;
const CURSOR_BYTES_MAX = 64;
const ARGV_ITEMS_MAX = 64;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const VERDICT_TEXT: Record<Verdict, { label: string; line: string }> = {
  passed: { label: "Passed", line: "Every test that ran passed." },
  failed: { label: "Failed", line: "At least one test failed or errored." },
  no_tests: { label: "No tests ran", line: "The check finished without running a single test, so it proves nothing." },
  build_failed: { label: "Build failed", line: "The build failed before any test ran." },
  unknown: { label: "Result unclear", line: "The report could not be read, or it does not match how the check exited." },
  unavailable: { label: "Couldn't run", line: "The check could not start." },
};

/** A verdict's label and the line saying what it means. Input: the verdict. Output: both. */
export function verdictText(verdict: Verdict): { label: string; line: string } {
  assert(Object.hasOwn(VERDICT_TEXT, verdict), `${verdict} is a verdict`);
  return VERDICT_TEXT[verdict];
}

/** A run's status in a word or two: its verdict once finished, Running while it runs, and for
 * an interrupted run why it stopped. Input: the state, verdict and reason. Output: the label. */
export function runLabel(run: Pick<RunHeader, "state" | "verdict" | "reason">): string {
  if (run.state === "running") return "Running";
  if (run.state === "interrupted") return run.reason === "timed_out" ? "Timed out" : run.reason === "cancelled" ? "Cancelled" : "Stopped";
  assert(run.verdict !== null, "a finished run has a verdict");
  return verdictText(run.verdict).label;
}

/** The tone a run's status shows in. Input: the run. Output: the tone. */
export function runTone(run: Pick<RunHeader, "state" | "verdict">): "ok" | "danger" | "warn" | "neutral" {
  if (run.state === "running") return "neutral";
  if (run.state === "interrupted" || run.verdict === null) return "warn";
  if (run.verdict === "passed") return "ok";
  return run.verdict === "failed" || run.verdict === "build_failed" ? "danger" : "warn";
}

const OUTCOME_LABEL: Record<Outcome, string> = { passed: "Passed", failed: "Failed", skipped: "Skipped", errored: "Errored", not_run: "Not run" };

/** A result's outcome in a word. Input: the outcome. Output: the label. */
export function outcomeLabel(outcome: Outcome): string {
  assert(Object.hasOwn(OUTCOME_LABEL, outcome), `${outcome} is an outcome`);
  return OUTCOME_LABEL[outcome];
}

/** A run's counts in one line. Input: the totals. Output: the line. */
export function totalsLine(totals: Totals): string {
  const counted = totals.passed + totals.failed + totals.skipped + totals.errored;
  assert(counted <= totals.total, "counted results are some of the total");
  if (totals.total === 0) return "No results";
  const parts = [`${totals.passed} passed`, `${totals.failed} failed`];
  if (totals.errored > 0) parts.push(`${totals.errored} errored`);
  if (totals.skipped > 0) parts.push(`${totals.skipped} skipped`);
  if (totals.total > counted) parts.push(`${totals.total - counted} not run`);
  return `${parts.join(", ")} of ${totals.total}`;
}

/** One argv item as the Covered block shows it: single-quoted when empty or holding
 * whitespace, a quote or a backslash, so distinct commands read distinctly. Input: the item.
 * Output: the text. */
export function argvItem(item: string): string {
  if (item !== "" && !/[\s'"\\]/.test(item)) return item;
  return `'${item.replaceAll("\\", "\\\\").replaceAll("'", "\\'")}'`;
}

/** A time in milliseconds as this Mac's local date and time. Input: the time. Output: the
 * text. */
export function whenText(ms: number): string {
  assert(Number.isSafeInteger(ms) && ms >= 0, "a time is a whole number of milliseconds");
  const at = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** One row of the Covered block: the binding field it shows, its label and its value. */
export interface CoveredRow { field: string; label: string; value: string }

const NETWORK_TEXT: Record<NetworkMode, string> = { off: "Off, no sockets", localhost: "This Mac only (loopback)", remotes: "The project's sandbox remotes" };

const GIT_ROWS: { field: string; label: string }[] = [
  { field: "source.commit", label: "Commit" },
  { field: "source.tree", label: "Tree" },
  { field: "source.base", label: "Base" },
  { field: "source.change_set", label: "Change set" },
  { field: "source.repository", label: "Repository" },
];

const POLICY_ROWS: { field: string; label: string }[] = [
  { field: "policy.profile", label: "Profile" },
  { field: "policy.profile_approved_ms", label: "Profile approved" },
  { field: "policy.check", label: "Check" },
  { field: "policy.kind", label: "Kind" },
  { field: "policy.argv", label: "Command" },
  { field: "policy.toolchain", label: "Toolchain" },
  { field: "policy.sandbox", label: "Sandbox digest" },
  { field: "policy.environment", label: "Environment digest" },
  { field: "policy.network", label: "Network" },
  { field: "policy.timeout_s", label: "Timeout" },
];

function sourceValues(source: SourceRef & { kind: "git" }): string[] {
  return [source.commit, source.tree, source.base ?? "None", source.change_set ?? "None", source.repository];
}

function policyValues(policy: PolicyRef): string[] {
  const toolchain = policy.toolchain.family === "none" ? "None" : `${policy.toolchain.family} ${policy.toolchain.version}`;
  return [policy.profile, whenText(policy.profile_approved_ms), policy.check, policy.kind, argvLine(policy.argv), toolchain,
    policy.sandbox, policy.environment, NETWORK_TEXT[policy.network], `${policy.timeout_s} s`];
}

/** The Covered block: every field a run is bound to, so the owner sees exactly what a passing
 * run proves. Git source names commit, tree, base, change set and repository; a declared
 * source names its reference; the policy names profile, approval, check, kind, command,
 * toolchain, digests, network and timeout. A part too large for one reply still names each
 * field. Input: the source and policy. Output: the rows in order. */
export function coveredRows(run: Pick<RunDetail, "source" | "policy">): CoveredRow[] {
  const { source, policy } = run;
  const rows: CoveredRow[] = source === "cut" ? GIT_ROWS.map((row) => ({ ...row, value: CUT_TEXT }))
    : source.kind === "git" ? GIT_ROWS.map((row, at) => ({ ...row, value: sourceValues(source)[at] }))
      : [{ field: "source.ref", label: "Declared source", value: source.ref }];
  const values = policy === "cut" ? POLICY_ROWS.map(() => CUT_TEXT) : policyValues(policy);
  assert(values.length === POLICY_ROWS.length, "every policy field has a value");
  POLICY_ROWS.forEach((row, at) => rows.push({ ...row, value: values[at] }));
  assert(new Set(rows.map((row) => row.field)).size === rows.length, "each binding field shows once");
  return rows;
}

/** One line of the integrity block and whether it is a warning. */
export interface IntegrityLine { text: string; warn: boolean }

/** The integrity block as lines: tests executed, tests lost against the baseline, skip
 * markers added, protected paths touched, holdouts and the build. Input: the integrity,
 * null when none was recorded, `cut` when too large. Output: the lines. */
export function integrityLines(integrity: IntegrityRef | null | Cut): IntegrityLine[] {
  if (integrity === "cut") return [{ text: "The integrity evidence is too large to show in one reply.", warn: true }];
  if (integrity === null) return [{ text: "No integrity evidence was recorded for this run.", warn: true }];
  const lines = [{ text: integrity.executed === 1 ? "1 test ran." : `${integrity.executed} tests ran.`, warn: integrity.executed === 0 }];
  const { baseline_run, removed, removed_total } = integrity.inventory;
  assert(removed.length <= removed_total, "listed removed tests are some of the total");
  const more = removed_total > removed.length ? `, and ${removed_total - removed.length} more` : "";
  if (baseline_run === null) lines.push({ text: "No earlier passed run to compare the tests with.", warn: false });
  else if (removed_total === 0) lines.push({ text: `Every test ${baseline_run} ran still ran.`, warn: false });
  else lines.push({ text: `${removed_total === 1 ? "1 test" : `${removed_total} tests`} ${baseline_run} ran did not run: ${removed.join(", ")}${more}`, warn: true });
  for (const skip of integrity.new_skips) lines.push({ text: `New skip marker ${skip.marker} at ${skip.path}:${skip.line}`, warn: true });
  if (integrity.protected_touched.length > 0) lines.push({ text: `Protected paths changed: ${integrity.protected_touched.join(", ")}`, warn: true });
  lines.push({ text: integrity.holdouts_ran ? "Holdout checks ran." : "No holdout checks ran.", warn: false });
  lines.push({ text: integrity.build_ok ? "The build succeeded." : "The build failed.", warn: !integrity.build_ok });
  return lines;
}

/** The current approved profile a run is compared with: its id and approval time. */
export interface CurrentProfile { id: string; approved_ms: number }

/** What a change set's latest run of a check was bound to: the commit it covered and the
 * profile with its approval time. */
export interface RunBinding { commit: string; profile: string; profile_approved_ms: number }

/** Why a run no longer counts for the change set's Accept: it covered another commit than
 * the change set's head, or a profile or approval that is not the current one. Inputs: the
 * run's binding, the change set's head commit and the current profile, null when none is
 * approved. Output: the sentence, or null when the run counts. */
export function staleLine(run: RunBinding, head: string, current: CurrentProfile | null): string | null {
  assert(head !== "", "a change set has a head");
  assert(run.commit !== "", "a run covered a commit");
  if (run.commit !== head) return "This run covered an earlier version of the change, so it doesn't count for Accept. Run checks again.";
  if (current === null) return "No check profile is approved now, so this run doesn't count for Accept.";
  if (run.profile !== current.id || run.profile_approved_ms !== current.approved_ms) {
    return "This run used a profile that is no longer the approved one, so it doesn't count for Accept. Run checks again.";
  }
  return null;
}

/** The profile in use among a page of stored profiles, as runs are compared with it. Input:
 * the rows. Output: the current profile, or null when none on the page is in use. */
export function currentProfile(rows: readonly ProfileRow[]): CurrentProfile | null {
  const current = rows.filter((row) => row.state === "current");
  assert(current.length <= 1, "at most one profile is in use");
  const row = current[0];
  if (row === undefined) return null;
  assert(row.approved_ms !== null, "the profile in use was approved");
  return { id: row.id, approved_ms: row.approved_ms };
}

/** The `case_list` arguments for a filter and cursor: only the filters set. Inputs: the
 * filter and the cursor. Output: the arguments; the bridge adds the project. */
export function caseListArgs(filter: CaseFilter, cursor: string | null): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  if (filter.kind !== null) args.kind = filter.kind;
  if (filter.status !== null) args.status = filter.status;
  if (filter.set !== null) args.set = filter.set;
  if (filter.unlinked) args.unlinked = true;
  if (cursor !== null) args.cursor = cursor;
  assert(Object.keys(args).length <= 5, "case_list takes at most five filters");
  return args;
}

/** The `run_list` arguments for a filter and cursor: only the filters set. Inputs: the filter
 * and the cursor. Output: the arguments; the bridge adds the project. */
export function runListArgs(filter: RunFilter, cursor: string | null): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  if (filter.verdict !== null) args.verdict = filter.verdict;
  if (filter.plan !== null) args.plan = filter.plan;
  if (filter.batch !== null) args.batch = filter.batch;
  if (filter.commit !== null) args.commit = filter.commit;
  if (cursor !== null) args.cursor = cursor;
  assert(Object.keys(args).length <= 5, "run_list takes at most five filters");
  return args;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function count(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function text(value: unknown): value is string {
  return typeof value === "string";
}

function among<T extends string>(options: readonly T[], value: unknown): value is T {
  return typeof value === "string" && (options as readonly string[]).includes(value);
}

function testId(value: unknown): value is string {
  return text(value) && TEST_ID.test(value);
}

function nullOr<T>(value: unknown, check: (value: unknown) => value is T): value is T | null {
  return value === null || check(value);
}

function sha(value: unknown): value is string {
  return text(value) && COMMIT.test(value);
}

function digest(value: unknown): value is string {
  return text(value) && DIGEST.test(value);
}

function sourceOf(value: unknown): value is SourceRef {
  if (!record(value)) return false;
  if (value.kind === "declared") return text(value.ref);
  return value.kind === "git" && text(value.repository) && sha(value.commit) && sha(value.tree) && nullOr(value.base, sha) && nullOr(value.change_set, text);
}

function policyOf(value: unknown): value is PolicyRef {
  if (!record(value) || !record(value.toolchain)) return false;
  const argv = Array.isArray(value.argv) && value.argv.length <= ARGV_ITEMS_MAX && value.argv.every(text);
  return digest(value.profile) && count(value.profile_approved_ms) && text(value.check) && among(CASE_KINDS, value.kind) && argv
    && among(TOOLCHAIN_FAMILIES, value.toolchain.family) && text(value.toolchain.version) && digest(value.sandbox)
    && among(NETWORK_MODES, value.network) && count(value.timeout_s) && digest(value.environment);
}

function integrityOf(value: unknown): value is IntegrityRef {
  if (!record(value) || !record(value.inventory)) return false;
  const inventory = value.inventory;
  const removed = Array.isArray(inventory.removed) && inventory.removed.every(text) && count(inventory.removed_total) && inventory.removed.length <= inventory.removed_total;
  const skips = Array.isArray(value.new_skips) && value.new_skips.every((skip) => record(skip) && text(skip.path) && count(skip.line) && text(skip.marker));
  return count(value.executed) && nullOr(inventory.baseline_run, testId) && removed && skips && Array.isArray(value.protected_touched)
    && value.protected_touched.every(text) && typeof value.holdouts_ran === "boolean" && typeof value.build_ok === "boolean";
}

function totalsOf(value: unknown): value is Totals {
  return record(value) && count(value.total) && count(value.passed) && count(value.failed) && count(value.skipped) && count(value.errored)
    && value.passed + value.failed + value.skipped + value.errored <= value.total;
}

function verdictOf(value: unknown): value is Verdict {
  return among(VERDICTS, value);
}

function headerOf(value: unknown): value is RunHeader {
  if (!record(value) || !testId(value.id) || !among(RUN_STATES, value.state) || !nullOr(value.verdict, verdictOf)) return false;
  if (value.state === "finished" && value.verdict === null) return false;
  return nullOr(value.plan, testId) && nullOr(value.batch, text) && text(value.title) && nullOr(value.reason, text) && count(value.started_ms)
    && nullOr(value.finished_ms, count) && text(value.recorded_by) && totalsOf(value.totals) && count(value.version);
}

function resultOf(value: unknown): value is ResultView {
  return record(value) && count(value.position) && value.position >= 1 && text(value.name) && among(OUTCOMES, value.outcome) && nullOr(value.case, testId)
    && nullOr(value.duration_ms, count) && nullOr(value.message, text) && nullOr(value.artifact, text) && (value.withheld === undefined || value.withheld === true);
}

function cursorOf(cursor: string | null, prefix: string): string | null | undefined {
  if (cursor === null) return null;
  return cursor.length <= CURSOR_BYTES_MAX && cursor.startsWith(`${prefix}:`) ? cursor : undefined;
}

function resultsOf(value: unknown): ResultView[] | null {
  if (!Array.isArray(value) || value.length > RUN_SHOW_PAGE_ROWS || !value.every(resultOf)) return null;
  return value.every((result, at) => at === 0 || result.position > value[at - 1].position) ? value : null;
}

function cutOr<T>(data: Record<string, unknown>, name: string, check: (value: unknown) => value is T): T | Cut | undefined {
  if (data[`${name}_cut`] === true && !Object.hasOwn(data, name)) return "cut";
  return check(data[name]) ? data[name] as T : undefined;
}

function integrityOrNull(value: unknown): value is IntegrityRef | null {
  return value === null || integrityOf(value);
}

/** Reads the first `run_show` page, refusing a page of another shape. Inputs: the reply data
 * and cursor. Output: the run's detail, its first results and the next cursor, or null. */
export function runPage(data: unknown, cursor: string | null): RunPage | null {
  if (!record(data) || !headerOf(data.run)) return null;
  const source = cutOr(data, "source", sourceOf);
  const policy = cutOr(data, "policy", policyOf);
  const integrity = cutOr(data, "integrity", integrityOrNull);
  const results = resultsOf(data.results);
  const next = cursorOf(cursor, "result");
  const unlinked = Array.isArray(data.unlinked) && data.unlinked.every(text) && count(data.unlinked_total) && data.unlinked.length <= data.unlinked_total;
  if (source === undefined || policy === undefined || integrity === undefined || results === null || next === undefined || !unlinked) return null;
  const detail: RunDetail = { run: data.run, source, policy, integrity, unlinked: data.unlinked as string[], unlinked_total: data.unlinked_total as number };
  return { detail, results, cursor: next };
}

/** Reads a later `run_show` page of one run. Inputs: the reply data, cursor and the run's
 * id. Output: the results and next cursor, or null for another shape or run. */
export function runResults(data: unknown, cursor: string | null, id: string): ResultsPage | null {
  if (!record(data) || data.id !== id) return null;
  const results = resultsOf(data.results);
  const next = cursorOf(cursor, "result");
  return results === null || next === undefined ? null : { results, cursor: next };
}

function rowsOf<T>(data: unknown, name: string, cursor: string | null, prefix: string, check: (row: unknown) => row is T): ListPage<T> | null {
  if (!record(data)) return null;
  const rows = data[name];
  const next = cursorOf(cursor, prefix);
  if (!Array.isArray(rows) || rows.length > LIST_ROWS_MAX || !rows.every(check) || next === undefined) return null;
  return { rows, cursor: next };
}

function caseRowOf(row: unknown): row is CaseRow {
  return record(row) && testId(row.id) && text(row.title) && typeof row.elided === "boolean" && typeof row.protected === "boolean"
    && nullOr(row.kind, (item): item is CaseKind => among(CASE_KINDS, item)) && nullOr(row.status, (item): item is CaseStatus => among(CASE_STATUSES, item))
    && nullOr(row.latest, (item): item is Outcome => among(OUTCOMES, item));
}

function runRowOf(row: unknown): row is RunRow {
  return record(row) && testId(row.id) && text(row.title) && typeof row.elided === "boolean" && among(RUN_STATES, row.state)
    && nullOr(row.verdict, verdictOf) && count(row.started_ms) && nullOr(row.finished_ms, count) && totalsOf(row.totals);
}

function planRowOf(row: unknown): row is PlanRow {
  return record(row) && testId(row.id) && text(row.name) && among(PLAN_STATUSES, row.status) && count(row.items);
}

/** Reads one `case_list` page. Inputs: the reply data and cursor. Output: the page, or null. */
export function casePage(data: unknown, cursor: string | null): ListPage<CaseRow> | null {
  return rowsOf(data, "cases", cursor, "case", caseRowOf);
}

/** Reads one `run_list` page. Inputs: the reply data and cursor. Output: the page, or null. */
export function runListPage(data: unknown, cursor: string | null): ListPage<RunRow> | null {
  return rowsOf(data, "runs", cursor, "run", runRowOf);
}

/** Reads one `plan_list` page. Inputs: the reply data and cursor. Output: the page, or null. */
export function planPage(data: unknown, cursor: string | null): ListPage<PlanRow> | null {
  return rowsOf(data, "plans", cursor, "plan", planRowOf);
}

function coverageCaseOf(item: unknown): boolean {
  if (!record(item) || !testId(item.case) || !text(item.title) || typeof item.proves !== "boolean") return false;
  const latest = item.latest;
  return latest === null || (record(latest) && among(OUTCOMES, latest.outcome) && testId(latest.run) && verdictOf(latest.verdict));
}

function coverageRowOf(row: unknown): row is CoverageRow {
  return record(row) && record(row.criterion) && text(row.criterion.ticket) && count(row.criterion.position) && text(row.text)
    && among(["open", "met", "waived"] as const, row.state) && count(row.cases_total) && Array.isArray(row.cases)
    && row.cases.length <= row.cases_total && row.cases.every(coverageCaseOf);
}

/** Reads one `coverage` page. Inputs: the reply data and cursor. Output: the page, or null. */
export function coveragePage(data: unknown, cursor: string | null): CoveragePage | null {
  if (!record(data) || !count(data.criteria_total) || !count(data.uncovered_total) || data.uncovered_total > data.criteria_total) return null;
  const page = rowsOf(data, "rows", cursor, "crit", coverageRowOf);
  return page === null ? null : { rows: page.rows, criteria_total: data.criteria_total, uncovered_total: data.uncovered_total, cursor: page.cursor };
}

/** Reads the version names from the first `roadmap` page, the versions coverage is shown per.
 * Input: the reply data. Output: the names in roadmap order, or null. */
export function versionNames(data: unknown): string[] | null {
  if (!record(data) || !Array.isArray(data.versions)) return null;
  const names = data.versions.map((row) => record(row) && text(row.name) ? row.name : null);
  return names.every((name): name is string => name !== null) ? names : null;
}

function refused(code: string): Failure {
  return { kind: "error", code, message: checksRefusal({ code }) };
}

const READ_SHAPE = "tracker_reply_bounds";

async function testsRead<T>(projectId: string, op: TestRead, args: Record<string, unknown>, parse: (data: unknown, cursor: string | null) => T | null): Promise<Result<T>> {
  assert(TEST_READS.includes(op), `${op} is a test read`);
  assert(!Object.hasOwn(args, "project"), "the bridge names the project");
  const reply = await call<TestsReply>("desktop_tests_read", { projectId, op, args });
  if (reply.kind !== "ok") return reply;
  if (reply.value.outcome === "refused") return refused(reply.value.code);
  const parsed = parse(reply.value.data, reply.value.cursor);
  return parsed === null ? refused(READ_SHAPE) : { kind: "ok", value: parsed };
}

/** Lists a project's cases. Inputs: the project id, the filter and the cursor. Output: the page. */
export function loadCases(projectId: string, filter: CaseFilter, cursor: string | null): Promise<Result<ListPage<CaseRow>>> {
  return testsRead(projectId, "case_list", caseListArgs(filter, cursor), casePage);
}

/** Lists a project's plans. Inputs: the project id, the status filter and the cursor. Output:
 * the page. */
export function loadPlans(projectId: string, status: PlanStatus | null, cursor: string | null): Promise<Result<ListPage<PlanRow>>> {
  const args: Record<string, unknown> = {};
  if (status !== null) args.status = status;
  if (cursor !== null) args.cursor = cursor;
  return testsRead(projectId, "plan_list", args, planPage);
}

/** Lists a project's runs newest first. Inputs: the project id, the filter and the cursor.
 * Output: the page. */
export function loadRuns(projectId: string, filter: RunFilter, cursor: string | null): Promise<Result<ListPage<RunRow>>> {
  return testsRead(projectId, "run_list", runListArgs(filter, cursor), runListPage);
}

/** Reads one run's first page. Inputs: the project id and the run id. Output: the page. */
export function loadRun(projectId: string, id: string): Promise<Result<RunPage>> {
  assert(TEST_ID.test(id), "a run id is a test id");
  return testsRead(projectId, "run_show", { id }, runPage);
}

/** Reads one run's next results. Inputs: the project id, the run id and the cursor. Output:
 * the page. */
export function loadRunResults(projectId: string, id: string, cursor: string): Promise<Result<ResultsPage>> {
  assert(TEST_ID.test(id), "a run id is a test id");
  return testsRead(projectId, "run_show", { id, cursor }, (data, next) => runResults(data, next, id));
}

/** Reads the coverage of one version's tickets. Inputs: the project id, the version name and
 * the cursor. Output: the page. */
export function loadCoverage(projectId: string, release: string, cursor: string | null): Promise<Result<CoveragePage>> {
  const args: Record<string, unknown> = { release };
  if (cursor !== null) args.cursor = cursor;
  return testsRead(projectId, "coverage", args, coveragePage);
}

/** Reads the project's version names for the coverage picker. Input: the project id. Output:
 * the names. */
export function loadVersions(projectId: string): Promise<Result<string[]>> {
  return testsRead(projectId, "roadmap", {}, (data) => versionNames(data));
}

/** A checks settings key, in display order. */
export const CHECKS_KEYS = ["gate", "parallel", "keep_failed_snapshots", "agent_requests", "inventory", "skip_markers", "protected_change", "removed_tests"] as const;
export type ChecksKey = typeof CHECKS_KEYS[number];

/** One checks option: an option word, or a number for the counted keys. */
export type ChecksValue = string | number;

/** The `settings_show` view the engine sends: the scope's own values (null or missing for a
 * key taken from below), its version, the layer below, the values in force and the choices. */
export interface ChecksSettingsView {
  project: string | null;
  values: Partial<Record<ChecksKey, ChecksValue | null>>;
  version: number;
  below: Record<ChecksKey, ChecksValue>;
  effective: Record<ChecksKey, ChecksValue>;
  choices: { key: string; options: ChecksValue[]; proposed: ChecksValue; custom: unknown }[];
}

/** One checks setting as a settings page shows it: every option, CrazyAgent's proposal, the
 * value this scope saved (null when it takes the layer below), the value below and the value
 * in force. */
export interface ChecksSettingRow { key: ChecksKey; options: ChecksValue[]; proposed: ChecksValue; here: ChecksValue | null; below: ChecksValue; effective: ChecksValue }

/** One scope's checks settings: the project (null for the app), the version a save sends
 * back and every key in `CHECKS_KEYS` order. */
export interface ChecksSettingsPage { project_id: string | null; version: number; keys: ChecksSettingRow[] }

const KEY_TEXT: Record<ChecksKey, { label: string; help: string }> = {
  gate: { label: "Checks before Accept", help: "What Accept asks of the change set's checks." },
  parallel: { label: "Runs at once", help: "How many check runs go at the same time." },
  keep_failed_snapshots: { label: "Failed snapshots kept", help: "How many failed runs keep their copy of the source to look into." },
  agent_requests: { label: "Agent run requests", help: "How many check runs one agent may ask for each hour." },
  inventory: { label: "Lost tests", help: "How a run's tests are compared, to catch tests that stopped running." },
  skip_markers: { label: "New skip markers", help: "What a skip marker the change adds does to its runs." },
  protected_change: { label: "Protected paths", help: "What a change set touching a protected path does at Accept." },
  removed_tests: { label: "Tests that stopped running", help: "What a test the last passed run reported, and this change's run no longer does, does at Accept." },
};

const WORD_TEXT: Partial<Record<ChecksKey, Record<string, { label: string; line: string }>>> = {
  gate: {
    off: { label: "Off", line: "Accept doesn't look at checks." },
    warn: { label: "Warn", line: "Accept goes ahead, and the change set and its ticket note any check that didn't pass. A protected path or a new skip marker still blocks it." },
    require: { label: "Require", line: "Accept waits until every required check passed on the reviewed change under the approved profile." },
  },
  inventory: {
    last_known: { label: "Compare with the last pass", line: "Each run's tests are compared with the check's last passed run on the base commit." },
    run_base: { label: "Also run the base", line: "The check also runs on the base commit, so there is always a run to compare with." },
    off: { label: "Off", line: "Tests are not compared between runs." },
  },
  skip_markers: {
    flag: { label: "Flag", line: "The run shows each skip marker the change added." },
    refuse: { label: "Refuse", line: "A run whose change added a skip marker can't pass." },
    off: { label: "Off", line: "Skip markers are not looked for." },
  },
  protected_change: {
    refuse: { label: "Block", line: "A change set touching a protected path can't pass the check gate." },
    flag: { label: "Flag", line: "A change set touching a protected path is flagged, and Accept follows the gate setting." },
  },
  removed_tests: {
    flag: { label: "Flag", line: "The change set notes each test that stopped running; under Require, Accept waits." },
    refuse: { label: "Block", line: "A change set whose passing run lost a test can't be accepted, under Warn too." },
    off: { label: "Off", line: "Tests that stopped running don't affect Accept." },
  },
};

const COUNTED: readonly ChecksKey[] = ["parallel", "keep_failed_snapshots", "agent_requests"];

/** A checks key's label and help line. Input: the key. Output: both. */
export function checksKeyText(key: ChecksKey): { label: string; help: string } {
  assert(Object.hasOwn(KEY_TEXT, key), `${key} is a checks key`);
  return KEY_TEXT[key];
}

function countedText(key: ChecksKey, value: number): { label: string; line: string } {
  if (key === "parallel") return { label: String(value), line: value === 1 ? "One check run at a time." : `Up to ${value} check runs at the same time.` };
  if (key === "keep_failed_snapshots") {
    const line = value === 0 ? "No failed run keeps its snapshot." : value === 1 ? "The last failed run keeps its snapshot." : `The last ${value} failed runs keep their snapshots.`;
    return { label: String(value), line };
  }
  return value === 0 ? { label: "Off", line: "Agents can't ask for check runs." } : { label: `${value} an hour`, line: `Each agent may ask for up to ${value} check runs an hour.` };
}

/** One checks option's label and the line saying what it does. Inputs: the key and the
 * option. Output: both. */
export function checksOptionText(key: ChecksKey, value: ChecksValue): { label: string; line: string } {
  if (COUNTED.includes(key)) {
    assert(typeof value === "number" && Number.isInteger(value) && value >= 0, `${key} is a whole number`);
    return countedText(key, value);
  }
  assert(typeof value === "string", `${key} options are words`);
  const words = WORD_TEXT[key]?.[value];
  assert(words !== undefined, `${key} has no option ${value}`);
  return words;
}

/** The choice that takes the layer below: "Default (...)" on the app page, "App default
 * (...)" on a project page. Inputs: the row and the scope. Output: the label. */
export function checksBelowLabel(row: Pick<ChecksSettingRow, "key" | "below">, scope: "app" | "project"): string {
  return `${scope === "app" ? "Default" : "App default"} (${checksOptionText(row.key, row.below).label})`;
}

/** Turns the engine's settings view into the rows a settings page shows, checking it is for
 * the scope asked: every key once, each value one of its options, and the value in force the
 * saved value over the layer below. Inputs: the view and the project id, null for the app.
 * Output: the page. */
export function checksPage(view: ChecksSettingsView, projectId: string | null): ChecksSettingsPage {
  assert(view.project === projectId, "a checks view is for the scope it was read for");
  assert(Number.isSafeInteger(view.version) && view.version >= 0, "a checks view has a version");
  assert(view.choices.length === CHECKS_KEYS.length, "a checks view lists every key");
  const keys = CHECKS_KEYS.map((key, at): ChecksSettingRow => {
    const choice = view.choices[at];
    assert(choice.key === key, "checks choices come in display order");
    const here = view.values[key] ?? null;
    const row = { key, options: choice.options, proposed: choice.proposed, here, below: view.below[key], effective: view.effective[key] };
    assert(row.options.includes(row.proposed) && row.options.includes(row.below) && row.options.includes(row.effective), `${key} values are options`);
    assert(here === null || row.options.includes(here), `${key} saved value is an option`);
    assert(row.effective === (here ?? row.below), `${key} in force is the saved value over the layer below`);
    return row;
  });
  return { project_id: projectId, version: view.version, keys };
}

/** The values one scope saves: each key set to an option, none for a key left on the layer
 * below. Inputs: the rows and the choices by key, null for the layer below; a key not chosen
 * keeps its saved value. Output: the values. */
export function checksValues(rows: readonly ChecksSettingRow[], picked: Partial<Record<ChecksKey, ChecksValue | null>>): Partial<Record<ChecksKey, ChecksValue>> {
  assert(rows.length === CHECKS_KEYS.length, "a checks page lists every key");
  const values: Partial<Record<ChecksKey, ChecksValue>> = {};
  for (const row of rows) {
    const value = Object.hasOwn(picked, row.key) ? picked[row.key] ?? null : row.here;
    if (value === null) continue;
    assert(row.options.includes(value), `${row.key} saves one of its options`);
    values[row.key] = value;
  }
  return values;
}

/** Whether a draft differs from what the scope saved. Inputs: the rows and the choices.
 * Output: true when Save has something to store. */
export function checksChanged(rows: readonly ChecksSettingRow[], picked: Partial<Record<ChecksKey, ChecksValue | null>>): boolean {
  return rows.some((row) => Object.hasOwn(picked, row.key) && (picked[row.key] ?? null) !== row.here);
}

/** Where a stored profile stands. */
export const PROFILE_STATES = ["proposed", "current", "approved", "revoked"] as const;
export type ProfileState = typeof PROFILE_STATES[number];

/** One stored profile in the project's list, without its body. */
export interface ProfileRow { id: string; state: ProfileState; proposed_by: string; proposed_ms: number; approved_ms: number | null; revoked_ms: number | null; question: number | null; checks: number; holdouts: number }

/** One check of a profile, as a stored profile carries it. */
export interface CheckSpec {
  id: string;
  kind: string;
  toolchain: string | null;
  argv: string[];
  cwd: string;
  format: string;
  report: string | null;
  timeout_s: number;
  network: NetworkMode;
  when: string[];
  required: boolean;
  protected: boolean;
}

/** One field of a profile check as the owner reads it before Approve: the field, its label
 * and its value in words. */
export interface CheckRow { field: keyof CheckSpec; label: string; value: string }

const KIND_TEXT: Readonly<Record<string, string>> = { unit: "Unit", integration: "Integration", e2e: "End to end", smoke: "Smoke", scenario: "Scenario" };
const FORMAT_TEXT: Readonly<Record<string, string>> = {
  libtest: "libtest text on standard output", junit: "A JUnit XML file", allure: "An Allure results folder", exit_code: "Only its exit status",
};
const TOOLCHAIN_TEXT: Readonly<Record<string, string>> = { rust: "Rust", node: "Node.js", python: "Python" };
const TRIGGER_TEXT: Readonly<Record<string, string>> = {
  turn_end: "at an agent's turn end", before_accept: "before you accept", on_request: "when you or the change set's agent asks",
};

function wordOf(table: Readonly<Record<string, string>>, word: string): string {
  return Object.hasOwn(table, word) ? table[word] : word;
}

/** Every field of a profile check in the order the engine declares them, in words: what
 * Approve stands on, so none may go unshown. Input: the check. Output: one row per field. */
export function checkRows(check: CheckSpec): CheckRow[] {
  assert(check.argv.length > 0, "a check runs a command");
  const runs = check.when.length === 0 ? "Never on its own" : check.when.map((trigger) => wordOf(TRIGGER_TEXT, trigger)).join(", ");
  const rows: CheckRow[] = [
    { field: "id", label: "Check", value: check.id },
    { field: "kind", label: "Kind", value: wordOf(KIND_TEXT, check.kind) },
    { field: "toolchain", label: "Toolchain", value: check.toolchain === null ? "None managed" : wordOf(TOOLCHAIN_TEXT, check.toolchain) },
    { field: "argv", label: "Command", value: argvLine(check.argv) },
    { field: "cwd", label: "Folder", value: check.cwd },
    { field: "format", label: "Reports", value: wordOf(FORMAT_TEXT, check.format) },
    { field: "report", label: "Report path", value: check.report ?? "None" },
    { field: "timeout_s", label: "Time limit", value: `${check.timeout_s} s` },
    { field: "network", label: "Network", value: wordOf(NETWORK_TEXT, check.network) },
    { field: "when", label: "Runs", value: runs },
    { field: "required", label: "Accept", value: check.required ? "Required to pass" : "Not required" },
    { field: "protected", label: "Cases", value: check.protected ? "Protected" : "Not protected" },
  ];
  assert(new Set(rows.map((row) => row.field)).size === rows.length, "each field has one row");
  return rows;
}

/** A stored profile with its body; holdout files are named by path and digest only. */
export interface StoredProfile {
  id: string;
  project: string;
  profile: { version: number; checks: CheckSpec[]; holdouts: { check: CheckSpec; files: Record<string, string> }[]; protected_paths: string[] };
  proposed_by: string;
  proposed_ms: number;
  approved_ms: number | null;
  revoked_ms: number | null;
  question: number | null;
  state: ProfileState;
}

const PROFILE_STATE_TEXT: Record<ProfileState, string> = { proposed: "Waiting for your approval", current: "Approved, in use", approved: "Approved before, replaced", revoked: "Revoked" };

/** A profile's state in words. Input: the state. Output: the words. */
export function profileStateText(state: ProfileState): string {
  assert(Object.hasOwn(PROFILE_STATE_TEXT, state), `${state} is a profile state`);
  return PROFILE_STATE_TEXT[state];
}

/** What the owner can do to a stored profile: approve one waiting or replaced, revoke the one
 * in use. Input: the state. Output: the actions. */
export function profileActions(state: ProfileState): { approve: boolean; revoke: boolean } {
  const actions = { approve: state === "proposed" || state === "approved", revoke: state === "current" };
  assert(!(actions.approve && actions.revoke), "a profile is approved or revoked, not both");
  return actions;
}

/** Who proposed a profile in words. Input: the stored actor (`owner`, `master:<id>` or
 * `agent:<id>`). Output: the words. */
export function proposerText(actor: string): string {
  if (actor === "owner") return "You";
  const match = /^(master|agent):(.+)$/.exec(actor);
  if (match === null) return actor;
  return `${match[1] === "master" ? "Master" : "Agent"} ${match[2]}`;
}

/** A check's argv as one line, items quoted as the Covered block quotes them. Input: the
 * argv. Output: the line. */
export function argvLine(argv: readonly string[]): string {
  return argv.length === 0 ? "None" : argv.map(argvItem).join(" ");
}

/** A holdout's files as the owner sees them: each path with its digest, never the bytes.
 * Input: the files map. Output: the rows by path. */
export function holdoutFiles(files: Record<string, string>): { path: string; digest: string }[] {
  const rows = Object.entries(files).map(([path, value]) => ({ path, digest: value }));
  assert(rows.every((row) => DIGEST.test(row.digest)), "a holdout file is named by its digest");
  return rows.sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0);
}

/** Checks profile text before it is sent: JSON of one object within the engine's input
 * bound; the engine checks the rest. Input: the text. Output: null when it can be sent, or
 * the problem in plain words. */
export function profileTextProblem(textValue: string): string | null {
  if (textValue.trim() === "") return "Write the profile as JSON first.";
  if (new TextEncoder().encode(textValue).length > PROFILE_INPUT_BYTES_MAX) return "The profile is too large. Profile text is at most 32 KiB.";
  try {
    const parsed: unknown = JSON.parse(textValue);
    return record(parsed) ? null : "A profile is one JSON object.";
  } catch {
    return "The profile isn't valid JSON.";
  }
}

/** One queued or finished check job of a change set. */
export interface CheckJob { batch: string; check: string; trigger: "turn_end" | "before_accept" | "on_request"; actor: string; state: "queued" | "running" | "recorded" | "cancelled"; run: number | null; reason: string | null; queued_ms: number }

/** The latest run of one check on a change set. */
export interface LatestRun {
  check: string;
  run_id: string;
  batch: string;
  state: RunState;
  verdict: Verdict | null;
  commit: string;
  tree: string;
  profile: string;
  profile_approved_ms: number;
  protected_touched: number;
  holdout: boolean;
  finished_ms: number | null;
}

/** Every gate mode. */
export const GATE_MODES = ["off", "warn", "require"] as const;
export type GateMode = typeof GATE_MODES[number];

/** What `removed_tests` asks of a test a passing run stopped reporting. */
export type RemovedTests = "flag" | "refuse" | "off";

/** What the gate says about a change set, once the engine joins it to `runs`. */
export interface GateGap { code: string; check: string | null; run: string | null; paths: string[]; paths_total: number }

/** What Accept would meet at a change set's head, as the engine's gate reads it now: the gate
 * and removed tests settings in effect, the head and profile, the code Accept would be refused
 * with (null when it may go ahead), the first gaps and how many there are. */
export interface GateView { gate: GateMode; removed_tests: RemovedTests; head: string; profile: string | null; refusal: string | null; gaps: GateGap[]; gaps_total: number }

/** A change set's checks: its jobs, the latest run per check and the gate's view. */
export interface ChangeSetChecks { jobs: CheckJob[]; latest: LatestRun[]; gate: GateView }

/** The gate's gap codes in precedence order. */
export const GATE_CODES = ["checks_protected_change", "checks_new_skips", "checks_tests_removed", "checks_failed", "checks_stale", "checks_not_run"] as const;
export type GateCode = typeof GATE_CODES[number];

const GATE_TEXT: Record<GateCode, string> = {
  checks_protected_change: "This change touches a protected path.",
  checks_new_skips: "This change adds a skip marker to the tests.",
  checks_tests_removed: "A passing run no longer reports tests the last passed run did.",
  checks_failed: "A required check didn't pass on this change.",
  checks_stale: "The latest runs covered an earlier version of the change or another profile.",
  checks_not_run: "A required check hasn't run on this change yet.",
};

/** One gap in plain words. Input: the gap's code. Output: the sentence. */
export function gapText(code: string): string {
  return Object.hasOwn(GATE_TEXT, code) ? GATE_TEXT[code as GateCode] : checksRefusal({ code });
}

/** A gap's first paths or test names and how many more there are, as the engine sent them;
 * the view escapes hidden characters. Input: the gap. Output: the line. */
export function pathsLine(gap: GateGap): string {
  assert(gap.paths.length <= gap.paths_total, "a gap counts every path it names");
  const more = gap.paths_total - gap.paths.length;
  assert(more >= 0, "no gap names more paths than it has");
  return `${gap.paths.join(", ")}${more > 0 ? ` and ${more} more` : ""}`;
}

/** The gate's line for a change set and its tone. Whether Accept is blocked is the engine's
 * call: its refusal code, never one the window works out. Input: the gate's view. Output: the
 * line. */
export function gateLine(view: GateView): { text: string; tone: "ok" | "warn" | "danger" | "neutral" } {
  assert(among(GATE_MODES, view.gate), `${view.gate} is a gate mode`);
  assert(view.gaps.length <= view.gaps_total, "the gate counts every gap it lists");
  if (view.gate === "off") return { text: "Checks don't affect Accept for this project.", tone: "neutral" };
  if (view.gaps_total === 0) return { text: "Every required check passed on this change.", tone: "ok" };
  const others = view.gaps_total - 1;
  const more = others > 0 ? ` ${others} more ${others === 1 ? "gap" : "gaps"}.` : "";
  if (view.refusal !== null) return { text: `${gapText(view.refusal)} Accept is blocked until this is fixed.${more}`, tone: "danger" };
  const first = view.gaps[0];
  return { text: `${first === undefined ? "The gate found a gap." : gapText(first.code)} Accept will go ahead and note it.${more}`, tone: "warn" };
}

const REFUSALS: Record<string, string> = {
  checks_owner_only: "Only you can change checks.",
  checks_setting_invalid: "One of these values isn't an option of its setting.",
  checks_settings_stale: "These check settings changed since you opened them. The page now shows the saved values; make your change again.",
  checks_project_missing: "CrazyAgent doesn't know this project any more.",
  checks_unavailable: "CrazyAgent couldn't read its checks. Try again.",
  checks_profile_invalid: "The profile isn't valid. Check its fields against the profile format.",
  checks_profile_unknown: "That profile isn't stored for this project.",
  checks_profile_revoked: "That profile was revoked. Propose a new one.",
  checks_profiles_full: "This project keeps the most profiles it can. Revoke or replace old ones first.",
  checks_holdout_missing: "A holdout file the profile names isn't stored.",
  checks_holdout_invalid: "A holdout file isn't valid.",
  checks_holdouts_full: "This project stores the most holdout files it can.",
  checks_batch_unknown: "That batch of runs isn't known.",
  checks_change_set_unknown: "That change set isn't one of this project's.",
  checks_request_invalid: "That request couldn't be sent. Reload the page and try again.",
  checks_request_bounds: "That request is too large to send.",
  checks_reply_bounds: "The engine's reply was too large to read.",
  checks_outcome_unknown: "CrazyAgent couldn't tell whether that worked. Reload to see what happened.",
  checks_change_set_accepted: "This change set was accepted, so its checks don't run any more.",
  checks_change_set_discarded: "This change set was discarded, so its checks don't run any more.",
  checks_change_set_moved: "This change set moved on since the check was asked for. Run checks again on the new version.",
  checks_check_unknown: "The approved profile names no check by that name.",
  checks_command_missing: "A check's command isn't installed where the profile says. Fix the profile, then propose it again.",
  checks_cwd_missing: "A check's folder isn't in the change set. Fix the profile's folder, then propose it again.",
  checks_engine_stopped: "The engine stopped while the check ran. Run checks again.",
  checks_holdout_collides: "A holdout file has the same path as a file in the project.",
  checks_no_profile: "This project has no approved profile. Approve one in Project Settings, Checks.",
  checks_nothing_to_run: "The approved profile has no check to run on request.",
  checks_queue_full: "Too many checks are waiting. Try again once some finish.",
  checks_rate_limited: "Agents asked for too many check runs. Wait, or change the agent requests setting.",
  checks_record_invalid: "The engine couldn't record this run. Run checks again.",
  checks_requests_off: "Agents can't ask for check runs in this project.",
  checks_run_exists: "This run was already recorded.",
  checks_run_invalid: "The engine couldn't read this check's results.",
  checks_run_not_found: "That run isn't recorded for this project.",
  checks_sandbox_unavailable: "The checks sandbox couldn't start on this Mac.",
  checks_snapshot_failed: "CrazyAgent couldn't copy the change set for the check.",
  checks_snapshot_link: "The change set has a link that points outside it, so checks can't copy it.",
  checks_snapshot_spent: "This snapshot was already used by another run.",
  checks_snapshot_store_invalid: "CrazyAgent's snapshot folder isn't safe to use. Restart the engine.",
  checks_snapshot_too_large: "The change set is too large to copy for a check.",
  checks_unavailable_off_macos: "Checks run only on macOS in this version.",
  questions_ask_invalid: "CrazyAgent couldn't ask you about this profile. Propose it again.",
  questions_asker_full: "Too many questions about profiles wait for you. Answer some in Questions first.",
  questions_full: "Too many questions wait for you. Answer some in Questions first.",
  questions_not_terminal: "That question isn't one for an agent's terminal.",
  questions_notes_invalid: "The question's notes couldn't be kept. Propose the profile again.",
  questions_owner_turn_invalid: "That question isn't waiting for you.",
  questions_page_unavailable: "CrazyAgent couldn't read your questions. Try again.",
  questions_reply_bounds: "The engine's reply about questions was too large to read.",
  questions_request_bounds: "That request about questions is too large to send.",
  questions_request_invalid: "That request about questions couldn't be sent. Reload the page and try again.",
  ...GATE_TEXT,
  tracker_reply_bounds: "The engine sent a test page this app can't read. Update CrazyAgent.",
  tracker_request_invalid: "That test request couldn't be sent. Reload the page and try again.",
  tracker_outcome_unknown: "CrazyAgent couldn't tell what the tracker answered. Reload to see.",
  project_not_attached: "This project has no tickets or tests yet. Turn on tickets in Project Settings, Tickets.",
  cursor_invalid: "That page is out of date. Go back to the first page.",
  stale_version: "This changed since you opened it. The page now shows the saved state; try again.",
};

/** The checks and tests refusals with their own sentence. */
export const CHECKS_REFUSALS: Readonly<Record<string, string>> = REFUSALS;

/** A refused checks or tests request in plain words: its own sentence, the owner connection's
 * and questions' sentence, or the shared sentence that names the code. Input: the refusal. Output: the sentence. */
export function checksRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : questionsRefusal(refusal);
}

/** The checks protocol's replies the window reads, by `kind`. */
export type ChecksReply =
  | { kind: "proposed"; proposal: { id: string; question: number | null } }
  | { kind: "profiles"; page: { profiles: ProfileRow[]; next: number | null } }
  | { kind: "profile"; profile: StoredProfile }
  | { kind: "approved"; profile: unknown }
  | { kind: "revoked" }
  | { kind: "settings"; settings: ChecksSettingsView }
  | { kind: "saved"; saved: { outcome: "saved"; version: number } | { outcome: "stale"; current: number } }
  | { kind: "queued"; batch: string }
  | { kind: "cancelled"; jobs: number }
  | { kind: "runs"; jobs: CheckJob[]; latest: LatestRun[] }
  | { kind: "gate"; gate: GateView };

/** The checks ops the window sends. */
export type ChecksOp = "profile_propose" | "profiles" | "profile_show" | "profile_approve" | "profile_revoke" | "settings_show" | "settings_save" | "run" | "cancel" | "runs" | "gate";

async function checksCall<K extends ChecksReply["kind"]>(projectId: string | null, op: ChecksOp, args: Record<string, unknown>, kind: K): Promise<Result<Extract<ChecksReply, { kind: K }>>> {
  assert(!Object.hasOwn(args, "project"), "the bridge names the project");
  const reply = await call<ChecksReply>("desktop_checks", { projectId, op, args });
  if (reply.kind !== "ok") return reply;
  if (!record(reply.value) || reply.value.kind !== kind) return refused("checks_outcome_unknown");
  return { kind: "ok", value: reply.value as Extract<ChecksReply, { kind: K }> };
}

/** Reads one scope's checks settings. Input: the project id, null for the app. Output: the
 * page. */
export async function loadChecksSettings(projectId: string | null): Promise<Result<ChecksSettingsPage>> {
  const reply = await checksCall(projectId, "settings_show", {}, "settings");
  return reply.kind === "ok" ? { kind: "ok", value: checksPage(reply.value.settings, projectId) } : reply;
}

/** Saves one scope's checks settings at the version the page read. Inputs: the project id,
 * null for the app, the values and the version. Output: Ok once saved, or
 * `checks_settings_stale` when the scope changed since. */
export async function saveChecksSettings(projectId: string | null, values: Partial<Record<ChecksKey, ChecksValue>>, version: number): Promise<Result<void>> {
  const reply = await checksCall(projectId, "settings_save", { values, version }, "saved");
  if (reply.kind !== "ok") return reply;
  return reply.value.saved.outcome === "saved" ? { kind: "ok", value: undefined } : refused("checks_settings_stale");
}

/** Lists a project's stored profiles newest first. Inputs: the project id and the page the
 * last list named. Output: the rows and the next page. */
export async function loadProfiles(projectId: string, after: number | null): Promise<Result<{ rows: ProfileRow[]; next: number | null }>> {
  const reply = await checksCall(projectId, "profiles", after === null ? {} : { after }, "profiles");
  return reply.kind === "ok" ? { kind: "ok", value: { rows: reply.value.page.profiles, next: reply.value.page.next } } : reply;
}

/** Reads one stored profile with its body. Inputs: the project id and the profile id.
 * Output: the profile. */
export async function loadProfile(projectId: string, id: string): Promise<Result<StoredProfile>> {
  assert(DIGEST.test(id), "a profile id is 64 hex digits");
  const reply = await checksCall(projectId, "profile_show", { id }, "profile");
  return reply.kind === "ok" ? { kind: "ok", value: reply.value.profile } : reply;
}

/** Proposes a profile from the owner's JSON text. Inputs: the project id and the text.
 * Output: the new profile's id. */
export async function proposeProfile(projectId: string, profile: string): Promise<Result<string>> {
  assert(profileTextProblem(profile) === null, "only checked profile text is sent");
  const reply = await checksCall(projectId, "profile_propose", { profile }, "proposed");
  return reply.kind === "ok" ? { kind: "ok", value: reply.value.proposal.id } : reply;
}

/** Approves a stored profile, making it the one runs use. Inputs: the project and profile
 * ids. Output: Ok once approved. */
export async function approveProfile(projectId: string, id: string): Promise<Result<void>> {
  assert(DIGEST.test(id), "a profile id is 64 hex digits");
  const reply = await checksCall(projectId, "profile_approve", { id }, "approved");
  return reply.kind === "ok" ? { kind: "ok", value: undefined } : reply;
}

/** Revokes the profile in use; no check runs until another is approved. Inputs: the project
 * and profile ids. Output: Ok once revoked. */
export async function revokeProfile(projectId: string, id: string): Promise<Result<void>> {
  assert(DIGEST.test(id), "a profile id is 64 hex digits");
  const reply = await checksCall(projectId, "profile_revoke", { id }, "revoked");
  return reply.kind === "ok" ? { kind: "ok", value: undefined } : reply;
}

/** Reads a change set's check jobs, latest runs and what Accept would meet now. Inputs: the
 * project and change set ids. Output: the checks. */
export async function loadChangeSetChecks(projectId: string, changeSet: string): Promise<Result<ChangeSetChecks>> {
  const [runs, gate] = await Promise.all([
    checksCall(projectId, "runs", { change_set: changeSet }, "runs"),
    checksCall(projectId, "gate", { change_set: changeSet }, "gate"),
  ]);
  if (runs.kind !== "ok") return runs;
  if (gate.kind !== "ok") return gate;
  return { kind: "ok", value: { jobs: runs.value.jobs, latest: runs.value.latest, gate: gate.value.gate } };
}

/** Asks for every on-request check of a change set. Inputs: the project and change set ids.
 * Output: the batch the runs belong to. */
export async function runChecks(projectId: string, changeSet: string): Promise<Result<string>> {
  const reply = await checksCall(projectId, "run", { change_set: changeSet, checks: [] }, "queued");
  return reply.kind === "ok" ? { kind: "ok", value: reply.value.batch } : reply;
}

/** Cancels a change set's queued and running checks. Inputs: the project and change set ids.
 * Output: how many jobs were cancelled. */
export async function cancelChecks(projectId: string, changeSet: string): Promise<Result<number>> {
  const reply = await checksCall(projectId, "cancel", { change_set: changeSet }, "cancelled");
  return reply.kind === "ok" ? { kind: "ok", value: reply.value.jobs } : reply;
}
