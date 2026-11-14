use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::onboarding::draft::{self, Folder, overview_seal};
use crate::project_setup::ResearchKind;
use crate::project_setup::links::{self, LinkKind};
use crate::protocol::ProjectId;
use crate::roadmap_status::{QUIET_MS, Refresh};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[cfg(unix)]
pub(crate) mod agent;
mod pack;
mod run;
#[cfg(test)]
mod tests;

pub use pack::{
    EVIDENCE_BYTES_MAX, RESEARCH_BYTES_MAX, Research, TREE_PATHS_MAX, evidence, named_tabs,
    parse_reply, prompt,
};
pub use run::{
    Ended, NO_ACCOUNT, NO_TABS, NOT_WRITTEN, REPLY_BYTES_MAX, REPLY_TOO_LONG, RUN_TIME,
    STARTS_PER_TICK_MAX, Started, UNFINISHED, account_found, brief, close_orphans, fail, finish,
    requeue, tick, wait_for_account,
};

/// The folder inside a project's `.crazyagent` that holds the tab files.
pub const TABS_FOLDER: &str = "overview";
/// The most bytes of one tab's text, as a file and as a proposal: two of them travel in
/// one owner frame.
pub const TAB_BYTES_MAX: usize = 24 * 1024;
/// The most bytes of the project's README the Overview shows; a longer one is cut at a
/// line, since the text travels in one owner frame.
pub const README_BYTES_MAX: u64 = 24 * 1024;
/// The most bytes of the words saying why the last writing run failed.
pub const FAILURE_BYTES_MAX: usize = 200;
/// The tab file is not at the version the owner saw.
pub const TAB_CHANGED: &str = "overview_tab_changed";
/// Something that is not a tab file sits where the tab file goes, or the folder cannot hold it.
pub const TAB_BLOCKED: &str = "overview_tab_blocked";
/// The text is longer than one tab holds.
pub const TAB_TOO_LARGE: &str = "overview_tab_too_large";
/// The text holds nothing but blank space.
pub const TAB_EMPTY: &str = "overview_tab_empty";
/// The tab has no proposal to accept.
pub const NO_PROPOSAL: &str = "overview_no_proposal";
/// The tab's proposal is not the one the owner reviewed: a newer one took its place.
pub const PROPOSAL_CHANGED: &str = "overview_proposal_changed";
/// The project has no `.crazyagent` folder to hold tab files: it was made before the
/// projects home and has to move into it first.
pub const NEEDS_HOME: &str = "overview_needs_project_home";
/// A refresh was asked for while the project's Overview tabs are off.
pub const OFF: &str = "overview_off";
/// The engine's store could not be read or written.
pub const UNAVAILABLE: &str = "overview_unavailable";

/// Why a tab's file is not read: it holds nothing but blank space.
pub const BLOCKED_BLANK: &str = "blank";
/// Why a tab's file is not read: it is longer than one tab holds.
pub const BLOCKED_TOO_LARGE: &str = "too_large";
/// Why a tab's file is not read: it is not valid UTF-8 text.
pub const BLOCKED_NOT_UTF8: &str = "not_utf8";
/// Why a tab's file is not read: a link, a folder or another special entry sits there.
pub const BLOCKED_NOT_A_FILE: &str = "not_a_file";
/// Why no tab is read: the tab folder's place holds something that is not a real folder.
pub const BLOCKED_NOT_A_FOLDER: &str = "not_a_folder";

const LAYOUT: &str = ".crazyagent";
const README_NAMES: [&str; 7] = [
    "README.md",
    "Readme.md",
    "readme.md",
    "README.markdown",
    "README",
    "README.txt",
    "README.rst",
];
const REPORT_PATH_BYTES_MAX: usize = 1024;
const TEMP_RANDOM_BYTES: usize = 8;

const _: () = assert!(TAB_BYTES_MAX <= 64 * 1024);
const _: () = assert!(TAB_BYTES_MAX as u64 <= README_BYTES_MAX);
const _: () = assert!(FAILURE_BYTES_MAX == 200 && TAB_BYTES_MAX == 24576);
const _: () = assert!(TEMP_RANDOM_BYTES >= 8);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_overview(
  project TEXT PRIMARY KEY,
  refresh TEXT NOT NULL DEFAULT 'after_landing' CHECK(refresh IN ('after_landing','refresh_only','off')),
  apply TEXT NOT NULL DEFAULT 'always_ask' CHECK(apply IN ('always_ask','when_unedited')),
  stale INTEGER NOT NULL DEFAULT 0,
  due_ms INTEGER NOT NULL DEFAULT 0,
  running_ms INTEGER,
  checked_ms INTEGER,
  failure TEXT CHECK(failure IS NULL OR length(CAST(failure AS BLOB)) <= 200)
);
CREATE TABLE IF NOT EXISTS project_overview_tabs(
  project TEXT NOT NULL,
  tab TEXT NOT NULL CHECK(tab IN ('quick_info','features','competitors','install_and_use')),
  seal TEXT,
  owner_seal TEXT,
  proposal TEXT CHECK(proposal IS NULL OR length(CAST(proposal AS BLOB)) <= 24576),
  PRIMARY KEY(project, tab)
);
";

/// Adds the owner's seal to a tabs table made before it was kept (RV-DS241-2).
const OWNER_SEAL_COLUMN: &str = "ALTER TABLE project_overview_tabs ADD COLUMN owner_seal TEXT";

/// One of the four Overview tabs an agent writes. The README tab is the repository's own
/// file and is not one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    QuickInfo,
    Features,
    Competitors,
    InstallAndUse,
}

impl Tab {
    pub const ALL: [Tab; 4] = [
        Tab::QuickInfo,
        Tab::Features,
        Tab::Competitors,
        Tab::InstallAndUse,
    ];

    /// The tab's file name inside `.crazyagent/overview/` and its fence name in an agent reply.
    /// Input: the tab. Output: the stem, such as "install-and-use".
    pub fn stem(self) -> &'static str {
        match self {
            Tab::QuickInfo => "quick-info",
            Tab::Features => "features",
            Tab::Competitors => "competitors",
            Tab::InstallAndUse => "install-and-use",
        }
    }

    /// The tab's name as the Overview strip shows it. Input: the tab. Output: the label,
    /// such as "Install & use".
    pub fn label(self) -> &'static str {
        match self {
            Tab::QuickInfo => "Quick info",
            Tab::Features => "Features",
            Tab::Competitors => "Competitors",
            Tab::InstallAndUse => "Install & use",
        }
    }

    /// The name the store and the wire use for the tab. Input: the tab. Output: its name,
    /// such as "install_and_use".
    fn name(self) -> &'static str {
        match self {
            Tab::QuickInfo => "quick_info",
            Tab::Features => "features",
            Tab::Competitors => "competitors",
            Tab::InstallAndUse => "install_and_use",
        }
    }

    /// The tab's file name. Input: the tab. Output: the name, such as "features.md".
    fn file(self) -> String {
        format!("{}.md", self.stem())
    }
}

/// What happens to a refreshed tab ("Refreshed tabs" in Project settings): the owner is
/// always asked, or a tab the owner has not edited is written at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    #[default]
    AlwaysAsk,
    WhenUnedited,
}

impl Apply {
    /// The name the settings table stores. Input: the choice. Output: its name.
    fn name(self) -> &'static str {
        match self {
            Apply::AlwaysAsk => "always_ask",
            Apply::WhenUnedited => "when_unedited",
        }
    }

    /// The choice a stored name stands for. Input: the name. Output: the choice, or `None`
    /// for a name the table never holds.
    fn named(name: &str) -> Option<Self> {
        [Apply::AlwaysAsk, Apply::WhenUnedited]
            .into_iter()
            .find(|apply| apply.name() == name)
    }
}

/// One tab as the owner sees it: the file's text, the version an edit or an accept must
/// name, whether the owner saved it here since the engine last wrote it, whether it came
/// with the project's files instead, neither written by the engine nor saved by the owner,
/// so no client offers to run its commands (RV-DS241-2; false from an older engine), a
/// waiting proposal with the version an accept must name for it (empty without one), and
/// for a file the engine does not read the reason code (`blank`, `too_large`, `not_utf8`,
/// `not_a_file`, `not_a_folder`), `None` for a readable or missing file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub tab: Tab,
    pub text: Option<String>,
    pub version: String,
    pub edited: bool,
    #[serde(default)]
    pub foreign: bool,
    pub proposal: Option<String>,
    pub proposal_version: String,
    pub blocked: Option<String>,
}

/// The repository's README as the Overview shows it: its path in the project, its text and
/// whether the text was cut at the bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readme {
    pub path: String,
    pub text: String,
    pub cut: bool,
}

/// What the Competitors tab can build on: nothing yet, research files the owner linked, or
/// a finished research report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Competitors {
    Missing,
    Linked { paths: Vec<String> },
    Report { path: String },
}

/// A project's Overview: the README, the four tabs in [`Tab::ALL`] order, the competitor
/// state, where the writing run stands, its stored line and whether that line is the wait
/// for an account (false in an older engine's reply), the project's two Overview settings
/// and whether the project has to move into the projects home before it can hold tab files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub readme: Option<Readme>,
    pub tabs: Vec<TabState>,
    pub competitors: Competitors,
    pub running: bool,
    pub stale: bool,
    pub checked_ms: Option<i64>,
    pub failure: Option<String>,
    #[serde(default)]
    pub waiting_for_account: bool,
    pub refresh: Refresh,
    pub apply: Apply,
    pub needs_home: bool,
}

/// What an offered text became: the tab file, a proposal waiting for the owner, or nothing
/// because the file already holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    Written,
    Proposed,
    Same,
}

/// A project's stored Overview row, with the defaults while there is none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct State {
    refresh: Refresh,
    apply: Apply,
    stale: bool,
    running_ms: Option<i64>,
    checked_ms: Option<i64>,
    failure: Option<String>,
}

/// What sits where a tab file goes: nothing, a tab file's text, or something the engine
/// neither reads nor replaces (a link, a folder, an empty, oversized or non-text file),
/// with its reason code.
enum Found {
    Missing,
    Text(String),
    Blocked(&'static str),
}

/// Where a project's tab folder stands: opened, not made yet, taken by something that is
/// not a real folder, or impossible because the project has no `.crazyagent` folder.
enum Place {
    Open(Folder),
    Missing,
    NotAFolder,
    NoLayout,
}

/// One write of a tab file at a time in this engine: the read of the file, the version
/// check, the rename and the record of the seal run under it, so a refresh and an owner's
/// save cannot interleave.
static WRITING: Mutex<()> = Mutex::new(());

/// Takes the one-writer lock, also after a writer panicked: the files are whole either
/// way, since each write is one rename. Output: the guard.
fn writing() -> MutexGuard<'static, ()> {
    WRITING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The longest prefix of `text` within `max` bytes that ends on a character boundary.
/// Inputs: the text and the bound. Output: the text itself when it fits.
fn clipped(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    assert!(end <= max);
    text[..end].to_owned()
}

#[cfg(test)]
thread_local! {
    /// What a test runs on its own thread between a write's checks and its rename, to stand
    /// in for another writer arriving at that moment.
    static BEFORE_REPLACE: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether the project has no `.crazyagent` folder of its own, as a project made before the
/// projects home: it holds no tab files until it moves into the home. Input: the project
/// folder. Output: the answer.
pub fn needs_home(folder: &Path) -> bool {
    crate::projects_home::state_folder(folder).as_path() == folder
}

fn unavailable<E>(_: E) -> EngineError {
    failure(UNAVAILABLE)
}

/// The name the settings table stores for a refresh choice. Input: the choice. Output:
/// its name.
fn refresh_name(refresh: Refresh) -> &'static str {
    match refresh {
        Refresh::AfterLanding => "after_landing",
        Refresh::RefreshOnly => "refresh_only",
        Refresh::Off => "off",
    }
}

/// The refresh choice a stored name stands for. Input: the name. Output: the choice, or
/// `None` for a name the table never holds.
fn refresh_named(name: &str) -> Option<Refresh> {
    [Refresh::AfterLanding, Refresh::RefreshOnly, Refresh::Off]
        .into_iter()
        .find(|refresh| refresh_name(*refresh) == name)
}

/// Creates the Overview tables, and adds the owner's seal to a tabs table made before it was
/// kept (RV-DS241-2); a connection that lost the race to add it finds it added. Input: the
/// engine connection. Output: unit, or `overview_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    if owner_sealed(db)? {
        return Ok(());
    }
    let added = db.execute_batch(OWNER_SEAL_COLUMN);
    if added.is_err() && !owner_sealed(db)? {
        return Err(unavailable(()));
    }
    assert!(owner_sealed(db)?);
    Ok(())
}

/// Whether the tabs table keeps the owner's seal. Input: the engine connection. Output: the
/// answer, or `overview_unavailable`.
fn owner_sealed(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('project_overview_tabs')
         WHERE name='owner_seal')",
        [],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Opens the project's tab folder beneath its `.crazyagent` folder, each opened once and
/// never through a link, making the tab folder first with `create`. Inputs: the project
/// folder and whether to create. Output: where the tab folder stands.
#[cfg(unix)]
fn tabs_folder(project: &Path, create: bool) -> Place {
    use rustix::fs::{Mode, OFlags};
    let Some(layout) = draft::open_folder(&project.join(LAYOUT)) else {
        return Place::NoLayout;
    };
    if create {
        match rustix::fs::mkdirat(&layout, TABS_FOLDER, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Place::NotAFolder,
        }
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match rustix::fs::openat(&layout, TABS_FOLDER, flags, Mode::empty()) {
        Ok(folder) => Place::Open(folder),
        Err(_) if exists(&layout, TABS_FOLDER) => Place::NotAFolder,
        Err(_) => Place::Missing,
    }
}

/// Checks the project's `.crazyagent` folder and its tab folder are real folders, making
/// the tab folder first with `create`: the path-based Windows form described on
/// [`Folder`]. Inputs: the project folder and whether to create. Output: where the tab
/// folder stands.
#[cfg(windows)]
fn tabs_folder(project: &Path, create: bool) -> Place {
    let Some(layout) = draft::open_folder(&project.join(LAYOUT)) else {
        return Place::NoLayout;
    };
    if create {
        match std::fs::DirBuilder::new().create(layout.join(TABS_FOLDER)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Place::NotAFolder,
        }
    }
    match draft::open_folder(&layout.join(TABS_FOLDER)) {
        Some(folder) => Place::Open(folder),
        None if exists(&layout, TABS_FOLDER) => Place::NotAFolder,
        None => Place::Missing,
    }
}

/// Opens the project's tab folder for a write, making it when absent. Input: the project
/// folder. Output: the folder, `overview_needs_project_home` for a project with no
/// `.crazyagent` folder, or `overview_tab_blocked` when the tab folder's place is taken.
fn place(project: &Path) -> Result<Folder, EngineError> {
    match tabs_folder(project, true) {
        Place::Open(folder) => Ok(folder),
        Place::NoLayout => Err(failure(NEEDS_HOME)),
        Place::Missing | Place::NotAFolder => Err(failure(TAB_BLOCKED)),
    }
}

/// Whether any entry, a link included, is named `name` in `folder`. Inputs: the open
/// folder and the name. Output: the answer.
#[cfg(unix)]
fn exists(folder: &Folder, name: &str) -> bool {
    rustix::fs::statat(folder, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).is_ok()
}

/// Whether any entry, a link included, is named `name` in `folder`. Inputs: the folder
/// and the name. Output: the answer.
#[cfg(windows)]
fn exists(folder: &Folder, name: &str) -> bool {
    std::fs::symlink_metadata(folder.join(name)).is_ok()
}

/// Reads an opened entry as a tab file: a regular file of valid text, not blank and not
/// over [`TAB_BYTES_MAX`]. The text is exact, never trimmed or cut. Input: the open file.
/// Output: the text, or `Blocked` with its reason for anything else.
fn read_file(file: std::fs::File) -> Found {
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Found::Blocked(BLOCKED_NOT_A_FILE);
    }
    let bound = TAB_BYTES_MAX as u64 + 1;
    let mut bytes = Vec::new();
    if file.take(bound).read_to_end(&mut bytes).is_err() {
        return Found::Blocked(BLOCKED_NOT_A_FILE);
    }
    assert!(bytes.len() as u64 <= bound);
    if bytes.len() > TAB_BYTES_MAX {
        return Found::Blocked(BLOCKED_TOO_LARGE);
    }
    match String::from_utf8(bytes) {
        Ok(text) if text.trim().is_empty() => Found::Blocked(BLOCKED_BLANK),
        Ok(text) => Found::Text(text),
        Err(_) => Found::Blocked(BLOCKED_NOT_UTF8),
    }
}

/// What sits where the tab's file goes in the opened tab folder, never read through a
/// link. Inputs: the open tab folder and the tab. Output: what was found.
fn found_in(folder: &Folder, tab: Tab) -> Found {
    let name = tab.file();
    match draft::open_in(folder, &name) {
        Some(file) => read_file(file),
        None if exists(folder, &name) => Found::Blocked(BLOCKED_NOT_A_FILE),
        None => Found::Missing,
    }
}

/// A fresh name for the partial file of `name`: `.<stem>.<16 random hex>.tmp`. Input: the
/// tab file's name. Output: the partial name, or the failure of the random source.
fn temp_name(name: &str) -> std::io::Result<String> {
    let mut random = [0u8; TEMP_RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|_| std::io::Error::other("no random bytes"))?;
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(hex.len(), TEMP_RANDOM_BYTES * 2);
    Ok(format!(
        ".{}.{hex}.tmp",
        name.strip_suffix(".md").unwrap_or(name)
    ))
}

/// Writes `bytes` to `name` in the opened folder through a fresh partial file and a rename
/// inside that same folder, so nothing is ever written through a link and a reader sees the
/// old text or the new, never a part. With `fresh` the rename never replaces an entry, so
/// a file made there meanwhile is kept. Inputs: the open folder, the file name, the bytes
/// and whether no file was there. Output: any I/O failure (`AlreadyExists` for a fresh
/// write that found an entry); the partial file is removed on one.
#[cfg(unix)]
fn replace(folder: &Folder, name: &str, bytes: &[u8], fresh: bool) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
    let temp = temp_name(name)?;
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = std::fs::File::from(rustix::fs::openat(
        folder,
        temp.as_str(),
        flags,
        Mode::from_raw_mode(0o644),
    )?);
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            let renamed = if fresh {
                rustix::fs::renameat_with(
                    folder,
                    temp.as_str(),
                    folder,
                    name,
                    RenameFlags::NOREPLACE,
                )
            } else {
                rustix::fs::renameat(folder, temp.as_str(), folder, name)
            };
            renamed.map_err(std::io::Error::from)
        });
    if written.is_err() {
        let _ = rustix::fs::unlinkat(folder, temp.as_str(), AtFlags::empty());
    }
    written
}

/// Writes `bytes` to `name` in the folder through a fresh partial file, created as the
/// reparse point itself, and a rename: the path-based Windows form described on
/// [`Folder`]. With `fresh` an entry found there just before the rename is kept. Inputs:
/// the folder, the file name, the bytes and whether no file was there. Output: any I/O
/// failure.
#[cfg(windows)]
fn replace(folder: &Folder, name: &str, bytes: &[u8], fresh: bool) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = folder.join(temp_name(name)?);
    let target = folder.join(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| {
            if fresh && std::fs::symlink_metadata(&target).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(&temp, &target)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Refuses a text one tab cannot hold. Owner text is never cut. Input: the text. Output:
/// unit, or `overview_tab_too_large` or `overview_tab_empty`.
fn checked(text: &str) -> Result<(), EngineError> {
    if text.len() > TAB_BYTES_MAX {
        return Err(failure(TAB_TOO_LARGE));
    }
    if text.trim().is_empty() {
        return Err(failure(TAB_EMPTY));
    }
    Ok(())
}

/// Writes the tab's file in the opened tab folder and reads it back. The caller holds the
/// one-writer lock. Inputs: the open tab folder, the tab, a checked text and whether no
/// file was there. Output: unit, `overview_tab_blocked` when the file cannot be written,
/// or `overview_tab_changed` when another writer made or replaced the file meanwhile.
fn write_tab(folder: &Folder, tab: Tab, text: &str, fresh: bool) -> Result<(), EngineError> {
    assert!(text.len() <= TAB_BYTES_MAX);
    assert!(!text.trim().is_empty());
    #[cfg(test)]
    BEFORE_REPLACE.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook();
        }
    });
    replace(folder, &tab.file(), text.as_bytes(), fresh).map_err(|error| {
        if fresh && error.kind() == std::io::ErrorKind::AlreadyExists {
            failure(TAB_CHANGED)
        } else {
            failure(TAB_BLOCKED)
        }
    })?;
    match found_in(folder, tab) {
        Found::Text(back) if back == text => Ok(()),
        _ => Err(failure(TAB_CHANGED)),
    }
}

/// The version of what sits where a tab file goes: the seal of the file's text, the empty
/// string while there is no file. Input: what was found. Output: the version, or
/// `overview_tab_blocked` for something that is not a tab file.
fn version_of(found: &Found) -> Result<String, EngineError> {
    match found {
        Found::Missing => Ok(String::new()),
        Found::Text(text) => Ok(overview_seal(text)),
        Found::Blocked(_) => Err(failure(TAB_BLOCKED)),
    }
}

/// The version an owner's write must name: as [`version_of`], with a blank file counted
/// as no text, so the owner can put text back into a tab whose file was emptied. Input:
/// what was found. Output: the version, or `overview_tab_blocked`.
fn owner_version(found: &Found) -> Result<String, EngineError> {
    match found {
        Found::Blocked(BLOCKED_BLANK) => Ok(String::new()),
        other => version_of(other),
    }
}

/// The project's stored row. Inputs: the connection and the project. Output: the row, the
/// defaults while there is none, or `overview_unavailable`.
fn state(db: &Connection, project: &ProjectId) -> Result<State, EngineError> {
    type Row = (
        String,
        String,
        bool,
        Option<i64>,
        Option<i64>,
        Option<String>,
    );
    let row: Option<Row> = db
        .query_row(
            "SELECT refresh,apply,stale,running_ms,checked_ms,failure
             FROM project_overview WHERE project=?1",
            [project.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(unavailable)?;
    let Some((refresh, apply, stale, running_ms, checked_ms, failure)) = row else {
        return Ok(State::default());
    };
    Ok(State {
        refresh: refresh_named(&refresh).ok_or_else(|| unavailable(()))?,
        apply: Apply::named(&apply).ok_or_else(|| unavailable(()))?,
        stale,
        running_ms,
        checked_ms,
        failure,
    })
}

/// One tab's stored row: the seal of the last text the engine wrote, the seal of the last
/// text the owner saved here since, and the waiting proposal, each `None` while unset.
#[derive(Default)]
struct TabRow {
    seal: Option<String>,
    owner_seal: Option<String>,
    proposal: Option<String>,
}

/// The stored row of one tab. Inputs: the connection, the project and the tab. Output: the
/// row, all `None` while there is none.
fn tab_row(db: &Connection, project: &ProjectId, tab: Tab) -> Result<TabRow, EngineError> {
    let row = db
        .query_row(
            "SELECT seal,owner_seal,proposal FROM project_overview_tabs
             WHERE project=?1 AND tab=?2",
            params![project.as_str(), tab.name()],
            |row| {
                Ok(TabRow {
                    seal: row.get(0)?,
                    owner_seal: row.get(1)?,
                    proposal: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(unavailable)?;
    Ok(row.unwrap_or_default())
}

/// Records that the engine wrote `text` as the tab's file: stores its seal, forgets the
/// owner's, since the engine wrote last, and drops any proposal. Inputs: the connection,
/// the project, the tab and the written text. Output: unit, or `overview_unavailable`.
fn wrote(db: &Connection, project: &ProjectId, tab: Tab, text: &str) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO project_overview_tabs(project,tab,seal,proposal) VALUES(?1,?2,?3,NULL)
             ON CONFLICT(project,tab) DO UPDATE SET seal=excluded.seal, owner_seal=NULL,
               proposal=NULL",
            params![project.as_str(), tab.name(), overview_seal(text)],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Records that the owner saved `text` as the tab's file through the engine, so a read
/// knows it as the owner's and not as one the project's files brought (RV-DS241-2).
/// Inputs: the connection, the project, the tab and the saved text. Output: unit, or
/// `overview_unavailable`.
fn saved(db: &Connection, project: &ProjectId, tab: Tab, text: &str) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO project_overview_tabs(project,tab,owner_seal) VALUES(?1,?2,?3)
             ON CONFLICT(project,tab) DO UPDATE SET owner_seal=excluded.owner_seal",
            params![project.as_str(), tab.name(), overview_seal(text)],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Offers a text an agent run produced for a tab. A tab with no file is written; a file
/// that already holds the text drops any proposal; with "Apply when I have not edited the
/// tab" an unedited file is replaced; otherwise the text is stored as the tab's one
/// proposal and the file is untouched. Inputs: the connection, the project, its folder,
/// the tab and the text. Output: what the text became, or the refusal.
pub fn offer(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
    text: &str,
) -> Result<Offered, EngineError> {
    checked(text)?;
    let _writing = writing();
    let place = place(folder)?;
    let now = match found_in(&place, tab) {
        Found::Blocked(_) => return Err(failure(TAB_BLOCKED)),
        Found::Missing => None,
        Found::Text(now) => Some(now),
    };
    if now.as_deref() == Some(text) {
        reject(db, project, tab)?;
        return Ok(Offered::Same);
    }
    let replaces = match &now {
        None => true,
        Some(now) => {
            let seal = tab_row(db, project, tab)?.seal;
            let edited = seal.as_deref() != Some(overview_seal(now).as_str());
            !edited && state(db, project)?.apply == Apply::WhenUnedited
        }
    };
    if replaces {
        write_tab(&place, tab, text, now.is_none())?;
        wrote(db, project, tab, text)?;
        return Ok(Offered::Written);
    }
    let stored = db
        .execute(
            "INSERT INTO project_overview_tabs(project,tab,seal,proposal) VALUES(?1,?2,NULL,?3)
             ON CONFLICT(project,tab) DO UPDATE SET proposal=excluded.proposal",
            params![project.as_str(), tab.name(), text],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(Offered::Proposed)
}

/// Saves the owner's edit of a tab as its file, only at the version the owner saw, checked
/// under the one-writer lock. The engine's seal is kept and the owner's is stored, so the
/// tab shows as the owner's edit, never as a file the project's files brought; a proposal
/// equal to the saved text is dropped. Inputs: the connection, the project, its folder, the
/// tab, the text and the version. Output: unit, or the refusal (`overview_tab_changed` at
/// another version).
pub fn save(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
    text: &str,
    version: &str,
) -> Result<(), EngineError> {
    checked(text)?;
    let _writing = writing();
    let place = place(folder)?;
    let found = found_in(&place, tab);
    if owner_version(&found)? != version {
        return Err(failure(TAB_CHANGED));
    }
    write_tab(&place, tab, text, matches!(found, Found::Missing))?;
    saved(db, project, tab, text)?;
    db.execute(
        "UPDATE project_overview_tabs SET proposal=NULL
         WHERE project=?1 AND tab=?2 AND proposal=?3",
        params![project.as_str(), tab.name(), text],
    )
    .map_err(unavailable)?;
    Ok(())
}

/// Writes a tab's proposal as its file, only when it is the proposal the owner reviewed and
/// the file is at the version the diff was made from, both checked under the one-writer
/// lock, then stores its seal and drops the proposal. Inputs: the connection, the project,
/// its folder, the tab, the file's version and the proposal's version. Output: unit,
/// `overview_no_proposal`, `overview_proposal_changed`, `overview_tab_changed` or another
/// refusal.
pub fn accept(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
    version: &str,
    proposal_version: &str,
) -> Result<(), EngineError> {
    let _writing = writing();
    let proposal = tab_row(db, project, tab)?.proposal;
    let proposal = proposal.ok_or_else(|| failure(NO_PROPOSAL))?;
    if overview_seal(&proposal) != proposal_version {
        return Err(failure(PROPOSAL_CHANGED));
    }
    checked(&proposal)?;
    let place = place(folder)?;
    let found = found_in(&place, tab);
    if owner_version(&found)? != version {
        return Err(failure(TAB_CHANGED));
    }
    write_tab(&place, tab, &proposal, matches!(found, Found::Missing))?;
    wrote(db, project, tab, &proposal)
}

/// Drops a tab's proposal; the file is untouched. Inputs: the connection, the project and
/// the tab. Output: unit, also when there was none, or `overview_unavailable`.
pub fn reject(db: &Connection, project: &ProjectId, tab: Tab) -> Result<(), EngineError> {
    let dropped = db
        .execute(
            "UPDATE project_overview_tabs SET proposal=NULL WHERE project=?1 AND tab=?2",
            params![project.as_str(), tab.name()],
        )
        .map_err(unavailable)?;
    assert!(dropped <= 1);
    Ok(())
}

/// Marks the project's Overview stale after a landed change set, due once the quiet period
/// has passed, only while its tabs refresh after landings (the default with no row). A
/// run that already waits to start at once (the first read, a Refresh) keeps its time.
/// Inputs: the landing's connection, the project and the time. Output: unit, or
/// `overview_unavailable`.
pub fn mark_stale(db: &Connection, project: &ProjectId, now_ms: i64) -> Result<(), EngineError> {
    assert!(now_ms >= 0);
    let due_ms = now_ms.saturating_add(QUIET_MS);
    assert!(due_ms >= now_ms);
    db.execute(
        "INSERT INTO project_overview(project,stale,due_ms) VALUES(?1,1,?2)
         ON CONFLICT(project) DO UPDATE SET stale=1,
           due_ms=CASE WHEN stale=1 AND due_ms=0 THEN 0 ELSE excluded.due_ms END
         WHERE refresh='after_landing'",
        params![project.as_str(), due_ms],
    )
    .map_err(unavailable)?;
    Ok(())
}

/// Marks a project's tabs stale after one of its change sets landed, making the Overview
/// tables first. A failure is logged by its plain code and never fails the landing. Inputs:
/// the landing's connection and the project.
pub fn mark_landed(db: &Connection, project: &ProjectId) {
    let marked =
        initialize(db).and_then(|()| mark_stale(db, project, crate::team::now_ms().max(0)));
    if let Err(error) = marked {
        eprintln!(
            "project_overview_mark_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// Asks for a writing run at once (the Refresh button). Inputs: the connection and the
/// project. Output: unit, `overview_off` while the project's Overview tabs are off, or
/// `overview_unavailable`.
pub fn refresh(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    if state(db, project)?.refresh == Refresh::Off {
        return Err(failure(OFF));
    }
    let stored = db
        .execute(
            "INSERT INTO project_overview(project,stale,due_ms) VALUES(?1,1,0)
             ON CONFLICT(project) DO UPDATE SET stale=1, due_ms=0",
            [project.as_str()],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Stores when the project's tabs are refreshed ("Overview tabs" in Project settings).
/// Inputs: the connection, the project and the choice. Output: unit, or
/// `overview_unavailable`.
pub fn set_refresh(
    db: &Connection,
    project: &ProjectId,
    refresh: Refresh,
) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO project_overview(project,refresh) VALUES(?1,?2)
             ON CONFLICT(project) DO UPDATE SET refresh=excluded.refresh",
            params![project.as_str(), refresh_name(refresh)],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// Stores what happens to a refreshed tab ("Refreshed tabs" in Project settings). Inputs:
/// the connection, the project and the choice. Output: unit, or `overview_unavailable`.
pub fn set_apply(db: &Connection, project: &ProjectId, apply: Apply) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO project_overview(project,apply) VALUES(?1,?2)
             ON CONFLICT(project) DO UPDATE SET apply=excluded.apply",
            params![project.as_str(), apply.name()],
        )
        .map_err(unavailable)?;
    assert_eq!(stored, 1);
    Ok(())
}

/// The length of the longest prefix of `text` within `max` bytes that ends after a
/// newline, or at a character boundary when no line ends inside the bound. Inputs: the
/// text and the bound. Output: the length in bytes, the whole text when it fits.
pub(crate) fn cut_at_line(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = text[..end].rfind('\n').map_or(end, |at| at + 1);
    assert!(cut <= max);
    cut
}

/// The repository's README as the Overview shows it: the first usual README name at the
/// folder root that is a regular file, opened through no link, read to at most
/// [`README_BYTES_MAX`] and cut at its last whole line when it is longer. Input: the
/// project folder. Output: the README, none without one or for one that is not text.
pub fn readme(folder: &Path) -> Option<Readme> {
    let root = draft::open_folder(folder)?;
    let (name, file) = README_NAMES.iter().find_map(|name| {
        let file = draft::open_in(&root, name)?;
        file.metadata()
            .is_ok_and(|metadata| metadata.is_file())
            .then_some((*name, file))
    })?;
    let mut bytes = Vec::new();
    file.take(README_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let cut = bytes.len() as u64 > README_BYTES_MAX;
    bytes.truncate(README_BYTES_MAX as usize);
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            if !cut || bytes.len() - valid > 3 {
                return None;
            }
            bytes.truncate(valid);
            String::from_utf8(bytes).ok()?
        }
    };
    if cut {
        let end = text.rfind('\n').map_or(text.len(), |at| at + 1);
        text.truncate(end);
    }
    assert!(text.len() as u64 <= README_BYTES_MAX);
    if text.trim().is_empty() {
        return None;
    }
    Some(Readme {
        path: name.to_owned(),
        text,
        cut,
    })
}

/// Whether the engine database holds a table. Inputs: the connection and the table name.
/// Output: the answer, or `overview_unavailable`.
fn present(db: &Connection, table: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(unavailable)
}

/// The text of a research job's report. The stored path is relative to the project's
/// state folder, where the research runner writes it, and is opened with no link followed
/// from that folder down, within the bound the runner writes a report under, so every
/// report the runner stored reads. Inputs: the project folder and the stored path. Output:
/// the text, or none for a report that is missing, longer than
/// [`REPORT_BYTES_MAX`](crate::research::runner::REPORT_BYTES_MAX) or that the links
/// reader refuses.
pub(crate) fn report_text(folder: &Path, path: &str) -> Option<String> {
    links::read_within(
        &crate::projects_home::state_folder(folder),
        path,
        crate::research::runner::REPORT_BYTES_MAX,
    )
}

/// What the Competitors tab can build on: the report of the project's newest finished
/// competitor research job when it reads as text, else the competitor research files the
/// owner linked that read as text through no link, else nothing. Inputs: the connection,
/// the project and its folder. Output: the state, or `overview_unavailable`.
pub fn competitors(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
) -> Result<Competitors, EngineError> {
    if present(db, "research_jobs")? {
        let report: Option<String> = db
            .query_row(
                "SELECT report_path FROM research_jobs
                 WHERE project=?1 AND kind=?2 AND status='done' AND report_path IS NOT NULL
                 ORDER BY id DESC LIMIT 1",
                params![project.as_str(), ResearchKind::CompetitorResearch.name()],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let readable = |path: &String| {
            path.len() <= REPORT_PATH_BYTES_MAX && report_text(folder, path).is_some()
        };
        if let Some(path) = report.filter(readable) {
            return Ok(Competitors::Report { path });
        }
    }
    if !present(db, "project_doc_links")? {
        return Ok(Competitors::Missing);
    }
    let paths: Vec<String> = links::list(db, project)
        .map_err(unavailable)?
        .into_iter()
        .filter(|link| link.kind == LinkKind::Research(ResearchKind::CompetitorResearch))
        .map(|link| link.path)
        .filter(|path| links::read_linked(folder, path).is_some())
        .collect();
    assert!(paths.len() <= links::LINKS_MAX);
    Ok(if paths.is_empty() {
        Competitors::Missing
    } else {
        Competitors::Linked { paths }
    })
}

/// One tab as the Overview shows it: its file, whose seal it carries, and its proposal.
/// Inputs: the connection, the project, its tabs folder and the tab. Output: the tab's
/// state, or `overview_unavailable`.
fn tab_state(
    db: &Connection,
    project: &ProjectId,
    place: &Place,
    tab: Tab,
) -> Result<TabState, EngineError> {
    let row = tab_row(db, project, tab)?;
    let found = match place {
        Place::Open(folder) => found_in(folder, tab),
        Place::NotAFolder => Found::Blocked(BLOCKED_NOT_A_FOLDER),
        Place::Missing | Place::NoLayout => Found::Missing,
    };
    let (text, blocked) = match found {
        Found::Text(text) => (Some(text), None),
        Found::Blocked(why) => (None, Some(why.to_owned())),
        Found::Missing => (None, None),
    };
    let version = text.as_deref().map(overview_seal).unwrap_or_default();
    let engine_wrote = row.seal.as_deref() == Some(version.as_str());
    let owner_saved = row.owner_seal.as_deref() == Some(version.as_str());
    let edited = text.is_some() && !engine_wrote && owner_saved;
    let foreign = text.is_some() && !engine_wrote && !owner_saved;
    assert!(!(edited && foreign));
    let proposal = row.proposal;
    let proposal_version = proposal.as_deref().map(overview_seal).unwrap_or_default();
    assert_eq!(proposal.is_none(), proposal_version.is_empty());
    Ok(TabState {
        tab,
        text,
        version,
        edited,
        foreign,
        proposal,
        proposal_version,
        blocked,
    })
}

/// Whether a tab's file came with the project's files, by the same check the Overview's
/// read shows, with none of the read's other work: neither the engine wrote it nor the
/// owner saved it here (RV-DS241-2). Inputs: the connection, the project, its folder and
/// the tab. Output: the answer, false with no file, or `overview_unavailable`.
pub fn tab_foreign(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
) -> Result<bool, EngineError> {
    let state = tab_state(db, project, &tabs_folder(folder, false), tab)?;
    assert!(!state.foreign || state.text.is_some());
    Ok(state.foreign)
}

/// Reads the project's Overview. A read makes a project's row stale and due at once while
/// no writing run has ever started for it under After each landed change set, even when a
/// setting saved earlier made the row, which asks for the first writing run; a project with
/// no `.crazyagent` folder gets no row and `needs_home` instead, so no run is asked for it.
/// The README and the competitor state are read as well. A run counts as running for as long as its claim is
/// open: the engine closes it when it ends or its time is over, and at engine start.
/// Inputs: the connection, the project, its folder and the time. Output: the Overview
/// with the four tabs in order, or `overview_unavailable`.
pub fn read(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    now_ms: i64,
) -> Result<Overview, EngineError> {
    assert!(now_ms >= 0);
    let place = tabs_folder(folder, false);
    let needs_home = matches!(place, Place::NoLayout);
    if !needs_home {
        db.execute(
            "INSERT INTO project_overview(project,stale,due_ms) VALUES(?1,1,0)
             ON CONFLICT(project) DO UPDATE SET stale=1, due_ms=0
             WHERE refresh='after_landing' AND stale=0 AND checked_ms IS NULL
               AND running_ms IS NULL AND failure IS NULL",
            [project.as_str()],
        )
        .map_err(unavailable)?;
    }
    let state = state(db, project)?;
    let mut tabs = Vec::with_capacity(Tab::ALL.len());
    for tab in Tab::ALL {
        tabs.push(tab_state(db, project, &place, tab)?);
    }
    assert_eq!(tabs.len(), Tab::ALL.len());
    let failure = state
        .failure
        .filter(|_| !needs_home)
        .map(|failure| clipped(failure, FAILURE_BYTES_MAX));
    assert!(
        failure
            .as_ref()
            .is_none_or(|failure| failure.len() <= FAILURE_BYTES_MAX)
    );
    Ok(Overview {
        readme: readme(folder),
        tabs,
        competitors: competitors(db, project, folder)?,
        running: state.running_ms.is_some(),
        stale: state.stale && !needs_home,
        checked_ms: state.checked_ms,
        waiting_for_account: failure.as_deref().is_some_and(run::waits_for_account),
        failure,
        refresh: state.refresh,
        apply: state.apply,
        needs_home,
    })
}
