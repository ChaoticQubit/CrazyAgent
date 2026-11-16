import { useState, type FormEvent } from "react";
import { MINUTES_MAX, MINUTES_MIN, sendKeepAwake, type KeepAwakeAction, type KeepAwakeMode, type KeepAwakeView, type LidAction } from "../../api-keep-awake";
import type { Loaded } from "../../state";
import { Button, LoadError, NotReady, Skeleton, formatRoute } from "../../ui";
import { APPROVAL_LINE, canEnd, canExtend, canStart, customMinutes, endsByItself, lidClosedLine, lidOffers, lidReason, panelLine, type LidPlace } from "./model";
import "../notifications/notifications.css";
import "./keep-awake.css";

/** The modes in the order the panel's switch lists them, with the panel's short words. */
export const PANEL_MODES: readonly { mode: KeepAwakeMode; label: string }[] = [
  { mode: "auto", label: "Auto" }, { mode: "manual", label: "Manual" }, { mode: "off", label: "Off" },
];

const HOUR_MINUTES = 60;
const SETTINGS_HREF = formatRoute({ name: "settings", tab: "keep-awake" });

/** The custom timer's minutes field with its Start, and the bounds once something typed is
 * not a whole number from 1 to 1440. Inputs: the typed text, a lock flag and the handlers.
 * Output: the form. */
function CustomTimer({ typed, busy, onType, onStart }: { typed: string; busy: boolean; onType: (typed: string) => void; onStart: (minutes: number) => void }) {
  const minutes = customMinutes(typed);
  const wrong = minutes === null && typed.trim() !== "";
  function submit(event: FormEvent) {
    event.preventDefault();
    if (minutes !== null && !busy) onStart(minutes);
  }
  return <form className="keep-awake-custom" noValidate onSubmit={submit}>
    <label htmlFor="keep-awake-minutes">Minutes</label>
    <input className="pj-input num" id="keep-awake-minutes" type="number" min={MINUTES_MIN} max={MINUTES_MAX} value={typed} disabled={busy} autoFocus
      onChange={(event) => onType(event.target.value)} aria-invalid={wrong || undefined} aria-describedby={wrong ? "keep-awake-minutes-error" : undefined} />
    <Button type="submit" disabled={minutes === null || busy}>Start</Button>
    {wrong && <p className="pj-error" id="keep-awake-minutes-error" role="alert">{`Enter whole minutes from ${MINUTES_MIN} to ${MINUTES_MAX}.`}</p>}
  </form>;
}

/** The lid helper buttons one place offers (QUESTION 35i, 35j): Turn on lid-closed hold or
 * Open Login Items, and Remove lid helper, or nothing. Inputs: the view, the place, a lock
 * flag and the command handler. Output: the buttons, or null. */
export function LidHelperButtons({ view, place, busy, onLid }: { view: KeepAwakeView; place: LidPlace; busy: boolean; onLid: (action: LidAction) => void }) {
  const offers = lidOffers(view, place);
  if (offers.turnOn === null && !offers.remove) return null;
  return <div className="keep-awake-row">
    {offers.turnOn !== null && <Button disabled={busy} onClick={() => onLid("turn_on_lid_hold")}>{offers.turnOn}</Button>}
    {offers.remove && <Button disabled={busy} onClick={() => onLid("remove_lid_helper")}>Remove lid helper</Button>}
  </div>;
}

/** The mode switch and the two lines under the actions: what a closed lid does, with the
 * reason this computer cannot hold it, the approval line while macOS waits and nothing is
 * held, the lid helper buttons, and what ends a hold by itself. Inputs: the view, a lock
 * flag and the mode and lid command handlers. Output: the rows. */
function PanelFacts({ view, busy, onMode, onLid }: { view: KeepAwakeView; busy: boolean; onMode: (mode: KeepAwakeMode) => void; onLid: (action: LidAction) => void }) {
  const reason = lidReason(view);
  const approval = view.lid_platform.kind === "needs_approval" && view.settings.lid !== "off" && view.state.want.kind === "nothing";
  return <>
    <div className="keep-awake-kv">
      <span id="keep-awake-panel-mode">Mode</span>
      <fieldset className="notice-choices" data-setting="mode" disabled={busy} aria-labelledby="keep-awake-panel-mode">
        {PANEL_MODES.map((option) => <label key={option.mode} className={option.mode === view.settings.mode ? "on" : undefined}>
          <input type="radio" name="keep-awake-panel-mode" value={option.mode} checked={option.mode === view.settings.mode} onChange={() => onMode(option.mode)} />
          {option.label}
        </label>)}
      </fieldset>
    </div>
    <div className="keep-awake-kv"><span>Lid closed</span><span>{lidClosedLine(view)}</span></div>
    {reason !== null && view.settings.lid !== "off" && <p className="keep-awake-hint">{reason}</p>}
    {approval && <p className="keep-awake-hint">{APPROVAL_LINE}</p>}
    <LidHelperButtons view={view} place="panel" busy={busy} onLid={onLid} />
    <div className="keep-awake-kv"><span>Ends by itself</span><span>{endsByItself(view.settings)}</span></div>
  </>;
}

/** The panel the Keep awake control opens, as a view of its state: what holds, Start 1 h,
 * Start 4 h, Custom, Extend 1 h and End now, the mode, the lid and end lines with the lid
 * helper buttons, and the link to the settings (DEC-167 addendum 5). In mode Off the Starts
 * are off and one line says why. Inputs: the view read, now in milliseconds, whether the
 * custom field is open, its text, a lock flag, the last problem and the handlers. Output:
 * the dialog. */
export function KeepAwakePanelView({ view, now, custom, typed, busy, problem, onStart, onCustom, onType, onExtend, onEnd, onMode, onLid, onRetry, onClose }: {
  view: Loaded<KeepAwakeView>;
  now: number;
  custom: boolean;
  typed: string;
  busy: boolean;
  problem: string;
  onStart: (minutes: number) => void;
  onCustom: () => void;
  onType: (typed: string) => void;
  onExtend: () => void;
  onEnd: () => void;
  onMode: (mode: KeepAwakeMode) => void;
  onLid: (action: LidAction) => void;
  onRetry: () => void;
  onClose?: () => void;
}) {
  const loaded = view.kind === "ok" ? view.value : null;
  const startable = loaded !== null && canStart(loaded);
  return <section className="keep-awake-panel" role="dialog" aria-label="Keep awake">
    <div>
      <h2>Keep awake</h2>
      {loaded && <p className="keep-awake-sub">{panelLine(loaded, now)}</p>}
    </div>
    {view.kind === "loading" && <Skeleton shape="line" count={4} />}
    {view.kind === "not_ready" && <NotReady what="Keep awake" compact />}
    {view.kind === "error" && <LoadError message={`Couldn't load Keep awake. ${view.message}`} onRetry={onRetry} />}
    {loaded && <>
      <div className="keep-awake-row">
        <Button disabled={busy || !startable} onClick={() => onStart(HOUR_MINUTES)}>Start 1 h</Button>
        <Button disabled={busy || !startable} onClick={() => onStart(4 * HOUR_MINUTES)}>Start 4 h</Button>
        <Button disabled={busy || !startable} aria-expanded={custom} onClick={onCustom}>Custom...</Button>
      </div>
      {custom && <CustomTimer typed={typed} busy={busy || !startable} onType={onType} onStart={onStart} />}
      <div className="keep-awake-row">
        <Button disabled={busy || !canExtend(loaded)} onClick={onExtend}>Extend 1 h</Button>
        <Button className="keep-awake-end" disabled={busy || !canEnd(loaded)} onClick={onEnd}>End now</Button>
      </div>
      {!startable && <p className="keep-awake-hint">Keep awake is Off. Pick Auto or Manual to start a timer.</p>}
      {startable && !canExtend(loaded) && <p className="keep-awake-hint">Extend works while a timer runs. Start one first.</p>}
      {problem && <p className="notice-problem" role="alert">{problem}</p>}
      <hr className="keep-awake-rule" />
      <PanelFacts view={loaded} busy={busy} onMode={onMode} onLid={onLid} />
      <a href={SETTINGS_HREF} onClick={onClose}>All Keep awake settings</a>
    </>}
  </section>;
}

/** The Keep awake panel: sends each action and the mode to the engine, one at a time, and
 * says what the engine refused. Inputs: the view the control read, now in milliseconds, the
 * retry of that read and the close action. Output: the panel. */
export function KeepAwakePanel({ view, now, onRetry, onClose }: { view: Loaded<KeepAwakeView>; now: number; onRetry: () => void; onClose: () => void }) {
  const [custom, setCustom] = useState(false);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  async function send(action: KeepAwakeAction): Promise<boolean> {
    if (busy) return false;
    setBusy(true);
    setProblem("");
    const result = await sendKeepAwake(action);
    setBusy(false);
    if (result.kind === "error") setProblem(result.message);
    if (result.kind === "not_ready") setProblem("The engine is not ready yet. Try again.");
    return result.kind === "ok";
  }
  async function start(minutes: number) {
    if (await send({ start: { minutes } })) { setCustom(false); setTyped(""); }
  }
  function mode(picked: KeepAwakeMode) {
    if (view.kind === "ok" && picked !== view.value.settings.mode) void send({ set: { settings: { ...view.value.settings, mode: picked } } });
  }
  return <KeepAwakePanelView view={view} now={now} custom={custom} typed={typed} busy={busy} problem={problem}
    onStart={(minutes) => void start(minutes)} onCustom={() => setCustom(!custom)} onType={(text) => { setTyped(text); setProblem(""); }}
    onExtend={() => void send({ extend: { minutes: HOUR_MINUTES } })} onEnd={() => void send("end")} onMode={mode}
    onLid={(action) => void send(action)} onRetry={onRetry} onClose={onClose} />;
}
