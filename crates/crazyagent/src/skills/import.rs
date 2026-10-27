use super::{SkillInfo, one_line};
use crate::engine::EngineError;
use crate::plugins::skill_folder::{self, IMPORTED, PlannedFile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const LIST_BYTES_MAX: usize = 48 * 1024;
pub const FOLDER_NAME_BYTES_MAX: usize = 255;
const ROOT_ENTRIES_MAX: usize = 1024;
const DIGEST_ENTRIES_MAX: usize = 4096;

/// Where a found skill lives: the owner's Claude Code or Codex skills folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFrom {
    Claude,
    Codex,
}

impl ImportFrom {
    /// The skills folder under the owner's home. Output: `.claude/skills` or `.codex/skills`.
    pub fn folder(self) -> &'static str {
        match self {
            Self::Claude => ".claude/skills",
            Self::Codex => ".codex/skills",
        }
    }

    /// The name the app shows for this folder. Output: "Claude Code" or "Codex".
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// How a found skill compares with the app's skills: not there yet, already added with the
/// same files, added under the same name with different files, or not importable and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportState {
    New,
    Added { id: String },
    Differs { id: String },
    Unusable { code: String },
}

/// One skill found in the owner's skills folders: where it is, its name and one-line
/// description from SKILL.md (the folder name and no line when it cannot be imported) and
/// its state against the app's skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportCandidate {
    pub from: ImportFrom,
    pub folder: String,
    pub name: String,
    pub line: String,
    pub state: ImportState,
}

/// What a scan found: the skills that fit one reply, in folder order, and how many more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportList {
    pub found: Vec<ImportCandidate>,
    pub more: u32,
}

/// One imported skill as the app's store now holds it, with the paths left out of its copy
/// (at most `skill_folder::SKIPPED_LISTED_MAX`) and how many were left out in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Imported {
    pub skill: SkillInfo,
    pub skipped: Vec<String>,
    pub skipped_total: u32,
}

fn error(code: &str) -> EngineError {
    EngineError(code.into())
}

fn folder_names(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .take(ROOT_ENTRIES_MAX)
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            !name.starts_with('.')
                && name.len() <= FOLDER_NAME_BYTES_MAX
                && !name.chars().any(char::is_control)
        })
        .collect();
    names.sort();
    names
}

fn digest(folder: &Path) -> Option<[u8; 32]> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).ok()? {
            entries += 1;
            if entries > DIGEST_ENTRIES_MAX {
                return None;
            }
            let path = entry.ok()?.path();
            let kind = std::fs::symlink_metadata(&path).ok()?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let relative = path.strip_prefix(folder).ok()?.to_str()?.to_owned();
                files.push((relative, path));
            } else {
                return None;
            }
        }
    }
    files.sort();
    let mut hash = Sha256::new();
    let mut read = 0u64;
    for (relative, path) in files {
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .ok()?
            .take(skill_folder::BYTES_MAX + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        read += bytes.len() as u64;
        if read > skill_folder::BYTES_MAX {
            return None;
        }
        feed(&mut hash, &relative, &bytes);
    }
    Some(hash.finalize().into())
}

fn feed(hash: &mut Sha256, relative: &str, bytes: &[u8]) {
    hash.update(relative.as_bytes());
    hash.update([0]);
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn planned_digest(planned: &[PlannedFile]) -> Option<[u8; 32]> {
    let mut files: Vec<(&str, &PlannedFile)> = planned
        .iter()
        .map(|file| Some((file.relative.to_str()?, file)))
        .collect::<Option<_>>()?;
    files.sort_by(|left, right| left.0.cmp(right.0));
    let mut hash = Sha256::new();
    for (relative, file) in files {
        feed(&mut hash, relative, &skill_folder::read_planned(file).ok()?);
    }
    Some(hash.finalize().into())
}

fn candidate(
    from: ImportFrom,
    folder: &Path,
    name: &str,
    added: &[SkillInfo],
) -> Result<Option<ImportCandidate>, EngineError> {
    let shown = folder
        .to_str()
        .ok_or_else(|| error("skill_import_home_unknown"))?
        .to_owned();
    Ok(match skill_folder::check_with(folder, &IMPORTED) {
        Err("skill_missing_skill_md") => None,
        Err(code) => Some(ImportCandidate {
            from,
            folder: shown,
            name: name.to_owned(),
            line: String::new(),
            state: ImportState::Unusable { code: code.into() },
        }),
        Ok((meta, planned)) => {
            let state = match added.iter().find(|skill| skill.name == meta.name) {
                None => ImportState::New,
                Some(skill) => {
                    let same = planned_digest(&planned)
                        .is_some_and(|checked| digest(Path::new(&skill.folder)) == Some(checked));
                    if same {
                        ImportState::Added {
                            id: skill.id.clone(),
                        }
                    } else {
                        ImportState::Differs {
                            id: skill.id.clone(),
                        }
                    }
                }
            };
            Some(ImportCandidate {
                from,
                folder: shown,
                line: one_line(&meta.description),
                name: meta.name,
                state,
            })
        }
    })
}

/// Finds the skills in the owner's `~/.claude/skills` and `~/.codex/skills` folders: each
/// folder holding SKILL.md, skipping dot folders. Each one is checked in place under the
/// import rules, so the state says exactly what an import would do; nothing is written and
/// the source folders are only read. Inputs: the owner's home and the app's skills. Output:
/// what fits one reply, and the rest as a count.
pub fn scan(home: &Path, added: &[SkillInfo]) -> Result<ImportList, EngineError> {
    if !home.is_absolute() || home.to_str().is_none() {
        return Err(error("skill_import_home_unknown"));
    }
    let mut all = Vec::new();
    for from in [ImportFrom::Claude, ImportFrom::Codex] {
        let root = home.join(from.folder());
        for name in folder_names(&root) {
            let folder = root.join(&name);
            if !std::fs::metadata(&folder).is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if let Some(found) = candidate(from, &folder, &name, added)? {
                all.push(found);
            }
        }
    }
    let mut cost = 0;
    let mut kept = 0;
    for found in &all {
        let size = serde_json::to_vec(found)
            .map_err(|_| error("plugins_failed"))?
            .len()
            + 1;
        if cost + size > LIST_BYTES_MAX {
            break;
        }
        cost += size;
        kept += 1;
    }
    let more = u32::try_from(all.len() - kept).map_err(|_| error("skill_too_many_files"))?;
    all.truncate(kept);
    assert!(cost <= LIST_BYTES_MAX);
    Ok(ImportList { found: all, more })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn skill(folder: &Path, name: &str, description: &str) {
        fs::create_dir_all(folder.join("references")).unwrap();
        fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nSteps.\n"),
        )
        .unwrap();
        fs::write(folder.join("references/guide.md"), "Guide.\n").unwrap();
    }

    fn stored(store: &Path, source: &Path, id: &str) -> SkillInfo {
        let folder = store.join(id);
        let meta = skill_folder::copy_with(source, &folder, &IMPORTED).unwrap();
        SkillInfo {
            id: id.into(),
            name: meta.name,
            description: meta.description.clone(),
            line: one_line(&meta.description),
            scope: crate::plugins::Scope::Global,
            scope_label: "For every project".into(),
            enabled: true,
            on_here: None,
            source: source.display().to_string(),
            folder: folder.display().to_string(),
        }
    }

    fn tree(folder: &Path) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(folder).unwrap().display().to_string();
                let kind = fs::symlink_metadata(&path).unwrap();
                if kind.is_dir() {
                    stack.push(path);
                    found.push((relative, Vec::new()));
                } else if kind.file_type().is_symlink() {
                    let target = fs::read_link(&path).unwrap().display().to_string();
                    found.push((relative, target.into_bytes()));
                } else {
                    found.push((relative, fs::read(&path).unwrap()));
                }
            }
        }
        found.sort();
        found
    }

    fn private_folder(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_scan_lists_both_skills_folders_with_each_state_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let store = root.path().join("store");
        private_folder(&store);
        let claude = home.join(".claude/skills");
        let codex = home.join(".codex/skills");
        skill(
            &claude.join("notes"),
            "notes",
            "Writes release notes. Use after a merge.",
        );
        fs::write(claude.join("notes/.env"), "TOKEN=dummy").unwrap();
        skill(&claude.join("heavy"), "heavy", "Too big.");
        fs::write(
            claude.join("heavy/big.bin"),
            vec![0u8; skill_folder::IMPORT_BYTES_MAX as usize + 1],
        )
        .unwrap();
        skill(&claude.join("leaky"), "leaky", "Links out.");
        fs::write(root.path().join("outside.md"), "secret").unwrap();
        symlink(root.path().join("outside.md"), claude.join("leaky/leak.md")).unwrap();
        fs::create_dir_all(claude.join("not-a-skill")).unwrap();
        fs::write(claude.join("not-a-skill/README.md"), "No skill.").unwrap();
        skill(&claude.join(".hidden"), "hidden", "Dot folder.");
        fs::write(claude.join("loose.md"), "A file, not a folder.").unwrap();
        skill(&root.path().join("linked"), "linked", "Linked folder.");
        symlink(root.path().join("linked"), claude.join("linked")).unwrap();
        skill(&codex.join("graph"), "graph", "Builds a code graph.");
        skill(&codex.join(".system/builtin"), "builtin", "Codex's own.");
        skill(&codex.join("fresh"), "fresh", "Only in Codex.");

        let kept = root.path().join("kept-graph");
        skill(&kept, "graph", "Builds a code graph.");
        fs::write(kept.join("references/guide.md"), "Older guide.\n").unwrap();
        let added = vec![
            stored(&store, &claude.join("notes"), "skill-notes"),
            stored(&store, &kept, "skill-graph"),
        ];
        let before = tree(&home);
        let everything = tree(root.path());

        let list = scan(&home, &added).unwrap();
        let rows: Vec<(ImportFrom, &str, &str, &ImportState)> = list
            .found
            .iter()
            .map(|found| {
                (
                    found.from,
                    found.name.as_str(),
                    found.line.as_str(),
                    &found.state,
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    ImportFrom::Claude,
                    "heavy",
                    "",
                    &ImportState::Unusable {
                        code: "skill_import_too_large".into()
                    }
                ),
                (
                    ImportFrom::Claude,
                    "leaky",
                    "",
                    &ImportState::Unusable {
                        code: "skill_link_outside".into()
                    }
                ),
                (
                    ImportFrom::Claude,
                    "linked",
                    "Linked folder.",
                    &ImportState::New
                ),
                (
                    ImportFrom::Claude,
                    "notes",
                    "Writes release notes.",
                    &ImportState::Added {
                        id: "skill-notes".into()
                    }
                ),
                (
                    ImportFrom::Codex,
                    "fresh",
                    "Only in Codex.",
                    &ImportState::New
                ),
                (
                    ImportFrom::Codex,
                    "graph",
                    "Builds a code graph.",
                    &ImportState::Differs {
                        id: "skill-graph".into()
                    }
                ),
            ]
        );
        assert_eq!(
            list.found[3].folder,
            claude.join("notes").display().to_string()
        );
        assert_eq!(list.more, 0);
        assert_eq!(tree(&home), before, "the owner's folders are only read");
        assert_eq!(
            tree(root.path()),
            everything,
            "a scan writes nothing anywhere"
        );

        let empty = root.path().join("empty-home");
        fs::create_dir(&empty).unwrap();
        assert_eq!(scan(&empty, &added).unwrap().found, vec![]);
        assert_eq!(
            scan(Path::new("relative"), &added).unwrap_err().0,
            "skill_import_home_unknown"
        );
    }

    #[test]
    fn a_scan_with_more_skills_than_one_reply_counts_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let claude = home.join(".claude/skills");
        let description = "d".repeat(900);
        for index in 0..200 {
            let name = format!("skill-{index:03}");
            skill(&claude.join(&name), &name, &description);
        }
        let list = scan(&home, &[]).unwrap();
        assert!(!list.found.is_empty());
        assert_eq!(list.found.len() + list.more as usize, 200);
        assert!(list.more > 0);
        let size: usize = list
            .found
            .iter()
            .map(|found| serde_json::to_vec(found).unwrap().len() + 1)
            .sum();
        assert!(size <= LIST_BYTES_MAX);
        assert_eq!(list.found[0].name, "skill-000");
    }
}
