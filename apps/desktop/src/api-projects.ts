import { call, classifyFailure, knownError, namedFailure, plainError, type Result } from "./api";
import {
  allMasterTurns, closeMaster, editModelAccess, masterSessions, masterState, modelAccessPage,
  newMasterSession, openMaster, openProject, projects, resumeMasterSession, submitMaster,
} from "./engine-client";
import type {
  AccountPool, IntentReceipt, MasterSessions, MasterState, MasterTurn, ModelAccessRecord, ModelConnection, ProjectDetail, ProjectPage, TeamStatus,
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
  revision: number;
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
  pending_intent: "Another change isn't confirmed yet. Use Check again at the top of the window, then try again.",
  not_connected: "CrazyAgent's engine isn't running.",
  not_configured: "CrazyAgent's engine isn't running.",
  owner_unavailable: "CrazyAgent's engine isn't running.",
  busy: "Another change is still running. Wait for it to finish.",
  stale_revision: "This changed since you opened it. Review and save again.",
  stale_model_access: "This changed since you opened it. Review and save again.",
  invalid_project: "This project no longer exists.",
  research_job_missing: "This research job no longer exists. The list was reloaded.",
  research_job_finished: "This research has already stopped.",
  research_job_not_retryable: "Only failed or stopped research can run again.",
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
  team_harness_unavailable: "A harness in this team can't run team agents yet. Pick another harness.",
  team_harnesses_too_many: "Use at most 8 agent harnesses.",
  team_agent_count_invalid: "Each harness runs 1 to 15 agents.",
  team_agent_range_invalid: "At most must be at least 1 and not less than At least.",
  team_task_missing: "That task is gone already.",
  team_engine_not_ready: "The engine is still starting. Try again in a moment.",
  team_account_left_pool: "That account left the pool. Another one takes over.",
  team_harness_cannot_run: "This harness cannot run yet.",
  team_has_no_sub_agents: "This team has no sub-agents to hand work to.",
  team_harness_not_in_plan: "That harness is no longer part of this team.",
  team_task_invalid: "The task is empty or too long.",
  team_queue_full: "Too much work is waiting already. Try again when some finishes.",
  team_task_limit: "This project has reached its task limit.",
  team_conversation_full: "Too many of your messages are waiting for the master. Send more once it has answered.",
  team_message_invalid: "The message is empty or too long.",
  team_plan_step_missing: "That step is no longer in the master's plan.",
  team_plan_step_handed: "That step was handed off already.",
  team_handle_invalid: "Use lower-case letters, digits and single hyphens, up to 32 characters, such as onboarding-fix.",
  team_handle_taken: "Another running agent of this project already has that name. Pick another.",
  team_agent_unknown: "That agent is not running anymore.",
  team_state_unavailable: "The engine cannot read the team safely right now.",
  team_state_not_canonical: "The engine cannot read the team safely right now.",
  team_state_not_private: "The engine cannot read the team safely right now.",
  research_report_missing: "This report hasn't been written yet.",
  research_section_missing: "That section isn't in the report anymore.",
  research_section_invalid: "That section can't be saved. Pick another heading.",
  memory_text_hidden: "This part has hidden or direction-changing characters, so it can't be saved to memory.",
  documents_empty: "Pick at least one file.",
  invalid_memory: "This memory no longer exists. The list was reloaded.",
  knowledge_entry_missing: "This memory no longer exists. The list was reloaded.",
  context_item_protected: "The brief can't be deleted.",
  knowledge_version_conflict: "This memory changed since you opened it. The list was reloaded.",
  context_item_missing: "This file isn't in the project folder anymore.",
  context_item_unreadable: "This file can't be read.",
  context_path_invalid: "This file can't be opened here.",
  master_open_unavailable: "Pick a pool to start. Choose one in this project's Settings, Pool.",
  master_project_mismatch: "The conversation opened for another project. Open this project again.",
  master_outcome_pending: "We couldn't confirm your last change. Check it and save again.",
  master_source_conflict: "Settings changed since the master started. End the conversation and start it again.",
  master_session_active: "This project's conversation is already open. Switch tabs and back, then send again.",
  master_session_capacity: "Too many master conversations are open. End one in another project, then try again.",
  master_session_missing: "This conversation is gone. Send again to start a new one.",
  master_session_not_active: "This conversation has ended. Send again to start a new one.",
  master_turn_outstanding: "The master is still answering your last message. Wait for it or stop it, then send.",
  master_turn_running: "The master is still answering. Stop that reply to switch sessions.",
  master_session_live: "That session is already the live one.",
  master_session_closed: "That session ended with its project and can't be resumed.",
  master_turn_capacity: "This conversation is full. End it from the conversation menu, then send again.",
  master_turn_not_current: "That reply already finished.",
  master_input_too_large: "This conversation is too long for the master. End it from the conversation menu, then send again.",
  master_scope_or_budget_denied: "The master's limits aren't allowed by your settings. Check the project's pool.",
  master_pool_missing: "This project needs a pool before the master can reply.",
  master_backend_account_denied: "The project's pool has no account the master can use. Check the pool's accounts.",
  rcli_profile_identity_changed: "The local model's profile changed while the master started. Try again.",
  stale_master: "The conversation changed. It was reloaded; send again.",
  resync_required: "The conversation changed while it loaded. Try again.",
  conversation_text_bounds: "Messages can be up to 8192 bytes.",
  unavailable: "The engine couldn't do this right now. Try again.",
  rejected: "The engine refused this change.",
};

/** Plain sentence for one engine failure, never opaque.
 * Input: the code, or a failure with the engine cause behind it. Output: what happened and
 * what to do, or the generic fallback naming the code. */
export function failureCopy(failure: string | { code: string; cause?: string }): string {
  const { code, cause } = typeof failure === "string" ? { code: failure, cause: undefined } : failure;
  return namedFailure(failureSentence(code), code, cause);
}

/** The sentence the project screens have for a code. Input: the code. Output: the sentence,
 * or undefined when there is none. */
export function failureSentence(code: string): string | undefined {
  return Object.hasOwn(copy, code) ? copy[code] : knownError(code);
}

/** Says whether the saved pools give a project exactly the chosen pool, or none.
 * Inputs: the saved pools, the project id and the chosen pool id (null for none).
 * Output: true when only the chosen pool lists the project. */
export function projectPoolIs(pools: readonly AccountPool[], projectId: string, poolId: string | null): boolean {
  const holding = pools.filter((pool) => pool.projects.includes(projectId)).map((pool) => pool.id);
  return poolId === null ? holding.length === 0 : holding.length === 1 && holding[0] === poolId;
}

export type ReceiptState ={ kind: "applied" } | { kind: "rejected"; code: string } | { kind: "unconfirmed" };

/** Classifies an intent receipt for the screens.
 * Input: the receipt. Output: applied, rejected with a code, or unconfirmed while pending. */
export function receiptState(receipt: IntentReceipt): ReceiptState {
  if (receipt.pending || receipt.outcome === "uncertain" || receipt.outcome === "unknown" || receipt.outcome === "not_recorded_at_barrier") {
    return { kind: "unconfirmed" };
  }
  if (receipt.rejection) return { kind: "rejected", code: receipt.rejection };
  if (receipt.outcome === "applied") return { kind: "applied" };
  return { kind: "rejected", code: receipt.outcome };
}

export function loadProjectCatalog(pool: string | null, research: ResearchKind[]): Promise<Result<ProjectCatalog>> {
  return call<ProjectCatalog>("desktop_project_catalog", { pool, research });
}

export function setupProject(spec: ProjectSpec, setup: ProjectSetup, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_setup_project", { spec, setup, generation });
}

/** Whether a New Project receipt gets one automatic reconcile, as the TUI does: the engine
 * answered OutcomeUnknown or the reply was lost, and the bridge still keeps the intent.
 * Input: the receipt from Create project. Output: true when it should reconcile once. */
export function needsReconcile(receipt: IntentReceipt): boolean {
  return receipt.pending && receipt.rejection === null && (receipt.outcome === "unknown" || receipt.outcome === "uncertain");
}

/** The New Project receipt after one reconcile: the reconcile's answer when it settled the
 * same kept intent (kind, operation id and project), otherwise the unconfirmed one sent.
 * Inputs: the sent receipt and the reconcile result. Output: the receipt to show. */
export function settledReceipt(sent: IntentReceipt, settled: Result<IntentReceipt>): IntentReceipt {
  if (settled.kind !== "ok") return sent;
  const same = settled.value.kind === sent.kind && settled.value.operation_id === sent.operation_id && settled.value.project_id === sent.project_id;
  return same ? settled.value : sent;
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

export type { TeamAgent, TeamAgentStatus, TeamStatus, TeamTask, TeamTaskStatus } from "./engine-client";

/** Reads the project's live team: its agents and the tasks the master or owner handed out.
 * Input: the project id. Output: the team status. */
export function loadTeamStatus(id: string): Promise<Result<TeamStatus>> {
  return call<TeamStatus>("desktop_team_status", { id });
}

/** Stops a running team task or cancels a waiting one; with no task id it does this for every
 * team task of the project at once. Inputs: the project id, the task id or null and a generation.
 * Output: the engine receipt. */
export function cancelTeamTask(id: string, task: number | null, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_cancel", { id, task, generation });
}

/** Renames a live sub-agent's role handle, the `@name` it is addressed by.
 * Inputs: the project id, the team agent id, the new handle and a generation. Output: the engine receipt. */
export function renameTeamAgent(id: string, agent: number, handle: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_team_rename", { id, agent, handle, generation });
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

/** Stops one research job: a running agent stops and a waiting one never starts.
 * Inputs: the project id, the job id and a generation. Output: the engine receipt. */
export function cancelResearch(id: string, job: number, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_cancel_research", { id, job, generation });
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

/** Makes one of the project's memories app-wide with one owner click; the engine keeps a
 * suggested one first. Inputs: project id, memory id, its version and a generation. Output: the receipt. */
export function promoteProjectMemory(id: string, memory: string, version: number, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_promote_memory", { project: id, memory, version, generation });
}

/** Reads the extra read-only folders this project's agents get beside its toolchains.
 * Input: project id. Output: the folders in saved order. */
export function loadReadPaths(id: string): Promise<Result<string[]>> {
  return call<string[]>("desktop_project_read_paths", { id });
}

/** Replaces this project's extra read-only folders; the engine refuses unsafe ones.
 * Inputs: project id, the folders and a generation. Output: the receipt. */
export function saveReadPaths(id: string, paths: string[], generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_set_project_read_paths", { id, paths, generation });
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

/** Changes a project's details, research, pool and team in one engine change. Inputs: project id,
 * the whole new setup (no documents), a generation, the new name or null to keep it, and the
 * setup revision the screen showed, so a change made elsewhere is refused. Output: the receipt. */
export function updateProjectSetup(id: string, setup: ProjectSetup, generation: number, name: string | null, revision: number | null): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_update_project_setup", { id, setup, name, revision, generation });
}

/** Removes a project from CrazyAgent; its research stops and its folder stays on disk.
 * Inputs: project id and a generation. Output: the receipt. */
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

/** Names the projects a saved pool serves, for the delete confirm.
 * Input: the pool id. Output: the project names in engine order; one not listed reads "a project". */
export function projectsUsing(poolId: string): Promise<Result<string[]>> {
  return wrap(async () => {
    const record = (await allRecords("pools")).find((item) => item.kind === "pool" && item.pool.id === poolId);
    const ids = record?.kind === "pool" ? record.pool.projects : [];
    if (ids.length === 0) return [];
    let page = await projects(false);
    const names = new Map(page.items.map((item) => [item.id, item.name]));
    for (let count = 1; page.has_more && count < PAGES_MAX; count += 1) {
      page = await projects(true);
      for (const item of page.items) names.set(item.id, item.name);
    }
    return ids.map((id) => names.get(id) ?? "a project");
  });
}

/** Gives a project exactly one saved pool, or none, in one engine change, then reads the pools
 * back. Inputs: the project id, the pool id (null for none) and a generation. Output: applied,
 * or why not. */
export async function setProjectPool(projectId: string, poolId: string | null, generation: number): Promise<Result<ReceiptState>> {
  return wrap(async () => {
    const opened = await openProject(projectId);
    if (!opened.fresh || opened.project_id !== projectId) throw { code: "invalid_project" };
    const page = await modelAccessPage("pools", false);
    if (!page.writable) throw { code: "settings_read_only" };
    const state = receiptState(await editModelAccess(page.ticket, { kind: "assign_project_pool", project: projectId, pool: poolId }, generation));
    if (state.kind !== "applied") return state;
    const saved = (await allRecords("pools")).flatMap((record) => record.kind === "pool" ? [record.pool] : []);
    return projectPoolIs(saved, projectId, poolId) ? state : { kind: "unconfirmed" } as ReceiptState;
  });
}

export function openMasterProject(id: string): Promise<Result<ProjectDetail>> {
  return wrap(() => openProject(id));
}

export function readMaster(ticket: string): Promise<Result<MasterState>> {
  return wrap(() => masterState(ticket));
}

export function readMasterTurns(ticket: string, session: string): Promise<Result<MasterTurn[]>> {
  return wrap(() => allMasterTurns(ticket, session));
}

export function startMaster(ticket: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => openMaster(ticket, generation));
}

export function endMaster(ticket: string, session: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => closeMaster(ticket, session, generation));
}

/** Lists the selected project's master sessions. Input: the master ticket. Output: the list. */
export function readSessions(ticket: string): Promise<Result<MasterSessions>> {
  return wrap(() => masterSessions(ticket));
}

/** Parks the live session and opens a fresh one. Inputs: the master ticket, whether the owner
 * confirmed stopping a running reply, and a draft generation. Output: the receipt. */
export function startNewSession(ticket: string, cancelRunning: boolean, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => newMasterSession(ticket, cancelRunning, generation));
}

/** Makes a parked session live again. Inputs: the master ticket, the session id, whether the
 * owner confirmed stopping a running reply, and a draft generation. Output: the receipt. */
export function resumeSession(ticket: string, session: string, cancelRunning: boolean, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => resumeMasterSession(ticket, session, cancelRunning, generation));
}

/** Asks the engine to stop the master's reply in progress.
 * Inputs: the master ticket, session id, turn id and a draft generation. Output: the receipt. */
export function cancelMasterTurn(ticket: string, session: string, turn: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_cancel_master_turn", { ticket, session, turn, generation });
}

export function sendToMaster(ticket: string, session: string, text: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => submitMaster(ticket, session, text, generation));
}
