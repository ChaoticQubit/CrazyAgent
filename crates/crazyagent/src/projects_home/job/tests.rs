use super::*;

fn job() -> IntakeJob {
    IntakeJob::new(
        RequestId::new("add-1").unwrap(),
        JobKind::Add,
        None,
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
fn a_view_shows_the_overrides_the_job_goes_past_so_an_answer_keeps_them() {
    let job = IntakeJob::new(
        RequestId::new("add-2").unwrap(),
        JobKind::Add,
        None,
        "/Users/someone/site".into(),
        AddMode::Copy,
        vec![AddOverride::TooLarge],
    );
    assert_eq!(job.view().overrides, vec![AddOverride::TooLarge]);
    job.copying();
    job.end(failure("home_copy_linked_checkout"));
    let view = job.view();
    assert_eq!(view.overrides, vec![AddOverride::TooLarge]);
    assert!(matches!(view.state, JobState::NeedsAnswer { .. }));
    assert!(self::job().view().overrides.is_empty());
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
        IntakeJob::restored(
            id_of("add-9"),
            JobKind::Add,
            None,
            site,
            AddMode::Move,
            ended,
        )
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

fn move_job() -> IntakeJob {
    IntakeJob::new(
        id_of("move-1"),
        JobKind::MoveProject,
        Some(ProjectId::new("reading").unwrap()),
        "/Users/someone/Code/reading-list".into(),
        AddMode::Move,
        Vec::new(),
    )
}

#[test]
fn a_move_job_reads_its_agents_stopping_and_a_stop_there_cancels_it_in_that_phase() {
    let job = move_job();
    job.copying();
    job.tally().wait_for(2);
    assert_eq!(job.view().state, JobState::Stopping { agents_left: 2 });
    job.tally().wait_for(1);
    assert_eq!(job.view().state, JobState::Stopping { agents_left: 1 });
    job.stop();
    job.end(failure("home_add_cancelled"));
    let cancelled = JobState::Cancelled {
        phase: Phase::Stopping,
    };
    assert_eq!(job.view().state, cancelled);
    let busy = move_job();
    busy.copying();
    busy.tally().wait_for(1);
    busy.end(failure("move_project_busy"));
    let failed = JobState::Failed {
        code: "move_project_busy".into(),
        phase: Phase::Stopping,
    };
    assert_eq!(busy.view().state, failed);
}

#[test]
fn a_move_job_copies_once_its_agents_stopped_and_names_its_kind_and_project() {
    let job = move_job();
    job.copying();
    job.tally().wait_for(1);
    job.tally().waited();
    assert_eq!(
        job.view().state,
        JobState::Copying {
            copied: 0,
            checked: 0
        }
    );
    let view = job.view();
    assert_eq!(view.kind, JobKind::MoveProject);
    assert_eq!(view.project_id, Some(ProjectId::new("reading").unwrap()));
    let subject = JobSubject::MoveProject {
        project_id: ProjectId::new("reading").unwrap(),
    };
    assert_eq!(view.subject(), Some(subject));
    assert_eq!(job.interrupted(), "move_project_interrupted");
    assert_eq!(self::job().interrupted(), "home_add_interrupted");
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains(r#""kind":"move_project""#), "{text}");
    assert!(text.contains(r#""project_id":"reading""#), "{text}");
}

fn delete_job() -> IntakeJob {
    IntakeJob::new(
        id_of("delete-1"),
        JobKind::Delete,
        Some(ProjectId::new("meals").unwrap()),
        "Meal planner".into(),
        AddMode::Move,
        Vec::new(),
    )
}

/// DEC-177 step 2: a Delete everything waits for its agents, then moves its folders to the
/// Trash one by one, then clears the engine's files with no Stop. Once it committed to the
/// first Trash move a stop is refused, so a half-deleted project never comes from a Stop.
#[test]
fn a_delete_job_reads_stopping_then_trashing_then_clearing_and_refuses_a_stop_once_committed() {
    let job = delete_job();
    job.copying();
    job.tally().wait_for(2);
    assert_eq!(job.view().state, JobState::Stopping { agents_left: 2 });
    job.tally().waited();
    job.tally().commit_trash(2).unwrap();
    assert_eq!(job.view().state, JobState::Trashing { done: 0, total: 2 });
    job.tally().trashed_one();
    assert_eq!(job.view().state, JobState::Trashing { done: 1, total: 2 });
    assert!(!job.stop(), "a stop after the first Trash move is refused");
    assert!(!job.view().stopping);
    job.tally().trashed_one();
    job.tally().mark_clearing();
    assert_eq!(job.view().state, JobState::Clearing);
    job.end(Ok(()));
    let view = job.view();
    assert_eq!(view.state, JobState::Done);
    assert_eq!(view.kind, JobKind::Delete);
    assert_eq!(view.project_id, Some(ProjectId::new("meals").unwrap()));
    assert_eq!(view.source, "Meal planner", "the name labels the end line");
    assert_eq!(job.interrupted(), "project_delete_interrupted");
}

/// A stop that comes before the delete commits wins: the commit is refused, nothing moves and
/// the job ends cancelled while its agents were stopping.
#[test]
fn a_delete_stopped_before_it_commits_moves_nothing_and_ends_cancelled_while_stopping() {
    let job = delete_job();
    job.copying();
    assert!(job.stop());
    assert_eq!(
        job.tally().commit_trash(1).unwrap_err().0,
        "home_add_cancelled"
    );
    assert_eq!(job.tally().trashing(), None, "nothing was committed");
    job.end(failure("home_add_cancelled"));
    assert_eq!(
        job.view().state,
        JobState::Cancelled {
            phase: Phase::Stopping
        }
    );
}

/// A delete's failure keeps the phase it reached: past the agent wait while stopping, a
/// Trash move that failed while trashing.
#[test]
fn a_delete_failure_keeps_the_phase_it_reached() {
    let waited = delete_job();
    waited.copying();
    waited.tally().wait_for(1);
    waited.end(failure("delete_agents_running"));
    assert_eq!(
        waited.view().state,
        JobState::Failed {
            code: "delete_agents_running".into(),
            phase: Phase::Stopping
        }
    );
    let partial = delete_job();
    partial.copying();
    partial.tally().commit_trash(2).unwrap();
    partial.tally().trashed_one();
    partial.end(failure("delete_trash_partial"));
    assert_eq!(
        partial.view().state,
        JobState::Failed {
            code: "delete_trash_partial".into(),
            phase: Phase::Trashing
        }
    );
}

/// A delete an engine stop cut short restores with how many of its folders reached the Trash.
/// It starts again only from the remove ask, where the exact name is typed (DEC-177 add.1 Q1
/// A), so its view offers no subject to restart from.
#[test]
fn a_delete_cut_short_reads_its_trash_count_and_offers_no_subject_to_restart() {
    let job = IntakeJob::cut_short(
        id_of("delete-7"),
        ProjectId::new("meals").unwrap(),
        "Meal planner".into(),
        1,
        2,
    );
    assert!(job.ended());
    let view = job.view();
    assert_eq!(
        view.state,
        JobState::CutShort {
            trashed: 1,
            total: 2
        }
    );
    assert_eq!(view.kind, JobKind::Delete);
    assert_eq!(view.subject(), None);
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains(r#""kind":"delete""#), "{text}");
    assert!(text.contains(r#""state":"cut_short""#), "{text}");
    let started = JobSubject::Delete {
        project_id: ProjectId::new("meals").unwrap(),
        name: "Meal planner".into(),
    };
    assert_eq!(started.kind(), JobKind::Delete);
}

fn setup_job() -> IntakeJob {
    IntakeJob::new(
        id_of("setup-1"),
        JobKind::Setup,
        Some(ProjectId::new("meals").unwrap()),
        "Meal planner".into(),
        AddMode::Move,
        Vec::new(),
    )
}

/// DEC-177 add.2: a New Project job reads Building from its start, then how many of its
/// documents it copied once it counted them, and refuses a stop once its record starts, so a
/// stop never leaves a half-recorded project; it ends done naming the project.
#[test]
fn a_setup_job_reads_building_with_its_documents_counted_and_refuses_a_stop_once_recording() {
    let job = setup_job();
    job.copying();
    assert_eq!(job.view().state, JobState::Building { done: 0, total: 0 });
    job.tally().documents(2).unwrap();
    assert_eq!(job.view().state, JobState::Building { done: 0, total: 2 });
    job.tally().document_copied();
    assert_eq!(job.view().state, JobState::Building { done: 1, total: 2 });
    job.tally().document_copied();
    job.tally().commit_record().unwrap();
    assert!(job.tally().recording());
    assert!(!job.stop(), "a stop once the record started is refused");
    assert!(!job.view().stopping);
    assert_eq!(job.view().state, JobState::Building { done: 2, total: 2 });
    job.end(Ok(()));
    let view = job.view();
    assert_eq!(view.state, JobState::Done);
    assert_eq!(view.kind, JobKind::Setup);
    assert_eq!(view.project_id, Some(ProjectId::new("meals").unwrap()));
    assert_eq!(view.source, "Meal planner", "the name labels the end line");
    assert_eq!(job.interrupted(), "home_new_project_interrupted");
}

/// A stop while a New Project builds wins over its record: the record is refused and the job
/// ends cancelled while building.
#[test]
fn a_setup_stopped_while_building_ends_cancelled_while_building() {
    let job = setup_job();
    job.copying();
    job.tally().documents(3).unwrap();
    job.tally().document_copied();
    assert!(job.stop());
    assert_eq!(
        job.tally().commit_record().unwrap_err().0,
        "home_add_cancelled"
    );
    assert!(!job.tally().recording(), "nothing was recorded");
    assert_eq!(
        job.tally().documents(1).unwrap_err().0,
        "home_add_cancelled",
        "a stopped build counts nothing more"
    );
    job.end(failure("home_add_cancelled"));
    assert_eq!(
        job.view().state,
        JobState::Cancelled {
            phase: Phase::Building
        }
    );
}

/// A New Project's failure ends it failed while building, whatever step failed.
#[test]
fn a_setup_failure_keeps_the_building_phase() {
    let job = setup_job();
    job.copying();
    job.tally().documents(1).unwrap();
    job.end(failure("home_new_project_failed"));
    assert_eq!(
        job.view().state,
        JobState::Failed {
            code: "home_new_project_failed".into(),
            phase: Phase::Building
        }
    );
}

/// A New Project an engine stop cut short restores as failed while building with the
/// interrupted code, naming the project; it starts again only from New Project, whose answers
/// are not kept (DEC-177 add.2 Q3 A), so its view offers no subject to restart from.
#[test]
fn a_setup_cut_short_restores_failed_interrupted_with_no_subject_to_restart() {
    let job = IntakeJob::setup_interrupted(
        id_of("setup-7"),
        ProjectId::new("meals").unwrap(),
        "Meal planner".into(),
    );
    assert!(job.ended());
    let view = job.view();
    assert_eq!(
        view.state,
        JobState::Failed {
            code: "home_new_project_interrupted".into(),
            phase: Phase::Building
        }
    );
    assert_eq!(view.kind, JobKind::Setup);
    assert_eq!(view.source, "Meal planner");
    assert_eq!(view.subject(), None);
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains(r#""kind":"setup""#), "{text}");
    assert!(text.contains(r#""phase":"building""#), "{text}");
    let started = JobSubject::Setup {
        project_id: ProjectId::new("meals").unwrap(),
        spec: crate::protocol::ProjectSpec {
            name: "Meal planner".into(),
            repositories: vec![],
        },
        setup: crate::project_setup::ProjectSetup {
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: None,
            documents: vec![],
        },
    };
    assert_eq!(started.kind(), JobKind::Setup);
}

#[test]
fn a_status_list_of_every_held_job_at_its_longest_fits_one_frame() {
    let quotes = |bytes: usize| "\"".repeat(bytes);
    let project = ProjectId::new("p".repeat(128)).unwrap();
    let source = format!("/{}", quotes(1023));
    let overrides = vec![
        AddOverride::Worktrees,
        AddOverride::LinkedCheckout,
        AddOverride::TooLarge,
    ];
    let mut views = Vec::new();
    for at in 0..22 {
        let job = IntakeJob::new(
            RequestId::new(format!("{at:0>128}")).unwrap(),
            JobKind::MoveProject,
            Some(project.clone()),
            source.clone(),
            AddMode::Copy,
            overrides.clone(),
        );
        match at {
            0..=7 => job.wait(&quotes(1024)),
            8 => job.end(failure("home_copy_linked_checkout")),
            _ => job.end(Err(EngineError(format!(
                "home_copy_failed: {}",
                quotes(4096)
            )))),
        }
        views.push(job.view());
    }
    let reply = serde_json::json!({"kind": "jobs", "jobs": views});
    let frame = crate::protocol::encode_frame(&reply).expect("every held job fits one frame");
    let read: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
    let listed: Vec<JobView> = serde_json::from_value(read["jobs"].clone()).unwrap();
    assert_eq!(listed.len(), 22);
    for (at, view) in listed.iter().enumerate() {
        assert_eq!(view.id.as_str(), format!("{at:0>128}"));
        assert_eq!(view.source, source, "a source is never cut");
    }
    assert_eq!(
        listed[8].state,
        JobState::NeedsAnswer {
            code: "home_copy_linked_checkout".into()
        }
    );
    let JobState::Failed { code, .. } = &listed[9].state else {
        panic!("a failed end: {:?}", listed[9].state);
    };
    assert!(code.starts_with("home_copy_failed: "), "{code}");
}
