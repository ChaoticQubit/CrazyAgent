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

/** Counts the unseen notifications of each project for the sidebar. Input: the app events.
 * Output: a map from project id to its unseen count, without projects at zero. */
export function unseenByProject(events: Pick<AppTeamEvents, "notifications">): Map<string, number> {
  const counts = new Map<string, number>();
  for (const item of events.notifications) {
    if (item.seen_ms === null) counts.set(item.project_id, (counts.get(item.project_id) ?? 0) + 1);
  }
  return counts;
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
