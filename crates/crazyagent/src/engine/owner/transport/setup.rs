use super::*;
use crate::harnesses::update::{Turns, Updates};
#[cfg(unix)]
use crate::plugins::ServerSpec;
#[cfg(unix)]
use crate::setup::fetch;
use crate::setup::pack::PackManifestEntry;
#[cfg(unix)]
use crate::setup::pack::{PackKind, PackSource};
use crate::setup::progress;
use crate::setup::{
    Dependency, DependencyState, DependencyStatus, InstallError, Installer, PackState, SETUP_JOB,
    plain_reason,
};
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
/// applied setup run, one-dependency install or permission fix starts in the background, and every removal, applied or
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
                overlay(
                    &mut status.dependencies,
                    &root,
                    engine.harness_updates.turns(),
                );
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
            start_run(
                engine.storage.sender.clone(),
                engine_root(authority)?,
                pack,
                engine.harness_updates.clone(),
            )?;
        }
        (
            OwnerOperation::Apply {
                mutation: OwnerMutation::InstallDependency { name },
            },
            OwnerResponse::Applied { operation },
        ) if !operation.historical_replay && matches!(operation.result, OwnerResult::Applied) => {
            start_install(
                engine_root(authority)?,
                *name,
                engine.harness_updates.clone(),
            )?;
        }
        (
            OwnerOperation::Apply {
                mutation:
                    OwnerMutation::FixToolPermissions {
                        name,
                        tree,
                        recursive,
                    },
            },
            OwnerResponse::Applied { operation },
        ) if !operation.historical_replay && matches!(operation.result, OwnerResult::Applied) => {
            start_fix(
                engine_root(authority)?,
                *name,
                tree.clone(),
                *recursive,
                engine.harness_updates.clone(),
            )?;
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
        let looked = tokio::task::spawn_blocking(move || look(installer.as_ref())).await;
        if let Ok((found, fixes)) = looked {
            progress::with(&guard.0, |state| {
                state.checked = Some(progress::Checked {
                    at: std::time::Instant::now(),
                    found,
                    probe: mark,
                });
                state.fixes = fixes;
            });
        }
        drop(guard);
    });
}

/// Looks for every dependency once, keeping the permission fix target of each one found and
/// passed over (DEC-162). Input: the installer. Output: what each lookup found and the fix
/// targets by dependency.
fn look(
    installer: &dyn Installer,
) -> (
    Vec<(Dependency, crate::setup::Lookup)>,
    std::collections::BTreeMap<Dependency, crate::tools::permission_fix::Target>,
) {
    let found: Vec<_> = Dependency::ALL
        .iter()
        .map(|dep| (*dep, installer.lookup(*dep)))
        .collect();
    let fixes = found
        .iter()
        .filter_map(|(dep, lookup)| match lookup {
            Ok(Err(missing)) => installer.fix_target(missing).map(|target| (*dep, target)),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), Dependency::ALL.len());
    (found, fixes)
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

/// Says on each tool row whether an install or fix of it runs now and, by name, the job it
/// waits behind in the engine's line. Inputs: the rows, the engine root and the line.
fn overlay(_rows: &mut [DependencyStatus], _root: &Path, _turns: &Turns) {}

fn start_run(
    sender: mpsc::Sender<Job>,
    root: PathBuf,
    ticked: &[String],
    updates: Arc<Updates>,
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
        run_setup(&sender, &root, entries, updates).await;
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
pub(super) fn start_install(
    root: PathBuf,
    dep: Dependency,
    updates: Arc<Updates>,
) -> Result<(), EngineError> {
    let installer = platform_installer(&root);
    start_install_with(installer, root, dep, updates)
}

/// [`start_install`] with the installer given. Inputs: the installer, the engine root, the
/// dependency and the engine's update state. Output: Ok once it runs, or `setup_busy`.
fn start_install_with(
    installer: Arc<dyn Installer>,
    root: PathBuf,
    dep: Dependency,
    updates: Arc<Updates>,
) -> Result<(), EngineError> {
    let guard = begin_one(root, dep)?;
    tokio::spawn(async move {
        let root = guard.0.clone();
        install_dependency(installer, &root, dep, updates, dep.program()).await;
        progress::with(&root, |state| state.checked = None);
        drop(guard);
    });
    Ok(())
}

/// Marks a one-dependency install or permission fix as the running setup: refused while
/// any run goes on, else running with only that dependency's earlier row and fix failure
/// cleared. Inputs: the engine root and the dependency. Output: the guard that ends the
/// run, or `setup_busy`.
fn begin_one(root: PathBuf, dep: Dependency) -> Result<RunGuard, EngineError> {
    let begun = progress::with(&root, |state| {
        if state.running {
            return false;
        }
        state.running = true;
        state.dependencies.remove(&dep);
        state.fix_failed.remove(&dep);
        true
    });
    if !begun {
        return Err(failure("setup_busy"));
    }
    assert!(progress::snapshot(&root).running);
    Ok(RunGuard(root))
}

/// Runs the owner's confirmed permission fix for one refused dependency alone in the
/// background, as the setup row's Fix asks (DEC-162), one at a time with any other setup
/// run. Inputs: the engine root, the dependency, the tree the owner confirmed and whether
/// the confirm named all of it. Output: Ok once it runs, or `setup_busy`.
fn start_fix(
    root: PathBuf,
    dep: Dependency,
    tree: String,
    recursive: bool,
    updates: Arc<Updates>,
) -> Result<(), EngineError> {
    let installer = platform_installer(&root);
    start_fix_with(installer, root, dep, tree, recursive, updates)
}

/// [`start_fix`] with the installer given. Inputs: the installer, the engine root, the
/// dependency, the confirmed tree, its scope and the engine's update state. Output: Ok once
/// it runs, or `setup_busy`.
fn start_fix_with(
    installer: Arc<dyn Installer>,
    root: PathBuf,
    dep: Dependency,
    tree: String,
    recursive: bool,
    updates: Arc<Updates>,
) -> Result<(), EngineError> {
    let guard = begin_one(root, dep)?;
    tokio::spawn(async move {
        let root = guard.0.clone();
        fix_dependency(installer, &root, dep, tree, recursive, updates).await;
        drop(guard);
    });
    Ok(())
}

/// Removes group and other write from the confirmed tree through the installer, logs one
/// line naming the tree and the counts or the code, keeps the failure's sentence on the
/// dependency's row, and has the next status read check every tool again. Inputs: the
/// installer, the engine root, the dependency, the confirmed tree and its confirmed scope.
async fn fix_dependency(
    installer: Arc<dyn Installer>,
    root: &Path,
    dep: Dependency,
    tree: String,
    recursive: bool,
    _updates: Arc<Updates>,
) {
    let shown = tree.clone();
    let outcome =
        tokio::task::spawn_blocking(move || installer.fix_permissions(dep, &tree, recursive))
            .await
            .unwrap_or(Err(crate::tools::permission_fix::TOOL_FIX_FAILED));
    match outcome {
        Ok(report) => eprintln!(
            "tool_fix_done tool={} tree={shown:?} changed={} shared={} skipped_foreign={} skipped_links={} skipped_other={}",
            dep.program(),
            report.changed,
            report.shared,
            report.skipped_foreign,
            report.skipped_links,
            report.skipped_other
        ),
        Err(code) => eprintln!(
            "tool_fix_stopped tool={} tree={shown:?} code={code}",
            dep.program()
        ),
    }
    progress::with(root, |state| {
        if let Err(code) = outcome {
            state.fix_failed.insert(dep, plain_reason(code).to_owned());
        }
        state.checked = None;
    });
}

/// Installs one dependency setup did not find and stores its row: installed, unsupported, or
/// why it failed. An install whose program then reports no usable version shows the check's
/// own line when that version is too old, naming it and the one needed (V243-TMUX-APT-OLD).
/// Inputs: the installer, the engine root and the dependency.
async fn install_dependency(
    installer: Arc<dyn Installer>,
    root: &Path,
    dep: Dependency,
    _updates: Arc<Updates>,
    _job: &'static str,
) {
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
    let outcome = tokio::task::spawn_blocking(move || {
        let installed = match crate::setup::platform_pin(dep) {
            Some(pin) => installer.install(dep, &pin),
            None => Err(InstallError(crate::setup::PLATFORM_UNSUPPORTED)),
        };
        match installed {
            Ok(version) => DependencyState::Installed { version },
            Err(code) if code.0 == "setup_version_unreadable" => match installer.lookup(dep) {
                Ok(Err(missing)) if missing.reason == crate::tools::Reason::WrongVersion => {
                    DependencyState::Missing {
                        line: missing.line(),
                    }
                }
                _ => DependencyState::refused(code.0),
            },
            Err(code) => DependencyState::refused(code.0),
        }
    })
    .await;
    let next = outcome.unwrap_or_else(|_| DependencyState::Failed {
        reason: plain_reason("setup_install_failed").to_owned(),
    });
    progress::with(root, |state| {
        state.dependencies.insert(dep, next);
    });
}

#[cfg(unix)]
fn source_key(source: &PackSource) -> String {
    match source {
        PackSource::Git { repo, commit, path } => {
            crate::setup::launch::git_key(repo, commit, path.as_deref())
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
        PackKind::McpServer => Ok(Prepared {
            source,
            skills: Vec::new(),
            server: Some(crate::setup::launch::pack_server(
                installer,
                entry,
                Some(&folder),
            )?),
            plugin,
        }),
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
    settle(root, id, outcome);
}

/// Installs one pack entry where setup fetches no checkout, since the fetch and bundled
/// steps keep Unix file modes: an MCP server entry whose program and arguments need no
/// entry folder is stored and approved ([`crate::setup::launch::server_only`]); any other
/// entry is marked failed with the reason. Inputs: the storage sender, the installer, the
/// engine root, the bundled folder (unused) and the entry. Output: none.
#[cfg(not(unix))]
async fn install_entry(
    sender: &mpsc::Sender<Job>,
    installer: Arc<dyn Installer>,
    root: &Path,
    _bundled: Option<PathBuf>,
    entry: PackManifestEntry,
) {
    let id = entry.id.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        crate::setup::launch::server_only(installer.as_ref(), &entry)
    })
    .await;
    let outcome = match prepared {
        Ok(Ok((source, server))) => storage(
            sender,
            Command::Install {
                id: id.clone(),
                source,
                skills: Vec::new(),
                server: Some(server),
            },
        )
        .await
        .map_err(|problem| problem.0),
        Ok(Err(code)) => Err(code.0.to_owned()),
        Err(_) => Err("setup_install_failed".to_owned()),
    };
    settle(root, id, outcome);
}

/// Records how one pack entry's install ended on the setup screen: a finished entry leaves
/// the list, a failed one shows the plain reason. Inputs: the engine root, the entry id and
/// the outcome code. Output: none.
fn settle(root: &Path, id: String, outcome: Result<(), String>) {
    progress::with(root, |state| match outcome {
        Ok(()) => {
            state.pack.remove(&id);
        }
        Err(code) => {
            state.pack.insert(
                id,
                PackState::Failed {
                    reason: crate::setup::pack_reason(&code),
                },
            );
        }
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

/// Runs the setup run's step on one tool: the tool the run is on while the step runs, its
/// install through [`install_dependency`] under the run's name in the engine's line.
/// Inputs: the installer, the engine root, the dependency and the engine's update state.
async fn run_step(
    installer: Arc<dyn Installer>,
    root: PathBuf,
    dep: Dependency,
    updates: Arc<Updates>,
) {
    progress::with(&root, |state| state.step = Some(dep));
    install_dependency(installer, &root, dep, updates, SETUP_JOB).await;
    progress::with(&root, |state| state.step = None);
}

async fn run_setup(
    sender: &mpsc::Sender<Job>,
    root: &Path,
    entries: Vec<PackManifestEntry>,
    updates: Arc<Updates>,
) {
    let installer = platform_installer(root);
    let graph_tool = progress::snapshot(root).graph_tool;
    for dep in Dependency::ALL {
        if dep == Dependency::Graphify && !graph_tool {
            continue;
        }
        run_step(installer.clone(), root.to_path_buf(), dep, updates.clone()).await;
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
    use crate::tools::permission_fix::{Report, TOOL_FIX_CHANGED, Target};
    use std::collections::{BTreeMap, BTreeSet};

    /// V243-SETUP-PARALLEL (DEC-199): a lone install or fix of one tool clears only that
    /// tool's earlier row, holds only that tool, and a second job on the same tool is
    /// refused as busy while one on another tool begins.
    #[test]
    fn one_dependency_job_clears_only_its_own_row_and_the_same_tool_twice_is_busy() {
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
        let guard = begin_one(root.clone(), Dependency::Node).unwrap();
        let during = progress::snapshot(&root);
        assert!(!during.running);
        assert_eq!(during.jobs, BTreeSet::from([Dependency::Node]));
        assert_eq!(during.dependencies.get(&Dependency::Node), None);
        assert_eq!(during.dependencies.get(&Dependency::Uv), Some(&installed));
        assert_eq!(during.pack.len(), 1);
        assert!(matches!(
            begin_one(root.clone(), Dependency::Node),
            Err(error) if error.0 == "setup_busy"
        ));
        let other = begin_one(root.clone(), Dependency::Uv).unwrap();
        assert_eq!(
            progress::snapshot(&root).dependencies.get(&Dependency::Uv),
            None
        );
        drop(guard);
        assert_eq!(
            progress::snapshot(&root).jobs,
            BTreeSet::from([Dependency::Uv])
        );
        drop(other);
        assert!(progress::snapshot(&root).jobs.is_empty());
    }

    struct TooOld(&'static str);

    impl Installer for TooOld {
        fn find(&self, _dep: Dependency) -> Result<Option<Version>, InstallError> {
            Ok(None)
        }

        fn lookup(&self, dep: Dependency) -> crate::setup::Lookup {
            Ok(Err(crate::tools::Missing {
                tool: dep.program().to_owned(),
                reason: crate::tools::Reason::WrongVersion,
                needs: Some("3.7".to_owned()),
                found: Some("3.2a".to_owned()),
                looked: vec![PathBuf::from("/usr/bin")],
                refused: None,
            }))
        }

        fn install(
            &self,
            _dep: Dependency,
            _pin: &crate::setup::Pin,
        ) -> Result<Version, InstallError> {
            Err(InstallError(self.0))
        }
    }

    /// V243-TMUX-APT-OLD: an install that leaves only a version too old to use, as Ubuntu
    /// 22.04's apt leaves tmux 3.2a, shows the row the check shows for it, naming the version
    /// found and the one needed, not that the program did not report its version. An install
    /// that failed for another reason keeps its own line.
    #[tokio::test]
    async fn an_install_that_leaves_a_too_old_version_names_found_and_needed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let unreadable = Arc::new(TooOld("setup_version_unreadable"));
        let updates = Updates::new();
        install_dependency(unreadable, &root, Dependency::Tmux, updates.clone(), "tmux").await;
        assert_eq!(
            progress::snapshot(&root).dependencies.get(&Dependency::Tmux),
            Some(&DependencyState::Missing {
                line: "tmux 3.2a is too old for CrazyAgent; it needs 3.7 or newer. Update it, then try again.".to_owned(),
            })
        );
        let failed = Arc::new(TooOld("setup_install_failed"));
        install_dependency(failed, &root, Dependency::Tmux, updates, "tmux").await;
        assert_eq!(
            progress::snapshot(&root)
                .dependencies
                .get(&Dependency::Tmux),
            Some(&DependencyState::Failed {
                reason: plain_reason("setup_install_failed").to_owned(),
            })
        );
    }

    struct FixFake(Result<Report, &'static str>);

    impl Installer for FixFake {
        fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
            Ok((dep != Dependency::Graphify).then(|| Version::parse("1.0.0").unwrap()))
        }

        fn install(
            &self,
            _dep: Dependency,
            _pin: &crate::setup::Pin,
        ) -> Result<Version, InstallError> {
            Err(InstallError("setup_install_failed"))
        }

        fn fix_target(&self, missing: &crate::tools::Missing) -> Option<Target> {
            (missing.tool == Dependency::Graphify.program()).then(|| Target {
                path: PathBuf::from("/h/t"),
                recursive: true,
            })
        }

        fn fix_permissions(
            &self,
            dep: Dependency,
            confirmed: &str,
            recursive: bool,
        ) -> Result<Report, &'static str> {
            assert_eq!(
                (dep, confirmed, recursive),
                (Dependency::Graphify, "/h/t", true)
            );
            self.0
        }
    }

    #[test]
    fn the_check_keeps_a_fix_target_only_for_a_tool_found_and_passed_over() {
        let (found, fixes) = look(&FixFake(Ok(Report::default())));
        assert_eq!(found.len(), Dependency::ALL.len());
        assert_eq!(fixes.len(), 1);
        assert_eq!(
            fixes
                .get(&Dependency::Graphify)
                .map(|target| target.path.as_path()),
            Some(Path::new("/h/t"))
        );
    }

    #[tokio::test]
    async fn a_permission_fix_names_only_its_own_failure_and_the_same_tool_twice_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        progress::with(&root, |state| {
            state
                .fix_failed
                .insert(Dependency::Graphify, "earlier".into());
            state.fix_failed.insert(Dependency::Uv, "other".into());
            state.checked = Some(progress::Checked {
                at: std::time::Instant::now(),
                found: Vec::new(),
                probe: crate::tools::probe::mark(),
            });
        });
        let guard = begin_one(root.clone(), Dependency::Graphify).unwrap();
        assert_eq!(
            progress::snapshot(&root)
                .fix_failed
                .get(&Dependency::Graphify),
            None
        );
        assert!(matches!(
            begin_one(root.clone(), Dependency::Graphify),
            Err(error) if error.0 == "setup_busy"
        ));
        let updates = Updates::new();
        let failing = Arc::new(FixFake(Err(TOOL_FIX_CHANGED)));
        fix_dependency(
            failing,
            &root,
            Dependency::Graphify,
            "/h/t".into(),
            true,
            updates.clone(),
        )
        .await;
        drop(guard);
        let after = progress::snapshot(&root);
        assert_eq!(
            after
                .fix_failed
                .get(&Dependency::Graphify)
                .map(String::as_str),
            Some(plain_reason(TOOL_FIX_CHANGED))
        );
        assert_eq!(
            after.fix_failed.get(&Dependency::Uv).map(String::as_str),
            Some("other")
        );
        assert!(after.checked.is_none());
        assert!(after.jobs.is_empty());
        let guard = begin_one(root.clone(), Dependency::Graphify).unwrap();
        let fixed = Arc::new(FixFake(Ok(Report {
            changed: 3,
            ..Report::default()
        })));
        fix_dependency(
            fixed,
            &root,
            Dependency::Graphify,
            "/h/t".into(),
            true,
            updates,
        )
        .await;
        drop(guard);
        assert_eq!(
            progress::snapshot(&root)
                .fix_failed
                .get(&Dependency::Graphify),
            None
        );
    }

    /// A tool installer for the rows on turns, keeping its files in one folder: a tool is
    /// found once `have-<program>` is there; its nth install notes "start <program> <n>" and
    /// "end <program> <n>" in the log around a wait for `gate-<program>-<n>`, the first one
    /// failing when `fail_first` is set; a fix notes "start fix <program>" and "end fix
    /// <program>" around a wait for `gate-fix-<program>`.
    #[cfg(unix)]
    struct Gated {
        dir: tempfile::TempDir,
        fail_first: bool,
        installs: std::sync::Mutex<BTreeMap<Dependency, u32>>,
    }

    #[cfg(unix)]
    impl Gated {
        fn new(fail_first: bool) -> Arc<Self> {
            Arc::new(Self {
                dir: tempfile::tempdir().unwrap(),
                fail_first,
                installs: std::sync::Mutex::default(),
            })
        }

        fn root(&self) -> PathBuf {
            self.dir.path().join("engine")
        }

        fn note(&self, line: &str) {
            use std::io::Write;
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.path().join("log"))
                .unwrap();
            writeln!(log, "{line}").unwrap();
        }

        fn logged(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
        }

        fn open(&self, gate: &str) {
            std::fs::write(self.dir.path().join(format!("gate-{gate}")), "go\n").unwrap();
        }

        fn wait(&self, gate: &str) {
            let file = self.dir.path().join(format!("gate-{gate}"));
            for _ in 0..WAIT_STEPS {
                if file.exists() {
                    return;
                }
                std::thread::sleep(WAIT_STEP);
            }
        }
    }

    #[cfg(unix)]
    impl Installer for Gated {
        fn find(&self, dep: Dependency) -> Result<Option<Version>, InstallError> {
            let have = self.dir.path().join(format!("have-{}", dep.program()));
            Ok(have.exists().then(|| Version::parse("1.0.0").unwrap()))
        }

        fn install(
            &self,
            dep: Dependency,
            _pin: &crate::setup::Pin,
        ) -> Result<Version, InstallError> {
            let n = {
                let mut installs = self.installs.lock().unwrap();
                let n = installs.entry(dep).or_default();
                *n += 1;
                *n
            };
            let program = dep.program();
            self.note(&format!("start {program} {n}"));
            self.wait(&format!("{program}-{n}"));
            self.note(&format!("end {program} {n}"));
            if self.fail_first && n == 1 {
                return Err(InstallError("setup_install_failed"));
            }
            Ok(Version::parse("1.0.0").unwrap())
        }

        fn fix_permissions(
            &self,
            dep: Dependency,
            _confirmed: &str,
            _recursive: bool,
        ) -> Result<Report, &'static str> {
            let program = dep.program();
            self.note(&format!("start fix {program}"));
            self.wait(&format!("fix-{program}"));
            self.note(&format!("end fix {program}"));
            Ok(Report::default())
        }
    }

    #[cfg(unix)]
    const WAIT_STEP: Duration = Duration::from_millis(25);
    #[cfg(unix)]
    const WAIT_STEPS: usize = 400;

    /// Waits for a condition the jobs under test reach on their own, looking every
    /// `WAIT_STEP` at most `WAIT_STEPS` times. Input: the condition. Output: whether it held.
    #[cfg(unix)]
    async fn eventually(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..WAIT_STEPS {
            if done() {
                return true;
            }
            tokio::time::sleep(WAIT_STEP).await;
        }
        false
    }

    /// One tool's row as the status read shows it. Inputs: the engine root, the update
    /// state and the tool. Output: the row.
    #[cfg(unix)]
    fn row(root: &Path, updates: &Updates, dep: Dependency) -> DependencyStatus {
        let mut rows = progress::dependency_rows(&progress::snapshot(root));
        overlay(&mut rows, root, updates.turns());
        rows.into_iter().find(|row| row.name == dep).unwrap()
    }

    /// V243-SETUP-PARALLEL (DEC-199, leader 14:16): the harness picker's Node install asked
    /// while the setup run is on its Node step is not refused: its row says it waits behind
    /// the setup run, and once the run's install ends, failed here, it installs Node itself.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_node_install_asked_during_the_runs_node_step_waits_its_turn_then_runs() {
        let case = Gated::new(true);
        let (root, updates) = (case.root(), Updates::new());
        assert!(begin_run(&root, &[]));
        let step = tokio::spawn(run_step(
            case.clone(),
            root.clone(),
            Dependency::Node,
            updates.clone(),
        ));
        assert!(eventually(|| case.logged() == "start node 1\n").await);
        start_install_with(
            case.clone(),
            root.clone(),
            Dependency::Node,
            updates.clone(),
        )
        .unwrap();
        let waits = || row(&root, &updates, Dependency::Node);
        assert!(
            eventually(|| waits().waiting.as_deref() == Some(SETUP_JOB)).await,
            "{:?}",
            waits()
        );
        assert!(waits().busy);
        case.open("node-2");
        assert_eq!(case.logged(), "start node 1\n", "the lone install waits");
        case.open("node-1");
        step.await.unwrap();
        assert!(eventually(|| progress::snapshot(&root).jobs.is_empty()).await);
        assert_eq!(
            case.logged(),
            "start node 1\nend node 1\nstart node 2\nend node 2\n"
        );
        let done = waits();
        assert!(
            matches!(done.state, DependencyState::Installed { .. }),
            "{done:?}"
        );
        assert!(!done.busy);
        assert_eq!(done.waiting, None);
    }

    /// V243-SETUP-PARALLEL (leader 14:16): a setup run keeps the rows of the tools it has
    /// not reached, a lone job begun during the run leaves the run's rows, its own tool's
    /// included, and with no run a lone job clears only its own row.
    #[test]
    fn a_run_keeps_the_rows_it_has_not_reached_and_a_lone_job_keeps_the_runs_rows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let failed = DependencyState::Failed {
            reason: "earlier".into(),
        };
        progress::with(&root, |state| {
            state.dependencies.insert(Dependency::Uv, failed.clone());
            state.dependencies.insert(Dependency::Node, failed.clone());
        });
        let lone = begin_one(root.clone(), Dependency::Node).unwrap();
        assert_eq!(
            progress::snapshot(&root)
                .dependencies
                .get(&Dependency::Node),
            None
        );
        progress::with(&root, |state| {
            state
                .dependencies
                .insert(Dependency::Node, DependencyState::Installing);
        });
        assert!(begin_run(&root, &[]), "a lone job does not hold the run");
        let during = progress::snapshot(&root);
        assert_eq!(
            during.dependencies.get(&Dependency::Node),
            Some(&DependencyState::Installing)
        );
        assert_eq!(during.dependencies.get(&Dependency::Uv), Some(&failed));
        let fix = begin_one(root.clone(), Dependency::Uv).unwrap();
        assert_eq!(
            progress::snapshot(&root).dependencies.get(&Dependency::Uv),
            Some(&failed)
        );
        assert!(!begin_run(&root, &[]), "one run at a time");
        drop((lone, fix));
        progress::with(&root, |state| state.running = false);
    }

    /// V243-SETUP-PARALLEL (DEC-199): permission fixes of two tools share no folder, so both
    /// run at once, while a second fix of a tool whose fix runs is still refused as busy.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn fixes_of_two_tools_run_together_and_the_same_tool_twice_is_busy() {
        let case = Gated::new(false);
        let (root, updates) = (case.root(), Updates::new());
        let fix = |dep: Dependency| {
            start_fix_with(
                case.clone(),
                root.clone(),
                dep,
                "/h/t".into(),
                true,
                updates.clone(),
            )
        };
        fix(Dependency::Uv).unwrap();
        fix(Dependency::Graphify).unwrap();
        let started = || {
            case.logged()
                .lines()
                .filter(|line| line.starts_with("start fix"))
                .count()
        };
        assert!(eventually(|| started() == 2).await, "{}", case.logged());
        assert!(matches!(
            fix(Dependency::Uv),
            Err(error) if error.0 == "setup_busy"
        ));
        for dep in [Dependency::Uv, Dependency::Graphify] {
            assert!(row(&root, &updates, dep).busy, "{dep:?}");
            assert_eq!(row(&root, &updates, dep).waiting, None);
        }
        case.open("fix-uv");
        case.open("fix-graphify");
        assert!(eventually(|| progress::snapshot(&root).jobs.is_empty()).await);
        assert!(!row(&root, &updates, Dependency::Uv).busy);
    }

    /// V243-SETUP-PARALLEL: the setup run's tmux step installs with Homebrew, so while a
    /// harness job holds a Homebrew prefix the step waits, its row naming that harness, and
    /// it installs by itself once the harness job ends.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_runs_tmux_step_waits_behind_a_harness_job_on_its_homebrew_prefix() {
        let case = Gated::new(false);
        let (root, updates) = (case.root(), Updates::new());
        let held = updates
            .turns()
            .take(
                "aider",
                vec![PathBuf::from(crate::harnesses::update::BREW_PREFIXES[0])],
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(begin_run(&root, &[]));
        let step = tokio::spawn(run_step(
            case.clone(),
            root.clone(),
            Dependency::Tmux,
            updates.clone(),
        ));
        let waits = || row(&root, &updates, Dependency::Tmux);
        assert!(
            eventually(|| waits().waiting.as_deref() == Some("Aider")).await,
            "{:?}",
            waits()
        );
        assert!(waits().busy);
        assert_eq!(case.logged(), "", "tmux waits for the harness job");
        drop(held);
        case.open("tmux-1");
        step.await.unwrap();
        assert_eq!(case.logged(), "start tmux 1\nend tmux 1\n");
        progress::with(&root, |state| state.running = false);
    }
}
