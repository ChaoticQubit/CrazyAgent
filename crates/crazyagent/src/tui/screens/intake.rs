use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeJob, IntakeQuery};
use crate::tui::ctx::Ctx;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::coded;

/// The plain sentence a screen shows when the engine does not support a request yet.
pub(in crate::tui) const NOT_READY: &str = "Not ready yet.";

/// Bytes above which copying a folder gets a warning before it starts.
pub(in crate::tui) const COPY_WARN_BYTES: u64 = 1 << 30;

/// One warning shown before a Move anyway or Copy anyway, in the owner's words. The texts
/// match the desktop's `ADD_WARNINGS` map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum AddWarning {
    CloseApps,
    OldPlace,
    AcrossDisks,
    SameDisk,
    CopyAll,
    Large,
    Unmeasured,
    TooMany,
    Unreadable,
    UnreadableCopy,
    Worktrees,
    LinkedWorktree,
    Submodule,
    GitDataUnread,
    LinkedCheckout,
    NoGit,
}

impl AddWarning {
    /// The sentence for this warning. Takes the warning. Returns its fixed text.
    pub(in crate::tui) fn text(self) -> &'static str {
        match self {
            Self::CloseApps => {
                "Close editors, terminals and apps that use this folder first. Files saved while it moves can be lost."
            }
            Self::OldPlace => {
                "Anything that points at the old place (terminals, editors, scripts, shortcuts) stops working."
            }
            Self::AcrossDisks => {
                "This copies every file to the other disk, then removes the original. It can take a long time and needs as much free space again. If the Mac sleeps, loses power or the disk fills while it copies, your original stays whole and a part copy may be left in your projects folder to delete. If it stops while it removes the original, the full copy stays in your projects folder (keep it) and part of the original is left."
            }
            Self::SameDisk => "This is a rename on the same disk; it is quick.",
            Self::CopyAll => {
                "This copies every file into your projects folder. It can take a long time and needs as much free space again. If the Mac sleeps, loses power or the disk fills, the copy stops: your original stays, and a part copy may be left in your projects folder to delete."
            }
            Self::Large => "This folder is very large.",
            Self::Unmeasured => "CrazyAgent couldn't measure it.",
            Self::TooMany => {
                "The folder is deeper or bigger than CrazyAgent checks, so the copy check can take a long time."
            }
            Self::Unreadable => {
                "Some files can't be read. They can't be copied, so a move across disks keeps the original in place and fails rather than leave them behind."
            }
            Self::UnreadableCopy => {
                "Some files can't be read. They can't be copied, so the copy fails rather than leave them behind."
            }
            Self::Worktrees => {
                "Git worktrees linked to this folder stop working until you run git worktree repair in each. Their files stay, but git there fails until repaired."
            }
            Self::LinkedWorktree => {
                "This folder is a git worktree of another repository, and that repository keeps this folder's staged changes and branch link. Right after the move, go to that repository and run git worktree repair with the moved folder's path. Until then git here may fail or seem to work, and a git worktree prune or git gc there drops those staged changes and the branch link for good; commits here that are on no branch can be lost too. The files stay."
            }
            Self::Submodule => {
                "This folder is a submodule of another repository. After the move, git here fails and the parent repository no longer finds it. Its git data stays in the parent repository, so moving the folder back brings git here back."
            }
            Self::GitDataUnread => {
                "CrazyAgent could not read this folder's git data, so it cannot tell what a move breaks."
            }
            Self::LinkedCheckout => {
                "The copy keeps this folder's link to another repository's git data. Where that link still reaches it, the copy and this folder share one index and current branch: switching branches, staging or resetting in either one changes the other, and uncommitted work in either can be lost. Where it does not, git fails in the copy."
            }
            Self::NoGit => {
                "This folder has no git history. Agents' changes can't be shown or undone with git until you run git init here."
            }
        }
    }
}

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
        "onboarding_project_ids_used" => {
            "Every id this folder's name can get, 99 in all, is taken by your projects or ones you removed. Rename the folder first."
        }
        "project_id_removed" => {
            "A project you removed used this name, so it can't be used again. Choose another project name."
        }
        "home_copy_special_file" => "The folder holds a device or pipe file that can't be copied.",
        "home_copy_too_large" => "The folder has too many files or levels to copy.",
        "home_override_invalid" => {
            "CrazyAgent couldn't match what you agreed to with this folder. Choose the folder again."
        }
        "home_copy_failed" | "home_move_failed" => {
            "The folder could not be added. Nothing was removed. Try again."
        }
        "home_copy_linked_checkout" => {
            "This folder is a git worktree or submodule of another repository, so a copy would still share it. Use it where it is, or copy the main repository folder."
        }
        "home_add_cancelled" => "Adding the folder stopped. Nothing was moved or copied.",
        "home_add_interrupted" => {
            "Adding the folder stopped part way. Check your projects folder before trying again."
        }
        "job_busy" => "Another job is running. Wait for it to finish on Home.",
        "intake_failed" => {
            "Adding the folder failed. Check your projects folder before trying again."
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
        "onboarding_instructions_unwritable" => {
            "This folder came with a .crazyagent/instructions.md that CrazyAgent can't label as Imported. Make that file and its .crazyagent folder writable, then start again."
        }
        "onboarding_instructions_unreadable" => {
            "This folder came with a .crazyagent/instructions.md that CrazyAgent can't read: it is a link, too large or not plain text. Move or fix that file, then start again."
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
        "github_repo_name_unsupported" => {
            "CrazyAgent can't clone a repository whose name starts with a dot or dash, ends in .git or uses characters other than letters, digits, dots, dashes and underscores."
        }
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
        "github_publish_detached" => {
            "The project's git isn't on a branch. Check out a branch, then try again."
        }
        "github_publish_draft_not_asked" => {
            "No agent drafted these lines, so CrazyAgent's standard lines are shown."
        }
        "github_publish_exclude_invalid" => "That file can't be left out. Check again.",
        "github_publish_failed" => "Publishing on GitHub failed. Try again.",
        "github_publish_file_invalid" => {
            "A file publishing writes (.gitignore, SECURITY.md or the .github folder) is a link or not a plain file. Fix it, then try again."
        }
        "github_publish_gitignore_invalid" => {
            "The project's .gitignore can't be read or changed safely. Fix it, then try again."
        }
        "github_publish_head_moved" => {
            "The project's git changed while it was being published. Try again."
        }
        "github_publish_hooks_set" => {
            "The project's git runs hooks, so CrazyAgent won't commit or push it. Remove the hooks, then try again."
        }
        "github_publish_ignore_invalid" => {
            "These lines can't go into .gitignore. Keep one pattern per line, without control characters."
        }
        "github_publish_in_progress" => {
            "This project already has a publish. Finish or discard it first."
        }
        "github_publish_not_plain_repo" => {
            "The project's .git is a link or a file, not a plain folder, so CrazyAgent won't publish it."
        }
        "github_publish_not_started" => {
            "This project has no publish to go on with. Start it again."
        }
        "github_publish_on_github" => "This project is already on GitHub.",
        "github_publish_other_host" => {
            "This project's git already points at another host, so CrazyAgent won't publish it on GitHub."
        }
        "github_publish_project_unknown" => "This project isn't in CrazyAgent anymore.",
        "github_publish_push_failed" => "Git couldn't push to GitHub. Try again.",
        "github_publish_push_timed_out" => {
            "Pushing to GitHub took too long and was stopped. Try again."
        }
        "github_publish_running" => {
            "This project is being published right now. Wait for it to finish."
        }
        "github_publish_secret_in_history" => {
            "The secret is already in the project's git history, so leaving the file out doesn't remove it. Remove it from the history, then check again."
        }
        "github_publish_step_invalid" => {
            "The publish moved on since this was shown. Look at it again."
        }
        "github_publish_too_large" => {
            "This project is too large to publish from CrazyAgent: over 20,000 files or 200 MB."
        }
        "github_publish_unavailable" => "Publishing on GitHub can't be used right now. Try again.",
        "github_publish_workflow_scope" => {
            "The GitHub sign-in can't push workflow files. Sign in to GitHub again, then try again."
        }
        "github_repo_name_taken" => {
            "That repository name is taken on this GitHub account. Pick another name."
        }
        "github_repo_create_refused" => {
            "GitHub refused to make the repository. Check that this account may create repositories."
        }
        "github_repo_visibility_mismatch" => {
            "GitHub made the repository with a different visibility than asked. Check it on GitHub."
        }
        "github_repo_description_invalid" => {
            "GitHub refused the repository description. Use at most 350 characters on one line."
        }
        "github_store_unavailable" => {
            "CrazyAgent's GitHub records can't be read or written right now. Try again."
        }
        "github_rules_not_published" => super::project_github::RULES_NOT_PUBLISHED,
        "github_rule_invalid" => {
            "This rule is a file in the project, so it can't be switched on GitHub."
        }
        "github_rules_repo_mismatch" => {
            "This project is now published to another GitHub repository, so nothing was changed. Check its rules again."
        }
        "github_repo_unreadable" => {
            "GitHub didn't show this repository. Check that it still exists and that your GitHub sign-in can see it."
        }
        "github_request_bounds" => "That request was too large for the engine. Try again.",
        "github_reply_bounds" => "GitHub's reply was too large to show. Try again.",
        "github_request_invalid" => "The engine couldn't read that request. Reload and try again.",
        "github_rule_busy" => "Another GitHub rule is still changing. Try again when it's done.",
        "github_rule_unstarted" => "CrazyAgent couldn't start the change. Try again.",
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
    fn the_tui_add_warnings_match_the_desktop_map_word_for_word() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/src/api-intake.ts"
        ))
        .unwrap();
        let start = source.find("export const ADD_WARNINGS = {").unwrap();
        let body = &source[start..];
        let body = &body[..body.find("\n} as const;").unwrap()];
        let mut desktop: std::collections::BTreeMap<&str, &str> = body
            .lines()
            .skip(1)
            .map(|line| {
                let (key, text) = line.trim().split_once(": ").unwrap();
                let text = text.strip_suffix(',').unwrap_or(text);
                (
                    key,
                    text.strip_prefix('"').unwrap().strip_suffix('"').unwrap(),
                )
            })
            .collect();
        assert_eq!(desktop.len(), 17, "{desktop:#?}");
        assert!(
            desktop.remove("git_unread").is_some(),
            "the TUI reads git itself, so only the desktop says it could not"
        );
        let key = |warning: AddWarning| match warning {
            AddWarning::CloseApps => "close_apps",
            AddWarning::OldPlace => "old_place",
            AddWarning::AcrossDisks => "across_disks",
            AddWarning::SameDisk => "same_disk",
            AddWarning::CopyAll => "copy_all",
            AddWarning::Large => "large",
            AddWarning::Unmeasured => "unmeasured",
            AddWarning::TooMany => "too_many",
            AddWarning::Unreadable => "unreadable",
            AddWarning::UnreadableCopy => "unreadable_copy",
            AddWarning::Worktrees => "worktrees",
            AddWarning::LinkedWorktree => "linked_worktree",
            AddWarning::Submodule => "submodule",
            AddWarning::GitDataUnread => "git_data_unread",
            AddWarning::LinkedCheckout => "linked_checkout",
            AddWarning::NoGit => "no_git",
        };
        let tui: std::collections::BTreeMap<&str, &str> = [
            AddWarning::CloseApps,
            AddWarning::OldPlace,
            AddWarning::AcrossDisks,
            AddWarning::SameDisk,
            AddWarning::CopyAll,
            AddWarning::Large,
            AddWarning::Unmeasured,
            AddWarning::TooMany,
            AddWarning::Unreadable,
            AddWarning::UnreadableCopy,
            AddWarning::Worktrees,
            AddWarning::LinkedWorktree,
            AddWarning::Submodule,
            AddWarning::GitDataUnread,
            AddWarning::LinkedCheckout,
            AddWarning::NoGit,
        ]
        .into_iter()
        .map(|warning| (key(warning), warning.text()))
        .collect();
        assert_eq!(tui, desktop);
    }

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
            "onboarding_project_ids_used",
            "project_id_removed",
            "projects_removed_full",
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
        assert_eq!(
            copy("onboarding_project_ids_used"),
            "Every id this folder's name can get, 99 in all, is taken by your projects or ones you removed. Rename the folder first."
        );
        assert_eq!(
            copy("project_id_removed"),
            "A project you removed used this name, so it can't be used again. Choose another project name."
        );
        assert_eq!(
            copy("projects_removed_full"),
            "CrazyAgent keeps a record of up to 100,000 removed projects and that record is full, so no project can be removed. Nothing was removed or moved to the Trash."
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

    /// RV-DS241-102: what a move leaves of the original are the entries that changed after
    /// its copy check, which can be the only copy of newer edits. For an added folder and a
    /// moved project alike the TUI says it in the desktop's own words (DEC-141), says what is
    /// left may be newer than the copy, tells the owner to check it and never to remove or
    /// delete what is there. A folder that did not arrive, a copy that did not match and an
    /// add that stopped part way are told in the desktop's words too (r1 F1).
    #[test]
    fn a_move_that_left_part_of_the_original_says_check_it_in_the_desktop_words() {
        let read = |path: &str| {
            std::fs::read_to_string(format!(
                "{}/../../apps/desktop/src/{path}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let desktop = |source: &str, code: &str| {
            let key = format!("  {code}: \"");
            let start = source.find(&key).unwrap() + key.len();
            let rest = &source[start..];
            rest[..rest.find("\",\n").unwrap()].to_owned()
        };
        let intake = read("api-intake.ts");
        assert_eq!(
            copy("home_copy_names_fold"),
            "Two names in the folder differ only in case or accents, and your projects folder can't keep both. Rename one of them, then try again. The original is untouched."
        );
        assert_eq!(
            copy("home_copy_entry_swapped"),
            "Something replaced a file or folder in it while it was being copied, so the copy stopped. The original is untouched. Try again once nothing else is changing the folder."
        );
        for code in [
            "home_add_unverified",
            "home_copy_unverified",
            "home_add_interrupted",
            "home_copy_names_fold",
            "home_copy_entry_swapped",
        ] {
            assert_eq!(copy(code), desktop(&intake, code), "{code}");
        }
        for (code, said, words) in [
            (
                "home_move_source_left",
                copy("home_move_source_left"),
                desktop(&intake, "home_move_source_left"),
            ),
            (
                "move_project_old_folder_left",
                super::super::project::plain_problem("move_project_old_folder_left"),
                desktop(&intake, "move_project_old_folder_left"),
            ),
        ] {
            assert_eq!(said, words, "{code}");
            let lower = said.to_lowercase();
            assert!(lower.contains("check"), "{code}: {said}");
            assert!(
                lower.contains("newer than the copy"),
                "{code} does not say what is left may be newer than the copy: {said}"
            );
            for told in [
                "yourself",
                "remove it",
                "delete it",
                "remove the",
                "delete the",
            ] {
                assert!(
                    !lower.contains(told),
                    "{code} tells the owner to remove what may be the only copy of newer edits: {said}"
                );
            }
        }
    }
}
