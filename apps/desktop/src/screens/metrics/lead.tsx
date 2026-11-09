import { useState } from "react";
import type { Metrics, MetricsBucket, UsageReport } from "../../api-usage";
import { Panel } from "../../ui";
import { barLabel, barWords, chartBars, chartReadout, ChartSvg } from "./chart";
import { compactCount, headlineTokens } from "./usage-model";

/** The chart's numbers as a table, for anyone who cannot read the bars. Inputs: the bars, their
 * width and the offset. Output: a table behind a disclosure. */
function ChartTable({ bars, bucketMs, offset }: { bars: MetricsBucket[]; bucketMs: number; offset: number }) {
  return <details className="mx-more">
    <summary>Show chart as a table</summary>
    <div className="pj-table-wrap mx-table-wrap"><table className="pj-table mx-table">
      <thead><tr><th scope="col">From</th><th scope="col" className="num">Tokens</th><th scope="col" className="num">Input</th>
        <th scope="col" className="num">Cache write</th><th scope="col" className="num">Output</th><th scope="col" className="num muted">Cache read</th></tr></thead>
      <tbody>{bars.map((bar) => <tr key={bar.start_ms}>
        <th scope="row">{barLabel(bar.start_ms, bucketMs, offset)}</th>
        <td className="num mx-headline">{compactCount(headlineTokens(bar.tokens))}</td>
        <td className="num">{compactCount(bar.tokens.input)}</td>
        <td className="num">{compactCount(bar.tokens.cache_write)}</td>
        <td className="num">{compactCount(bar.tokens.output)}</td>
        <td className="num muted">{compactCount(bar.tokens.cache_read)}</td>
      </tr>)}</tbody>
    </table></div>
  </details>;
}

/** The lead panel: the range's Tokens with what they are made of, beside the tokens over time
 * and the same numbers as a table. A range with only unreported usage says so, never zero. A
 * pointer left on a bar the new range no longer draws counts as no pointer, because a bar
 * that goes fires no leave event.
 * Inputs: the usage and metrics reads, the range start, now and the viewer's offset. */
export function LeadPanel({ usage, metrics, sinceMs, nowMs, offset }: {
  usage: UsageReport;
  metrics: Metrics;
  sinceMs: number;
  nowMs: number;
  offset: number;
}) {
  const [hovered, setHovered] = useState<number | null>(null);
  const total = usage.total;
  const unreported = (usage.not_reported_by ?? []).join(", ");
  const onlyUnreported = unreported !== "" && headlineTokens(total) === 0;
  const bars = chartBars(metrics, sinceMs, nowMs);
  const pointed = hovered !== null && hovered < bars.length ? hovered : null;
  const per = barWords(metrics.bucket_ms);
  const mix: [string, number][] = [["Input", total.input], ["Cache write", total.cache_write], ["Output", total.output], ["Re-read from cache", total.cache_read]];
  return <Panel className="mx-lead">
    <div className="mx-lead-grid">
      <div className="mx-lead-numbers">
        <span className="mx-lead-key">Tokens</span>
        <span className="mx-lead-value">{onlyUnreported ? "Not reported" : compactCount(headlineTokens(total))}</span>
        {onlyUnreported && <span className="mx-tile-detail">Usage not reported by {unreported}</span>}
        {!onlyUnreported && <dl className="mx-mix">{mix.map(([name, value]) => <div key={name}><dt>{name}</dt><dd>{compactCount(value)}</dd></div>)}</dl>}
        {unreported !== "" && !onlyUnreported && <p className="muted small">Not in these totals: usage not reported by {unreported}.</p>}
      </div>
      <div className="mx-lead-chart">
        <div className="mx-tools">
          <h3>Tokens per {per}</h3>
          <span className="mx-readout" aria-live="polite">{chartReadout(bars, metrics.bucket_ms, offset, pointed)}</span>
        </div>
        <ChartSvg bars={bars} bucketMs={metrics.bucket_ms} offset={offset} onBar={setHovered}
          label={`Tokens per ${per}: input, cache write and output. The table after the chart has the same numbers.`} />
        <span className="mx-legend">
          <span><i className="in" />Input</span><span><i className="cw" />Cache write</span><span><i className="out" />Output</span>
          <span className="muted">Re-read from cache not drawn</span>
        </span>
        <ChartTable bars={bars} bucketMs={metrics.bucket_ms} offset={offset} />
      </div>
    </div>
  </Panel>;
}
