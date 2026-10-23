use super::*;
use crate::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ResearchJob, ResearchKind, StoredSetup,
};
use crate::protocol::CredentialRef;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SetupQuery {
    Catalog {
        pool: Option<CredentialRef>,
        research: Vec<ResearchKind>,
    },
    Setup {
        project: ProjectId,
    },
    Jobs {
        project: ProjectId,
    },
    Context {
        project: ProjectId,
    },
    Item {
        project: ProjectId,
        path: String,
        offset: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum SetupView {
    Catalog(Box<ProjectCatalog>),
    Setup(Option<Box<StoredSetup>>),
    Jobs(Vec<ResearchJob>),
    Context(ProjectContext),
    Item(ContextChunk),
}

pub(in crate::tui) struct SetupEvent {
    pub query: SetupQuery,
    pub result: Result<SetupView, String>,
}

impl Connection {
    /// Queues one New Project read on the paired owner worker.
    /// Takes the catalog, setup, jobs, context or context item query.
    /// Returns an error while another native request is pending.
    pub(in crate::tui) fn project_setup(&mut self, query: SetupQuery) -> Result<(), String> {
        self.owner_job(Job::Setup(Box::new(query)))
    }
}

impl NativeOwner {
    /// Reads New Project state through the trusted owner controller.
    /// Takes the query.
    /// Returns the event with the view or a bounded plain problem.
    pub(super) async fn project_setup(&mut self, query: SetupQuery) -> SetupEvent {
        let result = self.read_setup(query.clone()).await;
        SetupEvent { query, result }
    }

    async fn read_setup(&mut self, query: SetupQuery) -> Result<SetupView, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        let problem = |error: crate::engine::EngineError| bounded_problem(&error.0);
        Ok(match query {
            SetupQuery::Catalog { pool, research } => SetupView::Catalog(Box::new(
                controller
                    .project_catalog(pool, research)
                    .await
                    .map_err(problem)?,
            )),
            SetupQuery::Setup { project } => SetupView::Setup(
                controller
                    .project_setup(project)
                    .await
                    .map_err(problem)?
                    .map(Box::new),
            ),
            SetupQuery::Jobs { project } => {
                SetupView::Jobs(controller.research_jobs(project).await.map_err(problem)?)
            }
            SetupQuery::Context { project } => {
                SetupView::Context(controller.project_context(project).await.map_err(problem)?)
            }
            SetupQuery::Item {
                project,
                path,
                offset,
            } => SetupView::Item(
                controller
                    .project_context_item(project, path, offset)
                    .await
                    .map_err(problem)?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::bootstrap::EngineService;
    use crate::project_setup::{Platform, ProjectSetup, ResearchStatus};
    use crate::tui::app::App;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn tui_creates_a_project_with_setup_and_reads_its_jobs_and_context() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-setup-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sources = tempfile::tempdir().unwrap();
        std::fs::write(sources.path().join("scope.csv"), "item,size\nlogin,small\n").unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let mut native = NativeOwner::new(root.path().join("e.sock"), Duration::from_secs(2));
        let (_, access) = native.attach().await;
        let mut app = App::new();
        app.apply_owner(access).unwrap();
        assert!(app.owner_ready);

        let setup = ProjectSetup {
            about: "Terminal-first notes.".into(),
            tech_stack: vec!["rust".into()],
            platforms: vec![Platform::Cli],
            research: vec![
                ResearchKind::CompetitorResearch,
                ResearchKind::TechStackResearch,
            ],
            pool: None,
            team: None,
            documents: vec![sources.path().join("scope.csv").to_str().unwrap().into()],
        };
        let spec = ProjectSpec {
            name: "Notes CLI".into(),
            repositories: vec![],
        };
        app.queue_project_setup(spec, setup).unwrap();
        let job = app.take_creation_job().unwrap();
        let project = job.submission.project_id.clone();
        let event = native.creation(job).await;
        assert!(
            matches!(event.result.as_deref(), Ok(OwnerResponse::Applied { .. })),
            "creation must commit"
        );
        assert!(!event.retained);
        app.apply_owner(OwnerEvent::Creation(Box::new(event)))
            .unwrap();
        assert!(
            app.has_unresolved_creation(),
            "creation waits for snapshot readback"
        );
        let snapshot = Client::new(root.path().join("e.sock"), Duration::from_secs(2))
            .unwrap()
            .request(crate::protocol::WireRequest {
                protocol_version: crate::protocol::PROTOCOL_VERSION,
                client_id: crate::protocol::ClientId::new("tui-setup-test").unwrap(),
                request_id: RequestId::new("tui-setup-snapshot").unwrap(),
                expected_revision: Cursor::new(0),
                command: crate::protocol::Command::Snapshot,
            })
            .await
            .unwrap();
        app.apply_reply(snapshot).unwrap();
        assert!(!app.has_unresolved_creation());
        assert!(
            app.projects
                .iter()
                .any(|stored| stored.project_id == project && stored.spec.name == "Notes CLI")
        );

        app.request_project_setup(SetupQuery::Jobs {
            project: project.clone(),
        })
        .unwrap();
        assert!(
            app.request_project_setup(SetupQuery::Setup {
                project: project.clone()
            })
            .is_err()
        );
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        let Some(SetupView::Jobs(jobs)) = &app.setup_view else {
            panic!("jobs view required");
        };
        assert_eq!(
            jobs.iter()
                .map(|job| (job.kind, job.status))
                .collect::<Vec<_>>(),
            [
                (ResearchKind::CompetitorResearch, ResearchStatus::NeedsPool),
                (ResearchKind::TechStackResearch, ResearchStatus::NeedsPool)
            ]
        );

        let event = native
            .project_setup(SetupQuery::Item {
                project: project.clone(),
                path: "context/documents/scope.csv".into(),
                offset: 0,
            })
            .await;
        let Ok(SetupView::Item(chunk)) = event.result else {
            panic!("context item required");
        };
        assert_eq!(chunk.text.as_deref(), Some("item,size\nlogin,small\n"));
        let missing = native
            .project_setup(SetupQuery::Context {
                project: ProjectId::new("project-absent").unwrap(),
            })
            .await;
        app.apply_owner(OwnerEvent::Setup(Box::new(missing)))
            .unwrap();
        assert_eq!(app.setup_problem.as_deref(), Some("project_setup_missing"));
        drop(native);
        service.shutdown().await.unwrap();
    }
}
