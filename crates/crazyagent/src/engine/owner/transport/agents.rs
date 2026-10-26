use super::*;
use crate::agents::{AgentView, Agents, LIST_MAX};
use crate::protocol::CredentialRef;
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
pub(super) const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
pub const AGENTS_PAGE: usize = 8;
const USING_PAGES_MAX: usize = LIST_MAX.div_ceil(AGENTS_PAGE);

/// Every code an agent control can answer an owner client with: the agent register's
/// refusals, this protocol's own, and the owner connection's it travels on. Launch
/// failures, `launch_*`, are not listed; a client shows them as one launch failure.
pub const AGENT_CONTROL_CODES: &[&str] = &[
    "agent_account_invalid",
    "agent_busy",
    "agent_hosted",
    "agent_identity_unavailable",
    "agent_label_invalid",
    "agent_not_busy",
    "agent_not_found",
    "agent_not_running",
    "agent_prompt_invalid",
    "agent_reconnected_read_only",
    "agent_registry_unavailable",
    "agent_restart_conflict",
    "agent_restart_limit",
    "agent_restart_needs_key",
    "agent_restart_unavailable",
    "agent_root_unavailable",
    "agent_root_unsafe",
    "agent_run_time_invalid",
    "agent_spec_invalid",
    "agent_state_unavailable",
    "agent_thread_unavailable",
    "agents_outcome_unknown",
    "agents_owner_unavailable",
    "agents_reply_bounds",
    "agents_request_bounds",
    "agents_request_invalid",
    "agents_unavailable",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_capability_content_changed",
    "owner_capability_replaced_or_unsafe",
    "owner_entropy_unavailable",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List {
        project: Option<ProjectId>,
        offset: u16,
    },
    Stop {
        id: String,
    },
    Interrupt {
        id: String,
    },
    Restart {
        id: String,
    },
    Using {
        account: CredentialRef,
        offset: u16,
    },
    StopUsing {
        account: CredentialRef,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    agents: Action,
}

/// One page of the agents the engine launched, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPage {
    pub agents: Vec<AgentView>,
    pub next: Option<u16>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Page { page: AgentPage },
    Agent { agent: AgentView },
    Done,
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("agents_outcome_unknown")
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Action {
    fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::List { .. } | Self::Using { .. } | Self::StopUsing { .. } => Ok(()),
            Self::Stop { id } | Self::Interrupt { id } | Self::Restart { id } if valid_id(id) => {
                Ok(())
            }
            _ => Err(failure("agent_not_found")),
        }
    }
}

impl OwnerClient {
    /// Reads one page of the agents the engine launched. Inputs: a project, or none for
    /// every project, and the page offset. Output: agents with their state, plain status
    /// and the controls the owner can use now, and the next offset.
    pub async fn agents(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<AgentPage, EngineError> {
        match self
            .agents_exchange(Action::List { project, offset })
            .await?
        {
            Reply::Page { page } if page.agents.len() <= AGENTS_PAGE => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Stops one agent and its whole process group. Input: the agent id.
    pub async fn stop_agent(&self, id: String) -> Result<AgentView, EngineError> {
        match self.agents_exchange(Action::Stop { id }).await? {
            Reply::Agent { agent } => Ok(agent),
            _ => Err(unknown()),
        }
    }

    /// Interrupts the current turn of one agent; it keeps running. Input: the agent id.
    pub async fn interrupt_agent(&self, id: String) -> Result<(), EngineError> {
        match self.agents_exchange(Action::Interrupt { id }).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Restarts one agent with its first task, stopping it first when it still runs.
    /// Input: the agent id. Output: the agent running again.
    pub async fn restart_agent(&self, id: String) -> Result<AgentView, EngineError> {
        match self.agents_exchange(Action::Restart { id }).await? {
            Reply::Agent { agent } => Ok(agent),
            _ => Err(unknown()),
        }
    }

    /// Reads every running agent that signs in with one account, newest first, up to
    /// `LIST_MAX`, so a refused sign-out can name them. Input: the account. Output: the
    /// agents with their project, label and plain status.
    pub async fn agents_using(
        &self,
        account: CredentialRef,
    ) -> Result<Vec<AgentView>, EngineError> {
        let mut agents = Vec::new();
        let mut offset = 0;
        for _ in 0..USING_PAGES_MAX {
            let action = Action::Using {
                account: account.clone(),
                offset,
            };
            let page = match self.agents_exchange(action).await? {
                Reply::Page { page } if page.agents.len() <= AGENTS_PAGE => page,
                _ => return Err(unknown()),
            };
            agents.extend(page.agents);
            match page.next {
                Some(next) if next > offset && agents.len() < LIST_MAX => offset = next,
                _ => break,
            }
        }
        agents.truncate(LIST_MAX);
        Ok(agents)
    }

    /// Stops every running agent that signs in with one account, so it can be signed out,
    /// then reads those still using it. Input: the account. Output: the agents still using
    /// it, none once all have stopped.
    pub async fn stop_agents_using(
        &self,
        account: CredentialRef,
    ) -> Result<Vec<AgentView>, EngineError> {
        let action = Action::StopUsing {
            account: account.clone(),
        };
        match self.agents_exchange(action).await? {
            Reply::Done => self.agents_using(account).await,
            _ => Err(unknown()),
        }
    }

    async fn agents_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        action.validate()?;
        tokio::time::timeout(self.timeout.max(CONTROL_TIMEOUT), async {
            let frame = encode_frame(&Envelope { agents: action })
                .map_err(|_| failure("agents_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("agents_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("agents_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("agents_owner_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

impl OwnerRuntimeHandle {
    /// Returns the engine's register of launched agents, opening it on first use, which
    /// reattaches agents still alive from before a restart and marks the rest
    /// "Stopped when CrazyAgent closed". Engine launchers start agents through it.
    pub fn agents(&self) -> Result<Arc<Agents>, EngineError> {
        self.engine.agents(&engine_root(&self.authority)?)
    }
}

impl Engine {
    pub(crate) fn agents(&self, root: &Path) -> Result<Arc<Agents>, EngineError> {
        let mut slot = self
            .agents
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))?;
        if let Some(agents) = slot.as_ref() {
            return Ok(agents.clone());
        }
        let agents = Arc::new(Agents::open(root).map_err(|problem| EngineError(problem.0))?);
        *slot = Some(agents.clone());
        Ok(agents)
    }
}

fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("agent_registry_unavailable"))
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "agents_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("agents_request_bounds"));
    }
    let Envelope { agents: action } =
        serde_json::from_slice(bytes).map_err(|_| failure("agents_request_invalid"))?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    action.validate()?;
    let root = engine_root(authority)?;
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || {
        let agents = engine.agents(&root)?;
        let plain = |problem: crate::harnesses::launch::LaunchError| EngineError(problem.0);
        Ok(match action {
            Action::List { project, offset } => {
                let offset = usize::from(offset);
                let mut listed = agents
                    .list(project.as_ref(), offset, AGENTS_PAGE + 1)
                    .map_err(plain)?;
                let next = (listed.len() > AGENTS_PAGE)
                    .then(|| u16::try_from(offset + AGENTS_PAGE).ok())
                    .flatten();
                listed.truncate(AGENTS_PAGE);
                Reply::Page {
                    page: AgentPage {
                        agents: listed,
                        next,
                    },
                }
            }
            Action::Stop { id } => Reply::Agent {
                agent: agents.stop(&id).map_err(plain)?,
            },
            Action::Interrupt { id } => {
                agents.interrupt(&id).map_err(plain)?;
                Reply::Done
            }
            Action::Restart { id } => Reply::Agent {
                agent: agents.restart(&id).map_err(plain)?,
            },
            Action::Using { account, offset } => {
                let offset = usize::from(offset);
                let listed = agents.using(&account).map_err(plain)?;
                let next = (listed.len() > offset + AGENTS_PAGE)
                    .then(|| u16::try_from(offset + AGENTS_PAGE).ok())
                    .flatten();
                Reply::Page {
                    page: AgentPage {
                        agents: listed.into_iter().skip(offset).take(AGENTS_PAGE).collect(),
                        next,
                    },
                }
            }
            Action::StopUsing { account } => {
                agents.stop_using(&account).map_err(plain)?;
                Reply::Done
            }
        })
    })
    .await
    .map_err(|_| failure("agents_unavailable"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the plain codes a source file refuses with through `failure`, before its
    /// tests. Input: the file text. Output: each code once.
    fn refusals(source: &str) -> std::collections::BTreeSet<&str> {
        let code = source.split("mod tests").next().unwrap();
        code.match_indices("failure(\"")
            .filter_map(|(at, call)| {
                let rest = &code[at + call.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect()
    }

    /// The listed codes cover every refusal the agent register and this protocol make,
    /// so a client that passes listed codes through never hides a real one.
    #[test]
    fn agent_control_codes_cover_every_refusal_of_the_register_and_protocol() {
        let register = refusals(include_str!("../../../agents.rs"));
        let protocol = refusals(include_str!("agents.rs"));
        assert!(register.len() >= 20 && protocol.len() >= 5);
        for code in register.iter().chain(&protocol) {
            assert!(
                code.starts_with("launch_") || AGENT_CONTROL_CODES.contains(code),
                "{code} is not in AGENT_CONTROL_CODES"
            );
        }
        let mut sorted = AGENT_CONTROL_CODES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, AGENT_CONTROL_CODES);
        assert!(AGENT_CONTROL_CODES.iter().all(|code| {
            !code.starts_with("launch_")
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }));
    }
}
