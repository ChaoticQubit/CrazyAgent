pub use crate::agent_home::Repeated;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

pub const FILE_BYTES_MAX: usize = 64 * 1024;
pub const FILES_BYTES_MAX: usize = 64 * 1024;
pub const FILES_MAX: usize = NAMES.len();
const NAME_BYTES_MAX: usize = 32;
#[cfg(unix)]
const IMPORTS_MAX: usize = 64;
const HEADING: &str = "\n## Instruction files\n\nThe owner's instruction files, read by CrazyAgent from the project's committed files and, when the owner chose so, from the owner's own files. Follow them as you would follow these files in your harness.\n";
const FILE_HEADING: &str = "\n### \n\n";
pub const SECTION_BYTES_MAX: usize =
    HEADING.len() + FILES_MAX * (FILE_HEADING.len() + NAME_BYTES_MAX + 1) + FILES_BYTES_MAX;
const TABLE: &str = "agent_instruction_settings";
const BADGES_TABLE: &str = "team_instruction_badges";
const SECTION_REMOVED: &str = "This section was removed.";
const PREAMBLE_REMOVED: &str = "The text before the first section was removed.";
const SECTIONS_CUT: &str =
    "More sections changed than one message carries; they apply in full when you next start.";
const SECTION_SEPARATOR: &str = "\n\n";
/// The words a running agent's next task starts with after the owner edited its
/// instructions, before the changed sections.
pub const CHANGED_HEADER: &str =
    "Your instructions changed since you started. The changed sections now read:";

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

const CODEX_OVERRIDE: &str = "AGENTS.override.md";
const HARNESS_FOLDERS: [&str; 2] = [".claude", ".codex"];
const HFS_IGNORED: [char; 16] = [
    '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
    '\u{202E}', '\u{206A}', '\u{206B}', '\u{206C}', '\u{206D}', '\u{206E}', '\u{206F}', '\u{FEFF}',
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
const HEADS_MAX: usize = 256;
static HEADS: Mutex<BTreeMap<PathBuf, (HeadStamp, Option<String>)>> = Mutex::new(BTreeMap::new());

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

/// How an edit to the owner's instruction texts reaches a project's agents that already
/// run (DEC-111 QR1): the changed sections at the start of the agent's next task (the
/// default), a restart at the agent's next idle that resumes its conversation with the new
/// text, or only an "Instructions changed" badge on its row until it starts again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditDelivery {
    #[default]
    NextMessage,
    Restart,
    Badge,
}

impl EditDelivery {
    /// Returns the stored and wire name of this choice.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NextMessage => "next_message",
            Self::Restart => "restart",
            Self::Badge => "badge",
        }
    }

    /// Reads a stored or wire name back. Input: the name. Output: the choice, or none.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::NextMessage, Self::Restart, Self::Badge]
            .into_iter()
            .find(|edits| edits.as_str() == name)
    }
}

/// One project's agent instruction settings: which instruction texts its agents get,
/// whether headless Claude runs keep their transcript, how an instruction edit reaches
/// agents already running, whether project lines the app text already has are left out
/// of the joined text and whether Edit with AI may send the project's instructions to a
/// model. A project the owner never changed gets both texts, keeps transcripts, sends an
/// edit in each running agent's next task, leaves repeated lines out and has Edit with AI on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub transcripts: bool,
    #[serde(default)]
    pub edits: EditDelivery,
    #[serde(default)]
    pub repeated: Repeated,
    #[serde(default = "on")]
    pub edit_with_ai: bool,
}

fn on() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Both,
            transcripts: true,
            edits: EditDelivery::NextMessage,
            repeated: Repeated::LeaveOut,
            edit_with_ai: true,
        }
    }
}

/// The owner's app-wide instruction settings: whether Edit with AI may send the app
/// instructions to a model. On until the owner turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub edit_with_ai: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { edit_with_ai: true }
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
/// index tracks it, so moving it shows as a deletion, the plain code of why it is not
/// imported, none when it is, and, for a moved file, whether its move is still an
/// uncommitted deletion in the owner's checkout that the commit action can record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportFile {
    pub path: String,
    pub bytes: u64,
    pub tracked: bool,
    pub refused: Option<String>,
    #[serde(default)]
    pub uncommitted: bool,
}

/// The commit that recorded the instruction import's move in the owner's checkout: its
/// id, the branch it is on and the moved paths whose deletion it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportCommit {
    pub commit: String,
    pub branch: String,
    pub files: Vec<String>,
}

/// The moved instruction files whose move the owner's checkout has not committed, the
/// branch that checkout has checked out while a move waits, so a notice can name where
/// merges are refused, and in `checked_out` that branch whether or not a move waits, for the
/// project head. A branch is none for a detached HEAD, a name over `BRANCH_BYTES_MAX` or a
/// branch git cannot read for the engine (a name that is not UTF-8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UncommittedImport {
    pub files: Vec<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub checked_out: Option<String>,
}

/// Tells whether a name is one an instruction files record may hold, so a record read back
/// from an agent home names only these files. Input: the name. Output: true for a known name.
pub fn known_name(name: &str) -> bool {
    NAMES.contains(&name)
}

/// One part of an instruction text: a `## ` heading line with the lines up to the next
/// one, or the text before the first heading (an empty heading). `place` counts the parts
/// with the same heading before it, so a repeated heading is told apart by its place.
struct Section<'a> {
    heading: &'a str,
    place: usize,
    text: &'a str,
}

/// Splits an instruction text at its `## ` heading lines, leaving out blank parts. Input:
/// the text. Output: the parts in order, each without the blank lines around it.
fn sections(text: &str) -> Vec<Section<'_>> {
    let mut bounds = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("## ") {
            bounds.push(at);
        }
        at += line.len();
    }
    assert_eq!(at, text.len());
    let mut found: Vec<Section<'_>> = Vec::new();
    let preamble = text[..bounds.first().copied().unwrap_or(text.len())].trim();
    if !preamble.is_empty() {
        found.push(Section {
            heading: "",
            place: 0,
            text: preamble,
        });
    }
    for (index, start) in bounds.iter().enumerate() {
        let end = bounds.get(index + 1).copied().unwrap_or(text.len());
        let part = text[*start..end].trim();
        let heading = part.lines().next().unwrap_or_default().trim_end();
        assert!(heading.starts_with("## "));
        let place = found.iter().filter(|seen| seen.heading == heading).count();
        found.push(Section {
            heading,
            place,
            text: part,
        });
    }
    found
}

/// Tells whether two parts read the same once blank lines and trailing spaces are left out.
fn same_text(left: &str, right: &str) -> bool {
    left.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .eq(right
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty()))
}

/// The parts of the owner's instruction text that changed between what an agent started with
/// and the text now, split at `## ` headings: each added or edited part in full, in the
/// order it now reads, then each removed part as its heading and "This section was removed.".
/// Blank lines alone are no change. Inputs: both texts. Output: the parts, at most
/// `SECTION_BYTES_MAX` bytes joined by blank lines; when they do not all fit, the parts that
/// fit in order and a last line saying the rest apply at the agent's next start.
pub fn changed_sections(started: &str, now: &str) -> Vec<String> {
    fit_sections(changed_parts(started, now), SECTION_BYTES_MAX)
}

/// Every part that changed between the two texts, as `changed_sections` describes, uncut.
/// Inputs: both texts. Output: the parts.
fn changed_parts(started: &str, now: &str) -> Vec<String> {
    let before = sections(started);
    let after = sections(now);
    let mut changed: Vec<String> = after
        .iter()
        .filter(|part| {
            !before.iter().any(|old| {
                old.heading == part.heading
                    && old.place == part.place
                    && same_text(old.text, part.text)
            })
        })
        .map(|part| part.text.to_owned())
        .collect();
    changed.extend(
        before
            .iter()
            .filter(|old| {
                !after
                    .iter()
                    .any(|part| part.heading == old.heading && part.place == old.place)
            })
            .map(|old| match old.heading {
                "" => PREAMBLE_REMOVED.to_owned(),
                heading => format!("{heading}{SECTION_SEPARATOR}{SECTION_REMOVED}"),
            }),
    );
    changed
}

/// Keeps the changed parts that fit in `limit` bytes joined by blank lines: all of them, or
/// the leading ones that fit with `SECTIONS_CUT` as the last line. Inputs: the parts and the
/// limit, at least the cut line and one separator. Output: at most `limit` bytes of parts.
fn fit_sections(changed: Vec<String>, limit: usize) -> Vec<String> {
    assert!(limit >= SECTION_SEPARATOR.len() + SECTIONS_CUT.len());
    if changed.join(SECTION_SEPARATOR).len() <= limit {
        return changed;
    }
    let room = limit - SECTION_SEPARATOR.len() - SECTIONS_CUT.len();
    let mut kept = Vec::new();
    let mut used = 0;
    for part in changed {
        let cost = part.len()
            + if kept.is_empty() {
                0
            } else {
                SECTION_SEPARATOR.len()
            };
        if used + cost > room {
            break;
        }
        used += cost;
        kept.push(part);
    }
    kept.push(SECTIONS_CUT.to_owned());
    assert!(kept.join(SECTION_SEPARATOR).len() <= limit);
    kept
}

/// The block a running agent's next task starts with after the owner edited its
/// instructions: `CHANGED_HEADER`, then the changed parts. Input: at least one part from
/// `changed_sections`. Output: the block, ending in a blank line.
pub fn changed_message(sections: &[String]) -> String {
    assert!(!sections.is_empty());
    let message = format!(
        "{CHANGED_HEADER}{SECTION_SEPARATOR}{}{SECTION_SEPARATOR}",
        sections.join(SECTION_SEPARATOR)
    );
    assert!(
        message.len() <= CHANGED_HEADER.len() + 2 * SECTION_SEPARATOR.len() + SECTION_BYTES_MAX
    );
    message
}

/// An owner edit one running agent has not had yet: the project's choice of how it reaches
/// the agent, the text the agent has, and the engine data folder and the text now, which
/// become the agent's record once the change reached it.
pub struct Edit {
    pub delivery: EditDelivery,
    started: String,
    data: PathBuf,
    now: String,
}

impl Edit {
    /// The change block cut to fit `bytes`, for a terminal input with a size limit such as
    /// the live master's: the changed parts that fit, then `SECTIONS_CUT` when some do not,
    /// with control characters other than line feed and tab dropped so the input stays plain.
    /// Input: the bytes the block may take. Output: the block, or none when not even the
    /// header and the cut line fit.
    pub fn message_within(&self, bytes: usize) -> Option<String> {
        let framing = CHANGED_HEADER.len() + 2 * SECTION_SEPARATOR.len();
        let limit = bytes.checked_sub(framing)?.min(SECTION_BYTES_MAX);
        if limit < SECTION_SEPARATOR.len() + SECTIONS_CUT.len() {
            return None;
        }
        let plain = |text: &str| -> String {
            text.chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                .collect()
        };
        let parts = match changed_parts(&plain(&self.started), &plain(&self.now)) {
            parts if parts.is_empty() => vec![SECTIONS_CUT.to_owned()],
            parts => fit_sections(parts, limit),
        };
        let message = changed_message(&parts);
        assert!(message.len() <= bytes);
        Some(message)
    }

    /// A hash of the owner text this edit brings, the same for the same text within one
    /// engine run, so one edit is acted on once. Output: the hash.
    pub fn text_hash(&self) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&self.now, &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }
}

/// Reads whether the owner's instructions changed since one agent of the project started or
/// last had a change: the engine's own record of what the agent was given, or for a launch
/// before that record the instruction file its launch wrote in its home, against what a
/// launch would give it now. Inputs: a connection to `engine.db`, the project, the agent's
/// home and its launch harness id. Output: the edit, none without a change or for a harness
/// with no instruction file or a home never launched, or a plain code.
pub fn edit_for(
    connection: &Connection,
    project: &ProjectId,
    home: &Path,
    harness: &str,
) -> Result<Option<Edit>, EngineError> {
    let Some(kind) = crate::agent_home::Harness::from_id(harness) else {
        return Ok(None);
    };
    if !home.is_dir() {
        return Ok(None);
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| failure("instruction_settings_unavailable"))?;
    let delivery = load(&tx, project).map_err(failure)?.edits;
    let agent = crate::launch_context::agent_project(&tx, project)
        .map_err(|problem| failure(&problem.0))?;
    drop(tx);
    let now = crate::agent_home::text(&agent)
        .map_err(|problem| failure(&problem.0))?
        .text;
    let started = match crate::agent_home::given_text(&agent.data, home)
        .map_err(|problem| failure(&problem.0))?
    {
        Some(text) => text,
        None => {
            match crate::agent_home::received(home, kind).map_err(|problem| failure(&problem.0))? {
                Some(received) => received.text,
                None => return Ok(None),
            }
        }
    };
    if changed_sections(&started, &now).is_empty() {
        return Ok(None);
    }
    Ok(Some(Edit {
        delivery,
        started,
        data: agent.data,
        now,
    }))
}

/// Records that an edit reached the agent, so the same change is never sent twice. Inputs:
/// the edit and the agent's home. Output: unit, or a plain code.
pub fn delivered(edit: &Edit, home: &Path) -> Result<(), EngineError> {
    crate::agent_home::record_given(&edit.data, home, &edit.now)
        .map_err(|problem| failure(&problem.0))
}

/// Replaces the running agents of one project whose row shows "Instructions changed", writing
/// only when the set changed. Inputs: the engine connection, the project and the agents.
/// Output: unit.
pub fn set_badges(
    connection: &Connection,
    project: &ProjectId,
    agents: &BTreeSet<i64>,
) -> rusqlite::Result<()> {
    if &badges(connection, project)? == agents {
        return Ok(());
    }
    let tx = connection.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM team_instruction_badges WHERE project=?1",
        [project.as_str()],
    )?;
    for agent in agents {
        tx.execute(
            "INSERT OR REPLACE INTO team_instruction_badges(agent, project) VALUES (?1, ?2)",
            params![agent, project.as_str()],
        )?;
    }
    tx.commit()
}

/// Reads the agents of one project whose row shows "Instructions changed"; a database
/// without the table has none. Inputs: the connection and the project. Output: their ids.
pub fn badges(connection: &Connection, project: &ProjectId) -> rusqlite::Result<BTreeSet<i64>> {
    let table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [BADGES_TABLE],
        |row| row.get(0),
    )?;
    if !table {
        return Ok(BTreeSet::new());
    }
    let mut statement =
        connection.prepare("SELECT agent FROM team_instruction_badges WHERE project=?1")?;
    statement
        .query_map([project.as_str()], |row| row.get(0))?
        .collect()
}

/// Folds a file name the way case-insensitive volumes compare names, so a name such a
/// volume opens as an instruction file matches it: ASCII letters to lower case, U+017F LONG S
/// to `s` and U+212A KELVIN SIGN to `k` as APFS folds them (probed), U+0131 DOTLESS I to `i` as
/// the NTFS upcase table maps it to `I`, and the code points HFS+ ignores in names (git's
/// `core.protectHFS` list) left out. Input: the name or path. Output: the folded text.
fn folded(name: &str) -> String {
    name.chars()
        .filter(|c| !HFS_IGNORED.contains(c))
        .map(|c| match c {
            '\u{17F}' => 's',
            '\u{212A}' => 'k',
            '\u{131}' => 'i',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

/// Tells whether a file name is one an instruction file is read under in any folder: a
/// project file name in [`PROJECT_FILES`] or Codex's `AGENTS.override.md`, compared as
/// `folded` compares names. Input: the name. Output: true for one.
pub(crate) fn instruction_name(name: &str) -> bool {
    let name = folded(name);
    PROJECT_FILES
        .iter()
        .filter_map(|file| file.rsplit('/').next())
        .chain([CODEX_OVERRIDE])
        .any(|known| folded(known) == name)
}

/// Tells whether a folder name is a harness folder, `.claude` or `.codex`, compared as
/// `folded` compares names. Input: the name. Output: true for one.
pub(crate) fn harness_folder(name: &str) -> bool {
    let name = folded(name);
    HARNESS_FOLDERS.iter().any(|folder| *folder == name)
}

/// Tells whether a changed project path is an instruction file agents are given or read: a
/// file named like a project source above or Codex's `AGENTS.override.md` in any folder, or
/// any path through a `.claude` or `.codex` folder, names compared as case-insensitive
/// volumes compare them, since macOS and Windows open `claude.md` or a long-s `AGENTS.md`
/// as the instruction file. Input: the path relative to the project. Output: true for one.
pub fn instruction_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or_default();
    instruction_name(name) || path.split('/').any(harness_folder)
}

/// Resolves a link or import target written in the instruction file at `path` to a path in
/// the project, from the file's folder. Inputs: the file's path in the project and the
/// target. Output: the path, or None for an empty, absolute or home target or one that
/// leaves the project.
#[cfg(unix)]
pub(crate) fn resolved(path: &str, target: &str) -> Option<String> {
    if target.is_empty() || target.starts_with('/') || target.starts_with('~') {
        return None;
    }
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The project paths one instruction file imports into what agents are told, as Claude Code
/// reads imports: each word that starts with `@`, with and without trailing punctuation,
/// resolved from the file's folder, at most `IMPORTS_MAX`. Inputs: the file's path in the
/// project and its text. Output: the paths.
#[cfg(unix)]
pub(crate) fn imports(path: &str, text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let Some(target) = word.strip_prefix('@') else {
            continue;
        };
        let trimmed = target.trim_end_matches(|c: char| ".,;:!?)]}'\"`".contains(c));
        for candidate in [target, trimmed] {
            if found.len() == IMPORTS_MAX {
                return found;
            }
            if let Some(resolved) = resolved(path, candidate)
                && !found.contains(&resolved)
            {
                found.push(resolved);
            }
        }
    }
    assert!(found.len() <= IMPORTS_MAX);
    found
}

/// Tells whether a changed path is a file, or lies under a folder, whose text an
/// instruction file feeds to agents, paths compared as case-insensitive volumes compare
/// names. Inputs: the fed paths, each with whether it is a folder, and the changed path.
/// Output: true when fed.
#[cfg(unix)]
pub(crate) fn fed(feeds: &[(String, bool)], path: &str) -> bool {
    let path = folded(path);
    feeds.iter().any(|(feed, folder)| {
        let feed = folded(feed);
        path == feed || (*folder && path.starts_with(&format!("{feed}/")))
    })
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
            transcripts INTEGER NOT NULL,
            edits TEXT NOT NULL DEFAULT 'next_message',
            repeated TEXT NOT NULL DEFAULT 'leave_out',
            edit_with_ai INTEGER NOT NULL DEFAULT 1 CHECK(edit_with_ai IN (0, 1))
        );
        CREATE TABLE IF NOT EXISTS agent_instruction_app_settings (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            edit_with_ai INTEGER NOT NULL CHECK(edit_with_ai IN (0, 1))
        );
        CREATE TABLE IF NOT EXISTS instruction_imports (
            project TEXT PRIMARY KEY,
            moved TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS team_instruction_badges (
            agent INTEGER PRIMARY KEY,
            project TEXT NOT NULL
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
    if !column(connection, "edits")? {
        connection.execute_batch(
            "ALTER TABLE agent_instruction_settings ADD COLUMN edits TEXT NOT NULL DEFAULT 'next_message';",
        )?;
    }
    if !column(connection, "repeated")? {
        connection.execute_batch(
            "ALTER TABLE agent_instruction_settings ADD COLUMN repeated TEXT NOT NULL DEFAULT 'leave_out';",
        )?;
    }
    if !column(connection, "edit_with_ai")? {
        connection.execute_batch(
            "ALTER TABLE agent_instruction_settings ADD COLUMN edit_with_ai INTEGER NOT NULL DEFAULT 1 CHECK(edit_with_ai IN (0, 1));",
        )?;
    }
    Ok(())
}

/// Tells whether the settings table holds a column added after its first shape: the edit
/// delivery column tables made before DEC-111 QR1 lack, or the repeated lines and Edit with
/// AI columns tables made before v2.2.1 lack. Inputs: a connection to `engine.db` and the
/// column name. Output: the answer.
fn column(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    assert!(matches!(name, "edits" | "repeated" | "edit_with_ai"));
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('agent_instruction_settings') WHERE name=?1)",
        [name],
        |row| row.get(0),
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
    let edits = match column(connection, "edits").map_err(unavailable)? {
        true => "edits",
        false => "'next_message'",
    };
    let repeated = match column(connection, "repeated").map_err(unavailable)? {
        true => "repeated",
        false => "'leave_out'",
    };
    let edit_with_ai = match column(connection, "edit_with_ai").map_err(unavailable)? {
        true => "edit_with_ai",
        false => "1",
    };
    let stored: Option<(String, i64, String, String, i64)> = connection
        .query_row(
            &format!(
                "SELECT mode, transcripts, {edits}, {repeated}, {edit_with_ai} FROM agent_instruction_settings WHERE project=?1"
            ),
            [project.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(unavailable)?;
    let Some((mode, transcripts, edits, repeated, edit_with_ai)) = stored else {
        return Ok(Settings::default());
    };
    let mode = Mode::parse(&mode).ok_or("instruction_settings_invalid")?;
    let transcripts = switch(transcripts).ok_or("instruction_settings_invalid")?;
    let edit_with_ai = switch(edit_with_ai).ok_or("instruction_settings_invalid")?;
    let edits = EditDelivery::parse(&edits).ok_or("instruction_settings_invalid")?;
    let repeated = Repeated::parse(&repeated).ok_or("instruction_settings_invalid")?;
    Ok(Settings {
        mode,
        transcripts,
        edits,
        repeated,
        edit_with_ai,
    })
}

/// Reads a stored on or off. Input: the stored number. Output: the choice, none for a number
/// that is neither 0 nor 1.
fn switch(stored: i64) -> Option<bool> {
    match stored {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Reads the app-wide instruction settings; a database that never stored any gets the
/// defaults. Input: a connection to `engine.db`, read-only is enough. Output: the settings,
/// `instruction_settings_invalid` for a stored value outside the options, or
/// `instruction_settings_unavailable`.
pub fn load_app(connection: &Connection) -> Result<AppSettings, &'static str> {
    let unavailable = |_| "instruction_settings_unavailable";
    let table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_instruction_app_settings')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !table {
        return Ok(AppSettings::default());
    }
    let stored: Option<i64> = connection
        .query_row(
            "SELECT edit_with_ai FROM agent_instruction_app_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    match stored {
        None => Ok(AppSettings::default()),
        Some(stored) => Ok(AppSettings {
            edit_with_ai: switch(stored).ok_or("instruction_settings_invalid")?,
        }),
    }
}

/// Stores the app-wide instruction settings whole, creating the table first when it is
/// missing. Inputs: a writable connection to `engine.db` and the settings. Output: unit.
pub fn save_app(connection: &Connection, settings: AppSettings) -> rusqlite::Result<()> {
    initialize(connection)?;
    let changed = connection.execute(
        "INSERT INTO agent_instruction_app_settings(singleton, edit_with_ai) VALUES (1, ?1)
         ON CONFLICT(singleton) DO UPDATE SET edit_with_ai=excluded.edit_with_ai",
        params![i64::from(settings.edit_with_ai)],
    )?;
    assert_eq!(changed, 1);
    assert!(load_app(connection) == Ok(settings));
    Ok(())
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
        "INSERT INTO agent_instruction_settings(project, mode, transcripts, edits, repeated, edit_with_ai) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(project) DO UPDATE SET mode=excluded.mode, transcripts=excluded.transcripts, edits=excluded.edits, repeated=excluded.repeated, edit_with_ai=excluded.edit_with_ai",
        params![
            project.as_str(),
            settings.mode.as_str(),
            i64::from(settings.transcripts),
            settings.edits.as_str(),
            settings.repeated.as_str(),
            i64::from(settings.edit_with_ai)
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

/// Reads what an import at a project's onboarding moved, empty when it moved nothing, each
/// git-tracked file marked by whether its move is still uncommitted in the project folder
/// now. Inputs: a connection to `engine.db`, the project and its folder. Output: the moved
/// files, or a plain code for a record the engine never writes.
pub fn moved(
    connection: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Vec<ImportFile>, EngineError> {
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
    let mut files: Vec<ImportFile> =
        serde_json::from_str(&stored).map_err(|_| failure("process_unavailable"))?;
    if files.len() > PROJECT_FILES.len() {
        return Err(failure("process_unavailable"));
    }
    if files.iter().any(|file| file.tracked) {
        let waiting = uncommitted(folder).unwrap_or_default();
        for file in &mut files {
            file.uncommitted = file.tracked && waiting.contains(&file.path);
        }
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
    if moved.iter().any(|file| file.tracked) {
        let waiting = uncommitted(&root).unwrap_or_default();
        for file in &mut moved {
            file.uncommitted = file.tracked && waiting.contains(&file.path);
        }
    }
    Ok(moved)
}

/// Lists the moved instruction files whose move the owner's checkout has not committed:
/// each project instruction path `.crazyagent/imported/` holds a moved copy of, that the
/// project folder no longer has and that the checked-out commit still holds. Input: the
/// project folder. Output: the paths in `PROJECT_FILES` order, empty for a folder that is
/// no git checkout, or `process_project_folder_missing` or `process_commit_failed`.
pub fn uncommitted(folder: &Path) -> Result<Vec<String>, EngineError> {
    let root = fs::canonicalize(folder).map_err(|_| failure("process_project_folder_missing"))?;
    let imported = root
        .join(crate::projects_home::CRAZYAGENT_DIRECTORY)
        .join(IMPORTED_DIRECTORY);
    let moved: Vec<&str> = PROJECT_FILES
        .into_iter()
        .filter(|path| {
            fs::symlink_metadata(imported.join(path)).is_ok()
                && matches!(
                    fs::symlink_metadata(root.join(path)),
                    Err(problem) if problem.kind() == std::io::ErrorKind::NotFound
                )
        })
        .collect();
    let checkout = fs::symlink_metadata(root.join(".git")).is_ok_and(|metadata| metadata.is_dir());
    if moved.is_empty() || !checkout {
        return Ok(Vec::new());
    }
    let committed = crate::landing::committed_files(&root, &moved)
        .map_err(|_| failure("process_commit_failed"))?;
    assert_eq!(committed.len(), moved.len());
    Ok(moved
        .into_iter()
        .zip(committed)
        .filter(|(_, held)| *held)
        .map(|(path, _)| path.to_owned())
        .collect())
}

/// What a reftable checkout's `.git/HEAD` holds: a stub naming a branch git never allows, as
/// no branch name starts with a dot, while the real HEAD lives in `.git/reftable`.
const REFTABLE_HEAD: &[u8] = b"ref: refs/heads/.invalid";
/// The most bytes of `.git/HEAD` read to tell the reftable stub from a real HEAD.
const HEAD_BYTES_MAX: u64 = 1024;

/// The stat of the file a checkout's branch is stamped by: `.git/HEAD`, or for a reftable
/// checkout, whose HEAD is a stub, `.git/reftable/tables.list`. Git replaces that file through
/// a new lock file on every branch switch, detach or rename of the current branch, so an
/// equal stamp means the checked-out branch is the one read before.
#[derive(Clone, Copy, PartialEq, Eq)]
struct HeadStamp {
    file: u64,
    len: u64,
    modified: SystemTime,
}

/// Stats the file that stamps a checkout's branch, chosen by what `.git/HEAD` holds and never
/// by which folders exist, so a stray `.git/reftable` in a files checkout is not stamped
/// (UI-9-L1): the reftable stub ([`REFTABLE_HEAD`]) stamps `.git/reftable/tables.list`, any
/// other HEAD stamps itself. Inputs: the checkout root. Output: the stamp, or None when
/// either file cannot be read as a plain file, as in a linked worktree whose `.git` is a file.
fn head_stamp(root: &Path) -> Option<HeadStamp> {
    let git = root.join(".git");
    let head = git.join("HEAD");
    let head_meta = fs::symlink_metadata(&head).ok()?;
    if !head_meta.is_file() {
        return None;
    }
    let mut text = Vec::new();
    fs::File::open(&head)
        .ok()?
        .take(HEAD_BYTES_MAX)
        .read_to_end(&mut text)
        .ok()?;
    let meta = if text.trim_ascii_end() == REFTABLE_HEAD {
        fs::symlink_metadata(git.join("reftable").join("tables.list")).ok()?
    } else {
        head_meta
    };
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    let file = meta.ino();
    #[cfg(not(unix))]
    let file = 0;
    Some(HeadStamp {
        file,
        len: meta.len(),
        modified: meta.modified().ok()?,
    })
}

/// Reads the branch a checkout has checked out through `read`, the git read, again only when
/// the checkout's HEAD stamp changed since the last read of that root, at most `HEADS_MAX`
/// roots kept. A failed read shows none and is not kept. Inputs: the canonical checkout root
/// and the read. Output: the branch.
fn branch_of(
    root: &Path,
    read: impl FnOnce() -> Result<Option<String>, EngineError>,
) -> Option<String> {
    let Some(stamp) = head_stamp(root) else {
        return read().unwrap_or(None);
    };
    let kept = HEADS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(root)
        .filter(|(seen, _)| *seen == stamp)
        .map(|(_, branch)| branch.clone());
    if let Some(branch) = kept {
        return branch;
    }
    let Ok(branch) = read() else {
        return None;
    };
    let mut heads = HEADS.lock().unwrap_or_else(PoisonError::into_inner);
    if heads.len() == HEADS_MAX && !heads.contains_key(root) {
        heads.clear();
    }
    heads.insert(root.to_path_buf(), (stamp, branch.clone()));
    assert!(heads.len() <= HEADS_MAX);
    branch
}

/// Lists the moved instruction files whose move the owner's checkout has not committed, as
/// `uncommitted` does, with the branch the checkout has checked out, named as the refusal's
/// branch only while any wait; a branch that cannot be read is left unnamed, so the move
/// still shows. Input: the project folder. Output: the files and branches, or a plain code
/// as `uncommitted`.
pub fn uncommitted_import(folder: &Path) -> Result<UncommittedImport, EngineError> {
    let root = fs::canonicalize(folder).map_err(|_| failure("process_project_folder_missing"))?;
    let files = uncommitted(&root)?;
    let checked_out = branch_of(&root, || crate::landing::checked_out_branch(&root));
    let branch = (!files.is_empty()).then(|| checked_out.clone()).flatten();
    Ok(UncommittedImport {
        files,
        branch,
        checked_out,
    })
}

/// Commits the instruction import's move in the owner's checkout, as the owner chose: one
/// local commit, never pushed, on the checked-out branch with the repository's own git
/// identity, holding only the deletions of the moved files the branch still holds.
/// Inputs: the project folder and the owner's home folder. Output: the commit, or
/// `process_commit_none` when no move waits for one, `process_commit_staged` when the
/// index stages another path, or another `process_commit_` code with nothing committed.
pub fn commit(folder: &Path, owner_home: &Path) -> Result<ImportCommit, EngineError> {
    let _importing = IMPORTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = fs::canonicalize(folder).map_err(|_| failure("process_project_folder_missing"))?;
    let waiting = uncommitted(&root)?;
    if waiting.is_empty() {
        return Err(failure("process_commit_none"));
    }
    let paths: Vec<&str> = waiting.iter().map(String::as_str).collect();
    let message = format!(
        "Move {} into .crazyagent/imported/\n\nCrazyAgent imported the text into the project instructions, .crazyagent/instructions.md.\n",
        paths.join(", ")
    );
    let (commit, branch, files) = crate::landing::commit_deletions(
        &root, owner_home, &paths, &message,
    )
    .map_err(|problem| {
        failure(match problem.0.as_str() {
            "changes_import_none" => "process_commit_none",
            "changes_import_staged" => "process_commit_staged",
            "changes_git_busy" => "process_commit_busy",
            "changes_detached_head" => "process_commit_detached",
            "changes_branch_moved" => "process_commit_moved",
            "changes_owner_identity_missing" => "process_commit_identity",
            _ => "process_commit_failed",
        })
    })?;
    assert!(!files.is_empty() && files.len() <= paths.len());
    Ok(ImportCommit {
        commit,
        branch,
        files,
    })
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
                    uncommitted: false,
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
