import type { AppTeamEvents, TeamEvent, TeamEventKind, TeamNotification, WakeRule, WakeRuleLevel } from "../../engine-client";

export const HOURLY_CAP_MAX = 500;

export const WAKE_KINDS: readonly { kind: TeamEventKind; label: string }[] = [
  { kind: "question", label: "A sub-agent asks a question" },
  { kind: "finished", label: "A sub-agent finishes its task" },
  { kind: "failed", label: "A sub-agent fails" },
  { kind: "change_set_ready", label: "A change set is ready" },
  { kind: "progress", label: "A sub-agent reports progress" },
];

const VERBS: Record<TeamEventKind, string> = {
  question: "asks",
  finished: "finished",
  failed: "failed",
  change_set_ready: "has changes ready",
  progress: "reports progress",
};

const STATES: Record<TeamEvent["state"], string> = {
  queued: "Waiting for the master",
  delivered: "Told the master",
  muted: "Not sent",
  carried: "In the task result",
  dropped: "Replaced by newer events",
};

const LEVELS: Record<WakeRuleLevel, string> = {
  shipped: "the default rule",
  app: "the app rule",
  project: "the project rule",
  agent: "this agent's rule",
};

/** Checks the hourly wake limit typed by the owner with the engine's bounds: a whole number
 * from 0 (never wake) to 500. Input: the field text. Output: the cap, or the plain reason. */
export function capInput(text: string): { kind: "ok"; cap: number } | { kind: "invalid"; reason: string } {
  const trimmed = text.trim();
  if (!/^\d{1,3}$/.test(trimmed)) return { kind: "invalid", reason: `Write a whole number from 0 to ${HOURLY_CAP_MAX}.` };
  const cap = Number(trimmed);
  if (cap > HOURLY_CAP_MAX) return { kind: "invalid", reason: `Write a whole number from 0 to ${HOURLY_CAP_MAX}.` };
  return { kind: "ok", cap };
}

/** The same rule with one kind of event turned on or off. Inputs: the rule, the kind and the
 * switch. Output: a new rule; the input is not changed. */
export function withKind(rule: WakeRule, kind: TeamEventKind, on: boolean): WakeRule {
  return { ...rule, [kind]: on };
}

/** Names what one wake rule editor shows: its target and the rule read for it, by value, so two
 * reads of the same rule give the same view. Inputs: the editor's target key, its own rule or
 * null and the rule in effect. Output: the view. */
export function wakeRuleView(key: string, own: WakeRule | null, effective: WakeRule): string {
  return JSON.stringify([key, own, effective]);
}

/** Whether a wake rule editor must not start another save: while one is in flight, and after one
 * the engine applied until the rule read back is no longer the one it was made from, so a quick
 * second switch cannot rebuild the rule from the stale copy and revert the first. Inputs: the view
 * the last applied save was made from or null, the view now and whether a save is in flight.
 * Output: true while edits wait. */
export function wakeSaveBlocked(savedFrom: string | null, view: string, saving: boolean): boolean {
  return saving || (savedFrom !== null && savedFrom === view);
}

/** Where the rule shown at one level comes from, for the line under its title. Inputs: whether
 * this level has its own rule and the level it falls back to. Output: one sentence. */
export function ruleSource(own: boolean, fallback: WakeRuleLevel): string {
  return own ? "Set here. Reset to use the rule above it." : `Uses ${LEVELS[fallback]}.`;
}

/** One event as the master reads it, without the team prefix. Input: the event. Output:
 * "claude-3 asks: Proceed? | 1. Yes | 2. No". */
export function eventLine(event: TeamEvent): string {
  return `${event.label} ${VERBS[event.kind]}: ${event.text}`;
}

/** The plain word for where an event went. Input: the event state. Output: the word. */
export function eventState(state: TeamEvent["state"]): string {
  return STATES[state];
}

/** The unseen notification count of each project for the sidebar, as the engine counts it,
 * so a busy project's newer notifications never hide another's. Input: the app events.
 * Output: a map from project id to its unseen count, without projects at zero. */
export function unseenByProject(events: Pick<AppTeamEvents, "projects">): Map<string, number> {
  return new Map(events.projects.filter((item) => item.unseen > 0).map((item) => [item.project_id, item.unseen]));
}

/** Finds the notifications that arrived since the window last looked. The first read only
 * remembers where it is, so an app start never alerts for old notifications. Inputs: the
 * newest id seen before, or null before the first read, and the app events. Output: the newest
 * id now and the unseen notifications newer than the old one, newest first. */
export function freshAlerts(last: number | null, events: Pick<AppTeamEvents, "notifications">): { last: number; fresh: TeamNotification[] } {
  const newest = events.notifications.reduce((max, item) => Math.max(max, item.id), last ?? 0);
  if (last === null) return { last: newest, fresh: [] };
  const fresh = events.notifications.filter((item) => item.id > last && item.seen_ms === null).sort((a, b) => b.id - a.id);
  return { last: newest, fresh };
}
