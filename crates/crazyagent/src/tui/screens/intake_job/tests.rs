use super::*;
use crate::projects_home::AddOverride;
use crate::projects_home::icloud::Progress;
use crate::protocol::RequestId;

fn job(state: JobState) -> JobView {
    JobView {
        left: Vec::new(),
        id: RequestId::new("add-1").unwrap(),
        kind: crate::projects_home::job::JobKind::Add,
        project_id: None,
        source: "/Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop".into(),
        mode: AddMode::Move,
        overrides: vec![AddOverride::TooLarge],
        stopping: false,
        state,
    }
}

fn copying(copied: u64, checked: u64) -> JobState {
    JobState::Copying { copied, checked }
}

fn failed(code: &str, phase: Phase) -> JobState {
    JobState::Failed {
        code: code.into(),
        phase,
    }
}

#[test]
fn an_intake_job_line_says_what_it_does_now_with_counts_as_files_land() {
    let downloading = |done, total| JobState::Downloading {
        progress: Progress {
            total,
            done,
            ..Default::default()
        },
    };
    assert_eq!(
        line(&job(downloading(0, 0))),
        "Downloading shop from iCloud..."
    );
    assert_eq!(
        line(&job(downloading(12, 3000))),
        "macOS is downloading shop: 12 of 3000 files"
    );
    assert_eq!(line(&job(copying(0, 0))), "Moving shop...");
    let copy_job = |state| JobView {
        mode: AddMode::Copy,
        ..job(state)
    };
    assert_eq!(
        line(&copy_job(copying(18, 0))),
        "Copying shop: 18 files copied"
    );
    assert_eq!(
        line(&job(copying(18, 5))),
        "Checking the copy of shop: 5 files checked"
    );
    assert_eq!(
        line(&job(JobState::Clearing)),
        "Removing shop from its old place..."
    );
    let stopping = JobView {
        stopping: true,
        ..job(copying(18, 0))
    };
    assert_eq!(line(&stopping), "Stopping shop...");
    assert_eq!(
        line(&job(JobState::Done)),
        "shop is in your projects folder. Set it up from Home."
    );
    assert_eq!(
        line(&job(JobState::Cancelled {
            phase: Phase::Copying
        })),
        "Adding shop stopped. Nothing was moved or copied."
    );
    assert_eq!(
        line(&job(failed("home_copy_failed", Phase::Copying))),
        format!("shop: {}", copy("home_copy_failed"))
    );
    let asked = |code: &str| JobState::NeedsAnswer { code: code.into() };
    assert_eq!(
        line(&job(asked("home_move_worktrees"))),
        "shop needs your answer before it is moved."
    );
    assert_eq!(
        line(&copy_job(asked("home_copy_too_large"))),
        "shop needs your answer before it is copied."
    );
}

#[test]
fn every_job_code_the_engine_sends_has_its_own_sentence() {
    for code in [
        "job_busy",
        "intake_failed",
        "home_add_cancelled",
        "home_add_in_progress",
        "home_add_interrupted",
    ] {
        assert!(crate::tui::screens::intake::known(code).is_some(), "{code}");
    }
}

#[test]
fn an_intake_job_offers_stop_while_it_runs_retry_only_when_nothing_landed() {
    use JobAction::{Answer, Dismiss, Retry, Stop};
    assert_eq!(actions(&job(copying(4, 0))), vec![Stop]);
    let stopping = JobView {
        stopping: true,
        ..job(copying(4, 0))
    };
    assert_eq!(actions(&stopping), Vec::new());
    assert_eq!(actions(&job(JobState::Clearing)), Vec::new());
    assert_eq!(actions(&job(JobState::Done)), vec![Dismiss]);
    assert_eq!(
        actions(&job(failed("home_copy_failed", Phase::Copying))),
        vec![Retry, Dismiss]
    );
    assert_eq!(
        actions(&job(failed("home_move_source_left", Phase::Clearing))),
        vec![Dismiss]
    );
    assert_eq!(
        actions(&job(JobState::Cancelled {
            phase: Phase::Downloading
        })),
        vec![Retry, Dismiss]
    );
    let asked = JobState::NeedsAnswer {
        code: "home_move_worktrees".into(),
    };
    assert_eq!(actions(&job(asked)), vec![Answer, Dismiss]);
    assert!(running(&job(JobState::Clearing)));
    assert!(!running(&job(JobState::Done)));
}

/// DEC-197: each job's end is said once, when the read after shows a job that ran or waited
/// before has ended, while the jobs beside it still run.
#[test]
fn each_jobs_end_is_said_once_while_the_jobs_beside_it_still_run() {
    let running_job = job(copying(3, 0));
    let done = job(JobState::Done);
    let garden = JobView {
        id: RequestId::new("add-2").unwrap(),
        source: "/Users/mira/Desktop/garden".into(),
        ..job(copying(1, 0))
    };
    let said = |before: &[JobView], after: &[JobView]| {
        ended(before, after)
            .into_iter()
            .map(line)
            .collect::<Vec<_>>()
    };
    let shop_done = "shop is in your projects folder. Set it up from Home.";
    assert_eq!(said(&[running_job.clone()], &[done.clone()]), [shop_done]);
    assert_eq!(
        said(
            &[running_job.clone(), garden.clone()],
            &[done.clone(), garden.clone()]
        ),
        [shop_done]
    );
    assert!(said(&[done.clone()], &[done.clone()]).is_empty());
    assert!(said(&[], &[done.clone()]).is_empty());
    assert!(said(&[running_job.clone()], &[running_job.clone()]).is_empty());
    assert!(said(&[running_job.clone()], &[]).is_empty());
    let other = JobView {
        id: RequestId::new("add-9").unwrap(),
        ..done
    };
    assert!(said(&[running_job.clone()], &[other]).is_empty());
    let waiting = job(JobState::Waiting {
        behind: "garden".into(),
    });
    assert!(
        said(&[waiting.clone()], &[running_job]).is_empty(),
        "a waiting job let go is still at work"
    );
    let cancelled = job(JobState::Cancelled {
        phase: Phase::Waiting,
    });
    assert_eq!(
        said(&[waiting], &[cancelled]),
        ["Adding shop stopped. Nothing was moved or copied."]
    );
}

/// DEC-197: a job waiting its turn names the job it waits behind, offers Stop, and reads
/// Waiting on its card and in the header.
#[test]
fn a_job_waiting_its_turn_names_the_job_it_waits_behind_and_offers_stop() {
    let waiting = job(JobState::Waiting {
        behind: "garden".into(),
    });
    assert_eq!(line(&waiting), "Waiting for garden to finish...");
    assert_eq!(actions(&waiting), [JobAction::Stop]);
    assert_eq!(badge(&waiting), (Status::Active, "Waiting"));
    assert_eq!(next(&waiting), "Enter stops it.");
    assert_eq!(brief(&waiting, "~"), "shop: waiting");
    assert!(running(&waiting));
}

#[test]
fn home_s_job_card_names_the_state_in_the_desktop_s_words_and_says_what_its_keys_do() {
    let downloading = JobState::Downloading {
        progress: Progress {
            total: 3,
            done: 1,
            ..Default::default()
        },
    };
    let rows = [
        (
            downloading.clone(),
            Status::Active,
            "Downloading",
            "Enter stops it.",
        ),
        (copying(2, 0), Status::Active, "Adding", "Enter stops it."),
        (
            JobState::Clearing,
            Status::Active,
            "Finishing",
            "Wait for it to finish.",
        ),
        (JobState::Done, Status::Ok, "Added", "Enter dismisses it."),
        (
            failed("home_copy_failed", Phase::Copying),
            Status::Fail,
            "Failed",
            "Enter tries again. d dismisses it.",
        ),
        (
            failed("home_move_source_left", Phase::Clearing),
            Status::Fail,
            "Failed",
            "Enter dismisses it.",
        ),
        (
            JobState::Cancelled {
                phase: Phase::Copying,
            },
            Status::Off,
            "Stopped",
            "Enter tries again. d dismisses it.",
        ),
        (
            JobState::NeedsAnswer {
                code: "home_move_worktrees".into(),
            },
            Status::Needs,
            "Needs answer",
            "Enter answers. d dismisses it.",
        ),
    ];
    for (state, kind, word, keys) in rows {
        let shown = job(state.clone());
        assert_eq!(badge(&shown), (kind, word), "{state:?}");
        assert_eq!(next(&shown), keys, "{state:?}");
    }
    let stopping = JobView {
        stopping: true,
        ..job(downloading)
    };
    assert_eq!(next(&stopping), "Wait for it to finish.");
    assert_eq!(name(&stopping), "shop");
    let ended = job(JobState::Done);
    let labels = [
        JobAction::Stop,
        JobAction::Retry,
        JobAction::Answer,
        JobAction::Dismiss,
    ]
    .map(|action| label(&ended, action));
    assert_eq!(labels, ["Stop", "Try again", "Answer", "Dismiss"]);
}

#[test]
fn the_header_brief_says_what_a_running_job_does_with_its_count_and_a_short_name() {
    let downloading = |done, total| JobState::Downloading {
        progress: Progress {
            total,
            done,
            ..Default::default()
        },
    };
    let briefs = [
        (job(downloading(0, 0)), "Downloading shop..."),
        (job(downloading(12, 3000)), "Downloading shop: 12 of 3000"),
        (job(copying(0, 0)), "Adding shop..."),
        (job(copying(18, 0)), "Adding shop: 18 files"),
        (job(copying(18, 5)), "Checking shop: 5 files"),
        (job(JobState::Clearing), "Finishing shop..."),
    ];
    for (shown, words) in briefs {
        assert_eq!(brief(&shown, "~"), words, "{:?}", shown.state);
    }
    let stopping = JobView {
        stopping: true,
        ..job(copying(18, 0))
    };
    assert_eq!(brief(&stopping, "~"), "Stopping shop...");
    let long = JobView {
        source: format!("/Users/mira/{}", "b".repeat(40)),
        ..job(downloading(1, 2))
    };
    assert_eq!(
        brief(&long, "~"),
        format!(
            "Downloading {}~: 1 of 2",
            "b".repeat(BRIEF_NAME_CELLS_MAX - 1)
        )
    );
    assert!(std::panic::catch_unwind(|| brief(&job(JobState::Done), "~")).is_err());
}

fn moving(state: JobState) -> JobView {
    JobView {
        left: Vec::new(),
        id: RequestId::new("move-1").unwrap(),
        kind: JobKind::MoveProject,
        project_id: Some(crate::protocol::ProjectId::new("reading").unwrap()),
        source: "/Users/mira/Code/reading-list".into(),
        mode: AddMode::Move,
        overrides: Vec::new(),
        stopping: false,
        state,
    }
}

/// DEC-197: a start past the waiting list says several folders wait, and a start on a folder
/// a live job holds points Home; neither names one running job, since several may run.
#[test]
fn a_start_past_the_waiting_list_says_several_wait_and_the_same_folder_points_home() {
    assert_eq!(
        copy("job_busy"),
        "Several folders are already waiting. Wait for one to finish on Home."
    );
    assert_eq!(
        copy("job_same_folder"),
        "This folder is already being moved or added. See it on Home."
    );
}

/// DEC-177: a project move says it waits for the project's agents, moves, and ends in the
/// move's words; Stop is offered while it waits, Retry only while nothing landed, and the
/// card, the header and the end line name the move.
#[test]
fn a_project_move_job_says_it_waits_for_its_agents_moves_and_ends_in_the_move_s_words() {
    let waiting = moving(JobState::Stopping { agents_left: 2 });
    assert_eq!(
        line(&waiting),
        "Waiting for 2 agents of reading-list to stop..."
    );
    assert_eq!(
        line(&moving(JobState::Stopping { agents_left: 1 })),
        "Waiting for 1 agent of reading-list to stop..."
    );
    assert_eq!(
        line(&moving(copying(18, 0))),
        "Moving reading-list: 18 files copied"
    );
    assert_eq!(
        line(&moving(JobState::Done)),
        "reading-list is now in your projects folder. Start the team again from Team."
    );
    assert_eq!(
        line(&moving(JobState::Cancelled {
            phase: Phase::Stopping
        })),
        "Moving reading-list stopped. Nothing was moved or copied."
    );
    let busy = line(&moving(failed("move_project_busy", Phase::Stopping)));
    assert!(
        busy.starts_with("reading-list: The project is still working"),
        "{busy}"
    );
    assert_eq!(
        line(&moving(failed("move_project_interrupted", Phase::Copying))),
        "reading-list: The move stopped part way. Check the project folder before trying again."
    );
    let left = line(&moving(failed(
        "move_project_old_folder_left",
        Phase::Clearing,
    )));
    assert!(
        left.starts_with("reading-list: The project moved, but part of its old folder"),
        "{left}"
    );
    assert!(
        !left.contains("yourself"),
        "what a move leaves may be the only copy of newer edits: {left}"
    );

    assert!(running(&waiting));
    assert_eq!(actions(&waiting), [JobAction::Stop]);
    let asked = JobView {
        stopping: true,
        ..waiting.clone()
    };
    assert_eq!(actions(&asked), []);
    assert_eq!(line(&asked), "Stopping reading-list...");
    assert_eq!(
        actions(&moving(failed("move_project_busy", Phase::Stopping))),
        [JobAction::Retry, JobAction::Dismiss]
    );
    assert_eq!(
        actions(&moving(failed(
            "move_project_old_folder_left",
            Phase::Clearing
        ))),
        [JobAction::Dismiss]
    );

    assert_eq!(badge(&waiting), (Status::Active, "Waiting"));
    assert_eq!(badge(&moving(copying(3, 0))), (Status::Active, "Moving"));
    assert_eq!(
        badge(&moving(JobState::Clearing)),
        (Status::Active, "Finishing")
    );
    assert_eq!(badge(&moving(JobState::Done)), (Status::Ok, "Moved"));
    assert_eq!(brief(&waiting, "..."), "Stopping reading-list: 2 agents");
    assert_eq!(
        brief(&moving(copying(4, 0)), "..."),
        "Moving reading-list: 4 files"
    );
    let moved = moving(JobState::Done);
    assert_eq!(
        ended(std::slice::from_ref(&waiting), std::slice::from_ref(&moved))
            .into_iter()
            .map(line)
            .collect::<Vec<_>>(),
        ["reading-list is now in your projects folder. Start the team again from Team."]
    );
}

/// A delete job of the project "notes", whose name labels it whole.
fn deleting(state: JobState) -> JobView {
    JobView {
        left: Vec::new(),
        id: crate::protocol::RequestId::new("delete-1").unwrap(),
        kind: JobKind::Delete,
        project_id: Some(crate::protocol::ProjectId::new("notes").unwrap()),
        source: "Notes/2026 plans".into(),
        mode: AddMode::Move,
        overrides: Vec::new(),
        stopping: false,
        state,
    }
}

/// DEC-177 add.1: a delete names the project whole, waits for its agents, moves its folders
/// to the Trash with no Stop once the first one moves, and once an engine stop cut it short
/// offers Finish delete and Keep the project, in the desktop's words.
#[test]
fn a_delete_job_says_it_moves_folders_to_the_trash_and_offers_finish_delete_once_cut_short() {
    let called = "Notes/2026 plans";
    let trashing = deleting(JobState::Trashing { done: 1, total: 2 });
    assert_eq!(name(&trashing), called);
    assert_eq!(
        line(&deleting(JobState::Stopping { agents_left: 2 })),
        format!("Waiting for 2 agents of {called} to stop...")
    );
    assert_eq!(
        line(&deleting(copying(0, 0))),
        format!("Deleting {called}...")
    );
    assert_eq!(
        line(&trashing),
        format!("Moving {called} to the Trash: 1 of 2 folders")
    );
    assert_eq!(
        line(&deleting(JobState::Trashing { done: 0, total: 1 })),
        format!("Moving {called} to the Trash: 0 of 1 folder")
    );
    assert_eq!(
        line(&deleting(JobState::Clearing)),
        format!("Removing {called} from CrazyAgent...")
    );
    assert_eq!(
        line(&deleting(JobState::Done)),
        format!("{called} is deleted. Its files are in the Trash.")
    );
    assert_eq!(
        line(&deleting(JobState::Cancelled {
            phase: Phase::Stopping
        })),
        format!("Deleting {called} stopped. Nothing was moved to the Trash.")
    );
    assert_eq!(
        line(&deleting(failed("delete_agents_running", Phase::Stopping))),
        format!("{called}: {}", copy("delete_agents_running"))
    );
    let cut = deleting(JobState::CutShort {
        trashed: 1,
        total: 2,
    });
    assert_eq!(
        line(&cut),
        format!(
            "Deleting {called} was cut short with 1 of 2 folders in the Trash. Finish delete or keep the project."
        )
    );

    assert!(running(&trashing));
    assert_eq!(actions(&trashing), Vec::<JobAction>::new());
    assert_eq!(badge(&trashing), (Status::Active, "Deleting"));
    assert_eq!(next(&trashing), "It can't stop now. Wait for it to finish.");
    assert_eq!(
        badge(&deleting(copying(0, 0))),
        (Status::Active, "Deleting")
    );
    assert_eq!(
        actions(&deleting(JobState::Stopping { agents_left: 1 })),
        [JobAction::Stop]
    );
    assert_eq!(badge(&deleting(JobState::Done)), (Status::Ok, "Deleted"));
    assert!(!running(&cut));
    assert_eq!(actions(&cut), [JobAction::Retry, JobAction::Dismiss]);
    assert_eq!(badge(&cut), (Status::Needs, "Cut short"));
    assert_eq!(
        next(&cut),
        "Enter finishes the delete. d keeps the project."
    );
    assert_eq!(label(&cut, JobAction::Retry), "Finish delete");
    assert_eq!(label(&cut, JobAction::Dismiss), "Keep the project");
    let stopped = deleting(JobState::Cancelled {
        phase: Phase::Stopping,
    });
    assert_eq!(label(&stopped, JobAction::Retry), "Try again");
    assert_eq!(next(&stopped), "Enter tries again. d dismisses it.");

    assert_eq!(brief(&trashing, "..."), "Deleting Notes/2026 plans: 1 of 2");
    assert_eq!(
        copy("delete_cannot_stop"),
        "It is moving its folders to the Trash, so it cannot stop now. Wait for it to finish on Home."
    );
}

/// rv-sec2 bgjobs 3b N3: a count the engine sends past its total is a wire value, not an
/// invariant, so a job read with one shows its plain line with no count, on its card and in
/// the header, and never stops the TUI.
#[test]
fn a_job_read_with_a_count_past_its_total_shows_its_plain_line_with_no_count() {
    let setup = |state| JobView {
        kind: JobKind::Setup,
        source: "plans".into(),
        ..deleting(state)
    };
    let cases = [
        (
            job(JobState::Downloading {
                progress: Progress {
                    total: 3,
                    done: 7,
                    ..Default::default()
                },
            }),
            "Downloading shop from iCloud...",
            "Downloading shop...",
        ),
        (
            setup(JobState::Building { done: 5, total: 3 }),
            "Setting up plans...",
            "Setting up plans...",
        ),
        (
            deleting(JobState::Trashing { done: 4, total: 2 }),
            "Moving Notes/2026 plans to the Trash...",
            "Deleting Notes/2026 plans...",
        ),
    ];
    for (read, said, header) in cases {
        let shown = std::panic::catch_unwind(|| line(&read)).ok();
        assert_eq!(shown.as_deref(), Some(said), "{:?}", read.state);
        let briefed = std::panic::catch_unwind(|| brief(&read, "...")).ok();
        assert_eq!(briefed.as_deref(), Some(header), "{:?}", read.state);
    }
    let cut = deleting(JobState::CutShort {
        trashed: 9,
        total: 1,
    });
    let shown = std::panic::catch_unwind(|| line(&cut)).ok();
    assert_eq!(
        shown.as_deref(),
        Some("Deleting Notes/2026 plans was cut short. Finish delete or keep the project.")
    );
}

/// The code of a refused job request arrives from the engine, so an empty one is input,
/// not an invariant: its line is the plain fallback sentence, and the TUI keeps running.
#[test]
fn a_refused_job_request_with_an_empty_code_says_the_plain_fallback() {
    let said = std::panic::catch_unwind(|| copy("")).ok();
    assert!(said.is_some_and(|line| line.starts_with("Something went wrong. Try again.")));
}

#[test]
fn the_download_line_says_macos_downloads_with_the_files_and_bytes_landed() {
    let landing = |done, total, bytes_done, bytes_total| JobState::Downloading {
        progress: Progress {
            total,
            done,
            bytes_total,
            bytes_done,
        },
    };
    assert_eq!(
        line(&job(landing(556, 127_940, 31 << 20, 2 << 30))),
        "macOS is downloading shop: 556 of 127940 files (31 MB of 2 GB)"
    );
    assert_eq!(
        line(&job(landing(1, 2, 0, 0))),
        "macOS is downloading shop: 1 of 2 files"
    );
    assert_eq!(
        line(&job(landing(1, 2, 9, 4))),
        "macOS is downloading shop: 1 of 2 files",
        "bytes past their total are left out"
    );
}

#[test]
fn a_done_add_says_which_rebuildable_folders_it_left_in_icloud_and_where() {
    let folder = |path: &str, files| crate::projects_home::icloud::Skipped {
        path: path.into(),
        files,
    };
    let left = vec![folder("node_modules", Some(57_108)), folder(".venv", None)];
    let moved = JobView {
        left: left.clone(),
        ..job(JobState::Done)
    };
    assert_eq!(
        line(&moved),
        "shop is in your projects folder. Set it up from Home. node_modules and .venv stayed in \
         /Users/mira/Library/Mobile Documents/com~apple~CloudDocs/shop; reinstall them in the project."
    );
    let copied = JobView {
        left: left[..1].to_vec(),
        mode: AddMode::Copy,
        ..job(JobState::Done)
    };
    assert_eq!(
        line(&copied),
        "shop is in your projects folder. Set it up from Home. node_modules was not copied; \
         reinstall it in the project."
    );
    let many = JobView {
        left: ["a", "b", "c", "d", "e"]
            .map(|name| folder(&format!("{name}/node_modules"), Some(1)))
            .to_vec(),
        mode: AddMode::Copy,
        ..job(JobState::Done)
    };
    assert!(
        line(&many).contains(
            " a/node_modules, b/node_modules, c/node_modules and 2 more were not copied;"
        ),
        "{}",
        line(&many)
    );
    assert_eq!(
        line(&job(JobState::Done)),
        "shop is in your projects folder. Set it up from Home."
    );
}
