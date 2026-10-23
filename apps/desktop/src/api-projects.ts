import { call, classifyFailure, plainError, type Result } from "./api";
import {
  closeMaster, editModelAccess, inspectModelAccess, masterState, masterTurns, modelAccessPage,
  openMaster, openProject, projects, submitMaster,
} from "./engine-client";
import type {
  AccountPool, IntentReceipt, MasterState, MasterTurns, ModelAccessRecord, ModelConnection, ProjectDetail, ProjectPage,
} from "./engine-client";

export type Platform = "website" | "mobile_app" | "desktop_app" | "cli" | "api_backend" | "other";
export type ResearchKind = "market_research" | "competitor_research" | "design_research" | "front_end_design" | "tech_stack_research";
export type ResearchStatus = "queued" | "needs_pool" | "running" | "done" | "failed" | "cancelled";

export interface HarnessSlot {
  harness: string;
  agents: number;
  tools: boolean;
}

export interface TeamPlan {
  master: string;
  master_tools: boolean;
  sub_harnesses: HarnessSlot[];
  min_agents: number;
  max_agents: number;
}

export interface ProjectSpec {
  name: string;
  repositories: string[];
}

export interface ProjectSetup {
  about: string;
  tech_stack: string[];
  platforms: Platform[];
  research: ResearchKind[];
  pool: string | null;
  team: TeamPlan | null;
  documents: string[];
}

export interface TechOption {
  id: string;
  label: string;
  group: "language" | "framework" | "database";
}

export interface ProjectCatalog {
  tech_stack: TechOption[];
  platforms: { kind: Platform; label: string }[];
  research: { kind: ResearchKind; label: string }[];
  document_extensions: string[];
  documents_max: number;
  document_bytes_max: number;
  project_document_bytes_max: number;
  default_team: TeamPlan;
}

export interface StoredSetup {
  project_id: string;
  name: string;
  about: string;
  tech_stack: string[];
  platforms: Platform[];
  research: ResearchKind[];
  pool: string | null;
  team: TeamPlan;
  documents: { name: string; bytes: number }[];
  folder: string;
  created_ms: number;
}

export interface ResearchJob {
  id: number;
  project_id: string;
  kind: ResearchKind;
  kind_label: string;
  status: ResearchStatus;
  status_label: string;
  report_path: string;
  harness: string;
  tools: boolean;
  attempts: number;
  claimer: string | null;
  error: string | null;
  cancel_requested: boolean;
  output_bytes: number;
  created_ms: number;
  started_ms: number | null;
  heartbeat_ms: number | null;
  finished_ms: number | null;
}

export interface ContextItem {
  path: string;
  kind: "brief" | "document" | "research";
  bytes: number;
}

export interface ProjectContext {
  project_id: string;
  folder: string;
  items: ContextItem[];
}

export interface ContextChunk {
  path: string;
  bytes: number;
  offset: number;
  text: string | null;
  next_offset: number | null;
}

export interface PoolChoice {
  pool: AccountPool;
  members: ModelConnection[];
}

async function wrap<T>(action: () => Promise<T>): Promise<Result<T>> {
  try {
    return { kind: "ok", value: await action() };
  } catch (failure) {
    return classifyFailure(failure);
  }
}

const copy: Record<string, string> = {
  pending_intent: "Another change is still being saved. Wait for it to finish, then try again.",
  owner_not_ready: "The engine isn't ready for changes yet. Try again in a moment.",
  owner_read_failed: "Couldn't read this from the engine. Try again.",
  not_connected: "CrazyAgent's engine isn't running.",
  not_configured: "CrazyAgent's engine isn't running.",
  owner_unavailable: "CrazyAgent's engine isn't running.",
  busy: "Another change is still being saved. Wait for it to finish, then try again.",
  stale_revision: "This changed since you opened it. Review and save again.",
  stale_model_access: "This changed since you opened it. Review and save again.",
  invalid_project: "This project no longer exists.",
  invalid_pool: "That pool no longer exists. Choose another pool.",
  pool_missing: "That pool no longer exists. Choose another pool.",
  project_spec_bounds: "Enter a project name of at most 80 characters.",
  project_setup_bounds: "Some answers are too long or repeated. Check each step, then try again.",
  project_about_invalid: "The description is too long or has characters that can't be saved.",
  tech_stack_too_long: "Pick at most 32 tech stack items.",
  tech_stack_entry_invalid: "Each tech stack item must be unique and at most 40 characters.",
  platforms_duplicate: "Each target platform can be picked once.",
  research_duplicate: "Each kind of research can be picked once.",
  documents_too_many: "Attach at most 16 documents.",
  document_path_invalid: "One of the documents has a path that can't be used. Choose it again.",
  document_not_found: "One of the documents was moved or deleted. Choose it again.",
  document_not_a_regular_file: "Can't add a folder or a shortcut. Choose the original file.",
  document_unreadable: "One of the documents can't be read. Check its permissions.",
  document_changed_while_copying: "A document changed while it was being copied. Try again.",
  document_name_duplicate: "Two documents have the same name. Rename one first.",
  document_name_invalid: "One of the documents has a name that can't be used. Rename it first.",
  document_too_large: "Files can be up to 50 MB.",
  document_type_not_allowed: "This file type isn't supported.",
  project_documents_too_large: "This project's documents would be over 200 MB. Remove some files first.",
  project_exists: "This project already exists.",
  project_folder_exists: "A folder for this project already exists. Try again.",
  project_folder_unwritable: "CrazyAgent can't write the project folder. Check the disk and try again.",
  projects_root_not_a_directory: "CrazyAgent can't write the project folder. Check the disk and try again.",
  project_setup_missing: "This project was created before project setup existed.",
  team_harness_invalid: "Pick a harness for the master and for each agent row.",
  team_harnesses_too_many: "Use at most 8 agent harnesses.",
  team_agent_count_invalid: "Each harness runs 1 to 15 agents.",
  team_agent_range_invalid: "At most must be at least 1 and not less than At least.",
  research_report_missing: "This report hasn't been written yet.",
  research_section_missing: "That section isn't in the report anymore.",
  research_section_invalid: "That section can't be saved. Pick another heading.",
  documents_empty: "Pick at least one file.",
  invalid_memory: "This memory no longer exists. The list was reloaded.",
  knowledge_entry_missing: "This memory no longer exists. The list was reloaded.",
  context_item_protected: "The brief can't be deleted.",
  knowledge_version_conflict: "This memory changed since you opened it. The list was reloaded.",
  context_item_missing: "This file isn't in the project folder anymore.",
  context_item_unreadable: "This file can't be read.",
  context_path_invalid: "This file can't be opened here.",
  master_open_unavailable: "This project needs a pool before the master can reply.",
  master_outcome_pending: "We couldn't confirm your last change. Check it and save again.",
  master_source_conflict: "Settings changed since the master started. End the conversation and start it again.",
  stale_master: "The conversation changed. It was reloaded; send again.",
  conversation_text_bounds: "Messages can be up to 8192 bytes.",
  unavailable: "The engine couldn't do this right now. Try again.",
  rejected: "The engine refused this change.",
};

/** Plain sentence for one engine failure code.
 * Input: the code. Output: what happened and what to do, or the generic fallback. */
export function failureCopy(code: string): string {
  return copy[code] ?? plainError(code);
}

/** Adds a project and the master role to a pool without changing anything else.
 * Inputs: the saved pool and the project id. Output: the new pool record. */
export function assignedPool(pool: AccountPool, projectId: string): AccountPool {
  const projects = pool.projects.includes(projectId) ? [...pool.projects] : [...pool.projects, projectId];
  const roles = pool.roles.includes("project_leader") ? [...pool.roles] : [...pool.roles, "project_leader"];
  if (projects.length > 64 || roles.length > 64) throw new Error("A pool allows at most 64 project and 64 role assignments.");
  return { ...pool, projects, roles };
}

/** Removes one project from a pool's assignments and keeps everything else.
 * Inputs: the saved pool and the project id. Output: the new pool record. */
export function unassignedPool(pool: AccountPool, projectId: string): AccountPool {
  return pool.projects.includes(projectId) ? { ...pool, projects: pool.projects.filter((id) => id !== projectId) } : pool;
}

export type ReceiptState ={ kind: "applied" } | { kind: "rejected"; code: string } | { kind: "unconfirmed" };

/** Classifies an intent receipt for the screens.
 * Input: the receipt. Output: applied, rejected with a code, or unconfirmed while pending. */
export function receiptState(receipt: IntentReceipt): ReceiptState {
  if (receipt.rejection) return { kind: "rejected", code: receipt.rejection };
  if (receipt.pending || receipt.outcome === "uncertain" || receipt.outcome === "unknown" || receipt.outcome === "not_recorded_at_barrier") {
    return { kind: "unconfirmed" };
  }
  if (receipt.outcome === "applied") return { kind: "applied" };
  return { kind: "rejected", code: receipt.outcome };
}

export function loadProjectCatalog(pool: string | null, research: ResearchKind[]): Promise<Result<ProjectCatalog>> {
  return call<ProjectCatalog>("desktop_project_catalog", { pool, research });
}

export function setupProject(spec: ProjectSpec, setup: ProjectSetup, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_setup_project", { spec, setup, generation });
}

export function loadSetup(id: string): Promise<Result<StoredSetup | null>> {
  return call<StoredSetup | null>("desktop_project_setup", { id });
}

export function loadResearch(id: string): Promise<Result<{ project_id: string; jobs: ResearchJob[] }>> {
  return call<{ project_id: string; jobs: ResearchJob[] }>("desktop_research_jobs", { id });
}

export function loadContext(id: string): Promise<Result<ProjectContext>> {
  return call<ProjectContext>("desktop_project_context", { id });
}

export const CONTEXT_READ_BYTES_MAX = 1024 * 1024;

/** Reads one context file whole, chunk by chunk, up to the report size limit.
 * Inputs: the project id and the item path. Output: the text, or null for a binary file. */
export async function loadContextText(id: string, path: string): Promise<Result<{ text: string | null; truncated: boolean }>> {
  let offset = 0;
  let text = "";
  for (let chunks = 0; chunks < 256; chunks += 1) {
    const chunk = await call<ContextChunk>("desktop_project_context_item", { id, path, offset });
    if (chunk.kind !== "ok") return chunk;
    if (chunk.value.text === null) return { kind: "ok", value: { text: null, truncated: false } };
    text += chunk.value.text;
    if (chunk.value.next_offset === null) return { kind: "ok", value: { text, truncated: false } };
    if (chunk.value.next_offset <= offset || chunk.value.next_offset > CONTEXT_READ_BYTES_MAX) {
      return { kind: "ok", value: { text, truncated: true } };
    }
    offset = chunk.value.next_offset;
  }
  return { kind: "ok", value: { text, truncated: true } };
}

export function saveResearchToMemory(id: string, research: ResearchKind, section: string | null, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_save_research_to_memory", { id, research, section, generation });
}

export const RESEARCH_LOG_LINES_MAX = 200;

/** Reads the latest plain lines of one research agent's log.
 * Inputs: the project id and the job id. Output: at most RESEARCH_LOG_LINES_MAX lines, oldest first. */
export async function loadResearchLog(id: string, job: number): Promise<Result<string[]>> {
  const result = await call<{ lines: string[] }>("desktop_research_log", { id, job });
  if (result.kind !== "ok") return result;
  if (!Array.isArray(result.value?.lines) || !result.value.lines.every((line) => typeof line === "string")) return { kind: "error", code: "invalid_response", message: plainError("invalid_response") };
  return { kind: "ok", value: result.value.lines.slice(-RESEARCH_LOG_LINES_MAX) };
}

/** Asks the engine to run one failed research job again.
 * Inputs: the project id, the job id and a draft generation. Output: the engine receipt. */
export function retryResearch(id: string, job: number, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_retry_research", { id, job, generation });
}

export type MemoryStatus = "Provisional" | "Confirmed" | "Rejected" | "Removed" | "Retracted";

export interface MemoryItem {
  id: string;
  version: number;
  text: string;
  status: MemoryStatus;
  human_reviewed: boolean;
}

export type MemoryDecision = { decision: "Keep" } | { decision: "Reject" } | { decision: "Remove" };

export const MEMORY_PAGES_MAX = 20;

/** Reads a project's memories, research saved for approval included, page by page.
 * Inputs: the project id and the last memory id already shown, or null.
 * Output: one engine batch of memories. */
export function loadProjectMemories(id: string, after: string | null): Promise<Result<MemoryItem[]>> {
  return call<MemoryItem[]>("desktop_project_memories", { id, after });
}

/** Saves, dismisses or deletes one memory; Keep is the owner's one-click approval.
 * Inputs: project id, memory id, the version shown, the decision and a generation.
 * Output: the engine receipt. */
export function reviewMemory(id: string, memory: string, version: number, decision: MemoryDecision, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_review_memory", { id, memory, version, decision, generation });
}

/** Deletes one document or research report from the project folder.
 * Inputs: project id, the path inside the folder and a generation. Output: the receipt. */
export function deleteContextItem(id: string, path: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_delete_project_context", { id, path, generation });
}

/** Copies more documents into an existing project folder.
 * Inputs: project id, absolute file paths and a generation. Output: the receipt. */
export function addProjectDocuments(id: string, documents: string[], generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_add_project_documents", { id, documents, generation });
}

export function pickDocuments(): Promise<Result<string[]>> {
  return call<string[]>("desktop_pick_documents");
}

export function revealProjectFolder(id: string): Promise<Result<void>> {
  return call<void>("desktop_reveal_project_folder", { id });
}

export function openLink(url: string): Promise<Result<void>> {
  return call<void>("desktop_open_link", { url });
}

export function loadProjectSkills(id: string): Promise<Result<unknown>> {
  return call<unknown>("desktop_project_skills", { id });
}

export function loadProjectMcpServers(id: string): Promise<Result<unknown>> {
  return call<unknown>("desktop_project_mcp_servers", { id });
}

export function updateProjectSetup(id: string, setup: ProjectSetup, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_update_project_setup", { id, setup, generation });
}

export function removeProject(id: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_remove_project", { id, generation });
}

export const PAGES_MAX = 50;

async function allRecords(collection: "pools" | "connections"): Promise<ModelAccessRecord[]> {
  const records: ModelAccessRecord[] = [];
  let page = await modelAccessPage(collection, false);
  records.push(...page.items);
  for (let count = 1; page.has_more && count < PAGES_MAX; count += 1) {
    page = await modelAccessPage(collection, true);
    records.push(...page.items);
  }
  return records;
}

/** Reads every pool with its member connections.
 * Input: none. Output: pools in engine order, each with the connections it holds. */
export function loadPools(): Promise<Result<PoolChoice[]>> {
  return wrap(async () => {
    const pools = (await allRecords("pools")).flatMap((record) => record.kind === "pool" ? [record.pool] : []);
    const connections = (await allRecords("connections")).flatMap((record) => record.kind === "connection" ? [record.connection] : []);
    return pools.map((pool) => ({ pool, members: connections.filter((connection) => pool.members.includes(connection.id)) }));
  });
}

/** Reads every project name the engine holds.
 * Input: none. Output: the projects, across every page. */
export function loadProjects(): Promise<Result<ProjectPage["items"]>> {
  return wrap(async () => {
    let page = await projects(false);
    const items = [...page.items];
    for (let count = 1; page.has_more && count < PAGES_MAX; count += 1) {
      page = await projects(true);
      items.push(...page.items);
    }
    return items;
  });
}

/** Assigns a saved pool to a project as its master pool, or removes that assignment,
 * then reads the saved record back. Inputs: the project id, the pool id, a generation and
 * whether to remove. Output: applied, or why not. */
export async function assignPool(projectId: string, poolId: string, generation: number, remove = false): Promise<Result<ReceiptState>> {
  return wrap(async () => {
    const opened = await openProject(projectId);
    if (!opened.fresh || opened.project_id !== projectId) throw { code: "invalid_project" };
    const basis = await inspectModelAccess("pools", poolId);
    if (basis.record.kind !== "pool" || !basis.writable) throw { code: "invalid_pool" };
    const pool = remove ? unassignedPool(basis.record.pool, projectId) : assignedPool(basis.record.pool, projectId);
    if (JSON.stringify(pool) === JSON.stringify(basis.record.pool)) return { kind: "applied" } as ReceiptState;
    const state = receiptState(await editModelAccess(basis.ticket, { kind: "replace_pool", pool }, generation));
    if (state.kind !== "applied") return state;
    const saved = await inspectModelAccess("pools", poolId);
    if (saved.record.kind !== "pool" || JSON.stringify(saved.record.pool) !== JSON.stringify(pool)) return { kind: "unconfirmed" } as ReceiptState;
    return state;
  });
}

export function openMasterProject(id: string): Promise<Result<ProjectDetail>> {
  return wrap(() => openProject(id));
}

export function readMaster(ticket: string): Promise<Result<MasterState>> {
  return wrap(() => masterState(ticket));
}

export function readMasterTurns(ticket: string, session: string): Promise<Result<MasterTurns>> {
  return wrap(() => masterTurns(ticket, session));
}

export function startMaster(ticket: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => openMaster(ticket, generation));
}

export function endMaster(ticket: string, session: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => closeMaster(ticket, session, generation));
}

export function sendToMaster(ticket: string, session: string, text: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => submitMaster(ticket, session, text, generation));
}
