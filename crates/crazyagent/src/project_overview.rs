use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::onboarding::draft::{self, Folder, overview_seal};
use crate::protocol::ProjectId;
use crate::roadmap_status::{QUIET_MS, Refresh};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(test)]
mod tests;

/// The folder inside a project's `.crazyagent` that holds the tab files.
pub const TABS_FOLDER: &str = "overview";
/// The most bytes of one tab's text, as a file and as a proposal.
pub const TAB_BYTES_MAX: usize = 64 * 1024;
/// The most bytes of the project's README the Overview shows.
pub const README_BYTES_MAX: u64 = 1024 * 1024;
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
/// A refresh was asked for while the project's Overview tabs are off.
pub const OFF: &str = "overview_off";
/// The engine's store could not be read or written.
pub const UNAVAILABLE: &str = "overview_unavailable";

const LAYOUT: &str = ".crazyagent";
const TEMP_RANDOM_BYTES: usize = 8;

const _: () = assert!(TAB_BYTES_MAX <= 64 * 1024);
const _: () = assert!(TAB_BYTES_MAX as u64 <= README_BYTES_MAX);
const _: () = assert!(FAILURE_BYTES_MAX > 0 && TEMP_RANDOM_BYTES >= 8);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_overview(
  project TEXT PRIMARY KEY,
  refresh TEXT NOT NULL DEFAULT 'after_landing' CHECK(refresh IN ('after_landing','refresh_only','off')),
  apply TEXT NOT NULL DEFAULT 'always_ask' CHECK(apply IN ('always_ask','when_unedited')),
  stale INTEGER NOT NULL DEFAULT 0,
  due_ms INTEGER NOT NULL DEFAULT 0,
  running_ms INTEGER,
  checked_ms INTEGER,
  failure TEXT
);
CREATE TABLE IF NOT EXISTS project_overview_tabs(
  project TEXT NOT NULL,
  tab TEXT NOT NULL CHECK(tab IN ('quick_info','features','competitors','install_and_use')),
  seal TEXT,
  proposal TEXT,
  PRIMARY KEY(project, tab)
);
";

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
/// name, whether the owner changed it since the engine last wrote it, and a waiting proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub tab: Tab,
    pub text: Option<String>,
    pub version: String,
    pub edited: bool,
    pub proposal: Option<String>,
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
/// state, where the writing run stands and the project's two Overview settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub readme: Option<Readme>,
    pub tabs: Vec<TabState>,
    pub competitors: Competitors,
    pub running: bool,
    pub stale: bool,
    pub checked_ms: Option<i64>,
    pub failure: Option<String>,
    pub refresh: Refresh,
    pub apply: Apply,
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
/// neither reads nor replaces (a link, a folder, an empty, oversized or non-text file).
enum Found {
    Missing,
    Text(String),
    Blocked,
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

/// Creates the Overview tables. Input: the engine connection. Output: unit, or
/// `overview_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)
}

/// Opens the project's tab folder beneath its `.crazyagent` folder, each opened once and
/// never through a link, making the tab folder first with `create`. Inputs: the project
/// folder and whether to create. Output: the folder, or `None` when it is not a real folder.
#[cfg(unix)]
fn tabs_folder(project: &Path, create: bool) -> Option<Folder> {
    use rustix::fs::{Mode, OFlags};
    let layout = draft::open_folder(&project.join(LAYOUT))?;
    if create {
        match rustix::fs::mkdirat(&layout, TABS_FOLDER, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return None,
        }
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(&layout, TABS_FOLDER, flags, Mode::empty()).ok()
}

/// Checks the project's `.crazyagent` folder and its tab folder are real folders, making
/// the tab folder first with `create`: the path-based Windows form described on
/// [`Folder`]. Inputs: the project folder and whether to create. Output: the folder or `None`.
#[cfg(windows)]
fn tabs_folder(project: &Path, create: bool) -> Option<Folder> {
    let tabs = draft::open_folder(&project.join(LAYOUT))?.join(TABS_FOLDER);
    if create {
        match std::fs::DirBuilder::new().create(&tabs) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    draft::open_folder(&tabs)
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
/// Output: the text, or `Blocked` for anything else.
fn read_file(file: std::fs::File) -> Found {
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Found::Blocked;
    }
    let bound = TAB_BYTES_MAX as u64 + 1;
    let mut bytes = Vec::new();
    if file.take(bound).read_to_end(&mut bytes).is_err() {
        return Found::Blocked;
    }
    assert!(bytes.len() as u64 <= bound);
    if bytes.len() > TAB_BYTES_MAX {
        return Found::Blocked;
    }
    match String::from_utf8(bytes) {
        Ok(text) if !text.trim().is_empty() => Found::Text(text),
        _ => Found::Blocked,
    }
}

/// What sits where the tab's file goes in the opened tab folder, never read through a
/// link. Inputs: the open tab folder and the tab. Output: what was found.
fn found_in(folder: &Folder, tab: Tab) -> Found {
    let name = tab.file();
    match draft::open_in(folder, &name) {
        Some(file) => read_file(file),
        None if exists(folder, &name) => Found::Blocked,
        None => Found::Missing,
    }
}

/// The text of the project's tab file. Inputs: the project folder and the tab. Output:
/// the exact text, or `None` while there is no tab file or something else sits there.
fn tab_text(folder: &Path, tab: Tab) -> Option<String> {
    match found_in(&tabs_folder(folder, false)?, tab) {
        Found::Text(text) => Some(text),
        Found::Missing | Found::Blocked => None,
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
/// old text or the new, never a part. Inputs: the open folder, the file name and the bytes.
/// Output: any I/O failure; the partial file is removed on one.
#[cfg(unix)]
fn replace(folder: &Folder, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags};
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
            rustix::fs::renameat(folder, temp.as_str(), folder, name).map_err(std::io::Error::from)
        });
    if written.is_err() {
        let _ = rustix::fs::unlinkat(folder, temp.as_str(), AtFlags::empty());
    }
    written
}

/// Writes `bytes` to `name` in the folder through a fresh partial file, created as the
/// reparse point itself, and a rename: the path-based Windows form described on
/// [`Folder`]. Inputs: the folder, the file name and the bytes. Output: any I/O failure.
#[cfg(windows)]
fn replace(folder: &Folder, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = folder.join(temp_name(name)?);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temp, folder.join(name)));
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

/// Writes the tab's file in the opened tab folder and reads it back. Inputs: the open tab
/// folder, the tab and a checked text. Output: unit, `overview_tab_blocked` when the file
/// cannot be written, or `overview_tab_changed` when another writer replaced it at once.
fn write_tab(folder: &Folder, tab: Tab, text: &str) -> Result<(), EngineError> {
    assert!(text.len() <= TAB_BYTES_MAX);
    assert!(!text.trim().is_empty());
    replace(folder, &tab.file(), text.as_bytes()).map_err(|_| failure(TAB_BLOCKED))?;
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
        Found::Blocked => Err(failure(TAB_BLOCKED)),
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

/// The stored seal and proposal of one tab. Inputs: the connection, the project and the
/// tab. Output: the seal of the last text the engine wrote and the waiting proposal, both
/// `None` while there is no row.
fn tab_row(
    db: &Connection,
    project: &ProjectId,
    tab: Tab,
) -> Result<(Option<String>, Option<String>), EngineError> {
    let row = db
        .query_row(
            "SELECT seal,proposal FROM project_overview_tabs WHERE project=?1 AND tab=?2",
            params![project.as_str(), tab.name()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(row.unwrap_or((None, None)))
}

/// Records that the engine wrote `text` as the tab's file: stores its seal and drops any
/// proposal. Inputs: the connection, the project, the tab and the written text. Output:
/// unit, or `overview_unavailable`.
fn wrote(db: &Connection, project: &ProjectId, tab: Tab, text: &str) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO project_overview_tabs(project,tab,seal,proposal) VALUES(?1,?2,?3,NULL)
             ON CONFLICT(project,tab) DO UPDATE SET seal=excluded.seal, proposal=NULL",
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
    let place = tabs_folder(folder, true).ok_or_else(|| failure(TAB_BLOCKED))?;
    let now = match found_in(&place, tab) {
        Found::Blocked => return Err(failure(TAB_BLOCKED)),
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
            let (seal, _) = tab_row(db, project, tab)?;
            let edited = seal.as_deref() != Some(overview_seal(now).as_str());
            !edited && state(db, project)?.apply == Apply::WhenUnedited
        }
    };
    if replaces {
        write_tab(&place, tab, text)?;
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

/// Saves the owner's edit of a tab as its file, only at the version the owner saw. The
/// stored seal is kept, so the tab shows as edited; a proposal equal to the saved text is
/// dropped. Inputs: the connection, the project, its folder, the tab, the text and the
/// version. Output: unit, or the refusal (`overview_tab_changed` at another version).
pub fn save(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
    text: &str,
    version: &str,
) -> Result<(), EngineError> {
    checked(text)?;
    let place = tabs_folder(folder, true).ok_or_else(|| failure(TAB_BLOCKED))?;
    if version_of(&found_in(&place, tab))? != version {
        return Err(failure(TAB_CHANGED));
    }
    write_tab(&place, tab, text)?;
    db.execute(
        "UPDATE project_overview_tabs SET proposal=NULL
         WHERE project=?1 AND tab=?2 AND proposal=?3",
        params![project.as_str(), tab.name(), text],
    )
    .map_err(unavailable)?;
    Ok(())
}

/// Writes a tab's proposal as its file, only at the version the diff was made from, then
/// stores its seal and drops the proposal. Inputs: the connection, the project, its
/// folder, the tab and the version. Output: unit, `overview_no_proposal`,
/// `overview_tab_changed` or another refusal.
pub fn accept(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    tab: Tab,
    version: &str,
) -> Result<(), EngineError> {
    let (_, proposal) = tab_row(db, project, tab)?;
    let proposal = proposal.ok_or_else(|| failure(NO_PROPOSAL))?;
    checked(&proposal)?;
    let place = tabs_folder(folder, true).ok_or_else(|| failure(TAB_BLOCKED))?;
    if version_of(&found_in(&place, tab))? != version {
        return Err(failure(TAB_CHANGED));
    }
    write_tab(&place, tab, &proposal)?;
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
/// has passed, only while its tabs refresh after landings (the default with no row).
/// Inputs: the landing's transaction, the project and the time. Output: unit, or
/// `overview_unavailable`.
pub fn mark_stale(
    tx: &Transaction<'_>,
    project: &ProjectId,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(now_ms >= 0);
    let due_ms = now_ms.saturating_add(QUIET_MS);
    assert!(due_ms >= now_ms);
    tx.execute(
        "INSERT INTO project_overview(project,stale,due_ms) VALUES(?1,1,?2)
         ON CONFLICT(project) DO UPDATE SET stale=1, due_ms=excluded.due_ms
         WHERE refresh='after_landing'",
        params![project.as_str(), due_ms],
    )
    .map_err(unavailable)?;
    Ok(())
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

/// Reads the project's Overview. The first read of a project makes its row stale and due
/// at once, which asks for the first writing run. The README and the competitor state are
/// not filled here. Inputs: the connection, the project, its folder and the time. Output:
/// the Overview with the four tabs in order, or `overview_unavailable`.
pub fn read(
    db: &Connection,
    project: &ProjectId,
    folder: &Path,
    now_ms: i64,
) -> Result<Overview, EngineError> {
    assert!(now_ms >= 0);
    db.execute(
        "INSERT OR IGNORE INTO project_overview(project,stale,due_ms) VALUES(?1,1,0)",
        [project.as_str()],
    )
    .map_err(unavailable)?;
    let state = state(db, project)?;
    let mut tabs = Vec::with_capacity(Tab::ALL.len());
    for tab in Tab::ALL {
        let (seal, proposal) = tab_row(db, project, tab)?;
        let text = tab_text(folder, tab);
        let version = text.as_deref().map(overview_seal).unwrap_or_default();
        let edited = text.is_some() && seal.as_deref() != Some(version.as_str());
        tabs.push(TabState {
            tab,
            text,
            version,
            edited,
            proposal,
        });
    }
    assert_eq!(tabs.len(), Tab::ALL.len());
    assert!(
        state
            .failure
            .as_ref()
            .is_none_or(|failure| failure.len() <= FAILURE_BYTES_MAX)
    );
    Ok(Overview {
        readme: None,
        tabs,
        competitors: Competitors::Missing,
        running: state.running_ms.is_some(),
        stale: state.stale,
        checked_ms: state.checked_ms,
        failure: state.failure,
        refresh: state.refresh,
        apply: state.apply,
    })
}
