import { useCallback, useEffect, useState } from "react";
import type { Result } from "../../api";
import { failureCopy, refreshRoadmapStatus, roadmapStatus, setRoadmapRefresh } from "../../api-projects";
import type { RoadmapItemState, RoadmapItemStatus, RoadmapRefresh, RoadmapStatus } from "../../api-projects";
import { Button, Segmented } from "../../ui";
import type { Tone } from "../../ui";
import { displayText } from "../conversation";
import { ago } from "./research";
import { Badge, NotReady } from "./shared";
import type { ProjectData } from "./shared";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/** The word and tone of each item status, in the order the counts line names them. */
export const STATUS_WORDS: Record<RoadmapItemStatus, { word: string; tone: Tone }> = {
  tested: { word: "Tested", tone: "ok" },
  done: { word: "Done", tone: "info" },
  in_progress: { word: "In progress", tone: "warn" },
  pending: { word: "Pending", tone: "neutral" },
};

/** Every choice of when a project's roadmap status is checked, as the setting lists them. */
export const REFRESH_OPTIONS: { value: RoadmapRefresh; label: string }[] = [
  { value: "after_landing", label: "After each landed change set and Refresh" },
  { value: "refresh_only", label: "Refresh only" },
  { value: "off", label: "Off" },
];

/** The choice a project has before the owner picks one. */
export const DEFAULT_REFRESH: RoadmapRefresh = "after_landing";

/** What the view says while the project's setting is Off. */
export const OFF_LINE = "Roadmap status is off in Project settings.";

const POLL_MS = 15_000;

/** The counts line: how many items are tested, done, in progress and pending, every status
 * named even at zero. Input: the item states. Output: the line. */
export function statusCounts(items: readonly RoadmapItemState[]): string {
  const names = Object.keys(STATUS_WORDS) as RoadmapItemStatus[];
  const counts = names.map((name) => items.filter((entry) => entry.status === name).length);
  assert(counts.reduce((sum, count) => sum + count, 0) === items.length, "every item has one of the known statuses");
  assert(names.length === 4, "the counts line names four statuses");
  return names.map((name, index) => `${counts[index]} ${STATUS_WORDS[name].word.toLowerCase()}`).join(", ");
}

/** The item states grouped by the heading they sit under, in roadmap order. Input: the item
 * states. Output: each section with its items. */
function sections(items: readonly RoadmapItemState[]): { section: string; items: RoadmapItemState[] }[] {
  const groups: { section: string; items: RoadmapItemState[] }[] = [];
  for (const entry of items) {
    const last = groups[groups.length - 1];
    if (last && last.section === entry.item.section) last.items.push(entry);
    else groups.push({ section: entry.item.section, items: [entry] });
  }
  assert(groups.reduce((sum, group) => sum + group.items.length, 0) === items.length, "grouping keeps every item");
  assert(groups.every((group) => group.items.length > 0), "no section is empty");
  return groups;
}

/** A project's roadmap status as a view: the counts, when it was checked and from which file,
 * what is happening now, the Refresh button, and each item under its section with its status
 * word and its evidence. Inputs: the status, now, whether a call runs, the last problem and
 * the Refresh handler. Output: the section. */
export function RoadmapStatusView({ status, now, busy, problem = "", onRefresh }: {
  status: RoadmapStatus; now: number; busy: boolean; problem?: string; onRefresh: () => void;
}) {
  const off = status.refresh === "off";
  const checked = status.checked_ms === null ? "Not checked yet" : `Checked ${ago(status.checked_ms, now).toLowerCase()}`;
  const failed = status.problem ? `The last check failed: ${status.problem.replace(/\.$/, "")}.` : "";
  return <section className="pj-stack-s" aria-labelledby="pj-roadmap-status"><h3 id="pj-roadmap-status">Roadmap status</h3>
    <div className="pj-row between">
      <strong>{statusCounts(status.items)}</strong>
      <Button pending={busy} pendingLabel="Asking..." disabled={status.running || off} onClick={onRefresh}>Refresh</Button>
    </div>
    <p className="pj-small pj-secondary" role="status">
      {status.running ? "Checking now. " : ""}{checked}{status.source ? `. From ${displayText(status.source)}` : ""}
    </p>
    {off && <p className="pj-small pj-secondary">{OFF_LINE}</p>}
    {status.stale && !status.running && !off
      && <p className="pj-small pj-secondary">A change set landed since this check. The next check runs about 10 minutes after the last landing.</p>}
    {(problem || failed) && <p className="pj-error" role="alert">{displayText(problem || failed)}</p>}
    {sections(status.items).map((group, at) => <div className="pj-stack-s" key={`${at}:${group.section}`}>
      {group.section && <h4>{displayText(group.section)}</h4>}
      <ul className="rs-items">
        {group.items.map((entry, index) => <li className="rs-item" key={`${index}:${entry.item.text}`}>
          <Badge word={STATUS_WORDS[entry.status].word} tone={STATUS_WORDS[entry.status].tone} />
          <span>{displayText(entry.item.text)}</span>
          {entry.evidence && <details className="rs-evidence"><summary>Evidence</summary><p>{displayText(entry.evidence)}</p></details>}
        </li>)}
      </ul>
    </div>)}
  </section>;
}

/** The "Roadmap status" setting as a view: every choice, the recommended one named, and the
 * last problem. Inputs: the saved choice, whether a save runs, the problem and the change
 * handler. Output: the field. */
export function RoadmapRefreshView({ value, busy, problem, onValue }: {
  value: RoadmapRefresh; busy: boolean; problem: string; onValue: (value: RoadmapRefresh) => void;
}) {
  const proposed = REFRESH_OPTIONS.find((option) => option.value === DEFAULT_REFRESH);
  assert(proposed !== undefined, "the default is one of the offered choices");
  return <div className="pj-field">
    <span className="pj-label" aria-hidden="true">Roadmap status</span>
    <Segmented label="Roadmap status" value={value} options={[...REFRESH_OPTIONS]} onValue={(next) => { if (!busy && next !== value) onValue(next); }} />
    <p className="pj-help">When CrazyAgent checks which roadmap items are pending, in progress, done or tested. Each check is one agent run on this project's pool. Recommended: {proposed.label}.</p>
    {problem && <p className="pj-error" role="alert">{displayText(problem)}</p>}
  </div>;
}

/** The words for a roadmap status call that did not go through. Input: the result. Output:
 * the words, empty when it went through. */
function problemOf(result: Result<unknown>): string {
  if (result.kind === "ok") return "";
  return result.kind === "not_ready" ? "Not ready yet: roadmap status isn't in this build." : failureCopy({ code: result.code, cause: result.cause });
}

/** A project's roadmap status under its planning line: read from the engine, read again every
 * few seconds while a check runs, with Refresh; in Project settings it shows the setting
 * instead of the items. Inputs: the project data and whether this is the settings page.
 * Output: the section, or a plain line while it cannot be read. */
export function RoadmapStatusPanel({ data, settings = false }: { data: ProjectData; settings?: boolean }) {
  const [read, setRead] = useState<Result<RoadmapStatus>>();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const load = useCallback(async () => { setRead(await roadmapStatus(data.id)); }, [data.id]);
  const waiting = read?.kind === "ok" && (read.value.running || read.value.stale);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    if (!waiting) return undefined;
    const timer = window.setInterval(() => { void load(); }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [waiting, load]);
  async function act(call: Promise<Result<void>>) {
    setBusy(true);
    setProblem("");
    setProblem(problemOf(await call));
    await load();
    setBusy(false);
  }
  if (read === undefined) return <p className="pj-muted" role="status">Loading roadmap status...</p>;
  if (read.kind === "not_ready") return <NotReady what="roadmap status isn't in this build." />;
  if (read.kind !== "ok") return <p className="pj-error" role="alert">Couldn't load the roadmap status. {problemOf(read)}</p>;
  if (settings) return <RoadmapRefreshView value={read.value.refresh} busy={busy} problem={problem}
    onValue={(refresh) => { void act(setRoadmapRefresh(data.id, refresh)); }} />;
  return <RoadmapStatusView status={read.value} now={data.now} busy={busy} problem={problem}
    onRefresh={() => { if (!busy) void act(refreshRoadmapStatus(data.id)); }} />;
}
