import type { AccountLimits, Metrics, MetricsBucket, UsageReport } from "../../api-usage";
import type { Adoption } from "../../api-process";
import type { Loaded } from "../../state";

export type MetricsRange = "today" | "7d" | "30d";

export type MetricsBy = "agent" | "run" | "account" | "harness";

/** Names shown in place of usage keys: agents by usage key and accounts by id. */
export interface MetricsNames {
  agents: Map<string, string>;
  accounts: Map<string, string>;
}

/** Every chart bar from the range start to now, empty bars included. Inputs: the metrics, the
 * range start and now. Output: the bars, oldest first. */
export function chartBars(metrics: Metrics, sinceMs: number, nowMs: number): MetricsBucket[] {
  void sinceMs;
  void nowMs;
  return metrics.series;
}

/** A chart bar's label. Inputs: its start, the bar width and the viewer's offset east of UTC
 * in minutes. Output: the label. */
export function barLabel(startMs: number, bucketMs: number, offset: number): string {
  void bucketMs;
  void offset;
  return String(startMs);
}

/** An account's limit with less room left and when it resets. Inputs: the account's limits
 * and now. Output: the room left and the reset, empty when unknown. */
export function limitLeft(limits: AccountLimits | undefined, nowMs: number): { left: string; resets: string } {
  void limits;
  void nowMs;
  return { left: "", resets: "" };
}

/** The Metrics page as a pure view. */
export function MetricsPage(props: {
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
  onRange: (range: MetricsRange) => void;
  onBy: (by: MetricsBy) => void;
  onRetry: () => void;
}) {
  void props;
  return null;
}
