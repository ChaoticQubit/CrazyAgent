use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeJob, IntakeQuery};
use crate::tui::ctx::Ctx;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::coded;

/// The plain sentence a screen shows when the engine does not support a request yet.
pub(in crate::tui) const NOT_READY: &str = "Not ready yet.";

/// Bytes above which copying a folder gets a warning before it starts.
pub(in crate::tui) const COPY_WARN_BYTES: u64 = 1 << 30;

/// Sends one intake query to the engine.
/// Takes the context and the query.
/// Returns nothing; the reply arrives as an `OwnerEvent::Intake` holding the same query.
pub(in crate::tui) fn ask(ctx: &mut Ctx, query: IntakeQuery) {
    ctx.request(Request::Intake(Box::new(IntakeJob::Query(query))));
}

/// Reads the intake reply out of an arrival.
/// Takes any arrival.
/// Returns the intake event, or none for every other arrival.
pub(in crate::tui) fn event(arrival: &Arrival) -> Option<&IntakeEvent> {
    match arrival {
        Arrival::Owner(owner) => match owner.as_ref() {
            OwnerEvent::Intake(event) => Some(event.as_ref()),
            _ => None,
        },
        _ => None,
    }
}

/// Says whether a problem code means the engine has no support for the request yet.
/// Takes the bounded plain code.
/// Returns true for an unsupported or not-ready code.
pub(in crate::tui) fn not_ready(code: &str) -> bool {
    code.ends_with("_unsupported") || code.ends_with("_not_ready") || code == "not_ready"
}

/// The plain sentence for one projects home, add project, onboarding or GitHub problem.
/// Takes the bounded plain code the engine or the client sent.
/// Returns the sentence; a code with no sentence gets a generic retry sentence naming the code.
pub(in crate::tui) fn copy(code: &str) -> String {
    known(code).map_or_else(
        || coded("Something went wrong. Try again.", code),
        str::to_owned,
    )
}

/// The sentence for a projects home, add project, onboarding or GitHub problem code.
/// Takes the code. Returns the sentence from this table or the engine's projects home
/// table, or none for a code neither knows.
pub(in crate::tui) fn known(code: &str) -> Option<&'static str> {
    Some(match code {
        "projects_home_unset" | "projects_home_not_set" => "Pick your projects folder first.",
        "projects_home_path_invalid" | "home_source_invalid" | "folder_size_path_invalid" => {
            "Type a full folder path, such as ~/Projects."
        }
        "projects_home_missing" => "That folder does not exist. Create it first or pick another.",
        "projects_home_not_folder" | "home_source_not_folder" => {
            "Pick a folder, not a file or a link."
        }
        "projects_home_unsafe" => {
            "Pick a separate folder for projects, not your home folder or a system folder."
        }
        "projects_home_unreadable" => "CrazyAgent can't read this folder. Pick another.",
        "home_source_missing" => "That folder isn't there anymore. Pick it again.",
        "home_source_in_home" => {
            "This folder is already in your projects folder. It shows on Home."
        }
        "home_source_unsafe" | "folder_size_unsafe" => {
            "This folder holds your home folder or app data, so it can't be added."
        }
        "home_source_hidden" | "home_source_name" => "Rename the folder to a plain name first.",
        "home_destination_exists" | "clone_destination_exists" => {
            "Your projects folder already has a folder with this name. Rename one of them first."
        }
        "home_copy_special_file" => "The folder holds a device or pipe file that can't be copied.",
        "home_copy_too_large" => "The folder has too many files or levels to copy.",
        "home_copy_failed" | "home_move_failed" => {
            "The folder could not be added. Nothing was removed. Try again."
        }
        "home_copy_linked_checkout" => {
            "This folder is a git worktree or submodule of another repository, so a copy would still share it. Use it where it is, or copy the main repository folder."
        }
        "home_copy_unverified" | "home_add_unverified" => {
            "The copy did not match the original, so the original was kept."
        }
        "home_move_source_left" => {
            "The folder was copied in, but the original could not be removed. Remove it yourself."
        }
        "folder_size_unreadable" => "Some files in this folder can't be read.",
        "folder_size_timeout" | "folder_size_too_large" => "This folder is very large.",
        "owner_home_unknown" => "Your home folder could not be found.",
        "onboarding_running" => "This project is already being set up.",
        "onboarding_missing" => "This project isn't being set up anymore. Start again.",
        "onboarding_path_invalid" | "onboarding_path_not_canonical" => {
            "CrazyAgent can't set up this folder from this path. Open it from Home again."
        }
        "onboarding_folder_missing" => "That folder isn't there anymore.",
        "onboarding_folder_unsafe" => {
            "This folder holds your home folder, app data or agent settings, so it can't be set up."
        }
        "onboarding_layout_incomplete" => {
            "The project's .crazyagent folder is incomplete. Check it and start again."
        }
        "onboarding_project_mismatch" | "project_exists" => {
            "Another project already uses this name. Rename the folder first."
        }
        "project_setup_missing" | "project_missing" => "This project is gone. It was reloaded.",
        "pool_missing" => "That pool no longer exists. Pick another pool.",
        "stale_revision" => "This project changed meanwhile. Check it and confirm again.",
        "github_signed_out" | "github_not_signed_in" => "Sign in with GitHub first.",
        "github_token_blank" => "Paste a GitHub token.",
        "github_token_invalid" | "github_token_refused" | "account_secret_invalid" => {
            "GitHub didn't accept this token. Check it and try again."
        }
        "github_device_expired" | "github_sign_in_expired" => {
            "The GitHub code expired. Start again."
        }
        "github_device_denied" | "github_sign_in_denied" | "github_sign_in_cancelled" => {
            "The sign-in was cancelled on GitHub. Start again."
        }
        "github_sign_in_failed" => "The GitHub sign-in didn't finish. Start again.",
        "github_device_flow_refused" | "github_client_id_refused" => {
            "GitHub refused sign-in for this app. Paste a token or use the GitHub CLI."
        }
        "github_cli_unavailable" => {
            "The GitHub CLI isn't installed or isn't signed in on this computer."
        }
        "github_cli_missing" => "The GitHub CLI (gh) isn't installed on this computer.",
        "github_cli_not_signed_in" => {
            "The GitHub CLI isn't signed in. Run gh auth login, then try again."
        }
        "github_unreachable" => "Couldn't reach GitHub. Check your connection and try again.",
        "github_reply_invalid" => "GitHub sent a reply CrazyAgent couldn't read. Try again.",
        "github_client_missing" | "github_client_id_not_configured" => {
            "Sign in with GitHub isn't set up in this build yet. Paste a token or use the GitHub CLI."
        }
        "github_config_unsafe" | "github_config_invalid" => {
            "The GitHub settings file is unreadable or not private to you."
        }
        "github_state_invalid" | "github_state_unavailable" => {
            "The saved GitHub sign-in couldn't be read. Sign in again."
        }
        "credential_missing" => "The saved GitHub token is gone. Sign in again.",
        "credential_store_unavailable" => {
            "This computer's password store can't be used right now. Try again."
        }
        "github_outcome_unknown" => {
            "CrazyAgent couldn't confirm the GitHub step. Check it and try again."
        }
        "github_page_invalid" | "github_repo_page_invalid" => {
            "There are no more repositories to show."
        }
        "github_repo_name_invalid" => "That isn't a GitHub repository name. Use owner/name.",
        "repo_missing" => "This repository isn't on your GitHub account anymore.",
        "repo_already_cloned" => "This repository is already in your projects folder.",
        "clone_failed" | "github_clone_failed" => {
            "Couldn't clone this repository. Nothing was kept. Try again."
        }
        "github_clone_timed_out" => {
            "Cloning took too long and was stopped. Nothing was kept. Try again."
        }
        "github_git_missing" => {
            "Git isn't installed on this computer, so the repository can't be cloned."
        }
        "trusted_connection_unavailable" | "trusted_connection_lost" => {
            "The engine connection dropped. CrazyAgent reconnects on its own."
        }
        code if not_ready(code) => NOT_READY,
        _ => return crate::projects_home::plain_problem(code),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_problem_reads_as_a_plain_sentence_and_an_unknown_one_names_its_code() {
        for code in [
            "github_device_expired",
            "projects_home_unset",
            "owner_read_unsupported",
            "github_sign_in_expired",
            "github_not_signed_in",
            "github_clone_failed",
            "github_clone_timed_out",
            "github_cli_missing",
            "github_config_unsafe",
            "projects_home_not_set",
            "clone_destination_exists",
            "home_move_partial",
            "home_move_nested",
        ] {
            let text = copy(code);
            assert!(known(code).is_some(), "{code} has a sentence");
            assert!(!text.contains('_'), "{code} shows {text}");
            assert!(text.ends_with('.'), "{code} shows {text}");
        }
        assert_eq!(copy("owner_read_unsupported"), NOT_READY);
        assert_eq!(
            copy("home_move_partial"),
            crate::projects_home::plain_problem("home_move_partial").unwrap()
        );
        assert_eq!(known("something_new_and_odd"), None);
        assert_eq!(
            copy("something_new_and_odd"),
            "Something went wrong. Try again. (code: something_new_and_odd)"
        );
        assert_eq!(
            copy("database is locked"),
            "Something went wrong. Try again. (code: engine_error_unplain)"
        );
        assert!(copy("home_copy_linked_checkout").contains("worktree or submodule"));
        for code in [
            "home_move_worktrees",
            "home_move_project",
            "home_trash_outside",
        ] {
            assert_eq!(
                Some(copy(code).as_str()),
                crate::projects_home::plain_problem(code),
                "{code}"
            );
        }
        assert!(not_ready("github_unsupported"));
        assert!(!not_ready("github_unreachable"));
    }
}
