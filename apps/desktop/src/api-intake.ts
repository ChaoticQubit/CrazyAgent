import { call, classifyFailure, plainError, type Result } from "./api";
import { failureCopy } from "./api-projects";
import { cloneRepo as clone, githubRepos, githubSignIn, githubStatus, githubToken, githubUseCli } from "./engine-client";
import type {
  AddFolderMode, CloneReceipt, FolderSize, FoundProject, GitHubDeviceCode, GitHubRepoPage, GitHubStatus, IntentReceipt, OnboardingStarted, OnboardingView,
  ProjectsHome, SetupStatus,
} from "./engine-client";

export const FOUND_PROJECTS_MAX = 500;
export const GITHUB_TOKEN_CHARS_MAX = 255;
export const SETUP_PACK_MAX = 64;

const COPY: Record<string, string> = {
  projects_home_unset: "Choose your projects folder first.",
  projects_home_invalid: "CrazyAgent can't use this folder. Choose a folder you can write to.",
  projects_home_unwritable: "CrazyAgent can't write to this folder. Choose a folder you can write to.",
  folder_missing: "That folder isn't there anymore. Choose it again.",
  folder_not_a_directory: "Choose a folder, not a file.",
  folder_inside_home: "This folder is already in your projects folder. It shows on Home.",
  folder_contains_home: "This folder holds your projects folder. Choose one project's folder.",
  folder_name_taken: "Your projects folder already has a folder with this name. Rename one of them first.",
  folder_move_failed: "Couldn't move the folder. Nothing was changed. Try again.",
  folder_copy_failed: "Couldn't copy the folder. The original is untouched. Try again.",
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
  repo_missing: "This repository isn't on your GitHub account anymore.",
  repo_already_cloned: "This repository is already in your projects folder.",
  clone_failed: "Couldn't clone this repository. Nothing was kept. Try again.",
  setup_running: "Setup is already running. Wait for it to finish.",
  pack_entry_unknown: "One of the picked items isn't in the pack anymore. The list was reloaded.",
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
  return call<ProjectsHome>("desktop_projects_home");
}

/** Sets the projects folder. Nothing already on disk moves.
 * Inputs: an absolute folder path and a generation. Output: the engine receipt. */
export function setProjectsHome(path: string, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_set_projects_home", { path, generation });
}

/** Reads every folder in the projects folder with its state: found, setting up, ready or missing.
 * Input: none. Output: at most FOUND_PROJECTS_MAX folders in engine order. */
export async function loadFoundProjects(): Promise<Result<FoundProject[]>> {
  const result = await call<FoundProject[]>("desktop_found_projects");
  if (result.kind !== "ok") return result;
  if (!Array.isArray(result.value)) return invalid();
  return { kind: "ok", value: result.value.slice(0, FOUND_PROJECTS_MAX) };
}

/** Measures a folder before it is copied and says whether its code is hosted on GitHub.
 * Input: an absolute folder path. Output: its size in bytes and the GitHub owner/name, if any. */
export function loadFolderSize(path: string): Promise<Result<FolderSize>> {
  return call<FolderSize>("desktop_folder_size", { path });
}

/** Moves or copies a folder into the projects folder, where it then shows as found.
 * Inputs: the absolute source folder, move or copy, and a generation. Output: the receipt. */
export function addExistingFolder(source: string, mode: AddFolderMode, generation: number): Promise<Result<IntentReceipt>> {
  return call<IntentReceipt>("desktop_add_existing_folder", { source, mode, generation });
}

/** Starts setting up a found folder: the graph builds, then a mapping agent on the pool fills in
 * the project details. Inputs: the folder path, the pool id and a generation.
 * Output: the receipt with the new project's id. */
export function startOnboarding(path: string, pool: string, generation: number): Promise<Result<OnboardingStarted>> {
  return call<OnboardingStarted>("desktop_start_onboarding", { path, pool, generation });
}

/** Reads a project's setup progress, the mapping agent's draft and what it couldn't find.
 * Input: the project id. Output: the onboarding view. */
export function loadOnboarding(id: string): Promise<Result<OnboardingView>> {
  return call<OnboardingView>("desktop_onboarding", { id });
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

/** Reads the tools setup installs (tmux, uv, graphify) and the default pack with their states.
 * Output: the setup status. */
export function loadSetupStatus(): Promise<Result<SetupStatus>> {
  return call<SetupStatus>("desktop_setup_status");
}

/** Installs missing tools and the ticked pack entries. Continuing with MCP servers ticked records
 * the owner's approval of them. Inputs: the ticked pack ids and a generation. Output: the receipt. */
export function runSetup(pack: string[], generation: number): Promise<Result<IntentReceipt>> {
  if (pack.length > SETUP_PACK_MAX) throw new Error(`setup pack holds at most ${SETUP_PACK_MAX} entries`);
  return call<IntentReceipt>("desktop_run_setup", { pack, generation });
}
