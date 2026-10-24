import { receiptState, type StoredSetup } from "../../api-projects";
import type {
  AddFolderMode, Dependency, DependencyName, DependencyState, FoundProject, FoundProjectState, GitHubRepo, GitHubStatus, IntentReceipt, OnboardingStep,
  OnboardingView, PackEntry, PackGroup, PackState, ProjectsHome, SetupStatus,
} from "../../engine-client";
import type { IconName, Tone } from "../../ui";
import { displayText } from "../conversation";
import { draftOf, formatBytes, type ProjectDraft } from "../new-project/wizard";
import { folderLabel } from "../project/overview";

export const COPY_WARN_BYTES = 1024 ** 3;
export const FOUND_POLL_MS = 3000;
export const ONBOARDING_POLL_MS = 2000;
export const GITHUB_POLL_MS = 3000;
export const SETUP_POLL_MS = 2000;
export const REPO_PAGES_MAX = 100;

export type FoundAction = "setup" | "continue" | "open" | "remove" | null;

export interface FoundRow {
  badge: string;
  tone: Tone;
  line: string;
  action: FoundAction;
}

const STATE_ORDER: FoundProjectState[] = ["found", "onboarding", "missing", "ready"];

/** What Home says about one folder in the projects folder and the one thing it offers.
 * Input: the found folder. Output: its badge, tone, line and action; no action without a project id
 * where the action needs one. */
export function foundRow(folder: FoundProject): FoundRow {
  const id = folder.project_id ?? null;
  switch (folder.state) {
    case "found":
      return { badge: "Found", tone: "info", action: "setup",
        line: folder.known ? "It has CrazyAgent files from before. Set it up to reuse them." : "New in your projects folder. Set it up to start working in it." };
    case "onboarding":
      return { badge: "Setting up", tone: "warn", line: "Review what CrazyAgent found, then confirm.", action: id ? "continue" : null };
    case "ready":
      return { badge: "Ready", tone: "ok", line: "Set up and ready.", action: id ? "open" : null };
    case "missing":
      return { badge: "Folder missing", tone: "danger", action: id ? "remove" : null,
        line: `The folder isn't in your projects folder anymore. ${id ? "Put it back, or remove the project." : "Put it back to see it here."}` };
    default:
      return { badge: "Found", tone: "neutral", line: "", action: null };
  }
}

/** The folders Home lists above the project cards: found, then setting up, then missing, each by
 * name. Ready folders are already project cards. Input: the found folders. Output: a new list. */
export function pendingFolders(folders: FoundProject[]): FoundProject[] {
  return folders.filter((folder) => folder.state !== "ready")
    .sort((left, right) => STATE_ORDER.indexOf(left.state) - STATE_ORDER.indexOf(right.state) || left.name.localeCompare(right.name));
}

/** Says where the projects folder is and whether new folders show up live.
 * Input: the projects home read. Output: one sentence, or null before a folder is chosen. */
export function homeLine(home: ProjectsHome): string | null {
  if (!home.path) return null;
  const where = folderLabel(home.path);
  return home.watching ? `Watching ${where}. Drop a project folder into it with Finder and it shows up here.`
    : `${where} is checked when CrazyAgent starts. Live updates are off right now.`;
}

const trimSlash = (path: string) => path.replace(/\/+$/u, "");

/** Checks a folder picked for Add existing folder; the engine checks again.
 * Inputs: the absolute path and the projects folder, or null. Output: the problem, or null. */
export function pickedFolderProblem(path: string, home: string | null): string | null {
  const clean = trimSlash(path);
  if (!clean.startsWith("/") || /[\u0000-\u001f\u007f-\u009f]/u.test(clean)) return "Choose the folder again with Choose folder.";
  if (home) {
    const root = trimSlash(home);
    if (clean === root || root.startsWith(`${clean}/`)) return "This folder holds your projects folder. Choose one project's folder.";
    if (clean.startsWith(`${root}/`)) return "This folder is already in your projects folder. It shows on Home.";
  }
  return null;
}

/** The last part of a folder path. Input: the path. Output: the folder's name. */
export function folderName(path: string): string {
  const clean = trimSlash(path);
  return clean.slice(clean.lastIndexOf("/") + 1);
}

/** Says what Move or Copy does; Copy shows the size first and warns above 1 GB.
 * Inputs: the mode, the folder size or null while it is read, and the projects folder.
 * Output: the sentence and whether it is a warning. */
export function sizeLine(mode: AddFolderMode, bytes: number | null, home: string): { text: string; warn: boolean } {
  const where = folderLabel(home);
  if (mode === "move") return { text: `Moves the folder into ${where}. It won't stay at its old place.`, warn: false };
  if (bytes === null) return { text: "Checking the folder size...", warn: false };
  if (bytes > COPY_WARN_BYTES) {
    return { text: `This copies ${formatBytes(bytes)} into ${where}. That takes a while and uses as much disk again. Move is instant on the same disk.`, warn: true };
  }
  return { text: `Copies ${formatBytes(bytes)} into ${where}. The original stays where it is.`, warn: false };
}

/** Says how long the GitHub device code still works. Inputs: its lifetime in seconds, when it
 * arrived and now, in milliseconds. Output: whether it expired and one sentence. */
export function deviceLine(expiresS: number, startedMs: number, nowMs: number): { expired: boolean; text: string } {
  const left = startedMs + expiresS * 1000 - nowMs;
  if (left <= 0) return { expired: true, text: "This code expired. Start again to get a new one." };
  const minutes = Math.ceil(left / 60000);
  return { expired: false, text: `Enter this code on the GitHub page. It works for ${minutes} more ${minutes === 1 ? "minute" : "minutes"}.` };
}

/** Allows only a GitHub page over https as the device sign-in page.
 * Input: the page the engine sent. Output: the same address, or null when it is anything else. */
export function gitHubPage(uri: string): string | null {
  try {
    const url = new URL(uri);
    return url.protocol === "https:" && url.hostname === "github.com" && !url.username && !url.password ? uri : null;
  } catch {
    return null;
  }
}

const METHOD_TEXT: Record<GitHubStatus["method"], string> = { device: "", token: " with a token", cli: " through the GitHub CLI", none: "" };

/** Says who CrazyAgent is signed in to GitHub as. Input: the status. Output: one sentence. */
export function accountLine(status: GitHubStatus): string {
  if (!status.signed_in) return "Not signed in to GitHub.";
  const who = status.login ? ` as ${displayText(status.login)}` : "";
  return `Signed in to GitHub${who}${METHOD_TEXT[status.method] ?? ""}.`;
}

/** Filters repositories by a case-insensitive part of owner/name.
 * Inputs: the repositories and the search text. Output: the matching ones in order. */
export function repoMatches(repos: GitHubRepo[], query: string): GitHubRepo[] {
  const needle = query.trim().toLowerCase();
  return needle ? repos.filter((repo) => repo.full_name.toLowerCase().includes(needle)) : repos;
}

/** One repository's details line. Input: the repository. Output: visibility, size and branch. */
export function repoMeta(repo: GitHubRepo): string {
  return `${repo.private ? "Private" : "Public"}, ${formatBytes(repo.size_kb * 1024)}, branch ${displayText(repo.default_branch)}`;
}

/** Badge word, tone and failure line for one onboarding step.
 * Input: the step. Output: the fixed status word, its tone and the reason when it failed. */
export function stepView(step: OnboardingStep): { word: string; tone: Tone; line: string | null } {
  switch (step.kind) {
    case "running": return { word: "Working", tone: "info", line: null };
    case "done": return { word: "Done", tone: "ok", line: null };
    case "failed": return { word: "Failed", tone: "danger", line: displayText(step.reason) };
    default: return { word: "Waiting", tone: "neutral", line: null };
  }
}

const MISSING_LABELS: Record<string, string> = {
  name: "Project name", about: "What is it about", tech_stack: "Tech stack", platforms: "Target platforms",
};

/** Names what the mapping agent couldn't find, as the form labels say it, once each.
 * Input: the missing item keys. Output: plain labels in order. */
export function missingLabels(missing: string[]): string[] {
  return [...new Set(missing)].map((item) => MISSING_LABELS[item] ?? displayText(item.replaceAll("_", " ")));
}

/** Starts the review form from the mapping agent's draft, keeping the saved pool and team.
 * Inputs: the onboarding view and the saved setup. Output: the form draft. */
export function onboardingDraft(view: OnboardingView, stored: StoredSetup): ProjectDraft {
  return {
    ...draftOf(stored),
    name: view.name.trim() || stored.name,
    about: view.draft.about,
    stack: [...view.draft.tech_stack],
    platforms: [...view.draft.platforms],
    research: [...view.draft.research],
  };
}

/** Tells whether the graph and mapping steps have stopped changing.
 * Input: the onboarding view. Output: true when polling can stop. */
export function settled(view: OnboardingView): boolean {
  const done = (step: OnboardingStep) => step.kind === "done" || step.kind === "failed";
  return done(view.mapping) || (view.graph.kind === "failed" && view.mapping.kind === "waiting");
}

/** Says whether the owner can confirm yet and what to do.
 * Input: the onboarding view. Output: ready and one sentence. */
export function confirmReady(view: OnboardingView): { ready: boolean; line: string } {
  if (view.mapping.kind === "done") return { ready: true, line: "Check the answers, fill in anything missing, then confirm." };
  if (view.mapping.kind === "failed") return { ready: true, line: "The mapping agent stopped. Fill in the form yourself, then confirm." };
  if (view.graph.kind === "failed") return { ready: true, line: "The code graph couldn't be built. Fill in the form yourself, then confirm." };
  if (view.graph.kind !== "done") return { ready: false, line: "Building the code graph. The form fills in when the mapping agent finishes." };
  return { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." };
}

/** Reads a Start setup receipt. Input: the receipt. Output: started with the project id, rejected
 * with a code, or unconfirmed when it is pending or carries no id. */
export function startedProject(receipt: IntentReceipt): { kind: "started"; id: string } | { kind: "rejected"; code: string } | { kind: "unconfirmed" } {
  const state = receiptState(receipt);
  if (state.kind !== "applied") return state;
  return receipt.project_id ? { kind: "started", id: receipt.project_id } : { kind: "unconfirmed" };
}

const DEPENDENCY_ABOUT: Record<DependencyName, string> = {
  tmux: "Runs each agent in a live terminal.",
  uv: "Installs graphify.",
  graphify: "Builds a code graph of each project.",
};

/** Badge word, tone and detail line for a tool or pack entry state.
 * Input: the state. Output: the fixed word, its tone and one line or null. */
export function stateView(state: DependencyState): { word: string; tone: Tone; line: string | null } {
  switch (state.state) {
    case "found": return { word: "Found", tone: "ok", line: `Version ${displayText(state.version)} on this Mac.` };
    case "installed": return { word: "Installed", tone: "ok", line: `Version ${displayText(state.version)} installed.` };
    case "installing": return { word: "Installing", tone: "info", line: null };
    case "failed": return { word: "Failed", tone: "danger", line: displayText(state.reason) };
    default: return { word: "Not installed", tone: "warn", line: "Setup installs it." };
  }
}

/** One tool row for setup. Input: the dependency. Output: its name, what it is for and its state. */
export function dependencyView(dependency: Dependency): { title: string; about: string; word: string; tone: Tone; line: string | null } {
  return { title: displayText(dependency.label.trim() || dependency.name), about: DEPENDENCY_ABOUT[dependency.name] ?? "", ...stateView(dependency.state) };
}

/** Badge word, tone and detail line for a default-pack entry state.
 * Input: the state. Output: the fixed word, its tone and one line or null. */
export function packStateView(state: PackState): { word: string; tone: Tone; line: string | null } {
  switch (state.state) {
    case "on": return { word: "On", tone: "ok", line: null };
    case "off": return { word: "Off", tone: "neutral", line: null };
    case "installing": return { word: "Installing", tone: "info", line: null };
    case "failed": return { word: "Failed", tone: "danger", line: displayText(state.reason) };
    default: return { word: "Not installed", tone: "neutral", line: null };
  }
}

/** Tells whether a pack entry is installed, so Settings can switch or remove it.
 * Input: the entry. Output: true when it is on or off. */
export function installed(entry: PackEntry): boolean {
  return entry.state.state === "on" || entry.state.state === "off";
}

/** The name a pack entry shows. Input: the entry. Output: its id as plain text. */
export function entryName(entry: PackEntry): string {
  return displayText(entry.id);
}

/** The icon a pack entry shows. Input: the entry. Output: server for an MCP server, blocks for a skill. */
export function entryIcon(entry: PackEntry): IconName {
  return entry.kind === "mcp_server" ? "server" : "blocks";
}

export const PACK_GROUPS: { group: PackGroup; title: string; about: string }[] = [
  { group: "always", title: "Always on", about: "Every project needs these." },
  { group: "workflow", title: "Workflow skills", about: "How agents plan, build, test and review." },
  { group: "design", title: "Design skills", about: "Taste and explainers for front ends and docs." },
  { group: "reply_style", title: "Reply style", about: "Change how every agent writes its replies." },
  { group: "context", title: "Context plugins", about: "MCP servers that keep agents' context small." },
  { group: "optional", title: "Optional", about: "Off unless you turn them on. Some need their own accounts." },
];

/** Groups the pack the way the setup screen lists it, keeping pack order inside each group.
 * Input: the pack. Output: the groups that have entries, with their titles. */
export function packGroups(pack: PackEntry[]): { group: PackGroup; title: string; about: string; entries: PackEntry[] }[] {
  const known = new Set(PACK_GROUPS.map((item) => item.group));
  const groups = PACK_GROUPS.map((item) => ({ ...item, entries: pack.filter((entry) => entry.group === item.group) }));
  const other = pack.filter((entry) => !known.has(entry.group));
  if (other.length > 0) groups.push({ group: "optional", title: "Other", about: "", entries: other });
  return groups.filter((item) => item.entries.length > 0);
}

/** The pack entries ticked before the owner changes anything: the ones on by default.
 * Input: the pack. Output: their ids in pack order. */
export function packTicks(pack: PackEntry[]): string[] {
  return pack.filter((entry) => entry.on_by_default).map((entry) => entry.id);
}

/** Names the ticked MCP servers that continuing approves.
 * Inputs: the pack and the ticked ids. Output: one sentence, or null when none is ticked. */
export function approvalLine(pack: PackEntry[], ticked: string[]): string | null {
  const servers = pack.filter((entry) => entry.kind === "mcp_server" && ticked.includes(entry.id)).map(entryName);
  if (servers.length === 0) return null;
  return `Continuing lets your agents run these MCP servers: ${servers.join(", ")}. You can turn any of them off later.`;
}

/** Tells whether setup is still working. Input: the status. Output: true while it runs or
 * anything is installing. */
export function setupBusy(status: SetupStatus): boolean {
  return status.running || status.dependencies.some((item) => item.state.state === "installing") || status.pack.some((item) => item.state.state === "installing");
}

/** Counts setup progress for the progress line: every tool and every ticked pack entry.
 * Inputs: the status and the ticked ids. Output: how many are settled, of how many, and how many failed. */
export function setupProgress(status: SetupStatus, ticked: string[]): { done: number; total: number; failed: number } {
  const states = [...status.dependencies.map((item) => item.state.state),
    ...status.pack.filter((item) => ticked.includes(item.id) || item.group === "always").map((item) => item.state.state)];
  const settledStates = new Set(["found", "installed", "on", "off", "failed"]);
  return { done: states.filter((state) => settledStates.has(state)).length, total: states.length, failed: states.filter((state) => state === "failed").length };
}
