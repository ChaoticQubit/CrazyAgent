use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::project_setup::{
    ContextKind, Platform, ProjectSetup, ResearchKind, ResearchStatus,
};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Engine {
    child: Child,
}

impl Engine {
    async fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(child.try_wait().unwrap().is_none(), "engine exited early");
            if OwnerClient::discover(root, OwnerClientKind::Tui, Duration::from_millis(200))
                .await
                .is_ok()
            {
                return Self { child };
            }
            assert!(Instant::now() < deadline, "engine readiness deadline");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn stop(mut self) {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(self.child.id().try_into().unwrap()).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "engine shutdown deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

async fn owner(root: &Path) -> OwnerController {
    OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .unwrap()
}

#[tokio::test]
async fn real_engine_process_creates_and_keeps_a_project_folder_and_research_jobs() {
    let root = tempfile::Builder::new()
        .prefix("ca-setup-cli-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        root.path().join("global.toml"),
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n",
    )
    .unwrap();
    std::fs::set_permissions(
        root.path().join("global.toml"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("research.md"), "# Prior research\n").unwrap();

    let engine = Engine::start(root.path()).await;
    let mut controller = owner(root.path()).await;
    let project = ProjectId::new("project-cli-setup").unwrap();
    let response = controller
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Recipe box".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Saves family recipes.".into(),
                tech_stack: vec!["nextjs".into(), "sqlite".into()],
                platforms: vec![Platform::Website, Platform::DesktopApp],
                research: vec![
                    ResearchKind::MarketResearch,
                    ResearchKind::DesignResearch,
                    ResearchKind::FrontEndDesign,
                ],
                pool: None,
                team: None,
                documents: vec![sources.path().join("research.md").to_str().unwrap().into()],
            },
            1,
        )
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = response else {
        panic!("setup must commit");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    drop(controller);
    engine.stop();

    let folder = std::fs::canonicalize(root.path())
        .unwrap()
        .join("projects/project-cli-setup");
    assert!(folder.join("context/brief.md").is_file());
    assert_eq!(
        std::fs::read_to_string(folder.join("context/documents/research.md")).unwrap(),
        "# Prior research\n"
    );

    let engine = Engine::start(root.path()).await;
    let mut controller = owner(root.path()).await;
    let jobs = controller.research_jobs(project.clone()).await.unwrap();
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind, job.status, job.status_label.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                ResearchKind::MarketResearch,
                ResearchStatus::NeedsPool,
                "Needs a pool"
            ),
            (
                ResearchKind::DesignResearch,
                ResearchStatus::NeedsPool,
                "Needs a pool"
            ),
            (
                ResearchKind::FrontEndDesign,
                ResearchStatus::NeedsPool,
                "Needs a pool"
            ),
        ]
    );
    let stored = controller
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.team.sub_harnesses[0].agents, 3);
    let context = controller.project_context(project).await.unwrap();
    assert_eq!(
        context.items.iter().map(|i| i.kind).collect::<Vec<_>>(),
        [ContextKind::Brief, ContextKind::Document]
    );
    drop(controller);
    engine.stop();
}
