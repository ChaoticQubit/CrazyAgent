use super::pack::PackManifest;
use super::{
    Dependency, DependencyState, DependencyStatus, InstallError, PackEntry, PackState, Version,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// What one engine's setup run is doing now: whether it runs, the dependencies it touched
/// with their states, and the pack entries still installing or failed with a reason. It
/// also keeps the last background version check, so a status read never waits on one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub running: bool,
    pub dependencies: BTreeMap<Dependency, DependencyState>,
    pub pack: BTreeMap<String, PackState>,
    pub checking: bool,
    pub checked: Option<Checked>,
}

/// What the last background version check found for each dependency, and when it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub at: Instant,
    pub found: Vec<(Dependency, Result<Option<Version>, InstallError>)>,
}

static PROGRESS: Mutex<BTreeMap<PathBuf, Progress>> = Mutex::new(BTreeMap::new());

fn key(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Reads or changes the setup progress of the engine whose root folder is given.
/// Inputs: the engine root and the change. Output: what the change returns.
pub fn with<T>(root: &Path, change: impl FnOnce(&mut Progress) -> T) -> T {
    let mut all = PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    change(all.entry(key(root)).or_default())
}

/// Returns a copy of one engine's setup progress. Input: the engine root.
pub fn snapshot(root: &Path) -> Progress {
    with(root, |progress| progress.clone())
}

/// Lists each dependency for the setup screen: the state a run gave it, else what the
/// last version check found, else checking while no check has finished yet.
/// Input: the progress. Output: one row per dependency in install order.
pub fn dependency_rows(progress: &Progress) -> Vec<DependencyStatus> {
    let found = progress
        .checked
        .as_ref()
        .map(|checked| checked.found.as_slice());
    let rows: Vec<DependencyStatus> = Dependency::ALL
        .iter()
        .map(|dep| {
            let seen =
                found.map(|found| found.iter().find(|(name, _)| name == dep).map(|(_, f)| f));
            let state = match (progress.dependencies.get(dep), seen) {
                (Some(step), _) => step.clone(),
                (None, None) => DependencyState::Checking,
                (None, Some(Some(Ok(Some(version))))) => DependencyState::Found {
                    version: version.clone(),
                },
                (None, Some(Some(Err(code)))) => DependencyState::Failed {
                    reason: super::plain_reason(code.0).to_owned(),
                },
                (None, Some(Some(Ok(None)) | None)) => DependencyState::Missing,
            };
            DependencyStatus {
                name: *dep,
                label: dep.label().to_owned(),
                state,
            }
        })
        .collect();
    assert_eq!(rows.len(), Dependency::ALL.len());
    rows
}

/// Lists each default-pack entry for the setup screen: installing while a run works on
/// it, on or off once installed, the failure a run met, else available. Inputs: the
/// manifest, the installed entries with their on flags and the progress. Output: one row
/// per manifest entry in manifest order.
pub fn pack_rows(
    manifest: &PackManifest,
    installed: &[(String, bool)],
    progress: &Progress,
) -> Vec<PackEntry> {
    let rows: Vec<PackEntry> = manifest
        .entries
        .iter()
        .map(|entry| {
            let row = installed.iter().find(|(id, _)| *id == entry.id);
            let state = match (progress.pack.get(&entry.id), row) {
                (Some(PackState::Installing), _) => PackState::Installing,
                (_, Some((_, true))) => PackState::On,
                (_, Some((_, false))) => PackState::Off,
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
    assert_eq!(rows.len(), manifest.entries.len());
    rows
}

/// The plain place an entry comes from, such as `github.com/obra/superpowers`, or
/// "Bundled with CrazyAgent". Input: the entry source. Output: the label.
pub fn source_label(source: &super::pack::PackSource) -> String {
    match source {
        super::pack::PackSource::Git { repo, .. } => repo
            .trim_start_matches("https://")
            .trim_end_matches(".git")
            .to_owned(),
        super::pack::PackSource::Bundled { .. } => "Bundled with CrazyAgent".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_system_setup_cannot_install_on_shows_its_own_plain_reason() {
        let unsupported = InstallError(super::super::PLATFORM_UNSUPPORTED);
        let progress = Progress {
            checked: Some(Checked {
                at: Instant::now(),
                found: Dependency::ALL.map(|dep| (dep, Err(unsupported))).to_vec(),
            }),
            ..Progress::default()
        };
        let rows = dependency_rows(&progress);
        assert_eq!(rows.len(), Dependency::ALL.len());
        for row in rows {
            let DependencyState::Failed { reason } = row.state else {
                panic!("{:?} is not failed", row.name);
            };
            assert_ne!(reason, super::super::plain_reason("setup_code_nobody_uses"));
            assert!(reason.contains("this system"), "{reason}");
        }
    }
}
