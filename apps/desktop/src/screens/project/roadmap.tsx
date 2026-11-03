import { useState } from "react";
import type { Result } from "../../api";
import { failureCopy } from "../../api-projects";
import { loadTicketSettings } from "../../api-ticket-settings";
import { trackerOp, trackerProjects } from "../../api-tracker";
import type { TrackerEnvelope, TrackerOpName } from "../../api-tracker";
import { useLoad, type Loaded } from "../../state";
import { Button, Checkbox, LoadError, Notice, NotReady, Skeleton } from "../../ui";
import { displayText } from "../conversation";
import type { ProjectData } from "./shared";
import "./roadmap.css";

/** The roadmap file planning writes, which "Import roadmap" reads. */
export const ROADMAP_PATH = ".crazyagent/context/roadmap.md";

/** The most versions and cycles one project holds, as the tracker bounds them. */
export const VERSIONS_MAX = 64;
export const CYCLES_MAX = 520;
const ROADMAP_PAGES_MAX = VERSIONS_MAX + CYCLES_MAX + 1;
const IMPORT_ITEMS_MAX = 256;
const DAY_MS = 86_400_000;
const DAY = /^([0-9]{4})-([0-9]{2})-([0-9]{2})$/;

/** A rollover policy: what closing a cycle does with its unfinished tickets. */
export type Rollover = "auto" | "ask" | "off";

/** One product version on the roadmap with its closed and total tickets. */
export interface RoadmapVersion {
  name: string;
  position: number;
  target_day: string | null;
  status: "planned" | "active" | "released";
  version: number;
  closed: number;
  total: number;
}

/** One cycle with its closed and total tickets, and the version it works toward. */
export interface RoadmapCycle {
  number: number;
  status: "upcoming" | "active" | "closed";
  starts_day: string;
  ends_day: string;
  release: string | null;
  version: number;
  closed: number;
  total: number;
}

/** A project's roadmap: versions by position, cycles by number and the tickets in no version. */
export interface Roadmap {
  unplanned: { closed: number; total: number };
  versions: RoadmapVersion[];
  cycles: RoadmapCycle[];
}

/** What closing a cycle did: the cycle, the policy it followed, how many tickets were
 * unfinished, the cycle they moved to (auto only) and their short ids. */
export interface CycleClosed {
  number: number;
  rollover: Rollover;
  unfinished: number;
  moved_to: number | null;
  tickets: string[];
}

/** One change an import would make, as the core previews it. */
export type ImportChange =
  | { change: "version_add"; name: string; position: number; target_day: string | null }
  | { change: "version_change"; name: string; position: number; target_day: string | null; was_position: number; was_target_day: string | null }
  | { change: "ticket_add"; version: string | null; kind: string; title: string };

/** A preview: each change with its key, and each part of the file left to a person. */
export interface ImportPreview {
  items: { key: string; change: ImportChange }[];
  unparsed: { heading: string; line_first: number; line_last: number; why: string; text: string }[];
}

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function count(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function dayOrNull(value: unknown): value is string | null {
  return value === null || (typeof value === "string" && dayNumber(value) !== null);
}

function tally(value: Record<string, unknown>): boolean {
  return count(value.closed) && count(value.total) && value.closed <= value.total;
}

/** The day number of a `YYYY-MM-DD` day, counted from 1970-01-01. Input: the day. Output:
 * the number, or null for text that is not a calendar day. */
export function dayNumber(day: string): number | null {
  const match = DAY.exec(day);
  if (!match) return null;
  const [year, month, date] = [Number(match[1]), Number(match[2]), Number(match[3])];
  const at = Date.UTC(year, month - 1, date);
  const back = new Date(at);
  if (back.getUTCFullYear() !== year || back.getUTCMonth() !== month - 1 || back.getUTCDate() !== date) return null;
  return at / DAY_MS;
}

/** Today on this Mac's calendar as `YYYY-MM-DD`. Input: the time. Output: the day. */
export function todayText(now: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** Closed out of total as a whole percent, rounded down so a version shows 100 only when
 * every ticket is done. Inputs: closed and total tickets. Output: the percent and its line. */
export function progress(closed: number, total: number): { percent: number; line: string } {
  assert(count(closed) && count(total), "ticket counts are whole numbers");
  assert(closed <= total, "closed tickets are some of the total");
  if (total === 0) return { percent: 0, line: "No tickets yet" };
  const percent = Math.floor((closed * 100) / total);
  assert(percent >= 0 && percent <= 100, "a percent is 0 to 100");
  return { percent, line: `${closed} of ${total} done (${percent}%)` };
}

/** Where a cycle stands on a day: not started, days left counting today and its last day,
 * or ended. Inputs: the cycle and today. Output: the line. */
export function cycleTime(cycle: Pick<RoadmapCycle, "starts_day" | "ends_day" | "status">, today: string): string {
  const [starts, ends, now] = [dayNumber(cycle.starts_day), dayNumber(cycle.ends_day), dayNumber(today)];
  assert(starts !== null && ends !== null && now !== null, "cycle days are calendar days");
  assert(starts <= ends, "a cycle ends on or after its first day");
  if (cycle.status === "closed") return `Closed, ran ${ends - starts + 1} days`;
  if (now < starts) return now + 1 === starts ? "Starts tomorrow" : `Starts in ${starts - now} days`;
  const left = ends - now + 1;
  if (left === 1) return "Last day";
  if (left > 1) return `${left} days left`;
  return -left + 1 === 1 ? "Ended yesterday" : `Ended ${-left + 1} days ago`;
}

/** How a version stands against its target day. Inputs: the version and today. Output: the
 * line. */
export function targetLine(version: Pick<RoadmapVersion, "target_day" | "status">, today: string): string {
  if (version.status === "released") return "Released";
  if (version.target_day === null) return "No target day";
  const [target, now] = [dayNumber(version.target_day), dayNumber(today)];
  assert(target !== null && now !== null, "target days are calendar days");
  const days = target - now;
  if (days === 0) return `Due today, ${version.target_day}`;
  if (days > 0) return `Due in ${days} ${days === 1 ? "day" : "days"}, ${version.target_day}`;
  return `${-days} ${days === -1 ? "day" : "days"} past ${version.target_day}`;
}

/** What closing a cycle will do under the project's rollover policy. Input: the policy.
 * Output: the line under Close cycle. */
export function rolloverLine(rollover: Rollover): string {
  switch (rollover) {
    case "auto": return "Unfinished tickets move to the next cycle, which starts if none is open.";
    case "ask": return "You'll move each unfinished ticket yourself after the cycle closes.";
    case "off": return "Unfinished tickets stay in the closed cycle.";
  }
}

/** What a cycle close did, in plain words. Input: the close reply. Output: the line. */
export function closedLine(closed: CycleClosed): string {
  assert(closed.tickets.length <= closed.unfinished, "the listed tickets are the unfinished ones");
  const head = `Cycle ${closed.number} closed.`;
  if (closed.unfinished === 0) return `${head} Every ticket in it was done.`;
  const one = closed.unfinished === 1;
  const tickets = one ? "1 unfinished ticket" : `${closed.unfinished} unfinished tickets`;
  switch (closed.rollover) {
    case "auto":
      assert(closed.moved_to !== null, "an auto rollover names the cycle it moved to");
      return `${head} ${tickets} moved to cycle ${closed.moved_to}.`;
    case "ask": return `${head} ${tickets} ${one ? "waits" : "wait"} for you: move ${one ? "it" : "each one"} below.`;
    case "off": return `${head} ${tickets} ${one ? "stays" : "stay"} in it.`;
  }
}

/** The cycle unfinished tickets can move to after a close: the first upcoming or active
 * cycle numbered after it. Inputs: the cycles and the closed cycle's number. Output: its
 * number, or null when none is open. */
export function moveTarget(cycles: readonly RoadmapCycle[], after: number): number | null {
  const open = cycles.filter((cycle) => cycle.status !== "closed" && cycle.number > after).map((cycle) => cycle.number);
  return open.length === 0 ? null : Math.min(...open);
}

/** One import change as a line of the diff, agent and file text made visible. Input: the
 * change. Output: the line. */
export function importLine(change: ImportChange): string {
  const target = (day: string | null) => day ?? "no target";
  switch (change.change) {
    case "version_add": return `Add version ${displayText(change.name)} at place ${change.position + 1}, ${target(change.target_day)}`;
    case "version_change": {
      const moves = [];
      if (change.was_position !== change.position) moves.push(`place ${change.was_position + 1} to ${change.position + 1}`);
      if (change.was_target_day !== change.target_day) moves.push(`${target(change.was_target_day)} to ${target(change.target_day)}`);
      assert(moves.length > 0, "a version change changes its place or target");
      return `Change version ${displayText(change.name)}: ${moves.join(", ")}`;
    }
    case "ticket_add": return `New ${displayText(change.kind)}${change.version === null ? "" : ` in ${displayText(change.version)}`}: ${displayText(change.title)}`;
  }
}

const UNPARSED_WHY: Record<string, string> = {
  before_first_section: "text before the first version heading",
  not_a_version: "the heading names no version",
  no_checklist_item: "the section has no checkbox item",
  target_not_a_day: "the target isn't a YYYY-MM-DD day",
  second_target: "the section names a second target",
  second_section: "a second section for the same version",
  item_refused: "the item's text can't be a ticket title",
};

/** One part of the file the import leaves to a person, as a line. Input: the part. Output:
 * the line naming its lines, heading and why. */
export function unparsedLine(part: ImportPreview["unparsed"][number]): string {
  const lines = part.line_first === part.line_last ? `Line ${part.line_first}` : `Lines ${part.line_first} to ${part.line_last}`;
  const heading = part.heading ? ` (${displayText(part.heading)})` : "";
  return `${lines}${heading}: ${UNPARSED_WHY[part.why] ?? "not imported"}`;
}

/** The keys an apply accepts: every previewed item the owner did not skip, in file order.
 * Inputs: the preview and the skipped keys. Output: the keys, empty when everything is
 * skipped. */
export function acceptedKeys(preview: ImportPreview, skipped: ReadonlySet<string>): string[] {
  assert(preview.items.length <= IMPORT_ITEMS_MAX, "a preview lists at most 256 items");
  const keys = preview.items.map((item) => item.key).filter((key) => !skipped.has(key));
  assert(new Set(keys).size === keys.length, "each preview item has its own key");
  return keys;
}

function versionOf(value: unknown): RoadmapVersion | null {
  if (!record(value) || typeof value.name !== "string" || !count(value.position) || !dayOrNull(value.target_day)) return null;
  if (!["planned", "active", "released"].includes(value.status as string) || !count(value.version) || !tally(value)) return null;
  return value as unknown as RoadmapVersion;
}

function cycleOf(value: unknown): RoadmapCycle | null {
  if (!record(value) || !count(value.number) || typeof value.starts_day !== "string" || typeof value.ends_day !== "string") return null;
  const [starts, ends] = [dayNumber(value.starts_day), dayNumber(value.ends_day)];
  if (starts === null || ends === null || starts > ends) return null;
  if (!["upcoming", "active", "closed"].includes(value.status as string) || !count(value.version) || !tally(value)) return null;
  if (value.release !== null && typeof value.release !== "string") return null;
  return value as unknown as RoadmapCycle;
}

/** Reads one roadmap page the core sent, refusing a page of another shape. Input: the
 * envelope's data. Output: the page, or null. */
export function roadmapPage(data: unknown): Roadmap | null {
  if (!record(data) || !record(data.unplanned) || !tally(data.unplanned) || !Array.isArray(data.versions) || !Array.isArray(data.cycles)) return null;
  const versions = data.versions.map(versionOf);
  const cycles = data.cycles.map(cycleOf);
  if (versions.some((item) => item === null) || cycles.some((item) => item === null)) return null;
  const unplanned = { closed: data.unplanned.closed as number, total: data.unplanned.total as number };
  return { unplanned, versions: versions as RoadmapVersion[], cycles: cycles as RoadmapCycle[] };
}

/** Joins a roadmap's pages in order, refusing a version or cycle that comes twice. Input: the
 * pages, first first. Output: the roadmap, or null. */
export function joinedRoadmap(pages: readonly Roadmap[]): Roadmap | null {
  assert(pages.length > 0 && pages.length <= ROADMAP_PAGES_MAX, "a roadmap is one to ROADMAP_PAGES_MAX pages");
  const versions = pages.flatMap((page) => page.versions);
  const cycles = pages.flatMap((page) => page.cycles);
  if (versions.length > VERSIONS_MAX || cycles.length > CYCLES_MAX) return null;
  if (new Set(versions.map((item) => item.name)).size !== versions.length) return null;
  if (new Set(cycles.map((item) => item.number)).size !== cycles.length) return null;
  if (cycles.filter((cycle) => cycle.status === "active").length > 1) return null;
  return { unplanned: pages[0].unplanned, versions, cycles };
}

/** Reads what a cycle close did, refusing another shape. Input: the envelope's data.
 * Output: the outcome, or null. */
export function cycleClosed(data: unknown): CycleClosed | null {
  if (!record(data) || !record(data.closed) || !count(data.closed.number) || !count(data.unfinished)) return null;
  if (!["auto", "ask", "off"].includes(data.rollover as string)) return null;
  if (data.moved_to !== null && !count(data.moved_to)) return null;
  if (!Array.isArray(data.tickets) || !data.tickets.every((id) => typeof id === "string")) return null;
  if (data.tickets.length > data.unfinished || (data.rollover === "auto" && data.unfinished > 0 && data.moved_to === null)) return null;
  return { number: data.closed.number, rollover: data.rollover as Rollover, unfinished: data.unfinished, moved_to: data.moved_to as number | null, tickets: data.tickets as string[] };
}

/** Reads an import preview, refusing another shape. Input: the envelope's data. Output: the
 * preview, or null. */
export function importPreview(data: unknown): ImportPreview | null {
  if (!record(data) || !Array.isArray(data.items) || !Array.isArray(data.unparsed) || data.items.length > IMPORT_ITEMS_MAX) return null;
  const changes = ["version_add", "version_change", "ticket_add"];
  const items = data.items.every((item) => record(item) && typeof item.key === "string" && record(item.change) && changes.includes(item.change.change as string));
  const unparsed = data.unparsed.every((part) => record(part) && typeof part.heading === "string" && count(part.line_first) && count(part.line_last)
    && typeof part.why === "string" && typeof part.text === "string");
  return items && unparsed ? data as unknown as ImportPreview : null;
}

const ROADMAP_REFUSALS: Record<string, string> = {
  stale_version: "This changed since you opened it. The roadmap now shows the saved state; try again.",
  project_not_attached: "Tickets aren't set up for this project yet. Set them up on the Tickets tab.",
  import_path_refused: `CrazyAgent can't read ${ROADMAP_PATH}. Planning writes it; run planning first.`,
  import_not_text: `${ROADMAP_PATH} isn't a text file.`,
  import_too_large: `${ROADMAP_PATH} is too large to import.`,
  import_format_unknown: `${ROADMAP_PATH} doesn't open with "# Roadmap" or "# Release checklist".`,
  import_item_unknown: "The roadmap file changed since the preview. Preview it again.",
  versions_full: "This project already has 64 versions.",
  rate_limited: "Too many changes in a minute. Wait a moment and try again.",
  op_unavailable: "This build can't do that yet.",
  roadmap_shape: "The engine's answer didn't read as a roadmap. Reload and try again.",
};

/** A refused roadmap action in plain words. Input: the code. Output: the sentence. */
export function roadmapRefusal(code: string): string {
  return Object.hasOwn(ROADMAP_REFUSALS, code) ? ROADMAP_REFUSALS[code] : failureCopy(code);
}

async function op<T>(name: TrackerOpName, args: Record<string, unknown>, read: (data: unknown) => T | null): Promise<Result<{ data: T; cursor: string | null }>> {
  const result: Result<TrackerEnvelope> = await trackerOp(name, args);
  if (result.kind !== "ok") return result;
  const envelope = result.value;
  if (!envelope.ok) return { kind: "error", code: envelope.code, message: roadmapRefusal(envelope.code) };
  const data = read(envelope.data);
  if (data === null) return { kind: "error", code: "roadmap_shape", message: roadmapRefusal("roadmap_shape") };
  return { kind: "ok", value: { data, cursor: envelope.cursor } };
}

/** Reads a project's whole roadmap, every page in order. Input: the project's key. Output:
 * the roadmap, or the refusal. */
export async function readRoadmap(project: string): Promise<Result<Roadmap>> {
  const pages: Roadmap[] = [];
  let cursor: string | null = null;
  for (let read = 0; read < ROADMAP_PAGES_MAX; read += 1) {
    const page: Result<{ data: Roadmap; cursor: string | null }> = await op("roadmap", cursor === null ? { project } : { project, cursor }, roadmapPage);
    if (page.kind !== "ok") return page;
    pages.push(page.value.data);
    if (page.value.cursor === null) {
      const roadmap = joinedRoadmap(pages);
      return roadmap === null ? { kind: "error", code: "roadmap_shape", message: roadmapRefusal("roadmap_shape") } : { kind: "ok", value: roadmap };
    }
    if (page.value.cursor === cursor) break;
    cursor = page.value.cursor;
  }
  return { kind: "error", code: "roadmap_shape", message: roadmapRefusal("roadmap_shape") };
}

function ProgressBar({ closed, total, label }: { closed: number; total: number; label: string }) {
  const shown = progress(closed, total);
  return <div className="rm-progress">
    <div className="rm-bar" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={shown.percent}>
      <span style={{ width: `${shown.percent}%` }} />
    </div>
    <span className="small muted">{shown.line}</span>
  </div>;
}

/** Everything the Roadmap tab shows and the actions it offers, without engine calls. */
export interface RoadmapViewProps {
  roadmap: Roadmap;
  today: string;
  rollover: Rollover | null;
  closed: CycleClosed | null;
  preview: ImportPreview | null;
  skipped: ReadonlySet<string>;
  busy: boolean;
  note: string;
  onStart: () => void;
  onClose: (cycle: RoadmapCycle) => void;
  onMove: (ticket: string, cycle: number) => void;
  onPreview: () => void;
  onSkip: (key: string, skip: boolean) => void;
  onApply: () => void;
  onCancelImport: () => void;
}

/** A project's roadmap as a pure view: the active cycle with its days left, progress, Close
 * cycle and what closing does; the last close's outcome with a move for each ticket the owner
 * places; every version with its target and progress; the other cycles; and Import roadmap
 * with the preview's changes to accept or skip. Input: the view props. Output: the tab body. */
export function RoadmapView(props: RoadmapViewProps) {
  const { roadmap, today, closed, preview, skipped, busy } = props;
  const active = roadmap.cycles.find((cycle) => cycle.status === "active") ?? null;
  const others = roadmap.cycles.filter((cycle) => cycle.status !== "active");
  const target = closed === null ? null : moveTarget(roadmap.cycles, closed.number);
  const accepted = preview === null ? [] : acceptedKeys(preview, skipped);
  return <div className="stack rm-page">
    <section className="rm-card" aria-label="Active cycle">
      <h3 className="group-title">Active cycle</h3>
      {active === null ? <p className="muted">No cycle is running.</p> : <>
        <p className="rm-title">Cycle {active.number}{active.release === null ? "" : ` for ${displayText(active.release)}`}</p>
        <p className="small muted">{active.starts_day} to {active.ends_day}, {cycleTime(active, today)}</p>
        <ProgressBar closed={active.closed} total={active.total} label={`Cycle ${active.number} progress`} />
        <span className="row"><Button variant="secondary" size="small" disabled={busy} onClick={() => props.onClose(active)}>Close cycle {active.number}</Button></span>
        {props.rollover !== null && <p className="helper">{rolloverLine(props.rollover)}</p>}
      </>}
      <span className="row"><Button variant="quiet" size="small" disabled={busy} onClick={props.onStart}>{active === null ? "Start a cycle" : "Plan the next cycle"}</Button></span>
    </section>
    {closed !== null && <section className="rm-card" aria-label="Cycle closed">
      <p role="status">{closedLine(closed)}</p>
      {closed.rollover === "ask" && closed.tickets.length > 0 && <ul className="rm-list">
        {closed.tickets.map((ticket) => <li key={ticket} className="rm-item">
          <span className="mono">{displayText(ticket)}</span>
          {target === null ? <span className="small muted">Start the next cycle to move it.</span>
            : <Button variant="quiet" size="small" disabled={busy} onClick={() => props.onMove(ticket, target)}>Move to cycle {target}</Button>}
        </li>)}
      </ul>}
    </section>}
    <section className="rm-card" aria-label="Versions">
      <h3 className="group-title">Versions ({roadmap.versions.length})</h3>
      {roadmap.versions.length === 0 ? <p className="muted">No versions yet. Import the roadmap planning writes, or add one from a ticket.</p>
        : <ul className="rm-list">{roadmap.versions.map((version) => <li key={version.name} className="rm-item">
          <span className="rm-title">{displayText(version.name)}</span>
          <span className="small muted">{version.status}, {targetLine(version, today)}</span>
          <ProgressBar closed={version.closed} total={version.total} label={`${displayText(version.name)} progress`} />
        </li>)}</ul>}
      <p className="small muted">Not in a version: {progress(roadmap.unplanned.closed, roadmap.unplanned.total).line}</p>
    </section>
    {others.length > 0 && <section className="rm-card" aria-label="Other cycles">
      <h3 className="group-title">Other cycles ({others.length})</h3>
      <ul className="rm-list">{others.map((cycle) => <li key={cycle.number} className="rm-item">
        <span className="rm-title">Cycle {cycle.number}{cycle.release === null ? "" : ` for ${displayText(cycle.release)}`}</span>
        <span className="small muted">{cycle.starts_day} to {cycle.ends_day}, {cycleTime(cycle, today)}</span>
        <ProgressBar closed={cycle.closed} total={cycle.total} label={`Cycle ${cycle.number} progress`} />
      </li>)}</ul>
    </section>}
    <section className="rm-card" aria-label="Import roadmap">
      <h3 className="group-title">Import roadmap</h3>
      {preview === null ? <>
        <p className="helper">Reads {ROADMAP_PATH} and shows what would change before anything does.</p>
        <span className="row"><Button variant="quiet" size="small" disabled={busy} onClick={props.onPreview}>Import roadmap</Button></span>
      </> : <>
        {preview.items.length === 0 ? <p className="muted">The roadmap matches what's here. Nothing to import.</p>
          : <ul className="rm-list">{preview.items.map((item) => <li key={item.key} className="rm-item">
            <Checkbox label={importLine(item.change)} checked={!skipped.has(item.key)} onChecked={(checked) => props.onSkip(item.key, !checked)} />
          </li>)}</ul>}
        {preview.unparsed.length > 0 && <>
          <p className="helper">Not imported; edit the file or add these yourself:</p>
          <ul className="rm-list">{preview.unparsed.map((part) => <li key={`${part.line_first}-${part.line_last}`} className="small muted">{unparsedLine(part)}</li>)}</ul>
        </>}
        <span className="row">
          <Button variant="primary" size="small" pending={busy} pendingLabel="Importing..." disabled={accepted.length === 0} onClick={props.onApply}>
            {accepted.length === 1 ? "Apply 1 change" : `Apply ${accepted.length} changes`}
          </Button>
          <Button variant="quiet" size="small" disabled={busy} onClick={props.onCancelImport}>Cancel</Button>
        </span>
      </>}
    </section>
    {props.note && <p className="helper" role="status">{props.note}</p>}
  </div>;
}

function shown<T>(loaded: Loaded<T>, what: string, retry: () => void) {
  if (loaded.kind === "loading") return <Skeleton shape="card" />;
  if (loaded.kind === "not_ready") return <NotReady what={what} />;
  if (loaded.kind === "error" && loaded.code === "project_not_attached") return <Notice tone="info">{roadmapRefusal("project_not_attached")}</Notice>;
  if (loaded.kind === "error") return <LoadError message={loaded.message} onRetry={retry} />;
  return null;
}

/** The Roadmap tab: finds the project's tickets, reads its roadmap and its rollover setting,
 * and runs start, close, move and import as the owner, reading the roadmap again after
 * each. Input: the project data. Output: the tab. */
export function RoadmapTab({ data }: { data: ProjectData }) {
  const [projects, reloadProjects] = useLoad(trackerProjects, []);
  const key = projects.kind === "ok" ? projects.value.find((item) => item.project === data.id)?.key ?? null : null;
  const [roadmap, reload] = useLoad(() => key === null ? Promise.resolve<Result<Roadmap>>({ kind: "error", code: "project_not_attached", message: "" }) : readRoadmap(key), [key]);
  const [settings] = useLoad(() => loadTicketSettings(data.id), [data.id]);
  const [closed, setClosed] = useState<CycleClosed | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [skipped, setSkipped] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const early = shown(projects, "Roadmap", reloadProjects) ?? (key === null ? shown({ kind: "error", code: "project_not_attached", message: "" }, "Roadmap", reload) : shown(roadmap, "Roadmap", reload));
  if (early !== null || roadmap.kind !== "ok" || key === null) return early;
  const policy = settings.kind === "ok" ? settings.value.keys.find((row) => row.key === "rollover")?.effective : undefined;
  const rollover: Rollover | null = policy === "auto" || policy === "ask" || policy === "off" ? policy : null;
  const act = async <T,>(run: () => Promise<Result<T>>, done: (value: T) => void) => {
    setBusy(true);
    setNote("");
    const result = await run();
    setBusy(false);
    if (result.kind === "ok") done(result.value);
    else setNote(result.kind === "not_ready" ? "Not ready yet: the roadmap isn't in this build." : result.message);
    reload();
  };
  const show = (id: string) => op("show", { id }, (value) => record(value) && record(value.ticket) && count(value.ticket.version) ? value.ticket.version : null);
  return <RoadmapView roadmap={roadmap.value} today={todayText(new Date())} rollover={rollover} closed={closed} preview={preview} skipped={skipped} busy={busy} note={note}
    onStart={() => void act(() => op("cycle_start", { project: key }, (value) => value), () => setNote("Cycle started."))}
    onClose={(cycle) => void act(() => op("cycle_close", { project: key, number: cycle.number, version: cycle.version }, cycleClosed), (value) => setClosed(value.data))}
    onMove={(ticket, cycle) => void act(async () => {
      const read = await show(ticket);
      if (read.kind !== "ok") return read;
      return op("edit", { id: ticket, cycle, version: read.value.data }, (value) => value);
    }, () => setClosed((current) => current === null ? null : { ...current, tickets: current.tickets.filter((id) => id !== ticket) }))}
    onPreview={() => void act(() => op("import_preview", { project: key, path: ROADMAP_PATH }, importPreview), (value) => { setPreview(value.data); setSkipped(new Set()); })}
    onSkip={(item, skip) => setSkipped((current) => { const next = new Set(current); if (skip) next.add(item); else next.delete(item); return next; })}
    onApply={() => {
      if (preview === null) return;
      const accept = acceptedKeys(preview, skipped);
      if (accept.length === 0) return;
      void act(() => op("import_apply", { project: key, path: ROADMAP_PATH, accept }, (value) => value), () => { setPreview(null); setNote(accept.length === 1 ? "Imported 1 change." : `Imported ${accept.length} changes.`); });
    }}
    onCancelImport={() => { setPreview(null); setSkipped(new Set()); }} />;
}
