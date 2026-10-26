use super::*;
#[cfg(unix)]
use crate::plugins::ServerSpec;
#[cfg(unix)]
use crate::setup::fetch;
use crate::setup::pack::PackManifestEntry;
#[cfg(unix)]
use crate::setup::pack::{PackKind, PackSource};
use crate::setup::progress;
use crate::setup::{Dependency, DependencyState, InstallError, Installer, PackState, plain_reason};
use crate::storage::setup::Command;
use tokio::sync::mpsc;

#[cfg(unix)]
struct Prepared {
    source: String,
    skills: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
}

const RECHECK_AFTER: Duration = Duration::from_secs(30);
const FIRST_CHECK_WAIT: Duration = Duration::from_millis(500);
const CHECK_POLL: Duration = Duration::from_millis(20);

struct RunGuard(PathBuf);

impl Drop for RunGuard {
    fn drop(&mut self) {
        progress::with(&self.0, |state| state.running = false);
    }
}

struct CheckGuard(PathBuf);

impl Drop for CheckGuard {
    fn drop(&mut self) {
        progress::with(&self.0, |state| state.checking = false);
    }
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

async fn storage(sender: &mpsc::Sender<Job>, command: Command) -> Result<(), EngineError> {
    let (send, receive) = oneshot::channel();
    sender
        .send(Job::Setup(command, send))
        .await
        .map_err(|_| failure("setup_store_unavailable"))?;
    receive
        .await
        .map_err(|_| failure("setup_outcome_unknown"))?
}

fn platform_installer(root: &Path) -> Arc<dyn Installer> {
    Arc::from(crate::setup::platform_installer(root))
}

/// Finishes one owner request that touches setup, after the engine answered it: a setup
/// status read gets each dependency's state from the last background version check, an
/// applied setup run starts installing in the background, and an applied removal sweeps
/// the folders and secrets it left. Inputs: the engine, its authority, the request, its
/// reply and the exchange deadline. Output: the reply, with the dependencies filled in
/// for a setup status read.
pub(super) async fn after_owner(
    engine: &Engine,
    authority: &Authority,
    request: &OwnerRequest,
    result: Result<OwnerReply, EngineError>,
    deadline: Instant,
) -> Result<OwnerReply, EngineError> {
    let mut reply = result?;
    match (&request.operation, &mut reply.response) {
        (
            OwnerOperation::Read {
                query: OwnerRead::SetupStatus,
            },
            OwnerResponse::Read { state },
        ) => {
            if let OwnerData::SetupStatus { status } = &mut state.data {
                let root = engine_root(authority)?;
                status.dependencies = dependency_rows(&root, deadline).await;
            }
        }
        (OwnerOperation::Apply { mutation }, OwnerResponse::Applied { operation })
            if !operation.historical_replay && matches!(operation.result, OwnerResult::Applied) =>
        {
            let sender = engine.storage.sender.clone();
            match mutation {
                OwnerMutation::RunSetup { pack } => {
                    start_run(sender, engine_root(authority)?, pack)?;
                }
                OwnerMutation::RemovePackEntry { .. } => {
                    let _ = storage(&sender, Command::Sweep).await;
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(reply)
}

fn start_check(root: &Path) {
    let begin = progress::with(root, |state| {
        let stale = state
            .checked
            .as_ref()
            .is_none_or(|checked| checked.at.elapsed() >= RECHECK_AFTER);
        let begin = stale && !state.checking;
        if begin {
            state.checking = true;
        }
        begin
    });
    if !begin {
        return;
    }
    let guard = CheckGuard(root.to_path_buf());
    let installer = platform_installer(root);
    tokio::spawn(async move {
        let found = tokio::task::spawn_blocking(move || {
            Dependency::ALL.map(|dep| (dep, installer.find(dep)))
        })
        .await;
        if let Ok(found) = found {
            progress::with(&guard.0, |state| {
                state.checked = Some(progress::Checked {
                    at: std::time::Instant::now(),
                    found: found.to_vec(),
                });
            });
        }
        drop(guard);
    });
}

async fn dependency_rows(root: &Path, deadline: Instant) -> Vec<crate::setup::DependencyStatus> {
    start_check(root);
    let wait = FIRST_CHECK_WAIT.min(deadline.saturating_duration_since(Instant::now()) / 2);
    let until = Instant::now() + wait;
    loop {
        let progress = progress::snapshot(root);
        if progress.checked.is_some() || Instant::now() >= until {
            return progress::dependency_rows(&progress);
        }
        tokio::time::sleep(CHECK_POLL).await;
    }
}

fn start_run(
    sender: mpsc::Sender<Job>,
    root: PathBuf,
    ticked: &[String],
) -> Result<(), EngineError> {
    let manifest = crate::setup::pack::default_pack()?;
    let entries: Vec<PackManifestEntry> = manifest
        .entries
        .into_iter()
        .filter(|entry| ticked.contains(&entry.id))
        .collect();
    let begun = progress::with(&root, |state| {
        if state.running {
            return false;
        }
        state.running = true;
        state.dependencies = Default::default();
        state.pack = entries
            .iter()
            .map(|entry| (entry.id.clone(), PackState::Installing))
            .collect();
        true
    });
    if !begun {
        return Err(failure("setup_busy"));
    }
    let guard = RunGuard(root);
    tokio::spawn(async move {
        let root = guard.0.clone();
        run_setup(&sender, &root, entries).await;
        drop(guard);
    });
    Ok(())
}

async fn install_dependency(installer: Arc<dyn Installer>, root: &Path, dep: Dependency) {
    let finder = installer.clone();
    let found = tokio::task::spawn_blocking(move || finder.find(dep)).await;
    if matches!(found, Ok(Ok(Some(_)))) {
        return;
    }
    progress::with(root, |state| {
        state.dependencies.insert(dep, DependencyState::Installing);
    });
    let outcome = tokio::task::spawn_blocking(move || match crate::setup::platform_pin(dep) {
        Some(pin) => installer.install(dep, &pin),
        None => Err(InstallError(crate::setup::PLATFORM_UNSUPPORTED)),
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
    progress::with(root, |state| {
        state.dependencies.insert(dep, next);
    });
}

#[cfg(unix)]
fn source_key(source: &PackSource) -> String {
    match source {
        PackSource::Git { repo, commit, path } => {
            format!("{repo}@{commit}/{}", path.as_deref().unwrap_or_default())
        }
        PackSource::Bundled { path } => {
            format!("bundled/{path}@{}", crate::setup::bundled::digest(path))
        }
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
async fn install_entry(
    sender: &mpsc::Sender<Job>,
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
        Ok(Ok(prepared)) => storage(
            sender,
            Command::Install {
                id: id.clone(),
                source: prepared.source,
                skills: prepared.skills,
                server: prepared.server,
            },
        )
        .await
        .map_err(|problem| problem.0),
        Ok(Err(code)) => Err(code.0.to_owned()),
        Err(_) => Err("setup_install_failed".to_owned()),
    };
    progress::with(root, |state| match outcome {
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

/// Marks one pack entry failed where its source cannot be fetched or unpacked, since the
/// fetch and bundled steps keep Unix file modes. Takes the same inputs as the Unix version;
/// only the engine root and the entry are used.
#[cfg(not(unix))]
async fn install_entry(
    _sender: &mpsc::Sender<Job>,
    _installer: Arc<dyn Installer>,
    root: &Path,
    _bundled: Option<PathBuf>,
    entry: PackManifestEntry,
) {
    progress::with(root, |state| {
        state.pack.insert(
            entry.id,
            PackState::Failed {
                reason: reason(crate::setup::PLATFORM_UNSUPPORTED),
            },
        );
    });
}

/// Unpacks the skills shipped inside the engine into the setup folder.
/// Takes the setup folder. Returns their folder, or `None` when they cannot be unpacked.
#[cfg(unix)]
fn extract_bundled(setup: &Path) -> Option<PathBuf> {
    crate::setup::bundled::extract(setup).ok()
}

/// Reports no bundled skills where they cannot be unpacked with their Unix file modes.
/// Takes the setup folder, unused. Returns `None`.
#[cfg(not(unix))]
fn extract_bundled(_setup: &Path) -> Option<PathBuf> {
    None
}

async fn run_setup(sender: &mpsc::Sender<Job>, root: &Path, entries: Vec<PackManifestEntry>) {
    let installer = platform_installer(root);
    let graph_tool = progress::snapshot(root).graph_tool;
    for dep in Dependency::ALL {
        if dep == Dependency::Graphify && !graph_tool {
            continue;
        }
        install_dependency(installer.clone(), root, dep).await;
    }
    let _ = storage(sender, Command::Sweep).await;
    let setup = root.join("setup");
    let bundled = tokio::task::spawn_blocking(move || extract_bundled(&setup))
        .await
        .ok()
        .flatten();
    for entry in entries {
        install_entry(sender, installer.clone(), root, bundled.clone(), entry).await;
    }
}
