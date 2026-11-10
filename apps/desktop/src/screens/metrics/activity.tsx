import type { ActivityRow, ModelRow, MoneyView, ToolBucket } from "../../api-usage";
import { Panel } from "../../ui";
import { displayText } from "../conversation";
import { compactCount } from "../settings/usage";
import { ACTIVITY_NAMES, COST_LABEL, Figure, dollarsBeside, firstTryText } from "./money";

const DAY_MS = 86_400_000;

type BarLabel = (startMs: number, bucketMs: number) => string;

/** Says when a panel's counting began. Inputs: the hour it began and the bar label. Output: the
 * words, nothing before any count. */
function Since({ ms, label }: { ms: number | null; label: BarLabel }) {
  return ms === null ? null : <span className="muted">Since {label(ms, DAY_MS)}</span>;
}

/** Tokens and dollars per model. Inputs: the rows and the money view. Output: the panel,
 * nothing with no rows. */
export function Models({ rows, money }: { rows: ModelRow[]; money: MoneyView }) {
  if (rows.length === 0) return null;
  return <Panel title="By model">
    <table className="pj-table mx-models">
      <thead><tr><th scope="col">Model</th><th scope="col" className="num">{money.shown === "dollars" ? COST_LABEL : "Tokens"}{dollarsBeside(money) && <small> {COST_LABEL}</small>}</th></tr></thead>
      <tbody>{rows.map((row) => <tr key={row.model}><th scope="row">{row.model === "" ? "Model not recorded" : displayText(row.model)}</th>
        <td className="num"><Figure tokens={row.tokens} costNanos={row.cost_nanos} money={money} /></td></tr>)}</tbody>
    </table>
  </Panel>;
}

/** Turns per activity with the first-try rate under them. Inputs: the rows, when counting
 * began and the bar label. Output: the panel, nothing with no rows. */
export function Activities({ rows, since, label }: { rows: ActivityRow[]; since: number | null; label: BarLabel }) {
  if (rows.length === 0) return null;
  const line = firstTryText(rows);
  return <Panel title="By activity" actions={<Since ms={since} label={label} />}>
    <table className="pj-table mx-activity">
      <thead><tr><th scope="col">Activity</th><th scope="col" className="num">Turns</th><th scope="col" className="num">Editing turns</th><th scope="col" className="num">First try</th></tr></thead>
      <tbody>{rows.map((row) => <tr key={row.activity}><th scope="row">{ACTIVITY_NAMES[row.activity] ?? displayText(row.activity)}</th>
        <td className="num">{row.turns}</td><td className="num">{row.edit_turns}</td><td className="num">{row.first_try_turns}</td></tr>)}</tbody>
    </table>
    {line && <p className="mx-first-try">{line}</p>}
  </Panel>;
}

/** Groups tool rows by bar and keeps their order. Input: the rows, newest bar first. Output: one
 * entry per bar. */
function toolBars(rows: ToolBucket[]): { start_ms: number; tools: ToolBucket[] }[] {
  const bars = new Map<number, ToolBucket[]>();
  for (const row of rows) bars.set(row.start_ms, [...(bars.get(row.start_ms) ?? []), row]);
  return Array.from(bars, ([start_ms, tools]) => ({ start_ms, tools }));
}

/** The most-used tools of each bar. Inputs: the rows, when counting began, the bar width and
 * the bar label. Output: the panel, nothing with no rows. */
export function ToolsOverTime({ rows, since, bucketMs, label }: { rows: ToolBucket[]; since: number | null; bucketMs: number; label: BarLabel }) {
  if (rows.length === 0) return null;
  return <Panel title="Tools over time" actions={<Since ms={since} label={label} />}>
    <table className="pj-table mx-tools-time">
      <thead><tr><th scope="col">When</th><th scope="col">Tools</th></tr></thead>
      <tbody>{toolBars(rows).map((bar) => <tr key={bar.start_ms}><th scope="row">{label(bar.start_ms, bucketMs)}</th>
        <td>{bar.tools.map((row) => `${displayText(row.tool)} ${compactCount(row.count)}`).join(", ")}</td></tr>)}</tbody>
    </table>
  </Panel>;
}
