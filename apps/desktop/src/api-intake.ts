import { classifyFailure, plainError, type Result } from "./api";
import { failureCopy } from "./api-projects";
import {
  addExistingFolder as addFolder, cloneRepo as clone, folderSize, foundProjects, githubRepos, githubSignIn, githubSignOut, githubStatus, githubToken,
  githubUseCli, onboarding, projectsHome, removePackEntry as removeEntry, runSetup as startSetup, setPackEntry as switchEntry,
  setProjectsHome as saveHome, setupStatus, startOnboarding as startMapping,
} from "./engine-client";
import type {
  AddFolderMode, CloneReceipt, FolderSize, FoundProject, GitHubDeviceCode, GitHubRepoPage, GitHubStatus, IntentReceipt, OnboardingView, ProjectsHome,
  SetupStatus,
} from "./engine-client";

export const FOUND_PROJECTS_MAX = 500;
export const GITHUB_TOKEN_CHARS_MAX = 255;
export const SETUP_PACK_MAX = 64;

const COPY: Record<string, string> = {
  projects_home_unset: "Choose your projects folder first.",
  projects_home_path_invalid: "CrazyAgent can't use this folder. Choose a folder you can write to.",
  projects_home_unwritable: "CrazyAgent can't write to this folder. Choose a folder you can write to.",
  home_source_invalid: "Choose the folder again with Choose folder.",
  folder_size_path_invalid: "Choose the folder again with Choose folder.",
  folder_missing: "That folder isn't there anymore. Choose it again.",
  folder_not_a_directory: "Choose a folder, not a file.",
  folder_inside_home: "This folder is already in your projects folder. It shows on Home.",
  folder_contains_home: "This folder holds your projects folder. Choose one project's folder.",
  folder_name_taken: "Your projects folder already has a folder with this name. Rename one of them first.",
  folder_move_failed: "Couldn't move the folder. Nothing was changed. Try again.",
  folder_copy_failed: "Couldn't copy the folder. The original is untouched. Try again.",
  onboarding_path_invalid: "This folder can't be set up. Check it's still in your projects folder.",
  onboarding_running: "This project is already being set up.",
  onboarding_missing: "This project isn't being set up anymore. It was reloaded.",
  github_signed_out: "Sign in with GitHub first.",
  github_token_blank: "Paste a GitHub token.",
  github_token_invalid: "GitHub didn't accept this token. Check it and try again.",
  github_device_expired: "The GitHub code expired. Start again.",
  github_device_denied: "The sign-in was cancelled on GitHub. Start again.",
  github_cli_unavailable: "The GitHub CLI isn't installed or isn't signed in on this Mac.",
  github_unreachable: "Couldn't reach GitHub. Check your connection and try again.",
  github_client_missing: "Sign in with GitHub isn't set up in this build yet. Paste a token or use the GitHub CLI.",
  github_rate_limited: "GitHub is limiting requests right now. Try again in a few minutes.",
  github_page_invalid: "That page of repositories doesn't exist.",
  github_failed: "Something went wrong talking to GitHub. Try again.",
  repo_missing: "This repository isn't on your GitHub account anymore.",
  repo_already_cloned: "This repository is already in your projects folder.",
  clone_failed: "Couldn't clone this repository. Nothing was kept. Try again.",
  setup_busy: "Setup is already running. Wait for it to finish.",
  setup_failed: "Something went wrong during setup.",
  setup_outcome_unknown: "We couldn't confirm what setup did. Check the list again.",
  setup_owner_unavailable: "CrazyAgent's engine isn't running.",
  setup_store_unavailable: "The app's data folder can't be written.",
  pack_unknown: "One of the picked items isn't in the pack anymore. The list was reloaded.",
  pack_required: "This entry is always on and can't be removed.",
  pack_not_installed: "This entry isn't installed yet. Run setup first.",
  pack_id_invalid: "One of the picked items isn't in the pack anymore. The list was reloaded.",
  pack_too_many: "Pick at most 64 entries.",
};

/** Plain sentence for one intake, GitHub or setup failure code.
 * Input: the code. Output: the intake copy, then the project and app copy, then the fallback. */
export function intakeCopy(code: string): string {
  return COPY[code] ?? failureCopy(code);
}

function failure<T>(code: string): Result<T> {
  return { kind: "error", code, message: intakeCopy(code) };
}

function invalid<T>(): Result<T> {
  return { kind: "error", code: "invalid_response", message: plainError("invalid_response") };
}

async function wrap<T>(action: () => Promise<T>): Promise<Result<T>> {
  try {
    return { kind: "ok", value: await action() };
  } catch (error) {
    const sorted = classifyFailure(error);
    return sorted.kind === "error" ? { ...sorted, message: intakeCopy(sorted.code) } : sorted;
  }
}

/** Reads the projects folder and whether the engine watches it for new folders.
 * Input: none. Output: the folder, null before the owner chose one. */
export function loadProjectsHome(): Promise<Result<ProjectsHome>> {
  return wrap(projectsHome);
}

/** Sets the projects folder. Nothing already on disk moves.
 * Inputs: an absolute folder path and a generation. Output: the engine receipt. */
export function setProjectsHome(path: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => saveHome(path, generation));
}

/** Reads every folder in the projects folder with its state: found, setting up, ready or missing.
 * Input: none. Output: at most FOUND_PROJECTS_MAX folders in engine order. */
export async function loadFoundProjects(): Promise<Result<FoundProject[]>> {
  const result = await wrap(foundProjects);
  if (result.kind !== "ok") return result;
  if (!Array.isArray(result.value)) return invalid();
  return { kind: "ok", value: result.value.slice(0, FOUND_PROJECTS_MAX) };
}

/** Measures a folder before it is copied and says whether its code is hosted on GitHub.
 * Input: an absolute folder path. Output: its size in bytes and the GitHub owner/name, if any. */
export function loadFolderSize(path: string): Promise<Result<FolderSize>> {
  return wrap(() => folderSize(path));
}

/** Moves or copies a folder into the projects folder, where it then shows as found.
 * Inputs: the absolute source folder, move or copy, and a generation. Output: the receipt. */
export function addExistingFolder(source: string, mode: AddFolderMode, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => addFolder(source, mode, generation));
}

/** Starts setting up a found folder: the graph builds, then a mapping agent on the pool fills in
 * the project details. Inputs: the folder path, the pool id and a generation. Output: the receipt. */
export function startOnboarding(path: string, pool: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => startMapping(path, pool, generation));
}

/** Reads a project's setup progress, the mapping agent's draft, what it couldn't find and the
 * overview it wrote. Input: the project id. Output: the onboarding view. */
export function loadOnboarding(id: string): Promise<Result<OnboardingView>> {
  return wrap(() => onboarding(id));
}

/** Starts Sign in with GitHub through the device flow; the engine then waits for approval.
 * Output: the code the owner types on GitHub, its page and lifetime. */
export function gitHubSignIn(): Promise<Result<GitHubDeviceCode>> {
  return wrap(githubSignIn);
}

/** Sends a pasted GitHub token once to the engine, which checks it and keeps it in the keychain.
 * The token is checked here first and never returned or kept. Input: the token.
 * Output: the status after the engine's check, or a plain refusal. */
export function gitHubToken(token: string): Promise<Result<GitHubStatus>> {
  const value = token.trim();
  if (!value) return Promise.resolve(failure("github_token_blank"));
  if (value.length > GITHUB_TOKEN_CHARS_MAX || !/^[\x21-\x7e]+$/u.test(value)) return Promise.resolve(failure("github_token_invalid"));
  return wrap(() => githubToken(value));
}

/** Reuses the GitHub CLI's sign-in on this Mac. Output: the new status. */
export function gitHubUseCli(): Promise<Result<GitHubStatus>> {
  return wrap(githubUseCli);
}

/** Signs CrazyAgent out of GitHub and deletes its keychain item. Output: the new status. */
export function gitHubSignOut(): Promise<Result<GitHubStatus>> {
  return wrap(githubSignOut);
}

/** Reads whether CrazyAgent is signed in to GitHub, as whom, how, and any plain problem.
 * Output: the status. */
export function loadGitHubStatus(): Promise<Result<GitHubStatus>> {
  return wrap(githubStatus);
}

/** Reads one page of the signed-in account's repositories.
 * Input: the page, counting from 1. Output: the repositories and the next page, if any. */
export async function loadGitHubRepos(page: number): Promise<Result<GitHubRepoPage>> {
  if (!Number.isSafeInteger(page) || page < 1) throw new Error(`GitHub repository page must count from 1, got ${page}`);
  const result = await wrap(() => githubRepos(page));
  if (result.kind !== "ok") return result;
  return Array.isArray(result.value?.repos) ? result : invalid();
}

/** Clones one repository into the projects folder, where it then shows as found.
 * Input: the owner/name. Output: the repository and the folder it landed in. */
export function cloneRepo(fullName: string): Promise<Result<CloneReceipt>> {
  return wrap(() => clone(fullName));
}

/** Reads the tools setup installs (tmux, uv, graphify), the default pack with their states and
 * whether setup is running. Output: the setup status. */
export function loadSetupStatus(): Promise<Result<SetupStatus>> {
  return wrap(setupStatus);
}

/** Installs missing tools and the ticked pack entries. Continuing with MCP servers ticked records
 * the owner's approval of them. Input: the ticked pack ids. Output: the status as the run starts. */
export function runSetup(pack: string[]): Promise<Result<SetupStatus>> {
  if (pack.length > SETUP_PACK_MAX) throw new Error(`setup pack holds at most ${SETUP_PACK_MAX} entries`);
  return wrap(() => startSetup(pack));
}

/** Switches one installed pack entry on or off for every project.
 * Inputs: its id and the state. Output: the new setup status. */
export function setPackEntry(id: string, on: boolean): Promise<Result<SetupStatus>> {
  return wrap(() => switchEntry(id, on));
}

/** Removes one installed pack entry and the skill or server it added.
 * Input: its id. Output: the new setup status. */
export function removePackEntry(id: string): Promise<Result<SetupStatus>> {
  return wrap(() => removeEntry(id));
}
