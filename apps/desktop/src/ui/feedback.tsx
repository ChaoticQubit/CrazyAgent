import type { ReactNode } from "react";
import { Icon, type IconName } from "./icon";

export type Tone = "ok" | "warn" | "danger" | "info" | "neutral";

const STATUSES: [RegExp, IconName, Tone][] = [
  [/^(Signed in|Connected|Ready|Done|Approved|Saved|Research done|Replied|Installed|On)$/, "circle-check", "ok"],
  [/^Key saved$/, "key-round", "ok"],
  [/^(Not signed in|Needs a pool|Needs approval|Needs you|Rate limited until .+)$/, "circle-alert", "warn"],
  [/^Limited control/, "triangle-alert", "warn"],
  [/^(Can't reach this URL|Failed|Can't start.*|Folder missing)$/, "circle-x", "danger"],
  [/^(Running|Signing in\.\.\.|Starting|Thinking\.\.\.|Writing|Working|Installing)$/, "loader-circle", "info"],
  [/^Found$/, "search", "info"],
  [/^Setting up$/, "list-checks", "warn"],
  [/^(Queued|Starts after .+)$/, "clock", "neutral"],
  [/^Waiting for a free account$/, "hourglass", "neutral"],
  [/^Waiting$/, "circle-dashed", "neutral"],
  [/^Idle$/, "circle-pause", "neutral"],
  [/^Sent$/, "send", "neutral"],
  [/^Stopped$/, "circle-stop", "neutral"],
  [/^(Turned off|Not installed|Off)$/, "power-off", "neutral"],
  [/^Suggested$/, "lightbulb", "neutral"],
];

/** Maps one fixed status word from the design vocabulary to its tone; unknown words are
 * neutral. Input: the status word. Output: ok, warn, danger, info or neutral. */
export function statusTone(word: string): Tone {
  return STATUSES.find(([pattern]) => pattern.test(word))?.[2] ?? "neutral";
}

/** Maps one fixed status word to its icon from the design icon table. Input: the status
 * word. Output: the icon name, or undefined for a word the table does not list. */
export function statusIcon(word: string): IconName | undefined {
  return STATUSES.find(([pattern]) => pattern.test(word))?.[1];
}

/** A status badge: always the word, with the design icon and tone color when the word is in
 * the table, text only otherwise. Inputs: the status word and an optional tone override. */
export function StatusBadge({ word, tone = statusTone(word) }: { word: string; tone?: Tone }) {
  const icon = statusIcon(word);
  return <span className={`badge${tone === "neutral" ? "" : ` tone-${tone}`}`}>
    {icon && <Icon name={icon} size={12} spin={icon === "loader-circle"} />}{word}
  </span>;
}

/** An inline notice with an icon. Inputs: tone, optional icon, content, alert role. */
export function Notice({ tone = "warn", icon, children, alert = false, id }: {
  tone?: "warn" | "info" | "ok" | "danger";
  icon?: IconName;
  children: ReactNode;
  alert?: boolean;
  id?: string;
}) {
  const fallback: Record<string, IconName> = { warn: "triangle-alert", info: "info", ok: "circle-check", danger: "circle-alert" };
  return <div id={id} className={`notice${tone === "warn" ? "" : ` ${tone}`}`} role={alert ? "alert" : undefined} tabIndex={id ? -1 : undefined}>
    <Icon name={icon ?? fallback[tone]} size={14} /><div>{children}</div>
  </div>;
}

/** A full-width warning banner under the page header with one action.
 * Inputs: message and an optional action. Output: a banner with role "status". */
export function Banner({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return <div className="banner" role="status"><span className="row"><Icon name="triangle-alert" size={16} />{children}</span>{action}</div>;
}

/** The single toast, bottom right, announced politely. Input: the message or nothing. */
export function Toast({ message }: { message: string }) {
  return message ? <div className="toast" role="status"><Icon name="circle-check" size={16} />{message}</div> : null;
}

/** An empty state: icon, title, body and one action. Inputs: icon, title, body, action. */
export function EmptyState({ icon = "inbox", title, children, action, compact = false }: {
  icon?: IconName;
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  compact?: boolean;
}) {
  return <section className={`empty-state${compact ? " compact" : ""}`} aria-label={title}>
    <Icon name={icon} size={28} />
    <h2>{title}</h2>
    {children && <p>{children}</p>}
    {action}
  </section>;
}

/** The plain "Not ready yet" state for a screen whose engine part is not in this build.
 * It never shows sample data. Inputs: what is not ready, in plain words. */
export function NotReady({ what, compact = false }: { what: string; compact?: boolean }) {
  return <section className={`empty-state not-ready${compact ? " compact" : ""}`} aria-label="Not ready yet">
    <span className="badge"><Icon name="clock" size={12} />Not ready yet</span>
    <h2>{what} isn't ready yet</h2>
    <p>This part of CrazyAgent's engine isn't in this build yet. Nothing here is saved or shown until it is.</p>
  </section>;
}

/** A plain load error with Try again. Inputs: the message and the retry action. */
export function LoadError({ message, onRetry }: { message: string; onRetry: () => void }) {
  return <section className="empty-state compact" role="alert">
    <Icon name="circle-alert" size={24} />
    <p>{message}</p>
    <button type="button" className="btn" onClick={onRetry}><Icon name="refresh-cw" size={15} />Try again</button>
  </section>;
}

/** Loading placeholders shaped like the final content. Inputs: shape and count. */
export function Skeleton({ shape, count = 1 }: { shape: "line" | "row" | "card"; count?: number }) {
  return <div className={shape === "card" ? "cards" : "stack-small"} aria-hidden="true">
    {Array.from({ length: count }, (_, index) => <div key={index} className={`skeleton ${shape === "line" ? "line" : `${shape}-shape`}`} />)}
  </div>;
}

/** A pool's "7 of 15" counter with a 15-cell meter, announced politely as it changes.
 * Inputs: member count and the maximum. Output: the counter and meter. */
export function CapacityMeter({ count, max = 15 }: { count: number; max?: number }) {
  return <div className="capacity">
    <span className="capacity-count" aria-live="polite">{count} of {max}</span>
    <div className="meter" aria-hidden="true">{Array.from({ length: max }, (_, index) => <span key={index} className={index < count ? "on" : undefined} />)}</div>
  </div>;
}
