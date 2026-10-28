use super::*;
use crate::agent_home::{PROCESS_BYTES_MAX, TEXT_BYTES_MAX};
use crate::protocol::encode_frame;
use crate::storage::versioned::{self, LastWriter, Refusal, Writer};
use rusqlite::{OptionalExtension, TransactionBehavior};
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
        version: u64,
        opened: Option<u64>,
    },
    ImportGlobal,
    Default,
    Agent {
        project: ProjectId,
        agent: Option<i64>,
    },
    AgentText {
        project: ProjectId,
        agent: i64,
    },
    SaveAgentText {
        project: ProjectId,
        agent: i64,
        version: u64,
        opened: Option<u64>,
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
    Current {
        bytes: u32,
        version: u64,
    },
    Saved {
        version: u64,
        size: Option<ProcessSaved>,
    },
    Stale {
        version: u64,
        current: u64,
        writer: LastWriter,
        bytes: u32,
    },
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

/// One instructions text and the version it was read at. A save names that version, so it
/// never replaces a text saved since without the owner seeing that text first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessText {
    pub text: String,
    pub version: u64,
}

/// What a save of instructions did: stored as the next version, or refused as stale because
/// another save landed after the version it named. A stale reply carries the newer text, its
/// version and who saved it, so the owner can reload it or keep their own text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SavedText {
    Saved {
        version: u64,
        size: Option<ProcessSaved>,
    },
    Stale {
        version: u64,
        current: ProcessText,
        writer: LastWriter,
    },
}

impl SavedText {
    /// The short sentence to show after a save a Claude master may refuse, as
    /// [`ProcessSaved::claude_warning`] gives it for an app or project save. Input: the
    /// save's result. Output: the sentence, or none for a stale reply, an agent's own text
    /// or a save a Claude master takes.
    pub fn claude_warning(&self) -> Option<String> {
        match self {
            SavedText::Saved {
                size: Some(size), ..
            } => size.claude_warning(),
            _ => None,
        }
    }
}

/// What a saved instructions text comes to: the bytes of app and project instructions the
/// project's agents now receive (the largest across set-up projects after an app save) and
/// the most a Claude master is sure to take beside the largest project context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSaved {
    pub bytes: u32,
    pub claude_bytes_max: u32,
}

impl ProcessSaved {
    /// The short sentence to show after a save a Claude master may refuse, naming what it
    /// takes and what agents now get; it fits an 80-column status line after "Instructions
    /// saved.". Input: the save. Output: the sentence, or none when a Claude master takes it.
    pub fn claude_warning(&self) -> Option<String> {
        (self.bytes > self.claude_bytes_max).then(|| {
            format!(
                "Shorten: a Claude master takes {}, not {}.",
                kilobytes(self.claude_bytes_max),
                kilobytes(self.bytes)
            )
        })
    }
}

fn kilobytes(bytes: u32) -> String {
    format!("{:.1} KB", f64::from(bytes) / 1024.0)
}

#[derive(Debug)]
enum Answer {
    Text(String),
    Current(ProcessText),
    Saved(SavedText),
    Agent(AgentInstructions),
}

fn unknown() -> EngineError {
    failure("process_outcome_unknown")
}

impl OwnerClient {
    /// Reads the owner's instructions: the app-wide text, or one project's text.
    /// Input: none for the app, or the project. Output: the text, empty when none is saved,
    /// and the version a save names.
    pub async fn process_text(
        &self,
        project: Option<ProjectId>,
    ) -> Result<ProcessText, EngineError> {
        match self
            .process_exchange(Action::Read { project }, None)
            .await?
        {
            (Reply::Current { version, .. }, Some(text)) => Ok(ProcessText { text, version }),
            _ => Err(unknown()),
        }
    }

    /// Saves the owner's instructions whole: the app-wide text, or one project's text.
    /// Inputs: none for the app, or the project; the text, at most
    /// `agent_home::TEXT_BYTES_MAX` bytes; the version it was read at; and only for "Keep
    /// mine" after a stale reply, the version the owner opened, older than `version`.
    /// Output: the saved version with what agents now receive beside what a Claude master
    /// is sure to take, the stale reply with the newer text, or a refusal.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        crate::agent_home::validate(&text).map_err(|problem| EngineError(problem.0))?;
        let action = Action::Save {
            project,
            version,
            opened,
        };
        self.save_reply(action, text).await
    }

    async fn save_reply(&self, action: Action, text: String) -> Result<SavedText, EngineError> {
        match self.process_exchange(action, Some(text)).await? {
            (Reply::Saved { version, size }, None)
                if size.is_none_or(|size| size.bytes as usize <= PROCESS_BYTES_MAX) =>
            {
                Ok(SavedText::Saved { version, size })
            }
            (
                Reply::Stale {
                    version,
                    current,
                    writer,
                    ..
                },
                Some(text),
            ) => Ok(SavedText::Stale {
                version,
                current: ProcessText {
                    text,
                    version: current,
                },
                writer,
            }),
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

    /// Reads the owner's own instructions for one team agent, which its next launch gets
    /// after the app and project text. Inputs: the project and the agent id. Output: the
    /// text, empty when none is saved, and the version a save names.
    pub async fn agent_text(
        &self,
        project: ProjectId,
        agent: i64,
    ) -> Result<ProcessText, EngineError> {
        match self
            .process_exchange(Action::AgentText { project, agent }, None)
            .await?
        {
            (Reply::Current { version, .. }, Some(text)) => Ok(ProcessText { text, version }),
            _ => Err(unknown()),
        }
    }

    /// Saves the owner's own instructions for one team agent whole when no other save landed
    /// since they were read. Inputs: the project, the agent id, the text, the version read
    /// and, only for "Keep mine" after a stale reply, the older version the owner opened.
    /// Output: the saved version, the stale reply with the newer text, or a refusal.
    pub async fn save_agent_text(
        &self,
        project: ProjectId,
        agent: i64,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        crate::agent_home::validate(&text).map_err(|problem| EngineError(problem.0))?;
        let action = Action::SaveAgentText {
            project,
            agent,
            version,
            opened,
        };
        self.save_reply(action, text).await
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
                Reply::Current { bytes, version } => {
                    let text = read_text(&mut stream, bytes).await.map_err(|_| unknown())?;
                    Ok((Reply::Current { bytes, version }, Some(text)))
                }
                Reply::Stale {
                    version,
                    current,
                    writer,
                    bytes,
                } => {
                    let text = read_text(&mut stream, bytes).await.map_err(|_| unknown())?;
                    let reply = Reply::Stale {
                        version,
                        current,
                        writer,
                        bytes,
                    };
                    Ok((reply, Some(text)))
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
        Ok(Answer::Current(current)) => {
            assert!(current.text.len() <= TEXT_BYTES_MAX);
            let reply = Reply::Current {
                bytes: current.text.len() as u32,
                version: current.version,
            };
            (reply, Some(current.text))
        }
        Ok(Answer::Saved(SavedText::Saved { version, size })) => {
            (Reply::Saved { version, size }, None)
        }
        Ok(Answer::Saved(SavedText::Stale {
            version,
            current,
            writer,
        })) => {
            assert!(current.text.len() <= TEXT_BYTES_MAX);
            let reply = Reply::Stale {
                version,
                current: current.version,
                writer,
                bytes: current.text.len() as u32,
            };
            (reply, Some(current.text))
        }
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
    if text_bytes.is_some() != matches!(action, Action::Save { .. } | Action::SaveAgentText { .. })
    {
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
        action => run(&root, home.as_deref(), action, text),
    })
    .await
    .map_err(|_| failure("process_unavailable"))?
}

/// Runs one Process request against the engine root. Inputs: the root holding
/// `engine.db`, the owner's home for an import, the action and a save's text.
/// Output: the text read with its version, a seed text, what a save did, or a plain code.
fn run(
    root: &Path,
    home: Option<&Path>,
    action: Action,
    text: Option<String>,
) -> Result<Answer, EngineError> {
    Ok(match (action, text) {
        (Action::Read { project }, None) => Answer::Current(current_text(root, project.as_ref())?),
        (
            Action::Save {
                project,
                version,
                opened,
            },
            Some(text),
        ) => Answer::Saved(save_text(root, project.as_ref(), &text, version, opened)?),
        (Action::ImportGlobal, None) => {
            let home = home.ok_or_else(|| failure("process_import_missing"))?;
            Answer::Text(
                crate::agent_home::import_global(home).map_err(|problem| EngineError(problem.0))?,
            )
        }
        (Action::Default, None) => Answer::Text(crate::agent_home::DEFAULT_PROCESS.to_owned()),
        (Action::AgentText { project, agent }, None) => {
            Answer::Current(current_agent_text(root, &project, agent)?)
        }
        (
            Action::SaveAgentText {
                project,
                agent,
                version,
                opened,
            },
            Some(text),
        ) => Answer::Saved(save_agent_text(
            root, &project, agent, &text, version, opened,
        )?),
        _ => return Err(failure("process_request_invalid")),
    })
}

/// Tells whether a team agent belongs to a project. Inputs: a connection to `engine.db`,
/// the project and the agent id, above 0. Output: true for an agent of that project.
fn team_member(
    connection: &rusqlite::Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    assert!(agent > 0);
    let unavailable = |_| failure("process_unavailable");
    let team: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'team_agents')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !team {
        return Ok(false);
    }
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id = ?1 AND project = ?2)",
            rusqlite::params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)
}

/// Reads the owner's own instructions for one team agent of a project with their version.
/// Inputs: the engine root, the project and the agent id. Output: the text and version,
/// `process_agent_unknown` for another project's agent, or another plain code.
fn current_agent_text(
    root: &Path,
    project: &ProjectId,
    agent: i64,
) -> Result<ProcessText, EngineError> {
    if agent <= 0 {
        return Err(failure("process_request_invalid"));
    }
    let connection = read_only(root)?;
    if !team_member(&connection, project, agent)? {
        return Err(failure("process_agent_unknown"));
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("process_unavailable"))?;
    let (text, version) = crate::storage::agent_text::read(&tx, project, agent)
        .map_err(|_| failure("process_unavailable"))?;
    Ok(ProcessText { text, version })
}

/// Saves the owner's own instructions for one team agent as their next version only when
/// the save names the current one; the text and its version commit together, so racing
/// saves queue and every loser gets the stale reply with the text that landed. "Keep mine"
/// (`opened`) must name an older version than `version` and is recorded. Inputs: the engine
/// root, the project, the agent id, the text, the version read and the version first
/// opened. Output: what the save did, or a plain code.
fn save_agent_text(
    root: &Path,
    project: &ProjectId,
    agent: i64,
    text: &str,
    version: u64,
    opened: Option<u64>,
) -> Result<SavedText, EngineError> {
    crate::agent_home::validate(text).map_err(|problem| EngineError(problem.0))?;
    if agent <= 0 || opened.is_some_and(|opened| opened >= version) {
        return Err(failure("process_request_invalid"));
    }
    let mut connection = versioned::open(root).map_err(|_| failure("process_unavailable"))?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| failure("process_unavailable"))?;
    if !team_member(&tx, project, agent)? {
        return Err(failure("process_agent_unknown"));
    }
    let writer = Writer::Owner {
        force_after_conflict: opened.is_some(),
    };
    match crate::storage::agent_text::save(&tx, project, agent, text, version, &writer) {
        Ok(next) => {
            tx.commit().map_err(|_| unknown())?;
            assert_eq!(next, version + 1);
            Ok(SavedText::Saved {
                version: next,
                size: None,
            })
        }
        Err(Refusal::Stale(stale)) => {
            let (text, current) = crate::storage::agent_text::read(&tx, project, agent)
                .map_err(|_| failure("process_unavailable"))?;
            assert_eq!(current, stale.current);
            Ok(SavedText::Stale {
                version: stale.version,
                current: ProcessText {
                    text,
                    version: current,
                },
                writer: stale.writer,
            })
        }
        Err(Refusal::Failed(_)) => Err(failure("process_unavailable")),
    }
}

/// Reads the instructions' version first and their text second, so a text saved between
/// the two reads is shown with the older version and its next save is refused instead of
/// replacing a text the owner never saw. Inputs: the engine root and the project, none for
/// the app. Output: the text and its version, or a plain code.
fn current_text(root: &Path, project: Option<&ProjectId>) -> Result<ProcessText, EngineError> {
    let folder = project
        .map(|project| project_folder(root, project))
        .transpose()?;
    let item = versioned::process_text_item(project);
    let connection = read_only(root)?;
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("process_unavailable"))?;
    let version = versioned::current(&tx, &item)
        .map_err(|_| failure("process_unavailable"))?
        .version;
    drop(tx);
    let text = stored_text(root, folder.as_deref())?;
    Ok(ProcessText { text, version })
}

/// Saves the instructions as their next version only when the save names the current one.
/// The version moves in an immediate transaction, the file is written while that write lock
/// is held, and the version commits last, so a failed file write keeps the old version and
/// racing saves queue: one lands, the others get the stale reply with the text that landed.
/// "Keep mine" (`opened`) must name an older version than `version`, the one the conflict
/// showed, and is recorded. Inputs: the engine root, the project (none for the app), the
/// text, the version read and the version first opened. Output: what the save did.
fn save_text(
    root: &Path,
    project: Option<&ProjectId>,
    text: &str,
    version: u64,
    opened: Option<u64>,
) -> Result<SavedText, EngineError> {
    crate::agent_home::validate(text).map_err(|problem| EngineError(problem.0))?;
    if opened.is_some_and(|opened| opened >= version) {
        return Err(failure("process_request_invalid"));
    }
    let folder = project
        .map(|project| project_folder(root, project))
        .transpose()?;
    let item = versioned::process_text_item(project);
    let mut connection = versioned::open(root).map_err(|_| failure("process_unavailable"))?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| failure("process_unavailable"))?;
    let writer = Writer::Owner {
        force_after_conflict: opened.is_some(),
    };
    match versioned::write_if(&tx, &item, version, &writer) {
        Ok(next) => {
            store_text(root, folder.as_deref(), text)?;
            tx.commit().map_err(|_| unknown())?;
            assert_eq!(next, version + 1);
            Ok(SavedText::Saved {
                version: next,
                size: Some(sized(root, project, text)),
            })
        }
        Err(Refusal::Stale(stale)) => {
            assert_ne!(stale.current, version);
            let text = stored_text(root, folder.as_deref())?;
            Ok(SavedText::Stale {
                version: stale.version,
                current: ProcessText {
                    text,
                    version: stale.current,
                },
                writer: stale.writer,
            })
        }
        Err(Refusal::Failed(_)) => Err(failure("process_unavailable")),
    }
}

/// Reads the stored instructions. Inputs: the engine root and the project folder, none for
/// the app. Output: the text, empty when none is saved, or a plain code.
fn stored_text(root: &Path, folder: Option<&Path>) -> Result<String, EngineError> {
    match folder {
        None => crate::agent_home::read_app(root),
        Some(folder) => crate::agent_home::read_project(folder),
    }
    .map_err(|problem| EngineError(problem.0))
}

/// Writes the instructions whole. Inputs: the engine root, the project folder (none for
/// the app) and the text. Output: unit, or a plain code.
fn store_text(root: &Path, folder: Option<&Path>, text: &str) -> Result<(), EngineError> {
    match folder {
        None => crate::agent_home::save_app(root, text),
        Some(folder) => crate::agent_home::save_project(folder, text),
    }
    .map_err(|problem| EngineError(problem.0))
}

/// The most set-up projects an app-wide save reads for its Claude warning, as many as Home
/// lists.
const SAVED_PROJECTS_MAX: i64 = 200;

const _: () = assert!(
    PROCESS_BYTES_MAX <= u32::MAX as usize
        && super::master_host_launch::CLAUDE_PROCESS_BYTES_MAX <= u32::MAX as usize
);

/// Measures what the owner's instructions come to after a save that landed, from the text
/// just saved: the app and project text the project's agents now receive, the largest
/// across set-up projects after an app save, beside what a Claude master is sure to take.
/// A text that cannot be read counts as empty, so a landed save never reports a failure.
/// Inputs: the engine root, the project, none for the app, and the saved text. Output: the
/// sizes.
fn sized(root: &Path, project: Option<&ProjectId>, saved: &str) -> ProcessSaved {
    let given = match project {
        Some(_) => {
            let app = crate::agent_home::read_app(root).unwrap_or_default();
            crate::agent_home::render(&app, saved).len()
        }
        None => project_texts(root)
            .iter()
            .map(|own| crate::agent_home::render(saved, own).len())
            .fold(crate::agent_home::render(saved, "").len(), usize::max),
    };
    assert!(given <= PROCESS_BYTES_MAX);
    ProcessSaved {
        bytes: given as u32,
        claude_bytes_max: super::master_host_launch::CLAUDE_PROCESS_BYTES_MAX as u32,
    }
}

/// Reads the instructions of the set-up projects, at most `SAVED_PROJECTS_MAX`, so an app
/// save can tell whether it pushes one past what a Claude master takes. A project whose
/// folder or text cannot be read is left out. Input: the engine root. Output: the texts.
fn project_texts(root: &Path) -> Vec<String> {
    let Ok(connection) = read_only(root) else {
        return Vec::new();
    };
    let Ok(tx) = connection.unchecked_transaction() else {
        return Vec::new();
    };
    let ids: Vec<String> =
        match tx.prepare("SELECT project FROM project_setups ORDER BY project LIMIT ?1") {
            Ok(mut statement) => statement
                .query_map([SAVED_PROJECTS_MAX], |row| row.get(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
    assert!(ids.len() <= SAVED_PROJECTS_MAX as usize);
    ids.iter()
        .filter_map(|id| {
            let project = ProjectId::new(id).ok()?;
            let folder = crate::launch_context::agent_project(&tx, &project)
                .ok()?
                .folder?;
            crate::agent_home::read_project(&folder).ok()
        })
        .collect()
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
