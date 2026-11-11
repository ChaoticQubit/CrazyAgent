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
            SetupQuery::Removal { project } => SetupView::Removal(Box::new(
                controller.removal_preview(project).await.map_err(problem)?,
            )),
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
                let unopened = match &action {
                    crate::team::TeamAction::LiveMaster {
                        project_id,
                        on: true,
                        ..
                    } => open_live_session(controller, project_id, generation).await,
                    _ => None,
                };
                let view = changed(
                    controller
                        .team_action(action, generation)
                        .await
                        .map_err(problem)?,
                )?;
                match (view, unopened) {
                    (SetupView::Changed(crate::engine::owner::OwnerResult::Applied), Some(why)) => {
                        SetupView::LiveWithoutSession(why)
                    }
                    (view, _) => view,
                }
            }
            SetupQuery::OpenLiveSession {
                project,
                generation,
            } => match open_live_session(controller, &project, generation).await {
                Some(why) => SetupView::LiveWithoutSession(why),
                None => SetupView::Changed(crate::engine::owner::OwnerResult::Applied),
            },
            SetupQuery::TeamMessages {
                project,
                agent,
                before,
            } => SetupView::TeamMessages(Box::new(
                controller
                    .team_agent_messages(project, agent, before, None)
                    .await
                    .map_err(problem)?,
            )),
            SetupQuery::MoveHome {
                project,
                mode,
                generation,
            } => SetupView::Changed(owner_result(
                controller
                    .move_project_home(project, mode, Vec::new(), generation)
                    .await
                    .map_err(problem)?,
            )?),
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
                    SetupAction::Delete { project_id, name } => {
                        controller
                            .delete_project(project_id, name, generation)
                            .await
                    }
                    SetupAction::CancelResearch { project_id, job } => {
                        controller
                            .cancel_research(project_id, job, generation)
                            .await
                    }
                    SetupAction::RetryResearch { project_id, job } => {
                        controller.retry_research(project_id, job, generation).await
                    }
                    SetupAction::PlanRoadmap { project_id } => {
                        controller.plan_roadmap(project_id, generation).await
                    }
                    SetupAction::SetReadPaths {
                        project_id,
                        version,
                        paths,
                    } => {
                        controller
                            .set_project_read_paths(project_id, version, paths, generation)
                            .await
                    }
                    SetupAction::SetHiddenPaths { project_id, paths } => {
                        controller
                            .set_project_hidden_paths(project_id, paths, generation)
                            .await
                    }
                    SetupAction::SetAgentPolicy {
                        project_id,
                        version,
                        policy,
                    } => {
                        controller
                            .set_project_agent_policy(project_id, version, policy, generation)
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

/// Sends the owner's default master Open for a project's live master when no live session
/// holds it, before its switch turns on or for a switch already on, so the live master runs
/// in a session its turns mark used and the Sessions list shows (LMS-1). Nothing here stops
/// the switch: a project whose defaults cannot open a master, an Open the engine refuses
/// and a read or delivery failure each come back as the reason (LMS-M1, LMS-L1).
/// Inputs: the controller, the project and the generation. Output: None when a live
/// session holds the master or the Open applied, else the engine's code or plain reason.
async fn open_live_session(
    controller: &mut OwnerController,
    project: &ProjectId,
    generation: u64,
) -> Option<String> {
    let problem = |error: crate::engine::EngineError| bounded_problem(&error.0);
    let state = match controller
        .read(OwnerRead::ProjectMaster {
            project_id: project.clone(),
        })
        .await
    {
        Ok(state) => state,
        Err(error) => return Some(problem(error)),
    };
    let OwnerData::ProjectMaster { session } = &state.data else {
        return Some("master_fresh_read_required".into());
    };
    if session.as_ref().is_some_and(|session| {
        matches!(
            session.phase,
            crate::master_session::MasterSessionPhase::Configured
                | crate::master_session::MasterSessionPhase::Closing
        )
    }) {
        return None;
    }
    let action = match crate::tui::default_master_open(controller, project).await {
        Ok(action) => action,
        Err(why) => return Some(why),
    };
    if let Err(error) = controller.read(OwnerRead::Capabilities).await {
        return Some(problem(error));
    }
    if let Err(error) =
        controller.begin_mutation(OwnerMutation::ProjectMaster { action }, generation)
    {
        return Some(problem(error));
    }
    match controller
        .send_pending()
        .await
        .map_err(problem)
        .and_then(owner_result)
    {
        Ok(crate::engine::owner::OwnerResult::Rejected { code }) => Some(bounded_problem(&code)),
        Ok(_) => None,
        Err(why) => Some(why),
    }
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
mod messages_tests;

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

    /// A started engine with one new project made through the TUI's creation path, for the
    /// live master tests. Input: the temporary root. Output: the service, the owner worker,
    /// the app, the project, the stop sender the service watches and the projects home.
    async fn live_project(
        root: &std::path::Path,
    ) -> (
        EngineService,
        NativeOwner,
        App,
        ProjectId,
        tokio::sync::watch::Sender<bool>,
        tempfile::TempDir,
    ) {
        live_project_with(root, LIVE_ACCESS, None, vec![]).await
    }

    /// [`live_project`] on the given global settings with the given team plan, None for
    /// the engine's default, and research toggles. Inputs: the root, the settings text, the
    /// plan and the toggles. Output: as [`live_project`].
    async fn live_project_with(
        root: &std::path::Path,
        access: &str,
        team: Option<crate::project_setup::TeamPlan>,
        research: Vec<ResearchKind>,
    ) -> (
        EngineService,
        NativeOwner,
        App,
        ProjectId,
        tokio::sync::watch::Sender<bool>,
        tempfile::TempDir,
    ) {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.join("global.toml");
        std::fs::write(&global, access).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root,
            "e.sock",
            &global,
            &root.join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let mut native = NativeOwner::new(root.join("e.sock"), crate::tui::test_home::ENGINE_WAIT);
        let (_, access) = native.attach().await;
        let mut app = App::new();
        app.apply_owner(access).unwrap();
        assert!(app.owner_ready);
        let home =
            crate::tui::test_home::projects_home(native.controller.as_mut().unwrap(), 1).await;
        let setup = ProjectSetup {
            about: "Notes kept by a live master.".into(),
            tech_stack: vec!["rust".into()],
            platforms: vec![Platform::Cli],
            research,
            pool: None,
            team,
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
        let snapshot = Client::new(root.join("e.sock"), crate::tui::test_home::ENGINE_WAIT)
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
        (service, native, app, project, stop, home)
    }

    /// Creates the project's master pool from the one API key connection, assigned to the
    /// project for its project leader, as the owner does on the pools page. Inputs: the
    /// controller and the project. Output: none; it panics unless the engine applied it.
    async fn assign_live_pool(controller: &mut OwnerController, project: &ProjectId) {
        assign_pool(controller, project, "api-a").await;
    }

    /// Creates the project's master pool from one connection, assigned to the project for
    /// its project leader, as the owner does on the pools page. Inputs: the controller, the
    /// project and the connection. Output: none; it panics unless the engine applied it.
    async fn assign_pool(controller: &mut OwnerController, project: &ProjectId, member: &str) {
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
                            members: vec![reference(member)],
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
    }

    /// The global settings of a test engine whose model access names only the owner's
    /// signed-in account, with no pool yet. Input: the account's connection as the owner's
    /// settings name it. Output: the settings text.
    #[cfg(target_os = "macos")]
    fn owner_settings(connection: &crate::config::ModelConnection) -> String {
        let mut settings = "version = 1\ntimeout_ms = 10000\nallowed_destinations = []\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        settings["model_access"] = toml_edit::Item::Table(
            toml_edit::ser::to_document(&crate::config::ModelAccess {
                accounts: vec![connection.account_ref.clone()],
                connections: vec![connection.clone()],
                pools: vec![],
            })
            .unwrap()
            .into_table(),
        );
        settings.to_string()
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
        let (mut service, mut native, mut app, project, _stop, _home) =
            live_project(root.path()).await;

        let controller = native.controller.as_mut().unwrap();
        assign_live_pool(controller, &project).await;
        assert_eq!(
            master_session(controller, &project).await,
            None,
            "a new project has no master session"
        );

        let mut opened = Vec::new();
        for _ in 0..2 {
            let version = native
                .controller
                .as_mut()
                .unwrap()
                .team_status(project.clone())
                .await
                .unwrap()
                .settings_version;
            app.request_team_change(crate::team::TeamAction::LiveMaster {
                project_id: project.clone(),
                on: true,
                version,
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

    /// LMS-1, live: turning a new project's live master on from the TUI opens the owner's
    /// default master session on the owner's signed-in Claude account before any master
    /// runs, since no team runtime runs yet; once that engine stops, the team runtime on its
    /// database starts the live master in tmux in that same session, and its first real
    /// turn is counted in it with the reply as its last line. Account ids are never printed.
    /// Runs in a live window only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
    fn turning_a_real_claude_live_master_on_opens_the_owner_s_default_session() {
        use crate::team::runtime::live_failover_tests::{ASK, OwnerAccount, START, TURN, Team};
        let owner = OwnerAccount::claude();
        let connection = owner.connection().clone();
        let settings = owner_settings(&connection);
        let plan = crate::project_setup::TeamPlan {
            master: "claude-code".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        };
        let root = tempfile::Builder::new()
            .prefix("ca-tui-live-lms-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let engine = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (project, session, _home) = engine.block_on(async {
            let (mut service, mut native, mut app, project, _stop, home) =
                live_project_with(root.path(), &settings, Some(plan), vec![]).await;
            let controller = native.controller.as_mut().unwrap();
            assign_pool(controller, &project, connection.id.as_str()).await;
            assert_eq!(
                master_session(controller, &project).await,
                None,
                "a new project has no master session"
            );
            let version = controller
                .team_status(project.clone())
                .await
                .unwrap()
                .settings_version;
            app.request_team_change(crate::team::TeamAction::LiveMaster {
                project_id: project.clone(),
                on: true,
                version,
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
            let state = native
                .controller
                .as_mut()
                .unwrap()
                .read(OwnerRead::ProjectMaster {
                    project_id: project.clone(),
                })
                .await
                .unwrap();
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!(
                    "turning the live master on opened no master session: {:?}",
                    app.setup_problem
                );
            };
            assert_eq!(
                session.phase,
                crate::master_session::MasterSessionPhase::Configured,
                "the opened session is the live one"
            );
            assert_eq!(
                session.selection.backend,
                crate::master_session::MasterBackend::Claude,
                "the owner's default master runs on the team plan's Claude Code"
            );
            assert!(
                session.selection.route.account_ref == connection.account_ref,
                "the default session is on the owner's signed-in Claude account"
            );
            assert_eq!(
                session.current_turn, None,
                "the session opened before the master's first turn"
            );
            drop(native);
            service.shutdown().await.unwrap();
            (project, session.session_id.as_str().to_owned(), home)
        });
        drop(engine);

        let mut team = Team::on_engine(
            root.path(),
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
            &project,
            &owner,
        );
        assert!(team.live(), "the TUI's switch is on: {}", team.seen());
        assert_eq!(
            team.engine_session(),
            Some(session.clone()),
            "the engine keeps the opened session for the live master"
        );
        team.until(
            "the live master starts on the owner's account",
            START,
            |t| {
                t.master()
                    .is_some_and(|(id, account)| account == "working" && t.live_agent() == Some(id))
            },
        );
        let (master, _) = team.master().unwrap();
        assert_eq!(
            team.live_session(master),
            Some(session.clone()),
            "the live master runs in the owner's default session: {}",
            team.seen()
        );
        team.say(ASK);
        team.until("the live master's first owner turn answers", TURN, |t| {
            t.turns().iter().any(|(_, outcome)| outcome == "success")
        });
        let counted = team.session_turns(&session);
        assert!(
            counted.as_ref().is_some_and(|(turns, line)| *turns >= 1
                && line.as_deref().is_some_and(|line| line.contains("READY"))),
            "the live master's real turn is counted in the opened session with its reply: {counted:?} {}",
            team.seen()
        );
        println!("live LMS-1: {counted:?} {}", team.seen());
    }

    /// How long one live DEC-090 task or research job may run on a real account.
    #[cfg(target_os = "macos")]
    const LIVE_RUN_LIMIT: std::time::Duration = std::time::Duration::from_secs(1200);

    /// The team plan of a live test project: a tools-on master and one tools-on sub-agent,
    /// both on the owner account's launch harness. Input: the harness. Output: the plan.
    #[cfg(target_os = "macos")]
    fn owner_team(harness: &str) -> crate::project_setup::TeamPlan {
        crate::project_setup::TeamPlan {
            master: harness.into(),
            master_tools: true,
            sub_harnesses: vec![crate::project_setup::HarnessSlot {
                harness: harness.into(),
                agents: 1,
                tools: true,
            }],
            min_agents: 1,
            max_agents: 2,
        }
    }

    /// The project's folder as the engine records it in the database it left, else its
    /// folder under `<root>/projects`, as the engine's research finds it. Inputs: the engine
    /// folder and the project. Output: the project's CrazyAgent state folder.
    #[cfg(target_os = "macos")]
    fn engine_state_folder(root: &std::path::Path, project: &ProjectId) -> std::path::PathBuf {
        let database = rusqlite::Connection::open_with_flags(
            root.join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tx = database.unchecked_transaction().unwrap();
        let folder = crate::project_setup::load_setup(&tx, project)
            .unwrap()
            .map(|stored| stored.project_folder())
            .unwrap_or_else(|| root.join("projects").join(project.as_str()));
        crate::projects_home::state_folder(&folder)
    }

    /// DEC-090, live: the owner hands one shell task to a tools-on sub-agent through the
    /// owner client of a test engine whose only account is the owner's signed-in managed one;
    /// once that engine stops, the team runtime on its database runs the task on that account
    /// under the owner engine's custody, and the task reports the command's output. Input:
    /// the owner's account. Output: none; it panics unless the task is done with the output.
    #[cfg(target_os = "macos")]
    fn managed_agent_completes_a_tool_turn(
        owner: crate::team::runtime::live_failover_tests::OwnerAccount,
    ) {
        use crate::team::TaskStatus;
        use crate::team::runtime::live_failover_tests::Team;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};
        let connection = owner.connection().clone();
        let (harness, _) = owner.launch();
        let nonce = format!(
            "DEC090-OK-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let prompt = format!(
            "Use your shell to run this command in your working folder: printf {nonce} > dec-090-check.txt && cat dec-090-check.txt. Then reply with exactly what it printed."
        );
        let root = tempfile::Builder::new()
            .prefix("ca-tui-live-dec090-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let engine = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (project, _home) = engine.block_on(async {
            let (mut service, mut native, _app, project, _stop, home) = live_project_with(
                root.path(),
                &owner_settings(&connection),
                Some(owner_team(harness)),
                vec![],
            )
            .await;
            let controller = native.controller.as_mut().unwrap();
            assign_pool(controller, &project, connection.id.as_str()).await;
            let delegated = controller
                .team_action(
                    crate::team::TeamAction::Delegate {
                        project_id: project.clone(),
                        harness: Some(harness.into()),
                        prompt: prompt.clone(),
                    },
                    1,
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    &delegated,
                    OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
                        if operation.result == crate::engine::owner::OwnerResult::Applied
                ),
                "the engine applies the owner's delegate: {delegated:?}"
            );
            let status = controller.team_status(project.clone()).await.unwrap();
            let queued = status
                .tasks
                .iter()
                .find(|task| task.prompt == prompt)
                .expect("the delegated task is listed");
            assert!(
                !matches!(
                    queued.status,
                    TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
                ),
                "no team runtime runs yet, so the task waits: {}",
                queued.status_label
            );
            drop(native);
            service.shutdown().await.unwrap();
            (project, home)
        });
        drop(engine);

        let (global, settings) = (
            root.path().join("global.toml"),
            root.path().join("project.toml"),
        );
        assert!(
            root.path().join("engine.db").is_file(),
            "the test engine keeps its engine.db"
        );
        assert!(
            crate::config::resolve_config(&global, &settings)
                .unwrap()
                .settings
                .model_access
                .is_some(),
            "the engine settings configure model access"
        );
        let database = rusqlite::Connection::open_with_flags(
            root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tx = database.unchecked_transaction().unwrap();
        let stored = crate::project_setup::load_setup(&tx, &project)
            .unwrap()
            .expect("the engine keeps the project it set up");
        drop(tx);
        drop(database);
        assert!(
            stored
                .team
                .sub_harnesses
                .iter()
                .any(|slot| slot.tools
                    && crate::team::launch_harness(&slot.harness) == Some(harness)),
            "the project's team has a tools-on {harness} sub-agent"
        );

        let mut team = Team::on_engine(root.path(), &global, &settings, &project, &owner);
        let deadline = Instant::now() + LIVE_RUN_LIMIT;
        let task = loop {
            let task = team.task(&prompt).expect("the delegated task is listed");
            if matches!(
                task.status,
                TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "task {} is still {}: {:?}",
                task.id,
                task.status_label,
                task.problem
            );
            team.tick();
        };
        assert_eq!(task.status, TaskStatus::Done, "{:?}", task.problem);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|text| text.contains(&nonce)),
            "{:?}",
            task.result
        );
        println!(
            "live DEC-090 {harness}: task {} {} after {} attempts",
            task.id, task.status_label, task.attempts
        );
    }

    /// DEC-090, live, Claude Code: a tools-on Claude Code sub-agent completes one shell task
    /// on the owner's signed-in managed Claude account. Runs in a live window only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
    fn a_tools_on_claude_agent_completes_a_turn_on_its_signed_in_managed_account() {
        managed_agent_completes_a_tool_turn(
            crate::team::runtime::live_failover_tests::OwnerAccount::claude(),
        );
    }

    /// DEC-090, live, Codex: a tools-on Codex sub-agent completes one shell task on the
    /// owner's managed ChatGPT account for Codex. Runs in a live window only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
    fn a_tools_on_codex_agent_completes_a_turn_on_its_signed_in_managed_account() {
        managed_agent_completes_a_tool_turn(
            crate::team::runtime::live_failover_tests::OwnerAccount::codex(),
        );
    }

    /// Research, live: a project created through the owner client of a test engine whose
    /// only account is the owner's signed-in managed one queues one research job; once its
    /// pool is assigned and that engine stops, the engine's research service on its database
    /// runs the job on that account under the owner engine's custody. Inputs: the owner's
    /// account and the research toggle. Output: the report the job wrote.
    #[cfg(target_os = "macos")]
    fn engine_research(
        owner: crate::team::runtime::live_failover_tests::OwnerAccount,
        kind: ResearchKind,
    ) -> String {
        use crate::research::runner::live_engine_tests::EngineResearch;
        let connection = owner.connection().clone();
        let (harness, _) = owner.launch();
        let root = tempfile::Builder::new()
            .prefix("ca-tui-live-research-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let engine = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (project, id, _home) = engine.block_on(async {
            let (mut service, mut native, _app, project, _stop, home) = live_project_with(
                root.path(),
                &owner_settings(&connection),
                Some(owner_team(harness)),
                vec![kind],
            )
            .await;
            let controller = native.controller.as_mut().unwrap();
            assign_pool(controller, &project, connection.id.as_str()).await;
            let jobs = controller.research_jobs(project.clone()).await.unwrap();
            let [job] = jobs.as_slice() else {
                panic!("one research job for the one toggle: {jobs:?}");
            };
            assert_eq!(
                (job.kind, job.harness.as_str(), job.tools),
                (kind, harness, true),
                "the job runs on the team's tools-on {harness} agent"
            );
            assert!(
                matches!(
                    job.status,
                    ResearchStatus::Queued | ResearchStatus::NeedsPool
                ),
                "no research runs yet: {}",
                job.status_label
            );
            let id = job.id;
            drop(native);
            service.shutdown().await.unwrap();
            (project, id, home)
        });
        drop(engine);

        let (global, settings) = (
            root.path().join("global.toml"),
            root.path().join("project.toml"),
        );
        assert!(
            root.path().join("engine.db").is_file(),
            "the test engine keeps its engine.db"
        );
        let research = EngineResearch::on_engine(root.path(), &global, &settings, &owner);
        let job = research.until_ended(id, LIVE_RUN_LIMIT);
        drop(research);
        assert_eq!(
            job.status,
            crate::research::jobs::ResearchStatus::Done,
            "{:?}",
            job.error
        );
        assert_eq!(
            job.report_path.as_deref(),
            Some(format!("context/research/{}.md", kind.name()).as_str())
        );
        let report = engine_state_folder(root.path(), &project)
            .join("context")
            .join("research")
            .join(format!("{}.md", kind.name()));
        std::fs::read_to_string(&report)
            .unwrap_or_else(|error| panic!("{}: {error}", report.display()))
    }

    /// Research, live, Codex: a competitor research job runs on the owner's managed ChatGPT
    /// account for Codex and writes its report with links. Runs in a live window only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
    fn research_runs_on_a_signed_in_managed_codex_account() {
        let report = engine_research(
            crate::team::runtime::live_failover_tests::OwnerAccount::codex(),
            ResearchKind::CompetitorResearch,
        );
        assert!(report.starts_with("# Competitor research\n"), "{report}");
        assert!(report.contains("Researched by Codex"), "{report}");
        assert!(report.contains("## Links and references"), "{report}");
        assert!(report.contains("https://"), "{report}");
    }

    /// Research, live, Claude Code: a market research job runs on the owner's signed-in
    /// managed Claude account and writes its report with links. Runs in a live window only.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
    fn research_runs_on_a_signed_in_managed_claude_account() {
        let report = engine_research(
            crate::team::runtime::live_failover_tests::OwnerAccount::claude(),
            ResearchKind::MarketResearch,
        );
        assert!(report.starts_with("# Market research\n"), "{report}");
        assert!(report.contains("Researched by Claude Code"), "{report}");
        assert!(report.contains("## Links and references"), "{report}");
        assert!(report.contains("https://"), "{report}");
    }

    /// Turning the live master on for a project whose defaults cannot open a master still
    /// turns the switch on, and the TUI names why no session opened instead of leaving a
    /// sessionless live master unexplained (LMS-M1, LMS-L1).
    #[tokio::test]
    async fn turning_the_live_master_on_names_why_no_session_opened() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-unopened-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let (mut service, mut native, mut app, project, _stop, _home) =
            live_project(root.path()).await;
        let version = native
            .controller
            .as_mut()
            .unwrap()
            .team_status(project.clone())
            .await
            .unwrap()
            .settings_version;
        app.request_team_change(crate::team::TeamAction::LiveMaster {
            project_id: project.clone(),
            on: true,
            version,
        })
        .unwrap();
        let query = app.take_setup_query().unwrap();
        let event = native.project_setup(query).await;
        app.apply_owner(OwnerEvent::Setup(Box::new(event))).unwrap();
        let shown = format!("{:?}", app.setup_view);
        assert!(
            shown.contains("No account pool is assigned to this project as its master"),
            "the TUI names why no session opened: {shown} {:?}",
            app.setup_problem
        );
        let controller = native.controller.as_mut().unwrap();
        let status = controller.team_status(project.clone()).await.unwrap();
        assert_eq!(status.live_master, Some(true), "the live switch is on");
        assert_eq!(
            master_session(controller, &project).await,
            None,
            "no master session opened"
        );
        drop(native);
        service.shutdown().await.unwrap();
    }

    /// A project whose live master switch is already on with no session, as an existing
    /// project's is, gets the owner's default session from the session check the master
    /// view sends at open, and a second check keeps it (LMS-1).
    #[tokio::test]
    async fn a_switch_already_on_gets_the_owner_s_default_session_from_the_session_check() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-on-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let (mut service, mut native, _app, project, _stop, _home) =
            live_project(root.path()).await;
        let controller = native.controller.as_mut().unwrap();
        assign_live_pool(controller, &project).await;
        let version = controller
            .team_status(project.clone())
            .await
            .unwrap()
            .settings_version;
        let switched = controller
            .team_action(
                crate::team::TeamAction::LiveMaster {
                    project_id: project.clone(),
                    on: true,
                    version,
                },
                1,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                &switched,
                OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
                    if operation.result == crate::engine::owner::OwnerResult::Applied
            ),
            "the switch is on: {switched:?}"
        );
        assert_eq!(
            master_session(controller, &project).await,
            None,
            "a switch turned on before LMS-1 left the master without a session"
        );
        let mut opened = Vec::new();
        for generation in [2, 3] {
            let event = native
                .project_setup(SetupQuery::OpenLiveSession {
                    project: project.clone(),
                    generation,
                })
                .await;
            assert_eq!(
                event.result,
                Ok(SetupView::Changed(
                    crate::engine::owner::OwnerResult::Applied
                )),
                "the session check applies"
            );
            opened.push(master_session(native.controller.as_mut().unwrap(), &project).await);
        }
        let Some((session, phase)) = opened[0].clone() else {
            panic!("the session check opened no master session: {opened:?}");
        };
        assert_eq!(phase, crate::master_session::MasterSessionPhase::Configured);
        assert_eq!(
            opened[1],
            Some((
                session,
                crate::master_session::MasterSessionPhase::Configured
            )),
            "a second check keeps the live session"
        );
        drop(native);
        service.shutdown().await.unwrap();
    }
}
