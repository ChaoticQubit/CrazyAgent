export type MemoryState = "pending" | "approved" | "rejected" | "removed" | "retracted";

/** One memory as `ide.memories` lists it, with the revision a decision must name. */
export interface Memory {
  id: string;
  revision: number;
  text: string;
  state: MemoryState;
  reviewed: boolean;
}

export type AgentStatus = "starting" | "idle" | "working" | "stopped" | "failed";

/** One team agent as `ide.agents` lists it; `worktree` is an absolute path or null. */
export interface Agent {
  id: number;
  label: string;
  role: "master" | "sub";
  harness: string;
  status: AgentStatus;
  status_label: string;
  worktree: string | null;
  message: boolean;
}

/** The parts of one `ide.change_sets` entry the sidebar shows. */
export interface ChangeSet {
  id: string;
  agent: string;
  title: string;
  state: "pending" | "conflict" | "updating" | "accepted" | "discarded";
  files: unknown[];
  restorable_until: number | null;
}

/** One row of a sidebar tree: a group with children, or a leaf the views turn into an item. */
export interface Row {
  key: string;
  label: string;
  description: string;
  kind: string;
  children: Row[];
  memory?: Memory;
  agent?: Agent;
  change?: ChangeSet;
  path?: string;
}

/** What the context view found at the project's `.crazyagent` path. */
export interface Found {
  directory: boolean;
  symlink: boolean;
}

export const LABEL_CHARS_MAX = 80;
export const ROWS_PER_GROUP_MAX = 200;
export const MEMORY_PAGES_MAX = 50;

const UNSAFE = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g;
const UNSAFE_CHAR = /[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/;
const OPEN_CHANGES = new Set<ChangeSet["state"]>(["pending", "conflict", "updating"]);
const MEMORY_GROUPS: { label: string; kind: string; states: MemoryState[] }[] = [
  { label: "Waiting for you", kind: "memory.pending", states: ["pending"] },
  { label: "Approved", kind: "memory.approved", states: ["approved"] },
  { label: "Set aside", kind: "memory.aside", states: ["rejected", "removed", "retracted"] },
];

/**
 * Groups memories for the sidebar: waiting first, then approved, then set aside, each in the
 * engine's id order; empty groups are left out. Input: the memories. Output: the groups.
 */
export function memoryGroups(memories: Memory[]): Row[] {
  return MEMORY_GROUPS.map(({ label, kind, states }) => group(`memories:${kind}`, label, memories
    .filter((memory) => states.includes(memory.state))
    .map((memory) => ({
      key: `memory:${memory.id}`,
      label: plainLine(memory.text, LABEL_CHARS_MAX),
      description: `revision ${memory.revision}`,
      kind,
      children: [],
      memory,
    })))).filter(nonEmpty);
}

/**
 * Groups agents for the sidebar: the master, then running sub-agents, then stopped ones,
 * each in the engine's order; empty groups are left out. A leaf's kind lists the actions it
 * offers: `agent.message` for an agent the engine lets the owner message, `agent.worktree`
 * for one with a worktree. Input: the agents. Output: the groups.
 */
export function agentGroups(agents: Agent[]): Row[] {
  const running = (agent: Agent) => agent.status !== "stopped" && agent.status !== "failed";
  const groups: [string, Agent[]][] = [
    ["Master", agents.filter((agent) => agent.role === "master")],
    ["Running", agents.filter((agent) => agent.role === "sub" && running(agent))],
    ["Stopped", agents.filter((agent) => agent.role === "sub" && !running(agent))],
  ];
  return groups.map(([label, members]) => group(`agents:${label}`, label, members.map((agent) => ({
    key: `agent:${agent.id}`,
    label: plainLine(agent.label, LABEL_CHARS_MAX),
    description: plainLine(agent.status_label, LABEL_CHARS_MAX),
    kind: ["agent", agent.message ? "agent.message" : "", agent.worktree ? "agent.worktree" : ""].filter(Boolean).join(" "),
    children: [],
    agent,
  })))).filter(nonEmpty);
}

/**
 * Groups the change sets that have a Source Control group: open ones, then discarded ones
 * still restorable at `now`; accepted and expired sets have none and are left out. Each leaf
 * is named the way its Source Control group is. Input: the change sets and the time in
 * milliseconds. Output: the groups.
 */
export function changeGroups(changes: ChangeSet[], now: number): Row[] {
  const restorable = (change: ChangeSet) =>
    change.state === "discarded" && change.restorable_until !== null && change.restorable_until > now;
  const groups: [string, ChangeSet[]][] = [
    ["Open", changes.filter((change) => OPEN_CHANGES.has(change.state))],
    ["Can restore", changes.filter(restorable)],
  ];
  return groups.map(([label, members]) => group(`changes:${label}`, label, members.map((change) => ({
    key: `change:${change.id}`,
    label: `${plainLine(change.agent, LABEL_CHARS_MAX)}: ${plainLine(change.title, LABEL_CHARS_MAX) || "Changes"}`,
    description: `${change.files.length} ${change.files.length === 1 ? "file" : "files"}`,
    kind: "change",
    children: [],
    change,
  })))).filter(nonEmpty);
}

/**
 * The quick links to the project's context files: the brief, then documents and research by
 * name. Names that are empty, start with a dot or hold a slash or a control character are
 * left out. Input: the context folder and the names in its documents and research folders,
 * or null for a missing folder. Output: the rows.
 */
export function contextLinks(folder: string, documents: string[] | null, research: string[] | null): Row[] {
  const base = folder.replace(/\/+$/, "");
  const files = (directory: string, names: string[] | null): Row[] => (names ?? [])
    .filter((name) => name !== "" && !name.startsWith(".") && !name.includes("/") && !UNSAFE_CHAR.test(name))
    .sort((a, b) => a.localeCompare(b))
    .map((name) => ({
      key: `context:${directory}/${name}`,
      label: name,
      description: "",
      kind: "context.file",
      children: [],
      path: `${base}/${directory}/${name}`,
    }));
  const brief: Row = { key: "context:brief.md", label: "Brief", description: "brief.md", kind: "context.file", children: [], path: `${base}/brief.md` };
  const groups = [group("context:documents", "Documents", files("documents", documents)), group("context:research", "Research", files("research", research))];
  return [brief, ...groups.filter(nonEmpty)];
}

/**
 * The project's context folder: `.crazyagent/context` when `.crazyagent` is a real folder,
 * not a link, else `context` in the project folder itself, as the engine decides. Input: the
 * project folder and what is at its `.crazyagent` path, or null for nothing. Output: the
 * absolute folder path.
 */
export function contextFolder(project: string, found: Found | null): string {
  const base = project.replace(/\/+$/, "");
  return found !== null && found.directory && !found.symlink ? `${base}/.crazyagent/context` : `${base}/context`;
}

/**
 * The `after` id for the next `ide.memories` call, or null to stop: a page that came back
 * empty ends the list, and so does the page limit. Input: the page just read and how many
 * pages have been read. Output: the last id of the page, or null.
 */
export function nextPage(page: Memory[], pages: number): string | null {
  if (page.length === 0 || pages >= MEMORY_PAGES_MAX) return null;
  return page[page.length - 1].id;
}

/**
 * One line of text safe to show: control and direction characters become spaces, runs of
 * space collapse, and anything over `max` characters ends in "...". Input: the text and
 * the limit. Output: the line.
 */
export function plainLine(text: string, max: number): string {
  const line = text.replace(UNSAFE, " ").replace(/\s+/g, " ").trim();
  const chars = [...line];
  return chars.length <= max ? line : `${chars.slice(0, max - 3).join("")}...`;
}

function group(key: string, label: string, children: Row[]): Row {
  const shown = children.slice(0, ROWS_PER_GROUP_MAX);
  const description = shown.length < children.length ? `${shown.length} of ${children.length}` : String(children.length);
  return { key, label, description, kind: "group", children: shown };
}

function nonEmpty(row: Row): boolean {
  return row.children.length > 0;
}
