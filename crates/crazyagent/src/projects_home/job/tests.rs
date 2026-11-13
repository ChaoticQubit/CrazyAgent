use super::*;

fn job() -> IntakeJob {
    IntakeJob::new(
        RequestId::new("add-1").unwrap(),
        "owner".into(),
        "/Users/someone/Library/Mobile Documents/com~apple~CloudDocs/site".into(),
        AddMode::Move,
        Vec::new(),
    )
}

fn failure(code: &str) -> Result<(), EngineError> {
    Err(EngineError(code.into()))
}

#[test]
fn a_job_reads_downloading_then_copying_with_its_counts() {
    let job = job();
    let view = job.view();
    assert_eq!(view.id.as_str(), "add-1");
    assert!(!view.stopping);
    assert_eq!(
        view.state,
        JobState::Downloading {
            progress: Progress { total: 0, done: 0 }
        }
    );
    job.copying();
    assert_eq!(
        job.view().state,
        JobState::Copying {
            copied: 0,
            checked: 0
        }
    );
    job.tally().mark_clearing();
    assert_eq!(job.view().state, JobState::Clearing);
    assert!(!job.ended());
}

#[test]
fn a_job_that_lands_ends_done_even_when_its_stop_came_too_late() {
    let job = job();
    job.copying();
    job.tally().mark_clearing();
    job.stop();
    job.end(Ok(()));
    let view = job.view();
    assert!(view.stopping);
    assert_eq!(view.state, JobState::Done);
    assert!(job.ended());
}

#[test]
fn a_stop_ends_a_job_cancelled_in_the_phase_it_reached() {
    let downloading = job();
    downloading.stop();
    assert!(downloading.download().cancelled());
    downloading.end(failure("home_icloud_not_downloaded"));
    assert_eq!(
        downloading.view().state,
        JobState::Cancelled {
            phase: Phase::Downloading
        }
    );
    let copying = job();
    copying.copying();
    copying.stop();
    copying.end(failure("home_add_cancelled"));
    assert_eq!(
        copying.view().state,
        JobState::Cancelled {
            phase: Phase::Copying
        }
    );
}

#[test]
fn a_soft_refusal_ends_a_job_waiting_for_the_owners_answer() {
    for code in NEEDS_ANSWER {
        let job = job();
        job.copying();
        job.end(failure(code));
        assert_eq!(
            job.view().state,
            JobState::NeedsAnswer { code: code.into() }
        );
    }
}

#[test]
fn a_failure_keeps_its_code_and_the_phase_it_reached() {
    let cases = [
        (0, "home_icloud_not_downloaded", Phase::Downloading),
        (1, "home_copy_failed", Phase::Copying),
        (2, "home_move_source_left", Phase::Clearing),
    ];
    for (reached, code, phase) in cases {
        let job = job();
        if reached >= 1 {
            job.copying();
        }
        if reached >= 2 {
            job.tally().mark_clearing();
        }
        job.end(failure(code));
        let failed = JobState::Failed {
            code: code.into(),
            phase,
        };
        assert_eq!(job.view().state, failed);
    }
}

#[test]
#[should_panic(expected = "a job ends once")]
fn a_job_ends_once() {
    let job = job();
    job.end(failure("home_copy_failed"));
    job.end(Ok(()));
}

#[test]
fn a_view_crosses_the_wire_as_tagged_json() {
    let job = job();
    job.copying();
    let text = serde_json::to_string(&job.view()).unwrap();
    assert!(text.contains(r#""state":"copying""#), "{text}");
    let back: JobView = serde_json::from_str(&text).unwrap();
    assert_eq!(back, job.view());
}

#[test]
fn a_job_an_engine_stop_cut_short_restores_as_the_disk_read_it() {
    let restored = |ended: &str| {
        let site = "/Users/someone/site".to_owned();
        IntakeJob::restored(id_of("add-9"), "owner".into(), site, AddMode::Move, ended)
    };
    assert_eq!(restored("done").view().state, JobState::Done);
    let left = JobState::Failed {
        code: "home_move_source_left".into(),
        phase: Phase::Clearing,
    };
    assert_eq!(restored("home_move_source_left").view().state, left);
    let interrupted = JobState::Failed {
        code: "home_add_interrupted".into(),
        phase: Phase::Copying,
    };
    let job = restored("home_add_interrupted");
    assert_eq!(job.view().state, interrupted);
    assert!(job.ended());
    assert!(!job.view().stopping);
}

fn id_of(text: &str) -> RequestId {
    RequestId::new(text).unwrap()
}
