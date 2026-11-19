use super::*;

/// One plain sentence for a refused Changes request.
/// Takes the bounded refusal code.
/// Returns the sentence, or the shared plain sentence for any other code.
pub(in crate::tui) fn refusal(code: &str) -> String {
    match code {
        "changes_owner_dirty" => {
            "Files in your project folder that these changes touch, or that you staged, have uncommitted changes. Commit or stash them, then accept again."
        }
        "changes_owner_untracked" => {
            "Files in your project folder that Git doesn't track sit where these changes add files. Move or delete them, then accept again."
        }
        "changes_detached_head" => {
            "Your project folder isn't on a branch. Check out the branch these changes should land on, then accept again."
        }
        "changes_git_busy" => {
            "Git is in the middle of a merge, rebase, cherry-pick or bisect in your project folder. Finish or stop it, then accept again."
        }
        "changes_conflict" => {
            "These changes conflict with your branch. Ask the agent to update, or discard."
        }
        "changes_stale" => {
            "These changes moved on since you opened them. Review them again, then accept."
        }
        "move_project_in_progress" => {
            "This project is being moved into your projects folder. Try again when the move is done."
        }
        "changes_branch_moved" => {
            "Your branch moved while these changes were landing, so nothing changed. Review and accept again."
        }
        "changes_branch_not_above_base" => {
            "The agent's branch no longer holds the commit these changes start from. Ask the agent to update, or discard."
        }
        "changes_git_hooks" => {
            "Your project's Git config sets a hook command, which CrazyAgent never runs. Remove the hook settings from your project's Git config, then try again."
        }
        "changes_git_include" => {
            "Your project's Git config includes a file inside your project folder, where accepted changes can rewrite it. Move that file outside the project folder and include it by its full path, then try again."
        }
        "changes_git_old" if cfg!(target_os = "macos") => {
            "Landing changes needs Git 2.40 or newer, and the Git of Xcode or its Command Line Tools on this Mac is older; CrazyAgent doesn't use Homebrew's Git. Update them in Software Update, or in Terminal with softwareupdate --install --all or xcode-select --install, then try again."
        }
        "changes_git_old" => {
            "Landing changes needs Git 2.40 or newer, and the Git on this computer is older. Update Git, then try again."
        }
        "changes_git_unsupported" => {
            "The Git on this computer can't land changes. Update Git, then try again."
        }
        "changes_merge_unsafe" => {
            "These changes can't land as a merge commit: an agent commit isn't safe to keep in your history (too many commits, hidden characters, another author's name, or files Git treats specially). Switch Landing to Squash, then accept again."
        }
        "changes_restore_expired" => {
            "These changes can no longer be restored. Discarded changes are kept for 7 days."
        }
        "changes_agent_has_pending" => {
            "This agent has newer changes waiting. Accept or discard those first."
        }
        "changes_agent_busy" => "The agent is working right now. Try again when it finishes.",
        "changes_not_found" => "These changes are gone. The list is up to date now.",
        "changes_message_invalid" => {
            "The commit message is empty, too long or has characters Git can't keep. Edit it, then accept again."
        }
        "changes_owner_identity_missing" => {
            "Git doesn't know your name and email yet. Set user.name and user.email, then accept again."
        }
        "changes_embedded_repository" => {
            "These changes add a folder Git would treat as a repository of its own inside the project, such as a submodule. Ask the agent to remove it."
        }
        "changes_agent_branch_invalid" => {
            "The agent's branch couldn't be read from its own copy of the project. Ask the agent to check its branch, or discard."
        }
        "changes_agent_branch_exists" => {
            "A branch with this agent's name is already in your project. Rename or delete it, then start the agent again."
        }
        "changes_target_invalid" => {
            "That branch isn't a local branch of your project any more. Pick another branch, then accept again."
        }
        "changes_target_checked_out" => {
            "That branch is checked out in another working copy CrazyAgent doesn't manage. Pick another branch, or check it out in your project folder."
        }
        "changes_master_tools_off" => {
            "This master has tools off, so it can't edit files to update these changes. Turn tools on for the master, or discard."
        }
        "changes_master_closed" => {
            "The master conversation these changes came from is closed, so it can't update them. Discard them, or start a new conversation."
        }
        "changes_master_not_started" => {
            "Your branch is merged into the master's copy, but the master couldn't start resolving it yet. It starts after the master's next turn ends."
        }
        "team_agent_not_found" => {
            "The agent that made these changes is no longer on the team, so it can't update them. Discard them instead."
        }
        "team_conversation_full" => {
            "Too many of your messages are waiting for the master. Send more once it has answered."
        }
        "team_task_invalid" => "The task is empty or too long.",
        "changes_unavailable" => return format!("{NOT_READY} {NOT_READY_LINE}"),
        other => return plain_problem(other),
    }
    .to_owned()
}

#[cfg(all(test, unix))]
mod tests;
