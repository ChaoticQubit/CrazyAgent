import { call } from "./api";
import type { Result } from "./api";
import { failureCopy } from "./api-projects";
import type { GraphNode, ImportCommit, ImportFile, Nudges, UncommittedImport } from "./engine-client";
import { displayText } from "./screens/conversation";
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
  process_import_missing: "That file no longer exists. Pick another one.",
  process_import_unsafe: "That file isn't a plain file you own, so CrazyAgent won't read it.",
  process_import_unreadable: "That file couldn't be read. Check it, then try again.",
  process_import_outside: "That file links outside the project folder, so it stays where it is.",
  process_import_unmovable: "The instruction files couldn't be moved aside, so nothing changed. Check the project folder, then try again.",
  process_request_invalid: "CrazyAgent couldn't read that path. Pick the file again.",
  process_commit_staged: "Other files are staged in this project's checkout, so nothing was committed. Unstage or commit them, then try again.",
  process_commit_none: "No moved instruction file waits for a commit.",
  process_commit_busy: "Git is busy in this project's checkout (a merge, a rebase or a locked index), so nothing was committed. Finish it, then try again.",
  process_commit_detached: "This project's checkout isn't on a branch, so nothing was committed. Check out a branch, then try again.",
  process_commit_moved: "The branch changed while committing, so nothing was committed. Try again.",
  process_commit_identity: "Git has no user.name or user.email for this project, so nothing was committed. Set them, then try again.",
  process_commit_failed: "Git couldn't commit the move. Check the project's checkout, then try again.",
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
  process_adoption_unavailable: "Workflow adoption couldn't be read. Try again.",
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

/** Opens the native chooser for one instructions file and reads the picked file's text for a
 * preview. Only the owner's Import click calls this; nothing is saved until the owner saves.
 * Output: the text, or null when the owner closed the chooser. */
export function importInstructionsFile(): Promise<Result<string | null>> {
  return call<string | null>("desktop_import_instructions_file");
}

/** Lists a project's own instruction files an import would copy into its instructions and
 * move into .crazyagent/imported/; nothing changes. Input: the project id. Output: the files,
 * empty when there is nothing to import. */
export function loadInstructionImport(projectId: string): Promise<Result<ImportFile[]>> {
  return call<ImportFile[]>("desktop_instruction_import", { projectId });
}

/** Copies a project's own instruction files into its instructions and moves them into
 * .crazyagent/imported/, on the owner's confirm only. Input: the project id. Output: the
 * moved files. */
export function confirmInstructionImport(projectId: string): Promise<Result<ImportFile[]>> {
  return call<ImportFile[]>("desktop_confirm_instruction_import", { projectId });
}

/** Lists the moved instruction files whose move the project's checkout has not committed,
 * with its checked-out branch. Input: the project id. Output: the paths, empty when no move
 * waits for a commit, and the branch, null for a detached HEAD. */
export function loadUncommittedImport(projectId: string): Promise<Result<UncommittedImport>> {
  return call<UncommittedImport>("desktop_uncommitted_instruction_import", { projectId });
}

/** Commits the instruction import's move as one local commit on the checkout's current branch
 * holding only the moved files, never pushed, on the owner's click only. Input: the project id.
 * Output: the commit, its branch and its files. */
export function commitInstructionImport(projectId: string): Promise<Result<ImportCommit>> {
  return call<ImportCommit>("desktop_commit_instruction_import", { projectId });
}

/** The toast after a committed move, naming the branch and files. Input: the commit.
 * Output: the sentence. */
export function committedMessage(commit: ImportCommit): string {
  return `Committed the move on ${displayText(commit.branch)}: ${commit.files.join(", ")}. Nothing was pushed.`;
}

/** Why one found instruction file stays in place, in plain words. Input: its refusal code.
 * Output: the sentence. */
export function importRefusal(code: string): string {
  return Object.hasOwn(REFUSALS, code) ? REFUSALS[code] : "CrazyAgent won't import this file.";
}

/** Reads the CrazyAgent default process for a preview. Output: the text. */
export function defaultProcess(): Promise<Result<string>> {
  return call<string>("desktop_default_process");
}

/** Reads the instructions a project's agents get, composed by the engine as launches compose
 * them for the project's mode. Input: the project id. Output: the text, empty when agents get
 * none. */
export function loadGivenText(projectId: string): Promise<Result<string>> {
  return call<string>("desktop_process_given", { projectId });
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

/** Which of the owner's two instruction texts a project's agents get (DEC-106). */
export type InstructionMode = "project_only" | "app_only" | "both";

/** How an edit to the instructions reaches agents that already run (DEC-111 QR1). */
export type EditDelivery = "next_message" | "restart" | "badge";

/** One project's agent settings: which instructions its agents get, whether headless
 * Claude runs keep their transcript and how an instruction edit reaches running agents. */
export interface InstructionSettings {
  mode: InstructionMode;
  transcripts: boolean;
  edits?: EditDelivery;
}

/** One project's agent settings and the version they were read at, which a save names. */
export interface InstructionView {
  settings: InstructionSettings;
  version: number;
}

/** The settings of a project the owner never changed. */
export const DEFAULT_INSTRUCTION_SETTINGS: InstructionSettings = { mode: "both", transcripts: true, edits: "next_message" };

/** Every way an instruction edit can reach running agents, in the order the page lists
 * them, with what each does. */
export const EDIT_DELIVERIES: readonly { value: EditDelivery; label: string; note: string }[] = [
  { value: "next_message", label: "Next task", note: "A running agent's next task starts with the sections you changed, sent once." },
  { value: "restart", label: "Restart when idle", note: "A running agent restarts when it is next idle and resumes its conversation with the new text." },
  { value: "badge", label: "Badge only", note: "Nothing is sent. A running agent shows Instructions changed until it starts again." },
];

/** Every instructions option in the order the page lists them, with what it hands agents. */
export const INSTRUCTION_MODES: readonly { value: InstructionMode; label: string; note: string }[] = [
  { value: "project_only", label: "Project only", note: "Agents get only this project's instructions, from its .crazyagent/instructions.md." },
  { value: "app_only", label: "App only", note: "Agents get only the app instructions from Settings > Process." },
  { value: "both", label: "Both (app, then project appended)", note: "Agents get the app instructions, then this project's appended. An empty project text gives the app text as it is." },
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
  same: "left out: the same text as the instructions or a file above",
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

/** What one team agent received at its last launch: its harness, whether it has launched
 * and the instruction file text, read back from its private home, then the engine's own
 * records, kept outside that home, of the installed skill names and each instruction file;
 * then what its harness reported in its own session records: how many were read, the
 * instruction files it loaded and the skills it invoked. */
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

const CLAUDE_GIVEN = "none loaded by Claude itself; CrazyAgent gives its instructions in the launch prompt";
const CLAUDE_NO_RECORD = "A headless Claude run reports nothing while Keep transcripts for headless Claude runs is off.";

/** The one-line summary of what an agent's harness reported: "Instructions: CLAUDE.md,
 * AGENTS.md - Skills used: tdd", each file name once. Claude loads no instruction file
 * itself, since CrazyAgent gives it the instructions in its launch prompt, and a headless
 * Claude run writes no session record while its project keeps no transcripts, so the line
 * says so. Input: the view. Output: the line, "not reported yet" before a session record. */
export function reportedLine(value: Pick<AgentInstructions, "harness" | "sessions" | "loaded" | "skills_used">): string {
  const claude = value.harness === "claude-code";
  if (value.sessions === 0) return `Instructions: not reported yet - Skills used: not reported yet${claude ? `. ${CLAUDE_NO_RECORD}` : ""}`;
  const names = [...new Set(value.loaded.map((file) => file.name))];
  const files = names.length > 0 ? names.join(", ") : claude ? CLAUDE_GIVEN : "none loaded by its harness";
  const used = value.skills_used.length > 0 ? value.skills_used.join(", ") : "none";
  return `Instructions: ${files} - Skills used: ${used}`;
}

/** Names the team agent whose instructions a graph card shows. Input: the card's node.
 * Output: null for the master card, which the engine reads as the project's newest master,
 * the agent id for a lead, sub-agent or teammate card, or undefined for a card with no agent,
 * such as a native teammate known only by its pane, or one whose agent id is missing, null
 * or not a positive safe integer. */
export function agentOf(node: Pick<GraphNode, "kind" | "agent">): number | null | undefined {
  if (node.kind === "master") return null;
  return typeof node.agent === "number" && Number.isSafeInteger(node.agent) && node.agent > 0 ? node.agent : undefined;
}

/** Reads what one team agent received at its last launch. Inputs: the project id and the
 * agent id, or null for the project's master. Output: the read-only view. */
export function loadAgentInstructions(projectId: string, agent: number | null): Promise<Result<AgentInstructions>> {
  return call<AgentInstructions>("desktop_agent_instructions", { projectId, agent });
}

/** A workflow habit an agent missed: editing files with no declared work, or working ten
 * minutes with no code-graph query and no context-mode call. */
export type AdoptionFlag = "edits_without_start_work" | "no_graphify_or_context_mode";

const FLAG_WORDS: Record<AdoptionFlag, string> = {
  edits_without_start_work: "Edits without start_work",
  no_graphify_or_context_mode: "10+ min without graphify or context-mode",
};

/** A flag in the owner's words. Input: the flag. Output: the words. */
export function flagWords(flag: AdoptionFlag): string {
  return FLAG_WORDS[flag];
}

/** One skill an agent called by name, with how many times. */
export interface SkillCount {
  name: string;
  count: number;
}

/** One agent's adoption row: who it is, whether it still runs, its count for each counted
 * tool in the settings' order, its calls of each enabled skill in the view's skill order
 * (zero included), the skills it called that the project has not enabled, its graphify and
 * ctx_* calls, its flags now and the flags it was already reminded of, and whether its
 * launch fires no hooks, whose calls the engine counts from its transcript instead. */
export interface AgentAdoption {
  agent: number;
  role: string;
  harness: string;
  active: boolean;
  counts: number[];
  skills?: number[];
  other_skills?: SkillCount[];
  graphify?: number;
  context_mode?: number;
  flags: AdoptionFlag[];
  reminded: AdoptionFlag[];
  hookless?: boolean;
}

/** A project's workflow adoption: its nudges option, the counted tools that name the
 * count columns, the skills its agents receive, its newest agents, and how many older agents
 * the view's size limit left out. */
export interface Adoption {
  nudges: Nudges;
  counted: string[];
  skills?: string[];
  agents: AgentAdoption[];
  hidden?: number;
}

/** Reads one project's workflow adoption. Input: the project id. Output: the view. */
export function loadAdoption(projectId: string): Promise<Result<Adoption>> {
  return call<Adoption>("desktop_adoption", { projectId });
}
