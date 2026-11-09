import type { Metrics, MetricsBucket, Tokens } from "../../api-usage";
import { compactCount, headlineTokens } from "./usage-model";

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const CHART = { width: 700, height: 200, left: 44, bottom: 22, top: 8, barMax: 54 };
const BARS_MAX = 64;
const LABELS_MAX = 12;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** Every chart bar from the range start to now, a bar with no usage counted as empty, so the
 * time axis has no gaps. All, a range start of zero, starts at the oldest bar the engine kept,
 * and when the bars would not all fit the newest are kept. Inputs: the metrics, the range start
 * and now. Output: the bars, oldest first. */
export function chartBars(metrics: Metrics, sinceMs: number, nowMs: number): MetricsBucket[] {
  assert(metrics.bucket_ms > 0, "a chart bar spans time");
  const counted = new Map(metrics.series.map((bar) => [bar.start_ms, bar.tokens]));
  const first_ms = sinceMs > 0 ? sinceMs : metrics.series[0]?.start_ms ?? nowMs;
  const bars_past = Math.floor((nowMs - first_ms) / metrics.bucket_ms);
  const start_ms = first_ms + Math.max(0, bars_past - (BARS_MAX - 1)) * metrics.bucket_ms;
  const bars: MetricsBucket[] = [];
  for (let start = start_ms; start <= nowMs && bars.length < BARS_MAX; start += metrics.bucket_ms) {
    bars.push({ start_ms: start, tokens: counted.get(start) ?? { input: 0, output: 0, cache_read: 0, cache_write: 0 } });
  }
  return bars;
}

/** A chart bar's label: the hour for bars shorter than a day, else the date, both read at the
 * fixed offset the range was cut with. Inputs: its start, the bar width and the viewer's
 * offset east of UTC in minutes. Output: "06:00" or "Nov 1". */
export function barLabel(startMs: number, bucketMs: number, offset: number): string {
  const local = new Date(startMs + offset * MINUTE);
  if (bucketMs >= DAY) return local.toLocaleDateString(undefined, { timeZone: "UTC", month: "short", day: "numeric" });
  return `${String(local.getUTCHours()).padStart(2, "0")}:${String(local.getUTCMinutes()).padStart(2, "0")}`;
}

/** The words the chart names its bars by. Input: the bar width. Output: "two hours", "day"
 * or "five days". */
export function barWords(bucketMs: number): string {
  return bucketMs < DAY ? "two hours" : bucketMs === DAY ? "day" : "five days";
}

/** The words over the chart: the bar under the pointer, or else the highest bar. Inputs: the
 * bars, the bar width, the offset and the index under the pointer, or null. Output: the
 * words. */
export function chartReadout(bars: MetricsBucket[], bucketMs: number, offset: number, hovered: number | null): string {
  const heights = bars.map((bar) => headlineTokens(bar.tokens));
  const top = heights.reduce((best, height, index) => height > heights[best] ? index : best, 0);
  if (bars.length === 0 || (hovered === null && heights[top] === 0)) return "No tokens in this range";
  const index = hovered ?? top;
  assert(index >= 0 && index < bars.length, "a readout names a drawn bar");
  return `${hovered === null ? "Highest, " : ""}${barLabel(bars[index].start_ms, bucketMs, offset)}: ${compactCount(heights[index])}`;
}

/** The stacked bars of input, cache write and output. A chart that passes `onBar` also says
 * which bar the pointer is over, through a strip the height of the chart so an empty bar
 * counts. Inputs: the bars, their width, the offset, the words for a screen reader and the
 * action. Output: the drawing. */
export function ChartSvg({ bars, bucketMs, offset, label, onBar }: {
  bars: MetricsBucket[];
  bucketMs: number;
  offset: number;
  label: string;
  onBar?: (index: number | null) => void;
}) {
  const max = Math.max(1, ...bars.map((bar) => headlineTokens(bar.tokens))) * 1.1;
  const { width, height, left, bottom, top, barMax } = CHART;
  const slot = (width - left) / Math.max(1, bars.length);
  const barWidth = Math.min(barMax, slot * 0.7);
  const labelEvery = Math.max(1, Math.ceil(bars.length / LABELS_MAX));
  const y = (value: number) => Math.round((height - bottom - (value / max) * (height - bottom - top)) * 10) / 10;
  const parts: [keyof Tokens, string, string][] = [["input", "in", "input"], ["cache_write", "cw", "cache write"], ["output", "out", "output"]];
  return <svg className="mx-chart" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label}>
    {[0, 0.5, 1].map((step) => <g key={step}>
      <line className="gl" x1={left} x2={width} y1={y(step * max)} y2={y(step * max)} />
      <text className="ax" x={left - 6} y={y(step * max) + 4} textAnchor="end">{step === 0 ? "0" : compactCount(Math.round(step * max))}</text>
    </g>)}
    {bars.map((bar, index) => {
      const x = Math.round((left + index * slot + (slot - barWidth) / 2) * 10) / 10;
      const words = barLabel(bar.start_ms, bucketMs, offset);
      let base = 0;
      return <g key={bar.start_ms} onPointerEnter={() => onBar?.(index)} onPointerLeave={() => onBar?.(null)}>
        {onBar && <rect className="hit" x={left + index * slot} y={0} width={slot} height={height} />}
        {parts.map(([field, className, part]) => {
          const value = bar.tokens[field];
          const from = base;
          base += value;
          return <rect key={field} className={className} x={x} y={y(base)} width={barWidth} height={Math.max(0, Math.round((y(from) - y(base)) * 10) / 10)} rx={2}>
            <title>{`${words}: ${compactCount(value)} ${part}`}</title>
          </rect>;
        })}
        {index % labelEvery === 0 && <text className="ax" x={x + barWidth / 2} y={height - 6} textAnchor="middle">{words}</text>}
      </g>;
    })}
  </svg>;
}
