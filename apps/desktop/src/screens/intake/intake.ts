import { ADD_WARNINGS, measuredLine } from "../../api-intake";
import { receiptState, type StoredSetup } from "../../api-projects";
import type {
  AddFolderMode, AddOverride, Dependency, DependencyName, DependencyState, FolderSize, FoundProject, FoundProjectState, GitHubRepo, GitHubStatus, IntentReceipt, MappingFacts,
  MappingTry, OnboardingStep, OnboardingView, PackEntry, PackGroup, PackState, ProjectsHome, ResearchKind, SetupStatus,
} from "../../engine-client";
import type { Loaded } from "../../state";
import type { IconName, Tone } from "../../ui";
import { displayText } from "../conversation";
import { draftOf, formatBytes, researchChoices, type ProjectDraft } from "../new-project/wizard";
import { folderLabel } from "../project/overview";
import { localClock } from "../project/research";

export const COPY_WARN_BYTES = 1024 ** 3;
export const FOUND_POLL_MS = 3000;
export const ONBOARDING_POLL_MS = 2000;
export const GITHUB_POLL_MS = 3000;
export const SETUP_POLL_MS = 2000;
export const SETUP_POLL_MAX_MS = 30000;
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

const trimSlash = (path: string) => path.replace(/[\\/]+$/u, "");
const unified = (path: string) => trimSlash(path).replace(/\\/gu, "/");

/** Checks a folder picked for Add existing folder: an absolute macOS, Linux or Windows path
 * (drive or network share) outside the projects folder. The engine checks again.
 * Inputs: the path and the projects folder, or null. Output: the problem, or null. */
export function pickedFolderProblem(path: string, home: string | null): string | null {
  const clean = unified(path);
  if (!/^(\/|[A-Za-z]:\/)/u.test(clean) || /[\u0000-\u001f\u007f-\u009f]/u.test(clean)) return "Choose the folder again with Choose folder.";
  if (home) {
    const root = unified(home);
    if (clean === root || root.startsWith(`${clean}/`)) return "This folder holds your projects folder. Choose one project's folder.";
    if (clean.startsWith(`${root}/`)) return "This folder is already in your projects folder. It shows on Home.";
  }
  return null;
}

/** The last part of a folder path written with forward or back slashes.
 * Input: the path. Output: the folder's name. */
export function folderName(path: string): string {
  const clean = unified(path);
  return clean.slice(clean.lastIndexOf("/") + 1);
}

/** Joins a folder name onto a folder path with that path's own separator.
 * Inputs: the folder path and the name. Output: the joined path. */
export function joinPath(root: string, name: string): string {
  const separator = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  return `${trimSlash(root)}${separator}${name}`;
}

/** How the owner adds an existing folder: set it up where it is (the default, nothing on disk
 * moves), or explicitly move or copy it into the projects folder. */
export type AddWay = "here" | AddFolderMode;

/** Says what using the folder where it is, Move or Copy does; Copy shows the size first and warns
 * above 1 GB or when the folder could not be measured. Inputs: the way, the folder size or null,
 * the projects folder and whether the size read finished without a size. Output: the sentence and
 * whether it is a warning. */
export function sizeLine(mode: AddWay, bytes: number | null, home: string, unmeasured = false): { text: string; warn: boolean } {
  const where = folderLabel(home);
  if (mode === "here") return { text: "Sets it up where it is. Nothing is moved or copied.", warn: false };
  if (mode === "move") return { text: `Moves the folder into ${where}. It won't stay at its old place.`, warn: false };
  if (bytes === null && unmeasured) return { text: `CrazyAgent couldn't measure this folder. A copy puts all of it into ${where}; the original stays where it is.`, warn: true };
  if (bytes === null) return { text: "Checking the folder size...", warn: false };
  if (bytes > COPY_WARN_BYTES) {
    return { text: `This copies ${formatBytes(bytes)} into ${where}. That takes a while and uses as much disk again. Using it where it is needs no copy.`, warn: true };
  }
  return { text: `Copies ${formatBytes(bytes)} into ${where}. The original stays where it is.`, warn: false };
}

/** Says that a Copy leaves the folder's linked git worktrees with the original, since a copy
 * that kept them would share them with it. Input: how many linked worktrees the folder has.
 * Output: one sentence. */
export function worktreesLine(count: number): string {
  const many = count !== 1;
  return `This folder has ${count} linked git ${many ? "worktrees" : "worktree"}. ${many ? "They stay" : "It stays"} with the original folder; the copy gets the main checkout and every branch.`;
}

/** Why Add folder offers no Move or Copy yet while the folder read is still checking its git links. */
export const WAYS_CHECKING = "Move and Copy are offered once CrazyAgent has checked this folder's git links.";

/** Whether Add folder offers Move and Copy: once the folder read has finished, whatever it found,
 * since what it found is asked about before anything moves. Input: the folder read, if any.
 * Output: true once a folder is picked and its read is not loading. */
export function waysOffered(size: Loaded<FolderSize> | null): boolean {
  return size !== null && size.kind !== "loading";
}

/** Why Add folder offers no Move or Copy for a picked folder. Input: the folder read, if any.
 * Output: the reason lines; one only while the read is still checking. */
export function waysWithheld(size: Loaded<FolderSize> | null): string[] {
  return size?.kind === "loading" ? [WAYS_CHECKING] : [];
}

/** The way Add folder uses: the chosen one, or Use where it is while Move and Copy wait for the
 * folder read. Inputs: the chosen way and the folder read. Output: the way to show and add with. */
export function addWay(mode: AddWay, size: Loaded<FolderSize> | null): AddWay {
  return mode !== "here" && !waysOffered(size) ? "here" : mode;
}

/** Something about a folder the owner is asked about before a Move or Copy goes ahead. */
export type AnywayIssue =
  | "large"
  | "too_many"
  | "unreadable"
  | "worktrees"
  | "linked_worktree"
  | "submodule"
  | "git_data_unread"
  | "linked_checkout"
  | "no_git"
  | "git_unread";

const ISSUE_ORDER: AnywayIssue[] = ["large", "too_many", "unreadable", "worktrees", "linked_worktree", "submodule", "git_data_unread", "linked_checkout", "no_git", "git_unread"];

const MOVE_LINK_ISSUES: AnywayIssue[] = ["worktrees", "linked_worktree", "submodule", "git_data_unread"];

const REFUSED_ISSUES: Record<string, AnywayIssue> = {
  home_copy_linked_checkout: "linked_checkout",
  home_copy_too_large: "too_many",
};

/** The issue a Move of a folder with git links outside it is asked about: the link the read
 * named, or linked worktrees when the read did not name one. Input: the folder read, if any.
 * Output: the issue. */
function moveLinkIssue(read: FolderSize | null): AnywayIssue {
  switch (read?.git_link) {
    case "linked_worktree":
      return "linked_worktree";
    case "submodule":
      return "submodule";
    case "unreadable":
      return "git_data_unread";
    default:
      return "worktrees";
  }
}

/** What the owner is asked about before a Move or Copy of a picked folder: what its read found
 * for that way, plus the issue of a soft refusal the engine sent back. A Move on the same disk is
 * a rename, so size matters only to a Copy or a Move that may cross disks. Inputs: the way, the
 * folder read and the refusal code, if any. Output: the issues in a fixed order, none while the
 * read is loading. */
export function anywayIssues(way: AddFolderMode, size: Loaded<FolderSize> | null, refused: string | null): AnywayIssue[] {
  if (size === null || size.kind === "loading") return [];
  const found = new Set<AnywayIssue>();
  const read = size.kind === "ok" ? size.value : null;
  if (refused === "home_move_worktrees") found.add(moveLinkIssue(read));
  else if (refused !== null && Object.hasOwn(REFUSED_ISSUES, refused)) found.add(REFUSED_ISSUES[refused]);
  if (read === null || typeof read.movable !== "boolean" || typeof read.copyable !== "boolean") found.add("git_unread");
  else if (way === "move" && !read.movable) found.add(moveLinkIssue(read));
  else if (way === "copy" && !read.copyable) found.add("linked_checkout");
  if (read?.git === false) found.add("no_git");
  if (read?.size_problem === "folder_size_unreadable") found.add("unreadable");
  const copies = way === "copy" || read?.same_volume !== true;
  if (copies && (read?.size_problem === "folder_size_timeout" || read?.size_problem === "folder_size_too_large")) found.add("large");
  if (copies && read?.size_problem === "folder_size_too_large") found.add("too_many");
  if (copies && typeof read?.bytes === "number" && read.bytes > COPY_WARN_BYTES) found.add("large");
  return ISSUE_ORDER.filter((issue) => found.has(issue));
}

/** The overrides Move anyway or Copy anyway sends: one for each issue the engine itself refuses,
 * never for one the owner was not shown. Inputs: the way, the issues and the folder read.
 * Output: the overrides, in the engine's order. */
export function anywayOverrides(way: AddFolderMode, issues: AnywayIssue[], size: Loaded<FolderSize> | null): AddOverride[] {
  const unmeasured = size?.kind === "ok" && size.value.size_problem === "folder_size_timeout";
  const overrides: AddOverride[] = [];
  if (way === "move" && issues.some((issue) => MOVE_LINK_ISSUES.includes(issue))) overrides.push("worktrees");
  if (way === "copy" && issues.includes("linked_checkout")) overrides.push("linked_checkout");
  if (issues.includes("too_many") || (issues.includes("large") && unmeasured)) overrides.push("too_large");
  return overrides;
}

const REFUSAL_OVERRIDES: Record<string, AddOverride> = {
  home_move_worktrees: "worktrees",
  home_copy_linked_checkout: "linked_checkout",
  home_copy_too_large: "too_large",
};

/** Whether an engine refusal of a Move or Copy turns into the question for that issue: only a
 * soft refusal whose override was not already sent, so the owner is never asked twice about one
 * issue. Inputs: the refusal code and the overrides sent. Output: true to ask. */
export function asksAgain(code: string, sent: AddOverride[]): boolean {
  return Object.hasOwn(REFUSAL_OVERRIDES, code) && !sent.includes(REFUSAL_OVERRIDES[code]);
}

/** One warning block of the Move anyway or Copy anyway question: whether it is about losing
 * data, its lines in the owner's words, and the folders it names. */
export interface AnywayWarning {
  loss: boolean;
  lines: string[];
  paths: string[];
}

/** The warnings the question shows: data loss blocks first (a copy sharing another checkout's
 * git data, a moved worktree whose git data its repository can prune, apps still using the folder,
 * a copy across disks, a very large or deep folder, unreadable files), then the other effects (the
 * old place, git worktrees with their folders, the folder being a submodule, git data not read, no
 * git, git not read, a quick rename). Inputs: the way, the issues and the folder read. Output: the
 * blocks in that order. */
export function anywayWarnings(way: AddFolderMode, issues: AnywayIssue[], size: Loaded<FolderSize> | null): AnywayWarning[] {
  const read = size?.kind === "ok" ? size.value : null;
  const moving = way === "move";
  const copyLine = moving ? ADD_WARNINGS.across_disks : ADD_WARNINGS.copy_all;
  const block = (loss: boolean, lines: string[], paths: string[] = []): AnywayWarning => ({ loss, lines, paths });
  const loss: AnywayWarning[] = issues.includes("linked_checkout") ? [block(true, [ADD_WARNINGS.linked_checkout])] : [];
  if (issues.includes("linked_worktree")) loss.push(block(true, [ADD_WARNINGS.linked_worktree]));
  if (moving) loss.push(block(true, [ADD_WARNINGS.close_apps]));
  let copying = false;
  if (issues.includes("large")) {
    loss.push(block(true, [ADD_WARNINGS.large, typeof read?.bytes === "number" ? measuredLine(read.bytes) : ADD_WARNINGS.unmeasured, copyLine]));
    copying = true;
  }
  if (issues.includes("too_many")) {
    loss.push(block(true, [...(copying ? [] : [copyLine]), ADD_WARNINGS.too_many]));
    copying = true;
  }
  if (moving && !copying && read?.same_volume === false) {
    loss.push(block(true, [ADD_WARNINGS.across_disks]));
    copying = true;
  }
  if (issues.includes("unreadable")) loss.push(block(true, [moving ? ADD_WARNINGS.unreadable : ADD_WARNINGS.unreadable_copy]));
  const effects: AnywayWarning[] = moving ? [block(false, [ADD_WARNINGS.old_place])] : [];
  if (issues.includes("worktrees")) effects.push(block(false, [ADD_WARNINGS.worktrees], read?.worktree_paths ?? []));
  if (issues.includes("submodule")) effects.push(block(false, [ADD_WARNINGS.submodule]));
  if (issues.includes("git_data_unread")) effects.push(block(false, [ADD_WARNINGS.git_data_unread]));
  if (issues.includes("no_git")) effects.push(block(false, [ADD_WARNINGS.no_git]));
  if (issues.includes("git_unread")) effects.push(block(false, [ADD_WARNINGS.git_unread]));
  if (moving && !copying && read?.same_volume === true) effects.push(block(false, [ADD_WARNINGS.same_disk]));
  return [...loss, ...effects];
}

/** Says how long the GitHub device code still works. Inputs: its lifetime in seconds, when it
 * arrived and now, in milliseconds. Output: whether it expired and one sentence. */
export function deviceLine(expiresS: number, startedMs: number, nowMs: number): { expired: boolean; text: string } {
  const left = startedMs + expiresS * 1000 - nowMs;
  if (left <= 0) return { expired: true, text: "This code expired. Start again to get a new one." };
  const minutes = Math.ceil(left / 60000);
  return { expired: false, text: `Enter this code on the GitHub page. It works for ${minutes} more ${minutes === 1 ? "minute" : "minutes"}.` };
}

/** Whether the GitHub device sign-in still asks for its status: a code is out, the owner isn't
 * signed in yet and the code hasn't expired, so nothing is polled past the code's deadline.
 * Inputs: the device flow (its code's lifetime and when it arrived) or null, whether the owner
 * is signed in, and now in milliseconds. Output: true while the status should be polled. */
export function devicePolling(device: { code: { expires_s: number }; started: number } | null, signedIn: boolean, nowMs: number): boolean {
  return device !== null && !signedIn && !deviceLine(device.code.expires_s, device.started, nowMs).expired;
}

/** Lets one call of an async read run at a time: a call made while one is still running reads
 * nothing and gives null, so a double click can't read the same page twice. A failed read
 * frees the guard like a finished one. Input: the read. Output: the guarded read. */
export function oneAtATime<A extends unknown[], T>(read: (...args: A) => Promise<T>): (...args: A) => Promise<T | null> {
  let running = false;
  return async (...args: A) => {
    if (running) return null;
    running = true;
    try {
      return await read(...args);
    } finally {
      running = false;
    }
  };
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

/** Says which mapping try runs and for how many whole minutes, and why the try before it failed.
 * Inputs: the try and the time now in ms. Output: one line. */
function tryLine(progress: MappingTry, now: number): string {
  const minutes = Math.floor(Math.max(0, now - progress.started_ms) / 60000);
  const head = `Mapping, try ${progress.number} of ${progress.tries}, ${minutes < 1 ? "under 1 min" : `${minutes} min`}.`;
  return progress.earlier ? `${head} Try ${progress.number - 1} failed: ${displayText(progress.earlier)}` : head;
}

/** Badge word, tone and line for one onboarding step; a step waiting out a paused pool names when
 * the pause ends in local time, and a running mapping step its try. Inputs: the step, whether the
 * setup is already saved, when a step that never started was skipped, the mapping try and the time
 * now in ms. Output: the status word, its tone and the try or the reason when it failed. */
export function stepView(step: OnboardingStep, finished = false, progress: MappingTry | null = null, now = Date.now()): { word: string; tone: Tone; line: string | null } {
  switch (step.kind) {
    case "running": return { word: "Working", tone: "info", line: progress ? tryLine(progress, now) : null };
    case "done": return { word: "Done", tone: "ok", line: null };
    case "off": return { word: "Off", tone: "neutral", line: "Graphify is off in Settings, Pack, so no code graph is built." };
    case "not_installed": return { word: "Not installed", tone: "neutral", line: "Graphify isn't installed, so no code graph is built. Install it in Settings, Pack." };
    case "failed": return { word: "Failed", tone: "danger", line: displayText(step.reason) };
    default:
      if (finished) return { word: "Skipped", tone: "neutral", line: null };
      if (step.until_ms !== undefined) return { word: `Rate limited until ${localClock(step.until_ms)}`, tone: "warn", line: null };
      return { word: "Waiting", tone: "neutral", line: null };
  }
}

/** Whether the review offers Retry mapping: the mapping agent failed and the setup isn't saved yet.
 * Input: the onboarding view. Output: true when Retry and Skip show. */
export function mappingRetry(view: OnboardingView): boolean {
  return view.mapping.kind === "failed" && !view.finished;
}

/** The project's own folder from its saved setup: the recorded folder, or the folder holding it when
 * it is the project's `.crazyagent` folder. Input: the saved setup. Output: the folder path. */
export function projectFolder(stored: StoredSetup): string {
  const clean = trimSlash(stored.folder);
  const name = folderName(clean);
  return name === ".crazyagent" ? clean.slice(0, clean.length - name.length - 1) : clean;
}

const MISSING_LABELS: Record<string, string> = {
  name: "Project name", about: "What is it about", tech_stack: "Tech stack", platforms: "Target platforms",
};

/** Names what the mapping agent couldn't find, as the form labels say it, once each.
 * Input: the missing item keys. Output: plain labels in order. */
export function missingLabels(missing: string[]): string[] {
  return [...new Set(missing)].map((item) => MISSING_LABELS[item] ?? displayText(item.replaceAll("_", " ")));
}

/** Starts the review form from the mapping agent's draft, keeping the saved pool and team. A
 * missing or blank proposed name falls back to the saved name, since the reply comes from another
 * process. Roadmap planning starts on until the setup is first saved, then shows the saved choice.
 * Inputs: the onboarding view and the saved setup. Output: the form draft. */
export function onboardingDraft(view: OnboardingView, stored: StoredSetup): ProjectDraft {
  const saved = draftOf(stored);
  return {
    ...saved,
    name: (view.name ?? "").trim() || stored.name,
    about: view.draft.about,
    stack: [...view.draft.tech_stack],
    platforms: [...view.draft.platforms],
    research: researchChoices(view.draft.research).research,
    roadmap: view.finished ? saved.roadmap : true,
  };
}

/** What the mapping read in the project's own documents: per research kind the files that already
 * hold it, and the other project documents, each path made safe to show. Input: the mapping facts.
 * Output: { research, documents }. */
export function inRepo(facts: MappingFacts): { research: Partial<Record<ResearchKind, string[]>>; documents: string[]; roadmap: string[] } {
  const research: Partial<Record<ResearchKind, string[]>> = {};
  for (const found of facts.research ?? []) research[found.kind] = found.files.map(displayText);
  return { research, documents: (facts.documents ?? []).map(displayText), roadmap: (facts.roadmap ?? []).map(displayText) };
}

/** The most repository files one project links, as the engine's links store bounds them. */
export const LINKS_MAX = 32;

/** The repository files a first confirm links: the mapping's roadmap files unless the owner
 * turned "roadmap" off, then each research kind's files unless the owner turned that kind off,
 * each once and at most LINKS_MAX, as the mapping wrote them. Null once the setup is saved or
 * when the mapping found no file, so the engine leaves the links alone. Inputs: the onboarding
 * view and the turned-off keys ("roadmap" or a research kind). Output: the paths or null. */
export function confirmKept(view: OnboardingView, dropped: readonly string[]): string[] | null {
  const kept: string[] = [];
  const keep = (paths: readonly string[]) => {
    for (const path of paths) if (kept.length < LINKS_MAX && !kept.includes(path)) kept.push(path);
  };
  const found = [...(view.facts.roadmap ?? []), ...(view.facts.research ?? []).flatMap((item) => item.files)];
  if (view.finished || found.length === 0) return null;
  if (!dropped.includes("roadmap")) keep(view.facts.roadmap ?? []);
  for (const item of view.facts.research ?? []) if (!dropped.includes(item.kind)) keep(item.files);
  return kept;
}

/** Tells whether the mapping step has stopped changing or the setup is already saved. A failed
 * graph does not settle it: the mapping agent still runs after it.
 * Input: the onboarding view. Output: true when polling can stop. */
export function settled(view: OnboardingView): boolean {
  const done = (step: OnboardingStep) => step.kind === "done" || step.kind === "failed";
  return view.finished || done(view.mapping);
}

/** Says whether the owner can confirm yet and what to do.
 * Input: the onboarding view. Output: ready and one sentence. */
export function confirmReady(view: OnboardingView): { ready: boolean; line: string } {
  if (view.finished) return { ready: true, line: "This project is set up. Change anything below, then save." };
  if (view.mapping.kind === "done") return { ready: true, line: "Check the answers, fill in anything missing, then confirm." };
  if (view.mapping.kind === "failed") return { ready: true, line: "The mapping agent stopped. Fill in the form yourself, then confirm." };
  if (view.graph.kind === "failed") return { ready: false, line: "The code graph couldn't be built. The mapping agent still reads the project; the form fills in when it finishes." };
  if (!["done", "off", "not_installed"].includes(view.graph.kind)) return { ready: false, line: "Building the code graph. The form fills in when the mapping agent finishes." };
  return { ready: false, line: "The mapping agent is reading the project. The form fills in when it finishes." };
}

/** Whether the review offers the mapping's answers for a setup saved without them: the setup is
 * saved and the engine kept the mapping's draft as `proposed` with an About or a tech stack.
 * Input: the onboarding view. Output: true when "Use the mapping's answers" shows. */
export function offerProposed(view: OnboardingView): boolean {
  const proposed = view.proposed ?? null;
  return view.finished && proposed !== null && (proposed.about.trim() !== "" || proposed.tech_stack.length > 0);
}

/** The form's draft with the mapping's About, tech stack and platforms when the review offers
 * them, every other answer the owner gave, saved or not, kept; any other draft as it is.
 * Input: the draft on screen and the onboarding view. Output: the draft the form shows. */
export function withProposed(draft: ProjectDraft, view: OnboardingView): ProjectDraft {
  if (!offerProposed(view) || !view.proposed) return draft;
  const { about, tech_stack, platforms } = view.proposed;
  return { ...draft, about, stack: [...tech_stack], platforms: [...platforms] };
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
    case "checking": return { word: "Checking", tone: "info", line: null };
    case "failed": return { word: "Failed", tone: "danger", line: displayText(state.reason) };
    default: return { word: "Not installed", tone: "warn", line: "Setup installs it." };
  }
}

/** One tool row for setup. Input: the dependency. Output: its name, what it is for and its state. */
export function dependencyView(dependency: Dependency): { title: string; about: string; word: string; tone: Tone; line: string | null } {
  return { title: displayText(dependency.label.trim() || dependency.name), about: DEPENDENCY_ABOUT[dependency.name] ?? "", ...stateView(dependency.state) };
}

/** Badge word, tone and detail line for a default-pack entry state.
 * Inputs: the state and, for an installed entry, why the engine refused its hooks.
 * Output: the fixed word, its tone and one line or null. */
export function packStateView(state: PackState, hooksRefused: string | null = null): { word: string; tone: Tone; line: string | null } {
  const hooks = hooksRefused ? `Hooks not added: ${displayText(hooksRefused)}` : null;
  switch (state.state) {
    case "on": return { word: "On", tone: "ok", line: hooks };
    case "off": return { word: "Off", tone: "neutral", line: hooks };
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

const ENTRY_NAMES: Record<string, string> = { graphify: "Graphify skill for agents" };

/** The name a pack entry shows, kept apart from the tool of the same name: the graphify entry
 * reads "Graphify skill for agents" beside the "Graphify command" tool. Input: the entry.
 * Output: the name as plain text. */
export function entryName(entry: PackEntry): string {
  return displayText(Object.hasOwn(ENTRY_NAMES, entry.id) ? ENTRY_NAMES[entry.id] : entry.id);
}

/** The icon a pack entry shows. Input: the entry. Output: server for an MCP server, blocks for a skill. */
export function entryIcon(entry: PackEntry): IconName {
  return entry.kind === "mcp_server" ? "server" : "blocks";
}

export const PACK_GROUPS: { group: PackGroup; title: string; about: string }[] = [
  { group: "graph", title: "Code graph", about: "Builds each project's code graph with the Graphify command and gives agents the Graphify skill. Untick it to set projects up without a graph." },
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

/** The pack entries ticked before the owner changes anything: the ones on by default that are not
 * installed or installing yet, so Setup never offers to install an entry again.
 * Input: the pack. Output: their ids in pack order. */
export function packTicks(pack: PackEntry[]): string[] {
  return pack.filter((entry) => entry.on_by_default && (entry.state.state === "available" || entry.state.state === "failed")).map((entry) => entry.id);
}

/** Names the ticked MCP servers that continuing approves.
 * Inputs: the pack and the ticked ids. Output: one sentence, or null when none is ticked. */
export function approvalLine(pack: PackEntry[], ticked: string[]): string | null {
  const servers = pack.filter((entry) => entry.kind === "mcp_server" && ticked.includes(entry.id)).map(entryName);
  if (servers.length === 0) return null;
  return `Continuing lets your agents run these MCP servers: ${servers.join(", ")}. You can turn any of them off later.`;
}

/** Tells whether the engine is still checking any tool's version, so setup reads again.
 * Input: the status. Output: true while a tool shows Checking. */
export function toolsChecking(status: SetupStatus): boolean {
  return status.dependencies.some((item) => item.state.state === "checking");
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
    ...status.pack.filter((item) => ticked.includes(item.id)).map((item) => item.state.state)];
  const settledStates = new Set(["found", "installed", "on", "off", "failed"]);
  return { done: states.filter((state) => settledStates.has(state)).length, total: states.length, failed: states.filter((state) => state === "failed").length };
}

/** What the setup screen knows from its reads: the setup the loader last read, the newest setup
 * to show, how many reads failed in a row since, and the last failure's sentence. */
export interface SetupReads {
  seen: SetupStatus | null;
  latest: SetupStatus | null;
  failures: number;
  line: string;
}

export const SETUP_READS: SetupReads = { seen: null, latest: null, failures: 0, line: "" };

/** Folds one setup read into what the screen knows. A newly read setup replaces the shown one and
 * clears the failures; a read in flight or the loader showing its last read again changes nothing;
 * a failed read keeps the shown setup, counts and says why. Inputs: what the screen knew and the
 * read. Output: what it knows now, a new object whenever the read changed it. */
export function foldSetupRead(known: SetupReads, read: Loaded<SetupStatus>): SetupReads {
  if (read.kind === "loading") return known;
  if (read.kind === "ok") return read.value === known.seen ? known : { seen: read.value, latest: read.value, failures: 0, line: "" };
  const reason = read.kind === "error" ? read.message : "Tool setup isn't in this build.";
  return { ...known, failures: known.failures + 1, line: `Couldn't refresh setup progress: ${reason}` };
}

/** How long the setup screen waits before reading again, doubling after each failed read up to
 * `SETUP_POLL_MAX_MS`. Input: the failed reads in a row. Output: the wait in milliseconds. */
export function setupPollMs(failures: number): number {
  return Math.min(SETUP_POLL_MS * 2 ** Math.min(Math.max(failures, 0), 5), SETUP_POLL_MAX_MS);
}

const TOOLS: DependencyName[] = ["tmux", "uv", "graphify"];
const TOOL_PRESENT = new Set(["found", "installed"]);
const TOOLS_NEED = "Live terminals and the team graph need tmux; setting up a project needs graphify.";

/** Joins tool names the way the setup panel says them: "tmux", "tmux and uv", "tmux, uv and graphify". */
function namesLine(names: string[]): string {
  return names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** Works out the Install tools item from the setup read: done once tmux, uv and graphify are
 * each found or installed; the default pack never blocks it. While the read or a tool is still
 * checking it is not done and says so without an error. Input: the setup read.
 * Output: whether it is done and its one-line body. */
export function toolsStep(tools: Loaded<SetupStatus>): { done: boolean; body: string } {
  const checking = { done: false, body: "Checking tmux, uv and graphify on this computer." };
  if (tools.kind === "loading") return checking;
  if (tools.kind === "not_ready") return { done: false, body: "Tool setup isn't in this build yet." };
  if (tools.kind === "error") return { done: false, body: "Couldn't check tmux, uv and graphify. Open setup to try again." };
  const states = TOOLS.map((name) => tools.value.dependencies.find((item) => item.name === name)?.state.state);
  const missing = TOOLS.filter((_, index) => !TOOL_PRESENT.has(states[index] ?? "missing"));
  if (missing.length === 0) return { done: true, body: "tmux, uv and graphify are installed." };
  if (toolsChecking(tools.value)) return checking;
  if (states.includes("installing")) return { done: false, body: "Setup is installing the tools. This updates when it finishes." };
  return { done: false, body: `${namesLine(missing)} ${missing.length === 1 ? "isn't" : "aren't"} installed. ${TOOLS_NEED}` };
}
