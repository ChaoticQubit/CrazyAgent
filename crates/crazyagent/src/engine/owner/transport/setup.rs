use super::*;
use crate::plugins::ServerSpec;
use crate::setup::fetch;
use crate::setup::pack::{PackGroup, PackKind, PackManifestEntry, PackSource};
use crate::setup::progress::{self, Progress};
use crate::setup::{Dependency, DependencyState, InstallError, Installer, PackState, plain_reason};
use crate::storage::setup::Command;
use tokio::sync::mpsc;

struct Prepared {
    source: String,
    skills: Vec<PathBuf>,
    server: Option<(String, ServerSpec)>,
}

struct RunGuard(PathBuf);

impl Drop for RunGuard {
    fn drop(&mut self) {
        progress::with(&self.0, |state| state.running = false);
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
/// status read gets each dependency's state from this machine, an applied setup run
/// starts installing in the background, and an applied removal sweeps the folders and
/// secrets it left. Inputs: the engine, its authority, the request and its reply.
/// Output: the reply, with the status filled in for a setup status read.
pub(super) async fn after_owner(
    engine: &Engine,
    authority: &Authority,
    request: &OwnerRequest,
    result: Result<OwnerReply, EngineError>,
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
                status.dependencies = dependency_rows(&root).await?;
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

async fn dependency_rows(root: &Path) -> Result<Vec<crate::setup::DependencyStatus>, EngineError> {
    let installer = platform_installer(root);
    let found =
        tokio::task::spawn_blocking(move || Dependency::ALL.map(|dep| (dep, installer.find(dep))))
            .await
            .map_err(|_| failure("setup_outcome_unknown"))?;
    Ok(progress::dependency_rows(&progress::snapshot(root), &found))
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
        .filter(|entry| entry.group == PackGroup::Always || ticked.contains(&entry.id))
        .collect();
    let begun = progress::with(&root, |state| {
        if state.running {
            return false;
        }
        *state = Progress {
            running: true,
            dependencies: Default::default(),
            pack: entries
                .iter()
                .map(|entry| (entry.id.clone(), PackState::Installing))
                .collect(),
        };
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
    progress::with(root, |state| {
        state.dependencies.insert(dep, next);
    });
}

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

async fn run_setup(sender: &mpsc::Sender<Job>, root: &Path, entries: Vec<PackManifestEntry>) {
    let installer = platform_installer(root);
    for dep in Dependency::ALL {
        install_dependency(installer.clone(), root, dep).await;
    }
    let _ = storage(sender, Command::Sweep).await;
    let setup = root.join("setup");
    let bundled = tokio::task::spawn_blocking(move || crate::setup::bundled::extract(&setup).ok())
        .await
        .ok()
        .flatten();
    for entry in entries {
        install_entry(sender, installer.clone(), root, bundled.clone(), entry).await;
    }
}
