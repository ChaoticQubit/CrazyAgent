import { call, nextGeneration } from "./api";
import type { Failure, Result } from "./api";
import { failureCopy, loadTeamStatus } from "./api-projects";
import type { AgentMessage, AgentMessages, IntentReceipt, MessageCursor, MessageKind, MessageSettings, MessageSource, MessageSpeaker, TeamStatus } from "./engine-client";

/** The kept-message counts a project may choose; the engine refuses any other. */
export const KEPT_CHOICES = [500, 2000, 5000] as const;

/** The page sizes a project may choose; the engine refuses any other. */
export const PAGE_CHOICES = [25, 50, 100] as const;

/** The settings the engine proposes until the owner saves some. */
export const DEFAULT_MESSAGE_SETTINGS: MessageSettings = {
  keep_master_messages: true, kept_per_project: 2000, progress_shown: true, results: "first_line", owner_items_shown: true, page_size: 50,
};

/** What a master's message without kept text shows. */
export const TEXT_NOT_KEPT = "Message sent, text not kept.";

const SOURCE_ORDER: Record<MessageSource, number> = { task: 0, message: 1, event: 2 };

const SPEAKERS: Record<Exclude<MessageSpeaker, "agent">, string> = { master: "Master", owner: "You", engine: "CrazyAgent" };

const KINDS: Record<MessageKind, string> = {
  task: "Task", wake: "Wake", message: "Message", answer: "Answer", prompt: "Prompt", question: "Question",
  progress: "Progress", change_set_ready: "Changes ready", finished: "Finished", failed: "Failed",
};

const REFUSALS: Record<string, string> = {
  team_agent_not_found: "This agent is no longer on this project's team.",
  team_messages_limit_invalid: "CrazyAgent asked for a page size the engine doesn't offer. Reopen this agent's messages.",
  team_message_settings_invalid: "That choice isn't one CrazyAgent offers. Pick one of the listed options.",
};

/** Reads one page of a sub-agent's Messages timeline, newest first, at the project's page size.
 * Inputs: the project id, the agent's team id and the cursor of the newest entry not wanted,
 * null for the newest page. Output: the page or the failure. */
export function loadAgentMessages(projectId: string, agent: number, before: MessageCursor | null): Promise<Result<AgentMessages>> {
  return call<AgentMessages>("desktop_team_agent_messages", { id: projectId, agent, before: before === null ? null : cursorOf(before), limit: null });
}

/** Stores a project's Messages settings whole. Inputs: the project id and the settings.
 * Output: the engine's receipt or the failure. */
export function saveMessageSettings(projectId: string, settings: MessageSettings): Promise<Result<IntentReceipt>> {
  const { keep_master_messages, kept_per_project, progress_shown, results, owner_items_shown, page_size } = settings;
  return call<IntentReceipt>("desktop_team_message_settings", {
    id: projectId, settings: { keep_master_messages, kept_per_project, progress_shown, results, owner_items_shown, page_size }, generation: nextGeneration(),
  });
}

/** What changing one Messages setting came to: sent with the settings built from the engine's
 * stored ones, refused because another client changed them since the owner's view was read, or
 * a team read that failed. */
export type SettingChange =
  | { kind: "sent"; settings: MessageSettings; result: Result<IntentReceipt> }
  | { kind: "elsewhere"; settings: MessageSettings }
  | { kind: "unread"; result: Failure };

/** Changes one Messages setting against what the engine stores now. It reads the team first;
 * when the stored settings differ from the ones the owner saw, it saves nothing and returns
 * them, so a view one poll old never undoes another client's change. Inputs: the project id,
 * the settings shown, the setting and its choice. Output: what the change came to. */
export async function changeMessageSetting(projectId: string, seen: MessageSettings, key: keyof MessageSettings, value: string): Promise<SettingChange> {
  const read = await loadTeamStatus(projectId);
  if (read.kind !== "ok") return { kind: "unread", result: read };
  const stored = messageSettingsOf(read.value);
  if (stored === null) return { kind: "unread", result: { kind: "not_ready" } };
  if (MESSAGE_SETTING_FIELDS.some((field) => fieldValue(stored, field.key) !== fieldValue(seen, field.key))) return { kind: "elsewhere", settings: stored };
  const settings = withField(stored, key, value);
  return { kind: "sent", settings, result: await saveMessageSettings(projectId, settings) };
}

/** A project's stored Messages settings from its team status. Input: the status. Output: the
 * settings, or null from an engine that has none. */
export function messageSettingsOf(status: Pick<TeamStatus, "message_settings">): MessageSettings | null {
  return status.message_settings ?? null;
}

/** Where an entry sits in its timeline, for the page after it. Input: the entry. Output: its
 * cursor and nothing else. */
export function cursorOf(entry: MessageCursor): MessageCursor {
  return { at_ms: entry.at_ms, source: entry.source, id: entry.id };
}

/** One entry's key, unique in its timeline. Input: the entry. Output: the key. */
export function entryKey(entry: MessageCursor): string {
  return `${entry.source}-${entry.id}`;
}

/** Orders entries the way the engine pages them, newest first: by time, then store, then id.
 * Inputs: two entries. Output: negative when the first is newer. */
export function newerFirst(a: MessageCursor, b: MessageCursor): number {
  return b.at_ms - a.at_ms || SOURCE_ORDER[b.source] - SOURCE_ORDER[a.source] || b.id - a.id;
}

/** One agent's timeline as shown: its entries newest first, whether older ones remain and the
 * settings the newest page was read under. */
export interface Timeline {
  entries: AgentMessage[];
  older: boolean;
  settings: MessageSettings;
}

/** Takes a fresh newest page into the shown timeline. Older pages the owner loaded stay below
 * it, unless the page reaches no further than the shown entries begin, holds every entry, or
 * was read under other settings: then the page alone is shown, so no entry is missing or stale.
 * Inputs: the shown timeline, if any, and the page. Output: the timeline. */
export function withNewest(shown: Timeline | null, page: AgentMessages): Timeline {
  const fresh: Timeline = { entries: page.entries, older: page.older, settings: page.settings };
  const oldest = page.entries.at(-1);
  if (shown === null || oldest === undefined || !page.older || JSON.stringify(shown.settings) !== JSON.stringify(page.settings)) return fresh;
  const below = shown.entries.filter((entry) => newerFirst(oldest, entry) < 0);
  if (below.length === 0 || below.length === shown.entries.length) return fresh;
  return { entries: [...page.entries, ...below], older: shown.older, settings: page.settings };
}

/** Adds the next older page below the shown timeline. Inputs: the shown timeline and the page
 * read before its oldest entry. Output: the timeline. */
export function withOlder(shown: Timeline, page: AgentMessages): Timeline {
  const last = shown.entries.at(-1);
  const older = last === undefined ? page.entries : page.entries.filter((entry) => newerFirst(last, entry) < 0);
  return { entries: [...shown.entries, ...older], older: page.older, settings: shown.settings };
}

/** Names who an entry comes from, from the engine's field only. Inputs: the speaker and the
 * agent's name. Output: the label. */
export function speakerName(from: MessageSpeaker, agent: string): string {
  return from === "agent" ? agent : SPEAKERS[from];
}

/** Names what an entry is. Input: the kind. Output: the badge word. */
export function kindWord(kind: MessageKind): string {
  return KINDS[kind];
}

/** The text one entry shows: what was said, or that a master's message was sent without its
 * text kept. Input: the entry. Output: the text. */
export function entryText(entry: Pick<AgentMessage, "text">): string {
  return entry.text ?? TEXT_NOT_KEPT;
}

/** The first line of a result with words on it. Input: the result. Output: the line. */
export function resultLine(result: string): string {
  return result.split("\n").map((line) => line.trim()).find((line) => line !== "") ?? "";
}

/** What a failed Messages read or save means for the owner. Input: the failure. Output: one
 * sentence. */
export function messagesProblem(failure: Failure): string {
  if (failure.kind === "not_ready") return "Not ready yet: agent messages aren't in this build.";
  return REFUSALS[failure.code] ?? failureCopy(failure);
}

/** One Messages setting as the owner chooses it: the label, a line on what it does and every
 * choice the engine accepts. */
export interface MessageSettingField {
  key: keyof MessageSettings;
  label: string;
  help: string;
  options: readonly { value: string; label: string }[];
}

const SHOWN = [{ value: "shown", label: "Shown" }, { value: "hidden", label: "Hidden" }] as const;

/** The six Messages settings, each with all its choices. */
export const MESSAGE_SETTING_FIELDS: readonly MessageSettingField[] = [
  { key: "keep_master_messages", label: "Keep the master's messages to agents", help: "Off keeps only that the master sent a message, not what it said.",
    options: [{ value: "on", label: "On" }, { value: "off", label: "Off" }] },
  { key: "kept_per_project", label: "Messages kept per project", help: "When the project holds this many, the oldest make room.",
    options: KEPT_CHOICES.map((count) => ({ value: String(count), label: String(count) })) },
  { key: "progress_shown", label: "Progress events in the timeline", help: "Hidden leaves out progress reports, for harnesses that send many.", options: SHOWN },
  { key: "results", label: "Task results in the timeline", help: "Results can be long. The first line shows until you open it, or the whole result shows.",
    options: [{ value: "first_line", label: "First line, then expand" }, { value: "full", label: "Full text" }] },
  { key: "owner_items_shown", label: "Your own tasks and prompts in the timeline", help: "Shown keeps everything the agent was asked in one place.", options: SHOWN },
  { key: "page_size", label: "Entries per page", help: "Load older reads the next page.",
    options: PAGE_CHOICES.map((size) => ({ value: String(size), label: String(size) })) },
];

/** The choice one setting holds. Inputs: the settings and the setting. Output: the choice. */
export function fieldValue(settings: MessageSettings, key: keyof MessageSettings): string {
  const value = settings[key];
  if (key === "keep_master_messages") return value ? "on" : "off";
  if (typeof value === "boolean") return value ? "shown" : "hidden";
  return String(value);
}

/** The settings with one setting changed to one of its choices. Inputs: the settings, the
 * setting and the choice. Output: the new settings; an unlisted choice throws. */
export function withField(settings: MessageSettings, key: keyof MessageSettings, value: string): MessageSettings {
  const field = MESSAGE_SETTING_FIELDS.find((item) => item.key === key);
  if (!field?.options.some((option) => option.value === value)) throw new Error(`${value} is not a choice of ${key}`);
  switch (key) {
    case "keep_master_messages": return { ...settings, keep_master_messages: value === "on" };
    case "progress_shown": return { ...settings, progress_shown: value === "shown" };
    case "owner_items_shown": return { ...settings, owner_items_shown: value === "shown" };
    case "results": return { ...settings, results: value === "full" ? "full" : "first_line" };
    case "kept_per_project": return { ...settings, kept_per_project: Number(value) };
    case "page_size": return { ...settings, page_size: Number(value) };
  }
}
