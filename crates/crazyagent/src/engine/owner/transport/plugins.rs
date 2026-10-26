use super::*;
use crate::engine::owner::AccountSecret;
use crate::plugins::{EffectivePlugins, Scope, ServerSpec, ServerView, SkillView, check};
use crate::protocol::encode_frame;
use crate::storage::plugins::{Command, ResultValue};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

const SECRET_MAX: usize = 8192;
const CHECKS_MAX: usize = 4;
const CHECK_POLL: Duration = Duration::from_millis(200);
const CHECK_FOLDER: &str = "mcp-checks";
static CHECKS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Skills {
        project: Option<ProjectId>,
        offset: u16,
    },
    Servers {
        project: Option<ProjectId>,
        offset: u16,
    },
    Server {
        id: String,
    },
    AddSkill {
        scope: Scope,
        folder: String,
    },
    SetSkill {
        id: String,
        enabled: bool,
    },
    RemoveSkill {
        id: String,
    },
    AddServer {
        scope: Scope,
        name: String,
        spec: ServerSpec,
    },
    UpdateServer {
        id: String,
        spec: ServerSpec,
    },
    Approve {
        id: String,
        spec: ServerSpec,
    },
    SetServer {
        id: String,
        enabled: bool,
    },
    RemoveServer {
        id: String,
    },
    Switch {
        project: ProjectId,
        id: String,
        on: bool,
    },
    SetSecret {
        id: String,
        name: String,
    },
    RemoveSecret {
        id: String,
        name: String,
    },
    Check {
        id: String,
    },
    Effective {
        project: ProjectId,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    plugins: Action,
    secret_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPage {
    pub skills: Vec<SkillView>,
    pub next: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPage {
    pub servers: Vec<ServerView>,
    pub next: Option<u16>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Skills { page: SkillPage },
    Servers { page: ServerPage },
    Skill { skill: SkillView },
    Server { server: ServerView },
    Done,
    Effective { plugins: EffectivePlugins },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("plugins_outcome_unknown")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_folder(folder: &str) -> bool {
    folder.starts_with('/')
        && folder.len() <= crate::plugins::skill_folder::SOURCE_BYTES_MAX
        && !folder.chars().any(char::is_control)
}

impl Action {
    fn validate(&self) -> Result<(), EngineError> {
        let id = match self {
            Self::Server { id }
            | Self::SetSkill { id, .. }
            | Self::RemoveSkill { id }
            | Self::UpdateServer { id, .. }
            | Self::Approve { id, .. }
            | Self::SetServer { id, .. }
            | Self::RemoveServer { id }
            | Self::Switch { id, .. }
            | Self::SetSecret { id, .. }
            | Self::RemoveSecret { id, .. }
            | Self::Check { id } => Some(id),
            _ => None,
        };
        if id.is_some_and(|id| !valid_id(id)) {
            return Err(failure("plugin_not_found"));
        }
        match self {
            Self::AddSkill { folder, .. } if !valid_folder(folder) => {
                Err(failure("skill_folder_not_found"))
            }
            Self::AddServer { name, spec, .. } => {
                crate::plugins::validate_server_name(name)?;
                spec.validate()
            }
            Self::UpdateServer { spec, .. } | Self::Approve { spec, .. } => spec.validate(),
            Self::SetSecret { name, .. } | Self::RemoveSecret { name, .. } => {
                crate::plugins::validate_secret_name(name)
            }
            _ => Ok(()),
        }
    }
}

impl OwnerClient {
    /// Reads one page of skills. Inputs: a project to include its own skills and its
    /// switches for global ones (or none for global settings) and the page offset.
    /// Output: skills with name, description, scope and On flags, and the next offset.
    pub async fn plugin_skills(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<SkillPage, EngineError> {
        match self
            .plugins_exchange(Action::Skills { project, offset }, None, self.timeout)
            .await?
        {
            Reply::Skills { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one page of MCP servers, with the same inputs as `plugin_skills`.
    /// Output: servers with how they run, plain status, secrets as saved names and tools.
    pub async fn plugin_servers(
        &self,
        project: Option<ProjectId>,
        offset: u16,
    ) -> Result<ServerPage, EngineError> {
        match self
            .plugins_exchange(Action::Servers { project, offset }, None, self.timeout)
            .await?
        {
            Reply::Servers { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Reads one MCP server by id. Output: the server as the lists show it.
    pub async fn plugin_server(&self, id: String) -> Result<ServerView, EngineError> {
        self.server_reply(Action::Server { id }, None).await
    }

    /// Adds a skill by copying an owner-picked folder that holds SKILL.md into the engine's
    /// store. Inputs: global or project scope and the absolute folder path. Output: the skill.
    pub async fn add_skill(&self, scope: Scope, folder: String) -> Result<SkillView, EngineError> {
        self.skill_reply(Action::AddSkill { scope, folder }).await
    }

    /// Turns a skill on or off where it was added. Inputs: skill id and the new state.
    pub async fn set_skill_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<SkillView, EngineError> {
        self.skill_reply(Action::SetSkill { id, enabled }).await
    }

    /// Removes a skill and its stored folder copy. Input: the skill id.
    pub async fn remove_skill(&self, id: String) -> Result<(), EngineError> {
        self.done_reply(Action::RemoveSkill { id }).await
    }

    /// Adds an MCP server that waits for approval. Inputs: scope, a plain name and the
    /// command or URL. Output: the server with status "Waiting for approval".
    pub async fn add_mcp_server(
        &self,
        scope: Scope,
        name: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::AddServer { scope, name, spec }, None)
            .await
    }

    /// Changes how an MCP server runs. Any change sends it back to "Waiting for approval".
    pub async fn update_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::UpdateServer { id, spec }, None)
            .await
    }

    /// Approves an MCP server exactly as the owner reviewed it. Inputs: id and the spec the
    /// owner saw; a server that changed since is refused. Output: the approved server.
    pub async fn approve_mcp_server(
        &self,
        id: String,
        spec: ServerSpec,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::Approve { id, spec }, None).await
    }

    /// Turns an MCP server on or off where it was added. Output: the server.
    pub async fn set_mcp_server_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::SetServer { id, enabled }, None)
            .await
    }

    /// Removes an MCP server and deletes its saved secrets. Input: the server id.
    pub async fn remove_mcp_server(&self, id: String) -> Result<(), EngineError> {
        self.done_reply(Action::RemoveServer { id }).await
    }

    /// Switches a global skill or MCP server on or off for one project.
    /// Inputs: the project, the global item id and the new state.
    pub async fn switch_plugin(
        &self,
        project: ProjectId,
        id: String,
        on: bool,
    ) -> Result<(), EngineError> {
        self.done_reply(Action::Switch { project, id, on }).await
    }

    /// Saves one secret for an MCP server, sent only as a separate frame and kept by the
    /// engine in a private file. Inputs: server id, the secret's name and its value.
    pub async fn set_mcp_secret(
        &self,
        id: String,
        name: String,
        value: AccountSecret,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::SetSecret { id, name }, Some(value))
            .await
    }

    /// Deletes one secret from an MCP server. Inputs: server id and the secret's name.
    pub async fn remove_mcp_secret(
        &self,
        id: String,
        name: String,
    ) -> Result<ServerView, EngineError> {
        self.server_reply(Action::RemoveSecret { id, name }, None)
            .await
    }

    /// Checks a command MCP server: the engine starts it, lists its tools and stops it.
    /// Input: the server id. Output: the server after the check, with tools or the plain
    /// reason it can't start.
    pub async fn check_mcp_server(&self, id: String) -> Result<ServerView, EngineError> {
        let before = self.plugin_server(id.clone()).await?.checks;
        self.server_reply(Action::Check { id: id.clone() }, None)
            .await?;
        let deadline = tokio::time::Instant::now() + check::CHECK_TIME + Duration::from_secs(3);
        loop {
            let server = self.plugin_server(id.clone()).await?;
            if server.checks > before {
                return Ok(server);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(failure("server_check_timeout"));
            }
            tokio::time::sleep(CHECK_POLL).await;
        }
    }

    /// Reads exactly what agents launched for a project receive. Input: the project.
    /// Output: skill folders and approved MCP servers with secret references.
    pub async fn project_plugins(
        &self,
        project: ProjectId,
    ) -> Result<EffectivePlugins, EngineError> {
        match self
            .plugins_exchange(Action::Effective { project }, None, self.timeout)
            .await?
        {
            Reply::Effective { plugins } => Ok(plugins),
            _ => Err(unknown()),
        }
    }

    async fn skill_reply(&self, action: Action) -> Result<SkillView, EngineError> {
        match self.plugins_exchange(action, None, self.timeout).await? {
            Reply::Skill { skill } => Ok(skill),
            _ => Err(unknown()),
        }
    }

    async fn server_reply(
        &self,
        action: Action,
        secret: Option<AccountSecret>,
    ) -> Result<ServerView, EngineError> {
        match self.plugins_exchange(action, secret, self.timeout).await? {
            Reply::Server { server } => Ok(server),
            _ => Err(unknown()),
        }
    }

    async fn done_reply(&self, action: Action) -> Result<(), EngineError> {
        match self.plugins_exchange(action, None, self.timeout).await? {
            Reply::Done => Ok(()),
            _ => Err(unknown()),
        }
    }

    async fn plugins_exchange(
        &self,
        action: Action,
        secret: Option<AccountSecret>,
        timeout: Duration,
    ) -> Result<Reply, EngineError> {
        action.validate()?;
        tokio::time::timeout(timeout, async {
            let envelope = Envelope {
                plugins: action,
                secret_present: secret.is_some(),
            };
            let frame = encode_frame(&envelope).map_err(|_| failure("plugins_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("plugins_owner_unavailable"))?;
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
                .map_err(|_| failure("plugins_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("plugins_owner_unavailable"))?;
            if let Some(secret) = secret {
                let mut bytes = secret.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("plugins_owner_unavailable"))?;
            }
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

impl Engine {
    /// Reads the skills and MCP servers agents launched for one project receive, for the
    /// harness launchers inside the engine. Input: the project. Output: skill folder paths
    /// and approved MCP servers; read each secret with `storage::plugins::read_secret`.
    pub(crate) async fn project_plugins(
        &self,
        project: &ProjectId,
    ) -> Result<EffectivePlugins, EngineError> {
        match actor(
            self,
            Command::Effective {
                project: project.clone(),
            },
        )
        .await?
        {
            ResultValue::Effective(plugins) => Ok(*plugins),
            _ => Err(unknown()),
        }
    }
}

impl Engine {
    /// Reads what agents launched for one project receive, with every MCP server secret
    /// value already read from its private file, for the harness launchers inside the
    /// engine. Input: the project. Output: skill folders and approved servers whose secret
    /// values are zeroed when dropped and redacted in `Debug`.
    pub async fn project_launch_plugins(
        &self,
        project: &ProjectId,
    ) -> Result<crate::plugins::LaunchPlugins, EngineError> {
        match actor(
            self,
            Command::Launch {
                project: project.clone(),
            },
        )
        .await?
        {
            ResultValue::Launch(plugins) => Ok(*plugins),
            _ => Err(unknown()),
        }
    }
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
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("plugins_request_invalid"))?;
    let secret = if envelope.secret_present {
        Some(receive_secret(stream).await?)
    } else {
        None
    };
    let reply = match handle(
        engine,
        authority,
        packet,
        admission,
        envelope.plugins,
        secret,
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "plugins_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("plugin_store_unavailable"))
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    action: Action,
    secret: Option<AccountSecret>,
) -> Result<Reply, EngineError> {
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    action.validate()?;
    if secret.is_some() != matches!(action, Action::SetSecret { .. }) {
        return Err(failure("server_secret_invalid"));
    }
    Ok(match action {
        Action::Skills { project, offset } => {
            let ResultValue::Skills(skills, next) =
                actor(engine, Command::Skills { project, offset }).await?
            else {
                return Err(unknown());
            };
            Reply::Skills {
                page: SkillPage { skills, next },
            }
        }
        Action::Servers { project, offset } => {
            let ResultValue::Servers(servers, next) =
                actor(engine, Command::Servers { project, offset }).await?
            else {
                return Err(unknown());
            };
            Reply::Servers {
                page: ServerPage { servers, next },
            }
        }
        Action::Server { id } => server(actor(engine, Command::Server { id }).await?)?,
        Action::AddSkill { scope, folder } => {
            let ResultValue::Skill(skill) = actor(
                engine,
                Command::AddSkill {
                    scope,
                    source: folder,
                },
            )
            .await?
            else {
                return Err(unknown());
            };
            Reply::Skill { skill: skill.view }
        }
        Action::SetSkill { id, enabled } => {
            let ResultValue::Skill(skill) =
                actor(engine, Command::SetSkill { id, enabled }).await?
            else {
                return Err(unknown());
            };
            Reply::Skill { skill: skill.view }
        }
        Action::RemoveSkill { id } => {
            actor(engine, Command::RemoveSkill { id }).await?;
            Reply::Done
        }
        Action::AddServer { scope, name, spec } => {
            server(actor(engine, Command::AddServer { scope, name, spec }).await?)?
        }
        Action::UpdateServer { id, spec } => {
            server(actor(engine, Command::UpdateServer { id, spec }).await?)?
        }
        Action::Approve { id, spec } => {
            server(actor(engine, Command::Approve { id, spec }).await?)?
        }
        Action::SetServer { id, enabled } => {
            server(actor(engine, Command::SetServer { id, enabled }).await?)?
        }
        Action::RemoveServer { id } => {
            let ResultValue::Removed(references) =
                actor(engine, Command::RemoveServer { id: id.clone() }).await?
            else {
                return Err(unknown());
            };
            let root = engine_root(authority)?;
            for reference in references {
                crate::storage::plugins::remove_secret(&root, &reference)?;
            }
            let checked = root.join(CHECK_FOLDER).join(&id);
            let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(checked))
                .await
                .map_err(|_| unknown())?;
            if let Err(problem) = removed
                && problem.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("plugin_check_folder_not_removed cause={problem}");
            }
            Reply::Done
        }
        Action::Switch { project, id, on } => {
            actor(engine, Command::Switch { project, id, on }).await?;
            Reply::Done
        }
        Action::SetSecret { id, name } => {
            let secret = secret.ok_or_else(|| failure("server_secret_invalid"))?;
            let root = engine_root(authority)?;
            let reference = crate::storage::plugins::new_secret_reference()?;
            crate::storage::plugins::store_secret(&root, &reference, secret.into_bytes())?;
            let stored = reference.clone();
            let attached = engine
                .settled_job(
                    |reply| {
                        Job::Plugins(
                            Command::AttachSecret {
                                id,
                                name,
                                reference,
                            },
                            reply,
                        )
                    },
                    move |result| match result {
                        Ok(ResultValue::Secret(_, Some(old))) => {
                            crate::storage::plugins::remove_secret(&root, old)
                        }
                        Ok(_) => Ok(()),
                        Err(_) => {
                            let _ = crate::storage::plugins::remove_secret(&root, &stored);
                            Ok(())
                        }
                    },
                )
                .await?;
            let ResultValue::Secret(view, _) = attached else {
                return Err(unknown());
            };
            Reply::Server { server: *view }
        }
        Action::RemoveSecret { id, name } => {
            let ResultValue::Secret(view, removed) =
                actor(engine, Command::DetachSecret { id, name }).await?
            else {
                return Err(unknown());
            };
            if let Some(reference) = removed {
                crate::storage::plugins::remove_secret(&engine_root(authority)?, &reference)?;
            }
            Reply::Server { server: *view }
        }
        Action::Check { id } => start_check(engine, authority, id).await?,
        Action::Effective { project } => Reply::Effective {
            plugins: engine.project_plugins(&project).await?,
        },
    })
}

fn server(value: ResultValue) -> Result<Reply, EngineError> {
    match value {
        ResultValue::Server(server) => Ok(Reply::Server { server: *server }),
        _ => Err(unknown()),
    }
}

struct CheckSlot;

impl CheckSlot {
    fn take() -> Result<Self, EngineError> {
        if CHECKS.fetch_add(1, Ordering::SeqCst) >= CHECKS_MAX {
            CHECKS.fetch_sub(1, Ordering::SeqCst);
            return Err(failure("server_check_busy"));
        }
        Ok(Self)
    }
}

impl Drop for CheckSlot {
    fn drop(&mut self) {
        let before = CHECKS.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0);
    }
}

async fn start_check(
    engine: &Arc<Engine>,
    authority: &Authority,
    id: String,
) -> Result<Reply, EngineError> {
    let ResultValue::Check(spec, secrets) =
        actor(engine, Command::GetServer { id: id.clone() }).await?
    else {
        return Err(unknown());
    };
    let digest = spec.digest();
    let ServerSpec::Stdio { command, args } = *spec else {
        return Err(failure("server_check_command_only"));
    };
    let slot = CheckSlot::take()?;
    let root = engine_root(authority)?;
    let mut values: Vec<(String, Vec<u8>)> = Vec::with_capacity(secrets.len());
    for secret in &secrets {
        match crate::storage::plugins::read_secret(&root, &secret.reference) {
            Ok(value) => values.push((secret.name.clone(), value)),
            Err(problem) => {
                for (_, value) in values.iter_mut() {
                    value.fill(0);
                }
                return Err(problem);
            }
        }
    }
    let ResultValue::Server(view) = actor(engine, Command::Server { id: id.clone() }).await? else {
        return Err(unknown());
    };
    let engine = engine.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let folder = id.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let outcome = boundary_check(&root, &folder, &command, &args, &values);
            for (_, value) in values.iter_mut() {
                value.fill(0);
            }
            outcome
        })
        .await;
        let outcome = match outcome {
            Ok(Ok(tools)) => Ok(tools),
            Ok(Err(code)) => Err(code.to_owned()),
            Err(_) => Err("server_cant_start".to_owned()),
        };
        if let Err(problem) = actor(
            &engine,
            Command::RecordCheck {
                id,
                digest,
                outcome,
            },
        )
        .await
        {
            eprintln!("plugin_check_not_recorded cause={}", problem.0);
        }
    });
    Ok(Reply::Server { server: *view })
}

/// Checks an approved command server inside the boundary agents run it in: its own
/// project, home and temp folders under `mcp-checks/<id>` in the engine folder, reads of
/// system paths and the command's folder only, open network, and only its own secrets.
/// Inputs: the engine folder, server id, command, arguments and secret values. Output:
/// the tool names, or a plain rejection code.
#[cfg(unix)]
fn boundary_check(
    root: &Path,
    id: &str,
    command: &str,
    args: &[String],
    secrets: &[(String, Vec<u8>)],
) -> Result<Vec<String>, &'static str> {
    use crate::sandbox::{InstalledSandbox, ProjectSandbox, SandboxNetwork};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    assert!(valid_id(id));
    let program = Path::new(command);
    match std::fs::metadata(program) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("server_command_not_found");
        }
        Ok(found) if found.is_file() && found.permissions().mode() & 0o111 != 0 => {}
        _ => return Err("server_command_not_runnable"),
    }
    let refused = |cause: &dyn std::fmt::Display| {
        eprintln!("plugin_check_boundary_refused cause={cause}");
        "server_cant_start"
    };
    let checks = root.join(CHECK_FOLDER);
    let mut folders = Vec::with_capacity(3);
    for name in ["project", "home", "tmp"] {
        let folder = checks.join(id).join(name);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&folder)
            .map_err(|error| refused(&error))?;
        let plain = checks
            .join(id)
            .canonicalize()
            .map_err(|error| refused(&error))?
            .join(name);
        if folder.canonicalize().map_err(|error| refused(&error))? != plain {
            return Err(refused(
                &"check folder is not the plain folder the engine made",
            ));
        }
        folders.push(plain);
    }
    let profiles = checks.join("profiles");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&profiles)
        .map_err(|error| refused(&error))?;
    let reads = program.parent().ok_or("server_command_not_runnable")?;
    let sandbox = ProjectSandbox::new(&folders[0], &folders[1], &folders[2], SandboxNetwork::Open)
        .and_then(|sandbox| sandbox.guard_engine(root))
        .and_then(|sandbox| sandbox.allow_read(reads))
        .and_then(|sandbox| InstalledSandbox::install(sandbox, &profiles.join(format!("{id}.sb"))))
        .map_err(|error| refused(&error))?;
    let mut server = std::process::Command::new(program);
    server.args(args).current_dir(&folders[0]);
    let mut process = sandbox.wrap(&server).map_err(|error| refused(&error))?;
    for (name, value) in secrets {
        process.env(name, std::ffi::OsStr::from_bytes(value));
    }
    assert_eq!(process.get_program(), sandbox.launcher().as_os_str());
    check::check_command(process, check::CHECK_TIME)
}

/// Refuses to check a command server where no process group can be stopped. Takes the
/// engine folder, server id, command, arguments and secrets, all unused. Returns
/// `server_check_unavailable`, like [`check::check_stdio`] on this system.
#[cfg(not(unix))]
fn boundary_check(
    _root: &Path,
    _id: &str,
    _command: &str,
    _args: &[String],
    _secrets: &[(String, Vec<u8>)],
) -> Result<Vec<String>, &'static str> {
    Err("server_check_unavailable")
}

async fn actor(engine: &Engine, command: Command) -> Result<ResultValue, EngineError> {
    engine
        .native_auth_job(|reply| Job::Plugins(command, reply))
        .await
}

async fn receive_secret(stream: &mut UnixStream) -> Result<AccountSecret, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let invalid = || failure("server_secret_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > SECRET_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            bytes.fill(0);
            return Err(invalid());
        }
        AccountSecret::new(bytes).map_err(|_| invalid())
    })
    .await
    .map_err(|_| failure("server_secret_invalid"))?
}

fn public_code(error: EngineError) -> String {
    if error.0.len() <= 96
        && error
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        error.0
    } else {
        "plugins_outcome_unknown".into()
    }
}
