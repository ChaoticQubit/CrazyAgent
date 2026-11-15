use super::*;
use crate::projects_home::job::{JobKind, JobState, JobSubject, JobView, Phase};
use crate::protocol::RequestId;

const NAME: &str = "Notes";
const OPEN_MARKS: &str =
    "SELECT count(*) FROM ledger_actions WHERE kind='project_setup' AND outcome IS NULL";

/// A New Project with no research, no pool and no documents, the quickest build.
fn quiet() -> ProjectSetup {
    ProjectSetup {
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: None,
        documents: vec![],
    }
}

/// The subject that starts a New Project job. Inputs: the project id and its name. Output:
/// the subject.
fn setting_up(project: &str, name: &str) -> JobSubject {
    JobSubject::Setup {
        project_id: ProjectId::new(project).unwrap(),
        spec: spec(name),
        setup: quiet(),
    }
}

/// Starts a job through the owner client. Inputs: the client, the request id and the subject.
/// Output: the start's answer.
async fn start(
    owner: &mut OwnerController,
    id: &str,
    subject: JobSubject,
) -> Result<JobView, crate::engine::EngineError> {
    owner
        .job_start(
            RequestId::new(id).unwrap(),
            subject,
            crate::projects_home::AddMode::Move,
            Vec::new(),
        )
        .await
}

/// Reads the engine's job until `want` holds, within the engine wait. Inputs: the client and
/// the check. Output: the job.
async fn until(owner: &mut OwnerController, want: impl Fn(&JobView) -> bool) -> JobView {
    let deadline = std::time::Instant::now() + ENGINE_WAIT * 3;
    loop {
        if let Some(job) = owner.job_status().await.unwrap().filter(&want) {
            return job;
        }
        assert!(std::time::Instant::now() < deadline, "no such job");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Runs SQL on the test engine's database. Inputs: the engine and the statements.
fn write(engine: &Engine, sql: &str) {
    rusqlite::Connection::open(engine.root.path().join("engine.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// The New Project staging folders left in the engine's home.
fn staging_left(engine: &Engine) -> Vec<String> {
    std::fs::read_dir(engine.home())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".crazyagent-new-"))
        .collect()
}

/// Seeds the mark a New Project job leaves when an engine stop cuts it short, as its plan
/// writes it. Inputs: the engine, the project, the request and the name.
fn cut_short_setup(engine: &Engine, project: &str, request: &str, name: &str) {
    drop(crate::ledger::open(engine.root.path()).unwrap());
    write(
        engine,
        &format!(
            "INSERT INTO ledger_actions(kind,action_key,payload,started_ms)
             VALUES ('project_setup','{project}',
                     '{{\"project\":\"{project}\",\"request\":\"{request}\",\"name\":\"{name}\"}}',1);"
        ),
    );
}

/// DEC-177 add.2: a New Project runs as the engine's job, named by the project's name, and
/// ends done with the project recorded in its folder, no staging folder left and its mark
/// ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_setup_job_builds_the_project_and_ends_done_naming_it() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let started = start(&mut owner, "setup-1", setting_up("setup-job", NAME))
        .await
        .unwrap();
    assert_eq!(started.kind, JobKind::Setup);
    assert_eq!(
        started.project_id,
        Some(ProjectId::new("setup-job").unwrap())
    );
    assert_eq!(started.source, NAME);
    let done = until(&mut owner, |job| job.state == JobState::Done).await;
    assert_eq!(done.id.as_str(), "setup-1");
    let stored = owner
        .project_setup(ProjectId::new("setup-job").unwrap())
        .await
        .unwrap()
        .expect("the project is recorded");
    assert_eq!(stored.project_folder(), engine.home().join("notes"));
    assert!(engine.home().join("notes").is_dir());
    assert_eq!(staging_left(&engine), Vec::<String>::new());
    assert_eq!(engine.rows(OPEN_MARKS), 0, "the record ended the mark");
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2: a New Project job carries every answer the wizard gives, so one with the
/// longest About text the setup takes starts and ends done, as the same New Project does
/// from a client's direct change.
#[tokio::test(flavor = "multi_thread")]
async fn a_setup_job_with_the_longest_about_text_starts_and_ends_done() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let mut setup = quiet();
    setup.about = "a".repeat(ABOUT_BYTES_MAX);
    let subject = JobSubject::Setup {
        project_id: ProjectId::new("setup-long").unwrap(),
        spec: spec(NAME),
        setup,
    };
    start(&mut owner, "setup-long", subject).await.unwrap();
    let ended = until(&mut owner, |job| {
        !matches!(job.state, JobState::Building { .. })
    })
    .await;
    assert_eq!(ended.state, JobState::Done);
    let stored = owner
        .project_setup(ProjectId::new("setup-long").unwrap())
        .await
        .unwrap()
        .expect("the project is recorded");
    assert_eq!(stored.about.len(), ABOUT_BYTES_MAX);
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2: a New Project job for a project id the engine already has is refused at its
/// start, as the build's own check refuses it, with no job held and nothing built.
#[tokio::test(flavor = "multi_thread")]
async fn a_setup_job_for_a_project_that_exists_is_refused_at_its_start_with_no_job_held() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let response = owner
        .setup_project(ProjectId::new("setup-job").unwrap(), spec(NAME), quiet(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    let refused = start(&mut owner, "setup-again", setting_up("setup-job", "Other"))
        .await
        .unwrap_err();
    assert_eq!(refused.0, "project_exists");
    assert_eq!(owner.job_status().await.unwrap(), None);
    assert!(!engine.home().join("other").exists());
    assert_eq!(engine.rows(OPEN_MARKS), 0);
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2: a New Project job whose build fails, here with a file where the engine's
/// projects folder goes, ends failed while building, with no project recorded, no folder or
/// staging folder in the home and its mark ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_setup_job_whose_build_fails_ends_failed_while_building_with_nothing_added() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let projects = engine.root.path().join(PROJECTS_DIRECTORY);
    match std::fs::remove_dir_all(&projects) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("{e}"),
    }
    std::fs::write(&projects, "not a folder").unwrap();
    start(&mut owner, "setup-broken", setting_up("setup-job", NAME))
        .await
        .unwrap();
    let ended = until(&mut owner, |job| {
        !matches!(job.state, JobState::Building { .. })
    })
    .await;
    assert_eq!(
        ended.state,
        JobState::Failed {
            code: "home_new_project_failed".into(),
            phase: Phase::Building
        }
    );
    assert!(
        owner
            .project_setup(ProjectId::new("setup-job").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(!engine.home().join("notes").exists());
    assert_eq!(staging_left(&engine), Vec::<String>::new());
    assert_eq!(
        engine.rows(OPEN_MARKS),
        0,
        "the failed record ended the mark"
    );
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2 Q2 A: while a job runs, here a delete waiting for its project's agent, a New
/// Project job is refused `job_busy`, and a direct New Project or the demo from any client is
/// refused `job_busy` too, so one job at a time holds for every way a folder is built.
/// Runs on macOS only, the one platform that offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn while_a_job_runs_a_setup_job_a_direct_new_project_and_the_demo_are_refused_busy() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let held = ProjectId::new("setup-pooled").unwrap();
    let response = owner
        .setup_project(held.clone(), spec("Meal planner"), pooled_setup(vec![]), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    write(
        &engine,
        &format!(
            "INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms)
             VALUES (47,'setup-pooled','team','codex',1,'{}','working',0,0);",
            engine.root.path().join("agent-47").display()
        ),
    );
    let deleting = JobSubject::Delete {
        project_id: held.clone(),
        name: "Meal planner".into(),
    };
    start(&mut owner, "delete-held", deleting).await.unwrap();
    until(&mut owner, |job| {
        job.state == JobState::Stopping { agents_left: 1 }
    })
    .await;
    let refused = start(&mut owner, "setup-busy", setting_up("setup-job", NAME))
        .await
        .unwrap_err();
    assert_eq!(refused.0, "job_busy");
    let direct = owner
        .setup_project(ProjectId::new("setup-job").unwrap(), spec(NAME), quiet(), 3)
        .await
        .unwrap();
    assert_eq!(
        direct,
        OwnerResponse::Rejected {
            code: "job_busy".into()
        }
    );
    let demo = owner
        .create_demo(CredentialRef::new("pool-team").unwrap(), 4)
        .await
        .unwrap();
    assert_eq!(
        demo,
        OwnerResponse::Rejected {
            code: "job_busy".into()
        }
    );
    assert!(!engine.home().join("notes").exists());
    assert_eq!(staging_left(&engine), Vec::<String>::new());
    assert!(
        owner
            .job_stop(RequestId::new("delete-held").unwrap())
            .await
            .unwrap()
    );
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2 Q3 A: a New Project an engine stop cut short shows after the next start as
/// the job failed while building with the interrupted code, naming the project; its mark ends
/// at that start, Dismiss forgets the job, and the start after shows none.
#[tokio::test(flavor = "multi_thread")]
async fn an_engine_start_shows_a_setup_an_engine_stop_cut_short_and_dismiss_clears_it() {
    let mut engine = Engine::start().await;
    cut_short_setup(&engine, "setup-cut", "setup-cut-1", "Cut Notes");
    engine.restart().await;
    let mut owner = engine.owner().await;
    let shown = owner
        .job_status()
        .await
        .unwrap()
        .expect("the New Project cut short");
    assert_eq!(shown.id.as_str(), "setup-cut-1");
    assert_eq!(shown.kind, JobKind::Setup);
    assert_eq!(shown.project_id, Some(ProjectId::new("setup-cut").unwrap()));
    assert_eq!(shown.source, "Cut Notes");
    assert_eq!(
        shown.state,
        JobState::Failed {
            code: "home_new_project_interrupted".into(),
            phase: Phase::Building
        }
    );
    assert_eq!(engine.rows(OPEN_MARKS), 0, "the start ended the mark");
    assert!(
        owner
            .job_dismiss(RequestId::new("setup-cut-1").unwrap())
            .await
            .unwrap()
    );
    assert_eq!(owner.job_status().await.unwrap(), None);
    drop(owner);
    engine.restart().await;
    let mut owner = engine.owner().await;
    assert_eq!(owner.job_status().await.unwrap(), None);
    drop(owner);
    engine.stop().await;
}

/// DEC-177 add.2 Q3 A: the mark of a New Project whose record went through before an engine
/// stop ends done at the next start, and no job is shown for it.
#[tokio::test(flavor = "multi_thread")]
async fn an_engine_start_ends_the_mark_of_a_recorded_project_done_and_shows_no_job() {
    let mut engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let response = owner
        .setup_project(ProjectId::new("setup-job").unwrap(), spec(NAME), quiet(), 2)
        .await
        .unwrap();
    assert_eq!(result(response), OwnerResult::Applied);
    drop(owner);
    cut_short_setup(&engine, "setup-job", "setup-recorded", NAME);
    engine.restart().await;
    let mut owner = engine.owner().await;
    assert_eq!(owner.job_status().await.unwrap(), None);
    assert_eq!(engine.rows(OPEN_MARKS), 0);
    assert_eq!(
        engine.rows(
            "SELECT count(*) FROM ledger_actions
             WHERE kind='project_setup' AND action_key='setup-job' AND outcome='done'"
        ),
        1
    );
    drop(owner);
    engine.stop().await;
}
