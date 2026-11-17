#![cfg(target_os = "macos")]

mod e2e_support;
mod golden_support;

use crazyagent::client_controller::OwnerController;
use crazyagent::config::{AccountPool, ModelAccessEdit};
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerData, OwnerMutation, OwnerRead, OwnerResult,
    OwnerSummary, OwnerView,
};
use crazyagent::project_setup::{Platform, ProjectSetup};
use crazyagent::projects_home::AddMode;
use crazyagent::projects_home::job::{JobKind, JobState, JobSubject};
use crazyagent::protocol::{ConfigId, CredentialRef, ProjectId, ProjectSpec, RequestId};
use crazyagent::sandbox::policy::{AgentAccess, AgentPolicy};
use golden_support::{Golden, landed, wait_until};
use std::path::Path;
use std::time::Duration;

const FLOW: &str = "add-project";
const NAME: &str = "Tide Gauge";
const POOL: &str = "local-pool";
const JOB_TIME: Duration = Duration::from_secs(90);
const STEPS: [&str; 9] = [
    "start the engine",
    "read New project's choices",
    "create the project",
    "wait for the job to end",
    "dismiss the ended job",
    "assign the chosen pool",
    "save the chosen agent access",
    "find the project set up",
    "stop the engine",
];
const GLOBAL: &str = r#"version = 1
timeout_ms = 5000
allowed_destinations = []

[model_access]
accounts = ["local-account"]

[[model_access.connections]]
id = "local-connection"
provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local-account"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "golden-model", author = "openai-compatible" }
auth = { method = "none" }

[[model_access.pools]]
id = "local-pool"
members = ["local-connection"]
projects = []
roles = []
fallback_allowed = false
"#;

/// Reads the saved pools as Model access does, all on one writable page.
/// Input: the owner's controller.
/// Output: the settings version and source hash a change names, and the pools.
async fn pools(owner: &mut OwnerController) -> (ConfigId, String, Vec<AccountPool>) {
    let query = OwnerRead::ModelAccessPage {
        collection: ModelAccessCollection::Pools,
        cursor: None,
        limit: 16,
    };
    let reply = owner.read(query).await.unwrap();
    let OwnerData::ModelAccessPage {
        source_hash,
        writable: true,
        items,
        next: None,
        ..
    } = reply.data
    else {
        panic!("the pools are one writable page");
    };
    let pools = items
        .into_iter()
        .map(|item| match item {
            ModelAccessRecord::Pool { pool } => pool,
            other => panic!("a pool page holds pools: {other:?}"),
        })
        .collect();
    (reply.config_id, source_hash, pools)
}

/// Whether a job has ended, by its state.
/// Input: the state.
/// Output: true once no step of the job is left to run.
fn ended(state: &JobState) -> bool {
    matches!(
        state,
        JobState::Done
            | JobState::Failed { .. }
            | JobState::Cancelled { .. }
            | JobState::NeedsAnswer { .. }
            | JobState::CutShort { .. }
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_adds_a_project_through_new_project_and_finds_it_set_up() {
    let mut golden = Golden::start(FLOW, GLOBAL).await;
    let pool = CredentialRef::new(POOL).unwrap();

    let catalog = golden
        .owner
        .controller
        .project_catalog(Some(pool.clone()), Vec::new())
        .await
        .unwrap();
    let levels = &catalog.agent_access;
    let own_branch = levels
        .iter()
        .find(|level| level.access == AgentAccess::OwnBranch)
        .map(|level| level.unavailable.as_deref());
    assert_eq!(
        own_branch,
        Some(None),
        "New project offers Own branch: {levels:?}"
    );
    golden.step(STEPS[1]);

    let project = ProjectId::new(format!("project-{}", e2e_support::hex(32))).unwrap();
    let request = RequestId::new(format!("golden-{}", e2e_support::hex(16))).unwrap();
    let subject = JobSubject::Setup {
        project_id: project.clone(),
        spec: ProjectSpec {
            name: NAME.into(),
            repositories: Vec::new(),
        },
        setup: ProjectSetup {
            about: "Reads buoy sensors and raises alerts.".into(),
            tech_stack: Vec::new(),
            platforms: vec![Platform::Website],
            research: Vec::new(),
            pool: Some(pool.clone()),
            team: None,
            documents: Vec::new(),
        },
    };
    let started = golden
        .owner
        .controller
        .job_start(request.clone(), subject, AddMode::Move, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        (
            started.kind,
            started.project_id.as_ref(),
            started.source.as_str()
        ),
        (JobKind::Setup, Some(&project), NAME)
    );
    golden.step(STEPS[2]);

    let controller = &mut golden.owner.controller;
    let state = wait_until("the New Project job to end", JOB_TIME, async || {
        let job = controller.job_status().await.unwrap()?;
        (job.id == request && ended(&job.state)).then_some(job.state)
    })
    .await;
    assert_eq!(state, JobState::Done);
    golden.step(STEPS[3]);

    let dismissed = golden.owner.controller.job_dismiss(request.clone()).await;
    assert!(dismissed.unwrap(), "the ended job is dismissed");
    assert_eq!(golden.owner.controller.job_status().await.unwrap(), None);
    golden.step(STEPS[4]);

    let (config_id, source_hash, _) = pools(&mut golden.owner.controller).await;
    let generation = golden.owner.next();
    let assign = OwnerMutation::ModelAccess {
        expected_config_id: config_id,
        expected_source_hash: source_hash,
        edit: ModelAccessEdit::AssignProjectPool {
            project: CredentialRef::new(project.as_str()).unwrap(),
            pool: Some(pool.clone()),
        },
    };
    golden
        .owner
        .controller
        .begin_mutation(assign, generation)
        .unwrap();
    let assigned = golden.owner.controller.send_pending().await.unwrap();
    assert_eq!(landed(assigned), OwnerResult::Applied);
    golden.step(STEPS[5]);

    let view = golden
        .owner
        .controller
        .project_agent_policy(project.clone())
        .await
        .unwrap();
    assert_eq!(
        view.access,
        AgentAccess::Sandboxed,
        "a new project starts Sandboxed"
    );
    let generation = golden.owner.next();
    let policy = AgentPolicy {
        access: AgentAccess::OwnBranch,
        ..AgentPolicy::default()
    };
    let saved = golden
        .owner
        .controller
        .set_project_agent_policy(project.clone(), view.version, policy, generation)
        .await
        .unwrap();
    assert_eq!(landed(saved), OwnerResult::Applied);
    golden.step(STEPS[6]);

    let listed = golden
        .owner
        .controller
        .read_page(OwnerView::Projects, None, Some(32))
        .await
        .unwrap();
    let OwnerData::Page { items, next: None } = listed.data else {
        panic!("the projects are one page");
    };
    let rows: Vec<(&ProjectId, &str)> = items
        .iter()
        .filter_map(|item| match item {
            OwnerSummary::Project {
                project_id, name, ..
            } => Some((project_id, name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [(&project, NAME)], "Home lists the new project");
    let setup = golden.owner.controller.project_setup(project.clone()).await;
    let setup = setup.unwrap().expect("the project's setup is recorded");
    assert_eq!(
        (setup.name.as_str(), setup.pool.as_ref()),
        (NAME, Some(&pool))
    );
    let folder = Path::new(&setup.folder);
    assert!(
        folder.starts_with(&golden.projects) && folder.is_dir(),
        "the project's folder is made in the projects folder"
    );
    let (_, _, saved_pools) = pools(&mut golden.owner.controller).await;
    let served: Vec<&str> = saved_pools
        .iter()
        .filter(|saved| saved.id == pool)
        .flat_map(|saved| saved.projects.iter().map(CredentialRef::as_str))
        .collect();
    assert_eq!(
        served,
        [project.as_str()],
        "the chosen pool serves the project"
    );
    let access = golden.owner.controller.project_agent_policy(project).await;
    assert_eq!(access.unwrap().access, AgentAccess::OwnBranch);
    golden.step(STEPS[7]);

    golden.engine.stop();
    golden.step(STEPS[8]);

    let record = golden.finish();
    let steps: Option<Vec<&str>> = record
        .as_ref()
        .and_then(|record| record["steps"].as_array())
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| step["step"].as_str())
                .collect()
        });
    let outcome = record
        .as_ref()
        .and_then(|record| record["outcome"].as_str());
    assert_eq!(
        (steps, outcome),
        (Some(STEPS.to_vec()), Some("passed")),
        "the run record names each step in order"
    );
}
