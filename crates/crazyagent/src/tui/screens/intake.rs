use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeJob, IntakeQuery};
use crate::tui::ctx::Ctx;
use crate::tui::seam::{Arrival, Request};

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
/// Returns the sentence; an unknown code gets a generic retry sentence, never the code.
pub(in crate::tui) fn copy(code: &str) -> &'static str {
    match code {
        "projects_home_unset" => "Pick your projects folder first.",
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
        "home_destination_exists" => {
            "Your projects folder already has a folder with this name. Rename one of them first."
        }
        "home_copy_special_file" => "The folder holds a device or pipe file that can't be copied.",
        "home_copy_too_large" => "The folder has too many files or levels to copy.",
        "home_copy_failed" | "home_move_failed" => {
            "The folder could not be added. Nothing was removed. Try again."
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
        "github_signed_out" => "Sign in with GitHub first.",
        "github_token_blank" => "Paste a GitHub token.",
        "github_token_invalid" | "account_secret_invalid" => {
            "GitHub didn't accept this token. Check it and try again."
        }
        "github_device_expired" => "The GitHub code expired. Start again.",
        "github_device_denied" => "The sign-in was cancelled on GitHub. Start again.",
        "github_cli_unavailable" => {
            "The GitHub CLI isn't installed or isn't signed in on this computer."
        }
        "github_unreachable" => "Couldn't reach GitHub. Check your connection and try again.",
        "github_client_missing" => {
            "Sign in with GitHub isn't set up in this build yet. Paste a token or use the GitHub CLI."
        }
        "github_page_invalid" => "There are no more repositories to show.",
        "repo_missing" => "This repository isn't on your GitHub account anymore.",
        "repo_already_cloned" => "This repository is already in your projects folder.",
        "clone_failed" => "Couldn't clone this repository. Nothing was kept. Try again.",
        "trusted_connection_unavailable" | "trusted_connection_lost" => {
            "The engine connection dropped. CrazyAgent reconnects on its own."
        }
        code if not_ready(code) => NOT_READY,
        _ => "Something went wrong. Try again.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_reads_as_a_plain_sentence_without_the_code() {
        for code in [
            "folder_name_taken",
            "github_device_expired",
            "projects_home_unset",
            "owner_read_unsupported",
            "something_new_and_odd",
        ] {
            let text = copy(code);
            assert!(!text.contains('_'), "{code} shows {text}");
            assert!(text.ends_with('.'), "{code} shows {text}");
        }
        assert_eq!(copy("owner_read_unsupported"), NOT_READY);
        assert!(not_ready("github_unsupported"));
        assert!(!not_ready("github_unreachable"));
    }
}
