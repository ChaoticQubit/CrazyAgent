import { useState, type ReactNode } from "react";
import type { DayTokens, LinesRow, Tokens } from "../../api-usage";
import { Button, Panel } from "../../ui";
import { compactCount, dayLabel, headlineTokens, tokenText } from "./usage-model";

const PANEL_ROWS_SHOWN = 5;
const DAYS_SHOWN = 7;
const BAR_PERCENT_MIN = 2;

/** One breakdown row: its name, its project when the breakdown spans projects, its tokens, the
 * cells of the breakdown's own columns, the key the usage read names it by when that differs
 * from `key`, and the harness that ran for it without reporting usage. */
export interface BreakdownRow {
  key: string;
  name: string;
  project?: string;
  tokens: Tokens;
  extra: string[];
  usageKey?: string;
  notReportedBy?: string | null;
}

/** A bar's width against the largest row. Inputs: the value and the largest value. Output: a
 * CSS width, never so thin it disappears, and none at all for nothing. */
function barWidth(value: number, top: number): string {
  if (value === 0) return "0%";
  return `${Math.max(BAR_PERCENT_MIN, Math.round((value * 100) / top))}%`;
}

/** The disclosure that holds every row and column of a panel. Inputs: how many rows and the
 * full table. */
function AllColumns({ count, children }: { count: number; children: ReactNode }) {
  return <details className="mx-more"><summary>All columns, {count} {count === 1 ? "row" : "rows"}</summary>{children}</details>;
}

/** A compact panel of the first rows as the engine orders them: the largest by tokens, and
 * for runs the newest, since a run list is a timeline. Every row and column is one step away.
 * Nothing shows for an empty list. Inputs: the title, the rows and the full table. */
export function TokenPanel({ title, rows, children }: { title: string; rows: BreakdownRow[]; children: ReactNode }) {
  if (rows.length === 0) return null;
  const top = Math.max(1, ...rows.map((row) => headlineTokens(row.tokens)));
  return <Panel title={title}>
    <div role="table" aria-label={title}>
      {rows.slice(0, PANEL_ROWS_SHOWN).map((row) => <div className="mx-row" role="row" key={row.key}>
        <span className="mx-row-name" role="cell">{row.name}</span>
        <span className="mx-bar" aria-hidden="true"><i style={{ width: barWidth(headlineTokens(row.tokens), top) }} /></span>
        <span className={row.notReportedBy ? "mx-row-main mx-row-words" : "mx-row-main"} role="cell">{tokenText(headlineTokens(row.tokens), row.notReportedBy)}</span>
        <span className="mx-row-side" role="cell">{row.notReportedBy && headlineTokens(row.tokens) === 0 ? "" : `${compactCount(row.tokens.cache_read)} cache read`}</span>
      </div>)}
    </div>
    <AllColumns count={rows.length}>{children}</AllColumns>
  </Panel>;
}

function LinesTable({ rows, name }: { rows: LinesRow[]; name: (key: string) => string }) {
  return <div className="pj-table-wrap usage-table"><table className="pj-table">
    <thead><tr><th scope="col">Project</th><th scope="col">Change sets</th><th scope="col">Lines</th><th scope="col">Accepted commits</th><th scope="col">Lines accepted</th></tr></thead>
    <tbody>{rows.map((row) => <tr key={row.project}>
      <th scope="row">{name(row.project)}</th>
      <td>{row.change_sets}</td>
      <td>+{row.added} -{row.removed}</td>
      <td>{row.commits}</td>
      <td>+{row.committed_added} -{row.committed_removed}</td>
    </tr>)}</tbody>
  </table></div>;
}

/** Lines changed by project: the first rows with their added and removed lines, and every
 * column one step away. Inputs: the rows and the project namer. */
export function LinesPanel({ rows, name }: { rows: LinesRow[]; name: (key: string) => string }) {
  if (rows.length === 0) return null;
  const top = Math.max(1, ...rows.map((row) => row.added));
  return <Panel title="Lines changed by project">
    <div role="table" aria-label="Lines changed by project">
      {rows.slice(0, PANEL_ROWS_SHOWN).map((row) => <div className="mx-row" role="row" key={row.project}>
        <span className="mx-row-name" role="cell">{name(row.project)}</span>
        <span className="mx-bar" aria-hidden="true"><i style={{ width: barWidth(row.added, top) }} /></span>
        <span className="mx-row-main" role="cell">+{compactCount(row.added)}</span>
        <span className="mx-row-side" role="cell">−{compactCount(row.removed)}</span>
      </div>)}
    </div>
    <AllColumns count={rows.length}><LinesTable rows={rows} name={name} /></AllColumns>
  </Panel>;
}

/** By day: the newest seven days first, and a button for every day. Inputs: the days, oldest
 * first as the engine sends them, and the offset the engine cut them with. */
export function DaysPanel({ days, offset }: { days: DayTokens[]; offset: number }) {
  const [all, setAll] = useState(false);
  if (days.length === 0) return null;
  const newest = [...days].reverse();
  const shown = all ? newest : newest.slice(0, DAYS_SHOWN);
  const more = days.length > DAYS_SHOWN;
  return <Panel title="By day" padded={false} className="mx-days" actions={more
    ? <Button size="small" onClick={() => setAll(!all)}>{all ? `Show last ${DAYS_SHOWN} days` : `Show all ${days.length} days`}</Button> : undefined}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Day</th><th scope="col">Tokens</th><th scope="col">Input</th><th scope="col">Cache write</th><th scope="col">Output</th><th scope="col" className="muted">Cache read</th></tr></thead>
      <tbody>{shown.map((day) => <tr key={day.day_ms}>
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
