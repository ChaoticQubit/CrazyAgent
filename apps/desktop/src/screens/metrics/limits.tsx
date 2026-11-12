import { useState, type ReactNode } from "react";
import type { AccountView } from "../../api";
import { pollFailure, type AccountLimits, type WindowKind } from "../../api-usage";
import { Button, Notice, Panel, TextField } from "../../ui";
import { atText } from "../settings/account-choice";
import { API_KEY_ACCOUNT, manualReset, missingWindow, readLine, untilText, windowNow } from "./usage-model";
import "../project/projects.css";
import "./metrics.css";

const NEAR_LIMIT_PERCENT = 80;

/** What the app page's limits need from the page around them: pool names, the last failed
 * action, the action in flight, the next accounts panel, a number that moves after each
 * action so the page reads again, and the two actions. */
export interface LimitsControls {
  pools: Map<string, string>;
  failure: string;
  busy: string | null;
  next: ReactNode;
  revision: number;
  onRefresh: () => void;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
}

/** Whether a window is close to its limit. Input: the percent used. Output: true from 80. */
export function nearLimit(usedPercent: number): boolean {
  return usedPercent >= NEAR_LIMIT_PERCENT;
}

/** When a window resets, on the clock and as a countdown. Inputs: the reset time, null when
 * unknown, and now. Output: "Resets at 14:30, in 2 h 14 min" or "Next reset not known yet". */
export function resetAtText(atMs: number | null, nowMs: number): string {
  if (atMs === null) return "Next reset not known yet";
  return `Resets at ${atText(atMs, nowMs)}, ${untilText(atMs, nowMs)}`;
}

function ResetEditor({ id, override, nowMs, pending, onSave }: {
  id: string;
  override: number | null;
  nowMs: number;
  pending: boolean;
  onSave: (at: number | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string>();
  if (!open) return <span className="row">
    <Button size="small" icon="clock" disabled={pending} onClick={() => setOpen(true)}>{override === null ? "Set reset time" : "Change reset time"}</Button>
    {override !== null && <Button size="small" pending={pending} pendingLabel="Clearing..." onClick={() => onSave(null)}>Use provider's time</Button>}
  </span>;
  const save = () => {
    const reset = manualReset(value, nowMs);
    setError(reset.kind === "invalid" ? reset.reason : undefined);
    if (reset.kind === "ok") { onSave(reset.at); setOpen(false); }
  };
  return <form className="row" onSubmit={(event) => { event.preventDefault(); save(); }}>
    <TextField id={id} type="datetime-local" label="Resets at" value={value} onValue={setValue} error={error} />
    <Button type="submit" size="small" variant="primary" pending={pending} pendingLabel="Saving...">Save</Button>
    <Button size="small" onClick={() => setOpen(false)}>Cancel</Button>
  </form>;
}

function WindowLine({ limits, kind, nowMs, pending, onSetReset }: {
  limits: AccountLimits;
  kind: WindowKind;
  nowMs: number;
  pending: boolean;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
}) {
  const override = kind === "five_hour" ? limits.five_hour_reset_override_ms : limits.weekly_reset_override_ms;
  const current = windowNow(kind === "five_hour" ? limits.five_hour : limits.weekly, override, nowMs);
  const label = kind === "five_hour" ? "5-hour" : "Weekly";
  const [missing, why] = missingWindow(limits.failure);
  const leftOut = kind === "five_hour" ? limits.five_hour_left_out_label : limits.weekly_left_out_label;
  const failed = limits.failure !== null && limits.failure !== API_KEY_ACCOUNT;
  if (!current && leftOut !== null && !failed) return <div className="usage-window">
    <div className="row usage-window-head"><strong>{label}</strong></div>
    <span className="muted small">{leftOut}</span>
  </div>;
  return <div className="usage-window">
    <div className="row usage-window-head">
      <strong>{label}</strong>
      <span>{current ? `${Math.round(current.used_percent)}% used` : missing}{current && nearLimit(current.used_percent) && <> <span className="badge">Near limit</span></>}</span>
    </div>
    {current && <meter className="usage-meter" min={0} max={100} low={70} high={90} optimum={0} value={current.used_percent} aria-label={`${label} limit used`} />}
    <span className="muted small">{current ? resetAtText(current.resets_at_ms, nowMs) : why}{current !== null && override !== null && current.resets_at_ms === override ? " (your time)" : ""}</span>
    <ResetEditor id={`usage-reset-${limits.account}-${kind}`} override={override} nowMs={nowMs} pending={pending}
      onSave={(at) => onSetReset(limits.account, kind, at)} />
  </div>;
}

export function LimitsCard({ limits, name, nowMs, busy, onSetReset }: {
  limits: AccountLimits;
  name: string;
  nowMs: number;
  busy: string | null;
  onSetReset: (account: string, kind: WindowKind, at: number | null) => void;
}) {
  return <section className="usage-limits" aria-label={`${name} limits`}>
    <div className="row usage-window-head">
      <h3>{name}</h3>
      {limits.plan && <span className="badge">{limits.plan}</span>}
    </div>
    <span className="muted small">{readLine(limits, nowMs)}</span>
    {limits.failure && <Notice tone={limits.failure === API_KEY_ACCOUNT ? "info" : "warn"}>{pollFailure(limits.failure)}</Notice>}
    <WindowLine limits={limits} kind="five_hour" nowMs={nowMs} pending={busy === `${limits.account}/five_hour`} onSetReset={onSetReset} />
    <WindowLine limits={limits} kind="weekly" nowMs={nowMs} pending={busy === `${limits.account}/weekly`} onSetReset={onSetReset} />
  </section>;
}

/** Asks the engine to check every account's limits now. Inputs: the action in flight and the
 * action. Output: the button. */
export function CheckLimits({ busy, onRefresh }: Pick<LimitsControls, "busy" | "onRefresh">) {
  return <Button icon="refresh-cw" pending={busy === "refresh"} pendingLabel="Asking..." onClick={onRefresh}>Check limits now</Button>;
}

const NO_WINDOW_KEY = "API key: no 5-hour or weekly window. The provider limits a key by rate and spend, set at the provider.";
const NO_WINDOW_URL = "Endpoint: no 5-hour or weekly window. The endpoint sets its own limits.";
const NO_WINDOW_LOCAL = "Local model: no plan or rate limit.";

/** One account with no 5-hour or weekly window and the sentence it shows in their place. */
export interface NoWindow {
  account: string;
  note: string;
}

/** Tells what an account with no 5-hour or weekly window has instead, the engine's
 * usage::no_window in the same words, so every account shows in Account limits (DEC-141
 * PA-16). Input: the account's sign-in method and provider. Output: the sentence, or null for
 * a subscription, whose windows the engine reads. */
export function noWindow(account: Pick<AccountView, "method" | "provider">): string | null {
  if (account.method === "subscription") return null;
  if (account.provider === "local") return NO_WINDOW_LOCAL;
  return account.method === "api_key" ? NO_WINDOW_KEY : NO_WINDOW_URL;
}

function NoWindowCard({ name, note }: { name: string; note: string }) {
  return <section className="usage-limits" aria-label={`${name} limits`}>
    <div className="row usage-window-head"><h3>{name}</h3></div>
    <span className="muted small">{note}</span>
  </section>;
}

/** Account limits: each account's windows and resets under the Check limits now button, then
 * each key, URL or local account without a read of its own, saying what it has instead.
 * Inputs: the limits read, the account namer, now, the page's controls and those accounts. */
export function AccountLimitsPanel({ limits, name, nowMs, controls, noWindow: plain = [] }: {
  limits: AccountLimits[];
  name: (key: string) => string;
  nowMs: number;
  controls: LimitsControls;
  noWindow?: readonly NoWindow[];
}) {
  const read = new Set(limits.map((row) => row.account));
  const others = plain.filter((row) => !read.has(row.account));
  return <Panel title="Account limits" actions={<CheckLimits busy={controls.busy} onRefresh={controls.onRefresh} />}>
    {limits.length === 0 && others.length === 0
      ? <p className="muted">No limits read yet. Signed-in Claude and ChatGPT accounts show here after their first check.</p>
      : <div className="usage-limit-grid">{limits.map((row) => <LimitsCard key={row.account} limits={row} name={name(row.account)}
        nowMs={nowMs} busy={controls.busy} onSetReset={controls.onSetReset} />)}
        {others.map((row) => <NoWindowCard key={row.account} name={name(row.account)} note={row.note} />)}</div>}
  </Panel>;
}
