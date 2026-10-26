use super::*;
use crate::agents::{AccountUse, AgentView, Holder, LIST_MAX};
use crate::engine::owner::{AGENTS_PAGE, AgentPage, RELEASE_WAIT};
use crate::master_session::MasterOwnerAction;
use crate::protocol::{CredentialRef, ProjectId};

const PAGES_MAX: usize = LIST_MAX.div_ceil(AGENTS_PAGE);

impl OwnerController {
    fn agents_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads one page of launched agents. Inputs: a project, or none for every project,
    /// and the previous page's `next`. Output: agents with plain status and controls.
    pub async fn agents_page(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<AgentPage, EngineError> {
        self.agents_client()?.agents(project, offset).await
    }

    /// Reads every launched agent, newest first, up to `LIST_MAX`.
    /// Input: a project, or none for every project. Output: the agents.
    pub async fn agents(&self, project: Option<ProjectId>) -> Result<Vec<AgentView>, EngineError> {
        let mut agents = Vec::new();
        let mut offset = 0;
        for _ in 0..PAGES_MAX {
            let page = self.agents_page(project.clone(), offset).await?;
            agents.extend(page.agents);
            match page.next {
                Some(next) if next > offset && agents.len() < LIST_MAX => offset = next,
                _ => break,
            }
        }
        agents.truncate(LIST_MAX);
        Ok(agents)
    }

    /// Stops one agent and its whole process group, leaving every other agent running.
    /// Input: the agent id. Output: the stopped agent.
    pub async fn stop_agent(&self, id: String) -> Result<AgentView, EngineError> {
        self.agents_client()?.stop_agent(id).await
    }

    /// Interrupts the current turn of one agent, which keeps running. Input: the agent id.
    pub async fn interrupt_agent(&self, id: String) -> Result<(), EngineError> {
        self.agents_client()?.interrupt_agent(id).await
    }

    /// Restarts one agent with its first task. Input: the agent id. Output: the agent.
    pub async fn restart_agent(&self, id: String) -> Result<AgentView, EngineError> {
        self.agents_client()?.restart_agent(id).await
    }

    /// Reads everything that holds one account's profile, so a refused sign-out can name
    /// it. Input: the account. Output: its running agents and other holders, newest first.
    pub async fn account_use(&self, account: CredentialRef) -> Result<AccountUse, EngineError> {
        self.agents_client()?.account_use(account).await
    }

    /// Ends each holder through its own owner action: a research job is cancelled and a
    /// master conversation is closed. A holder that already ended is refused, which is not
    /// an error. Inputs: the holders and the client generation. Output: Ok once each was
    /// answered, or `controller_pending_uncertain` when one may still be recorded.
    pub async fn end_holders(
        &mut self,
        holders: &[Holder],
        generation: u64,
    ) -> Result<(), EngineError> {
        for holder in holders {
            let response = match holder {
                Holder::Research { project, job, .. } => {
                    self.cancel_research(project.clone(), *job, generation)
                        .await?
                }
                Holder::Master { project, session } => {
                    self.read(OwnerRead::ProjectMaster {
                        project_id: project.clone(),
                    })
                    .await?;
                    self.begin_mutation(
                        OwnerMutation::ProjectMaster {
                            action: MasterOwnerAction::Close {
                                project_id: project.clone(),
                                session_id: session.clone(),
                            },
                        },
                        generation,
                    )?;
                    self.send_pending().await?
                }
            };
            if matches!(response, OwnerResponse::OutcomeUnknown { .. }) {
                return Err(EngineError("controller_pending_uncertain".into()));
            }
        }
        Ok(())
    }

    /// Ends everything that holds one account's profile, so it can be signed out: stops its
    /// agents, cancels its research jobs and closes its master conversations, then waits,
    /// at most `RELEASE_WAIT`, until nothing holds it. Inputs: the account and the client
    /// generation. Output: what still holds it, nothing once all have ended.
    pub async fn release_account(
        &mut self,
        account: CredentialRef,
        generation: u64,
    ) -> Result<AccountUse, EngineError> {
        self.release_within(account, generation, RELEASE_WAIT).await
    }

    /// `release_account` with its wait given. Inputs: the account, the client generation
    /// and the wait. Output: what still holds the account after the wait.
    pub(crate) async fn release_within(
        &mut self,
        account: CredentialRef,
        generation: u64,
        wait: Duration,
    ) -> Result<AccountUse, EngineError> {
        let client = self.shared_client()?;
        client.stop_agents_using(account.clone()).await?;
        let holders = client.account_holders(account.clone()).await?;
        self.end_holders(&holders, generation).await?;
        client.await_account_free(account, wait).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agents::AgentState;
    use crate::agents::tests::{alive, install_fixture, pids, spec, until};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_controls_reach_the_engine_and_touch_only_one_agent() {
        let root = tempfile::Builder::new()
            .prefix("ca-cagent-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
        let (_, controller) = OwnerController::attach_configured(
            Client::new(home.join("e.sock"), Duration::from_secs(10)).unwrap(),
            OwnerClientKind::Desktop,
        )
        .await
        .unwrap();

        let listed = controller.agents(None).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|agent| agent.state == AgentState::Running
            && agent.status != "Stopped when CrazyAgent closed"));

        let stopped = controller.stop_agent(first.id.clone()).await.unwrap();
        assert_eq!(stopped.state, AgentState::Stopped);
        assert!(stopped.can_restart && !stopped.can_stop);
        until("the stopped group to be gone", || {
            !alive(first_pid) && !alive(first_grandchild)
        });
        assert!(alive(second_pid) && alive(second_grandchild));

        assert_eq!(
            controller.interrupt_agent(second.id.clone()).await,
            Err(EngineError("agent_not_busy".into()))
        );
        assert_eq!(
            controller.stop_agent("0".repeat(32)).await,
            Err(EngineError("agent_not_running".into()))
        );

        std::fs::remove_file(home.join("work").join("first").join("pids.json")).unwrap();
        let restarted = controller.restart_agent(first.id.clone()).await.unwrap();
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

    /// A real engine whose owner request deadline is 1000 ms, its agent register and a
    /// desktop owner controller attached to it. Input: the temp folder prefix. Output: the
    /// folder, the engine's stop sender and service, which must outlive the test, the
    /// register and the controller.
    async fn engine_with_one_second_deadline(
        prefix: &str,
    ) -> (
        tempfile::TempDir,
        tokio::sync::watch::Sender<bool>,
        EngineService,
        std::sync::Arc<crate::agents::Agents>,
        OwnerController,
    ) {
        let root = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().canonicalize().unwrap();
        let global = home.join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 1000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
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
        let (_, controller) = OwnerController::attach_configured(
            Client::new(home.join("e.sock"), Duration::from_secs(10)).unwrap(),
            OwnerClientKind::Desktop,
        )
        .await
        .unwrap();
        (root, stop, service, agents, controller)
    }

    /// The owner stops a hosted agent, such as a team agent, whose host takes longer to
    /// end it than the engine's 1000 ms owner request deadline. The owner still gets the
    /// stopped agent, never an unknown outcome, and the agents view agrees.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_slower_than_the_request_deadline_answers_the_stopped_agent() {
        let (_root, _stop, _service, agents, controller) =
            engine_with_one_second_deadline("ca-cstop-").await;
        let project = ProjectId::new("slow-stop").unwrap();
        let hosted = agents
            .host(
                crate::agents::HostedAgent {
                    project: project.clone(),
                    role: crate::agents::AgentRole::SubAgent,
                    label: "Team sub-agent 1".into(),
                    harness: "codex".into(),
                    host: "team",
                    reference: "1".into(),
                    account: None,
                },
                None,
            )
            .unwrap();

        let id = hosted.id().to_owned();
        let host = std::thread::spawn(move || {
            let asked = std::time::Instant::now();
            while hosted.requested().is_none() {
                assert!(asked.elapsed() < Duration::from_secs(5), "no stop arrived");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(hosted.requested(), Some(crate::agents::HostedRequest::Stop));
            std::thread::sleep(Duration::from_millis(1500));
            hosted.finish(AgentState::Stopped, None);
        });

        let stopped = controller.stop_agent(id).await.unwrap();
        host.join().unwrap();
        assert_eq!(stopped.state, AgentState::Stopped);
        assert_eq!(stopped.status, "Stopped");
        assert!(!stopped.can_stop);
        let listed = controller.agents(Some(project)).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, AgentState::Stopped);
        assert_eq!(listed[0].status, "Stopped");
    }

    /// A refused sign-out names the agents using the account and stops them over the owner
    /// connection: the listing names the hosted agent recorded with the account and none of
    /// another's, and the stop returns once its host has ended it, leaving none using it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agents_using_an_account_are_named_and_stopped_over_the_owner_connection() {
        let (_root, _stop, _service, agents, mut controller) =
            engine_with_one_second_deadline("ca-cusing-").await;
        let project = ProjectId::new("sign-out").unwrap();
        let account = CredentialRef::new("claude-account-c").unwrap();
        let hosted = agents
            .host(
                crate::agents::HostedAgent {
                    project: project.clone(),
                    role: crate::agents::AgentRole::SubAgent,
                    label: "Team sub-agent 4".into(),
                    harness: "claude-code".into(),
                    host: "team",
                    reference: "4".into(),
                    account: Some(account.clone()),
                },
                None,
            )
            .unwrap();

        let used = controller.account_use(account.clone()).await.unwrap();
        assert!(used.holders.is_empty());
        let using = used.agents;
        assert_eq!(using.len(), 1);
        assert_eq!(
            (using[0].project.clone(), using[0].label.as_str()),
            (project.clone(), "Team sub-agent 4")
        );
        let other = CredentialRef::new("claude-account-d").unwrap();
        assert!(controller.account_use(other).await.unwrap().is_free());

        let host = std::thread::spawn(move || {
            let asked = std::time::Instant::now();
            while hosted.requested().is_none() {
                assert!(asked.elapsed() < Duration::from_secs(5), "no stop arrived");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(hosted.requested(), Some(crate::agents::HostedRequest::Stop));
            hosted.finish(AgentState::Stopped, None);
        });
        let left = controller.release_account(account, 1).await.unwrap();
        host.join().unwrap();
        assert!(left.is_free());
        let listed = controller.agents(Some(project)).await.unwrap();
        assert_eq!(listed[0].state, AgentState::Stopped);
    }
}
