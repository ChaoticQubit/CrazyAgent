import { useEffect, useState, type ReactNode } from "react";
import { accountRanking, listAccounts, poolSummaries, type Result } from "../../api";
import {
  loadUsage, refreshUsage, setManualReset, usageNotReady, usageRefusal,
  type DayTokens, type LinesRow, type UsageReport, type UsageRow, type WindowKind,
} from "../../api-usage";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, EmptyState, LoadError, NotReady, Notice, Panel, Segmented, Skeleton } from "../../ui";
import { LimitsCard } from "../metrics/limits";
import {
  RANGES, REMOVED_LABEL, REMOVED_PROJECTS, agentLabel, agoText, cacheShare, compactCount, dayLabel, headlineTokens, healthText,
  offsetMinutes, PartialNotice, rangeStart, rowName, tokenText, type UsageNames, type UsageRange,
} from "../metrics/usage-model";
import { NextAccountsPanel } from "./account-choice";
import "../project/projects.css";
import "./usage.css";

export * from "../metrics/usage-model";

const TICK_MS = 30_000;
const NOT_READY = "This isn't ready in this build yet.";

function Tile({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return <div className="usage-tile">
    <span className="muted small">{label}</span>
    <span className="usage-tile-value">{value}</span>
    {detail && <span className="muted small">{detail}</span>}
  </div>;
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
        : <Tile label="Tokens" value={compactCount(headlineTokens(report.total))}
          detail={`${compactCount(report.total.input)} input, ${compactCount(report.total.cache_write)} cache write, ${compactCount(report.total.output)} output`} />}
      <Tile label="Re-read from cache" value={compactCount(report.total.cache_read)}
        detail={share === null ? "Billed far lower, not in Tokens" : `${share}% of everything read; billed far lower, not in Tokens`} />
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
      <thead><tr><th scope="col">Name</th><th scope="col">Tokens</th><th scope="col">Input</th><th scope="col">Cache write</th><th scope="col">Output</th><th scope="col" className="muted">Cache read</th><th scope="col">Runs</th><th scope="col">Last used</th></tr></thead>
      <tbody>{rows.map((row) => <tr key={row.key}>
        <th scope="row">{name(row.key)}</th>
        {row.not_reported_by && headlineTokens(row.tokens) === 0
          ? <td colSpan={6}>{tokenText(0, row.not_reported_by)}</td>
          : <>
            <td>{tokenText(headlineTokens(row.tokens), row.not_reported_by)}</td>
            <td>{compactCount(row.tokens.input)}</td>
            <td>{compactCount(row.tokens.cache_write)}</td>
            <td>{compactCount(row.tokens.output)}</td>
            <td className="muted">{compactCount(row.tokens.cache_read)}</td>
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
      <thead><tr><th scope="col">Day</th><th scope="col">Tokens</th><th scope="col">Input</th><th scope="col">Cache write</th><th scope="col">Output</th><th scope="col" className="muted">Cache read</th></tr></thead>
      <tbody>{days.map((day) => <tr key={day.day_ms}>
        <th scope="row">{dayLabel(day.day_ms, offset)}</th>
        <td>{compactCount(headlineTokens(day.tokens))}</td>
        <td>{compactCount(day.tokens.input)}</td>
        <td>{compactCount(day.tokens.cache_write)}</td>
        <td>{compactCount(day.tokens.output)}</td>
        <td className="muted">{compactCount(day.tokens.cache_read)}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
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
  const project = (key: string) => key === REMOVED_PROJECTS ? REMOVED_LABEL : rowName(names.projects, key);
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
