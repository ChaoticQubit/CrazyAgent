use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE_BYTES_MAX: usize = 64 * 1024;
pub const FILES_BYTES_MAX: usize = 64 * 1024;
pub const FILES_MAX: usize = NAMES.len();
const NAME_BYTES_MAX: usize = 32;
const HEADING: &str = "\n## Instruction files\n\nThe owner's instruction files, read by CrazyAgent from the project's committed files and, when the owner chose so, from the owner's own files. Follow them as you would follow these files in your harness.\n";
const FILE_HEADING: &str = "\n### \n\n";
pub const SECTION_BYTES_MAX: usize =
    HEADING.len() + FILES_MAX * (FILE_HEADING.len() + NAME_BYTES_MAX + 1) + FILES_BYTES_MAX;
const TABLE: &str = "agent_instruction_settings";

/// The names an agent's instruction files record may hold: launches before DEC-106 read
/// these files and recorded them; a launch now records none.
const NAMES: [&str; 6] = [
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    "AGENTS.md",
    "CLAUDE.local.md",
    "~/.claude/CLAUDE.md",
    "~/.codex/AGENTS.md",
];

/// The project's own instruction files an import copies into the project instructions and
/// then moves aside, by their path inside the project folder.
pub const PROJECT_FILES: [&str; 4] = [
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    "AGENTS.md",
    "CLAUDE.local.md",
];
const IMPORTED_DIRECTORY: &str = "imported";
const MOVED_COPIES_MAX: u32 = 99;
const IMPORTS_TABLE: &str = "instruction_imports";

const _: () = {
    let mut at = 0;
    while at < NAMES.len() {
        assert!(NAMES[at].len() <= NAME_BYTES_MAX);
        at += 1;
    }
    assert!(FILE_BYTES_MAX <= FILES_BYTES_MAX);
    assert!(PROJECT_FILES.len() <= NAMES.len());
};

static IMPORTING: Mutex<()> = Mutex::new(());

/// Which of the owner's two instruction texts a project's agents get: the project's own,
/// the app-wide one, or both with the project's appended after the app's (the default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ProjectOnly,
    AppOnly,
    #[default]
    Both,
}

impl Mode {
    /// Returns the stored and wire name of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectOnly => "project_only",
            Self::AppOnly => "app_only",
            Self::Both => "both",
        }
    }

    /// Reads a stored or wire name back. Input: the name. Output: the mode, or none.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::ProjectOnly, Self::AppOnly, Self::Both]
            .into_iter()
            .find(|mode| mode.as_str() == name)
    }

    /// Tells whether agents in this mode get the app-wide instructions.
    pub fn app(self) -> bool {
        self != Self::ProjectOnly
    }

    /// Tells whether agents in this mode get the project instructions.
    pub fn project(self) -> bool {
        self != Self::AppOnly
    }
}

/// One project's agent instruction settings: which instruction texts its agents get and
/// whether headless Claude runs keep their transcript. A project the owner never changed
/// gets both texts and keeps transcripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub transcripts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Both,
            transcripts: true,
        }
    }
}

/// What became of one instruction file at a launch before DEC-106: given, left out because
/// its text was the same as a file given before it, over its bound, a link or other unsafe
/// entry, not plain text, or unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Given,
    Same,
    TooLarge,
    Unsafe,
    Invalid,
    Unreadable,
}

impl State {
    /// Returns the name the agent home record keeps.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Given => "given",
            Self::Same => "same",
            Self::TooLarge => "too_large",
            Self::Unsafe => "unsafe",
            Self::Invalid => "invalid",
            Self::Unreadable => "unreadable",
        }
    }

    /// Reads a recorded name back. Input: the name. Output: the state, or none.
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Given,
            Self::Same,
            Self::TooLarge,
            Self::Unsafe,
            Self::Invalid,
            Self::Unreadable,
        ]
        .into_iter()
        .find(|state| state.as_str() == name)
    }
}

/// The engine's own record of one instruction file at a launch: the name the owner knows
/// it by, its size in bytes and what became of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub name: String,
    pub bytes: u64,
    pub state: State,
}

/// One of the project's own instruction files an import found or moved: its path inside
/// the project folder with `/` separators, its size in bytes, whether the checkout's git
/// index tracks it, so moving it shows as a deletion, and the plain code of why it is not
/// imported, none when it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportFile {
    pub path: String,
    pub bytes: u64,
    pub tracked: bool,
    pub refused: Option<String>,
}

/// Tells whether a name is one an instruction files record may hold, so a record read back
/// from an agent home names only these files. Input: the name. Output: true for a known name.
pub fn known_name(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Creates the settings and import tables, and turns a settings table from before DEC-106,
/// whose `files` column held the removed instruction files choice, into the mode column
/// with every stored choice read as the default mode. Input: a connection to `engine.db`.
/// Output: unit.
pub fn initialize(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_instruction_settings (
            project TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            transcripts INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS instruction_imports (
            project TEXT PRIMARY KEY,
            moved TEXT NOT NULL
        );",
    )?;
    let old: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('agent_instruction_settings') WHERE name='files')",
        [],
        |row| row.get(0),
    )?;
    if old {
        connection.execute_batch(
            "ALTER TABLE agent_instruction_settings RENAME COLUMN files TO mode;
             UPDATE agent_instruction_settings SET mode='both'
              WHERE mode IN ('project', 'project_and_user', 'none');",
        )?;
    }
    Ok(())
}

/// Reads one project's settings; a project, or a database, that never stored any gets the
/// defaults. Inputs: a connection to `engine.db`, read-only is enough, and the project.
/// Output: the settings, or `instruction_settings_invalid` for a stored value outside the
/// options, or `instruction_settings_unavailable`.
pub fn load(connection: &Connection, project: &ProjectId) -> Result<Settings, &'static str> {
    let unavailable = |_| "instruction_settings_unavailable";
    let table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [TABLE],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !table {
        return Ok(Settings::default());
    }
    let stored: Option<(String, i64)> = connection
        .query_row(
            "SELECT mode, transcripts FROM agent_instruction_settings WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((mode, transcripts)) = stored else {
        return Ok(Settings::default());
    };
    let mode = Mode::parse(&mode).ok_or("instruction_settings_invalid")?;
    let transcripts = match transcripts {
        0 => false,
        1 => true,
        _ => return Err("instruction_settings_invalid"),
    };
    Ok(Settings { mode, transcripts })
}

/// Stores one project's settings whole, creating the table first when it is missing.
/// Inputs: a writable connection to `engine.db`, the project and the settings.
/// Output: unit.
pub fn save(
    connection: &Connection,
    project: &ProjectId,
    settings: Settings,
) -> rusqlite::Result<()> {
    initialize(connection)?;
    let changed = connection.execute(
        "INSERT INTO agent_instruction_settings(project, mode, transcripts) VALUES (?1, ?2, ?3)
         ON CONFLICT(project) DO UPDATE SET mode=excluded.mode, transcripts=excluded.transcripts",
        params![
            project.as_str(),
            settings.mode.as_str(),
            i64::from(settings.transcripts)
        ],
    )?;
    assert_eq!(changed, 1);
    Ok(())
}

/// Keeps what an import at a project's onboarding moved, so the onboarding view can name
/// the moved files to the owner. Inputs: the owner transaction, the project and the moved
/// files. Output: unit, or a plain code.
pub fn record_moved(
    connection: &Connection,
    project: &ProjectId,
    moved: &[ImportFile],
) -> Result<(), EngineError> {
    assert!(moved.len() <= PROJECT_FILES.len());
    let text = serde_json::to_string(moved).map_err(|_| failure("process_unavailable"))?;
    connection
        .execute(
            "INSERT INTO instruction_imports(project, moved) VALUES (?1, ?2)
             ON CONFLICT(project) DO UPDATE SET moved=excluded.moved",
            params![project.as_str(), text],
        )
        .map_err(|_| failure("process_unavailable"))?;
    Ok(())
}

/// Reads what an import at a project's onboarding moved, empty when it moved nothing.
/// Inputs: a connection to `engine.db` and the project. Output: the moved files, or a
/// plain code for a record the engine never writes.
pub fn moved(connection: &Connection, project: &ProjectId) -> Result<Vec<ImportFile>, EngineError> {
    let table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [IMPORTS_TABLE],
            |row| row.get(0),
        )
        .map_err(|_| failure("process_unavailable"))?;
    if !table {
        return Ok(Vec::new());
    }
    let stored: Option<String> = connection
        .query_row(
            "SELECT moved FROM instruction_imports WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure("process_unavailable"))?;
    let Some(stored) = stored else {
        return Ok(Vec::new());
    };
    let files: Vec<ImportFile> =
        serde_json::from_str(&stored).map_err(|_| failure("process_unavailable"))?;
    if files.len() > PROJECT_FILES.len() {
        return Err(failure("process_unavailable"));
    }
    Ok(files)
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// One project instruction file found for an import: what the owner is shown and, when
/// it can be imported, the file it resolves to and its text.
struct Found {
    file: ImportFile,
    read: Option<(PathBuf, String)>,
}

/// Lists the project's own instruction files an import would copy and move, without
/// changing anything: each with its size, whether git tracks it and why it would be left
/// in place, if so. Input: the project folder. Output: the files, empty when there are
/// none, or a plain code for a missing folder.
pub fn pending(folder: &Path) -> Result<Vec<ImportFile>, EngineError> {
    let root = fs::canonicalize(folder).map_err(|_| failure("process_project_folder_missing"))?;
    let found = find(&root);
    assert!(found.len() <= PROJECT_FILES.len());
    Ok(found.into_iter().map(|found| found.file).collect())
}

/// Copies the project's own instruction files into its project instructions, each text
/// once under a heading naming every file that held it, after the text already there and
/// never twice, then moves each copied file into `.crazyagent/imported/` at its same path.
/// A file that cannot be imported stays where it is. A project without `.crazyagent/`
/// gets it first. Input: the project folder. Output: the moved files, or a plain code
/// with nothing moved, such as `process_text_too_large` when the result would not fit.
pub fn import(folder: &Path) -> Result<Vec<ImportFile>, EngineError> {
    let _importing = IMPORTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = fs::canonicalize(folder).map_err(|_| failure("process_project_folder_missing"))?;
    let state = crate::projects_home::prepare_layout(&root)
        .map_err(|_| failure("process_folder_unsafe"))?;
    let found: Vec<Found> = find(&root)
        .into_iter()
        .filter(|found| found.read.is_some())
        .collect();
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let code = |problem: crate::agent_home::AgentHomeError| EngineError(problem.0);
    let existing = crate::agent_home::read_project(&root).map_err(code)?;
    let mut sections: Vec<(Vec<&str>, &Path, &str)> = Vec::new();
    for found in &found {
        let Some((target, text)) = &found.read else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match sections
            .iter_mut()
            .find(|(_, held, same)| *held == target.as_path() || *same == text)
        {
            Some((names, _, _)) => names.push(&found.file.path),
            None => sections.push((vec![found.file.path.as_str()], target, text)),
        }
    }
    let mut text = existing.clone();
    for (names, _, part) in &sections {
        if existing.contains(part) {
            continue;
        }
        if !text.is_empty() {
            text.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
        }
        text.push_str("## Imported from ");
        text.push_str(&names.join(", "));
        text.push_str("\n\n");
        text.push_str(part);
        text.push('\n');
    }
    if text.len() > crate::agent_home::TEXT_BYTES_MAX {
        return Err(failure("process_text_too_large"));
    }
    if text != existing {
        crate::agent_home::save_project(&root, &text).map_err(code)?;
    }
    let mut moved = Vec::new();
    for found in found {
        move_aside(&root, &state, &found.file.path)?;
        moved.push(found.file);
    }
    assert!(moved.len() <= PROJECT_FILES.len());
    Ok(moved)
}

/// Reads one file the owner picked for the app-wide instructions editor; a link is
/// followed to the file it names. Input: the absolute path. Output: the text, or
/// `process_import_missing`, `process_import_unsafe` for anything but a regular file the
/// owner owns, or another plain code.
pub fn read_picked(path: &Path) -> Result<String, EngineError> {
    if !path.is_absolute() {
        return Err(failure("process_request_invalid"));
    }
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
            return Err(failure("process_import_missing"));
        }
        Err(_) => return Err(failure("process_import_unreadable")),
    };
    owned_text(&target).map_err(failure)
}

/// Finds the project's own instruction files and checks each: a file is imported only
/// when it, or the file a link names, is a regular file the owner owns inside the project
/// folder, outside `.crazyagent/`. Input: the canonical project folder. Output: what was
/// found, in the order of `PROJECT_FILES`.
fn find(root: &Path) -> Vec<Found> {
    let present: Vec<&str> = PROJECT_FILES
        .into_iter()
        .filter(|path| fs::symlink_metadata(root.join(path)).is_ok())
        .collect();
    if present.is_empty() {
        return Vec::new();
    }
    let tracked = match fs::symlink_metadata(root.join(".git")) {
        Ok(metadata) if metadata.is_dir() => crate::landing::tracked_files(root, &present)
            .unwrap_or_else(|_| vec![false; present.len()]),
        _ => vec![false; present.len()],
    };
    assert_eq!(tracked.len(), present.len());
    present
        .into_iter()
        .zip(tracked)
        .map(|(path, tracked)| {
            let named = root.join(path);
            let read = fs::canonicalize(&named)
                .map_err(|_| "process_import_unreadable")
                .and_then(|target| {
                    let inside = target.starts_with(root)
                        && !target
                            .starts_with(root.join(crate::projects_home::CRAZYAGENT_DIRECTORY));
                    match inside {
                        true => owned_text(&target).map(|text| (target, text)),
                        false => Err("process_import_outside"),
                    }
                });
            let bytes = match &read {
                Ok((_, text)) => text.len() as u64,
                Err(_) => fs::symlink_metadata(&named).map_or(0, |metadata| metadata.len()),
            };
            let (read, refused) = match read {
                Ok(read) => (Some(read), None),
                Err(code) => (None, Some(code.to_owned())),
            };
            Found {
                file: ImportFile {
                    path: path.to_owned(),
                    bytes,
                    tracked,
                    refused,
                },
                read,
            }
        })
        .collect()
}

/// Reads a file at a path with no link in it as owner text: a regular file the owner owns,
/// at most `agent_home::TEXT_BYTES_MAX` bytes of UTF-8 with no NUL, opened without
/// following a link or waiting on a FIFO. Input: the canonical path. Output: the text, or
/// a plain code.
fn owned_text(target: &Path) -> Result<String, &'static str> {
    let file = crate::platform::open_plain(target).map_err(|_| "process_import_unreadable")?;
    let held = file.metadata().map_err(|_| "process_import_unreadable")?;
    if !held.is_file() || !owned(&held) {
        return Err("process_import_unsafe");
    }
    if held.len() > crate::agent_home::TEXT_BYTES_MAX as u64 {
        return Err("process_text_too_large");
    }
    let mut bytes = Vec::new();
    file.take(crate::agent_home::TEXT_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "process_import_unreadable")?;
    if bytes.len() > crate::agent_home::TEXT_BYTES_MAX {
        return Err("process_text_too_large");
    }
    let text = String::from_utf8(bytes).map_err(|_| "process_text_invalid")?;
    if text.contains('\0') {
        return Err("process_text_invalid");
    }
    assert!(text.len() <= crate::agent_home::TEXT_BYTES_MAX);
    Ok(text)
}

#[cfg(unix)]
fn owned(metadata: &fs::Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned(_metadata: &fs::Metadata) -> bool {
    true
}

/// Moves one project instruction file, or the link at its path, to the same path under
/// `.crazyagent/imported/`, beside an earlier move as `<path>.2` and so on rather than over
/// it. Inputs: the canonical project folder, its `.crazyagent/` folder and the file's path
/// inside the project. Output: unit, or `process_import_unmovable`.
fn move_aside(root: &Path, state: &Path, path: &str) -> Result<(), EngineError> {
    assert!(PROJECT_FILES.contains(&path));
    let unmovable = || failure("process_import_unmovable");
    let mut folder = state.join(IMPORTED_DIRECTORY);
    real_folder(&folder).map_err(|_| unmovable())?;
    let relative = Path::new(path);
    if let Some(parent) = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        folder = folder.join(parent);
        real_folder(&folder).map_err(|_| unmovable())?;
    }
    let name = relative
        .file_name()
        .ok_or_else(unmovable)?
        .to_string_lossy()
        .into_owned();
    let destination = (1..=MOVED_COPIES_MAX)
        .map(|copy| match copy {
            1 => folder.join(&name),
            _ => folder.join(format!("{name}.{copy}")),
        })
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
        .ok_or_else(unmovable)?;
    fs::rename(root.join(path), &destination).map_err(|_| unmovable())?;
    assert!(fs::symlink_metadata(root.join(path)).is_err());
    Ok(())
}

/// Makes a folder that is a real folder, not a link, creating it when it is missing.
/// Input: the path. Output: unit, or an error for anything else at the path.
fn real_folder(folder: &Path) -> std::io::Result<()> {
    match fs::create_dir(folder) {
        Err(problem) if problem.kind() != std::io::ErrorKind::AlreadyExists => return Err(problem),
        _ => {}
    }
    let metadata = fs::symlink_metadata(folder)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "instruction_files/tests.rs"]
mod tests;
