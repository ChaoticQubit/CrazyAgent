use super::*;
use crate::memory::library::{LibraryQuery, LibraryView};
use crate::project_setup::SetupAction;
pub(in crate::tui) use crate::project_setup::{SetupQuery, SetupView};

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
            SetupQuery::ReadPaths { project } => SetupView::ReadPaths(
                controller
                    .project_read_paths(project)
                    .await
                    .map_err(problem)?,
            ),
            SetupQuery::HiddenPaths { project } => SetupView::HiddenPaths(
                controller
                    .project_hidden_paths(project)
                    .await
                    .map_err(problem)?,
            ),
            SetupQuery::AgentPolicy { project } => SetupView::AgentPolicy(
                controller
                    .project_agent_policy(project)
                    .await
                    .map_err(problem)?,
            ),
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
            SetupQuery::Log { project, job } => SetupView::Log(
                controller
                    .research_log(project, job)
                    .await
                    .map_err(problem)?,
            ),
            SetupQuery::Memories { project, after } => SetupView::Memories(
                match after {
                    None => controller.all_project_memories(project).await,
                    Some(after) => controller.project_memories(project, Some(after)).await,
                }
                .map_err(problem)?,
            ),
            SetupQuery::ReviewMemory {
                project,
                entry_id,
                version,
                decision,
                generation,
            } => changed(
                controller
                    .review_memory(project, entry_id, version, decision, generation)
                    .await
                    .map_err(problem)?,
            )?,
            SetupQuery::Team { project } => SetupView::Team(Box::new(
                controller.team_status(project).await.map_err(problem)?,
            )),
            SetupQuery::CoordQuestions { project } => SetupView::CoordQuestions(Box::new(
                controller.coord_questions(project).await.map_err(problem)?,
            )),
            SetupQuery::TeamChange { action, generation } => {
                if let crate::team::TeamAction::LiveMaster {
                    project_id,
                    on: true,
                } = &action
                {
                    open_live_session(controller, project_id, generation).await?;
                }
                changed(
                    controller
                        .team_action(action, generation)
                        .await
                        .map_err(problem)?,
                )?
            }
            SetupQuery::Library(query) => SetupView::Library(match query {
                LibraryQuery::Page {
                    scope,
                    search,
                    before,
                } => LibraryView::Page(
                    controller
                        .memories(scope, search, before)
                        .await
                        .map_err(problem)?,
                ),
                LibraryQuery::View { memory_id } => {
                    LibraryView::View(controller.memory(memory_id).await.map_err(problem)?)
                }
                LibraryQuery::Approved { project } => LibraryView::Approved(
                    controller
                        .approved_memories(project)
                        .await
                        .map_err(problem)?,
                ),
                LibraryQuery::Change { action, generation } => LibraryView::Changed(owner_result(
                    controller
                        .change_memory(action, generation)
                        .await
                        .map_err(problem)?,
                )?),
            }),
            SetupQuery::Change { action, generation } => {
                let response = match action {
                    SetupAction::Create { .. } => {
                        return Err("project_setup_create_uses_creation".into());
                    }
                    SetupAction::SaveToMemory {
                        project_id,
                        research,
                        section,
                    } => {
                        controller
                            .save_research_to_memory(project_id, research, section, generation)
                            .await
                    }
                    SetupAction::AddDocuments {
                        project_id,
                        documents,
                    } => {
                        controller
                            .add_project_documents(project_id, documents, generation)
                            .await
                    }
                    SetupAction::DeleteContext { project_id, path } => {
                        controller
                            .delete_project_context(project_id, path, generation)
                            .await
                    }
                    SetupAction::Update {
                        project_id,
                        expected_revision,
                        name,
                        setup,
                    } => {
                        controller
                            .update_project_setup(
                                project_id,
                                expected_revision,
                                name,
                                setup,
                                generation,
                            )
                            .await
                    }
                    SetupAction::Remove { project_id } => {
                        controller.remove_project(project_id, generation).await
                    }
                    SetupAction::CancelResearch { project_id, job } => {
                        controller
                            .cancel_research(project_id, job, generation)
                            .await
                    }
                    SetupAction::RetryResearch { project_id, job } => {
                        controller.retry_research(project_id, job, generation).await
                    }
                    SetupAction::SetReadPaths { project_id, paths } => {
                        controller
                            .set_project_read_paths(project_id, paths, generation)
                            .await
                    }
                    SetupAction::SetHiddenPaths { project_id, paths } => {
                        controller
                            .set_project_hidden_paths(project_id, paths, generation)
                            .await
                    }
                    SetupAction::SetAgentPolicy { project_id, policy } => {
                        controller
                            .set_project_agent_policy(project_id, policy, generation)
                            .await
                    }
                }
                .map_err(problem)?;
                changed(response)?
            }
        })
    }
}

fn changed(response: OwnerResponse) -> Result<SetupView, String> {
    owner_result(response).map(SetupView::Changed)
}

/// Sends the owner's default master Open before a project's live master switch turns on,
/// when the project has no live session, so the live master runs in a session its turns
/// mark used and the Sessions list shows (LMS-1). A project whose defaults cannot open a
/// master, or whose Open the engine refuses, still turns its switch on as before.
/// Inputs: the controller, the project and the change's generation. Output: Ok, or the
/// problem when the master read or the Open's delivery failed.
async fn open_live_session(
    controller: &mut OwnerController,
    project: &ProjectId,
    generation: u64,
) -> Result<(), String> {
    let problem = |error: crate::engine::EngineError| bounded_problem(&error.0);
    let state = controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
        .map_err(problem)?;
    let OwnerData::ProjectMaster { session } = &state.data else {
        return Err("master_fresh_read_required".into());
    };
    if session.as_ref().is_some_and(|session| {
        matches!(
            session.phase,
            crate::master_session::MasterSessionPhase::Configured
                | crate::master_session::MasterSessionPhase::Closing
        )
    }) {
        return Ok(());
    }
    let Ok(action) = crate::tui::default_master_open(controller, project).await else {
        return Ok(());
    };
    controller
        .read(OwnerRead::Capabilities)
        .await
        .map_err(problem)?;
    controller
        .begin_mutation(OwnerMutation::ProjectMaster { action }, generation)
        .map_err(problem)?;
    owner_result(controller.send_pending().await.map_err(problem)?).map(|_| ())
}

fn owner_result(response: OwnerResponse) -> Result<crate::engine::owner::OwnerResult, String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            Ok(operation.result)
        }
        OwnerResponse::Rejected { code } => Err(bounded_problem(&code)),
        _ => Err("setup_change_outcome_unknown".into()),
    }
}

#[cfg(all(test, unix))]
mod memory_tests;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::engine::bootstrap::EngineService;
    use crate::project_setup::{Platform, ProjectSetup, ResearchKind, ResearchStatus};
    use crate::tui::app::App;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn tui_creates_a_project_with_setup_and_reads_its_jobs_and_context() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-setup-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sources = tempfile::tempdir().unwrap();
        std::fs::write(sources.path().join("scope.csv"), "item,size\nlogin,small\n").unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let config = crate::tui::test_home::engine_config();
        let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
            .await
            .unwrap();
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let (_, access) = native.attach().await;
        let mut app = App::new();
        app.apply_owner(access).unwrap();
        assert!(app.owner_ready);
        let _home =
            crate::tui::test_home::projects_home(native.controller.as_mut().unwrap(), 1).await;

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
            "creation must commit: {:?}",
            event.result
        );
        assert!(!event.retained);
        app.apply_owner(OwnerEvent::Creation(Box::new(event)))
            .unwrap();
        assert!(
            app.has_unresolved_creation(),
            "creation waits for snapshot readback"
        );
        let snapshot = Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
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
        let Some((SetupQuery::Jobs { project: shown }, SetupView::Jobs(jobs))) = &app.setup_view
        else {
            panic!("jobs view required");
        };
        assert_eq!(shown, &project);
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

        std::fs::write(sources.path().join("notes.md"), "# Notes\n").unwrap();
        for action in [
            SetupAction::AddDocuments {
                project_id: project.clone(),
                documents: vec![sources.path().join("notes.md").to_str().unwrap().into()],
            },
            SetupAction::DeleteContext {
                project_id: project.clone(),
                path: "context/documents/scope.csv".into(),
            },
        ] {
            app.request_setup_change(action.clone()).unwrap();
            let query = app.take_setup_query().unwrap();
            let event = native.project_setup(query).await;
            app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
            let Some((SetupQuery::Change { action: shown, .. }, SetupView::Changed(result))) =
                &app.setup_view
            else {
                panic!("change view required");
            };
            assert_eq!(shown, &action);
            assert_eq!(result, &crate::engine::owner::OwnerResult::Applied);
        }
        let context = native
            .project_setup(SetupQuery::Context {
                project: project.clone(),
            })
            .await;
        let Ok(SetupView::Context(context)) = context.result else {
            panic!("context required");
        };
        assert_eq!(
            context
                .items
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>(),
            ["context/brief.md", "context/documents/notes.md"]
        );
        std::fs::write(
            std::path::Path::new(&context.folder).join("context/research/competitor_research.md"),
            "# Competitors\n\nTwo terminal note apps.\n",
        )
        .unwrap();
        app.request_setup_change(SetupAction::SaveToMemory {
            project_id: project.clone(),
            research: ResearchKind::CompetitorResearch,
            section: None,
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        app.request_project_setup(SetupQuery::Memories {
            project: project.clone(),
            after: None,
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        let Some((_, SetupView::Memories(memories))) = &app.setup_view else {
            panic!("memories view required");
        };
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].status,
            crate::memory::KnowledgeStatus::Provisional
        );
        let (entry, version) = (memories[0].id.clone(), memories[0].version);
        app.request_memory_review(
            project.clone(),
            entry,
            version,
            crate::memory::ReviewDecision::Keep,
        )
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        assert!(matches!(
            &app.setup_view,
            Some((
                SetupQuery::ReviewMemory { .. },
                SetupView::Changed(crate::engine::owner::OwnerResult::Applied)
            ))
        ));
        let Ok(SetupView::Memories(memories)) = native
            .project_setup(SetupQuery::Memories {
                project: project.clone(),
                after: None,
            })
            .await
            .result
        else {
            panic!("memories required");
        };
        assert_eq!(
            memories
                .iter()
                .map(|m| (&m.status, m.human_reviewed))
                .collect::<Vec<_>>(),
            [(&crate::memory::KnowledgeStatus::Confirmed, true)]
        );
        app.request_setup_change(SetupAction::DeleteContext {
            project_id: project.clone(),
            path: "context/brief.md".into(),
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        assert!(matches!(
            &app.setup_view,
            Some((_, SetupView::Changed(crate::engine::owner::OwnerResult::Rejected { code })))
                if code == "context_item_protected"
        ));
        assert!(
            app.request_setup_change(SetupAction::DeleteContext {
                project_id: project.clone(),
                path: "../engine.db".into(),
            })
            .is_err()
        );

        app.request_team_change(crate::team::TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the notes app.".into(),
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        let Some((SetupQuery::TeamChange { .. }, SetupView::Changed(result))) = &app.setup_view
        else {
            panic!("team change view required: {:?}", app.setup_problem);
        };
        assert_eq!(result, &crate::engine::owner::OwnerResult::Applied);
        app.request_project_setup(SetupQuery::Team {
            project: project.clone(),
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        let Some((SetupQuery::Team { .. }, SetupView::Team(team))) = &app.setup_view else {
            panic!("team view required");
        };
        assert_eq!(team.project_id, project);
        assert_eq!(team.harnesses[0].role, crate::team::AgentRole::Master);
        assert_eq!(team.harnesses[0].harness, "codex");
        assert_eq!(
            team.messages
                .iter()
                .map(|m| (m.author, m.state, m.text.as_str()))
                .collect::<Vec<_>>(),
            [(
                crate::team::MessageAuthor::Owner,
                crate::team::MessageState::Pending,
                "Plan the notes app."
            )]
        );
        assert!(!team.paused);
        assert!(
            app.request_team_change(crate::team::TeamAction::Message {
                project_id: project.clone(),
                text: " ".into(),
            })
            .is_err()
        );
        app.request_team_change(crate::team::TeamAction::Pause {
            project_id: project.clone(),
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        assert!(matches!(
            &app.setup_view,
            Some((
                SetupQuery::TeamChange { .. },
                SetupView::Changed(crate::engine::owner::OwnerResult::Applied)
            ))
        ));
        let read = native
            .project_setup(SetupQuery::Team {
                project: project.clone(),
            })
            .await;
        let Ok(SetupView::Team(team)) = read.result else {
            panic!("team view required");
        };
        assert!(team.paused, "the TUI pauses the whole team");
        assert!(team.plan.is_empty(), "no master reply proposed steps yet");
        app.request_team_change(crate::team::TeamAction::HandOff {
            project_id: project.clone(),
            reply: team.messages[0].id,
            step: 0,
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        assert!(
            matches!(
                &app.setup_view,
                Some((_, SetupView::Changed(crate::engine::owner::OwnerResult::Rejected { code })))
                    if code == "team_plan_step_missing"
            ),
            "an owner message holds no plan steps"
        );

        let catalog = native
            .project_setup(SetupQuery::Catalog {
                pool: None,
                research: vec![ResearchKind::MarketResearch],
            })
            .await;
        let Ok(SetupView::Catalog(catalog)) = catalog.result else {
            panic!("catalog required");
        };
        assert_eq!(catalog.default_team.master, "codex");
        assert_eq!(catalog.research.len(), ResearchKind::ALL.len());
        let missing = native
            .project_setup(SetupQuery::Context {
                project: ProjectId::new("project-absent").unwrap(),
            })
            .await;
        app.apply_owner(OwnerEvent::Setup(Box::new(missing)))
            .unwrap();
        assert_eq!(app.setup_problem.as_deref(), Some("project_setup_missing"));

        let Ok(SetupView::Jobs(jobs)) = native
            .project_setup(SetupQuery::Jobs {
                project: project.clone(),
            })
            .await
            .result
        else {
            panic!("jobs required");
        };
        let first = jobs[0].id;
        let Ok(SetupView::Log(lines)) = native
            .project_setup(SetupQuery::Log {
                project: project.clone(),
                job: first,
            })
            .await
            .result
        else {
            panic!("log required");
        };
        assert!(lines.is_empty(), "no agent ran yet");
        let Ok(SetupView::Setup(Some(stored))) = native
            .project_setup(SetupQuery::Setup {
                project: project.clone(),
            })
            .await
            .result
        else {
            panic!("setup required");
        };
        let edited = ProjectSetup {
            about: "Terminal-first notes for teams.".into(),
            tech_stack: stored.tech_stack.clone(),
            platforms: stored.platforms.clone(),
            research: stored.research.clone(),
            pool: None,
            team: None,
            documents: vec![],
        };
        for action in [
            SetupAction::CancelResearch {
                project_id: project.clone(),
                job: first,
            },
            SetupAction::RetryResearch {
                project_id: project.clone(),
                job: first,
            },
            SetupAction::Update {
                project_id: project.clone(),
                expected_revision: stored.revision,
                name: Some("Team notes".into()),
                setup: edited,
            },
            SetupAction::Remove {
                project_id: project.clone(),
            },
        ] {
            app.request_setup_change(action.clone()).unwrap();
            let query = app.take_setup_query().unwrap();
            let event = native.project_setup(query).await;
            app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
            assert!(
                matches!(
                    &app.setup_view,
                    Some((
                        SetupQuery::Change { .. },
                        SetupView::Changed(crate::engine::owner::OwnerResult::Applied)
                    ))
                ),
                "{action:?}"
            );
        }
        let Ok(SetupView::Setup(removed)) = native
            .project_setup(SetupQuery::Setup {
                project: project.clone(),
            })
            .await
            .result
        else {
            panic!("setup read required");
        };
        assert!(removed.is_none(), "a removed project has no setup");
        drop(native);
        service.shutdown().await.unwrap();
    }

    /// One API key account and its connection, for a project whose pool lets the owner's
    /// default Open pick a master.
    const LIVE_ACCESS: &str = r#"version = 1
timeout_ms = 10000
allowed_destinations = []

[model_access]
accounts = ["api-1"]
connections = [
  { id = "api-a", provider = "openrouter", harness = "rcli", protocol = "openai-chat", account_ref = "api-1", endpoint = { kind = "http", url = "https://openrouter.ai/api/v1" }, model = { id = "some/model", author = "openrouter" }, auth = { method = "api_key", credential_ref = "api-key-ref" } },
]
pools = []
"#;

    /// Reads a project's master session as the engine holds it. Inputs: the controller and
    /// the project. Output: the session id and phase, or None before any session.
    async fn master_session(
        controller: &mut OwnerController,
        project: &ProjectId,
    ) -> Option<(
        crate::protocol::SessionId,
        crate::master_session::MasterSessionPhase,
    )> {
        let state = controller
            .read(OwnerRead::ProjectMaster {
                project_id: project.clone(),
            })
            .await
            .unwrap();
        let OwnerData::ProjectMaster { session } = state.data else {
            panic!("project master read required");
        };
        session.map(|session| (session.session_id, session.phase))
    }

    /// Turning a project's live master on from the TUI first opens the owner's default
    /// master session when the project has none, so the live master runs in a session its
    /// turns mark used and the Sessions list shows (LMS-1); turning it on again keeps that
    /// session and opens no second one.
    #[tokio::test]
    async fn turning_the_live_master_on_opens_the_owner_s_default_session_first() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-live-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, LIVE_ACCESS).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let (_, access) = native.attach().await;
        let mut app = App::new();
        app.apply_owner(access).unwrap();
        assert!(app.owner_ready);
        let _home =
            crate::tui::test_home::projects_home(native.controller.as_mut().unwrap(), 1).await;
        let setup = ProjectSetup {
            about: "Notes kept by a live master.".into(),
            tech_stack: vec!["rust".into()],
            platforms: vec![Platform::Cli],
            research: vec![],
            pool: None,
            team: None,
            documents: vec![],
        };
        let spec = ProjectSpec {
            name: "Live notes".into(),
            repositories: vec![],
        };
        app.queue_project_setup(spec, setup).unwrap();
        let job = app.take_creation_job().unwrap();
        let project = job.submission.project_id.clone();
        let event = native.creation(job).await;
        assert!(
            matches!(event.result.as_deref(), Ok(OwnerResponse::Applied { .. })),
            "creation must commit: {:?}",
            event.result
        );
        app.apply_owner(OwnerEvent::Creation(Box::new(event)))
            .unwrap();
        let snapshot = Client::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap()
        .request(crate::protocol::WireRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            client_id: crate::protocol::ClientId::new("tui-live-test").unwrap(),
            request_id: RequestId::new("tui-live-snapshot").unwrap(),
            expected_revision: Cursor::new(0),
            command: crate::protocol::Command::Snapshot,
        })
        .await
        .unwrap();
        app.apply_reply(snapshot).unwrap();
        assert!(!app.has_unresolved_creation());

        let controller = native.controller.as_mut().unwrap();
        let pools = controller
            .read(OwnerRead::ModelAccessPage {
                collection: crate::engine::owner::ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessPage { source_hash, .. } = &pools.data else {
            panic!("pool page required");
        };
        let reference = |value: &str| crate::protocol::CredentialRef::new(value).unwrap();
        controller
            .begin_mutation(
                OwnerMutation::ModelAccess {
                    expected_config_id: pools.config_id.clone(),
                    expected_source_hash: source_hash.clone(),
                    edit: crate::config::ModelAccessEdit::CreatePool {
                        pool: crate::config::AccountPool {
                            id: reference("pool-live"),
                            name: None,
                            members: vec![reference("api-a")],
                            projects: vec![reference(project.as_str())],
                            roles: vec![reference("project_leader")],
                            fallback_allowed: false,
                            provider_quota_policy: crate::config::ProviderQuotaPolicy::default(),
                        },
                    },
                },
                1,
            )
            .unwrap();
        let assigned = controller.send_pending().await.unwrap();
        assert!(
            matches!(
                &assigned,
                OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
                    if operation.result == crate::engine::owner::OwnerResult::Applied
            ),
            "the pool is created and assigned: {assigned:?}"
        );
        assert_eq!(
            master_session(controller, &project).await,
            None,
            "a new project has no master session"
        );

        let mut opened = Vec::new();
        for _ in 0..2 {
            app.request_team_change(crate::team::TeamAction::LiveMaster {
                project_id: project.clone(),
                on: true,
            })
            .unwrap();
            let query = app.take_setup_query().unwrap();
            let event = native.project_setup(query).await;
            app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
            assert!(
                matches!(
                    &app.setup_view,
                    Some((
                        SetupQuery::TeamChange { .. },
                        SetupView::Changed(crate::engine::owner::OwnerResult::Applied)
                    ))
                ),
                "the live switch turns on: {:?} {:?}",
                app.setup_view,
                app.setup_problem
            );
            opened.push(master_session(native.controller.as_mut().unwrap(), &project).await);
        }
        let Some((session, phase)) = opened[0].clone() else {
            panic!("turning the live master on opened no master session: {opened:?}");
        };
        assert_eq!(
            phase,
            crate::master_session::MasterSessionPhase::Configured,
            "the opened session is the live one"
        );
        assert_eq!(
            opened[1],
            Some((
                session,
                crate::master_session::MasterSessionPhase::Configured
            )),
            "turning it on again keeps the live session"
        );
        drop(native);
        service.shutdown().await.unwrap();
    }
}
