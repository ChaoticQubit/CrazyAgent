use super::*;
use crate::projects_home::AddOverride;
use crate::projects_home::icloud::Progress;
use crate::protocol::RequestId;

fn job(state: JobState) -> JobView {
    JobView {
        id: RequestId::new("add-1").unwrap(),
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
        progress: Progress { total, done },
    };
    assert_eq!(
        line(&job(downloading(0, 0))),
        "Downloading shop from iCloud..."
    );
    assert_eq!(
        line(&job(downloading(12, 3000))),
        "Downloading shop from iCloud: 12 of 3000 files"
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
        "intake_busy",
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

#[test]
fn a_jobs_end_is_said_once_when_the_read_after_a_running_one_shows_it_ended() {
    let running_job = job(copying(3, 0));
    let done = job(JobState::Done);
    assert_eq!(
        end_line(Some(&running_job), Some(&done)).as_deref(),
        Some("shop is in your projects folder. Set it up from Home.")
    );
    assert_eq!(end_line(Some(&done), Some(&done)), None);
    assert_eq!(end_line(None, Some(&done)), None);
    assert_eq!(end_line(Some(&running_job), Some(&running_job)), None);
    assert_eq!(end_line(Some(&running_job), None), None);
    let other = JobView {
        id: RequestId::new("add-2").unwrap(),
        ..done
    };
    assert_eq!(end_line(Some(&running_job), Some(&other)), None);
}

#[test]
fn home_s_job_card_names_the_state_in_the_desktop_s_words_and_says_what_its_keys_do() {
    let downloading = JobState::Downloading {
        progress: Progress { total: 3, done: 1 },
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
    let labels = [
        JobAction::Stop,
        JobAction::Retry,
        JobAction::Answer,
        JobAction::Dismiss,
    ]
    .map(label);
    assert_eq!(labels, ["Stop", "Try again", "Answer", "Dismiss"]);
}
