use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const FILE_BYTES_MAX: usize = 32 * 1024;
pub const FILES_BYTES_MAX: usize = 64 * 1024;
pub const FILES_MAX: usize = SOURCES.len();
const NAME_BYTES_MAX: usize = 32;
const HEADING: &str = "\n## Instruction files\n\nThe owner's instruction files, read by CrazyAgent from the project's committed files and, when the owner chose so, from the owner's own files. Follow them as you would follow these files in your harness.\n";
const FILE_HEADING: &str = "\n### \n\n";
pub const SECTION_BYTES_MAX: usize =
    HEADING.len() + FILES_MAX * (FILE_HEADING.len() + NAME_BYTES_MAX + 1) + FILES_BYTES_MAX;
const TABLE: &str = "agent_instruction_settings";

/// Where one instruction file comes from: committed in the owner's checkout at its HEAD,
/// the owner's checkout folder itself for a file git does not track, or the owner's home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Committed(&'static str),
    Checkout(&'static str),
    Home(&'static str),
}

const CODEX_OVERRIDE: &str = "AGENTS.override.md";
const HARNESS_FOLDERS: [&str; 2] = [".claude", ".codex"];

const SOURCES: [(&str, Source); 6] = [
    ("CLAUDE.md", Source::Committed("CLAUDE.md")),
    (".claude/CLAUDE.md", Source::Committed(".claude/CLAUDE.md")),
    ("AGENTS.md", Source::Committed("AGENTS.md")),
    ("CLAUDE.local.md", Source::Checkout("CLAUDE.local.md")),
    ("~/.claude/CLAUDE.md", Source::Home(".claude/CLAUDE.md")),
    ("~/.codex/AGENTS.md", Source::Home(".codex/AGENTS.md")),
];

const _: () = {
    let mut at = 0;
    while at < SOURCES.len() {
        assert!(SOURCES[at].0.len() <= NAME_BYTES_MAX);
        at += 1;
    }
    assert!(FILE_BYTES_MAX <= FILES_BYTES_MAX);
};

/// Which instruction files the engine hands a project's agents: the project's own files
/// (the default), those and the owner's own files, or none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    #[default]
    Project,
    ProjectAndUser,
    None,
}

impl Choice {
    /// Returns the stored and wire name of this choice.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::ProjectAndUser => "project_and_user",
            Self::None => "none",
        }
    }

    /// Reads a stored or wire name back. Input: the name. Output: the choice, or none.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Project, Self::ProjectAndUser, Self::None]
            .into_iter()
            .find(|choice| choice.as_str() == name)
    }
}

/// One project's agent instruction settings: which instruction files its agents get and
/// whether headless Claude runs keep their transcript. A project the owner never changed
/// gets the project's files and keeps transcripts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub files: Choice,
    pub transcripts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            files: Choice::Project,
            transcripts: true,
        }
    }
}

/// What became of one instruction file at a launch: given, left out because its text is
/// the same as a file given before it, over its bound, a link or other unsafe entry, not
/// plain text, or unreadable.
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

/// The instruction files one launch hands its agent: the prompt section holding every
/// given file, empty when none was given, and the record of each file found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delivery {
    pub text: String,
    pub records: Vec<Record>,
}

/// Tells whether a name is one this module records, so a record read back from an agent
/// home names only these files. Input: the name. Output: true for a known name.
pub fn known_name(name: &str) -> bool {
    SOURCES.iter().any(|(known, _)| *known == name)
}

/// Tells whether a changed project path is an instruction file agents are given or read: a
/// file named like a project source above or Codex's `AGENTS.override.md` in any folder, or
/// any path through a `.claude` or `.codex` folder, names compared without ASCII case since
/// macOS and Windows open `claude.md` as `CLAUDE.md`. Input: the path relative to the
/// project. Output: true for one.
pub fn instruction_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    let named = SOURCES
        .iter()
        .filter_map(|(_, source)| match source {
            Source::Committed(file) | Source::Checkout(file) => file.rsplit('/').next(),
            Source::Home(_) => None,
        })
        .chain([CODEX_OVERRIDE])
        .any(|known| known.eq_ignore_ascii_case(name));
    named
        || path.split('/').any(|part| {
            HARNESS_FOLDERS
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(part))
        })
}

/// Creates the settings table. Input: a connection to `engine.db`. Output: unit.
pub fn initialize(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_instruction_settings (
            project TEXT PRIMARY KEY,
            files TEXT NOT NULL,
            transcripts INTEGER NOT NULL
        );",
    )
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
            "SELECT files, transcripts FROM agent_instruction_settings WHERE project=?1",
            [project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((files, transcripts)) = stored else {
        return Ok(Settings::default());
    };
    let files = Choice::parse(&files).ok_or("instruction_settings_invalid")?;
    let transcripts = match transcripts {
        0 => false,
        1 => true,
        _ => return Err("instruction_settings_invalid"),
    };
    Ok(Settings { files, transcripts })
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
        "INSERT INTO agent_instruction_settings(project, files, transcripts) VALUES (?1, ?2, ?3)
         ON CONFLICT(project) DO UPDATE SET files=excluded.files, transcripts=excluded.transcripts",
        params![
            project.as_str(),
            settings.files.as_str(),
            i64::from(settings.transcripts)
        ],
    )?;
    assert_eq!(changed, 1);
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// The owner home a test stands in for the process's HOME, so a launch in that test
    /// reads that home's own instruction files. None keeps the process's HOME.
    pub(crate) static OWNER_HOME: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Finds the owner's home whose own instruction files "Project and user files" reads.
/// Takes nothing. Returns the home, or none when it is unknown.
pub(crate) fn owner_home() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(home) = OWNER_HOME.with(|home| home.borrow().clone()) {
        return Some(home);
    }
    crate::projects_home::owner_home().ok()
}

/// Reads the instruction files one launch hands its agent, never from the agent's own
/// worktree: the project's `CLAUDE.md`, `.claude/CLAUDE.md` and `AGENTS.md` as committed at
/// the owner's checkout's HEAD, its untracked `CLAUDE.local.md` from the checkout folder,
/// and for "Project and user files" the owner's `~/.claude/CLAUDE.md` and
/// `~/.codex/AGENTS.md`. Each file is at most `FILE_BYTES_MAX` bytes and all given files at
/// most `FILES_BYTES_MAX`; hidden characters are shown as visible escapes; a file that
/// cannot be given is recorded with the reason and never stops the launch.
/// Inputs: the owner's checkout folder, the owner's home and the choice.
/// Output: the section and the records.
pub fn read(checkout: Option<&Path>, home: Option<&Path>, choice: Choice) -> Delivery {
    let mut delivery = Delivery::default();
    let mut given: Vec<(&str, String)> = Vec::new();
    let mut total = 0;
    let mut committed_found = match (choice, checkout) {
        (Choice::None, _) | (_, None) => Vec::new(),
        (_, Some(checkout)) => committed(checkout),
    }
    .into_iter();
    for (name, source) in SOURCES {
        let found = match (source, choice, checkout, home) {
            (_, Choice::None, _, _) => continue,
            (Source::Committed(_), _, Some(_), _) => committed_found
                .next()
                .expect("one committed read per committed source"),
            (Source::Checkout(path), _, Some(checkout), _) => {
                plain(&checkout.join(path), crate::platform::open_plain)
            }
            (Source::Home(path), Choice::ProjectAndUser, _, Some(home)) => {
                plain(&home.join(path), open_following)
            }
            _ => continue,
        };
        let (bytes, state) = match found {
            Found::Missing => continue,
            Found::Refused(bytes, state) => (bytes, state),
            Found::Text(bytes, text) => {
                let text = crate::memory::visible_text(text.trim_end());
                if text.trim().is_empty() {
                    continue;
                }
                let state = if text.len() > FILE_BYTES_MAX || total + text.len() > FILES_BYTES_MAX {
                    State::TooLarge
                } else if given.iter().any(|(_, earlier)| *earlier == text) {
                    State::Same
                } else {
                    total += text.len();
                    given.push((name, text));
                    State::Given
                };
                (bytes, state)
            }
        };
        delivery.records.push(Record {
            name: name.to_owned(),
            bytes,
            state,
        });
    }
    if !given.is_empty() {
        delivery.text.push_str(HEADING);
        for (name, text) in &given {
            delivery.text.push_str("\n### ");
            delivery.text.push_str(name);
            delivery.text.push_str("\n\n");
            delivery.text.push_str(text);
            delivery.text.push('\n');
        }
    }
    assert!(committed_found.next().is_none());
    assert!(total <= FILES_BYTES_MAX);
    assert!(delivery.text.len() <= SECTION_BYTES_MAX);
    assert!(delivery.records.len() <= FILES_MAX);
    delivery
}

/// What reading one instruction file found: nothing there, a refusal with the file's size
/// and the reason, or its text with its size.
enum Found {
    Missing,
    Refused(u64, State),
    Text(u64, String),
}

/// Turns a file's bytes into its text when it is plain UTF-8 with no NUL and within its
/// bound. Inputs: the bytes, at most one past the bound, and whether git cut the blob.
/// Output: what was found.
fn text_of(bytes: Vec<u8>, size: u64, cut: bool) -> Found {
    if cut || bytes.len() > FILE_BYTES_MAX {
        return Found::Refused(size, State::TooLarge);
    }
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Found::Text(size, text),
        _ => Found::Refused(size, State::Invalid),
    }
}

/// Reads every committed source as committed at the owner's checkout's HEAD with one engine
/// git repository, which runs nothing the repository configures; a folder that is not a git
/// checkout has none of them. Input: the checkout folder. Output: what was found for each
/// committed source, in the order of `SOURCES`.
fn committed(checkout: &Path) -> Vec<Found> {
    let paths: Vec<&str> = SOURCES
        .iter()
        .filter_map(|(_, source)| match source {
            Source::Committed(path) => Some(*path),
            _ => None,
        })
        .collect();
    let refused = |error: &crate::engine::EngineError| match error.0.as_str() {
        "instruction_file_unsafe" => Found::Refused(0, State::Unsafe),
        _ => Found::Refused(0, State::Unreadable),
    };
    let not_git = matches!(
        std::fs::symlink_metadata(checkout.join(".git")),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound
    );
    let found: Vec<Found> = match not_git {
        true => paths.iter().map(|_| Found::Missing).collect(),
        false => match crate::landing::head_files(checkout, &paths, FILE_BYTES_MAX) {
            Err(error) => paths.iter().map(|_| refused(&error)).collect(),
            Ok(entries) => entries
                .into_iter()
                .map(|entry| match entry {
                    Ok(None) => Found::Missing,
                    Ok(Some(blob)) => text_of(blob.bytes, blob.size, blob.cut),
                    Err(error) => refused(&error),
                })
                .collect(),
        },
    };
    assert_eq!(found.len(), paths.len());
    found
}

/// Opens a file of the owner's own home: a link is followed, as the owner's own files often
/// are links into a dotfiles folder, and a FIFO is never waited on. Input: the path.
/// Output: the open file or the I/O error.
fn open_following(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    options.open(path)
}

/// Reads one file of the owner's checkout folder or home through the given open, which
/// must not wait on a FIFO, and only when the opened entry is a regular file. Inputs: the
/// path and the open. Output: what was found.
fn plain(path: &Path, open: fn(&Path) -> std::io::Result<std::fs::File>) -> Found {
    let file = match open(path) {
        Ok(file) => file,
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => return Found::Missing,
        Err(_) if std::fs::symlink_metadata(path).is_ok() => {
            return Found::Refused(0, State::Unsafe);
        }
        Err(_) => return Found::Refused(0, State::Unreadable),
    };
    let Ok(held) = file.metadata() else {
        return Found::Refused(0, State::Unreadable);
    };
    if !held.is_file() {
        return Found::Refused(0, State::Unsafe);
    }
    let mut bytes = Vec::new();
    if file
        .take(FILE_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Found::Refused(held.len(), State::Unreadable);
    }
    text_of(bytes, held.len(), false)
}

#[cfg(all(test, unix))]
#[path = "instruction_files/tests.rs"]
mod tests;
