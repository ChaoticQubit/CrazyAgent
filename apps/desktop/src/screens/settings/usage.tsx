import { useEffect, useState, type ReactNode } from "react";
import { accountRanking, listAccounts, poolSummaries, type Result } from "../../api";
import {
  loadUsage, pollFailure, refreshUsage, setManualReset, usageNotReady, usageRefusal,
  type AccountLimits, type DayTokens, type LimitWindow, type LinesRow, type StepHealth, type Tokens, type UsageReport, type UsageRow, type WindowKind,
} from "../../api-usage";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, EmptyState, LoadError, NotReady, Notice, Panel, Segmented, Skeleton, TextField } from "../../ui";
import { NextAccountsPanel } from "./account-choice";
import "../project/projects.css";
import "./usage.css";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const TICK_MS = 30_000;
const NOT_READY = "This isn't ready in this build yet.";
const API_KEY_ACCOUNT = "native_profile_codex_api_key";
const COMPACT = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

export type UsageRange = "today" | "7d" | "30d" | "all";

/** The range switch's choices, shortest first. */
export const RANGES: { value: UsageRange; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "all", label: "All" },
];

/** Names shown in place of ids: projects, pools and accounts by id. */
export interface UsageNames {
  projects: Map<string, string>;
  pools: Map<string, string>;
  accounts: Map<string, string>;
}

/** Where a range starts: midnight today at the viewer's offset now, the one fixed offset the
 * engine cuts every day with, six or twenty-nine days before it so the range holds whole
 * engine days even across a daylight saving change, or null for all time. Inputs: the range
 * and now. Output: milliseconds since the epoch, or null. */
export function rangeStart(range: UsageRange, now: Date): number | null {
  if (range === "all") return null;
  const shift = offsetMinutes(now) * MINUTE;
  const today = Math.floor((now.getTime() + shift) / DAY) * DAY - shift;
  return today - (range === "today" ? 0 : range === "7d" ? 6 : 29) * DAY;
}

/** A By day row's date as the engine cut it: the day's start moved by the same fixed offset
 * and read in UTC, so a day before a daylight saving change keeps its own date. Inputs: the
 * day's start in milliseconds and the offset in minutes east of UTC. Output: the label. */
export function dayLabel(dayMs: number, offset: number): string {
  return new Date(dayMs + offset * MINUTE).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" });
}

/** The viewer's offset east of UTC in minutes, as the engine cuts local days. Input: now. */
export function offsetMinutes(now: Date): number {
  const west = now.getTimezoneOffset();
  return west === 0 ? 0 : -west;
}

/** A token count in a few characters: 950, 1.2K, 3.4M. Input: the count. */
export function compactCount(value: number): string {
  return COMPACT.format(value);
}

/** The headline count usage shows and budgets spend (DEC-117 Q6): new input, cache writes and output, never cache re-reads. Input: the tokens. */
export function headlineTokens(tokens: Tokens): number {
  return tokens.input + tokens.output + tokens.cache_write;
}

/** The share of prompt tokens served from the provider's cache, as a whole percent.
 * Input: the tokens. Output: 0 to 100, or null when no prompt tokens were counted. */
export function cacheShare(tokens: Tokens): number | null {
  const prompt = tokens.input + tokens.cache_read + tokens.cache_write;
  return prompt === 0 ? null : Math.round((tokens.cache_read * 100) / prompt);
}

/** An observed limit window as it stands now, as the engine reads it: the owner's manual
 * reset replaces the observed one, and a window whose reset passed counts as unused with
 * its next reset unknown. Inputs: the window, the manual reset and now. Output: the window,
 * or null when none was observed. */
export function windowNow(observed: LimitWindow | null, overrideMs: number | null, nowMs: number): LimitWindow | null {
  if (!observed) return null;
  const resets = overrideMs ?? observed.resets_at_ms;
  if (resets !== null && resets <= nowMs) return { used_percent: 0, resets_at_ms: null };
  return { used_percent: observed.used_percent, resets_at_ms: resets };
}

/** How long until a window resets. Inputs: the reset time, null when unknown, and now.
 * Output: "Resets in 2 h 14 min", "Resets in 3 d 4 h" or "Next reset not known yet". */
export function resetText(atMs: number | null, nowMs: number): string {
  if (atMs === null) return "Next reset not known yet";
  const minutes = Math.floor((atMs - nowMs) / MINUTE);
  if (minutes < 1) return "Resets in under a minute";
  const hours = Math.floor(minutes / 60);
  if (hours < 1) return `Resets in ${minutes} min`;
  if (hours < 24) return `Resets in ${hours} h ${minutes % 60} min`;
  return `Resets in ${Math.floor(hours / 24)} d ${hours % 24} h`;
}

/** How long ago something happened. Inputs: the time and now. Output: "just now",
 * "5 min ago", "3 h ago" or "2 d ago". */
export function agoText(atMs: number, nowMs: number): string {
  const passed = Math.max(0, nowMs - atMs);
  if (passed < MINUTE) return "just now";
  if (passed < HOUR) return `${Math.floor(passed / MINUTE)} min ago`;
  if (passed < DAY) return `${Math.floor(passed / HOUR)} h ago`;
  return `${Math.floor(passed / DAY)} d ago`;
}

const STEP_WORDS: Record<StepHealth["step"], [string, string]> = {
  poll: ["Checking account limits", "limits shown may be out of date"],
  team: ["Counting agent tokens", "agent and research run token totals may be low"],
  runs: ["Counting research run tokens", "research token totals may be low"],
};

/** One plain sentence for a usage step kind that panicked since the engine started: what
 * may be missing while it keeps failing, or when it worked again. Inputs: the step's health
 * and now. Output: the sentence. */
export function healthText(health: StepHealth, nowMs: number): string {
  const [what, missing] = STEP_WORDS[health.step];
  const times = health.panics === 1 ? "once" : `${health.panics} times`;
  const said = `${what} hit an internal error ${times} since the engine started, last ${agoText(health.last_ms, nowMs)}.`;
  return health.recovered_ms === null
    ? `${said} CrazyAgent keeps retrying; ${missing}.`
    : `${said} It worked again ${agoText(health.recovered_ms, nowMs)}.`;
}

/** Where and when an account's limits were last read, and whether the last check failed.
 * Inputs: the limits and now. Output: one line, or "Not read yet". */
export function readLine(limits: AccountLimits, nowMs: number): string {
  if (limits.observed_ms === null || limits.source === null) return "Not read yet";
  const from = limits.source === "poll" ? "Read from the provider" : "Read from a run's own record";
  return `${from} ${agoText(limits.observed_ms, nowMs)}`;
}

/** What a window with no current reading says: not read when the last read failed, whose
 * cause the card's notice above gives, or not reported by the provider, as for an API-key
 * account, which has no such window. Input: the account's last failure. Output: the
 * window's state and its line. */
export function missingWindow(failure: string | null): [string, string] {
  return failure && failure !== API_KEY_ACCOUNT
    ? ["Not read", "The last read failed, for the reason above."]
    : ["Not reported", "The provider hasn't reported this window."];
}

/** An agent's name in the tables. Inputs: the engine's agent key and the project namer.
 * Output: "Agent 3", "Research harbor", "Master, Harbor Notes", "Onboarding" or the key. */
export function agentLabel(key: string, project: (key: string) => string = (key) => key): string {
  const team = /^team-(\d+)$/.exec(key);
  if (team) return `Agent ${team[1]}`;
  const research = /^research-(.+)$/.exec(key);
  if (research) return `Research ${research[1]}`;
  const master = /^master-(.+)$/.exec(key);
  if (master) return `Master, ${project(master[1])}`;
  if (key === "master") return "Master";
  return key === "onboarding" ? "Onboarding" : key;
}

/** Names a project or account row, the empty key being runs tied to none. Inputs: the
 * names by key and the key. Output: the name, "Unattributed" or the key itself. */
function rowName(names: Map<string, string>, key: string): string {
  return key === "" ? "Unattributed" : names.get(key) ?? key;
}

/** Reads a manual reset typed as local date and time. Inputs: the field's value and now.
 * Output: the time in milliseconds, or the reason it can't be used. */
export function manualReset(value: string, nowMs: number): { kind: "ok"; at: number } | { kind: "invalid"; reason: string } {
  const at = new Date(value).getTime();
  if (!value || !Number.isFinite(at)) return { kind: "invalid", reason: "Pick a date and time." };
  if (at <= nowMs) return { kind: "invalid", reason: "Pick a time in the future." };
  return { kind: "ok", at };
}

/** The plain line a reading shows when a scan left records out. Input: how many agent homes
 * held more records than one scan reads. Output: the line, or null when every home was read
 * whole. */
export function partialLine(homes: number): string | null {
  if (!(homes > 0)) return null;
  return homes === 1
    ? "Some usage may be missing: 1 agent home holds more records than one scan reads, so its least recently changed records are not counted."
    : `Some usage may be missing: ${homes} agent homes hold more records than one scan reads, so their least recently changed records are not counted.`;
}

/** A warning that a reading is partial, shown only when a scan left records out. Input: how
 * many agent homes held more records than one scan reads. Output: the notice, or nothing. */
export function PartialNotice({ homes }: { homes: number }) {
  const line = partialLine(homes);
  return line === null ? null : <Notice tone="warn">{line}</Notice>;
}

function Tile({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return <div className="usage-tile">
    <span className="muted small">{label}</span>
    <span className="usage-tile-value">{value}</span>
    {detail && <span className="muted small">{detail}</span>}
  </div>;
}

/** The tokens one row shows: the count, the count and the harness that also ran for it
 * without reporting usage, or only that harness when nothing was counted, never a bare zero.
 * Inputs: the token count and that harness, if any. Output: the text. */
export function tokenText(total: number, notReportedBy?: string | null): string {
  if (!notReportedBy) return compactCount(total);
  if (total === 0) return `Usage not reported by ${notReportedBy}`;
  return `${compactCount(total)} + usage not reported by ${notReportedBy}`;
}

function Totals({ report }: { report: UsageReport }) {
  const share = cacheShare(report.total);
  const added = report.lines.reduce((sum, row) => sum + row.added, 0);
  const removed = report.lines.reduce((sum, row) => sum + row.removed, 0);
  const unreported = (report.not_reported_by ?? []).join(", ");
  const onlyUnreported = unreported !== "" && headlineTokens(report.total) === 0;
  return <>
    <div className="usage-tiles">
      {onlyUnreported
        ? <Tile label="Tokens" value="Not reported" detail={`Usage not reported by ${unreported}`} />
        : <Tile label="Tokens" value={compactCount(headlineTokens(report.total))} detail={`${compactCount(report.total.input)} in, ${compactCount(report.total.output)} out`} />}
      <Tile label="Cache" value={share === null ? "None" : `${share}%`} detail={`${compactCount(report.total.cache_read)} read, ${compactCount(report.total.cache_write)} written`} />
      <Tile label="Runs" value={String(report.runs)} />
      <Tile label="Lines changed" value={`+${compactCount(added)} -${compactCount(removed)}`} />
    </div>
    {unreported !== "" && !onlyUnreported && <p className="muted small">Not in these totals: usage not reported by {unreported}.</p>}
  </>;
}

function UsageTable({ title, rows, name, nowMs }: { title: string; rows: UsageRow[]; name: (key: string) => string; nowMs: number }) {
  if (rows.length === 0) return null;
  return <Panel title={title} padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Name</th><th scope="col">Tokens</th><th scope="col">In</th><th scope="col">Out</th><th scope="col">Cache read</th><th scope="col">Runs</th><th scope="col">Last used</th></tr></thead>
      <tbody>{rows.map((row) => <tr key={row.key}>
        <th scope="row">{name(row.key)}</th>
        {row.not_reported_by && headlineTokens(row.tokens) === 0
          ? <td colSpan={5}>{tokenText(0, row.not_reported_by)}</td>
          : <>
            <td>{tokenText(headlineTokens(row.tokens), row.not_reported_by)}</td>
            <td>{compactCount(row.tokens.input)}</td>
            <td>{compactCount(row.tokens.output)}</td>
            <td>{compactCount(row.tokens.cache_read)}</td>
            <td>{row.runs}</td>
          </>}
        <td>{agoText(row.last_ms, nowMs)}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

function LinesTable({ rows, name }: { rows: LinesRow[]; name: (key: string) => string }) {
  if (rows.length === 0) return null;
  return <Panel title="Lines changed by project" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Project</th><th scope="col">Change sets</th><th scope="col">Lines</th><th scope="col">Accepted commits</th><th scope="col">Lines accepted</th></tr></thead>
      <tbody>{rows.map((row) => <tr key={row.project}>
        <th scope="row">{name(row.project)}</th>
        <td>{row.change_sets}</td>
        <td>+{row.added} -{row.removed}</td>
        <td>{row.commits}</td>
        <td>+{row.committed_added} -{row.committed_removed}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

function DaysTable({ days, offset }: { days: DayTokens[]; offset: number }) {
  if (days.length === 0) return null;
  return <Panel title="By day" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Day</th><th scope="col">Tokens</th><th scope="col">In</th><th scope="col">Out</th><th scope="col">Cache read</th></tr></thead>
      <tbody>{days.map((day) => <tr key={day.day_ms}>
        <th scope="row">{dayLabel(day.day_ms, offset)}</th>
        <td>{compactCount(headlineTokens(day.tokens))}</td>
        <td>{compactCount(day.tokens.input)}</td>
        <td>{compactCount(day.tokens.output)}</td>
        <td>{compactCount(day.tokens.cache_read)}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

function ResetEditor({ id, override, nowMs, pending, onSave }: {
  id: string;
  override: number | null;
  nowMs: number;
  pending: boolean;
  onSave: (at: number | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string>();
  if (!open) return <span className="row">
    <Button size="small" icon="clock" disabled={pending} onClick={() => setOpen(true)}>{override === null ? "Set reset time" : "Change reset time"}</Button>
    {override !== null && <Button size="small" pending={pending} pendingLabel="Clearing..." onClick={() => onSave(null)}>Use provider's time</Button>}
  </span>;
  const save = () => {
    const reset = manualReset(value, nowMs);
    setError(reset.kind === "invalid" ? reset.reason : undefined);
    if (reset.kind === "ok") { onSave(reset.at); setOpen(false); }
  };
  return <form className="row" onSubmit={(event) => { event.preventDefault(); save(); }}>
    <TextField id={id} type="datetime-local" label="Resets at" value={value} onValue={setValue} error={error} />
    <Button type="submit" size="small" variant="primary" pending={pending} pendingLabel="Saving...">Save</Button>
    <Button size="small" onClick={() => setOpen(false)}>Cancel</Button>
  </form>;
}

function WindowLine({ limits, kind, nowMs, pending, onSetReset }: {
  limits: AccountLimits;
  kind: WindowKind;
  nowMs: number;
  pending: boolean;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
}) {
  const override = kind === "five_hour" ? limits.five_hour_reset_override_ms : limits.weekly_reset_override_ms;
  const current = windowNow(kind === "five_hour" ? limits.five_hour : limits.weekly, override, nowMs);
  const label = kind === "five_hour" ? "5-hour" : "Weekly";
  const [missing, why] = missingWindow(limits.failure);
  const leftOut = kind === "five_hour" ? limits.five_hour_left_out_label : limits.weekly_left_out_label;
  const failed = limits.failure !== null && limits.failure !== API_KEY_ACCOUNT;
  if (!current && leftOut !== null && !failed) return <div className="usage-window">
    <div className="row usage-window-head"><strong>{label}</strong></div>
    <span className="muted small">{leftOut}</span>
  </div>;
  return <div className="usage-window">
    <div className="row usage-window-head">
      <strong>{label}</strong>
      <span>{current ? `${Math.round(current.used_percent)}% used` : missing}</span>
    </div>
    {current && <meter className="usage-meter" min={0} max={100} low={70} high={90} optimum={0} value={current.used_percent} aria-label={`${label} limit used`} />}
    <span className="muted small">{current ? resetText(current.resets_at_ms, nowMs) : why}{override !== null ? " (your time)" : ""}</span>
    <ResetEditor id={`usage-reset-${limits.account}-${kind}`} override={override} nowMs={nowMs} pending={pending}
      onSave={(at) => onSetReset(limits.account, kind, at)} />
  </div>;
}

function LimitsCard({ limits, name, nowMs, busy, onSetReset }: {
  limits: AccountLimits;
  name: string;
  nowMs: number;
  busy: string | null;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
}) {
  return <section className="usage-limits" aria-label={`${name} limits`}>
    <div className="row usage-window-head">
      <h3>{name}</h3>
      {limits.plan && <span className="badge">{limits.plan}</span>}
    </div>
    <span className="muted small">{readLine(limits, nowMs)}</span>
    {limits.failure && <Notice tone={limits.failure === API_KEY_ACCOUNT ? "info" : "warn"}>{pollFailure(limits.failure)}</Notice>}
    <WindowLine limits={limits} kind="five_hour" nowMs={nowMs} pending={busy === `${limits.account}/five_hour`} onSetReset={onSetReset} />
    <WindowLine limits={limits} kind="weekly" nowMs={nowMs} pending={busy === `${limits.account}/weekly`} onSetReset={onSetReset} />
  </section>;
}

/** Settings, Usage as a pure view: the range switch, app-wide totals, each account's limits
 * with reset countdowns and manual reset times, tokens by project, pool, account, agent and
 * day, and lines changed. Inputs: the report, names, now, the last failure and the actions. */
export function UsageView({ report, range, names, nowMs, failure, busy, next, onRange, onRefresh, onSetReset, onRetry }: {
  report: Loaded<UsageReport>;
  range: UsageRange;
  names: UsageNames;
  nowMs: number;
  failure: string;
  busy: string | null;
  next?: ReactNode;
  onRange: (range: UsageRange) => void;
  onRefresh: () => void;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
  onRetry: () => void;
}) {
  if (report.kind === "not_ready" || (report.kind === "error" && usageNotReady(report))) return <NotReady what="Usage" />;
  const project = (key: string) => rowName(names.projects, key);
  const account = (key: string) => rowName(names.accounts, key);
  const pool = (key: string) => names.pools.get(key) ?? key;
  const head = <div className="row usage-head">
    <Segmented label="Range" value={range} options={RANGES} onValue={onRange} />
    <Button icon="refresh-cw" pending={busy === "refresh"} pendingLabel="Asking..." onClick={onRefresh}>Check limits now</Button>
  </div>;
  if (report.kind === "loading") return <div className="stack">{head}<Skeleton shape="card" count={3} /></div>;
  if (report.kind === "error") return <div className="stack">{head}<LoadError message={usageRefusal(report)} onRetry={onRetry} /></div>;
  const value = report.value;
  const empty = headlineTokens(value.total) === 0 && value.lines.length === 0 && (value.not_reported_by ?? []).length === 0;
  return <div className="stack">
    <p className="secondary">Tokens your agents used and each account's limits. Counts come from each run's own record; limits are checked every 10 minutes.</p>
    {head}
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {(value.health ?? []).map((item) => <Notice key={item.step} tone={item.recovered_ms === null ? "warn" : "info"}>{healthText(item, nowMs)}</Notice>)}
    <Totals report={value} />
    <PartialNotice homes={value.partial_homes} />
    <Panel title="Account limits">
      {value.limits.length === 0
        ? <p className="muted">No limits read yet. Signed-in Claude and ChatGPT accounts show here after their first check.</p>
        : <div className="usage-limit-grid">{value.limits.map((limits) => <LimitsCard key={limits.account} limits={limits} name={account(limits.account)}
          nowMs={nowMs} busy={busy} onSetReset={onSetReset} />)}</div>}
    </Panel>
    {next}
    {empty ? <EmptyState icon="trending-up" title="No usage in this range" compact>Runs show up here a few seconds after an agent works.</EmptyState> : <>
      <UsageTable title="By project" rows={value.projects} name={project} nowMs={nowMs} />
      <UsageTable title="By pool" rows={value.pools} name={pool} nowMs={nowMs} />
      <UsageTable title="By account" rows={value.accounts} name={account} nowMs={nowMs} />
      <UsageTable title="By agent" rows={value.agents} name={(key) => agentLabel(key, project)} nowMs={nowMs} />
      <DaysTable days={value.days} offset={offsetMinutes(new Date(nowMs))} />
      <LinesTable rows={value.lines} name={project} />
    </>}
  </div>;
}

/** Settings, Usage: reads the usage report for the chosen range every 30 seconds, asks the
 * engine to check limits now, and saves the owner's manual reset times. */
export function UsageSettings() {
  const session = useAppSession();
  const [range, setRange] = useState<UsageRange>("7d");
  const [tick, setTick] = useState(0);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [report, reload] = useLoad(() => {
    const now = new Date();
    return loadUsage(rangeStart(range, now), offsetMinutes(now));
  }, [range, tick]);
  const [accounts] = useLoad(listAccounts, [], "accounts");
  const [pools] = useLoad(poolSummaries, [], "pool-summaries");
  const [ranking] = useLoad(accountRanking, [tick]);
  useEffect(() => {
    const timer = setInterval(() => { setNowMs(Date.now()); setTick((old) => old + 1); }, TICK_MS);
    return () => clearInterval(timer);
  }, []);
  const names: UsageNames = {
    projects: new Map(session.projects.kind === "ok" ? session.projects.value.map((row) => [row.id, row.name]) : []),
    pools: new Map(pools.kind === "ok" ? pools.value.map((row) => [row.id, row.name]) : []),
    accounts: new Map(accounts.kind === "ok" ? accounts.value.map((row) => [row.id, row.name]) : []),
  };

  async function act(id: string, action: () => Promise<Result<void>>, done: string) {
    setFailure("");
    setBusy(id);
    const result = session.noteResult(await action());
    setBusy(null);
    if (result.kind === "ok") session.showToast(done);
    else setFailure(result.kind === "not_ready" ? NOT_READY : usageRefusal(result));
    setNowMs(Date.now());
    reload();
  }

  return <UsageView report={report} range={range} names={names} nowMs={nowMs} failure={failure} busy={busy}
    next={<NextAccountsPanel ranking={ranking} pools={pools} names={names.accounts} nowMs={nowMs} />}
    onRange={(next) => { setRange(next); setNowMs(Date.now()); }} onRetry={reload}
    onRefresh={() => void act("refresh", refreshUsage, "Checking limits now. They update here within a minute.")}
    onSetReset={(account, kind, at) => void act(`${account}/${kind}`, () => setManualReset(account, kind, at),
      at === null ? "Using the provider's reset time." : "Reset time saved.")} />;
}
