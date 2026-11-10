import { Component, useEffect, useRef, useState, type ReactNode } from "react";
import { accountSchedule, setAccountSchedule, type AccountPin, type AccountPlan, type AccountSchedule, type AccountStrategy, type PoolSummary, type Result, type ScheduleChange } from "../../api";
import { useAppSession, useHeld, type Hold, type Loaded } from "../../state";
import { Button, LoadError, NotReady, Segmented, TextField } from "../../ui";
import { planFor, planLine, poolStrategy, rankInput, reserveInput, strategyLabel, strategyOptions, STRATEGIES } from "./account-choice";

/** How long the saved line stays, in milliseconds. */
const SAVED_SHOWN_MS = 6000;

/** How many changes may wait their turn. It only stops a key held down on a slow engine. */
const CHANGES_WAITING_MAX = 64;

const TOO_MANY = "Too many changes at once. Wait a moment and try again.";

type PinChoice = "none" | AccountPin;

const PIN_LABEL: Record<PinChoice, string> = { none: "None", use_last: "Use last", never_use: "Never use" };
const PIN_CHOICES = (["none", "use_last", "never_use"] as const).map((value) => ({ value, label: PIN_LABEL[value] }));

/** One change an owner made: how to build it from the schedule as it stands, and the line to
 * say once the engine applied it. */
export interface MadeChange {
  change: (now: AccountSchedule) => ScheduleChange;
  done: string;
}

/** Queues one change. Input: the change. Output: the engine's answer. */
export type ApplyChange = (made: MadeChange) => Promise<Result<void>>;

/** Applies the owner's changes one at a time in the order made. Each reads the schedule first,
 * because a save answers with no version and the engine refuses a change naming an old one. The
 * reads after run once, when no change is left waiting, so a burst never shows a half-done state.
 * Inputs: the re-read of the page and where a refusal is said. Output: the saved line and `apply`. */
export function useScheduleChanges(reread: () => void, refused: (sentence: string | undefined) => void): { saved: string | undefined; apply: ApplyChange } {
  const session = useAppSession();
  const [saved, setSaved] = useState<{ text: string } | null>(null);
  const tail = useRef<Promise<unknown>>(Promise.resolve());
  const waiting = useRef(0);
  useEffect(() => {
    if (saved === null) return undefined;
    const timer = window.setTimeout(() => setSaved(null), SAVED_SHOWN_MS);
    return () => window.clearTimeout(timer);
  }, [saved]);

  const save = async (made: MadeChange): Promise<Result<void>> => {
    const read = session.noteResult(await accountSchedule());
    if (read.kind !== "ok") return read;
    return session.noteResult(await setAccountSchedule(made.change(read.value), read.value.version));
  };

  const job = async (made: MadeChange): Promise<Result<void>> => {
    try {
      const result = await save(made);
      if (result.kind === "ok") setSaved({ text: made.done });
      else refused(result.kind === "not_ready" ? "Account choice isn't ready in this build." : result.message);
      return result;
    } finally {
      waiting.current -= 1;
      if (waiting.current === 0) reread();
    }
  };

  const apply: ApplyChange = (made) => {
    if (waiting.current >= CHANGES_WAITING_MAX) {
      refused(TOO_MANY);
      return Promise.resolve({ kind: "error", code: "too_many_changes", message: TOO_MANY });
    }
    refused(undefined);
    waiting.current += 1;
    const answer = tail.current.then(() => job(made));
    tail.current = answer.catch(() => undefined);
    return answer;
  };

  return { saved: saved?.text, apply };
}

/** Holds a pick on its control and queues its change, letting go of the pick when the engine
 * does not apply it. Inputs: the queue, the control's hold, the pick and the change. */
function applyHeld<T>(apply: ApplyChange, hold: Hold<T>, choice: T, made: MadeChange) {
  const settled = hold(choice);
  void apply(made).then((result) => settled(result.kind === "ok" ? "" : result.kind));
}

/** Builds an account change that keeps the rest of the plan as the engine holds it now. Inputs:
 * the account and the part of its plan that changes. Output: the change's builder. */
function accountChange(id: string, part: Partial<AccountPlan>): MadeChange["change"] {
  return (now) => ({ kind: "account", connection: id, plan: { ...planFor(now, id), ...part } });
}

/** Names the row control holding the focus, null when the focus is outside the rows. */
function focusedControl(): string | null {
  return document.activeElement?.closest<HTMLElement>("[data-control]")?.dataset.control ?? null;
}

/** Puts the focus on a row control by name, on the picked option of a choice group. */
function focusControl(name: string) {
  const found = [...document.querySelectorAll<HTMLElement>("[data-control]")].find((item) => item.dataset.control === name);
  const target = found?.matches("button, input") ? found : found?.querySelector<HTMLElement>('[aria-checked="true"]');
  target?.focus();
}

/** Keeps the keyboard on its control when a read moves the row: moving a keyed row, or taking it
 * into the held list, takes the focus off it. Focus the owner moved elsewhere stays there.
 * Input: the rows. Output: the rows. */
export class KeepRowFocus extends Component<{ children: ReactNode }> {
  getSnapshotBeforeUpdate(): string | null {
    return focusedControl();
  }

  componentDidUpdate(_props: unknown, _state: unknown, name: string | null) {
    if (name !== null && (document.activeElement ?? document.body) === document.body) focusControl(name);
  }

  render() {
    return this.props.children;
  }
}

/** One account's pin. Inputs: the account, its name, its plan and the queue. Output: the choice. */
function PinControl({ id, name, plan, apply }: { id: string; name: string; plan: AccountPlan; apply: ApplyChange }) {
  const [shown, hold] = useHeld<PinChoice>(plan.pin ?? "none");
  return <div data-control={`pin:${id}`}>
    <Segmented label={`Pin for ${name}`} value={shown} options={PIN_CHOICES} onValue={(choice) => {
      if (choice === shown) return;
      applyHeld(apply, hold, choice, { change: accountChange(id, { pin: choice === "none" ? null : choice }), done: `Pin for ${name}: ${PIN_LABEL[choice]}.` });
    }} />
  </div>;
}

/** One number of a plan, saved on Enter or on leaving the field; a change that fails its rule
 * shows it and sends nothing. Inputs: the field's id, label and length, the saved value, its
 * parser, the queue and the change for a value. Output: the field. */
function PlanField<T extends number | null>({ id, label, length, saved, parse, apply, made }: {
  id: string;
  label: string;
  length: number;
  saved: T;
  parse: (text: string) => { kind: "ok"; value: T } | { kind: "error"; message: string };
  apply: ApplyChange;
  made: (value: T) => MadeChange;
}) {
  const [shown, hold] = useHeld<T>(saved);
  const words = shown === null ? "" : String(shown);
  const [text, setText] = useState(words);
  const [error, setError] = useState<string>();
  useEffect(() => {
    setText(words);
    setError(undefined);
  }, [words]);
  const commit = () => {
    const parsed = parse(text);
    if (parsed.kind === "error") return setError(parsed.message);
    setError(undefined);
    if (parsed.value === shown) return setText(words);
    applyHeld(apply, hold, parsed.value, made(parsed.value));
  };
  return <TextField id={id} label={label} data-control={id} value={text} onValue={setText} maxLength={length} inputMode="numeric" error={error} onBlur={commit}
    onKeyDown={(event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      commit();
    }} />;
}

/** One account's rank and reserve, and what its plan comes to. Inputs: the account, its name, its
 * plan and the queue. Output: the panel. */
export function SchedulePanel({ id, name, plan, apply }: { id: string; name: string; plan: AccountPlan; apply: ApplyChange }) {
  return <div className="pool-sched">
    <PlanField id={`pool-rank-${id}`} label="Rank" length={4} saved={plan.rank} parse={rankInput} apply={apply}
      made={(rank) => ({ change: accountChange(id, { rank }), done: `Rank for ${name} saved.` })} />
    <PlanField id={`pool-reserve-${id}`} label="Reserve %" length={3} saved={plan.reserve_percent} parse={reserveInput} apply={apply}
      made={(reserve_percent) => ({ change: accountChange(id, { reserve_percent }), done: `Reserve for ${name} saved.` })} />
    <div className="pool-sched-text"><b>{planLine(plan)}</b>Applies in every pool this account is in.</div>
  </div>;
}

/** The controls on one row: its pin and the button of its Schedule panel. Inputs: the account, its
 * name, its plan, whether its panel is open, the toggle and the queue. Output: the controls. */
export function RowControls({ id, name, plan, open, onToggle, apply }: {
  id: string;
  name: string;
  plan: AccountPlan;
  open: boolean;
  onToggle: () => void;
  apply: ApplyChange;
}) {
  return <div className="pool-ctl">
    <PinControl id={id} name={name} plan={plan} apply={apply} />
    <Button size="small" aria-expanded={open} aria-label={`Schedule for ${name}`} data-control={`schedule:${id}`} onClick={onToggle}>Schedule</Button>
  </div>;
}


/** A pool's own account order. Inputs: the pool, the schedule and the queue. Output: the choice. */
export function PoolOrderPicker({ pool, schedule, apply }: { pool: PoolSummary; schedule: AccountSchedule; apply: ApplyChange }) {
  const [shown, hold] = useHeld<string>(poolStrategy(schedule, pool.id) ?? "");
  const options = strategyOptions(strategyLabel(schedule.strategy));
  return <Segmented label="Account order" value={shown} options={options} onValue={(value) => {
    if (value === shown) return;
    const strategy = value === "" ? null : value as AccountStrategy;
    const label = options.find((item) => item.value === value)?.label ?? value;
    applyHeld(apply, hold, value, { change: () => ({ kind: "pool_strategy", pool: pool.id, strategy }), done: `Account order: ${label}.` });
  }} />;
}

/** The order every pool follows by default. Inputs: the schedule read, the queue and the retry
 * action. Output: the card, or the state of the read. */
export function AppOrder({ schedule, apply, onRetry }: { schedule: Loaded<AccountSchedule>; apply: ApplyChange; onRetry: () => void }) {
  if (schedule.kind === "loading") return null;
  if (schedule.kind === "not_ready") return <NotReady what="Account choice" compact />;
  if (schedule.kind === "error") return <LoadError message="Couldn't load account choice." onRetry={onRetry} />;
  return <AppOrderChoice schedule={schedule.value} apply={apply} />;
}

/** The choice of the order every pool follows, with what the pick does. */
function AppOrderChoice({ schedule, apply }: { schedule: AccountSchedule; apply: ApplyChange }) {
  const [shown, hold] = useHeld<AccountStrategy>(schedule.strategy);
  const choice = STRATEGIES.find((item) => item.value === shown);
  return <section className="pool-app-order" aria-label="How every pool picks an account">
    <h3>How every pool picks an account</h3>
    <Segmented label="How every pool picks an account" value={shown} options={STRATEGIES.map((item) => ({ value: item.value, label: item.label }))} onValue={(value) => {
      if (value === shown) return;
      applyHeld(apply, hold, value, { change: () => ({ kind: "app_strategy", strategy: value }), done: `Every pool now defaults to ${strategyLabel(value)}.` });
    }} />
    <span className="pool-app-hint">{choice?.hint}</span>
  </section>;
}
