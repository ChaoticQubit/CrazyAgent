use super::*;
use crate::plugins::ServerSpec;
use crate::protocol::encode_frame;
use crate::setup::fetch;
use crate::setup::pack::{PackGroup, PackKind, PackManifest, PackManifestEntry, PackSource};
use crate::setup::{
    Dependency, DependencyState, DependencyStatus, InstallError, Installer, PackEntry, PackState,
    SetupStatus, plain_reason,
};
use crate::storage::setup::{Command, ResultValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const STATUS_TIME: Duration = Duration::from_secs(45);
const PACK_ID_BYTES_MAX: usize = 64;

static PROGRESS: std::sync::Mutex<BTreeMap<PathBuf, Progress>> =
    std::sync::Mutex::new(BTreeMap::new());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status,
    Run { pack: Vec<String> },
    SetPack { id: String, on: bool },
    RemovePack { id: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    setup: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Status { status: SetupStatus },
    Rejected { code: String },
}

#[derive(Default)]
struct Progress {
    running: bool,
    dependencies: BTreeMap<Dependency, DependencyState>,
    pack: BTreeMap<String, PackState>,
}

struct Prepared {
    source: String,
    skills: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
}

struct RunGuard(PathBuf);

impl Drop for RunGuard {
    fn drop(&mut self) {
        progress(&self.0, |state| state.running = false);
    }
}

fn unknown() -> EngineError {
    failure("setup_outcome_unknown")
}

fn progress<T>(root: &Path, change: impl FnOnce(&mut Progress) -> T) -> T {
    let mut all = PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    change(all.entry(root.to_path_buf()).or_default())
}

fn valid_pack_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= PACK_ID_BYTES_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

impl Action {
    fn validate(&self) -> Result<(), EngineError> {
        let valid = match self {
            Self::Status => true,
            Self::Run { pack } => {
                pack.len() <= crate::setup::pack::ENTRIES_MAX
                    && pack.iter().all(|id| valid_pack_id(id))
            }
            Self::SetPack { id, .. } | Self::RemovePack { id } => valid_pack_id(id),
        };
        if !valid {
            return Err(failure("pack_unknown"));
        }
        Ok(())
    }
}

impl OwnerClient {
    /// Reads the setup screen: each dependency (tmux, uv, Graphify) with its state and
    /// version, each default-pack entry with its source, license and state, and whether a
    /// setup run is going on. Output: the setup status.
    pub async fn setup_status(&self) -> Result<SetupStatus, EngineError> {
        self.setup_reply(Action::Status).await
    }

    /// Starts setup in the engine: installs missing dependencies, then the ticked default-
    /// pack entries plus every required one. Ticked MCP servers count as approved by the
    /// owner. Input: the ticked entry ids. Output: the status as the run starts; read
    /// `setup_status` again for progress.
    pub async fn run_setup(&self, pack: Vec<String>) -> Result<SetupStatus, EngineError> {
        self.setup_reply(Action::Run { pack }).await
    }

    /// Switches one installed default-pack entry on or off with all its skills and its MCP
    /// server. Inputs: the entry id and the new state. Output: the setup status.
    pub async fn set_pack_entry(&self, id: String, on: bool) -> Result<SetupStatus, EngineError> {
        self.setup_reply(Action::SetPack { id, on }).await
    }

    /// Removes one installed default-pack entry and everything it added; required entries
    /// are refused. Input: the entry id. Output: the setup status.
    pub async fn remove_pack_entry(&self, id: String) -> Result<SetupStatus, EngineError> {
        self.setup_reply(Action::RemovePack { id }).await
    }

    async fn setup_reply(&self, action: Action) -> Result<SetupStatus, EngineError> {
        action.validate()?;
        let timeout = self.timeout.max(STATUS_TIME);
        let reply = tokio::time::timeout(timeout, async {
            let frame = encode_frame(&Envelope { setup: action })
                .map_err(|_| failure("setup_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = UnixStream::connect(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("setup_owner_unavailable"))?;
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
                .map_err(|_| failure("setup_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("setup_owner_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            serde_json::from_slice::<Reply>(&frame[4..]).map_err(|_| unknown())
        })
        .await
        .map_err(|_| unknown())??;
        match reply {
            Reply::Status { status } => Ok(status),
            Reply::Rejected { code } => Err(EngineError(code)),
        }
    }
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
        serde_json::from_slice(bytes).map_err(|_| failure("setup_request_invalid"))?;
    let reply = match handle(engine, authority, packet, admission, envelope.setup).await {
        Ok(status) => Reply::Status { status },
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "setup_reply_bounds".into(),
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
        .ok_or_else(|| failure("setup_store_unavailable"))
}

fn known<'a>(manifest: &'a PackManifest, id: &str) -> Result<&'a PackManifestEntry, EngineError> {
    manifest
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| failure("pack_unknown"))
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    action: Action,
) -> Result<SetupStatus, EngineError> {
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    action.validate()?;
    let root = engine_root(authority)?;
    let manifest = crate::setup::pack::default_pack()?;
    match action {
        Action::Status => {}
        Action::Run { pack } => start_run(engine, &root, &manifest, pack)?,
        Action::SetPack { id, on } => {
            if !on && known(&manifest, &id)?.group == PackGroup::Always {
                return Err(failure("pack_required"));
            }
            actor(engine, Command::Set { id, enabled: on }).await?;
        }
        Action::RemovePack { id } => {
            if known(&manifest, &id)?.group == PackGroup::Always {
                return Err(failure("pack_required"));
            }
            actor(engine, Command::Remove { id }).await?;
        }
    }
    status(engine, &root, &manifest).await
}

fn start_run(
    engine: &Arc<Engine>,
    root: &Path,
    manifest: &PackManifest,
    ticked: Vec<String>,
) -> Result<(), EngineError> {
    for id in &ticked {
        known(manifest, id)?;
    }
    let entries: Vec<PackManifestEntry> = manifest
        .entries
        .iter()
        .filter(|entry| entry.group == PackGroup::Always || ticked.contains(&entry.id))
        .cloned()
        .collect();
    let begun = progress(root, |state| {
        if state.running {
            return false;
        }
        state.running = true;
        state.dependencies.clear();
        state.pack = entries
            .iter()
            .map(|entry| (entry.id.clone(), PackState::Installing))
            .collect();
        true
    });
    if !begun {
        return Err(failure("setup_busy"));
    }
    let guard = RunGuard(root.to_path_buf());
    let engine = engine.clone();
    tokio::spawn(async move {
        let root = guard.0.clone();
        run_setup(&engine, &root, entries).await;
        drop(guard);
    });
    Ok(())
}

fn platform_installer(root: &Path) -> Arc<dyn Installer> {
    Arc::from(crate::setup::platform_installer(root))
}

async fn install_dependency(installer: Arc<dyn Installer>, root: &Path, dep: Dependency) {
    let finder = installer.clone();
    let found = tokio::task::spawn_blocking(move || finder.find(dep)).await;
    if matches!(found, Ok(Ok(Some(_)))) {
        return;
    }
    progress(root, |state| {
        state.dependencies.insert(dep, DependencyState::Installing);
    });
    let outcome = tokio::task::spawn_blocking(move || {
        installer.install(dep, &crate::setup::platform_pin(dep))
    })
    .await;
    let next = match outcome {
        Ok(Ok(version)) => DependencyState::Installed { version },
        Ok(Err(code)) => DependencyState::Failed {
            reason: plain_reason(code.0).to_owned(),
        },
        Err(_) => DependencyState::Failed {
            reason: plain_reason("setup_install_failed").to_owned(),
        },
    };
    progress(root, |state| {
        state.dependencies.insert(dep, next);
    });
}

fn source_key(source: &PackSource) -> String {
    match source {
        PackSource::Git { repo, commit, path } => {
            format!("{repo}@{commit}/{}", path.as_deref().unwrap_or_default())
        }
        PackSource::Bundled { path } => format!("bundled/{path}"),
    }
}

fn source_label(source: &PackSource) -> String {
    match source {
        PackSource::Git { repo, .. } => repo
            .trim_start_matches("https://")
            .trim_end_matches(".git")
            .to_owned(),
        PackSource::Bundled { .. } => "Bundled with CrazyAgent".to_owned(),
    }
}

fn bundled_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let folder = executable.parent()?;
    [folder.join("bundled"), folder.join("../Resources/bundled")]
        .into_iter()
        .find(|candidate| std::fs::symlink_metadata(candidate).is_ok_and(|m| m.is_dir()))
}

fn prepare(
    installer: &dyn Installer,
    setup: &Path,
    bundled: Option<&Path>,
    entry: &PackManifestEntry,
) -> Result<Prepared, InstallError> {
    let folder = fetch::entry_folder(setup, entry, bundled)?;
    let source = source_key(&entry.source);
    match entry.kind {
        PackKind::Skill => Ok(Prepared {
            source,
            skills: fetch::skill_folders(&folder)?,
            server: None,
        }),
        PackKind::McpServer => {
            let resolve = |name: &str| installer.program(name);
            let plan = fetch::server_plan(entry, &folder, &resolve)?;
            if let Some((package, version)) = &plan.tool {
                installer.install_tool(package, version)?;
            }
            let spec = fetch::finish_plan(plan, &resolve)?;
            Ok(Prepared {
                source,
                skills: Vec::new(),
                server: Some((fetch::server_name(entry)?, spec)),
            })
        }
    }
}

fn reason(code: &str) -> String {
    if code.starts_with("setup_") || code.starts_with("pack_") {
        return plain_reason(code).to_owned();
    }
    crate::plugins::plain_problem(code)
        .unwrap_or_else(|| plain_reason(code))
        .to_owned()
}

async fn install_entry(
    engine: &Arc<Engine>,
    installer: Arc<dyn Installer>,
    root: &Path,
    bundled: Option<PathBuf>,
    entry: PackManifestEntry,
) {
    let setup = root.join("setup");
    let id = entry.id.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare(installer.as_ref(), &setup, bundled.as_deref(), &entry)
    })
    .await;
    let outcome = match prepared {
        Ok(Ok(prepared)) => actor(
            engine,
            Command::Install {
                id: id.clone(),
                source: prepared.source,
                skills: prepared.skills,
                server: prepared.server,
            },
        )
        .await
        .map(|_| ())
        .map_err(|problem| problem.0),
        Ok(Err(code)) => Err(code.0.to_owned()),
        Err(_) => Err("setup_install_failed".to_owned()),
    };
    progress(root, |state| match outcome {
        Ok(()) => {
            state.pack.remove(&id);
        }
        Err(code) => {
            state.pack.insert(
                id.clone(),
                PackState::Failed {
                    reason: reason(&code),
                },
            );
        }
    });
}

async fn run_setup(engine: &Arc<Engine>, root: &Path, entries: Vec<PackManifestEntry>) {
    let installer = platform_installer(root);
    for dep in Dependency::ALL {
        install_dependency(installer.clone(), root, dep).await;
    }
    let bundled = bundled_root();
    for entry in entries {
        install_entry(engine, installer.clone(), root, bundled.clone(), entry).await;
    }
}

async fn status(
    engine: &Arc<Engine>,
    root: &Path,
    manifest: &PackManifest,
) -> Result<SetupStatus, EngineError> {
    let ResultValue::Installed(installed) = actor(engine, Command::Installed).await? else {
        return Err(unknown());
    };
    let installer = platform_installer(root);
    let found =
        tokio::task::spawn_blocking(move || Dependency::ALL.map(|dep| (dep, installer.find(dep))))
            .await
            .map_err(|_| unknown())?;
    let (running, steps, entries) = progress(root, |state| {
        (
            state.running,
            state.dependencies.clone(),
            state.pack.clone(),
        )
    });
    let dependencies: Vec<DependencyStatus> = found
        .into_iter()
        .map(|(dep, found)| DependencyStatus {
            name: dep,
            label: dep.label().to_owned(),
            state: match (steps.get(&dep), found) {
                (Some(step), _) => step.clone(),
                (None, Ok(Some(version))) => DependencyState::Found { version },
                (None, Ok(None)) => DependencyState::Missing,
                (None, Err(code)) => DependencyState::Failed {
                    reason: plain_reason(code.0).to_owned(),
                },
            },
        })
        .collect();
    let pack: Vec<PackEntry> = manifest
        .entries
        .iter()
        .map(|entry| {
            let row = installed.iter().find(|row| row.id == entry.id);
            let state = match (entries.get(&entry.id), row) {
                (Some(PackState::Installing), _) => PackState::Installing,
                (_, Some(row)) if row.enabled => PackState::On,
                (_, Some(_)) => PackState::Off,
                (Some(state), None) => state.clone(),
                (None, None) => PackState::Available,
            };
            PackEntry {
                id: entry.id.clone(),
                kind: entry.kind,
                group: entry.group,
                source: source_label(&entry.source),
                license: entry.license.clone(),
                on_by_default: entry.on_by_default,
                state,
            }
        })
        .collect();
    assert_eq!(dependencies.len(), Dependency::ALL.len());
    assert_eq!(pack.len(), manifest.entries.len());
    Ok(SetupStatus {
        dependencies,
        pack,
        running,
    })
}

async fn actor(engine: &Engine, command: Command) -> Result<ResultValue, EngineError> {
    engine
        .native_auth_job(|reply| Job::Setup(command, reply))
        .await
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
        "setup_outcome_unknown".into()
    }
}
