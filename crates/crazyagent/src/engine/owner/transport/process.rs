use super::*;
use crate::agent_home::{PROCESS_BYTES_MAX, TEXT_BYTES_MAX};
pub use crate::instruction_files::{Record as InstructionRecord, Settings as InstructionSettings};
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
    "instruction_settings_invalid",
    "instruction_settings_unavailable",
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
    Settings {
        project: ProjectId,
    },
    SaveSettings {
        project: ProjectId,
        settings: InstructionSettings,
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
    Saved {
        bytes: u32,
        claude_bytes_max: u32,
    },
    Received {
        agent: i64,
        harness: String,
        launched: bool,
        skills: Vec<String>,
        files: Vec<InstructionRecord>,
        bytes: u32,
    },
    Settings {
        settings: InstructionSettings,
    },
    Rejected {
        code: String,
    },
}

/// What one team agent received at its last launch, read back from its private home: the
/// agent id, its harness, whether it has launched, the text of its harness's instruction
/// file (empty when none was given), the names of the skills installed for it and the
/// engine's record of the instruction files it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInstructions {
    pub agent: i64,
    pub harness: String,
    pub launched: bool,
    pub text: String,
    pub skills: Vec<String>,
    pub files: Vec<InstructionRecord>,
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

enum Answer {
    Text(String),
    Saved(ProcessSaved),
    Agent(AgentInstructions),
    Settings(InstructionSettings),
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
    /// `agent_home::TEXT_BYTES_MAX` bytes. Output: what agents now receive beside what a
    /// Claude master is sure to take, or a `PROCESS_CODES` refusal.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
    ) -> Result<ProcessSaved, EngineError> {
        crate::agent_home::validate(&text).map_err(|problem| EngineError(problem.0))?;
        match self
            .process_exchange(Action::Save { project }, Some(text))
            .await?
        {
            (
                Reply::Saved {
                    bytes,
                    claude_bytes_max,
                },
                None,
            ) if bytes as usize <= PROCESS_BYTES_MAX => Ok(ProcessSaved {
                bytes,
                claude_bytes_max,
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
                    files,
                    ..
                },
                Some(text),
            ) if files.len() <= crate::instruction_files::FILES_MAX => Ok(AgentInstructions {
                agent,
                harness,
                launched,
                text,
                skills,
                files,
            }),
            _ => Err(unknown()),
        }
    }

    /// Reads one project's agent instruction settings: which instruction files its agents
    /// get and whether headless Claude runs keep their transcript. Input: the project.
    /// Output: the settings, the defaults when the owner never changed them.
    pub async fn instruction_settings(
        &self,
        project: ProjectId,
    ) -> Result<InstructionSettings, EngineError> {
        self.process_settings_reply(Action::Settings { project })
            .await
    }

    /// Stores one project's agent instruction settings whole; agents launched after this
    /// follow them. Inputs: the project and the settings. Output: the settings as stored.
    pub async fn save_instruction_settings(
        &self,
        project: ProjectId,
        settings: InstructionSettings,
    ) -> Result<InstructionSettings, EngineError> {
        self.process_settings_reply(Action::SaveSettings { project, settings })
            .await
    }

    async fn process_settings_reply(
        &self,
        action: Action,
    ) -> Result<InstructionSettings, EngineError> {
        match self.process_exchange(action, None).await? {
            (Reply::Settings { settings }, None) => Ok(settings),
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
                    files,
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
                        files,
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
        Ok(Answer::Saved(saved)) => (
            Reply::Saved {
                bytes: saved.bytes,
                claude_bytes_max: saved.claude_bytes_max,
            },
            None,
        ),
        Ok(Answer::Agent(view)) => {
            assert!(view.text.len() <= PROCESS_BYTES_MAX);
            let reply = Reply::Received {
                agent: view.agent,
                harness: view.harness,
                launched: view.launched,
                skills: view.skills,
                files: view.files,
                bytes: view.text.len() as u32,
            };
            (reply, Some(view.text))
        }
        Ok(Answer::Settings(settings)) => (Reply::Settings { settings }, None),
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
        Action::Settings { project } if text.is_none() => {
            instruction_settings(&root, &project, None).map(Answer::Settings)
        }
        Action::SaveSettings { project, settings } if text.is_none() => {
            instruction_settings(&root, &project, Some(settings)).map(Answer::Settings)
        }
        action => {
            let saved = match (&action, &text) {
                (Action::Save { project }, Some(text)) => Some((project.clone(), text.clone())),
                _ => None,
            };
            match (saved, run(&root, home.as_deref(), action, text)?) {
                (Some((project, text)), None) => {
                    Ok(Answer::Saved(sized(&root, project.as_ref(), &text)))
                }
                (None, Some(text)) => Ok(Answer::Text(text)),
                _ => Err(unknown()),
            }
        }
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
        files: received.files,
    })
}

/// Reads one set-up project's agent instruction settings, or stores new ones whole first.
/// The setup check, the save and the read back run in one transaction, taken for writing
/// up front on a save, so no other save or project removal lands between them.
/// Inputs: the engine root, the project and the settings to store, none to only read.
/// Output: the settings as stored, `process_project_unknown` for a project with no setup,
/// or a plain code.
fn instruction_settings(
    root: &Path,
    project: &ProjectId,
    save: Option<InstructionSettings>,
) -> Result<InstructionSettings, EngineError> {
    let unavailable = |_| failure("instruction_settings_unavailable");
    let mut connection = match save {
        None => read_only(root)?,
        Some(_) => {
            let connection = rusqlite::Connection::open_with_flags(
                root.join("engine.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(unavailable)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(unavailable)?;
            connection
        }
    };
    let behavior = match save {
        None => rusqlite::TransactionBehavior::Deferred,
        Some(_) => rusqlite::TransactionBehavior::Immediate,
    };
    let tx = connection
        .transaction_with_behavior(behavior)
        .map_err(unavailable)?;
    let known: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM project_setups WHERE project=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !known {
        return Err(failure("process_project_unknown"));
    }
    if let Some(settings) = save {
        crate::instruction_files::save(&tx, project, settings).map_err(unavailable)?;
    }
    let stored = crate::instruction_files::load(&tx, project).map_err(failure)?;
    assert!(save.is_none_or(|saved| saved == stored));
    tx.commit().map_err(unavailable)?;
    Ok(stored)
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
