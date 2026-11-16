import { classifyFailure, namedFailure, nextGeneration, plainError, type Result } from "./api";
import { formatBytes } from "./screens/new-project/wizard";
import { failureCopy } from "./api-projects";
import { githubPublishPrompt, githubPublishPromptSet } from "./engine-client";
import type { GitHubPromptAnswer } from "./engine-client";
import {
  addExistingFolder as addFolder, cloneRepo as clone, folderSize, foundProjects, githubOwners, githubPublishApprove, githubPublishContinue, githubPublishDiscard, githubPublishExclude, githubPublishRecheck, githubPublishStart, githubPublishStatus, githubRepos, githubSignIn, githubSignOut, githubStatus, githubToken,
  githubUseCli, icloudCancel, icloudDownload, icloudProgress, intakeDismiss, intakeStart, intakeStatus, intakeStop, moveProjectsHome as moveHome, onboarding, projectsHome, removePackEntry as removeEntry, runSetup as startSetup, installDependency as startInstall, setPackEntry as switchEntry,
  fixToolPermissions as startFix, setRefusedTool as saveRefusedTool,
  setProjectsHome as saveHome, setupStatus, startOnboarding as startMapping,
} from "./engine-client";
import type {
  AddFolderMode, AddOverride, CloneReceipt, FolderSize, FoundProject, GitHubDeviceCode, GitHubOwners, GitHubPublishJob, GitHubPublishLeft, GitHubPublishPlan, GitHubRepoPage, GitHubStatus, IcloudProgress, IntakeJob,
  IntentReceipt, OnboardingView,
  DependencyName, ProjectsHome, RefusedTool, SetupStatus,
} from "./engine-client";
import { githubRuleDefaults, githubRuleDefaultsSet, githubRules, githubRuleSet } from "./engine-client";
import type { GitHubRepoRules } from "./engine-client";

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
  home_copy_too_large: "The folder has too many files or levels to copy. Use it where it is instead.",
  home_copy_failed: "Couldn't copy the folder. The original is untouched. Try again.",
  home_copy_unverified: "The copy didn't match the original, so the original was kept.",
  home_copy_linked_checkout: "This folder is a git worktree or submodule of another repository, so a copy would still share it. Use it where it is, or copy the main repository folder.",
  home_move_failed: "Couldn't move the folder. Nothing was removed. Try again.",
  home_move_source_left: "The folder was added, but part of the original couldn't be removed. What's left there may be newer than the copy, so check it before you delete anything.",
  home_move_same: "This is already your projects folder.",
  home_move_nested: "Choose a projects folder that isn't inside the current one.",
  home_move_partial: "Some projects were moved and couldn't be moved back. Check both folders.",
  home_move_worktrees: "This folder has git worktrees linked to it, so moving it would break them. Use it where it is or copy it.",
  home_move_project: "This folder holds or sits inside a project you already added. Use it where it is or copy it.",
  home_move_interrupted: "Moving your projects stopped part way. Check both folders before trying again.",
  home_move_too_many: "Your projects folder holds more than 512 folders, more than CrazyAgent moves at once. Move some out, then try again. Nothing was moved.",
  home_move_in_progress: "Your projects folder is still being moved. Try again when it finishes.",
  home_move_delete_in_progress: "A project in your projects folder is being deleted. Try again when it finishes. Nothing was moved.",
  home_move_agent_work: "Agents still have work in projects in this folder, and moving it would cut them off from it. Land their changes and remove those agents first. An agent whose change you discarded can be removed 7 days later, when the change can no longer be restored. Nothing was moved.",
  home_move_changes_restorable: "A change you discarded in a project in this folder can still be restored, and moving the folder would break that. You can move it 7 days after the discard, when the change can no longer be restored. Nothing was moved.",
  home_move_research_running: "A research job is running in a project in this folder, and moving it would cut the job off. Wait for the job to finish or cancel it, then try again. Nothing was moved.",
  home_folder_name_invalid: "Give the project a name with letters or digits, and no folder by that name.",
  home_new_project_failed: "The project folder couldn't be created. Nothing was kept.",
  home_new_project_interrupted: "Setting up the project stopped part way. Nothing was kept; set it up again from New Project.",
  setup_cannot_stop: "The project is being saved, so it can't stop now. Wait for it to finish on Home.",
  project_folder_unavailable: "Documents can't be copied into a project on this computer yet. Create it without documents.",
  home_add_unverified: "The folder didn't arrive in your projects folder. Check it and try again.",
  home_add_interrupted: "Adding the folder stopped part way. Check your projects folder before trying again.",
  home_add_cancelled: "Adding the folder stopped. Nothing was moved or copied.",
  home_add_in_progress: "A folder is being added to your projects folder. Try again when it finishes.",
  job_busy: "Another job is running. Wait for it to finish on Home.",
  intake_failed: "Adding the folder failed. Check your projects folder before trying again.",
  home_override_invalid: "CrazyAgent couldn't match what you agreed to with this folder. Choose the folder again.",
  home_record_invalid: "CrazyAgent's record of your projects folder can't be read.",
  home_icloud_not_downloaded: "Some files are still only in iCloud, so the folder wasn't added. Nothing was removed. Download them and try again.",
  home_permission_denied: "macOS didn't let CrazyAgent read or move this folder. Allow CrazyAgent in System Settings > Privacy & Security > Files and Folders, check the folder's permissions, then try again. Nothing was removed.",
  icloud_download_busy: "Another iCloud download is running. Wait for it to finish.",
  icloud_folder_missing: "That folder isn't there anymore. Choose it again.",
  icloud_folder_unsafe: "That folder can't be downloaded.",
  icloud_folder_unreadable: "Some files in that folder can't be read.",
  icloud_path_invalid: "Choose the folder again with Choose folder.",
  icloud_outcome_unknown: "CrazyAgent couldn't confirm the iCloud download. Nothing was moved or copied. Try again.",
  icloud_owner_unavailable: "CrazyAgent couldn't confirm the iCloud download. Nothing was moved or copied. Try again.",
  folder_size_path_invalid: "Choose the folder again with Choose folder.",
  folder_size_unsafe: "That folder can't be measured.",
  folder_size_unreadable: "Some files in that folder can't be read.",
  folder_size_timeout: "This folder is very large. Use it where it is instead of copying it.",
  folder_size_too_large: "This folder is very large. Use it where it is instead of copying it.",
  folder_size_interrupted: "Measuring the folder stopped. Try again.",
  crazyagent_folder_invalid: "The project's .crazyagent folder isn't a plain folder. Rename or remove it, then try again.",
  crazyagent_folder_unwritable: "CrazyAgent can't write the project's .crazyagent folder. Check the folder's permissions.",
  git_exclude_invalid: "The project's git exclude file can't be updated: .git/info must be a plain folder and .git/info/exclude a plain file under 1 MiB, not links. Fix them, then try again.",
  owner_home_unknown: "Your home folder couldn't be found.",
  onboarding_folder_missing: "This folder isn't there anymore. Choose it again.",
  onboarding_folder_unsafe: "This folder can't be set up. Choose a project's own folder, not your home folder or the app's data.",
  onboarding_instructions_unwritable: "This folder came with a .crazyagent/instructions.md that CrazyAgent can't label as Imported. Make that file and its .crazyagent folder writable, then start again.",
  onboarding_instructions_unreadable: "This folder came with a .crazyagent/instructions.md that CrazyAgent can't read: it is a link, too large or not plain text. Move or fix that file, then start again.",
  onboarding_layout_incomplete: "Setup didn't finish preparing this project. Try again.",
  onboarding_path_invalid: "This folder can't be set up. Choose it again with Choose folder.",
  onboarding_path_not_canonical: "This folder can't be set up from this path. Choose it again with Choose folder.",
  onboarding_project_mismatch: "Another project already uses this folder's name. Rename the folder first.",
  onboarding_step_invalid: "This project's setup record can't be read. Start setup again.",
  project_exists: "A project with this name already exists. Rename the folder first.",
  onboarding_project_ids_used: "Every id this folder's name can get, 99 in all, is taken by your projects or ones you removed. Rename the folder first.",
  project_id_removed: "A project you removed used this name, so it can't be used again. Choose another project name.",
  projects_removed_full: "CrazyAgent keeps a record of up to 100,000 removed projects and that record is full, so no project can be removed. Nothing was removed or moved to the Trash.",
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
  github_repo_name_unsupported: "CrazyAgent can't clone a repository whose name starts with a dot or dash, ends in .git or uses characters other than letters, digits, dots, dashes and underscores.",
  github_repo_page_invalid: "That page of repositories doesn't exist.",
  clone_destination_exists: "Your projects folder already has a folder with this repository's name.",
  github_clone_failed: "Couldn't clone this repository. Nothing was kept. Try again.",
  github_clone_timed_out: "Cloning took too long and was stopped. Nothing was kept. Try again.",
  github_publish_detached: "The project's git isn't on a branch. Check out a branch, then try again.",
  github_publish_draft_not_asked: "No agent drafted these lines, so CrazyAgent's standard lines are shown.",
  github_publish_exclude_invalid: "That file can't be left out. Check again.",
  github_publish_failed: "Publishing on GitHub failed. Try again.",
  github_publish_file_invalid: "A file publishing writes (.gitignore, SECURITY.md or the .github folder) is a link or not a plain file. Fix it, then try again.",
  github_publish_gitignore_invalid: "The project's .gitignore can't be read or changed safely. Fix it, then try again.",
  github_publish_head_moved: "The project's git changed while it was being published. Try again.",
  github_publish_hooks_set: "The project's git runs hooks, so CrazyAgent won't commit or push it. Remove the hooks, then try again.",
  github_publish_ignore_invalid: "These lines can't go into .gitignore. Keep one pattern per line, without control characters.",
  github_publish_in_progress: "This project already has a publish. Finish or discard it first.",
  github_publish_not_plain_repo: "The project's .git is a link or a file, not a plain folder, so CrazyAgent won't publish it.",
  github_publish_not_started: "This project has no publish to go on with. Start it again.",
  github_publish_on_github: "This project is already on GitHub.",
  github_publish_other_host: "This project's git already points at another host, so CrazyAgent won't publish it on GitHub.",
  github_publish_push_failed: "Git couldn't push to GitHub. Try again.",
  github_publish_push_timed_out: "Pushing to GitHub took too long and was stopped. Try again.",
  github_publish_running: "This project is being published right now. Wait for it to finish.",
  github_publish_secret_in_history: "The secret is already in the project's git history, so leaving the file out doesn't remove it. Remove it from the history, then check again.",
  github_publish_step_invalid: "The publish moved on since this was shown. Look at it again.",
  github_publish_too_large: "This project is too large to publish from CrazyAgent: over 20,000 files or 200 MB.",
  github_publish_workflow_scope: "The GitHub sign-in can't push workflow files. Sign in to GitHub again, then try again.",
  github_repo_name_taken: "That repository name is taken on this GitHub account. Pick another name.",
  github_repo_create_refused: "GitHub refused to make the repository. Check that this account may create repositories.",
  github_repo_visibility_mismatch: "GitHub made the repository with a different visibility than asked. Check it on GitHub.",
  github_repo_description_invalid: "GitHub refused the repository description. Use at most 350 characters on one line.",
  github_git_missing: "Git isn't installed on this computer.",
  github_outcome_unknown: "We couldn't confirm what GitHub did. Check the list again.",
  github_owner_unavailable: "CrazyAgent's engine isn't running.",
  github_request_bounds: "That request was too large for the engine. Try again.",
  github_reply_bounds: "GitHub's reply was too large to show. Try again.",
  github_request_invalid: "The engine couldn't read that request. Reload and try again.",
  github_token_frame_invalid: "The token couldn't be sent to the engine safely. Paste it again.",
  github_token_frame_mismatch: "The token couldn't be sent to the engine safely. Paste it again.",
  github_failed: "Something went wrong talking to GitHub. Try again.",
  github_rules_not_published: "GitHub rules show here for a project CrazyAgent published on GitHub, once its publish is done.",
  github_rule_invalid: "This rule is a file in the project, so it can't be switched on GitHub.",
  github_rules_repo_mismatch: "This project is now published to another GitHub repository, so nothing was changed. Check its rules again.",
  github_repo_unreadable: "GitHub didn't show this repository. Check that it still exists and that your GitHub sign-in can see it.",
  github_publish_project_unknown: "CrazyAgent can't find this project. Reload and try again.",
  github_publish_unavailable: "CrazyAgent couldn't read its projects just now. Try again.",
  github_store_unavailable: "CrazyAgent couldn't find where it keeps its GitHub sign-in. Restart CrazyAgent and try again.",
  setup_busy: "Setup is already running. Wait for it to finish.",
  tool_fix_off: "Fixing permissions from CrazyAgent is turned off in Settings. Run the command shown instead.",
  tool_fix_not_needed: "This tool can run now, so nothing needed fixing.",
  tool_fix_unavailable: "Changing permissions can't fix why this tool is not used. Follow the line shown.",
  tool_fix_changed: "What blocks this tool changed since you looked, so nothing was changed. Check the row again.",
  tool_fix_refused: "CrazyAgent only fixes folders you own inside your home folder, never system, Homebrew, project or app data folders. Run the command yourself if you are sure.",
  tool_fix_too_large: "That folder is too large to fix from CrazyAgent. Run the command shown instead.",
  tool_fix_failed: "The permissions could not be changed. Run the command shown instead.",
  setup_failed: "Something went wrong during setup.",
  setup_outcome_unknown: "We couldn't confirm what setup did. Check the list again.",
  setup_owner_unavailable: "CrazyAgent's engine isn't running.",
  setup_store_unavailable: "The app's data folder can't be written.",
  pack_unknown: "One of the picked items isn't in the pack anymore. The list was reloaded.",
  pack_not_installed: "This entry isn't installed yet. Run setup first.",
  stale_revision: "The pack changed in another window, so this was not saved. The list was reloaded.",
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
  setup_homebrew_unsafe: "Another account can change Homebrew's files, so setup didn't run it. Install tmux yourself, then run setup again.",
  setup_install_failed: "The installer didn't finish. Try again.",
  setup_timeout: "The install took too long and was stopped. Try again.",
  setup_version_mismatch: "A different version from the pinned one is installed.",
  setup_version_unreadable: "The installed program didn't report its version.",
  setup_uv_missing: "uv is needed to install graphify and isn't installed.",
  setup_winget_missing: "winget is needed to install uv and isn't installed. Install App Installer from the Microsoft Store, then run setup again.",
  setup_pin_invalid: "The pinned source isn't valid, so nothing was installed.",
  setup_tool_unsupported: "This tool can't be installed on this computer yet.",
  setup_archive_unsafe: "The download held a file it shouldn't, so it wasn't installed.",
  setup_pin_unlocked: "Setup has no locked list of this tool's files, so it wasn't installed.",
  setup_local_app_data_missing: "Windows didn't say where your local app data folder is, so setup can't install here.",
  setup_platform_unsupported: "Setup can't install this on this system yet. Install it yourself, then check again.",
  agent_git_needs_tools: "Agent git needs the Command Line Tools: run xcode-select --install.",
  setup_request_bounds: "Setup couldn't take this request. Pick fewer entries and try again.",
  setup_request_invalid: "Setup couldn't read this request. Reload and try again.",
  setup_reply_bounds: "Setup's reply was too large to show. Reload and try again.",
  owner_outcome_unknown_reconcile_same_identity: "CrazyAgent didn't answer in time. Try again.",
};

/** The warnings the Move anyway and Copy anyway question shows, one per issue, in the owner's
 * words (DEC-115). Data loss lines come first wherever they are shown. */
export const ADD_WARNINGS = {
  close_apps: "Close editors, terminals and apps that use this folder first. Files saved while it moves can be lost.",
  old_place: "Anything that points at the old place (terminals, editors, scripts, shortcuts) stops working.",
  across_disks: "This copies every file to the other disk, then removes the original. It can take a long time and needs as much free space again. If the Mac sleeps, loses power or the disk fills while it copies, your original stays whole and a part copy may be left in your projects folder to delete. If it stops while it removes the original, the full copy stays in your projects folder (keep it) and part of the original is left.",
  same_disk: "This is a rename on the same disk; it is quick.",
  copy_all: "This copies every file into your projects folder. It can take a long time and needs as much free space again. If the Mac sleeps, loses power or the disk fills, the copy stops: your original stays, and a part copy may be left in your projects folder to delete.",
  large: "This folder is very large.",
  unmeasured: "CrazyAgent couldn't measure it.",
  too_many: "The folder is deeper or bigger than CrazyAgent checks, so the copy check can take a long time.",
  unreadable: "Some files can't be read. They can't be copied, so a move across disks keeps the original in place and fails rather than leave them behind.",
  unreadable_copy: "Some files can't be read. They can't be copied, so the copy fails rather than leave them behind.",
  worktrees: "Git worktrees linked to this folder stop working until you run git worktree repair in each. Their files stay, but git there fails until repaired.",
  linked_checkout: "The copy keeps this folder's link to another repository's git data. Where that link still reaches it, the copy and this folder share one index and current branch: switching branches, staging or resetting in either one changes the other, and uncommitted work in either can be lost. Where it does not, git fails in the copy.",
  linked_worktree: "This folder is a git worktree of another repository, and that repository keeps this folder's staged changes and branch link. Right after the move, go to that repository and run git worktree repair with the moved folder's path. Until then git here may fail or seem to work, and a git worktree prune or git gc there drops those staged changes and the branch link for good; commits here that are on no branch can be lost too. The files stay.",
  submodule: "This folder is a submodule of another repository. After the move, git here fails and the parent repository no longer finds it. Its git data stays in the parent repository, so moving the folder back brings git here back.",
  git_data_unread: "CrazyAgent could not read this folder's git data, so it cannot tell what a move breaks.",
  no_git: "This folder has no git history. Agents' changes can't be shown or undone with git until you run git init here.",
  git_unread: "CrazyAgent couldn't read this folder's git setup, so it can't tell what moving or copying it breaks.",
} as const;

/** The size line of the very large warning once the folder was measured. Input: its bytes.
 * Output: one sentence, such as "It is about 42 GB.". */
export function measuredLine(bytes: number): string {
  return `It is about ${formatBytes(bytes)}.`;
}

const MOVE_COPY: Record<string, string> = {
  move_project_busy: "The team is still stopping, or research or a change is still running. Move again in a few seconds.",
  move_project_master_session: "The master is still working in its own copy of this project. Let it finish, then move.",
  move_project_already_home: "This project is already in your CrazyAgent folder.",
  move_project_copy_worktrees: "An agent works in a git worktree of this project, which a copy would leave behind. Move the project instead.",
  move_project_folder_link: "This project's folder is reached through a link, so it can't be moved from here.",
  move_project_in_progress: "A project is being moved. Try again when it is done.",
  move_project_unavailable: "CrazyAgent couldn't check the team, so nothing was moved. Try again.",
  move_project_interrupted: "The move stopped part way. Check the project folder before trying again.",
  move_project_record_invalid: "CrazyAgent couldn't read the move it recorded. Check the project folder before trying again.",
  move_project_old_folder_left: "The project moved, but part of its old folder couldn't be deleted. What's left there may be newer than the copy, so check it before you delete anything.",
  home_move_project: "Another project uses this folder too, so it can't be moved or copied.",
};

/** Plain sentence for one intake, GitHub or setup failure.
 * Input: the code, or the failure with the engine cause behind it. Output: the intake copy, then
 * the project move copy, then the project and app copy, then the fallback naming the code; a
 * different cause is named after. */
export function intakeCopy(failure: string | { code: string; cause?: string }): string {
  const { code, cause } = typeof failure === "string" ? { code: failure, cause: undefined } : failure;
  if (Object.hasOwn(COPY, code)) return namedFailure(COPY[code], code, cause);
  return Object.hasOwn(MOVE_COPY, code) ? namedFailure(MOVE_COPY[code], code, cause) : failureCopy({ code, cause });
}

/** Plain sentence for one refusal of a project move, the move's own words first. Input: the code,
 * or the failure with the engine cause behind it. Output: the move copy, else the intake and app
 * copy. */
export function moveCopy(failure: string | { code: string; cause?: string }): string {
  const code = typeof failure === "string" ? failure : failure.code;
  return Object.hasOwn(MOVE_COPY, code) ? MOVE_COPY[code] : intakeCopy(failure);
}

/** Plain sentence for one refusal or end of a New Project job (DEC-177 add.2), New Project's
 * own words first. Input: the code, or the failure with the engine cause behind it. Output: the
 * New Project copy, else the intake and app copy. */
export function newProjectCopy(failure: string | { code: string; cause?: string }): string {
  const code = typeof failure === "string" ? failure : failure.code;
  return Object.hasOwn(NEW_PROJECT_COPY, code) ? NEW_PROJECT_COPY[code] : intakeCopy(failure);
}

const NEW_PROJECT_COPY: Record<string, string> = {
  home_add_cancelled: "Setting up the project stopped. Nothing was kept.",
  home_destination_exists: "Your projects folder already has a folder with this name. Choose another project name.",
  project_exists: "This project already exists.",
};

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
    return sorted.kind === "error" ? { ...sorted, message: intakeCopy(sorted) } : sorted;
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

/** Downloads every file of a folder that is only in iCloud, so it can then be moved or copied.
 * Input: the absolute folder. Output: how many files it downloaded, or home_icloud_not_downloaded
 * after a stop, a stall or the time limit. */
export function downloadFromIcloud(path: string): Promise<Result<number>> {
  return wrap(() => icloudDownload(path));
}

/** Reads how far the running iCloud download has come. Output: the counts, or null when none runs. */
export function loadIcloudProgress(): Promise<Result<IcloudProgress | null>> {
  return wrap(icloudProgress);
}

/** Stops the running iCloud download; its download call then ends with home_icloud_not_downloaded.
 * Output: whether one was running. */
export function cancelIcloudDownload(): Promise<Result<boolean>> {
  return wrap(icloudCancel);
}

/** Starts the engine's intake job, which downloads what is only in iCloud and then moves or
 * copies the folder into the projects folder apart from the window, or attaches to the job
 * adding that folder already. The native bridge checks the path by the host's own rule, so a
 * Windows drive or network share folder goes through (RV-DS241-1). Inputs: the absolute folder,
 * move or copy and the overrides. Output: the job, home_source_invalid for a path that is not
 * absolute there, or job_busy while another job runs. */
export function startIntake(path: string, mode: AddFolderMode, overrides: AddOverride[]): Promise<Result<IntakeJob>> {
  return wrap(() => intakeStart(path, mode, overrides));
}

/** Reads the engine's intake job, running or last ended. Output: the job, or null. */
export function loadIntakeJob(): Promise<Result<IntakeJob | null>> {
  return wrap(intakeStatus);
}

/** Asks the running intake job to stop between files. Input: its id. Output: whether it ran. */
export function stopIntake(id: string): Promise<Result<boolean>> {
  return wrap(() => intakeStop(id));
}

/** Forgets an ended intake job so Home no longer shows it. Input: its id. Output: whether it
 * was forgotten. */
export function dismissIntake(id: string): Promise<Result<boolean>> {
  return wrap(() => intakeDismiss(id));
}

/** Moves or copies a folder into the projects folder, where it then shows as found.
 * Inputs: the absolute source folder, move or copy, the soft refusals the owner chose to go past
 * and a generation. Output: the receipt. */
export function addExistingFolder(source: string, mode: AddFolderMode, overrides: AddOverride[], generation: number): Promise<Result<IntentReceipt>> {
  return wrap(() => addFolder(source, mode, overrides, generation));
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

/** Reads the GitHub accounts a published project's repository can go under, the personal
 * account first. Output: the accounts and a plain problem, if any. */
export function loadGitHubOwners(): Promise<Result<GitHubOwners>> {
  return wrap(githubOwners);
}

/** Reads where a project's GitHub publish stands. Input: the project id. Output: the job,
 * null without a publish, or a plain failure. */
export function loadGitHubPublish(project: string): Promise<Result<GitHubPublishJob | null>> {
  return wrap(() => githubPublishStatus(project));
}

/** Approves the .gitignore lines and runs the publish on. Inputs: the project id and the
 * lines. Output: the job where it stands, or a plain failure. */
export function approveGitHubPublish(project: string, lines: string[]): Promise<Result<GitHubPublishJob>> {
  return wrap(() => githubPublishApprove(project, lines));
}

/** Leaves a file with a secret out and runs the publish on. Inputs: the project id and the
 * path. Output: the job where it stands, or a plain failure. */
export function excludeFromGitHubPublish(project: string, path: string): Promise<Result<GitHubPublishJob>> {
  return wrap(() => githubPublishExclude(project, path));
}

/** Checks the files for secrets again. Input: the project id. Output: the job where it
 * stands, or a plain failure. */
export function recheckGitHubPublish(project: string): Promise<Result<GitHubPublishJob>> {
  return wrap(() => githubPublishRecheck(project));
}

/** Runs a stopped publish on. Input: the project id. Output: the job where it stands, or a
 * plain failure. */
export function continueGitHubPublish(project: string): Promise<Result<GitHubPublishJob>> {
  return wrap(() => githubPublishContinue(project));
}

/** Reads whether the Overview asks to publish the project on GitHub. Input: the project id.
 * Output: true to ask, or a plain failure. */
export function loadGitHubPublishPrompt(project: string): Promise<Result<boolean>> {
  return wrap(() => githubPublishPrompt(project));
}

/** Keeps Not now or Don't ask again for the project's publish prompt. Inputs: the project id
 * and the answer. Output: null once kept, or a plain failure. */
export function answerGitHubPublishPrompt(project: string, answer: GitHubPromptAnswer): Promise<Result<null>> {
  return wrap(() => githubPublishPromptSet(project, answer));
}

/** Discards the publish record. Input: the project id. Output: the repository left on
 * GitHub, null, or a plain failure. */
export function discardGitHubPublish(project: string): Promise<Result<GitHubPublishLeft | null>> {
  return wrap(() => githubPublishDiscard(project));
}

/** Starts publishing a project on GitHub with a plan the New Project publish block built.
 * Inputs: the project id and the plan. Output: the job at Drafting, or a plain failure. */
export function startGitHubPublish(project: string, plan: GitHubPublishPlan): Promise<Result<GitHubPublishJob>> {
  return wrap(() => githubPublishStart(project, plan));
}

export const DEFAULTS_UNREADABLE = "The saved defaults for new repositories can't be read safely. Use the standard setup to replace them.";

const DEFAULTS_UNREADABLE_CODES = ["github_state_invalid", "github_config_unsafe", "github_config_invalid"];

/** Gives the defaults read its own sentence when the saved file cannot be read safely, since
 * signing in again would not replace it. Input: the read. Output: the read, with the
 * defaults sentence on such a failure. */
export function defaultsRead(read: Result<string[]>): Result<string[]> {
  return read.kind === "error" && DEFAULTS_UNREADABLE_CODES.includes(read.code) ? { ...read, message: DEFAULTS_UNREADABLE } : read;
}

export const DEFAULTS_UNSAVED = "Couldn't save the defaults for new repositories. Try again.";

/** Gives a failed defaults save its own sentence: the file could not be written, or what was
 * saved could not be read back safely. Input: the save's answer. Output: the answer, with the
 * defaults sentences on a failure they name. */
export function savedDefaults(read: Result<string[]>): Result<string[]> {
  return read.kind === "error" && read.code === "github_state_unavailable" ? { ...read, message: DEFAULTS_UNSAVED } : defaultsRead(read);
}

/** Reads the rules a new repository starts with, DEC-182's while none are saved. Output:
 * the rules on. */
export async function loadGitHubRuleDefaults(): Promise<Result<string[]>> {
  return defaultsRead(await wrap(githubRuleDefaults));
}

/** Saves the rules a new repository starts with; a publish already started keeps its copy.
 * Input: the rules on. Output: the rules the engine read back. */
export async function saveGitHubRuleDefaults(rules: string[]): Promise<Result<string[]>> {
  return savedDefaults(await wrap(() => githubRuleDefaultsSet(rules)));
}

/** Reads the live rules of the repository a project was published to; GitHub gets only
 * reads. Input: the project id. Output: the page. */
export function loadGitHubRules(project: string): Promise<Result<GitHubRepoRules>> {
  return wrap(() => githubRules(project));
}

/** Turns one rule on or off on the repository a project was published to, with the owner's
 * sign-in. Inputs: the project id, the `owner/name` the page showed, the rule and whether it
 * is on. Output: the fresh page. */
export function setGitHubRule(project: string, repo: string, rule: string, on: boolean): Promise<Result<GitHubRepoRules>> {
  return wrap(() => githubRuleSet(project, repo, rule, on));
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

/** Installs one missing tool alone, such as Node for the harnesses npm installs.
 * Input: its name. Output: the status as the install starts. */
export function installDependency(name: DependencyName): Promise<Result<SetupStatus>> {
  return wrap(() => startInstall(name, nextGeneration()));
}

/** Fixes the permissions of the tree the owner confirmed for one refused tool (DEC-162).
 * Inputs: its name, the tree the confirm named and whether it named all of it. Output: the
 * status as the fix starts. */
export function fixToolPermissions(name: DependencyName, tree: string, recursive: boolean): Promise<Result<SetupStatus>> {
  return wrap(() => startFix(name, tree, recursive, nextGeneration()));
}

/** Saves what setup offers for a tool found and passed over. Input: the choice. Output: the
 * new setup status. */
export function setRefusedTool(choice: RefusedTool): Promise<Result<SetupStatus>> {
  return wrap(() => saveRefusedTool(choice, nextGeneration()));
}

/** Switches one installed pack entry on or off for every project.
 * Inputs: its id, the state and the pack version shown. Output: the new setup status. */
export function setPackEntry(id: string, on: boolean, version: number): Promise<Result<SetupStatus>> {
  return wrap(() => switchEntry(id, on, version, nextGeneration()));
}

/** Removes one installed pack entry and the skill or server it added.
 * Inputs: its id and the pack version shown. Output: the new setup status. */
export function removePackEntry(id: string, version: number): Promise<Result<SetupStatus>> {
  return wrap(() => removeEntry(id, version, nextGeneration()));
}
