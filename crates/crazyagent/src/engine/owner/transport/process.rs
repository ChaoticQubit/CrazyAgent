use super::*;
use crate::agent_home::{PROCESS_BYTES_MAX, TEXT_BYTES_MAX};
use crate::protocol::encode_frame;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

const REQUEST_BYTES_MAX: usize = 1024;
const PAYLOAD_TIME: Duration = Duration::from_secs(5);

/// Every code a Process request can answer an owner client with: the instruction
/// store's refusals, this protocol's own, and the owner connection's it travels on.
pub const PROCESS_CODES: &[&str] = &[
    "agent_instructions_invalid",
    "agent_instructions_too_large",
    "agent_instructions_unreadable",
    "agent_instructions_unsafe",
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
    "process_agent_unknown",
    "process_agent_unsupported",
    "process_folder_missing",
    "process_folder_unsafe",
    "process_import_missing",
    "process_outcome_unknown",
    "process_owner_unavailable",
    "process_project_folder_missing",
    "process_project_unknown",
    "process_reply_bounds",
    "process_request_bounds",
    "process_request_invalid",
    "process_text_invalid",
    "process_text_too_large",
    "process_text_unreadable",
    "process_text_unsafe",
    "process_text_unwritable",
    "process_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Read {
        project: Option<ProjectId>,
    },
    Save {
        project: Option<ProjectId>,
    },
    ImportGlobal,
    Default,
    Agent {
        project: ProjectId,
        agent: Option<i64>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    process: Action,
    text_bytes: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Text {
        bytes: u32,
    },
    Saved,
    Received {
        agent: i64,
        harness: String,
        launched: bool,
        skills: Vec<String>,
        bytes: u32,
    },
    Rejected {
        code: String,
    },
}

/// What one team agent received at its last launch, read back from its private home: the
/// agent id, its harness, whether it has launched, the text of its harness's instruction
/// file (empty when none was given) and the names of the skills installed for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInstructions {
    pub agent: i64,
    pub harness: String,
    pub launched: bool,
    pub text: String,
    pub skills: Vec<String>,
}

enum Answer {
    Text(String),
    Saved,
    Agent(AgentInstructions),
}

fn unknown() -> EngineError {
    failure("process_outcome_unknown")
}

impl OwnerClient {
    /// Reads the owner's instructions: the app-wide text, or one project's text.
    /// Input: none for the app, or the project. Output: the text, empty when none is saved.
    pub async fn process_text(&self, project: Option<ProjectId>) -> Result<String, EngineError> {
        self.process_text_reply(Action::Read { project }).await
    }

    /// Saves the owner's instructions whole: the app-wide text, or one project's text.
    /// Inputs: none for the app, or the project, and the text, at most
    /// `agent_home::TEXT_BYTES_MAX` bytes. Output: unit, or a `PROCESS_CODES` refusal.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
    ) -> Result<(), EngineError> {
        crate::agent_home::validate(&text).map_err(|problem| EngineError(problem.0))?;
        match self
            .process_exchange(Action::Save { project }, Some(text))
            .await?
        {
            (Reply::Saved, None) => Ok(()),
            _ => Err(unknown()),
        }
    }

    /// Reads the owner's own global Claude Code instructions, `~/.claude/CLAUDE.md`, for a
    /// preview the owner may then save. Only this explicit request reads that file.
    /// Output: the text, or `process_import_missing` when there is none.
    pub async fn import_global_instructions(&self) -> Result<String, EngineError> {
        self.process_text_reply(Action::ImportGlobal).await
    }

    /// Reads the CrazyAgent default process, for a preview the owner may then save.
    pub async fn default_process(&self) -> Result<String, EngineError> {
        self.process_text_reply(Action::Default).await
    }

    /// Reads what one team agent of a project received at its last launch. Inputs: the
    /// project and the agent id, or none for the project's newest master. Output: the
    /// read-only view, or a `PROCESS_CODES` refusal such as `agent_instructions_unsafe`.
    pub async fn agent_instructions(
        &self,
        project: ProjectId,
        agent: Option<i64>,
    ) -> Result<AgentInstructions, EngineError> {
        match self
            .process_exchange(Action::Agent { project, agent }, None)
            .await?
        {
            (
                Reply::Received {
                    agent,
                    harness,
                    launched,
                    skills,
                    ..
                },
                Some(text),
            ) => Ok(AgentInstructions {
                agent,
                harness,
                launched,
                text,
                skills,
            }),
            _ => Err(unknown()),
        }
    }

    async fn process_text_reply(&self, action: Action) -> Result<String, EngineError> {
        match self.process_exchange(action, None).await? {
            (Reply::Text { .. }, Some(text)) => Ok(text),
            _ => Err(unknown()),
        }
    }

    async fn process_exchange(
        &self,
        action: Action,
        text: Option<String>,
    ) -> Result<(Reply, Option<String>), EngineError> {
        tokio::time::timeout(self.timeout, async {
            let envelope = Envelope {
                process: action,
                text_bytes: text.as_ref().map(|text| text.len() as u32),
            };
            let frame = encode_frame(&envelope).map_err(|_| failure("process_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let unavailable = |_| failure("process_owner_unavailable");
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(unavailable)?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream.write_all(&packet).await.map_err(unavailable)?;
            stream.write_all(&frame).await.map_err(unavailable)?;
            if let Some(text) = &text {
                stream
                    .write_all(text.as_bytes())
                    .await
                    .map_err(unavailable)?;
            }
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                Reply::Text { bytes } => {
                    let text = read_text(&mut stream, bytes).await.map_err(|_| unknown())?;
                    Ok((Reply::Text { bytes }, Some(text)))
                }
                Reply::Received {
                    agent,
                    harness,
                    launched,
                    skills,
                    bytes,
                } => {
                    let text = read_payload(&mut stream, bytes, PROCESS_BYTES_MAX)
                        .await
                        .map_err(|_| unknown())?;
                    if text.contains('\0') {
                        return Err(unknown());
                    }
                    let reply = Reply::Received {
                        agent,
                        harness,
                        launched,
                        skills,
                        bytes,
                    };
                    Ok((reply, Some(text)))
                }
                reply => Ok((reply, None)),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Reads one owner text payload that follows a frame: exactly `bytes` bytes of UTF-8, at
/// most `TEXT_BYTES_MAX`, with no NUL. Inputs: the stream and the announced size.
/// Output: the text.
async fn read_text<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    bytes: u32,
) -> Result<String, EngineError> {
    let text = read_payload(stream, bytes, TEXT_BYTES_MAX).await?;
    crate::agent_home::validate(&text).map_err(|problem| EngineError(problem.0))?;
    Ok(text)
}

/// Reads one payload that follows a frame: exactly `bytes` bytes of UTF-8 within `bytes_max`,
/// waiting at most `PAYLOAD_TIME`. Inputs: the stream, the announced size and the bound.
/// Output: the text.
async fn read_payload<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
    bytes: u32,
    bytes_max: usize,
) -> Result<String, EngineError> {
    let size = bytes as usize;
    if size > bytes_max {
        return Err(failure("process_text_too_large"));
    }
    let mut buffer = vec![0; size];
    tokio::time::timeout(PAYLOAD_TIME, stream.read_exact(&mut buffer))
        .await
        .map_err(|_| failure("process_request_invalid"))?
        .map_err(|_| failure("process_request_invalid"))?;
    String::from_utf8(buffer).map_err(|_| failure("process_text_invalid"))
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
    let (reply, text) = match handle(stream, engine, authority, packet, admission, bytes).await {
        Ok(Answer::Text(text)) => (
            Reply::Text {
                bytes: text.len() as u32,
            },
            Some(text),
        ),
        Ok(Answer::Saved) => (Reply::Saved, None),
        Ok(Answer::Agent(view)) => {
            assert!(view.text.len() <= PROCESS_BYTES_MAX);
            let reply = Reply::Received {
                agent: view.agent,
                harness: view.harness,
                launched: view.launched,
                skills: view.skills,
                bytes: view.text.len() as u32,
            };
            (reply, Some(view.text))
        }
        Err(error) => (
            Reply::Rejected {
                code: public_code(error),
            },
            None,
        ),
    };
    let frame = encode_frame(&reply).map_err(|_| unknown())?;
    stream.write_all(&frame).await.map_err(io_error)?;
    if let Some(text) = text {
        stream.write_all(text.as_bytes()).await.map_err(io_error)?;
    }
    Ok(())
}

fn public_code(error: EngineError) -> String {
    if PROCESS_CODES.contains(&error.0.as_str()) {
        error.0
    } else {
        "process_outcome_unknown".into()
    }
}

async fn handle(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Answer, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("process_request_bounds"));
    }
    let Envelope {
        process: action,
        text_bytes,
    } = serde_json::from_slice(bytes).map_err(|_| failure("process_request_invalid"))?;
    if text_bytes.is_some() != matches!(action, Action::Save { .. }) {
        return Err(failure("process_request_invalid"));
    }
    let text = match text_bytes {
        Some(size) => Some(read_text(stream, size).await?),
        None => None,
    };
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    authority.root.verify()?;
    let root = authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("process_unavailable"))?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    tokio::task::spawn_blocking(move || match action {
        Action::Agent { project, agent } if text.is_none() => {
            agent_instructions(&root, &project, agent).map(Answer::Agent)
        }
        action => run(&root, home.as_deref(), action, text)
            .map(|text| text.map_or(Answer::Saved, Answer::Text)),
    })
    .await
    .map_err(|_| failure("process_unavailable"))?
}

/// Runs one Process request against the engine root. Inputs: the root holding
/// `engine.db`, the owner's home for an import, the action and a save's text.
/// Output: the text read, none for a save, or a plain code.
fn run(
    root: &Path,
    home: Option<&Path>,
    action: Action,
    text: Option<String>,
) -> Result<Option<String>, EngineError> {
    let code = |problem: crate::agent_home::AgentHomeError| EngineError(problem.0);
    Ok(match (action, text) {
        (Action::Read { project: None }, None) => {
            Some(crate::agent_home::read_app(root).map_err(code)?)
        }
        (
            Action::Read {
                project: Some(project),
            },
            None,
        ) => Some(crate::agent_home::read_project(&project_folder(root, &project)?).map_err(code)?),
        (Action::Save { project: None }, Some(text)) => {
            crate::agent_home::save_app(root, &text).map_err(code)?;
            None
        }
        (
            Action::Save {
                project: Some(project),
            },
            Some(text),
        ) => {
            crate::agent_home::save_project(&project_folder(root, &project)?, &text)
                .map_err(code)?;
            None
        }
        (Action::ImportGlobal, None) => {
            let home = home.ok_or_else(|| failure("process_import_missing"))?;
            Some(crate::agent_home::import_global(home).map_err(code)?)
        }
        (Action::Default, None) => Some(crate::agent_home::DEFAULT_PROCESS.to_owned()),
        _ => return Err(failure("process_request_invalid")),
    })
}

/// Reads what one team agent of a project received at its last launch from its private
/// home under `<root>/team`. Inputs: the engine root, the project and the agent id, none
/// for the project's newest master. Output: the view, or a plain code.
fn agent_instructions(
    root: &Path,
    project: &ProjectId,
    agent: Option<i64>,
) -> Result<AgentInstructions, EngineError> {
    if agent.is_some_and(|id| id <= 0) {
        return Err(failure("process_request_invalid"));
    }
    let unavailable = |_| failure("process_unavailable");
    let connection = read_only(root)?;
    let team: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'team_agents')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !team {
        return Err(failure("process_agent_unknown"));
    }
    let (id, harness): (i64, String) = connection
        .query_row(
            "SELECT id, harness FROM team_agents WHERE project = ?1
             AND (id = ?2 OR (?2 IS NULL AND role = 'master')) ORDER BY id DESC LIMIT 1",
            rusqlite::params![project.as_str(), agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(|| failure("process_agent_unknown"))?;
    assert!(id > 0 && agent.is_none_or(|asked| asked == id));
    let kind = crate::agent_home::Harness::from_id(&harness)
        .ok_or_else(|| failure("process_agent_unsupported"))?;
    let home = crate::team::runtime::agent_home(&root.join("team"), id);
    let received =
        crate::agent_home::received(&home, kind).map_err(|problem| EngineError(problem.0))?;
    let launched = received.is_some();
    let received = received.unwrap_or_default();
    Ok(AgentInstructions {
        agent: id,
        harness,
        launched,
        text: received.text,
        skills: received.skills,
    })
}

fn read_only(root: &Path) -> Result<rusqlite::Connection, EngineError> {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| failure("process_unavailable"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|_| failure("process_unavailable"))?;
    Ok(connection)
}

/// Finds one project's folder from its stored setup. Inputs: the engine root and the
/// project. Output: the folder, or `process_project_unknown`.
fn project_folder(root: &Path, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let connection = read_only(root)?;
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("process_unavailable"))?;
    crate::launch_context::agent_project(&tx, project)
        .map_err(|_| failure("process_unavailable"))?
        .folder
        .ok_or_else(|| failure("process_project_unknown"))
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
