import { useEffect, useState, type ReactNode } from "react";
import { listAccounts, type Result } from "../../api";
import { loadAdoption, type Adoption, type AgentAdoption } from "../../api-process";
import { loadTeamStatus } from "../../api-projects";
import type { TeamStatus } from "../../engine-client";
import { loadMetrics, loadUsage, usageNotReady, usageRefusal, type AccountLimits, type Metrics, type Tokens, type UsageReport, type UsageRow } from "../../api-usage";
import { useLoad, type Loaded } from "../../state";
import { EmptyState, LoadError, NotReady, Notice, Panel, Segmented, Select, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import { HARNESS_NAMES } from "../team/coordination";
import { barWords, chartBars, ChartSvg } from "./chart";
import { LeadPanel } from "./lead";
import { AccountLimitsPanel, CheckLimits, type LimitsControls } from "./limits";
import { DaysPanel, LinesPanel, TokenPanel, type BreakdownRow } from "./panels";
import { REMOVED_LABEL, REMOVED_PROJECTS, agentLabel, agoText, cacheShare, compactCount, headlineTokens, healthText, offsetMinutes, PartialNotice, rangeStart, RANGES, rowName, tokenText, windowNow, type UsageRange } from "./usage-model";
import "../project/projects.css";
import "./metrics.css";

const MINUTE = 60_000;
const TICK_MS = 10_000;
const RELOAD_TICKS = 3;

export type MetricsRange = UsageRange;

export type MetricsBy = "project" | "agent" | "run" | "account" | "pool" | "harness";

/** The range switch's choices, shortest first. */
export const METRIC_RANGES: { value: MetricsRange; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
];

const BREAKDOWNS: { value: MetricsBy; label: string }[] = [
  { value: "agent", label: "By agent" },
  { value: "run", label: "By run" },
  { value: "account", label: "By account" },
  { value: "harness", label: "By harness" },
];

const APP_PANELS: { by: MetricsBy; title: string }[] = [
  { by: "project", title: "By project" },
  { by: "agent", title: "By agent" },
  { by: "run", title: "By run" },
  { by: "account", title: "By account" },
  { by: "pool", title: "By pool" },
  { by: "harness", title: "By harness" },
];

const RESULTS: Record<string, string> = {
  running: "Working",
  done: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
  queued: "Waiting",
  needs_pool: "Waiting",
};

/** Names shown in place of usage keys: agents by usage key and accounts by id. */
export interface MetricsNames {
  agents: Map<string, string>;
  accounts: Map<string, string>;
}

/** What makes a Metrics page the app page: the engine's projects, the action that picks one,
 * null for every project, and what its account limits need from the page around them. */
export interface MetricsFilter {
  projects: { id: string; name: string }[];
  onProject: (id: string | null) => void;
  limits: LimitsControls;
}

/** When a reset comes, short: "in 40 min", "in 3 h", or its date a day or more away.
 * Inputs: the reset and now. Output: the words. */
function resetsShort(atMs: number, nowMs: number): string {
  const minutes = Math.max(0, Math.floor((atMs - nowMs) / MINUTE));
  if (minutes < 60) return `in ${minutes} min`;
  if (minutes < 24 * 60) return `in ${Math.floor(minutes / 60)} h`;
  return new Date(atMs).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** An account's limit window with less room left as it stands now, and when it resets.
 * Inputs: the account's limits, if read, and now. Output: the room left as a whole percent
 * and the reset, each empty when unknown. */
export function limitLeft(limits: AccountLimits | undefined, nowMs: number): { left: string; resets: string } {
  if (!limits) return { left: "", resets: "" };
  const windows = [
    windowNow(limits.five_hour, limits.five_hour_reset_override_ms, nowMs),
    windowNow(limits.weekly, limits.weekly_reset_override_ms, nowMs),
  ].filter((window) => window !== null);
  if (windows.length === 0) return { left: "", resets: "" };
  const tight = windows.reduce((least, window) => window.used_percent > least.used_percent ? window : least);
  const left = Math.max(0, Math.round(100 - tight.used_percent));
  return { left: `${left}%`, resets: tight.resets_at_ms === null ? "" : resetsShort(tight.resets_at_ms, nowMs) };
}

/** How long ago the page last read its numbers. Inputs: that time, null before the first
 * read, and now. Output: "Updated 10 s ago", "Updated 5 min ago" or "Updating". */
function updatedText(updatedMs: number | null, nowMs: number): string {
  if (updatedMs === null) return "Updating";
  const passed = Math.max(0, nowMs - updatedMs);
  return passed < MINUTE ? `Updated ${Math.floor(passed / 1000)} s ago` : `Updated ${agoText(updatedMs, nowMs)}`;
}

/** An agent's usage key as the usage store writes it: `master-<project>` for a master,
 * `team-<id>` for any other agent. Inputs: the project and the agent's adoption row. */
function agentKey(projectId: string | null, agent: Pick<AgentAdoption, "agent" | "role">): string {
  return agent.role === "master" && projectId !== null ? `master-${projectId}` : `team-${agent.agent}`;
}

function Tile({ label, value, detail, quiet = false }: { label: string; value: string; detail: string; quiet?: boolean }) {
  return <div className={`mx-tile${quiet ? " quiet" : ""}`}>
    <span className="mx-tile-key">{label}</span>
    <span className="mx-tile-value">{value}</span>
    <span className="mx-tile-detail">{detail}</span>
  </div>;
}

function Tiles({ usage, metrics, lead = false }: { usage: UsageReport; metrics: Metrics; lead?: boolean }) {
  const total = usage.total;
  const share = cacheShare(total);
  const added = usage.lines.reduce((sum, row) => sum + row.added, 0);
  const removed = usage.lines.reduce((sum, row) => sum + row.removed, 0);
  const sets = usage.lines.reduce((sum, row) => sum + row.change_sets, 0);
  return <div className="mx-tiles">
    {!lead && <Tile label="Tokens" value={compactCount(headlineTokens(total))}
      detail={`${compactCount(total.input)} input, ${compactCount(total.cache_write)} cache write, ${compactCount(total.output)} output`} />}
    <Tile label="Re-read from cache" value={compactCount(total.cache_read)} quiet
      detail={share === null ? "Billed far lower, not in Tokens" : `${share}% of everything read; billed far lower, not in Tokens`} />
    <Tile label="Runs" value={String(usage.runs)} detail={`${metrics.running} running now`} />
    <Tile label="Tasks" value={`${metrics.tasks_done} done`} detail={`${metrics.tasks_waiting} waiting, ${metrics.tasks_failed} failed`} />
    <Tile label="Merges" value={String(metrics.merges)} detail={`${metrics.merges_into_main} into main, ${metrics.merges_undone} undone`} />
    <Tile label="Lines" value={`+${compactCount(added)}`} detail={`−${compactCount(removed)} across ${sets} change sets`} />
  </div>;
}

function Chart({ metrics, sinceMs, nowMs, offset, reread }: { metrics: Metrics; sinceMs: number; nowMs: number; offset: number; reread: number }) {
  return <Panel title="Tokens over time" actions={<span className="mx-legend">
    <span><i className="in" />Input</span><span><i className="cw" />Cache write</span><span><i className="out" />Output</span>
    <span className="muted">Re-read from cache: {compactCount(reread)}, not drawn</span>
  </span>}>
    <ChartSvg bars={chartBars(metrics, sinceMs, nowMs)} bucketMs={metrics.bucket_ms} offset={offset}
      label={`Tokens per ${barWords(metrics.bucket_ms)}: input, cache write and output`} />
  </Panel>;
}

/** A project's name on the app page, the empty key being runs tied to no project. Inputs: the
 * filter, if any, and the project id. Output: the name. */
function projectLabel(filter: MetricsFilter | undefined, id: string): string {
  if (id === "") return "Unattributed";
  return id === REMOVED_PROJECTS ? REMOVED_LABEL : displayText(filter?.projects.find((project) => project.id === id)?.name ?? id);
}

/** The breakdown's rows and own columns for one choice. The app page (with a filter) adds By
 * project, names each agent's harnesses and accounts and each account's and harness's
 * projects, and shows a Project column on agents and runs across every project; the project
 * tab shows each agent's turns in the range, tool counts and account limits. Inputs: the choice, the project, the reads, the
 * names, now and the filter. Output: the first column's head, the own columns' heads, the
 * rows and whether a Project column shows. */
function breakdownRows(by: MetricsBy, projectId: string | null, usage: UsageReport, metrics: Metrics, adoption: Adoption | null, names: MetricsNames, nowMs: number, filter: MetricsFilter | undefined): { head: string; extra: string[]; rows: BreakdownRow[]; projectColumn: boolean } {
  const projectName = (id: string) => projectLabel(filter, id);
  const agent = (key: string, label?: string | null) => names.agents.get(key) ?? (label ? displayText(label) : agentLabel(key, projectName));
  const account = (key: string) => key === "" ? "Unattributed" : names.accounts.get(key) ?? key;
  const harness = (key: string) => HARNESS_NAMES[key as keyof typeof HARNESS_NAMES] ?? key;
  const listed = (kept: string[], more: number | undefined, show: (name: string) => string) =>
    [...kept.map(show), ...(more ? [`+${more} more`] : [])].join(", ");
  const across = filter !== undefined && projectId === null;
  const projectColumn = across && (by === "agent" || by === "run");
  if (by === "project") {
    return {
      head: "Project", extra: ["Agents", "Runs", "Merges"], projectColumn,
      rows: metrics.projects.map((row) => ({ key: row.key, name: projectName(row.key), tokens: row.tokens, extra: [String(row.agents), String(row.runs), String(row.merges)] })),
    };
  }
  if (by === "pool") {
    return {
      head: "Pool", extra: ["Runs", "Last used"], projectColumn,
      rows: usage.pools.map((row) => ({
        key: row.key, name: filter?.limits.pools.get(row.key) ?? row.key, tokens: row.tokens,
        notReportedBy: row.not_reported_by, extra: [String(row.runs), agoText(row.last_ms, nowMs)],
      })),
    };
  }
  if (by === "run") {
    return {
      head: "Run", extra: ["Agent", "Ended", "Result"], projectColumn,
      rows: metrics.runs.map((run) => ({
        key: `${run.project}/${run.agent}/${run.session}`,
        name: run.task !== null && run.title !== null ? `#${run.task} ${displayText(run.title)}` : "No task",
        project: projectColumn ? projectName(run.project) : undefined,
        tokens: run.tokens,
        extra: [agent(run.agent, run.label), run.status === "running" ? "running" : agoText(run.last_ms, nowMs), run.status === null ? "" : RESULTS[run.status] ?? run.status],
      })),
    };
  }
  if (by === "account" && filter) {
    return {
      head: "Account", extra: ["Agents", "Projects"], projectColumn,
      rows: metrics.accounts.map((row) => ({ key: row.key, name: account(row.key), tokens: row.tokens, extra: [String(row.agents), listed(row.projects, row.more_projects, projectName)] })),
    };
  }
  if (by === "account") {
    return {
      head: "Account", extra: ["Agents", "Limit left", "Resets"], projectColumn,
      rows: metrics.accounts.map((row) => {
        const limit = limitLeft(usage.limits.find((limits) => limits.account === row.key), nowMs);
        return { key: row.key, name: account(row.key), tokens: row.tokens, extra: [String(row.agents), limit.left, limit.resets] };
      }),
    };
  }
  if (by === "harness") {
    return {
      head: "Harness", extra: filter ? ["Agents", "Projects"] : ["Agents", "Runs"], projectColumn,
      rows: metrics.harnesses.map((row) => ({
        key: row.key, name: harness(row.key), tokens: row.tokens, extra: [String(row.agents), filter ? listed(row.projects, row.more_projects, projectName) : String(row.runs)],
      })),
    };
  }
  if (filter) {
    return {
      head: "Agent", extra: ["Harness", "Account"], projectColumn,
      rows: metrics.agents.map((row) => ({
        key: `${row.project}/${row.agent}`, usageKey: row.agent, name: agent(row.agent, row.label), project: projectColumn ? projectName(row.project) : undefined, tokens: row.tokens,
        extra: [listed(row.harnesses, row.more_harnesses, harness), listed(row.accounts, row.more_accounts, account)],
      })),
    };
  }
  const tools = new Map((adoption?.agents ?? []).map((row) => [agentKey(projectId, row), row]));
  const turns = new Map(metrics.agents.filter((row) => row.project === projectId).map((row) => [row.agent, row.turns]));
  const agentRow = (key: string, name: string, tokens: Tokens): BreakdownRow => {
    const used = tools.get(key);
    const skills = used ? (used.skills ?? []).reduce((sum, count) => sum + count, 0) + (used.other_skills ?? []).reduce((sum, other) => sum + other.count, 0) : null;
    const ended = turns.get(key);
    return {
      key, name, tokens,
      extra: [typeof ended === "number" ? String(ended) : "", ...(used ? [String(used.graphify ?? 0), String(used.context_mode ?? 0), String(skills)] : ["", "", ""])],
    };
  };
  const reported = new Set(usage.agents.map((row) => row.key));
  const turnsOnly = metrics.agents.filter((row) => row.project === projectId && !reported.has(row.agent) && (row.turns ?? 0) > 0 && headlineTokens(row.tokens) + row.tokens.cache_read === 0);
  return {
    head: "Agent", extra: ["Turns", "graphify", "ctx_*", "Skills"], projectColumn,
    rows: [...usage.agents.map((row) => agentRow(row.key, agent(row.key), row.tokens)), ...turnsOnly.map((row) => agentRow(row.agent, agent(row.agent, row.label), row.tokens))],
  };
}

type BreakdownParts = ReturnType<typeof breakdownRows>;

/** Whether two token counts match in every part. Inputs: the two counts. Output: the answer. */
function sameTokens(one: Tokens, other: Tokens): boolean {
  return one.input === other.input && one.output === other.output && one.cache_read === other.cache_read && one.cache_write === other.cache_write;
}

/** Adds what only the usage read knows to the app page's rows: when each was last used, its
 * runs when asked, and which harness ran for it without reporting usage. An agent's usage row
 * adds up every project, so its Runs and Last used stay blank unless it is plainly that row's
 * alone: no other row has the key and the tokens match. A usage row the metrics read has no
 * row for stays listed when its harness reported nothing. Inputs: the table, the usage rows,
 * whether to add Runs, the namer and now. Output: the table with its added columns. */
function withUsage(table: BreakdownParts, used: UsageRow[], runs: boolean, name: (key: string) => string, nowMs: number): BreakdownParts {
  const byKey = new Map(used.map((row) => [row.key, row]));
  const keyOf = (row: BreakdownRow) => row.usageKey ?? row.key;
  const rowsOfKey = new Map<string, number>();
  for (const row of table.rows) rowsOfKey.set(keyOf(row), (rowsOfKey.get(keyOf(row)) ?? 0) + 1);
  const isAlone = (row: BreakdownRow, match: UsageRow) => row.usageKey === undefined || (rowsOfKey.get(row.usageKey) === 1 && sameTokens(match.tokens, row.tokens));
  const cells = (row: UsageRow | undefined) => [...(runs ? [row ? String(row.runs) : ""] : []), row ? agoText(row.last_ms, nowMs) : ""];
  const kept = table.rows.map((row) => {
    const match = byKey.get(keyOf(row));
    return { ...row, notReportedBy: match?.not_reported_by, extra: [...row.extra, ...cells(match && isAlone(row, match) ? match : undefined)] };
  });
  const listed = new Set(table.rows.map(keyOf));
  const added = used.filter((row) => row.not_reported_by && !listed.has(row.key)).map((row) => ({
    key: row.key, name: name(row.key), tokens: row.tokens, notReportedBy: row.not_reported_by,
    extra: [...table.extra.map(() => ""), ...cells(row)],
  }));
  const columns = [...table.extra, ...(runs ? ["Runs"] : []), "Last used"];
  return { ...table, extra: columns, rows: [...kept, ...added] };
}

/** One app page panel's table: its breakdown with what only the usage read knows. Inputs: the
 * choice, the project, the reads, the names, now and the filter. Output: the table's parts. */
function appRows(by: MetricsBy, projectId: string | null, usage: UsageReport, metrics: Metrics, names: MetricsNames, nowMs: number, filter: MetricsFilter): BreakdownParts {
  const table = breakdownRows(by, projectId, usage, metrics, null, names, nowMs, filter);
  const project = (key: string) => projectLabel(filter, key);
  if (by === "project") return withUsage(table, usage.projects, false, project, nowMs);
  if (by === "agent") return withUsage(table, usage.agents, true, (key) => agentLabel(key, project), nowMs);
  if (by === "account") return withUsage(table, usage.accounts, true, (key) => rowName(names.accounts, key), nowMs);
  return table;
}

/** The breakdown table: each listed row with its four token parts, its own columns and its
 * share of tokens, then an Other row for what a capped list leaves out of the range, then the
 * Total, which is the Tokens tile's total. Every share is out of that total. Across all
 * projects each row also names its project. A row whose harness reported nothing says so in
 * place of its token counts. Inputs: the choice, the heads, the rows, whether to show the
 * project column and the range total. */
function BreakdownTable({ by, head, extra, rows, projectColumn, total }: { by: MetricsBy; head: string; extra: string[]; rows: BreakdownRow[]; projectColumn: boolean; total: Tokens }) {
  const left = (field: keyof Tokens) => Math.max(0, total[field] - rows.reduce((sum, row) => sum + row.tokens[field], 0));
  const other: Tokens = { input: left("input"), output: left("output"), cache_read: left("cache_read"), cache_write: left("cache_write") };
  const unlisted = other.input + other.output + other.cache_read + other.cache_write > 0;
  const headline = headlineTokens(total);
  const share = cacheShare(total);
  const percent = (tokens: Tokens) => headline === 0 ? 0 : Math.min(100, Math.round((headlineTokens(tokens) * 100) / headline));
  const bar = (tokens: Tokens) => <td><span className="mx-share"><span className="mx-bar"><i style={{ width: `${percent(tokens)}%` }} /></span><span className="mono small">{percent(tokens)}%</span></span></td>;
  const cells = (tokens: Tokens) => <>
    <td className="num">{compactCount(tokens.input)}</td>
    <td className="num">{compactCount(tokens.cache_write)}</td>
    <td className="num">{compactCount(tokens.output)}</td>
    <td className="num muted">{compactCount(tokens.cache_read)}</td>
  </>;
  return <div className="pj-table-wrap mx-table-wrap"><table className="pj-table mx-table" data-by={by}>
    <thead><tr>
      <th scope="col">{head}</th>{projectColumn && <th scope="col">Project</th>}<th scope="col" className="num">Tokens</th><th scope="col" className="num">Input</th>
      <th scope="col" className="num">Cache write</th><th scope="col" className="num">Output</th><th scope="col" className="num muted">Cache read</th>
      {extra.map((column) => <th scope="col" key={column}>{column}</th>)}<th scope="col">Share of tokens</th>
    </tr></thead>
    <tbody>
      {rows.map((row) => <tr key={row.key}>
        <th scope="row">{row.name}</th>
        {projectColumn && <td>{row.project}</td>}
        {row.notReportedBy && headlineTokens(row.tokens) === 0
          ? <td colSpan={5}>{tokenText(0, row.notReportedBy)}</td>
          : <><td className="num mx-headline">{tokenText(headlineTokens(row.tokens), row.notReportedBy)}</td>{cells(row.tokens)}</>}
        {row.extra.map((value, index) => <td key={extra[index]}>{value}</td>)}
        {bar(row.tokens)}
      </tr>)}
      {unlisted && <tr className="mx-other">
        <th scope="row" className="muted">Other, not listed</th>
        {projectColumn && <td />}
        <td className="num mx-headline">{compactCount(headlineTokens(other))}</td>
        {cells(other)}
        {extra.map((column) => <td key={column} />)}
        {bar(other)}
      </tr>}
      <tr className="mx-total">
        <th scope="row">Total</th>
        {projectColumn && <td />}
        <td className="num mx-headline">{compactCount(headline)}</td>
        {cells(total)}
        {extra.map((column) => <td key={column} />)}
        <td className="mono small muted">{share === null ? "" : `cache read ${share}% of all read`}</td>
      </tr>
    </tbody>
  </table></div>;
}

/** The project tab's breakdown: its table in a panel, with the switch that picks it. Inputs:
 * the switch and the table's parts. */
function Breakdown({ control, ...parts }: BreakdownParts & { by: MetricsBy; control: ReactNode; total: Tokens }) {
  return <Panel title="Breakdown" padded={false} actions={control}><BreakdownTable {...parts} /></Panel>;
}

/** The app page's words above its numbers: what it counts, that the counting is the old one,
 * and anything wrong with this reading. Inputs: the usage read, the last failed action and
 * now. */
function AppNotices({ usage, failure, nowMs }: { usage: UsageReport; failure: string; nowMs: number }) {
  return <>
    <p className="secondary">Tokens your agents used and each account's limits. Counts come from each run's own record; limits are checked every 10 minutes.</p>
    <Notice tone="warn"><strong>These numbers still use the old counting.</strong> The recount comes in the next update, and you will see old and new side by side once.</Notice>
    {failure && <Notice tone="danger" alert>{failure}</Notice>}
    {(usage.health ?? []).map((item) => <Notice key={item.step} tone={item.recovered_ms === null ? "warn" : "info"}>{healthText(item, nowMs)}</Notice>)}
    <PartialNotice homes={usage.partial_homes} />
  </>;
}

/** What the app page keeps while its reads are incomplete: a refused action's words and, once
 * the usage read holds, each account's limits and the next accounts. Without that read only
 * Check limits now stays. Inputs: the usage read, the filter, the names and now. */
function LimitsWhileReading({ usage, filter, names, nowMs }: { usage: Loaded<UsageReport>; filter: MetricsFilter; names: MetricsNames; nowMs: number }) {
  return <>
    {filter.limits.failure && <Notice tone="danger" alert>{filter.limits.failure}</Notice>}
    {usage.kind === "ok"
      ? <><AccountLimitsPanel limits={usage.value.limits} name={(key) => rowName(names.accounts, key)} nowMs={nowMs} controls={filter.limits} />{filter.limits.next}</>
      : <CheckLimits busy={filter.limits.busy} onRefresh={filter.limits.onRefresh} />}
  </>;
}

/** The Metrics page as a pure view: the range switch and what it covers, the tiles (tokens
 * with their three parts, cache re-reads apart, runs, tasks, merges and lines), the tokens chart and
 * the breakdown by agent, run, account or harness, each with its four token parts and its
 * share of tokens. With a filter (the app page) it also picks a project, breaks down by
 * project and shows each account's limits with the usage notices. Inputs: the project, null for every project, the usage and metrics reads, the
 * project's adoption read, the range, the breakdown, the names, the range start, now, when
 * the page last read, the viewer's offset, the filter and the actions. */
export function MetricsPage({ projectId, usage, metrics, adoption, range, by, names, sinceMs, nowMs, updatedMs, offset, filter, onRange, onBy, onRetry }: {
  projectId: string | null;
  usage: Loaded<UsageReport>;
  metrics: Loaded<Metrics>;
  adoption: Loaded<Adoption> | null;
  range: MetricsRange;
  by: MetricsBy;
  names: MetricsNames;
  sinceMs: number;
  nowMs: number;
  updatedMs: number | null;
  offset: number;
  filter?: MetricsFilter;
  onRange: (range: MetricsRange) => void;
  onBy: (by: MetricsBy) => void;
  onRetry: () => void;
}) {
  const ranges = <Segmented label="Range" value={range} options={filter ? RANGES : METRIC_RANGES} onValue={onRange} />;
  const covers = filter === undefined ? (projectId === null ? "All projects" : "This project only")
    : projectId === null ? `${filter.projects.length} projects` : displayText(filter.projects.find((project) => project.id === projectId)?.name ?? projectId);
  const tools = <div className="mx-tools">
    {filter ? <div className="pj-row">
      {ranges}
      <Select id="mx-project" label="Project" hideLabel value={projectId ?? ""} onValue={(id) => filter.onProject(id === "" ? null : id)}
        options={[{ value: "", label: "All projects" }, ...filter.projects.map((project) => ({ value: project.id, label: displayText(project.name) }))]} />
    </div> : ranges}
    <span className="mx-count">{covers} · {updatedText(updatedMs, nowMs)} · provider-reported usage per turn</span>
  </div>;
  const kept = filter && <LimitsWhileReading usage={usage} filter={filter} names={names} nowMs={nowMs} />;
  const reads = [usage, metrics];
  if (reads.some((read) => read.kind === "not_ready" || (read.kind === "error" && usageNotReady(read)))) {
    return filter && usage.kind === "ok" ? <div className="stack">{tools}<NotReady what="Metrics" />{kept}</div> : <NotReady what="Metrics" />;
  }
  if (usage.kind === "error" || metrics.kind === "error") {
    const failed = usage.kind === "error" ? usage : metrics.kind === "error" ? metrics : null;
    return <div className="stack">{tools}{kept}{failed && <LoadError message={usageRefusal(failed)} onRetry={onRetry} />}</div>;
  }
  if (usage.kind !== "ok" || metrics.kind !== "ok") return <div className="stack">{tools}{kept}<Skeleton shape="card" count={3} /></div>;
  const empty = headlineTokens(usage.value.total) === 0 && usage.value.lines.length === 0 && (usage.value.not_reported_by ?? []).length === 0;
  if (filter) return <div className="stack mx-page">
    {tools}
    <AppNotices usage={usage.value} failure={filter.limits.failure} nowMs={nowMs} />
    <LeadPanel usage={usage.value} metrics={metrics.value} sinceMs={sinceMs} nowMs={nowMs} offset={offset} />
    <Tiles usage={usage.value} metrics={metrics.value} lead />
    <AccountLimitsPanel limits={usage.value.limits} name={(key) => rowName(names.accounts, key)} nowMs={nowMs} controls={filter.limits} />
    {filter.limits.next}
    {empty ? <EmptyState icon="trending-up" title="No usage in this range" compact>Runs show up here a few seconds after an agent works.</EmptyState> : <>
      <div className="mx-panels">
        {APP_PANELS.map(({ by: choice, title }) => {
          const parts = appRows(choice, projectId, usage.value, metrics.value, names, nowMs, filter);
          return <TokenPanel key={choice} title={title} rows={parts.rows}><BreakdownTable by={choice} {...parts} total={usage.value.total} /></TokenPanel>;
        })}
        <LinesPanel rows={usage.value.lines} name={(key) => projectLabel(filter, key)} />
      </div>
      <DaysPanel days={usage.value.days} offset={offset} />
    </>}
  </div>;
  const table = breakdownRows(by, projectId, usage.value, metrics.value, adoption?.kind === "ok" ? adoption.value : null, names, nowMs, undefined);
  return <div className="stack mx-page">
    {tools}
    <Tiles usage={usage.value} metrics={metrics.value} />
    <Chart metrics={metrics.value} sinceMs={sinceMs} nowMs={nowMs} offset={offset} reread={usage.value.total.cache_read} />
    <Breakdown by={by} control={<Segmented label="Breakdown" value={by} options={BREAKDOWNS} onValue={onBy} />} {...table} total={usage.value.total} />
  </div>;
}

/** The projects whose team status names agents on the page: only the project tab's own,
 * which also gives each name its harness. The app page reads no team status; the metrics read
 * names its agents. Inputs: the project id or null, and the app page's filter. Output: the
 * project ids. */
export function namedProjects(projectId: string | null, filter: MetricsFilter | undefined): string[] {
  return filter || projectId === null ? [] : [projectId];
}

/** Each named project's team status paired with its id, the project tab's own and none on
 * the app page; a project whose read fails is left out, so its agents keep the metrics read's
 * names or their usage labels. Input: the project ids. Output: the pairs read, in order. */
async function teamStatuses(ids: string[]): Promise<Result<[string, TeamStatus][]>> {
  const read = await Promise.all(ids.map((id) => loadTeamStatus(id)));
  return { kind: "ok", value: read.flatMap((result, index): [string, TeamStatus][] => result.kind === "ok" ? [[ids[index], result.value]] : []) };
}

/** The Metrics page for one project, or every project when the id is null: reads usage and
 * metrics for the chosen range, 7 days at first, every 30 seconds, and its agents' names. The
 * project tab (no filter) also reads its tool counts and its team status for names with their
 * harness, and starts By agent; the app page (with a filter) takes agent names from the metrics
 * read. Inputs: the project id or null, and the app page's filter. */
export function MetricsView({ projectId, filter }: { projectId: string | null; filter?: MetricsFilter }) {
  const [range, setRange] = useState<MetricsRange>("7d");
  const [by, setBy] = useState<MetricsBy>("agent");
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [ticks, setTicks] = useState(0);
  const [updatedMs, setUpdatedMs] = useState<number | null>(null);
  const reload = Math.floor(ticks / RELOAD_TICKS);
  const revision = filter?.limits.revision;
  const sinceMs = rangeStart(range, new Date(nowMs)) ?? 0;
  const [usage, retryUsage] = useLoad(() => {
    const now = new Date();
    return loadUsage(rangeStart(range, now), offsetMinutes(now), projectId);
  }, [range, reload, projectId, revision]);
  const [metrics, retryMetrics] = useLoad(() => loadMetrics(rangeStart(range, new Date()) ?? 0, projectId), [range, reload, projectId, revision]);
  const named = namedProjects(projectId, filter);
  const [adoption] = useLoad<Adoption | null>(() => projectId === null || filter ? Promise.resolve({ kind: "ok" as const, value: null }) : loadAdoption(projectId), [projectId, reload, Boolean(filter)]);
  const [statuses] = useLoad(() => teamStatuses(named), [named.join("\n"), reload]);
  const [accounts] = useLoad(listAccounts, [], "accounts");
  useEffect(() => {
    const timer = setInterval(() => { setNowMs(Date.now()); setTicks((old) => old + 1); }, TICK_MS);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => { if (usage.kind === "ok" && metrics.kind === "ok") setUpdatedMs(Date.now()); }, [usage, metrics]);
  const agents = new Map<string, string>();
  for (const [id, status] of statuses.kind === "ok" ? statuses.value : []) {
    for (const agent of status.agents) {
      const harness = HARNESS_NAMES[agent.harness as keyof typeof HARNESS_NAMES] ?? agent.harness;
      agents.set(agentKey(id, { agent: agent.id, role: agent.role }), `${displayText(agent.label)} · ${harness}`);
    }
  }
  const names: MetricsNames = { agents, accounts: new Map(accounts.kind === "ok" ? accounts.value.map((row) => [row.id, row.name]) : []) };
  const adopted: Loaded<Adoption> | null = adoption.kind === "ok" ? (adoption.value === null ? null : { kind: "ok", value: adoption.value }) : adoption;
  return <MetricsPage projectId={projectId} usage={usage} metrics={metrics} adoption={adopted} range={range} by={by} names={names}
    sinceMs={sinceMs} nowMs={nowMs} updatedMs={updatedMs} offset={offsetMinutes(new Date(nowMs))} filter={filter}
    onRange={(next) => { setRange(next); setNowMs(Date.now()); }} onBy={setBy} onRetry={() => { retryUsage(); retryMetrics(); }} />;
}
