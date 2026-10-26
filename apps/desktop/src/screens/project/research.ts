import type { IconName, Tone } from "../../ui";
import { failureCopy, receiptState } from "../../api-projects";
import type { ContextItem, MemoryItem, ResearchJob, TeamAgentStatus, TeamStatus, TeamTaskStatus } from "../../api-projects";
import type { HarnessEntry, PoolSummary, Result } from "../../api";
import type { IntentReceipt, MasterState, MasterTurn, ProjectDetail } from "../../engine-client";
import { formatBytes } from "../new-project/wizard";
import type { Loaded } from "../../state";


/** Formats a duration the way the design writes elapsed time.
 * Input: milliseconds. Output: for example "45 s", "18 min" or "2 h 5 min". */
export function elapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`;
}

/** Formats how long ago something happened.
 * Inputs: the event time and now, in milliseconds. Output: for example "12 min ago". */
export function ago(at: number, now: number): string {
  const minutes = Math.floor((now - at) / 60_000);
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "Yesterday" : `${days} days ago`;
}

const PROMPT_LINE_MAX = 140;

/** The first line of a task prompt, cut to fit one row. Input: the prompt. Output: the line. */
export function firstLine(text: string): string {
  const line = text.trim().split("\n", 1)[0].trim();
  return line.length > PROMPT_LINE_MAX ? `${line.slice(0, PROMPT_LINE_MAX - 1)}...` : line;
}

/** Maps a team agent's engine status to the design's status word. Input: the status. Output: the word. */
export function teamAgentWord(status: TeamAgentStatus): string {
  switch (status) {
    case "starting": return "Starting";
    case "idle": return "Ready";
    case "working": return "Running";
    case "stopped": return "Stopped";
    case "failed": return "Failed";
  }
}

/** Maps a team task's engine status to the design's status word. Input: the status. Output: the word. */
export function teamTaskWord(status: TeamTaskStatus): string {
  switch (status) {
    case "queued": return "Queued";
    case "needs_pool": return "Needs a pool";
    case "running": return "Running";
    case "done": return "Done";
    case "failed": return "Failed";
    case "cancelled": return "Stopped";
  }
}

export type TeamFilter = "running" | "all";

export interface TeamAgentRow {
  key: string;
  role: "Master" | "Sub-agent";
  name: string;
  harness: string;
  account: string;
  word: string;
  task: string;
  folder: string;
}

export interface TeamTaskRow {
  key: string;
  from: string;
  to: string;
  word: string;
  prompt: string;
  summary: string;
  result: string | null;
  elapsed: string;
  stop: { task: number; label: "Stop" | "Cancel" } | null;
}

const WAITING: TeamTaskStatus[] = ["queued", "needs_pool"];
const SUMMARY_MAX = 200;
const MARKER = /^(?:#{1,6}|[-*+]|>|\d+\.)\s+/u;

/** Sums up a task result in one plain line: its first line that is not a heading, without list,
 * quote or heading marks, cut to fit a row. Input: the result text. Output: the line, or "". */
export function resultSummary(text: string): string {
  const lines = text.split("\n").map((line) => line.trim()).filter((line) => line !== "");
  const line = (lines.find((item) => !item.startsWith("#")) ?? lines[0] ?? "").replace(MARKER, "");
  return line.length > SUMMARY_MAX ? `${line.slice(0, SUMMARY_MAX - 1)}...` : line;
}

/** Builds the Team panel: each agent with its role, plain name, harness, account, current task and
 * the folder it works in (the master in the project folder, a sub-agent in its own worktree),
 * then the tasks handed from the master or the owner to a sub-agent. "running" hides stopped agents
 * and finished tasks. Sub-agents are named by harness and order, such as "Codex 2", never by id.
 * Inputs: the team status, the account names, a harness namer, the filter and now in milliseconds.
 * Output: the agent rows and task rows in engine order. */
export function teamPanel(status: Pick<TeamStatus, "agents" | "tasks">, accounts: { id: string; name: string }[], harnessLabel: (id: string) => string,
  filter: TeamFilter, now: number): { agents: TeamAgentRow[]; tasks: TeamTaskRow[] } {
  const names = new Map<number, string>();
  const counts = new Map<string, number>();
  for (const agent of status.agents) {
    if (agent.role === "master") { names.set(agent.id, "Master"); continue; }
    const count = (counts.get(agent.harness) ?? 0) + 1;
    counts.set(agent.harness, count);
    names.set(agent.id, `${harnessLabel(agent.harness)} ${count}`);
  }
  const account = (id: string | null) => id === null ? "No account yet" : accounts.find((item) => item.id === id)?.name ?? "An account in the pool";
  const agents = status.agents.filter((agent) => filter === "all" || (agent.status !== "stopped" && agent.status !== "failed")).map((agent): TeamAgentRow => {
    const held = status.tasks.find((item) => item.id === agent.task);
    return {
      key: `agent-${agent.id}`,
      role: agent.role === "master" ? "Master" : "Sub-agent",
      name: names.get(agent.id) ?? harnessLabel(agent.harness),
      harness: harnessLabel(agent.harness),
      account: account(agent.account),
      word: teamAgentWord(agent.status),
      task: agent.problem ?? (held ? firstLine(held.prompt) : agent.status === "idle" ? "Waiting for work" : ""),
      folder: agent.role === "master" ? "Works in the project folder" : agent.worktree ? `Works in ${agent.worktree}` : "",
    };
  });
  const tasks = status.tasks.filter((item) => filter === "all" || item.status === "running" || WAITING.includes(item.status)).map((item): TeamTaskRow => ({
    key: `task-${item.id}`,
    from: item.source === "owner" ? "From you" : "From the master",
    to: (item.agent !== null ? names.get(item.agent) : undefined) ?? harnessLabel(item.harness),
    word: teamTaskWord(item.status),
    prompt: firstLine(item.prompt),
    summary: item.problem ?? (item.result !== null ? resultSummary(item.result) : ""),
    result: item.result,
    elapsed: item.started_ms !== null ? elapsed((item.finished_ms ?? now) - item.started_ms) : "",
    stop: item.status === "running" ? { task: item.id, label: "Stop" } : WAITING.includes(item.status) ? { task: item.id, label: "Cancel" } : null,
  }));
  return { agents, tasks };
}

/** What one owner change on the project page tells the owner: a toast when the engine applied it,
 * otherwise a line in the panel. Inputs: the engine result, the toast and what is missing when
 * the command isn't in this build. Output: the toast, the panel line and whether to reload the list. */
export function changeOutcome(result: Result<IntentReceipt>, done: string, missing: string): { toast: string; note: string; reload: boolean } {
  if (result.kind === "not_ready") return { toast: "", note: `Not ready yet: ${missing} isn't in this build.`, reload: false };
  if (result.kind === "error") return { toast: "", note: failureCopy(result.code), reload: false };
  const state = receiptState(result.value);
  if (state.kind === "applied") return { toast: done, note: "", reload: true };
  return { toast: "", note: state.kind === "rejected" ? failureCopy(state.code) : "We couldn't confirm your last change. Check the list before trying again.", reload: true };
}

/** What Pause all agents will do to the team's tasks: running ones stop, waiting ones are
 * cancelled. Input: the team tasks. Output: how many it touches and the confirm line. */
export function pauseSummary(tasks: Pick<TeamStatus["tasks"][number], "status">[]): { count: number; line: string } {
  const running = tasks.filter((task) => task.status === "running").length;
  const waiting = tasks.filter((task) => WAITING.includes(task.status)).length;
  const parts = [
    running ? `${running} running ${running === 1 ? "task stops" : "tasks stop"}` : "",
    waiting ? `${waiting} waiting ${waiting === 1 ? "task is" : "tasks are"} cancelled` : "",
  ].filter((part) => part !== "");
  return { count: running + waiting, line: parts.length ? `${parts.join(" and ")}.` : "No team agents are working or waiting." };
}

/** The Needs you line for team tasks that wait for a pool account, with the engine's first
 * reason when it gave one. Input: the team tasks. Output: the line, or null when none wait. */
export function teamWaitingLine(tasks: Pick<TeamStatus["tasks"][number], "status" | "problem">[]): string | null {
  const waiting = tasks.filter((task) => task.status === "needs_pool");
  if (waiting.length === 0) return null;
  const reason = waiting.find((task) => task.problem)?.problem?.trim().replace(/\.$/u, "");
  const lead = waiting.length === 1 ? "A team task waits" : `${waiting.length} team tasks wait`;
  return reason ? `${lead}: ${reason}.` : `${lead} for an account in this project's pool.`;
}

/** The Overview fact for skills or MCP servers: how many are on in this project. An item is on
 * when it is on where it was added and this project has not switched it off.
 * Inputs: the engine list and the singular and plural words. Output: the line, or null until it loads. */
export function onFact(list: Loaded<{ on_here: boolean | null; enabled: boolean }[]>, one: string, many: string): string | null {
  if (list.kind !== "ok") return null;
  const count = list.value.filter((item) => item.enabled && item.on_here !== false).length;
  return count === 0 ? "None on" : `${count} ${count === 1 ? one : many} on`;
}

export interface ResearchRow {
  title: string;
  status: string;
  tone: Tone;
  progress: string;
  elapsed: string;
  report: boolean;
  active: boolean;
}

/** What the owner can do with one research agent: read its log once it started, stop it while it
 * runs, cancel it while it waits, and try again after it failed or was stopped.
 * Input: the job. Output: whether to offer the log, the stop label or null, and Try again. */
export function researchActions(job: Pick<ResearchJob, "status" | "started_ms" | "cancel_requested">): { log: boolean; stop: "Stop" | "Cancel" | null; retry: boolean } {
  const stop = job.cancel_requested ? null : job.status === "running" ? "Stop" : job.status === "queued" || job.status === "needs_pool" ? "Cancel" : null;
  return { log: job.started_ms !== null, stop, retry: job.status === "failed" || job.status === "cancelled" };
}

/** Builds one research row from a real job; progress text only states what the job reports.
 * Inputs: the job and now in milliseconds. Output: the row's words and flags. */
export function researchRow(job: ResearchJob, now: number): ResearchRow {
  const ran = job.started_ms === null ? "" : elapsed((job.finished_ms ?? now) - job.started_ms);
  const base = { title: job.kind_label, elapsed: "", report: false, active: false };
  switch (job.status) {
    case "queued": return { ...base, status: "Queued", tone: "neutral", progress: "Waiting to start", active: true };
    case "needs_pool": return { ...base, status: "Needs a pool", tone: "warn", progress: job.error?.trim() || "Waiting for a pool" };
    case "running": return {
      ...base, status: "Running", tone: "info", elapsed: ran, active: true,
      progress: job.cancel_requested ? "Stopping" : job.output_bytes > 0 ? `Writing report, ${formatBytes(job.output_bytes)} so far` : "Working",
    };
    case "done": return { ...base, status: "Done", tone: "ok", progress: "Report saved", elapsed: ran, report: true };
    case "failed": return { ...base, status: "Failed", tone: "danger", progress: job.error?.trim() || "Stopped with an error.", elapsed: ran };
    case "cancelled": return { ...base, status: "Stopped", tone: "neutral", progress: "Stopped before it finished", elapsed: ran };
  }
}

/** Tells whether research can still change on its own, so the page keeps polling. A job
 * waiting for a pool resumes once the engine finds a usable account.
 * Input: the jobs. Output: true while one is queued, running or waiting for a pool. */
export function shouldPoll(jobs: ResearchJob[]): boolean {
  return jobs.some((job) => job.status === "queued" || job.status === "running" || job.status === "needs_pool");
}

/** Builds the Needs you line for research that waits on its pool, using the engine's reason
 * when a waiting job carries one. Input: the jobs. Output: one sentence. */
export function waitingLine(jobs: ResearchJob[]): string {
  const reason = jobs.find((job) => job.status === "needs_pool" && job.error?.trim())?.error?.trim();
  if (!reason) return "Research is waiting for a pool.";
  return `Research is waiting: ${reason}${/[.!?]$/.test(reason) ? "" : "."}`;
}

/** Builds the Recent activity list from real job times.
 * Inputs: the jobs, the project name and when it was created. Output: every event, newest first. */
export function activity(jobs: ResearchJob[], project: string, created: number | null): { at: number; text: string }[] {
  const events: { at: number; text: string }[] = [];
  if (created !== null) events.push({ at: created, text: `You created ${project} with ${jobs.length} research ${jobs.length === 1 ? "task" : "tasks"}` });
  for (const job of jobs) {
    if (job.started_ms !== null) events.push({ at: job.started_ms, text: `${job.kind_label} started` });
    if (job.finished_ms === null) continue;
    const file = job.report_path.slice(job.report_path.lastIndexOf("/") + 1);
    if (job.status === "done") events.push({ at: job.finished_ms, text: `${job.kind_label} saved ${file}` });
    else if (job.status === "failed") events.push({ at: job.finished_ms, text: `${job.kind_label} stopped: ${job.error?.trim() || "error"}` });
    else if (job.status === "cancelled") events.push({ at: job.finished_ms, text: `${job.kind_label} was stopped` });
  }
  return events.sort((a, b) => b.at - a.at);
}

export const ACTIVITY_ROWS = 8;

const settled: MasterTurn["phase"][] = ["succeeded", "failed", "cancelled", "uncertain", "blocked"];

export function turnSettled(turn: MasterTurn): boolean {
  return settled.includes(turn.phase);
}

/** Names the master's state with the design's fixed words.
 * Inputs: the session or null and its turns. Output: Ready, Thinking..., Replied, Not signed in or Stopped. */
export function masterStatus(session: MasterState["session"], turns: MasterTurn[]): string {
  if (session && session.phase !== "configured") return "Stopped";
  const last = [...turns].sort((a, b) => a.ordinal - b.ordinal).at(-1);
  if (!last) return "Ready";
  if (!turnSettled(last)) return "Thinking...";
  if (last.phase === "succeeded") return "Replied";
  if (last.phase === "blocked" && (last.reason === "readiness_unknown" || last.reason === "readiness_stale")) return "Not signed in";
  return "Stopped";
}

/** One plain line for a master turn that has no reply yet or ended without one.
 * Input: the turn. Output: the line. */
export function turnLine(turn: MasterTurn): string {
  const reason = turn.reason ? turn.reason.replaceAll("_", " ") : "";
  if (turn.phase === "blocked" && turn.reason === "readiness_unknown") return "Not signed in. Sign in again in Settings, Accounts, then send again.";
  if (turn.phase === "blocked" && turn.reason === "readiness_stale") return "Sign-in is out of date. Sign in again in Settings, Accounts, then send again.";
  if (turn.phase === "blocked" && turn.reason === "connection_changed") return "Settings changed since the master started. End the conversation, then send again.";
  if ((turn.phase === "blocked" || turn.phase === "failed") && turn.reason === "repository_missing") return "The master couldn't start: this project has no repository yet.";
  if ((turn.phase === "blocked" || turn.phase === "failed") && turn.reason === "repository_config_refused") return `The master couldn't start: ${turn.detail ?? "a Git setting in this project's repository runs commands or redirects Git. Remove it, then send again."}`;
  switch (turn.phase) {
    case "queued": case "prepared": case "delivering": return "Waiting to start";
    case "running": return "Thinking...";
    case "waiting_approval": return "Waiting for your approval";
    case "succeeded": return "Replied";
    case "cancelled": return "Stopped";
    case "failed": case "blocked": case "uncertain": return `The master stopped${reason ? `: ${reason}` : ""}.`;
  }
}

/** The Thinking line for a master turn, with the time since this window sent the message.
 * Inputs: when it was sent in milliseconds, if this window sent it, and now. Output: the line. */
export function thinkingLine(sentAt: number | undefined, now: number): string {
  return sentAt === undefined ? "Thinking..." : `Thinking... ${elapsed(now - sentAt)}`;
}

const FOLLOW_SLACK_PX = 120;

/** Tells whether a scrolled list sits close enough to its bottom to keep following new lines.
 * Input: the element's scroll sizes. Output: true within FOLLOW_SLACK_PX of the bottom. */
export function nearBottom(box: { scrollHeight: number; scrollTop: number; clientHeight: number }): boolean {
  return box.scrollHeight - box.scrollTop - box.clientHeight <= FOLLOW_SLACK_PX;
}

/** The master ticket for the project this conversation shows.
 * Inputs: the bridge's answer to opening a project and the project shown.
 * Output: the ticket; throws `master_project_mismatch` when the bridge opened another project. */
export function openedTicket(opened: ProjectDetail, project: string): string {
  if (opened.project_id !== project) throw { code: "master_project_mismatch" };
  return opened.ticket;
}

/** Finds the master turn that is still replying, which Stop cancels.
 * Input: the turns. Output: the newest unsettled turn, or null. */
export function replyingTurn(turns: MasterTurn[]): MasterTurn | null {
  return turns.filter((turn) => !turnSettled(turn)).reduce<MasterTurn | null>((last, turn) => last === null || turn.ordinal > last.ordinal ? turn : last, null);
}

/** The Conversation line when the master's harness is not installed on this Mac.
 * Inputs: the harness catalog, or null while it is not ready, and the master's harness id.
 * Output: the line, or null when the harness is installed or unknown. */
export function masterMissingLine(harnesses: HarnessEntry[] | null, master: string): string | null {
  const entry = harnesses?.find((harness) => harness.id === master);
  return entry?.status.kind === "not_installed" ? `${entry.name} isn't installed on this Mac.` : null;
}

/** The Needs you line when accounts in the project's pool need to sign in again.
 * Input: the pool's summary, if known. Output: the line, or null when none needs it. */
export function signInLine(pool: PoolSummary | undefined): string | null {
  const count = pool?.members.filter((member) => member.status === "not_signed_in").length ?? 0;
  if (!pool || count === 0) return null;
  return count === 1 ? `1 account in ${pool.name} isn't signed in.` : `${count} accounts in ${pool.name} aren't signed in.`;
}

const RESENDABLE = new Set<MasterTurn["phase"]>(["failed", "blocked", "uncertain", "cancelled"]);

/** Finds the owner's message to offer again when the latest master turn stopped or failed.
 * Inputs: the turns and the messages this window sent, by turn id. Output: the message, or
 * null when the latest turn replied, is still going or its text is not in this window. */
export function resendText(turns: MasterTurn[], sent: Record<string, string>): string | null {
  const latest = turns.reduce<MasterTurn | null>((last, turn) => last === null || turn.ordinal > last.ordinal ? turn : last, null);
  if (latest === null || !RESENDABLE.has(latest.phase)) return null;
  return sent[latest.id] ?? null;
}

const SEND_FALLBACK = "Couldn't send. Try again.";

/** The inline line under the composer when a message could not be sent; the message stays
 * in the composer. Input: the failure code. Output: the plain line. */
export function sendFailureLine(code: string): string {
  const copy = failureCopy(code);
  return copy === failureCopy("") ? SEND_FALLBACK : `Couldn't send. ${copy}`;
}

/** The muted line shown when a send found its conversation older than the settings and a new
 * conversation took the message instead. */
export const NEW_CONVERSATION_NOTE = "Settings changed, so a new conversation started.";

/** Whether a send refused because the settings changed after the conversation started should
 * end that conversation, start a new one and send the same message there.
 * Input: the send receipt. Output: true only for that confirmed refusal. */
export function restartForSettings(receipt: IntentReceipt): boolean {
  const state = receiptState(receipt);
  return state.kind === "rejected" && state.code === "master_source_conflict";
}

/** The most bytes one message to the master may hold; the bridge refuses more. */
export const MESSAGE_BYTES_MAX = 8192;

/** Whether the composer may send its draft now, the one rule for the Send button and Cmd+Enter.
 * Inputs: the draft and whether sending is blocked (an action running, no ticket, a reply in
 * progress or the master unreachable). Output: true for a non-blank draft within the byte limit. */
export function canSend(text: string, blocked: boolean): boolean {
  return !blocked && text.trim() !== "" && new TextEncoder().encode(text).length <= MESSAGE_BYTES_MAX;
}

/** The path a failed Add documents offers to retry in its one-path box.
 * Input: the picked paths. Output: the path when exactly one was picked, else empty, so
 * several paths are never joined into one invalid path. */
export function retryPath(paths: string[]): string {
  return paths.length === 1 ? paths[0] : "";
}

const FILE_ICONS = new Map<string, IconName>([
  ["md", "file-text"], ["txt", "file-text"], ["csv", "file-spreadsheet"], ["png", "file-image"], ["jpg", "file-image"], ["jpeg", "file-image"],
]);

/** Picks the icon for a project folder file from its extension.
 * Input: the file path. Output: a text, spreadsheet or image icon, or the plain file icon. */
export function fileIcon(path: string): IconName {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? FILE_ICONS.get(name.slice(dot + 1).toLowerCase()) ?? "file" : "file";
}

/** Splits a project's memories into the ones waiting for the owner and the saved ones;
 * rejected, removed and retracted memories are not shown.
 * Input: memories from the engine. Output: Suggested and Saved lists in engine order. */
export function memoryGroups(items: MemoryItem[]): { suggested: MemoryItem[]; saved: MemoryItem[] } {
  return {
    suggested: items.filter((item) => item.status === "Provisional"),
    saved: items.filter((item) => item.status === "Confirmed"),
  };
}

/** Tells whether the owner may delete a context file; the brief always stays.
 * Input: the context item. Output: true for documents and research reports. */
export function deletable(item: ContextItem): boolean {
  return item.kind === "document" || item.kind === "research";
}

/** The delete confirmation title for a context file.
 * Input: the file path. Output: the question naming the file. */
export function deleteQuestion(path: string): string {
  return `Delete ${path.slice(path.lastIndexOf("/") + 1)}?`;
}

/** The Overview's Needs you line for suggested memories.
 * Input: how many wait, at least one. Output: one plain sentence. */
export function suggestedLine(count: number): string {
  if (!(Number.isInteger(count) && count > 0)) throw new Error("suggestedLine needs a positive count");
  return count === 1 ? "1 suggested memory waits for you to save or dismiss it." : `${count} suggested memories wait for you to save or dismiss them.`;
}
