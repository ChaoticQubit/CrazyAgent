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
    plugin: Option<PathBuf>,
}

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

pub(super) fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
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
/// applied setup run or one-dependency install starts in the background, and every removal, applied or
/// not, sweeps the folders and secrets it moved aside or left, an applied agent policy
/// reaches the project's open IDE, and a Reconcile answered Committed with an applied result
/// reaches every open IDE, since a Save's lost reply leaves that the only call after its
/// commit and its answer names no project. Inputs: the engine, its authority, the request,
/// its reply and the exchange deadline. Output: the reply, with the dependencies filled in
/// for a setup status read.
pub(super) async fn after_owner(
    engine: &Engine,
    authority: &Arc<Authority>,
    request: &OwnerRequest,
    result: Result<OwnerReply, EngineError>,
    deadline: Instant,
) -> Result<OwnerReply, EngineError> {
    if let OwnerOperation::Apply {
        mutation: OwnerMutation::RemovePackEntry { .. },
    } = &request.operation
    {
        let _ = storage(&engine.storage.sender, Command::Sweep).await;
    }
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
                status.notes = crate::research::runner::agent_git_problem()
                    .into_iter()
                    .map(|code| plain_reason(code).to_owned())
                    .collect();
            }
        }
        (
            OwnerOperation::Apply {
                mutation: OwnerMutation::RunSetup { pack },
            },
            OwnerResponse::Applied { operation },
        ) if !operation.historical_replay && matches!(operation.result, OwnerResult::Applied) => {
            start_run(engine.storage.sender.clone(), engine_root(authority)?, pack)?;
        }
        (
            OwnerOperation::Apply {
                mutation: OwnerMutation::InstallDependency { name },
            },
            OwnerResponse::Applied { operation },
        ) if !operation.historical_replay && matches!(operation.result, OwnerResult::Applied) => {
            start_install(engine_root(authority)?, *name)?;
        }
        (
            OwnerOperation::Apply {
                mutation: OwnerMutation::SetProjectAgentPolicy { project_id, .. },
            },
            OwnerResponse::Applied { operation },
        ) if matches!(operation.result, OwnerResult::Applied) => {
            follow_saved_level(authority, project_id.clone());
        }
        (OwnerOperation::Reconcile { .. }, OwnerResponse::Committed { operation })
            if matches!(operation.result, OwnerResult::Applied) =>
        {
            follow_saved_levels(authority);
        }
        _ => {}
    }
    Ok(reply)
}

/// Stops a project's IDE in the background when the agent policy just committed no longer
/// fits it. Detached so a Save never waits on a launch holding the start lock or on a
/// server's stop grace, and a replay of the same Save follows too, since the check only
/// compares. Inputs: the authority and the project.
#[cfg(unix)]
fn follow_saved_level(authority: &Arc<Authority>, project: crate::protocol::ProjectId) {
    let authority = Arc::clone(authority);
    tokio::spawn(async move {
        authority.ide_servers.follow_level(&project).await;
    });
}

#[cfg(not(unix))]
fn follow_saved_level(_authority: &Arc<Authority>, _project: crate::protocol::ProjectId) {}

/// Compares every project's open IDE with its saved policy, each in the background so a
/// launch holding one project's start lock never delays another's stop. Input: the authority.
#[cfg(unix)]
fn follow_saved_levels(authority: &Arc<Authority>) {
    for project in authority.ide_servers.projects() {
        follow_saved_level(authority, project);
    }
}

#[cfg(not(unix))]
fn follow_saved_levels(_authority: &Arc<Authority>) {}

fn start_check(root: &Path) {
    let mark = crate::tools::probe::mark();
    let begin = progress::with(root, |state| {
        let stale = progress::check_due(state.checked.as_ref(), mark, std::time::Instant::now());
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
            Dependency::ALL.map(|dep| (dep, installer.lookup(dep)))
        })
        .await;
        if let Ok(found) = found {
            progress::with(&guard.0, |state| {
                state.checked = Some(progress::Checked {
                    at: std::time::Instant::now(),
                    found: found.to_vec(),
                    probe: mark,
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
    if !begin_run(&root, &entries) {
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

/// Marks a setup run as going for the owner's explicit Set up tools, Install missing or a
/// tool row's Try again, unless one is going: every tool and the ticked pack entries start
/// over as installing, and the background probes look for tool folders again at once.
/// Inputs: the engine root and the ticked pack entries. Output: whether this run begins.
pub(super) fn begin_run(root: &Path, entries: &[PackManifestEntry]) -> bool {
    let begun = progress::with(root, |state| {
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
    if begun {
        crate::tools::probe::refresh_now();
    }
    begun
}

/// Installs one missing dependency alone in the background, as the setup row's Install and
/// the harness picker ask: found first and left as it is when present, then installed from
/// its pin. Only that row changes, and the next status read checks every tool again.
/// Inputs: the engine root and the dependency. Output: Ok once it runs, or `setup_busy`.
pub(super) fn start_install(root: PathBuf, dep: Dependency) -> Result<(), EngineError> {
    let guard = begin_install(root, dep)?;
    tokio::spawn(async move {
        let root = guard.0.clone();
        install_dependency(platform_installer(&root), &root, dep).await;
        progress::with(&root, |state| state.checked = None);
        drop(guard);
    });
    Ok(())
}

/// Marks a one-dependency install as the running setup: refused while any run goes on,
/// else running with only that dependency's earlier row cleared. Inputs: the engine root
/// and the dependency. Output: the guard that ends the run, or `setup_busy`.
fn begin_install(root: PathBuf, dep: Dependency) -> Result<RunGuard, EngineError> {
    let begun = progress::with(&root, |state| {
        if state.running {
            return false;
        }
        state.running = true;
        state.dependencies.remove(&dep);
        true
    });
    if !begun {
        return Err(failure("setup_busy"));
    }
    assert!(progress::snapshot(&root).running);
    Ok(RunGuard(root))
}

async fn install_dependency(installer: Arc<dyn Installer>, root: &Path, dep: Dependency) {
    let finder = installer.clone();
    let found = tokio::task::spawn_blocking(move || finder.find(dep)).await;
    if matches!(found, Ok(Ok(Some(_)))) {
        return;
    }
    if let Ok(Err(code)) = &found
        && code.0 == crate::setup::PLATFORM_UNSUPPORTED
    {
        progress::with(root, |state| {
            state.dependencies.insert(dep, DependencyState::Unsupported);
        });
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
        Ok(Err(code)) => DependencyState::refused(code.0),
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
    let plugin = fetch::plugin_folder(setup, entry);
    match entry.kind {
        PackKind::Skill => Ok(Prepared {
            source,
            skills: fetch::skill_folders(&folder)?,
            server: None,
            plugin,
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
                plugin,
            })
        }
    }
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
        Ok(Ok(prepared)) => {
            let installed = storage(
                sender,
                Command::Install {
                    id: id.clone(),
                    source: prepared.source,
                    skills: prepared.skills,
                    server: prepared.server,
                },
            )
            .await
            .map_err(|problem| problem.0);
            if installed.is_ok()
                && let Some(plugin) = prepared.plugin
                && let Err(problem) = storage(
                    sender,
                    Command::SeedHooks {
                        id: id.clone(),
                        plugin,
                    },
                )
                .await
            {
                eprintln!("pack_hooks_not_added entry={id} cause={}", problem.0);
            }
            installed
        }
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
                    reason: crate::setup::pack_reason(&code),
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
                reason: crate::setup::pack_reason(crate::setup::PLATFORM_UNSUPPORTED),
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

/// Adds the hooks of every installed default-pack entry whose hook folder this build ships
/// or whose pinned checkout is already in the setup folder, so entries installed before
/// hooks were seeded get them too. Before a setup run made the setup folder it does
/// nothing, so an owner who continued without installing gets no folder. Otherwise the
/// shipped hook folders are unpacked first; nothing is downloaded, a linked checkout is
/// skipped and an entry whose hooks are present is left alone. Inputs: the storage job
/// sender and the engine root. Output: nothing; an entry that fails is logged, its setup
/// row says why, and the rest still run.
#[cfg(unix)]
pub(super) async fn backfill_hooks(sender: mpsc::Sender<Job>, root: PathBuf) {
    let setup = root.join("setup");
    let cached = tokio::task::spawn_blocking(move || {
        if !std::fs::symlink_metadata(&setup).is_ok_and(|metadata| metadata.is_dir()) {
            return None;
        }
        if let Err(problem) = crate::setup::bundled::extract_hooks(&setup) {
            eprintln!("pack_hooks_not_unpacked cause={}", problem.0);
        }
        let manifest = crate::setup::pack::default_pack().ok()?;
        Some(
            manifest
                .entries
                .iter()
                .filter_map(|entry| Some((entry.id.clone(), fetch::plugin_folder(&setup, entry)?)))
                .collect::<Vec<_>>(),
        )
    })
    .await;
    let Ok(Some(cached)) = cached else {
        return;
    };
    for (id, plugin) in cached {
        if let Err(problem) = storage(
            &sender,
            Command::SeedHooks {
                id: id.clone(),
                plugin,
            },
        )
        .await
        {
            eprintln!("pack_hooks_not_added entry={id} cause={}", problem.0);
        }
    }
}

/// Adds no hooks where pack checkouts cannot be installed. Takes the same inputs as the
/// Unix version, unused. Returns nothing.
#[cfg(not(unix))]
pub(super) async fn backfill_hooks(_sender: mpsc::Sender<Job>, _root: PathBuf) {}

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
    backfill_hooks(sender.clone(), root.to_path_buf()).await;
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use crate::setup::Version;

    #[test]
    fn one_dependency_install_waits_for_no_run_and_clears_only_its_own_row() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let installed = DependencyState::Installed {
            version: Version::parse("0.9.9").unwrap(),
        };
        progress::with(&root, |state| {
            state.dependencies.insert(
                Dependency::Node,
                DependencyState::Failed {
                    reason: "earlier".into(),
                },
            );
            state.dependencies.insert(Dependency::Uv, installed.clone());
            state.pack.insert("caveman".into(), PackState::Installing);
        });
        let guard = begin_install(root.clone(), Dependency::Node).unwrap();
        let during = progress::snapshot(&root);
        assert!(during.running);
        assert_eq!(during.dependencies.get(&Dependency::Node), None);
        assert_eq!(during.dependencies.get(&Dependency::Uv), Some(&installed));
        assert_eq!(during.pack.len(), 1);
        assert!(matches!(
            begin_install(root.clone(), Dependency::Uv),
            Err(error) if error.0 == "setup_busy"
        ));
        drop(guard);
        assert!(!progress::snapshot(&root).running);
    }
}
