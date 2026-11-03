#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind, ResearchStatus};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use e2e_support::{Engine, Owner, rejected, result, scratch};
use std::fs;

const PLANNING: ResearchKind = ResearchKind::RoadmapPlanning;

/// The running engine with a projects home and an attached owner.
struct Planned {
    engine: Engine,
    owner: Owner,
    _disk: tempfile::TempDir,
}

impl Planned {
    /// Starts the engine and picks a projects home through the owner protocol.
    /// Input: a scratch prefix. Output: the case.
    async fn start(prefix: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-plan-home-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        Self {
            engine,
            owner,
            _disk: disk,
        }
    }

    /// Sets up one project the way New project does, with the research the owner ticked;
    /// "Plan a roadmap after research" adds `roadmap_planning` to it. No pool is chosen,
    /// so no agent is started and every job waits for one. Inputs: the project id and the
    /// research. Output: the project.
    async fn create(&mut self, id: &str, research: Vec<ResearchKind>) -> ProjectId {
        let project = ProjectId::new(id).unwrap();
        let generation = self.owner.next();
        let response = self
            .owner
            .controller
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: format!("Buoy planner {id}"),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Plans releases for buoy firmware.".into(),
                    tech_stack: vec!["rust".into()],
                    platforms: vec![Platform::Website],
                    research,
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                generation,
            )
            .await
            .unwrap();
        assert_eq!(result(response), OwnerResult::Applied);
        project
    }

    /// The engine's research queue for one project as kind and status, in queue order.
    async fn queue(&mut self, project: &ProjectId) -> Vec<(ResearchKind, ResearchStatus)> {
        self.owner
            .controller
            .research_jobs(project.clone())
            .await
            .unwrap()
            .into_iter()
            .map(|job| (job.kind, job.status))
            .collect()
    }

    /// Whether the saved setup has the roadmap toggle on.
    async fn toggle(&mut self, project: &ProjectId) -> bool {
        self.owner
            .controller
            .project_setup(project.clone())
            .await
            .unwrap()
            .expect("the project is set up")
            .research
            .contains(&PLANNING)
    }

    /// The owner's Plan roadmap now. Input: the project. Output: the engine's result.
    async fn plan_now(&mut self, project: &ProjectId) -> OwnerResult {
        let generation = self.owner.next();
        result(
            self.owner
                .controller
                .plan_roadmap(project.clone(), generation)
                .await
                .unwrap(),
        )
    }
}

fn planning_jobs(queue: &[(ResearchKind, ResearchStatus)]) -> usize {
    queue.iter().filter(|(kind, _)| *kind == PLANNING).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn roadmap_planning_on_queues_one_planning_job_beside_the_research() {
    let mut case = Planned::start("ca-v2opt-plan-on-").await;
    let project = case
        .create("v2-plan-on", vec![ResearchKind::MarketResearch, PLANNING])
        .await;
    assert!(case.toggle(&project).await);
    let queue = case.queue(&project).await;
    assert_eq!(
        queue,
        vec![
            (ResearchKind::MarketResearch, ResearchStatus::NeedsPool),
            (PLANNING, ResearchStatus::NeedsPool),
        ],
        "On queues the planning job beside the research it plans from"
    );
    assert_eq!(
        case.plan_now(&project).await,
        rejected("planning_busy"),
        "the waiting plan runs once: Plan now cannot queue a second"
    );
    case.engine.restart();
    case.owner = Owner::attach(&case.engine).await;
    let kept = case.queue(&project).await;
    assert_eq!(
        planning_jobs(&kept),
        1,
        "the queued plan survives a restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn roadmap_planning_off_queues_no_planning_job() {
    let mut case = Planned::start("ca-v2opt-plan-off-").await;
    let project = case
        .create("v2-plan-off", vec![ResearchKind::MarketResearch])
        .await;
    assert!(!case.toggle(&project).await);
    let queue = case.queue(&project).await;
    assert_eq!(
        queue,
        vec![(ResearchKind::MarketResearch, ResearchStatus::NeedsPool)],
        "Off queues only the research"
    );
    case.engine.restart();
    case.owner = Owner::attach(&case.engine).await;
    let kept = case.queue(&project).await;
    assert_eq!(planning_jobs(&kept), 0, "no plan appears after a restart");
    assert!(!case.toggle(&project).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_roadmap_now_queues_the_planning_job_and_turns_the_toggle_on() {
    let mut case = Planned::start("ca-v2opt-plan-now-").await;
    let project = case
        .create("v2-plan-now", vec![ResearchKind::MarketResearch])
        .await;
    assert_eq!(planning_jobs(&case.queue(&project).await), 0);

    assert_eq!(case.plan_now(&project).await, OwnerResult::Applied);
    let queue = case.queue(&project).await;
    assert_eq!(
        queue.iter().find(|(kind, _)| *kind == PLANNING),
        Some(&(PLANNING, ResearchStatus::NeedsPool)),
        "Plan now queues the planning job: {queue:?}"
    );
    assert_eq!(planning_jobs(&queue), 1);
    assert!(case.toggle(&project).await, "Plan now turns the toggle on");
    assert_eq!(case.plan_now(&project).await, rejected("planning_busy"));
    assert_eq!(planning_jobs(&case.queue(&project).await), 1);
}
