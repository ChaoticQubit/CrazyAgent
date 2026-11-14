use super::import::{self, ImportFrom};
use crate::setup::pack::{ENTRIES_MAX, PackManifestEntry, PackSource, default_pack, repo_key};
use std::path::{Component, Path};
use std::sync::OnceLock;

/// The label of a skill that came from neither the default pack nor an import folder.
pub const ADDED_BY_YOU: &str = "Added by you";
const SETUP_FOLDER: &str = "setup";
const PACK_FOLDER: &str = "pack";
const BUNDLED_FOLDER: &str = "bundled";

/// Names where a stored skill came from for its row, against this engine's setup folder,
/// the owner's home as imports read it and the default pack this build ships. Inputs: the
/// engine root and the skill's stored source folder. Output: the label; it never fails.
pub fn of(root: &Path, source: &str) -> String {
    label(
        &root.join(SETUP_FOLDER),
        import::home().as_deref(),
        pack_entries(),
        source,
    )
}

/// Names where a skill came from by the folder it was added from: "Default pack: <entry id>"
/// inside a pack entry's checkout or bundled folder, "Imported from Claude Code" or "Imported
/// from Codex" inside that harness's skills folder, else "Added by you". Inputs: the setup
/// folder, the owner's home when known, the pack entries and the source. Output: the label.
pub fn label(
    setup: &Path,
    home: Option<&Path>,
    entries: &[PackManifestEntry],
    source: &str,
) -> String {
    let source = Path::new(source);
    if source
        .components()
        .any(|part| part == Component::ParentDir)
    {
        return ADDED_BY_YOU.into();
    }
    if let Some(entry) = pack_entry(setup, entries, source) {
        assert!(!entry.id.is_empty(), "pack entry ids are checked at parse");
        return format!("Default pack: {}", entry.id);
    }
    let imported = home.and_then(|home| {
        ImportFrom::ALL
            .into_iter()
            .find(|from| source.starts_with(home.join(from.folder())))
    });
    match imported {
        Some(from) => format!("Imported from {}", from.label()),
        None => ADDED_BY_YOU.into(),
    }
}

/// The default pack compiled into this build, parsed once per process. Input: none. Output:
/// its entries, or none when the shipped manifest does not parse, so labels then never name
/// the pack.
pub fn pack_entries() -> &'static [PackManifestEntry] {
    static ENTRIES: OnceLock<Vec<PackManifestEntry>> = OnceLock::new();
    let entries =
        ENTRIES.get_or_init(|| default_pack().map(|pack| pack.entries).unwrap_or_default());
    assert!(entries.len() <= ENTRIES_MAX);
    entries
}

/// Picks the pack entry whose folder holds the source, taking the longest entry path when
/// several do, so an entry inside another entry's folder keeps its own skills. Inputs: the
/// setup folder, the entries and the source. Output: the entry, or none.
fn pack_entry<'a>(
    setup: &Path,
    entries: &'a [PackManifestEntry],
    source: &Path,
) -> Option<&'a PackManifestEntry> {
    assert!(entries.len() <= ENTRIES_MAX);
    let mut best: Option<(&PackManifestEntry, usize)> = None;
    for entry in entries {
        let Some(depth) = depth_in(setup, entry, source) else {
            continue;
        };
        if best.is_none_or(|(_, kept)| depth > kept) {
            best = Some((entry, depth));
        }
    }
    best.map(|(entry, _)| entry)
}

/// Says how deep an entry's folder is when it holds the source: the parts of its entry path,
/// 0 for a whole checkout. A git checkout matches `<repo key>-<any commit>`, so a skill added
/// before a pin bump keeps its entry; paths match whole folders. Inputs: the setup folder, the
/// entry and the source. Output: the depth, or none when the folder does not hold the source.
fn depth_in(setup: &Path, entry: &PackManifestEntry, source: &Path) -> Option<usize> {
    let (inside, path) = match &entry.source {
        PackSource::Git { repo, path, .. } => {
            let mut parts = source
                .strip_prefix(setup.join(PACK_FOLDER))
                .ok()?
                .components();
            let Some(Component::Normal(checkout)) = parts.next() else {
                return None;
            };
            let prefix = format!("{}-", repo_key(repo));
            let commit = checkout.to_str()?.strip_prefix(prefix.as_str())?;
            if commit.is_empty() {
                return None;
            }
            (parts.as_path(), path.as_deref().unwrap_or(""))
        }
        PackSource::Bundled { path } => (
            source.strip_prefix(setup.join(BUNDLED_FOLDER)).ok()?,
            path.as_str(),
        ),
    };
    let path = Path::new(path);
    inside
        .starts_with(path)
        .then(|| path.components().count())
}

#[cfg(test)]
#[path = "origin_tests.rs"]
mod tests;
