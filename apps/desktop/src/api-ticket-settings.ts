import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";

/** A ticket setting's key: the tracker policy keys, then who may record manual and CI check
 * results. */
export type TicketKey = "close" | "create" | "agent_edit" | "cycle_days" | "rollover" | "import" | "manual_evidence" | "ci_evidence";

/** Every ticket setting in the order the screens show it, matching the engine's choices. */
export const TICKET_KEYS: readonly TicketKey[] = ["close", "create", "agent_edit", "cycle_days", "rollover", "import", "manual_evidence", "ci_evidence"];

/** One option of a ticket setting: an option word, or a number of days for `cycle_days`. */
export type TicketValue = string | number;

/** Whether a screen edits the app values every project starts from, or one project's override. */
export type TicketScope = "app" | "project";

/** One ticket setting as the window shows it. `options` lists every option in order and
 * `proposed` is CrazyAgent's proposed default; `custom` bounds a custom number of days.
 * `here` is the value this scope sets, null when it takes `below` (the app value on a
 * project page, the proposed default on the app page); `effective` is what applies. */
export interface TicketSettingRow {
  key: TicketKey;
  options: TicketValue[];
  proposed: TicketValue;
  custom: { min: number; max: number } | null;
  here: TicketValue | null;
  below: TicketValue;
  effective: TicketValue;
}

/** One scope's ticket settings: the project, null for the app; the version a save sends
 * back; and every setting in `TICKET_KEYS` order. */
export interface TicketSettingsPage {
  project_id: string | null;
  version: number;
  keys: TicketSettingRow[];
}

/** What a row's choice holds before Save: the value below, one listed option, or a custom
 * number of days as typed. */
export type KeyDraft = { kind: "below" } | { kind: "option"; value: TicketValue } | { kind: "custom"; text: string };

/** The values one scope saves: only the keys it sets. */
export type TicketValues = Partial<Record<TicketKey, TicketValue>>;

const CUSTOM_TEXT_BYTES_MAX = 8;

/** The line a save refused as stale shows. */
export const TICKET_SETTINGS_STALE_LINE = "These ticket settings changed since you opened them. The page now shows the saved values; make your change again.";

/** The line a project without tickets shows instead of its settings. */
export const NOT_ATTACHED_LINE = "Tickets aren't set up for this project yet. Set them up on the Tickets tab, then its settings show here.";

/** The line under each settings page: when a change applies. */
export const TICKET_EFFECT_LINE = "Changes apply to the next ticket action. Tickets already closed stay closed.";

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

const KEY_TEXT: Record<TicketKey, { label: string; help: string }> = {
  close: { label: "Closing tickets", help: "Who or what closes a ticket once its work is done." },
  create: { label: "Tickets agents create", help: "Where a ticket an agent creates goes first." },
  agent_edit: { label: "What agents may change", help: "What an agent may do to tickets it didn't create." },
  cycle_days: { label: "Cycle length", help: "How long a new cycle runs when it starts." },
  rollover: { label: "Unfinished work at cycle close", help: "What happens to a closing cycle's unfinished tickets." },
  import: { label: "Roadmap import", help: "When the roadmap planning writes becomes versions and tickets." },
  manual_evidence: { label: "Manual check results", help: "Who may record whether a manual check passed." },
  ci_evidence: { label: "CI check results", help: "Who may record whether a CI check passed." },
};

const OPTION_TEXT: Record<Exclude<TicketKey, "cycle_days">, Record<string, { label: string; line: string }>> = {
  close: {
    agents: { label: "Agents", line: "An agent may close a ticket when it judges the work done." },
    evidence: { label: "When the evidence is in", line: "A ticket closes on its own once every criterion is met and every check passes." },
    owner: { label: "Only you", line: "Tickets close only when you close them." },
  },
  create: {
    direct: { label: "Straight to the backlog", line: "A ticket an agent creates goes straight to the backlog." },
    triage: { label: "Through Triage", line: "A ticket an agent creates waits in Triage until you accept it." },
    owner_only: { label: "Only you", line: "Only you create tickets; agents can't." },
  },
  agent_edit: {
    any: { label: "Anything", line: "Agents may change any ticket and plan versions and cycles." },
    status_and_criteria: { label: "Status and criteria", line: "On others' tickets, agents may only move, claim and work through criteria and checks." },
    none: { label: "Their own tickets", line: "Agents change only tickets they created." },
  },
  rollover: {
    auto: { label: "Move to the next cycle", line: "Closing a cycle moves its unfinished tickets to the next cycle, starting one if none is open." },
    ask: { label: "Ask me", line: "Closing a cycle lists its unfinished tickets so you move each one." },
    off: { label: "Leave them", line: "Unfinished tickets stay in the closed cycle." },
  },
  import: {
    auto_once: { label: "Once, on its own", line: "When planning writes the roadmap, CrazyAgent imports it one time." },
    on_request: { label: "When I ask", line: "The roadmap is imported only when you choose Import roadmap." },
  },
  manual_evidence: {
    owner: { label: "Only you", line: "A manual check is your judgment, so only you record it." },
    agents: { label: "Agents too", line: "Anyone the ticket settings let record evidence may record a manual check." },
  },
  ci_evidence: {
    engine: { label: "CrazyAgent and you", line: "Results come from CrazyAgent's own runs or from you, not from an agent's word." },
    owner: { label: "Only you", line: "Only you record CI results." },
    agents: { label: "Agents too", line: "Anyone the ticket settings let record evidence may record a CI result." },
  },
};

const REFUSALS: Record<string, string> = {
  tracker_settings_stale: TICKET_SETTINGS_STALE_LINE,
  project_not_attached: NOT_ATTACHED_LINE,
  project_unknown: "CrazyAgent doesn't know this project any more.",
  policy_value_invalid: "One of these values isn't an option of its setting.",
  cycle_days_invalid: "A cycle runs from 1 to 90 days.",
  policy_in_settings: "Ticket settings are changed here, not by a ticket action.",
  tracker_unavailable: "CrazyAgent couldn't reach its tickets. Try again.",
  tracker_request_invalid: "These ticket settings couldn't be sent. Reload the page and try again.",
  tracker_request_bounds: "These ticket settings are too large to send.",
  tracker_reply_bounds: "The engine's answer was too large to show.",
  owner_busy: "CrazyAgent is busy with another change. Try again in a moment.",
};

/** The ticket settings refusals with their own sentence, for checks. */
export const TICKET_SETTINGS_REFUSALS: Readonly<Record<string, string>> = REFUSALS;

/** A refused ticket settings read or save in plain words: its own sentence, or the shared
 * sentence that names the code. Input: the refusal. Output: the sentence. */
export function ticketSettingsRefusal(refusal: { code: string; cause?: string }): string {
  return Object.hasOwn(REFUSALS, refusal.code) ? REFUSALS[refusal.code] : failureCopy(refusal);
}

/** A setting's label and its help line. Input: the key. Output: both. */
export function keyText(key: TicketKey): { label: string; help: string } {
  return KEY_TEXT[key];
}

/** One option's label and the line saying what it does. Inputs: the key and the option.
 * Output: both; a number of days reads as weeks when it is whole weeks. */
export function optionText(key: TicketKey, value: TicketValue): { label: string; line: string } {
  if (key === "cycle_days") {
    assert(typeof value === "number" && Number.isInteger(value), "a cycle length is a whole number of days");
    const weeks = value % 7 === 0 ? value / 7 : null;
    const days = value === 1 ? "1 day" : `${value} days`;
    const label = weeks === null ? days : weeks === 1 ? "1 week" : `${weeks} weeks`;
    return { label, line: `Each new cycle runs ${days}.` };
  }
  assert(typeof value === "string", "a policy option is a word");
  const text = OPTION_TEXT[key][value];
  assert(text !== undefined, `${key} has no option ${value}`);
  return text;
}

/** The choice that takes the value below: "Default (...)" on the app page, "App default
 * (...)" on a project page. Inputs: the row and the scope. Output: the label. */
export function belowLabel(row: Pick<TicketSettingRow, "key" | "below">, scope: TicketScope): string {
  return `${scope === "app" ? "Default" : "App default"} (${optionText(row.key, row.below).label})`;
}

/** The choice a row starts from: the value below when this scope sets none, a listed option,
 * or a custom number of days. Input: the row. Output: the draft. */
export function draftOf(row: TicketSettingRow): KeyDraft {
  if (row.here === null) return { kind: "below" };
  if (row.options.includes(row.here)) return { kind: "option", value: row.here };
  assert(row.custom !== null && typeof row.here === "number", "only a custom length is outside the options");
  return { kind: "custom", text: String(row.here) };
}

/** The value a draft saves, checked as the engine checks it: an option of the row, or a whole
 * number of days inside the custom bounds. Inputs: the row and the draft. Output: the value,
 * null for the value below, or the problem in plain words. */
export function draftValue(row: TicketSettingRow, draft: KeyDraft): { value: TicketValue | null } | { problem: string } {
  if (draft.kind === "below") return { value: null };
  if (draft.kind === "option") {
    assert(row.options.includes(draft.value), "a picked option is one of the row's");
    return { value: draft.value };
  }
  assert(row.custom !== null, "only a cycle length takes a custom number");
  const text = draft.text.trim();
  const { min, max } = row.custom;
  const problem = `Use a whole number of days from ${min} to ${max}.`;
  if (text.length === 0 || text.length > CUSTOM_TEXT_BYTES_MAX || !/^[0-9]+$/.test(text)) return { problem };
  const days = Number(text);
  return days >= min && days <= max ? { value: days } : { problem };
}

/** The values a scope saves from its drafts: every key set to an option or a custom number,
 * none for a key left on the value below. Inputs: the rows and the drafts by key. Output:
 * the values, or the first row's problem with its key. */
export function valuesOf(rows: readonly TicketSettingRow[], drafts: Partial<Record<TicketKey, KeyDraft>>): { values: TicketValues } | { key: TicketKey; problem: string } {
  assert(rows.length === TICKET_KEYS.length, "a page lists every ticket setting");
  const values: TicketValues = {};
  for (const row of rows) {
    const checked = draftValue(row, drafts[row.key] ?? draftOf(row));
    if ("problem" in checked) return { key: row.key, problem: checked.problem };
    if (checked.value !== null) values[row.key] = checked.value;
  }
  assert(Object.keys(values).length <= TICKET_KEYS.length, "at most one value per setting");
  return { values };
}

/** Whether the drafts change anything the scope stores. Inputs: the rows and the drafts.
 * Output: true when Save has something to write; a draft with a problem counts as changed
 * so Save can say what is wrong. */
export function draftsChanged(rows: readonly TicketSettingRow[], drafts: Partial<Record<TicketKey, KeyDraft>>): boolean {
  return rows.some((row) => {
    const draft = drafts[row.key];
    if (draft === undefined) return false;
    const checked = draftValue(row, draft);
    return "problem" in checked || checked.value !== row.here;
  });
}

/** Whether a project page offers "Use app default": only when the project sets a key.
 * Input: the page. Output: true when there is an override to clear. */
export function canUseAppDefault(page: TicketSettingsPage): boolean {
  return page.project_id !== null && page.keys.some((row) => row.here !== null);
}

/** Checks a read page against the screen it is for: every setting once, in order, a version,
 * and the project the screen asked for. Inputs: the page and the project id, null for the
 * app. Output: the page. */
export function checkedPage(page: TicketSettingsPage, projectId: string | null): TicketSettingsPage {
  assert(page.project_id === projectId, "a ticket settings page is for the scope it was read for");
  assert(page.keys.map((row) => row.key).join() === TICKET_KEYS.join(), "a ticket settings page lists every setting once, in order");
  assert(Number.isSafeInteger(page.version) && page.version >= 0, "a ticket settings page has a version");
  return page;
}

/** Reads the ticket settings of the app (null) or one project. Input: the project id.
 * Output: the page, or the engine's refusal, such as `project_not_attached`. */
export function loadTicketSettings(projectId: string | null): Promise<Result<TicketSettingsPage>> {
  return call<TicketSettingsPage>("desktop_ticket_settings", { projectId });
}

/** Saves one scope's ticket settings at the version the page read; empty values are "Use
 * app default" for every key on a project. Inputs: the project id, null for the app, the
 * values and the version. Output: Ok once saved; `tracker_settings_stale` when it changed. */
export function saveTicketSettings(projectId: string | null, values: TicketValues, version: number): Promise<Result<void>> {
  return call<void>("desktop_save_ticket_settings", { projectId, values, version });
}
