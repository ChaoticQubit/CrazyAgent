import { classifyFailure, nextGeneration, plainError, type Result } from "./api";
import { failureCopy } from "./api-projects";
import {
  addExistingFolder as addFolder, cloneRepo as clone, folderSize, foundProjects, githubRepos, githubSignIn, githubSignOut, githubStatus, githubToken,
  githubUseCli, moveProjectsHome as moveHome, onboarding, projectsHome, removePackEntry as removeEntry, runSetup as startSetup, setPackEntry as switchEntry,
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
  projects_home_not_set: "Choose your projects folder first.",
  projects_home_path_invalid: "Choose the folder again with Choose folder.",
  projects_home_missing: "That folder doesn't exist. Choose another one.",
  projects_home_not_folder: "Choose a folder, not a file or a link.",
  projects_home_unsafe: "Choose a separate folder for your projects, not your home folder or a system folder.",
  projects_home_unreadable: "Your projects folder can't be read. Check its permissions.",
  projects_home_watch_failed: "CrazyAgent can't watch your projects folder right now. New folders show up when the app restarts.",
  home_source_invalid: "Choose the folder again with Choose folder.",
  home_source_missing: "That folder isn't there anymore. Choose it again.",
  home_source_not_folder: "Choose a folder, not a file or a link.",
  home_source_in_home: "This folder is already in your projects folder. It shows on Home.",
  home_source_unsafe: "This folder holds your home folder or the app's data, so it can't be added.",
  home_source_hidden: "Rename the folder so it has a plain name first.",
  home_source_name: "Rename the folder so it has a plain name first.",
  home_destination_exists: "Your projects folder already has a folder with this name. Rename one of them first.",
  home_copy_special_file: "The folder holds a device or pipe file that can't be copied.",
  home_copy_too_large: "The folder has too many files or levels to copy. Move it instead.",
  home_copy_failed: "Couldn't copy the folder. The original is untouched. Try again.",
  home_copy_unverified: "The copy didn't match the original, so the original was kept.",
  home_move_failed: "Couldn't move the folder. Nothing was removed. Try again.",
  home_move_source_left: "The folder was added, but part of the original couldn't be removed. Check its old place.",
  home_move_same: "This is already your projects folder.",
  home_move_nested: "Choose a projects folder that isn't inside the current one.",
  home_move_partial: "Some projects were moved and couldn't be moved back. Check both folders.",
  home_move_interrupted: "Moving your projects stopped part way. Check both folders before trying again.",
  home_add_unverified: "The folder didn't arrive in your projects folder. Check it and try again.",
  home_add_interrupted: "Adding the folder stopped part way. Check your projects folder before trying again.",
  home_record_invalid: "CrazyAgent's record of your projects folder can't be read.",
  folder_size_path_invalid: "Choose the folder again with Choose folder.",
  folder_size_unsafe: "That folder can't be measured.",
  folder_size_unreadable: "Some files in that folder can't be read.",
  folder_size_timeout: "This folder is very large. Move it instead of copying it.",
  folder_size_too_large: "This folder is very large. Move it instead of copying it.",
  folder_size_interrupted: "Measuring the folder stopped. Try again.",
  crazyagent_folder_invalid: "The project's .crazyagent folder isn't a plain folder. Rename or remove it, then try again.",
  crazyagent_folder_unwritable: "CrazyAgent can't write the project's .crazyagent folder. Check the folder's permissions.",
  git_exclude_invalid: "The project's git exclude file can't be updated. Check .git/info/exclude.",
  owner_home_unknown: "Your home folder couldn't be found.",
  onboarding_folder_missing: "This folder isn't in your projects folder anymore.",
  onboarding_folder_unsafe: "This folder can't be set up. Check it's a plain folder in your projects folder.",
  onboarding_layout_incomplete: "Setup didn't finish preparing this project. Try again.",
  onboarding_path_invalid: "This folder can't be set up. Check it's still in your projects folder.",
  onboarding_path_not_canonical: "This folder can't be set up from this path. Reload Home and try again.",
  onboarding_project_mismatch: "Another project already uses this folder's name. Rename the folder first.",
  onboarding_step_invalid: "This project's setup record can't be read. Start setup again.",
  project_exists: "A project with this name already exists. Rename the folder first.",
  github_not_signed_in: "Sign in to GitHub first.",
  github_token_blank: "Paste a GitHub token.",
  github_token_invalid: "GitHub didn't accept this token. Check it and try again.",
  github_token_refused: "GitHub didn't accept the token. Sign in again.",
  github_sign_in_expired: "The GitHub code expired before it was entered. Start again.",
  github_sign_in_denied: "The sign-in was denied on GitHub. Start again.",
  github_sign_in_cancelled: "The sign-in was cancelled. Start again.",
  github_sign_in_failed: "GitHub sign-in failed. Try again.",
  github_device_flow_refused: "GitHub refused Sign in with GitHub for this app. Paste a token or use the GitHub CLI.",
  github_client_id_not_configured: "Sign in with GitHub isn't set up in this build yet. Paste a token or use the GitHub CLI.",
  github_client_id_refused: "GitHub didn't accept this app. Paste a token or use the GitHub CLI.",
  github_cli_missing: "The GitHub CLI (gh) isn't installed on this computer.",
  github_cli_not_signed_in: "The GitHub CLI isn't signed in. Run gh auth login first.",
  github_unreachable: "Couldn't reach GitHub. Check your connection and try again.",
  github_reply_invalid: "GitHub sent a reply CrazyAgent couldn't read. Try again.",
  github_config_unsafe: "CrazyAgent's GitHub settings file can't be read safely. Sign in again.",
  github_config_invalid: "CrazyAgent's GitHub settings file can't be read safely. Sign in again.",
  github_state_invalid: "The saved GitHub sign-in can't be read. Sign in again.",
  github_state_unavailable: "The saved GitHub sign-in can't be read. Sign in again.",
  credential_missing: "The saved GitHub sign-in is gone from your keychain. Sign in again.",
  credential_store_unavailable: "Your system keychain can't be used right now.",
  github_repo_name_invalid: "That repository name isn't valid. Pick it from the list.",
  github_repo_page_invalid: "That page of repositories doesn't exist.",
  clone_destination_exists: "Your projects folder already has a folder with this repository's name.",
  github_clone_failed: "Couldn't clone this repository. Nothing was kept. Try again.",
  github_clone_timed_out: "Cloning took too long and was stopped. Nothing was kept. Try again.",
  github_outcome_unknown: "We couldn't confirm what GitHub did. Check the list again.",
  github_owner_unavailable: "CrazyAgent's engine isn't running.",
  github_request_bounds: "That request was too large for the engine. Try again.",
  github_reply_bounds: "GitHub's reply was too large to show. Try again.",
  github_request_invalid: "The engine couldn't read that request. Reload and try again.",
  github_token_frame_invalid: "The token couldn't be sent to the engine safely. Paste it again.",
  github_token_frame_mismatch: "The token couldn't be sent to the engine safely. Paste it again.",
  github_failed: "Something went wrong talking to GitHub. Try again.",
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
  pack_name_taken: "A skill or server with this name already exists. Remove it first, then try again.",
  pack_commit_mismatch: "The source doesn't have the pinned version, so nothing was installed.",
  pack_fetch_failed: "The source couldn't be downloaded. Check your connection and try again.",
  pack_no_skills: "The source has no skill folder with a SKILL.md file, so nothing was installed.",
  pack_server_invalid: "This server's start command isn't valid, so it wasn't installed.",
  pack_server_command_missing: "The program this server needs isn't installed on this computer.",
  pack_bundled_unavailable: "This entry isn't included in this build.",
  project_missing: "This project isn't in CrazyAgent anymore. The list was reloaded.",
  setup_readonly: "Setup can't change anything while CrazyAgent is read-only.",
  setup_checksum_mismatch: "The download didn't match its checksum, so it wasn't installed.",
  setup_download_failed: "The download failed. Check your connection and try again.",
  setup_homebrew_missing: "Homebrew isn't installed. Install tmux yourself, then run setup again.",
  setup_install_failed: "The installer didn't finish. Try again.",
  setup_timeout: "The install took too long and was stopped. Try again.",
  setup_version_mismatch: "A different version from the pinned one is installed.",
  setup_version_unreadable: "The installed program didn't report its version.",
  setup_uv_missing: "uv is needed to install graphify and isn't installed.",
  setup_pin_invalid: "The pinned source isn't valid, so nothing was installed.",
  setup_tool_unsupported: "This tool can't be installed on this computer yet.",
  setup_request_bounds: "Setup couldn't take this request. Pick fewer entries and try again.",
  setup_request_invalid: "Setup couldn't read this request. Reload and try again.",
  setup_reply_bounds: "Setup's reply was too large to show. Reload and try again.",
  owner_outcome_unknown_reconcile_same_identity: "CrazyAgent didn't answer in time. Try again.",
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

/** Moves every project folder into a new projects folder, after the owner saw the size.
 * Inputs: the new absolute folder and a generation. Output: the engine receipt. */
export function moveProjectsHome(path: string, generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => moveHome(path, generation));
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
  return wrap(() => startSetup(pack, nextGeneration()));
}

/** Switches one installed pack entry on or off for every project.
 * Inputs: its id and the state. Output: the new setup status. */
export function setPackEntry(id: string, on: boolean): Promise<Result<SetupStatus>> {
  return wrap(() => switchEntry(id, on, nextGeneration()));
}

/** Removes one installed pack entry and the skill or server it added.
 * Input: its id. Output: the new setup status. */
export function removePackEntry(id: string): Promise<Result<SetupStatus>> {
  return wrap(() => removeEntry(id, nextGeneration()));
}
