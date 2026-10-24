use super::*;
use crate::agents::AgentView;
use crate::engine::EngineError;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(all(test, unix)),
    expect(dead_code, reason = "built by the agents view")
)]
pub(in crate::tui) enum AgentsRequest {
    List { project: Option<ProjectId> },
    Stop { id: String },
    Interrupt { id: String },
    Restart { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum AgentsReply {
    Listed(Vec<AgentView>),
    Agent(AgentView),
    Interrupted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct AgentsEvent {
    pub result: Result<AgentsReply, String>,
}

impl Connection {
    /// Queues one agent list or control on the paired owner connection. The reply
    /// arrives later as `OwnerEvent::Agents`. Input: the request. Output: queued or why not.
    #[cfg_attr(
        not(all(test, unix)),
        expect(dead_code, reason = "called by the agents view")
    )]
    pub(in crate::tui) fn agents(&mut self, request: AgentsRequest) -> Result<(), String> {
        self.owner_job(Job::Agents(Box::new(request)))
    }
}

impl NativeOwner {
    /// Runs one agent list or control through the owner controller and turns any engine
    /// error into its plain code. Input: the request. Output: the reply event.
    pub(super) async fn agents(&mut self, request: AgentsRequest) -> AgentsEvent {
        let result = async {
            if !self.ready {
                return Err(EngineError("controller_disconnected".into()));
            }
            let controller = self
                .controller
                .as_ref()
                .ok_or_else(|| EngineError("controller_disconnected".into()))?;
            Ok(match request {
                AgentsRequest::List { project } => {
                    AgentsReply::Listed(controller.agents(project).await?)
                }
                AgentsRequest::Stop { id } => AgentsReply::Agent(controller.stop_agent(id).await?),
                AgentsRequest::Interrupt { id } => {
                    controller.interrupt_agent(id.clone()).await?;
                    AgentsReply::Interrupted(id)
                }
                AgentsRequest::Restart { id } => {
                    AgentsReply::Agent(controller.restart_agent(id).await?)
                }
            })
        }
        .await;
        AgentsEvent {
            result: result.map_err(|error: EngineError| bounded_problem(&error.0)),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agents::AgentState;
    use crate::agents::tests::{alive, install_fixture, pids, spec, until};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    async fn owner_event(connection: &mut Connection) -> OwnerEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(event) = connection.poll_owner().unwrap() {
                return event;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "owner event timed out"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn reply(
        connection: &mut Connection,
        request: AgentsRequest,
    ) -> Result<AgentsReply, String> {
        connection.agents(request).unwrap();
        loop {
            if let OwnerEvent::Agents(event) = owner_event(connection).await {
                return event.result;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tui_agent_controls_reach_the_engine_and_touch_only_one_agent() {
        let root = tempfile::Builder::new()
            .prefix("ca-tagent-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().canonicalize().unwrap();
        install_fixture(&home);
        let global = home.join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            &home,
            "e.sock",
            &global,
            &home.join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let agents = service.owner_runtime().unwrap().agents().unwrap();
        let first = agents
            .launch(spec(&home, "first", Some("echo one")), None)
            .unwrap();
        let second = agents.launch(spec(&home, "second", None), None).unwrap();
        let (first_pid, first_grandchild) = pids(&home, "first");
        let (second_pid, second_grandchild) = pids(&home, "second");
        let mut connection = Connection::connect_paired(
            Client::new(home.join("e.sock"), Duration::from_secs(10)).unwrap(),
        )
        .unwrap();
        while !matches!(
            owner_event(&mut connection).await,
            OwnerEvent::Access { ready: true, .. }
        ) {}

        let Ok(AgentsReply::Listed(listed)) =
            reply(&mut connection, AgentsRequest::List { project: None }).await
        else {
            panic!("agent list required")
        };
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|agent| agent.state == AgentState::Running
            && agent.status != "Stopped when CrazyAgent closed"));

        let Ok(AgentsReply::Agent(stopped)) = reply(
            &mut connection,
            AgentsRequest::Stop {
                id: first.id.clone(),
            },
        )
        .await
        else {
            panic!("stopped agent required")
        };
        assert_eq!(stopped.state, AgentState::Stopped);
        assert!(stopped.can_restart && !stopped.can_stop);
        until("the stopped group to be gone", || {
            !alive(first_pid) && !alive(first_grandchild)
        });
        assert!(alive(second_pid) && alive(second_grandchild));

        assert_eq!(
            reply(
                &mut connection,
                AgentsRequest::Interrupt {
                    id: second.id.clone(),
                },
            )
            .await,
            Err("agent_not_busy".into())
        );
        assert_eq!(
            reply(&mut connection, AgentsRequest::Stop { id: "0".repeat(32) },).await,
            Err("agent_not_running".into())
        );

        std::fs::remove_file(home.join("work").join("first").join("pids.json")).unwrap();
        let Ok(AgentsReply::Agent(restarted)) = reply(
            &mut connection,
            AgentsRequest::Restart {
                id: first.id.clone(),
            },
        )
        .await
        else {
            panic!("restarted agent required")
        };
        assert_eq!(restarted.id, first.id);
        assert_eq!(restarted.state, AgentState::Running);
        assert_eq!(restarted.attempt, 2);
        let (restarted_pid, _) = pids(&home, "first");
        assert_ne!(restarted_pid, first_pid);
        assert!(alive(second_pid));

        agents.stop(&first.id).unwrap();
        agents.stop(&second.id).unwrap();
        until("every fixture to be gone", || {
            !alive(restarted_pid) && !alive(second_pid) && !alive(second_grandchild)
        });
    }
}
