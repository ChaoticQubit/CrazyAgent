import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import type { GraphNode } from "./engine-client";
import { formatRoute } from "./ui/router";

/** The most bytes one instructions text may hold; the engine refuses more. */
export const PROCESS_TEXT_BYTES_MAX = 65536;

const SKILL_NAME = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/u;
const NAME_CHARACTER = /[A-Za-z0-9_.:-]/u;
const WORD_CHARACTER = /[A-Za-z0-9_]/u;

/** Tells whether a name that matched up to `end` stops there: the next character is not part
 * of a name, or is punctuation such as a sentence's full stop that no letter follows.
 * Inputs: the text and the index after the name. Output: true when the name is whole. */
function endsAt(text: string, end: number): boolean {
  const next = text[end] ?? "";
  return !NAME_CHARACTER.test(next) || (!WORD_CHARACTER.test(next) && !NAME_CHARACTER.test(text[end + 1] ?? ""));
}

const REFUSALS: Record<string, string> = {
  process_text_too_large: "This text is over 64 KB. Shorten it, then save again.",
  process_text_invalid: "This text has characters CrazyAgent can't save. Remove them, then save again.",
  process_text_unsafe: "The instructions file is a link or not a plain file, so CrazyAgent won't read it. Replace it with a plain file.",
  process_text_unreadable: "The instructions file couldn't be read. Check it, then try again.",
  process_text_unwritable: "The instructions couldn't be saved. Check the folder, then try again.",
  process_project_folder_missing: "This project keeps its files without a .crazyagent folder, so it can't hold project instructions.",
  process_project_unknown: "This project has no setup yet, so it can't hold instructions.",
  process_import_missing: "There is no ~/.claude/CLAUDE.md to import.",
  process_folder_missing: "CrazyAgent's data folder is missing. Restart the engine, then try again.",
  process_folder_unsafe: "CrazyAgent's data folder isn't a plain folder, so nothing was saved.",
  process_unavailable: "Instructions aren't ready in this engine yet.",
  process_outcome_unknown: "CrazyAgent couldn't confirm what happened. Reload before you try again.",
  process_agent_unknown: "This agent isn't part of this project's team.",
  process_agent_unsupported: "CrazyAgent doesn't write instructions for this agent's harness.",
  agent_instructions_unsafe: "This agent's instructions file is a link or not a plain file, so CrazyAgent won't read it.",
  agent_instructions_too_large: "This agent's instructions file is larger than CrazyAgent writes, so it isn't shown.",
  agent_instructions_invalid: "This agent's instructions file has characters CrazyAgent doesn't write, so it isn't shown.",
  agent_instructions_unreadable: "This agent's instructions file couldn't be read. Try again.",
  instruction_settings_invalid: "This project's agent settings hold a value CrazyAgent doesn't know. Pick an option to replace it.",
  instruction_settings_unavailable: "This project's agent settings couldn't be read. Try again.",
};

/** The refusal codes of the process protocol that have their own sentence. */
export const PROCESS_REFUSALS = Object.keys(REFUSALS);

/** One plain sentence for a refused Process action. Input: the failed result.
 * Output: the sentence, the plain not-ready line, or the fallback naming the code. */
export function processMessage(result: Exclude<Result<unknown>, { kind: "ok" }>): string {
  if (result.kind === "not_ready") return "Instructions aren't ready in this build yet.";
  return Object.hasOwn(REFUSALS, result.code) ? REFUSALS[result.code] : failureCopy(result);
}

/** Counts the bytes the engine stores for a text. Input: the text. Output: UTF-8 bytes. */
export function textBytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Checks a draft before Save, as the engine does. Input: the draft.
 * Output: why it can't be saved, or undefined. */
export function processTextError(text: string): string | undefined {
  if (textBytes(text) > PROCESS_TEXT_BYTES_MAX) return REFUSALS.process_text_too_large;
  if (text.includes("\0")) return REFUSALS.process_text_invalid;
  return undefined;
}

/** Who saved a shared item's current version: nobody yet, the owner, or one agent. */
export type LastWriter = "nobody" | "owner" | { agent: string };

/** One instructions text and the version it was read at; a save names that version. */
export interface ProcessText {
  text: string;
  version: number;
}

/** What a save comes to: the bytes of app and project instructions the project's agents now
 * get (the largest across set-up projects after an app save) and the most a Claude master is
 * sure to take. */
export type ProcessSaved = { bytes: number; claude_bytes_max: number };

/** What a save of instructions did: stored as the next version, with what it comes to for app
 * or project text and null for an agent's, or refused as stale because another save landed
 * after the version it named, with that newer text and who saved it. */
export type SavedText =
  | { kind: "saved"; version: number; size: ProcessSaved | null }
  | { kind: "stale"; version: number; current: ProcessText; writer: LastWriter };

/** Reads the owner's instructions. Input: null for app-wide, or the project id.
 * Output: the text, empty when nothing is saved, and the version a save names. */
export function loadProcess(projectId: string | null): Promise<Result<ProcessText>> {
  return call<ProcessText>("desktop_process_text", { projectId });
}

/** Saves the owner's instructions whole when no other save landed since they were read.
 * Inputs: null for app-wide, or the project id; the text; the version read; and, only for
 * "Keep mine" after a stale reply, the older version the owner opened. Output: saved with
 * the new version and what it comes to, or stale with the newer text. Agents launched after
 * a save get it. */
export function saveProcess(projectId: string | null, text: string, version: number, opened: number | null = null): Promise<Result<SavedText>> {
  return call<SavedText>("desktop_save_process_text", { projectId, text, version, opened });
}

/** The line shown after a save: the plain saved line, or, when a Claude master may refuse the
 * instructions, what it takes beside what agents now get. Input: what the save comes to, null
 * when it reports none. Output: the line. */
export function savedMessage(saved: ProcessSaved | null): string {
  if (saved === null || saved.bytes <= saved.claude_bytes_max) return "Instructions saved. Agents started from now on get them.";
  const size = (bytes: number) => `${(bytes / 1024).toFixed(1)} KB`;
  return `Instructions saved, but a project's agents now get ${size(saved.bytes)} of app and project instructions and a Claude master takes only ${size(saved.claude_bytes_max)}. Shorten them before a Claude master's next turn.`;
}

/** Reads the owner's ~/.claude/CLAUDE.md for a preview. Only the owner's Import click calls
 * this; nothing is saved until the owner saves. Output: the text. */
export function importGlobalInstructions(): Promise<Result<string>> {
  return call<string>("desktop_import_global_instructions");
}

/** Reads the CrazyAgent default process for a preview. Output: the text. */
export function defaultProcess(): Promise<Result<string>> {
  return call<string>("desktop_default_process");
}

/** A skill the tags can name: its engine id, its name and its one-line description. */
export interface SkillEntry {
  id: string;
  name: string;
  description: string;
}

/** One piece of instructions text: plain text, or a skill name that matches the catalog. */
export type ProcessSegment = { kind: "text"; text: string } | { kind: "skill"; id: string; name: string; description: string };

/** Splits instructions text into plain text and skill tags: a whole skill name, not part of
 * a longer name, that matches the catalog. Longer names win over names they contain.
 * Inputs: the text and the skill catalog. Output: the pieces, which join back to the text. */
export function skillSegments(text: string, skills: readonly SkillEntry[]): ProcessSegment[] {
  const known = new Map<string, SkillEntry>();
  for (const skill of skills) {
    if (SKILL_NAME.test(skill.name) && !known.has(skill.name)) known.set(skill.name, skill);
  }
  const byFirst = new Map<string, string[]>();
  for (const name of [...known.keys()].sort((left, right) => right.length - left.length)) {
    byFirst.set(name[0], [...(byFirst.get(name[0]) ?? []), name]);
  }
  const segments: ProcessSegment[] = [];
  let plain = "";
  let at = 0;
  while (at < text.length) {
    const before = at === 0 ? "" : text[at - 1];
    const name = NAME_CHARACTER.test(before) ? undefined
      : byFirst.get(text[at])?.find((candidate) => text.startsWith(candidate, at) && endsAt(text, at + candidate.length));
    if (name === undefined) {
      plain += text[at];
      at += 1;
      continue;
    }
    if (plain) segments.push({ kind: "text", text: plain });
    plain = "";
    const skill = known.get(name);
    if (skill === undefined) throw new Error("A matched skill name is missing from the catalog.");
    segments.push({ kind: "skill", id: skill.id, name, description: skill.description });
    at += name.length;
  }
  if (plain) segments.push({ kind: "text", text: plain });
  return segments;
}

/** Where a skill tag opens: that skill's detail in Settings. Input: the skill's engine id.
 * Output: the hash. */
export function skillHref(id: string): string {
  return formatRoute({ name: "settings", tab: "plugins", skill: id });
}

/** One instruction file an agent's harness reported loading: its file name and where it was.
 * For Codex the place is the folder whose AGENTS.md chain it loaded. */
export interface LoadedFile {
  name: string;
  path: string;
}

/** Which instruction files the engine hands a project's agents. */
export type InstructionChoice = "project" | "project_and_user" | "none";

/** One project's agent settings: the instruction files its agents get and whether headless
 * Claude runs keep their transcript. */
export interface InstructionSettings {
  files: InstructionChoice;
  transcripts: boolean;
}

/** One project's agent settings and the version they were read at, which a save names. */
export interface InstructionView {
  settings: InstructionSettings;
  version: number;
}

/** The settings of a project the owner never changed. */
export const DEFAULT_INSTRUCTION_SETTINGS: InstructionSettings = { files: "project", transcripts: true };

/** Every instruction files option in the order the page lists them, with what it hands agents. */
export const INSTRUCTION_CHOICES: readonly { value: InstructionChoice; label: string; note: string }[] = [
  { value: "project", label: "Project files", note: "Agents get the project's CLAUDE.md, .claude/CLAUDE.md and AGENTS.md as committed in your checkout, and its CLAUDE.local.md. An agent's own edits to them never change what the next agent gets." },
  { value: "project_and_user", label: "Project and user files", note: "Agents get the project's files and also your ~/.claude/CLAUDE.md and ~/.codex/AGENTS.md." },
  { value: "none", label: "None", note: "Agents get no instruction files, only the project's Process instructions." },
];

/** Reads one project's agent settings. Input: the project id. Output: the settings, the
 * defaults when the owner never changed them, with their version. */
export function loadInstructionSettings(projectId: string): Promise<Result<InstructionView>> {
  return call<InstructionView>("desktop_instruction_settings", { projectId });
}

/** Stores one project's agent settings whole; agents launched after this follow them.
 * Inputs: the project id, the settings and the version the page read, null when it could
 * not read the stored settings and replaces them. Output: the settings as stored with their
 * new version, or `stale_revision` when they changed since that version. */
export function saveInstructionSettings(projectId: string, settings: InstructionSettings, version: number | null): Promise<Result<InstructionView>> {
  return call<InstructionView>("desktop_save_instruction_settings", { projectId, settings, version });
}

/** What the agent settings say after a save refused because they changed since the page
 * read them, once it shows the saved ones again. */
export const AGENT_SETTINGS_STALE = "These agent settings changed since you opened them. The page now shows the saved ones; make your change again.";

/** The version an agent settings save names: the one read, or null when the stored settings
 * could not be read and the save replaces them. Input: the read. Output: the version. */
export function settingsSaveVersion(settings: { kind: string; value?: InstructionView }): number | null {
  return settings.kind === "ok" && settings.value ? settings.value.version : null;
}

/** What became of one instruction file at a launch, as the engine recorded it. */
export type InstructionState = "given" | "same" | "too_large" | "unsafe" | "invalid" | "unreadable";

/** The engine's record of one instruction file at an agent's last launch. */
export interface InstructionRecord {
  name: string;
  bytes: number;
  state: InstructionState;
}

const FILE_STATES: Record<InstructionState, string> = {
  given: "given",
  same: "left out: the same text as a file above",
  too_large: "left out: larger than CrazyAgent gives",
  unsafe: "left out: a link or not a plain file",
  invalid: "left out: not plain text",
  unreadable: "left out: couldn't be read",
};

/** What became of one instruction file, in the owner's words. Input: the recorded state.
 * Output: the words. */
export function fileStateWords(state: InstructionState): string {
  return FILE_STATES[state];
}

/** What one team agent received at its last launch, read back from its private home: its
 * harness, whether it has launched, the instruction file text, the installed skill names and
 * the engine's record of each instruction file it found; then what its harness reported in
 * its own session records: how many were read, the instruction files it loaded and the
 * skills it invoked. */
export interface AgentInstructions {
  agent: number;
  harness: string;
  launched: boolean;
  text: string;
  skills: string[];
  sessions: number;
  loaded: LoadedFile[];
  skills_used: string[];
  files: InstructionRecord[];
}

/** The one-line summary of what an agent's harness reported: "Instructions: CLAUDE.md,
 * AGENTS.md - Skills used: tdd", each file name once. Input: the view. Output: the line,
 * saying "not reported yet" before the harness wrote a session record. */
export function reportedLine(value: Pick<AgentInstructions, "sessions" | "loaded" | "skills_used">): string {
  if (value.sessions === 0) return "Instructions: not reported yet - Skills used: not reported yet";
  const names = [...new Set(value.loaded.map((file) => file.name))];
  const files = names.length > 0 ? names.join(", ") : "none loaded by its harness";
  const used = value.skills_used.length > 0 ? value.skills_used.join(", ") : "none";
  return `Instructions: ${files} - Skills used: ${used}`;
}

const TEAM_AGENT = /^team-([1-9][0-9]{0,14})$/u;

/** Names the team agent whose instructions a graph card shows. Input: the card's node.
 * Output: null for the master card, which the engine reads as the project's newest master,
 * the agent id for an agent card, or undefined for a card with no agent. */
export function agentOf(node: Pick<GraphNode, "id" | "kind">): number | null | undefined {
  if (node.kind === "master") return null;
  const id = node.kind === "agent" ? TEAM_AGENT.exec(node.id)?.[1] : undefined;
  return id === undefined ? undefined : Number(id);
}

/** Reads what one team agent received at its last launch. Inputs: the project id and the
 * agent id, or null for the project's master. Output: the read-only view. */
export function loadAgentInstructions(projectId: string, agent: number | null): Promise<Result<AgentInstructions>> {
  return call<AgentInstructions>("desktop_agent_instructions", { projectId, agent });
}
