import { useState, type FormEvent } from "react";
import { MINUTES_MAX, MINUTES_MIN, sendKeepAwake, type KeepAwakeAction, type KeepAwakeMode, type KeepAwakeView } from "../../api-keep-awake";
import type { Loaded } from "../../state";
import { Button, LoadError, NotReady, Skeleton, formatRoute } from "../../ui";
import { canEnd, canExtend, customMinutes, endsByItself, lidClosedLine, lidReason, panelLine } from "./model";
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

/** The mode switch and the two lines under the actions: what a closed lid does, with the
 * reason this computer cannot hold it, and what ends a hold by itself. Inputs: the view, a
 * lock flag and the mode handler. Output: the rows. */
function PanelFacts({ view, busy, onMode }: { view: KeepAwakeView; busy: boolean; onMode: (mode: KeepAwakeMode) => void }) {
  const reason = lidReason(view);
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
    <div className="keep-awake-kv"><span>Ends by itself</span><span>{endsByItself(view.settings)}</span></div>
  </>;
}

/** The panel the Keep awake control opens, as a view of its state: what holds, Start 1 h,
 * Start 4 h, Custom, Extend 1 h and End now, the mode, the lid and end lines, and the link to
 * the settings (DEC-167 addendum 5). Inputs: the view read, now in milliseconds, whether the
 * custom field is open, its text, a lock flag, the last problem and the handlers. Output:
 * the dialog. */
export function KeepAwakePanelView({ view, now, custom, typed, busy, problem, onStart, onCustom, onType, onExtend, onEnd, onMode, onRetry, onClose }: {
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
  onRetry: () => void;
  onClose?: () => void;
}) {
  const loaded = view.kind === "ok" ? view.value : null;
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
        <Button disabled={busy} onClick={() => onStart(HOUR_MINUTES)}>Start 1 h</Button>
        <Button disabled={busy} onClick={() => onStart(4 * HOUR_MINUTES)}>Start 4 h</Button>
        <Button disabled={busy} aria-expanded={custom} onClick={onCustom}>Custom...</Button>
      </div>
      {custom && <CustomTimer typed={typed} busy={busy} onType={onType} onStart={onStart} />}
      <div className="keep-awake-row">
        <Button disabled={busy || !canExtend(loaded)} onClick={onExtend}>Extend 1 h</Button>
        <Button className="keep-awake-end" disabled={busy || !canEnd(loaded)} onClick={onEnd}>End now</Button>
      </div>
      {!canExtend(loaded) && <p className="keep-awake-hint">Extend works while a timer runs. Start one first.</p>}
      {problem && <p className="notice-problem" role="alert">{problem}</p>}
      <hr className="keep-awake-rule" />
      <PanelFacts view={loaded} busy={busy} onMode={onMode} />
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
    onExtend={() => void send({ extend: { minutes: HOUR_MINUTES } })} onEnd={() => void send("end")} onMode={mode} onRetry={onRetry} onClose={onClose} />;
}
