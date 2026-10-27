import type { HarnessEntry, PoolSummary } from "../../api";
import type { HarnessSlot, Platform, PoolChoice, ProjectSetup, ProjectSpec, ResearchKind, StoredSetup, TeamPlan } from "../../api-projects";
import type { AccountPool, ModelConnection } from "../../engine-client";

export type StepId = "basics" | "documents" | "work" | "pool" | "team";

export const STEPS: { id: StepId; label: string }[] = [
  { id: "basics", label: "Basics" },
  { id: "documents", label: "Documents" },
  { id: "work", label: "Work" },
  { id: "pool", label: "Pool" },
  { id: "team", label: "Team" },
];

export const NAME_CHARS_MAX = 80;
export const ABOUT_CHARS_MAX = 4000;
export const OTHER_PLATFORM_CHARS_MAX = 60;
export const CUSTOM_STACK_CHARS_MAX = 40;
export const STACK_MAX = 32;
export const HARNESS_AGENTS_MAX = 15;
export const TEAM_AGENTS_MAX = 64;
export const SUB_HARNESSES_MAX = 8;

export const RESEARCH_COPY: Record<ResearchKind, string> = {
  market_research: "Who needs this, how big the market is, and how people pay today.",
  competitor_research: "Products that already do this, their features, prices and gaps.",
  design_research: "Patterns and references for how this kind of product looks and works.",
  front_end_design: "A first design direction for the screens, based on the research.",
  tech_stack_research: "Libraries and services that fit your stack and platforms.",
};

export interface DocumentPick {
  path: string;
  name: string;
}

const DOCUMENT_TYPES: Record<string, string> = {
  md: "Markdown", pdf: "PDF", docx: "Word", csv: "CSV", jpg: "JPG", jpeg: "JPG", png: "PNG", txt: "Text",
};

/** Names a document's type from its extension, as the file list shows it.
 * Input: the file name. Output: for example "Markdown", or "File" without an extension. */
export function documentType(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "File";
  const extension = name.slice(dot + 1).toLowerCase();
  return DOCUMENT_TYPES[extension] ?? extension.toUpperCase();
}

export interface ProjectDraft {
  name: string;
  about: string;
  stack: string[];
  platforms: Platform[];
  otherPlatform: string;
  documents: DocumentPick[];
  research: ResearchKind[];
  pool: string | null;
  poolChosen: boolean;
  team: TeamDraft | null;
}

/** One agent row being edited; `tools_wanted` keeps the owner's choice while the harness forces
 * tools on. */
export type SlotDraft = HarnessSlot & { tools_wanted?: boolean };

/** A team plan being edited: while a harness can't turn tools off, the owner's own choice is
 * kept beside the forced value so switching back restores it. `wireTeam` drops it before sending. */
export type TeamDraft = Omit<TeamPlan, "sub_harnesses"> & { master_tools_wanted?: boolean; sub_harnesses: SlotDraft[] };

/** Starts a New Project draft with the design's default research toggles.
 * Input: none. Output: an empty draft. */
export function emptyDraft(): ProjectDraft {
  return {
    name: "", about: "", stack: [], platforms: [], otherPlatform: "", documents: [],
    research: ["market_research", "competitor_research", "design_research"], pool: null, poolChosen: false, team: null,
  };
}

const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u;
const chars = (value: string) => [...value].length;

/** Checks the Basics step. Inputs: the draft and the names of existing projects.
 * Output: one plain message per field with a problem; empty when the step is valid. */
export function basicsErrors(draft: ProjectDraft, existing: string[]): { name?: string; about?: string; otherPlatform?: string } {
  const errors: { name?: string; about?: string; otherPlatform?: string } = {};
  const name = draft.name.trim();
  if (!name) errors.name = "Enter a project name.";
  else if (chars(name) > NAME_CHARS_MAX) errors.name = `Use at most ${NAME_CHARS_MAX} characters.`;
  else if (/[\u0000-\u001f\u007f-\u009f]/u.test(name)) errors.name = "Remove control characters from the name.";
  else {
    const same = existing.find((other) => other.trim().toLowerCase() === name.toLowerCase());
    if (same !== undefined) errors.name = `You already have a project called ${same}.`;
  }
  if (chars(draft.about) > ABOUT_CHARS_MAX) errors.about = `Use at most ${ABOUT_CHARS_MAX} characters.`;
  else if (CONTROL.test(draft.about)) errors.about = "Remove control characters from the description.";
  if (draft.platforms.includes("other")) {
    if (chars(draft.otherPlatform.trim()) > OTHER_PLATFORM_CHARS_MAX) errors.otherPlatform = `Use at most ${OTHER_PLATFORM_CHARS_MAX} characters.`;
    else if (/[\u0000-\u001f\u007f-\u009f]/u.test(draft.otherPlatform)) errors.otherPlatform = "Remove control characters.";
  }
  return errors;
}

/** Adds one tech stack entry, ignoring a case-insensitive duplicate.
 * Inputs: the current stack and the entry. Output: the new stack, or an error. */
export function addStack(stack: string[], entry: string): { stack: string[]; error?: string } {
  const value = entry.trim();
  if (!value) return { stack, error: "Enter a name for the custom entry." };
  if (chars(value) > CUSTOM_STACK_CHARS_MAX) return { stack, error: `Custom entries can be up to ${CUSTOM_STACK_CHARS_MAX} characters.` };
  if (/[\u0000-\u001f\u007f-\u009f]/u.test(value)) return { stack, error: "Remove control characters." };
  if (stack.some((item) => item.toLowerCase() === value.toLowerCase())) return { stack };
  if (stack.length >= STACK_MAX) return { stack, error: `Pick at most ${STACK_MAX} tech stack items.` };
  return { stack: [...stack, value] };
}

export function removeStack(stack: string[], entry: string): string[] {
  return stack.filter((item) => item !== entry);
}

/** Adds picked files after checking type, name and count; the engine checks size and kind again.
 * Inputs: attached files, picked absolute paths, allowed extensions and the count limit.
 * Output: the new list and one plain message per refused file. */
export function addDocuments(current: DocumentPick[], paths: string[], extensions: string[], limit: number): { documents: DocumentPick[]; errors: string[] } {
  const documents = [...current];
  const errors: string[] = [];
  for (const path of paths) {
    if (path.endsWith("/")) {
      errors.push("Can't add a folder. Choose the files inside it.");
      continue;
    }
    const name = path.slice(path.lastIndexOf("/") + 1);
    if (!path.startsWith("/") || CONTROL.test(path) || !name) {
      errors.push(`${name || path} can't be added. Choose it again with Choose files.`);
      continue;
    }
    const dot = name.lastIndexOf(".");
    const extension = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
    if (!extensions.includes(extension)) {
      errors.push(`${name}: this file type isn't supported.`);
      continue;
    }
    if (documents.some((document) => document.name === name)) {
      errors.push(`${name} is already attached.`);
      continue;
    }
    if (documents.length >= limit) {
      if (!errors.includes(`Attach at most ${limit} documents.`)) errors.push(`Attach at most ${limit} documents.`);
      continue;
    }
    documents.push({ path, name });
  }
  return { documents, errors };
}

/** Formats a byte count the way the design writes sizes.
 * Input: bytes. Output: for example "50 MB" or "1.5 KB". */
export function formatBytes(bytes: number): string {
  const units = ["bytes", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const shown = unit === 0 || value >= 10 || Number.isInteger(value) ? Math.round(value).toString() : value.toFixed(1);
  return `${shown} ${units[unit]}`;
}

export function totalAgents(team: TeamPlan): number {
  return 1 + team.sub_harnesses.reduce((sum, slot) => sum + slot.agents, 0);
}

/** What an empty agent list means (DEC-096 addendum 3): any harness runs, up to the team maximum. */
export const ANY_HARNESS_NOTE = "Any harness, no limit. Add a harness to limit which ones run and how many.";

/** Says what an empty agent list runs: any harness, or no sub-agent under a team maximum of one.
 * Input: the team. Output: the note shown where the agent rows would be. */
export function anyHarnessNote(team: TeamPlan): string {
  return team.max_agents <= 1 ? "No sub-agents run with a team maximum of 1. Raise the maximum to let any harness run." : ANY_HARNESS_NOTE;
}

/** Names the sub-agents an empty agent list allows: the team maximum less the master.
 * Input: the team. Output: for example "Any harness, up to 63 agents". */
export function anyHarnessTotal(team: TeamPlan): string {
  const subs = Math.max(0, team.max_agents - 1);
  return subs === 0 ? "No sub-agents (team maximum is 1)" : `Any harness, up to ${subs} ${subs === 1 ? "agent" : "agents"}`;
}

/** Says how many agents the team has: up to the team maximum when no harness is chosen,
 * otherwise the exact total. Input: the team. Output: one sentence. */
export function agentTotalText(team: TeamPlan): string {
  const count = team.sub_harnesses.length ? totalAgents(team) : team.max_agents;
  const noun = count === 1 ? "agent" : "agents";
  return team.sub_harnesses.length ? `This team has ${count} ${noun}.` : `This team has up to ${count} ${noun}.`;
}

function entry(harnesses: HarnessEntry[] | null, id: string): HarnessEntry | undefined {
  return harnesses?.find((harness) => harness.id === id);
}

const choosable = (harness: HarnessEntry | undefined) => harness !== undefined && harness.status.kind !== "not_installed";
const toolsFixed = (harness: HarnessEntry | undefined) => harness?.tools.kind === "limited";

function toolsFor(wanted: boolean, harness: HarnessEntry | undefined): { tools: boolean; wanted?: boolean } {
  return !wanted && toolsFixed(harness) ? { tools: true, wanted } : { tools: wanted };
}

function withMaster(team: TeamDraft, master: string, wanted: boolean, harnesses: HarnessEntry[] | null): TeamDraft {
  const tools = toolsFor(wanted, entry(harnesses, master));
  const { master_tools_wanted: _kept, ...rest } = team;
  return { ...rest, master, master_tools: tools.tools, ...(tools.wanted === undefined ? {} : { master_tools_wanted: tools.wanted }) };
}

function withSlot(slot: SlotDraft, harness: string, wanted: boolean, harnesses: HarnessEntry[] | null): SlotDraft {
  const tools = toolsFor(wanted, entry(harnesses, harness));
  return { harness, agents: slot.agents, tools: tools.tools, ...(tools.wanted === undefined ? {} : { tools_wanted: tools.wanted }) };
}

/** The team plan the engine takes, without the owner choices kept while tools are forced on.
 * Input: the edited team. Output: exactly the engine's team fields. */
export function wireTeam(team: TeamDraft): TeamPlan {
  return {
    master: team.master, master_tools: team.master_tools,
    sub_harnesses: team.sub_harnesses.map((slot) => ({ harness: slot.harness, agents: slot.agents, tools: slot.tools })),
    min_agents: team.min_agents, max_agents: team.max_agents,
  };
}

/** Applies the harness catalog to the engine's smart team defaults: a harness that
 * is not installed is replaced by the first ready one, and fixed tools stay on.
 * Inputs: the engine default and the catalog, or null while it is not ready. Output: the team. */
export function defaultTeam(engine: TeamPlan, harnesses: HarnessEntry[] | null): TeamPlan {
  if (!harnesses) return engine;
  const fallback = harnesses.find((harness) => harness.status.kind === "ready") ?? harnesses.find((harness) => choosable(harness));
  const pick = (id: string) => choosable(entry(harnesses, id)) || !fallback ? id : fallback.id;
  const master = pick(engine.master);
  const team: TeamPlan = {
    ...engine,
    master,
    master_tools: engine.master_tools || toolsFixed(entry(harnesses, master)),
    sub_harnesses: engine.sub_harnesses.map((slot) => {
      const harness = pick(slot.harness);
      return { ...slot, harness, tools: slot.tools || toolsFixed(entry(harnesses, harness)) };
    }),
  };
  return team;
}

/** Turns tools on or off for the master or one agent row, keeping fixed tools on.
 * Inputs: the team, "master" or a row index, the wanted value and the catalog. Output: the team. */
export function setTools(team: TeamDraft, row: "master" | number, tools: boolean, harnesses: HarnessEntry[] | null): TeamDraft {
  if (row === "master") return withMaster(team, team.master, tools, harnesses);
  return { ...team, sub_harnesses: team.sub_harnesses.map((slot, index) => index === row ? withSlot(slot, slot.harness, tools, harnesses) : slot) };
}

const withRoom = (team: TeamDraft): TeamDraft => ({ ...team, max_agents: Math.min(TEAM_AGENTS_MAX, Math.max(team.max_agents, totalAgents(team))) });

/** Picks the master harness; tools follow the owner's choice unless this harness can't turn
 * them off. Inputs: the team, the harness id and the catalog. Output: the team. */
export function setMaster(team: TeamDraft, harness: string, harnesses: HarnessEntry[] | null): TeamDraft {
  return withMaster(team, harness, team.master_tools_wanted ?? team.master_tools, harnesses);
}

/** Picks the harness of one agent row; tools follow the owner's choice unless this harness can't
 * turn them off. Inputs: the team, the row, the harness id and the catalog. Output: the team. */
export function setRowHarness(team: TeamDraft, row: number, harness: string, harnesses: HarnessEntry[] | null): TeamDraft {
  return {
    ...team,
    sub_harnesses: team.sub_harnesses.map((slot, index) => index === row ? withSlot(slot, harness, slot.tools_wanted ?? slot.tools, harnesses) : slot),
  };
}

export function setAgents(team: TeamDraft, row: number, agents: number): TeamDraft {
  const count = Math.min(HARNESS_AGENTS_MAX, Math.max(1, Math.round(Number.isFinite(agents) ? agents : 1)));
  return withRoom({ ...team, sub_harnesses: team.sub_harnesses.map((slot, index) => index === row ? { ...slot, agents: count } : slot) });
}

/** Adds an agent row on the first ready harness the team does not use yet.
 * Inputs: the team and the catalog. Output: the team with one more row of one agent. */
export function addHarnessRow(team: TeamDraft, harnesses: HarnessEntry[] | null): TeamDraft {
  if (team.sub_harnesses.length >= SUB_HARNESSES_MAX) return team;
  const used = new Set(team.sub_harnesses.map((slot) => slot.harness));
  const ready = (harnesses ?? []).filter((harness) => harness.status.kind === "ready");
  const harness = ready.find((candidate) => !used.has(candidate.id))?.id ?? ready[0]?.id ?? team.master;
  return withRoom({ ...team, sub_harnesses: [...team.sub_harnesses, { harness, agents: 1, tools: true }] });
}

export function removeHarnessRow(team: TeamDraft, row: number): TeamDraft {
  return { ...team, sub_harnesses: team.sub_harnesses.filter((_, index) => index !== row) };
}

const harnessName = (harnesses: HarnessEntry[] | null, id: string) => entry(harnesses, id)?.name ?? id;

/** Lists the plain notes for limited harnesses the team uses.
 * Inputs: the team and the catalog. Output: one sentence per limitation, each once. */
export function teamNotes(team: TeamPlan, harnesses: HarnessEntry[] | null): string[] {
  const notes: string[] = [];
  for (const id of [team.master, ...team.sub_harnesses.map((slot) => slot.harness)]) {
    const harness = entry(harnesses, id);
    if (!harness) continue;
    const add = (reason: string) => {
      const note = `${harness.name}: ${reason.replace(/\.$/u, "")}.`;
      if (!notes.includes(note)) notes.push(note);
    };
    if (harness.tools.kind === "limited") add(harness.tools.reason);
    if (harness.status.kind === "limited") add(harness.status.reason);
  }
  return notes;
}

const GROUPS = { ready: "Ready", limited: "Limited control", not_installed: "Not installed" } as const;

/** Orders every catalog harness for a picker: Ready, Limited control, Not installed.
 * Input: the catalog. Output: options; a harness that is not installed or can't run team
 * agents can't be picked. */
export function harnessChoices(harnesses: HarnessEntry[]): { id: string; label: string; group: string; disabled: boolean }[] {
  return (["ready", "limited", "not_installed"] as const).flatMap((kind) => harnesses
    .filter((harness) => harness.status.kind === kind)
    .map((harness) => {
      const blocked = harness.team.kind === "unavailable";
      const label = kind === "not_installed" ? `${harness.name} - Not installed` : blocked ? `${harness.name} - Can't run team agents` : harness.name;
      return { id: harness.id, label, group: GROUPS[kind], disabled: kind === "not_installed" || blocked };
    }));
}

const HARNESS_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u;
const RANGE = "At most must be at least 1 and not less than At least.";

/** Checks the Team step with the engine's own agent range rules.
 * Inputs: the team and the catalog, or null while it is not ready.
 * Output: messages for the master, each agent row and the agent range. */
export function teamErrors(team: TeamPlan, harnesses: HarnessEntry[] | null): { master?: string; rows: (string | undefined)[]; range?: string } {
  const errors: { master?: string; rows: (string | undefined)[]; range?: string } = { rows: [] };
  const harnessProblem = (id: string, what: string) => {
    if (!HARNESS_ID.test(id)) return `Pick a harness for ${what}.`;
    const harness = entry(harnesses, id);
    if (harness?.status.kind === "not_installed") return `${harness.name} isn't installed on this Mac. Pick another harness.`;
    if (harness?.team.kind === "unavailable") return `${harness.name} can't run team agents yet. Pick another harness.`;
    return undefined;
  };
  errors.master = harnessProblem(team.master, "the master");
  if (errors.master === undefined) delete errors.master;
  errors.rows = team.sub_harnesses.map((slot) => {
    const problem = harnessProblem(slot.harness, "this row");
    if (problem) return problem;
    return Number.isInteger(slot.agents) && slot.agents >= 1 && slot.agents <= HARNESS_AGENTS_MAX ? undefined : `Each harness runs 1 to ${HARNESS_AGENTS_MAX} agents.`;
  });
  const total = totalAgents(team);
  if (team.sub_harnesses.length > SUB_HARNESSES_MAX) errors.range = `Use at most ${SUB_HARNESSES_MAX} agent harnesses.`;
  else if (!Number.isInteger(team.min_agents) || !Number.isInteger(team.max_agents) || team.min_agents < 1 || team.min_agents > team.max_agents) errors.range = RANGE;
  else if (team.max_agents > TEAM_AGENTS_MAX) errors.range = `At most can be up to ${TEAM_AGENTS_MAX}.`;
  else if (total > team.max_agents) errors.range = `This team has ${total} agents: the master and ${total - 1} more. Set At most to ${total} or more.`;
  else if (total < team.min_agents) errors.range = `This team has ${total} agents. Set At least to ${total} or less.`;
  return errors;
}

export function teamValid(team: TeamPlan, harnesses: HarnessEntry[] | null): boolean {
  const errors = teamErrors(team, harnesses);
  return !errors.master && !errors.range && errors.rows.every((row) => row === undefined);
}

/** Tells whether a step's answers are complete enough to show its check mark.
 * Inputs: the step, the draft and existing project names. Output: true when complete. */
export function stepComplete(step: StepId, draft: ProjectDraft, existing: string[], harnesses: HarnessEntry[] | null = null): boolean {
  switch (step) {
    case "basics": return Object.keys(basicsErrors(draft, existing)).length === 0;
    case "documents": return true;
    case "work": return true;
    case "pool": return true;
    case "team": return draft.team !== null && teamValid(draft.team, harnesses);
  }
}

/** Builds the engine request from a finished draft.
 * Input: the draft. Output: the project spec and setup; the "Other" platform text joins the description. */
export function buildRequest(draft: ProjectDraft): { spec: ProjectSpec; setup: ProjectSetup } {
  const other = draft.platforms.includes("other") ? draft.otherPlatform.trim() : "";
  const about = other ? `${draft.about.trim()}${draft.about.trim() ? "\n\n" : ""}Other platform: ${other}` : draft.about.trim();
  return {
    spec: { name: draft.name.trim(), repositories: [] },
    setup: {
      about,
      tech_stack: [...draft.stack],
      platforms: [...draft.platforms],
      research: [...draft.research],
      pool: draft.pool,
      team: draft.team && wireTeam(draft.team),
      documents: draft.documents.map((document) => document.path),
    },
  };
}

/** Starts a Project Settings, General draft from the saved project.
 * Input: the saved setup. Output: a draft with its details, research, pool and team. */
export function draftOf(stored: StoredSetup): ProjectDraft {
  return {
    name: stored.name, about: stored.about, stack: [...stored.tech_stack], platforms: [...stored.platforms], otherPlatform: "", documents: [],
    research: [...stored.research], pool: stored.pool, poolChosen: true, team: stored.team,
  };
}

/** Builds the engine change for an edited project the way New Project builds a new one, keeping
 * the saved pool and team and adding no documents. Inputs: the saved setup and the edited draft.
 * Output: the new name or null when it is unchanged, the setup, and whether anything differs. */
export function editRequest(stored: StoredSetup, draft: ProjectDraft): { name: string | null; setup: ProjectSetup; changed: boolean } {
  const built = buildRequest({ ...draft, documents: [], pool: stored.pool, team: stored.team });
  const name = built.spec.name === stored.name ? null : built.spec.name;
  const same = <T,>(left: T[], right: T[]) => left.length === right.length && left.every((item, index) => item === right[index]);
  const changed = name !== null || built.setup.about !== stored.about || !same(built.setup.tech_stack, stored.tech_stack)
    || !same(built.setup.platforms, stored.platforms) || !same(built.setup.research, stored.research);
  return { name, setup: built.setup, changed };
}

/** The name the owner gave a pool, which the engine stores beside its id; older pools have
 * only an id. Input: the pool record. Output: the name to show. */
export function poolName(pool: AccountPool & { name?: string }): string {
  return pool.name?.trim() || pool.id;
}

/** Writes the line above the footer that says what Create project will do.
 * Inputs: the draft and the chosen pool's name, or null. Output: one sentence. */
export function summary(draft: ProjectDraft, pool: string | null): string {
  const name = draft.name.trim() || "this project";
  if (!pool) return `Creating ${name} saves it. Research starts when you choose a pool.`;
  const count = draft.research.length;
  if (count === 0) return `Creating ${name} saves it. No research is picked.`;
  return `Creating ${name} starts ${count} research ${count === 1 ? "agent" : "agents"} on ${pool}.`;
}

function accountLabel(connection: ModelConnection): string {
  if (connection.auth.method === "native_managed") return connection.harness === "claude" ? "Claude" : "ChatGPT";
  if (connection.auth.method === "api_key" || connection.auth.method === "oauth") return "API key";
  if (connection.endpoint.kind === "http" && /^http:\/\/(127\.0\.0\.1|localhost|\[::1\])(:|\/)/u.test(connection.endpoint.url)) return "local model";
  return "URL";
}

/** Summarises a pool's accounts by kind, for example "2 ChatGPT, 1 Claude, 1 API key".
 * Input: the member connections. Output: the summary. */
export function poolSummary(members: ModelConnection[]): string {
  if (members.length === 0) return "No accounts";
  const counts = new Map<string, number>();
  for (const member of members) counts.set(accountLabel(member), (counts.get(accountLabel(member)) ?? 0) + 1);
  return [...counts].map(([label, count]) => `${count} ${label}`).join(", ");
}

/** Counts a pool's accounts that can work now: signed in, or set up with a key or URL.
 * Input: the pool's summary. Output: the count. */
export function readyCount(pool: PoolSummary): number {
  return pool.members.filter((member) => member.status !== "not_signed_in").length;
}

/** Picks the pool to preselect: the one with the most ready accounts when the summaries are
 * known, else the one with the most accounts; the first wins a tie.
 * Inputs: the pools and their summaries, if known. Output: its id, or null when there are none. */
export function preselectPool(pools: PoolChoice[], summaries: PoolSummary[] = []): string | null {
  const score = (choice: PoolChoice) => {
    const summary = summaries.find((item) => item.id === choice.pool.id);
    return summaries.length ? (summary ? readyCount(summary) : 0) : choice.pool.members.length;
  };
  let best: PoolChoice | undefined;
  for (const choice of pools) if (!best || score(choice) > score(best)) best = choice;
  return best?.pool.id ?? null;
}

/** Tells whether a pool holds an account a harness can use. A connection names its harness by
 * the catalog id or by one of the entry's aliases from the engine, such as "claude" for Claude Code.
 * Inputs: the harness and the pool's connections. Output: true when one fits. */
export function poolFits(harness: HarnessEntry, members: ModelConnection[]): boolean {
  const names = [harness.id, ...harness.aliases];
  return members.some((member) => {
    const same = names.includes(member.harness);
    if (member.auth.method === "native_managed") return same && harness.account_kinds.includes("subscription");
    if (member.auth.method === "api_key" || member.auth.method === "oauth") return same || harness.account_kinds.includes("api_key");
    return same || harness.account_kinds.includes("open_ai_compatible_url") || harness.account_kinds.includes("local");
  });
}

export function harnessLabel(harnesses: HarnessEntry[] | null, id: string): string {
  return harnessName(harnesses, id);
}
