import { useState } from "react";
import { call, knownError, namedFailure, type Result } from "../../api";
import { useAppSession, useLoad, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Notice, Panel, Segmented, Skeleton, Switch, TextField } from "../../ui";
import { agoText, dayLabel, offsetMinutes } from "./usage";

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;
const NOT_READY = "This isn't ready in this build yet.";
const DAY_SHAPE = /^\d{4}-\d{2}-\d{2}$/u;

export type ScorecardWindow = "rolling" | { from_day: string };
export type WindowChoice = "rolling" | "from_day";
export type ScorecardBaseline = "first_days" | "previous_window";
export type DailyQuestion = "off" | "on";

export interface ScorecardSettings {
  window: ScorecardWindow;
  baseline: ScorecardBaseline;
  daily_question: DailyQuestion;
}

export interface Span {
  start_ms: number;
  end_ms: number;
}

export type Flaw = "discarded" | "owner_edited" | "first_batch_not_passed";

export interface ScorecardTask {
  ticket: string;
  title: string;
  closed_ms: number;
  change_sets: string[];
  clean: boolean;
  flaws: Flaw[];
}

export interface Tally {
  messages: number;
  answers: number;
  interventions: number;
  tasks: number;
}

export interface Coordination {
  baseline: Tally;
  measured: Tally;
}

export type ActKind = "owner_message" | "answer" | "owner_edit" | "owner_stop" | "owner_interrupt" | "owner_restart" | "discard" | "accept" | "daily";

export interface Act {
  kind: ActKind;
  project: string;
  subject: string;
  at_ms: number;
}

export interface DayAnswer {
  day: string;
  yes: boolean;
}

export interface Scorecard {
  settings: ScorecardSettings;
  version: number;
  window: Span;
  baseline: Span;
  measured: Span;
  real: number;
  clean: number;
  tasks: ScorecardTask[];
  coordination: Coordination;
  acts: Act[];
  chosen_days: number;
  answered_days: number;
  days: DayAnswer[];
  targets: { tasks: number; clean_of_ten: number; coordination_cut_percent: number; chosen_of_ten: number };
  met: { tasks: boolean; clean: boolean; coordination: boolean; chosen: boolean };
}

export type ScorecardSave = { kind: "saved"; version: number } | { kind: "stale"; current: number };

/** The window switch's choices, the proposed default first. */
export const WINDOWS: { value: WindowChoice; label: string }[] = [
  { value: "rolling", label: "Last 14 days" },
  { value: "from_day", label: "14 days from a start day" },
];

/** The baseline switch's choices, the proposed default first. */
export const BASELINES: { value: ScorecardBaseline; label: string }[] = [
  { value: "first_days", label: "Its first 3 days" },
  { value: "previous_window", label: "The 14 days before" },
];

/** The daily question's choices, the proposed default first. */
export const DAILY_QUESTIONS: DailyQuestion[] = ["off", "on"];

const REFUSALS: Record<string, string> = {
  scorecard_outcome_unknown: "CrazyAgent couldn't confirm what happened. Check the page before you try again.",
  scorecard_reply_bounds: "The engine's scorecard answer was too large to read. Try again.",
  scorecard_request_bounds: "This scorecard request was too large to send. Reload the page and try again.",
  scorecard_request_invalid: "The engine didn't accept this scorecard request. Reload the page and try again.",
  scorecard_settings_invalid: "That start day isn't a calendar day. Pick a day like 2026-11-03.",
  scorecard_unavailable: "The scorecard isn't available in this engine yet.",
};

const FLAWS: Record<Flaw, string> = {
  discarded: "A change set was discarded",
  owner_edited: "You edited it before accepting",
  first_batch_not_passed: "Its first check batch didn't pass",
};

const ACTS: Record<ActKind, string> = {
  owner_message: "Message",
  answer: "Answer",
  owner_edit: "Edit",
  owner_stop: "Stop",
  owner_interrupt: "Interrupt",
  owner_restart: "Restart",
  discard: "Discard",
  accept: "Accept",
  daily: "Daily answer",
};

/** The owner acts the scorecard lists, by the engine's stored name. */
export const ACT_KINDS = Object.keys(ACTS);

/** The refusal codes of the scorecard protocol that have their own sentence. */
export const SCORECARD_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused scorecard request. Input: the refusal code and its engine
 * cause. Output: the sentence, or the plain fallback naming the code. */
export function scorecardRefusal(refusal: { code: string; cause?: string }): string {
  const sentence = Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : knownError(refusal.code);
  return namedFailure(sentence, refusal.code, refusal.cause);
}

/** Reads the go/no-go scorecard in the viewer's local days. Input: the viewer's offset east
 * of UTC in minutes. Output: the scorecard. */
export function loadScorecard(offset: number): Promise<Result<Scorecard>> {
  return call<Scorecard>("desktop_scorecard", { offsetMinutes: offset });
}

/** Saves the scorecard settings, naming the version this page read. Inputs: the settings and
 * the version. Output: saved at a new version, or stale with the current one. */
export function saveScorecard(settings: ScorecardSettings, version: number): Promise<Result<ScorecardSave>> {
  return call<ScorecardSave>("desktop_scorecard_set", { settings, version });
}

/** Counted owner acts per real task of one span. Input: the span's tally. Output: the
 * average, or null when no real task closed in the span. */
export function perTask(tally: Tally): number | null {
  if (tally.tasks === 0) return null;
  return (tally.messages + tally.answers + tally.interventions) / tally.tasks;
}

/** An average per task in words: whole, one decimal, or no tasks yet. Input: the average. */
export function perTaskText(value: number | null): string {
  if (value === null) return "No tasks yet";
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

/** How much less coordination per task the measured span took than the baseline, in whole
 * percent, negative when it took more. Input: both tallies. Output: the percent, or null when
 * either span closed no real task or the baseline counted no act. */
export function coordinationCut(coordination: Coordination): number | null {
  const baseline = perTask(coordination.baseline);
  const measured = perTask(coordination.measured);
  if (baseline === null || measured === null || baseline === 0) return null;
  return Math.round((1 - measured / baseline) * 100);
}

/** A coordination cut in words. Input: the percent. Output: less, more or the same. */
export function cutText(cut: number): string {
  if (cut === 0) return "the same";
  return cut > 0 ? `${cut}% less` : `${-cut}% more`;
}

/** Tells whether text is a calendar day written YYYY-MM-DD, as the engine reads a start day.
 * Input: the text. Output: true for a real day. */
export function calendarDay(text: string): boolean {
  if (!DAY_SHAPE.test(text)) return false;
  const [year, month, day] = text.split("-").map(Number);
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

/** The local day of a time, YYYY-MM-DD, as the engine cuts days. Inputs: the time and the
 * offset east of UTC in minutes. Output: the day. */
export function dayText(ms: number, offset: number): string {
  return new Date(ms + offset * MINUTE).toISOString().slice(0, 10);
}

/** Which window choice the settings hold. Input: the stored window. Output: the choice. */
export function windowChoice(window: ScorecardWindow): WindowChoice {
  return window === "rolling" ? "rolling" : "from_day";
}

/** The settings with one window choice. Inputs: the settings, the choice and the start day a
 * window from a start day takes. Output: the whole settings to save. */
export function withWindow(settings: ScorecardSettings, choice: WindowChoice, startDay: string): ScorecardSettings {
  return { ...settings, window: choice === "rolling" ? "rolling" : { from_day: startDay } };
}

function spanText(span: Span, offset: number): string {
  return `${dayLabel(span.start_ms, offset)} to ${dayLabel(span.end_ms - DAY, offset)}`;
}

function Score({ label, value, target, met }: { label: string; value: string; target: string; met: boolean }) {
  return <div className="usage-tile">
    <span className="muted small">{label}</span>
    <span className="usage-tile-value">{value}</span>
    <span className="muted small">{target}</span>
    <span className={`badge ${met ? "tone-ok" : "tone-warn"}`}>{met ? "Target met" : "Not met yet"}</span>
  </div>;
}

function Scores({ card }: { card: Scorecard }) {
  const { coordination, met, targets } = card;
  const cut = coordinationCut(coordination);
  const asked = card.settings.daily_question === "on" || card.answered_days > 0;
  const baseline = `Baseline ${perTaskText(perTask(coordination.baseline))}${cut === null ? "" : `, ${cutText(cut)}`}`;
  return <div className="usage-tiles">
    <Score label="Real tasks" value={String(card.real)} target={`Target ${targets.tasks} done tickets you accepted`} met={met.tasks} />
    <Score label="Clean" value={`${card.clean} of ${card.real}`} target={`Target ${targets.clean_of_ten} of 10, from ${targets.tasks} tasks`} met={met.clean} />
    <Score label="Coordination per task" value={perTaskText(perTask(coordination.measured))}
      target={`${baseline}. Target ${targets.coordination_cut_percent}% less`} met={met.coordination} />
    <Score label="Chose CrazyAgent" value={`${card.chosen_days} of ${card.answered_days} days`}
      target={asked ? `Target ${targets.chosen_of_ten} of 10 answered days` : "The daily question is off"} met={met.chosen} />
  </div>;
}

function SettingsForm({ settings, today, busy, onSave }: {
  settings: ScorecardSettings;
  today: string;
  busy: boolean;
  onSave: (settings: ScorecardSettings) => void;
}) {
  const choice = windowChoice(settings.window);
  const saved = settings.window === "rolling" ? today : settings.window.from_day;
  const [day, setDay] = useState(saved);
  const [error, setError] = useState("");
  function saveDay() {
    if (!calendarDay(day)) {
      setError("Pick a calendar day.");
      return;
    }
    setError("");
    onSave(withWindow(settings, "from_day", day));
  }
  return <div className="stack">
    <div className="row">
      <Segmented label="Window" value={choice} options={WINDOWS} onValue={(next) => onSave(withWindow(settings, next, saved))} />
      <Segmented label="Baseline" value={settings.baseline} options={BASELINES} onValue={(baseline) => onSave({ ...settings, baseline })} />
    </div>
    {choice === "from_day" && <form className="row" onSubmit={(event) => { event.preventDefault(); saveDay(); }}>
      <TextField id="scorecard-start-day" type="date" label="Start day" value={day} onValue={setDay} error={error} />
      <Button type="submit" pending={busy} pendingLabel="Saving...">Save start day</Button>
    </form>}
    <Switch id="scorecard-daily-question" label="Ask me once a day whether I chose CrazyAgent for my work"
      checked={settings.daily_question === "on"} onChecked={(on) => onSave({ ...settings, daily_question: on ? "on" : "off" })} />
  </div>;
}

function TasksTable({ tasks, real, offset }: { tasks: ScorecardTask[]; real: number; offset: number }) {
  if (tasks.length === 0) {
    return <Panel title="Real tasks behind the numbers">
      <p className="muted">No real tasks in this window yet. A ticket counts once it is done and you accepted its change set.</p>
    </Panel>;
  }
  return <Panel title="Real tasks behind the numbers" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Ticket</th><th scope="col">Title</th><th scope="col">Closed</th><th scope="col">Change sets</th><th scope="col">Clean</th></tr></thead>
      <tbody>{tasks.map((task) => <tr key={task.ticket}>
        <th scope="row">{task.ticket}</th>
        <td>{task.title}</td>
        <td>{dayLabel(task.closed_ms, offset)}</td>
        <td>{task.change_sets.join(", ")}</td>
        <td>{task.clean ? "Yes" : task.flaws.map((flaw) => FLAWS[flaw]).join("; ")}</td>
      </tr>)}</tbody>
    </table></div>
    {real > tasks.length && <div className="panel-body"><p className="muted small">Showing the newest {tasks.length} of {real}.</p></div>}
  </Panel>;
}

function CoordinationTable({ card, offset }: { card: Scorecard; offset: number }) {
  const rows: [string, Span, Tally][] = [["Baseline", card.baseline, card.coordination.baseline], ["Measured", card.measured, card.coordination.measured]];
  return <Panel title="Coordination" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Span</th><th scope="col">Days</th><th scope="col">Messages</th><th scope="col">Answers</th>
        <th scope="col">Interventions</th><th scope="col">Real tasks</th><th scope="col">Per task</th></tr></thead>
      <tbody>{rows.map(([name, span, tally]) => <tr key={name}>
        <th scope="row">{name}</th>
        <td>{spanText(span, offset)}</td>
        <td>{tally.messages}</td>
        <td>{tally.answers}</td>
        <td>{tally.interventions}</td>
        <td>{tally.tasks}</td>
        <td>{perTaskText(perTask(tally))}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

function ActsTable({ acts, projects, nowMs }: { acts: Act[]; projects: Map<string, string>; nowMs: number }) {
  if (acts.length === 0) return null;
  return <Panel title="Your newest acts" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Act</th><th scope="col">Project</th><th scope="col">Subject</th><th scope="col">When</th></tr></thead>
      <tbody>{acts.map((act, index) => <tr key={`${act.at_ms}-${index}`}>
        <th scope="row">{ACTS[act.kind]}</th>
        <td>{projects.get(act.project) ?? act.project}</td>
        <td>{act.subject}</td>
        <td>{agoText(act.at_ms, nowMs)}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

function DaysTable({ days }: { days: DayAnswer[] }) {
  if (days.length === 0) return null;
  return <Panel title="Daily answers" padded={false}>
    <div className="pj-table-wrap usage-table"><table className="pj-table">
      <thead><tr><th scope="col">Day</th><th scope="col">Chose CrazyAgent</th></tr></thead>
      <tbody>{days.map((day) => <tr key={day.day}>
        <th scope="row">{day.day}</th>
        <td>{day.yes ? "Yes" : "No"}</td>
      </tr>)}</tbody>
    </table></div>
  </Panel>;
}

/** The go/no-go scorecard as a pure view: the four numbers with their targets, the window,
 * baseline and daily question settings, and the tasks, acts and daily answers behind them.
 * Inputs: the scorecard read, project names, now, the last failure and the actions. */
export function ScorecardView({ card, projects, nowMs, failure, busy, onSave, onRetry }: {
  card: Loaded<Scorecard>;
  projects: Map<string, string>;
  nowMs: number;
  failure: string;
  busy: boolean;
  onSave: (settings: ScorecardSettings) => void;
  onRetry: () => void;
}) {
  if (card.kind === "not_ready" || (card.kind === "error" && card.code === "scorecard_unavailable")) return <NotReady what="The scorecard" compact />;
  if (card.kind === "loading") return <Panel title="Go/no-go scorecard"><Skeleton shape="card" count={1} /></Panel>;
  if (card.kind === "error") return <Panel title="Go/no-go scorecard"><LoadError message={scorecardRefusal(card)} onRetry={onRetry} /></Panel>;
  const value = card.value;
  const offset = offsetMinutes(new Date(nowMs));
  return <div className="stack">
    <Panel title="Go/no-go scorecard">
      <div className="stack">
        <p className="secondary">Whether CrazyAgent earns its place over {spanText(value.window, offset)}: real tasks, clean ones, how much you coordinated per task and the days you chose it.</p>
        {failure && <Notice tone="danger" alert>{failure}</Notice>}
        <Scores card={value} />
        <SettingsForm key={value.version} settings={value.settings} today={dayText(nowMs, offset)} busy={busy} onSave={onSave} />
      </div>
    </Panel>
    <TasksTable tasks={value.tasks} real={value.real} offset={offset} />
    <CoordinationTable card={value} offset={offset} />
    <ActsTable acts={value.acts} projects={projects} nowMs={nowMs} />
    <DaysTable days={value.days} />
  </div>;
}

/** Settings, Usage, the go/no-go scorecard: reads it in the viewer's local days and saves its
 * settings by version, reloading the saved ones when another window changed them first. */
export function ScorecardPanel() {
  const session = useAppSession();
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [failure, setFailure] = useState("");
  const [busy, setBusy] = useState(false);
  const [card, reload] = useLoad(() => loadScorecard(offsetMinutes(new Date())), []);
  const projects = new Map<string, string>(session.projects.kind === "ok" ? session.projects.value.map((row): [string, string] => [row.id, row.name]) : []);

  async function save(settings: ScorecardSettings) {
    if (busy || card.kind !== "ok") return;
    setFailure("");
    setBusy(true);
    const result = session.noteResult(await saveScorecard(settings, card.value.version));
    setBusy(false);
    if (result.kind === "ok" && result.value.kind === "saved") session.showToast("Scorecard settings saved.");
    else if (result.kind === "ok") setFailure("These settings changed in another window. This page now shows the saved ones; choose again.");
    else setFailure(result.kind === "not_ready" ? NOT_READY : scorecardRefusal(result));
    setNowMs(Date.now());
    reload();
  }

  return <ScorecardView card={card} projects={projects} nowMs={nowMs} failure={failure} busy={busy}
    onSave={(settings) => void save(settings)} onRetry={reload} />;
}
