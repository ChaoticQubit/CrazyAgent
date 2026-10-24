use super::{KnowledgeEntry, KnowledgeStatus, ReviewDecision, error, hash, live_entry, load_entry};
use crate::config::EffectiveConfig;
use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::{FRAME_BYTES_MAX, Outcome, ProjectId, RequestId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const NOTE_BYTES_MAX: usize = 2000;
pub const CONTEXT_PATH_BYTES_MAX: usize = 512;
pub const SEARCH_BYTES_MAX: usize = 100;
pub const PAGE_ITEMS_MAX: u8 = 20;
pub const SCOPE_ITEMS_MAX: usize = 200;
pub const LIBRARY_ITEMS_MAX: usize = 2000;
pub const LAUNCH_ITEMS_MAX: usize = 24;
pub const LAUNCH_BYTES_MAX: usize = 12288;
const TEXT_BYTES_MAX: usize = 4096;
const PAGE_BYTES_MAX: usize = FRAME_BYTES_MAX - 8192;
const RENDER_HEADER_BYTES_MAX: usize = 1024;
const RESEARCH_ROWS_MAX: i64 = 4096;
const FORGET_ROWS_MAX: i64 = 16384;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS memory_library (
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    project TEXT,
    origin TEXT NOT NULL CHECK(length(CAST(origin AS BLOB)) BETWEEN 2 AND 1024),
    text TEXT NOT NULL CHECK(length(CAST(text AS BLOB)) BETWEEN 1 AND 4096)
);
CREATE INDEX IF NOT EXISTS memory_library_scope ON memory_library(project, ordinal);";

/// Where a saved memory applies: everywhere, or inside one project only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryScope {
    Global,
    Project { project_id: ProjectId },
}

impl MemoryScope {
    fn project(&self) -> Option<&str> {
        match self {
            Self::Global => None,
            Self::Project { project_id } => Some(project_id.as_str()),
        }
    }
}

/// How a saved memory came to exist: typed by the owner, or promoted by the
/// owner from a kept project memory at the named entry version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryOrigin {
    Written,
    Promoted {
        project_id: ProjectId,
        entry_id: RequestId,
        version: u64,
    },
    Selected {
        path: String,
    },
}

/// One memory the owner saved by hand or promoted to global memory.
/// `ordinal` is only a paging cursor and is never shown to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedMemory {
    pub id: RequestId,
    pub ordinal: u64,
    pub scope: MemoryScope,
    pub origin: MemoryOrigin,
    pub text: String,
}

/// One page of saved memories, newest first; `next` is passed back as `before`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPage {
    pub items: Vec<SavedMemory>,
    pub next: Option<u64>,
}

/// An explicit owner change to saved memory. Nothing else writes this store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryAction {
    Add {
        scope: MemoryScope,
        text: String,
    },
    Promote {
        project_id: ProjectId,
        entry_id: RequestId,
        version: u64,
    },
    Delete {
        memory_id: RequestId,
    },
    Forget {
        project_id: ProjectId,
        entry_id: RequestId,
    },
    Remember {
        project_id: ProjectId,
        research: crate::project_setup::ResearchKind,
        section: Option<String>,
    },
    SaveSelection {
        project_id: ProjectId,
        path: String,
        text: String,
    },
}

impl LibraryAction {
    /// Checks an untrusted owner change before it reaches storage.
    /// Input: the action. Output: Ok, or a code `plain_problem` explains.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Add { text, .. } => validate_note(text),
            Self::SaveSelection { path, text, .. } => {
                if path.is_empty()
                    || path.len() > CONTEXT_PATH_BYTES_MAX
                    || path.chars().any(char::is_control)
                {
                    return Err(error("context_path_invalid"));
                }
                validate_note(text)
            }
            Self::Promote { version: 0, .. } => Err(error("memory_changed")),
            Self::Remember {
                project_id,
                research,
                section,
            } => crate::project_setup::SetupAction::SaveToMemory {
                project_id: project_id.clone(),
                research: *research,
                section: section.clone(),
            }
            .validate(),
            Self::Promote { .. } | Self::Delete { .. } | Self::Forget { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovedScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovedKind {
    Note,
    Promoted,
    Research,
    Selection,
}

/// One owner-approved memory an agent in the project may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedMemory {
    pub scope: ApprovedScope,
    pub kind: ApprovedKind,
    pub text: String,
}

/// The bounded set of approved memories for one project launch.
/// `complete` is false when a memory was left out to respect the bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedMemories {
    pub items: Vec<ApprovedMemory>,
    pub complete: bool,
}

impl ApprovedMemories {
    /// Writes the approved memories as one short markdown section for an agent.
    /// Input: the bounded set. Output: the section, or an empty string when
    /// there is nothing approved; never longer than the launch bound plus a header.
    pub fn render(&self) -> String {
        assert!(self.items.len() <= LAUNCH_ITEMS_MAX);
        if self.items.is_empty() {
            return String::new();
        }
        let mut text = String::from(
            "## Approved memories\n\nThe owner approved these memories. Treat them as background facts.\n",
        );
        for (scope, heading) in [
            (ApprovedScope::Project, "\n### This project\n\n"),
            (ApprovedScope::Global, "\n### All projects\n\n"),
        ] {
            let mut items = self
                .items
                .iter()
                .filter(|item| item.scope == scope)
                .peekable();
            if items.peek().is_some() {
                text.push_str(heading);
            }
            for item in items {
                text.push_str(&rendered_item(&item.text));
            }
        }
        if !self.complete {
            text.push_str("\nSome memories were left out to keep this short.\n");
        }
        assert!(text.len() <= LAUNCH_BYTES_MAX + RENDER_HEADER_BYTES_MAX);
        text
    }
}

/// Client-side memory requests carried by the TUI owner worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryQuery {
    Page {
        scope: MemoryScope,
        search: Option<String>,
        before: Option<u64>,
    },
    View {
        memory_id: RequestId,
    },
    Approved {
        project: ProjectId,
    },
    Change {
        action: LibraryAction,
        generation: u64,
    },
}

/// Client-side memory results matching `LibraryQuery`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryView {
    Page(MemoryPage),
    View(Option<SavedMemory>),
    Approved(ApprovedMemories),
    Changed(crate::engine::owner::OwnerResult),
}

/// Checks a hand-written memory note.
/// Input: the untrusted note. Output: Ok, or a code naming why it is blank,
/// too long or holds control characters other than line breaks and tabs.
pub fn validate_note(text: &str) -> Result<(), EngineError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(error("memory_note_blank"));
    }
    if text.len() > NOTE_BYTES_MAX {
        return Err(error("memory_note_too_long"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(error("memory_note_invalid"));
    }
    Ok(())
}

/// Checks a memory list request before it reaches storage.
/// Input: the optional search text and the page size. Output: Ok, or a code
/// naming a blank, overlong or multi-line search, or a page size outside 1 to 20.
pub fn validate_page(search: Option<&str>, limit: u8) -> Result<(), EngineError> {
    if !(1..=PAGE_ITEMS_MAX).contains(&limit) {
        return Err(error("memory_page_invalid"));
    }
    if let Some(search) = search
        && (search.trim().is_empty()
            || search.len() > SEARCH_BYTES_MAX
            || search.chars().any(char::is_control))
    {
        return Err(error("memory_search_invalid"));
    }
    Ok(())
}

const CODES: [&str; 14] = [
    "memory_selection_stale",
    "memory_source_removed",
    "memory_note_blank",
    "memory_note_too_long",
    "memory_note_invalid",
    "memory_duplicate",
    "memory_full",
    "memory_missing",
    "memory_changed",
    "memory_not_kept",
    "memory_already_global",
    "memory_project_missing",
    "memory_search_invalid",
    "memory_page_invalid",
];

/// Finds the fixed spelling of a saved-memory refusal code for clients that
/// keep codes as static strings. Input: any code. Output: the same code, or None
/// when it is not a saved-memory code.
pub fn known_code(code: &str) -> Option<&'static str> {
    let known = CODES.iter().find(|known| **known == code).copied();
    assert!(known.is_none() || plain_problem(code).is_some());
    known
}

/// Turns a memory refusal code into the sentence shown to the owner.
/// Input: an engine rejection code. Output: the plain sentence, or None for
/// codes that are not about saved memory.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    match code {
        "memory_note_blank" => Some("Write something to remember."),
        "memory_note_too_long" => Some("Keep the memory under 2000 characters."),
        "memory_note_invalid" => Some("Remove special control characters from the memory."),
        "memory_duplicate" => Some("This memory is already saved."),
        "memory_full" => Some("Memory is full. Delete an old memory first."),
        "memory_missing" => Some("That memory no longer exists."),
        "memory_changed" => Some("This memory changed. Reload it and try again."),
        "memory_not_kept" => Some("Only memories you kept can be made global."),
        "memory_already_global" => Some("This memory is already in global memory."),
        "memory_project_missing" => Some("That project no longer exists."),
        "memory_search_invalid" => Some("Keep the search on one line, up to 100 characters."),
        "memory_page_invalid" => Some("Show between 1 and 20 memories at a time."),
        "memory_selection_stale" => {
            Some("This text is no longer in that file. Open the file again and select it again.")
        }
        "memory_source_removed" => Some(
            "This memory's source was removed. Delete the memory, then save the research again.",
        ),
        _ => None,
    }
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

fn rejected(code: &str) -> Result<Outcome, EngineError> {
    Ok(Outcome::Rejected { code: code.into() })
}

/// Applies one owner change to saved memory inside the owner transaction.
/// Input: the transaction, current config, the action and the current revision.
/// Output: Applied, or a plain rejection that leaves storage unchanged.
pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &LibraryAction,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if let Err(problem) = action.validate() {
        return rejected(&problem.0);
    }
    match action {
        LibraryAction::Add { scope, text } => add(tx, scope, text.trim(), revision),
        LibraryAction::Promote {
            project_id,
            entry_id,
            version,
        } => promote(tx, config, project_id, entry_id, *version, revision),
        LibraryAction::Delete { memory_id } => delete(tx, memory_id),
        LibraryAction::Forget {
            project_id,
            entry_id,
        } => forget(tx, project_id, entry_id),
        LibraryAction::Remember {
            project_id,
            research,
            section,
        } => remember(
            tx,
            config,
            project_id,
            *research,
            section.as_deref(),
            revision,
        ),
        LibraryAction::SaveSelection {
            project_id,
            path,
            text,
        } => save_selection(tx, project_id, path, text.trim(), revision),
    }
}

fn save_selection(
    tx: &Transaction<'_>,
    project: &ProjectId,
    path: &str,
    text: &str,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let source = match crate::project_setup::context_text(tx, project, path)? {
        Ok(source) => source,
        Err(code) => return rejected(code),
    };
    if !selected_from(&source, text) {
        return rejected("memory_selection_stale");
    }
    let scope = MemoryScope::Project {
        project_id: project.clone(),
    };
    let digest = hash(format!("{revision}\n{}\n{path}\n{text}", project.as_str()).as_bytes());
    let id = RequestId::new(format!("selection-{}", &digest[..32])).map_err(error)?;
    if let Some(code) = blocked(tx, &id, &scope, text, "memory_duplicate")? {
        return rejected(code);
    }
    let origin = MemoryOrigin::Selected { path: path.into() };
    insert(tx, &id, &scope, &origin, text)
}

/// Removes markdown link targets so file text reads as the owner saw it rendered.
/// Input: the raw file text. Output: the text with every `](target)` reduced to `]`.
fn readable(source: &str) -> String {
    let mut text = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("](") {
        text.push_str(&rest[..=start]);
        let target = &rest[start + 2..];
        match target.find(')') {
            Some(end) if !target[..end].contains('\n') => rest = &target[end + 1..],
            _ => {
                text.push('(');
                rest = target;
            }
        }
    }
    text.push_str(rest);
    assert!(text.len() <= source.len());
    text
}

/// Splits text into lowercase words, ignoring punctuation, markup and spacing.
/// Input: any text. Output: the words in order.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Finds a run of words inside a longer run in linear time.
/// Input: the file words and the nonempty selected words. Output: whether they appear
/// together, in order, with nothing between them.
fn contains_words(haystack: &[String], needle: &[String]) -> bool {
    assert!(!needle.is_empty());
    let mut fallback = vec![0usize; needle.len()];
    let mut length = 0;
    for index in 1..needle.len() {
        while length > 0 && needle[index] != needle[length] {
            length = fallback[length - 1];
        }
        if needle[index] == needle[length] {
            length += 1;
        }
        fallback[index] = length;
    }
    let mut matched = 0;
    for word in haystack {
        while matched > 0 && word != &needle[matched] {
            matched = fallback[matched - 1];
        }
        if word == &needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            return true;
        }
    }
    assert!(matched < needle.len());
    false
}

/// Checks that a selection the owner made in the reader still comes from the file.
/// Input: the current file text and the trimmed selection. Output: true when the
/// selection's words appear together in the file as rendered, or, for a selection
/// without words, when the file holds it exactly.
fn selected_from(source: &str, selection: &str) -> bool {
    let needle = words(selection);
    if needle.is_empty() {
        return source.contains(selection);
    }
    contains_words(&words(&readable(source)), &needle)
}

fn remember(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    research: crate::project_setup::ResearchKind,
    section: Option<&str>,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let text = match crate::project_setup::research_text(tx, project, research, section)? {
        Ok(text) => text,
        Err(code) => return rejected(code),
    };
    let id = super::research_entry_id(project, research.name(), &text)?;
    if load_entry(tx, &id)?.is_none() {
        let outcome =
            super::propose_research(tx, config, project, research.name(), &text, revision)?;
        if outcome != Outcome::Applied {
            return Ok(outcome);
        }
    }
    let entry = load_entry(tx, &id)?.ok_or_else(|| error("memory_insert_lost"))?;
    assert_eq!(&entry.project_id, project);
    if entry.status == KnowledgeStatus::Confirmed {
        return rejected("memory_duplicate");
    }
    for source in &entry.source_ids {
        if !super::source_live(tx, project, source)? {
            return rejected("memory_source_removed");
        }
    }
    let outcome = super::review(
        tx,
        config,
        &entry.id,
        entry.version,
        &ReviewDecision::Keep,
        revision,
    )?;
    if outcome != Outcome::Applied {
        return Err(error("memory_keep_failed"));
    }
    let kept = load_entry(tx, &id)?.ok_or_else(|| error("memory_keep_failed"))?;
    assert_eq!(kept.status, KnowledgeStatus::Confirmed);
    assert!(kept.human_reviewed);
    Ok(Outcome::Applied)
}

fn project_exists(tx: &Transaction<'_>, project: &ProjectId) -> Result<bool, EngineError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project.as_str()],
        |row| row.get(0),
    )
    .map_err(error)
}

fn add(
    tx: &Transaction<'_>,
    scope: &MemoryScope,
    text: &str,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if let MemoryScope::Project { project_id } = scope
        && !project_exists(tx, project_id)?
    {
        return rejected("memory_project_missing");
    }
    let digest = hash(format!("{revision}\n{}\n{text}", scope.project().unwrap_or("")).as_bytes());
    let id = RequestId::new(format!("note-{}", &digest[..32])).map_err(error)?;
    if let Some(code) = blocked(tx, &id, scope, text, "memory_duplicate")? {
        return rejected(code);
    }
    insert(tx, &id, scope, &MemoryOrigin::Written, text)
}

fn blocked(
    tx: &Transaction<'_>,
    id: &RequestId,
    scope: &MemoryScope,
    text: &str,
    duplicate: &'static str,
) -> Result<Option<&'static str>, EngineError> {
    let (exists, in_scope, total): (bool, i64, i64) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_library WHERE id=?1 OR (project IS ?2 AND text=?3)),
            (SELECT COUNT(*) FROM memory_library WHERE project IS ?2),
            (SELECT COUNT(*) FROM memory_library)",
            params![id.as_str(), scope.project(), text],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(error)?;
    if exists {
        return Ok(Some(duplicate));
    }
    if in_scope >= SCOPE_ITEMS_MAX as i64 || total >= LIBRARY_ITEMS_MAX as i64 {
        return Ok(Some("memory_full"));
    }
    assert!(in_scope <= total);
    Ok(None)
}

fn insert(
    tx: &Transaction<'_>,
    id: &RequestId,
    scope: &MemoryScope,
    origin: &MemoryOrigin,
    text: &str,
) -> Result<Outcome, EngineError> {
    assert!(!text.trim().is_empty());
    assert!(text.len() <= TEXT_BYTES_MAX);
    tx.execute(
        "INSERT INTO memory_library(id,project,origin,text) VALUES (?1,?2,?3,?4)",
        params![
            id.as_str(),
            scope.project(),
            serde_json::to_string(origin).map_err(error)?,
            text
        ],
    )
    .map_err(error)?;
    let saved = get(tx, id)?.ok_or_else(|| error("memory_insert_lost"))?;
    assert_eq!(&saved.scope, scope);
    assert_eq!(saved.text, text);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn promote(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    entry_id: &RequestId,
    version: u64,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let Some(entry) = load_entry(tx, entry_id)? else {
        return rejected("memory_missing");
    };
    if &entry.project_id != project {
        return rejected("memory_missing");
    }
    if entry.version != version {
        return rejected("memory_changed");
    }
    if !live_entry(tx, &entry)? {
        return rejected("memory_not_kept");
    }
    let digest = hash(format!("{}\n{}", entry.id.as_str(), entry.content_hash).as_bytes());
    let id = RequestId::new(format!("global-{}", &digest[..32])).map_err(error)?;
    if let Some(code) = blocked(
        tx,
        &id,
        &MemoryScope::Global,
        &entry.text,
        "memory_already_global",
    )? {
        return rejected(code);
    }
    let entry = kept(tx, config, entry, revision)?;
    insert(
        tx,
        &id,
        &MemoryScope::Global,
        &MemoryOrigin::Promoted {
            project_id: project.clone(),
            entry_id: entry.id.clone(),
            version: entry.version,
        },
        &entry.text,
    )
}

fn kept(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    entry: KnowledgeEntry,
    revision: u64,
) -> Result<KnowledgeEntry, EngineError> {
    if entry.status == KnowledgeStatus::Confirmed {
        return Ok(entry);
    }
    assert_eq!(entry.status, KnowledgeStatus::Provisional);
    let outcome = super::review(
        tx,
        config,
        &entry.id,
        entry.version,
        &ReviewDecision::Keep,
        revision,
    )?;
    if outcome != Outcome::Applied {
        return Err(error("memory_keep_failed"));
    }
    let confirmed = load_entry(tx, &entry.id)?.ok_or_else(|| error("memory_keep_failed"))?;
    assert_eq!(confirmed.status, KnowledgeStatus::Confirmed);
    assert_eq!(confirmed.content_hash, entry.content_hash);
    Ok(confirmed)
}

fn delete(tx: &Transaction<'_>, id: &RequestId) -> Result<Outcome, EngineError> {
    let count = tx
        .execute("DELETE FROM memory_library WHERE id=?1", [id.as_str()])
        .map_err(error)?;
    if count == 0 {
        return rejected("memory_missing");
    }
    assert_eq!(count, 1);
    assert!(get(tx, id)?.is_none());
    Ok(Outcome::Applied)
}

fn bodies<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    sql: &str,
    project: &ProjectId,
) -> Result<Vec<T>, EngineError> {
    let mut statement = tx.prepare(sql).map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), FORGET_ROWS_MAX], |r| {
            r.get::<_, String>(0)
        })
        .map_err(error)?;
    let mut values = Vec::new();
    for body in rows {
        values.push(serde_json::from_str(&body.map_err(error)?).map_err(error)?);
    }
    if values.len() as i64 >= FORGET_ROWS_MAX {
        return Err(error("memory_forget_bounds"));
    }
    Ok(values)
}

fn forget(
    tx: &Transaction<'_>,
    project: &ProjectId,
    entry_id: &RequestId,
) -> Result<Outcome, EngineError> {
    let Some(entry) = load_entry(tx, entry_id)? else {
        return rejected("memory_missing");
    };
    if &entry.project_id != project {
        return rejected("memory_missing");
    }
    let versions = tx
        .execute(
            "DELETE FROM knowledge_entries WHERE id=?1",
            [entry_id.as_str()],
        )
        .map_err(error)?;
    tx.execute(
        "DELETE FROM knowledge_heads WHERE id=?1",
        [entry_id.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM knowledge_globals WHERE entry=?1",
        [entry_id.as_str()],
    )
    .map_err(error)?;
    let remaining: Vec<KnowledgeEntry> = bodies(
        tx,
        "SELECT body FROM knowledge_entries WHERE project=?1 LIMIT ?2",
        project,
    )?;
    if !remaining
        .iter()
        .any(|other| other.nomination_id == entry.nomination_id)
    {
        tx.execute(
            "DELETE FROM knowledge_nominations WHERE id=?1 AND project=?2",
            params![entry.nomination_id.as_str(), project.as_str()],
        )
        .map_err(error)?;
    }
    let nominations: Vec<super::Nomination> = bodies(
        tx,
        "SELECT body FROM knowledge_nominations WHERE project=?1 LIMIT ?2",
        project,
    )?;
    let mut dropped = BTreeSet::new();
    for activity in &entry.activity_ids {
        if nominations
            .iter()
            .any(|n| n.input.activity_ids.contains(activity))
        {
            continue;
        }
        tx.execute(
            "DELETE FROM knowledge_activity WHERE id=?1 AND project=?2",
            params![activity.as_str(), project.as_str()],
        )
        .map_err(error)?;
        dropped.insert(activity.clone());
    }
    let jobs: Vec<super::ExtractionJob> = bodies(
        tx,
        "SELECT body FROM knowledge_jobs WHERE project=?1 LIMIT ?2",
        project,
    )?;
    for job in jobs
        .iter()
        .filter(|job| job.activity_ids.iter().any(|id| dropped.contains(id)))
    {
        tx.execute("DELETE FROM knowledge_jobs WHERE id=?1", [job.id.as_str()])
            .map_err(error)?;
    }
    for source in &entry.source_ids {
        let captured: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM knowledge_activity WHERE project=?1 AND source=?2)",
                params![project.as_str(), source.as_str()],
                |r| r.get(0),
            )
            .map_err(error)?;
        if !captured
            && !remaining
                .iter()
                .any(|other| other.source_ids.contains(source))
        {
            tx.execute(
                "DELETE FROM knowledge_sources WHERE project=?1 AND id=?2 AND revoked=0",
                params![project.as_str(), source.as_str()],
            )
            .map_err(error)?;
        }
    }
    assert!(versions >= 1);
    assert!(load_entry(tx, entry_id)?.is_none());
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn row(
    ordinal: i64,
    id: String,
    project: Option<String>,
    origin: String,
    text: String,
) -> Result<SavedMemory, EngineError> {
    let scope = match project {
        None => MemoryScope::Global,
        Some(project) => MemoryScope::Project {
            project_id: ProjectId::new(project).map_err(error)?,
        },
    };
    let memory = SavedMemory {
        id: RequestId::new(id).map_err(error)?,
        ordinal: u64::try_from(ordinal).map_err(error)?,
        scope,
        origin: serde_json::from_str(&origin).map_err(error)?,
        text,
    };
    assert!(memory.ordinal > 0);
    assert!(!memory.text.is_empty() && memory.text.len() <= TEXT_BYTES_MAX);
    Ok(memory)
}

/// Reads one saved memory by id for the owner's detail view.
/// Input: the transaction and memory id. Output: the memory, or None once deleted.
pub(crate) fn get(
    tx: &Transaction<'_>,
    id: &RequestId,
) -> Result<Option<SavedMemory>, EngineError> {
    tx.query_row(
        "SELECT ordinal,id,project,origin,text FROM memory_library WHERE id=?1",
        [id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
    .map_err(error)?
    .map(|(ordinal, id, project, origin, text)| row(ordinal, id, project, origin, text))
    .transpose()
}

fn scoped(
    tx: &Transaction<'_>,
    scope: &MemoryScope,
    search: Option<&str>,
    before: Option<u64>,
    limit: usize,
) -> Result<Vec<SavedMemory>, EngineError> {
    assert!(limit <= SCOPE_ITEMS_MAX + 1);
    let before = before.map_or(i64::MAX, |n| i64::try_from(n).unwrap_or(i64::MAX));
    let mut statement = tx
        .prepare(
            "SELECT ordinal,id,project,origin,text FROM memory_library
            WHERE project IS ?1 AND ordinal<?2 AND (?3 IS NULL OR instr(lower(text),lower(?3))>0)
            ORDER BY ordinal DESC LIMIT ?4",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![scope.project(), before, search, limit as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(error)?;
    let mut items = Vec::new();
    for found in rows {
        let (ordinal, id, project, origin, text) = found.map_err(error)?;
        items.push(row(ordinal, id, project, origin, text)?);
    }
    assert!(items.len() <= limit);
    assert!(
        items
            .windows(2)
            .all(|pair| pair[0].ordinal > pair[1].ordinal)
    );
    Ok(items)
}

/// Lists saved memories in one scope, newest first, with optional search.
/// Input: scope, case-insensitive search text, the `next` cursor of the previous
/// page and the page size. Output: owner data holding one bounded page.
pub(crate) fn page(
    tx: &Transaction<'_>,
    scope: &MemoryScope,
    search: Option<&str>,
    before: Option<u64>,
    limit: u8,
) -> Result<OwnerData, EngineError> {
    validate_page(search, limit)?;
    if let MemoryScope::Project { project_id } = scope
        && !project_exists(tx, project_id)?
    {
        return Err(error("memory_project_missing"));
    }
    let found = scoped(tx, scope, search, before, usize::from(limit) + 1)?;
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut more = found.len() > usize::from(limit);
    for memory in found.into_iter().take(usize::from(limit)) {
        let size = serde_json::to_vec(&memory).map_err(error)?.len() + 1;
        if bytes + size > PAGE_BYTES_MAX {
            more = true;
            break;
        }
        bytes += size;
        items.push(memory);
    }
    let next = if more {
        items.last().map(|memory| memory.ordinal)
    } else {
        None
    };
    assert!(items.len() <= usize::from(limit));
    assert!(bytes <= PAGE_BYTES_MAX);
    Ok(OwnerData::Memories {
        page: MemoryPage { items, next },
    })
}

fn rendered_item(text: &str) -> String {
    format!("- {}\n", text.trim().replace('\n', "\n  "))
}

struct Collector {
    items: Vec<ApprovedMemory>,
    seen: BTreeSet<String>,
    bytes: usize,
    complete: bool,
}

impl Collector {
    fn push(&mut self, scope: ApprovedScope, kind: ApprovedKind, text: &str) {
        if !self.seen.insert(hash(text.trim().as_bytes())) {
            return;
        }
        let size = rendered_item(text).len();
        if self.items.len() >= LAUNCH_ITEMS_MAX || self.bytes + size > LAUNCH_BYTES_MAX {
            self.complete = false;
            return;
        }
        self.bytes += size;
        self.items.push(ApprovedMemory {
            scope,
            kind,
            text: text.into(),
        });
        assert!(self.bytes <= LAUNCH_BYTES_MAX);
    }
}

fn kept_research(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Vec<KnowledgeEntry>, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT e.body FROM knowledge_heads h JOIN knowledge_entries e ON h.id=e.id AND h.version=e.version
            WHERE h.project=?1 ORDER BY h.id LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), RESEARCH_ROWS_MAX], |r| {
            r.get::<_, String>(0)
        })
        .map_err(error)?;
    let mut entries = Vec::new();
    for body in rows {
        let entry: KnowledgeEntry = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        entry.assert_consistent();
        if entry.status == KnowledgeStatus::Confirmed && live_entry(tx, &entry)? {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|a| std::cmp::Reverse(a.revision.value()));
    assert!(entries.iter().all(|e| &e.project_id == project));
    Ok(entries)
}

fn kind_of(origin: &MemoryOrigin) -> ApprovedKind {
    match origin {
        MemoryOrigin::Written => ApprovedKind::Note,
        MemoryOrigin::Promoted { .. } => ApprovedKind::Promoted,
        MemoryOrigin::Selected { .. } => ApprovedKind::Selection,
    }
}

/// Returns the bounded owner-approved memories an agent in this project may read.
/// Input: the engine transaction and project id. Output: project notes and kept
/// project research first, then global memory, newest first, within the launch bounds.
pub(crate) fn approved_for_project(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<ApprovedMemories, EngineError> {
    let mut collector = Collector {
        items: Vec::new(),
        seen: BTreeSet::new(),
        bytes: 0,
        complete: true,
    };
    let local = MemoryScope::Project {
        project_id: project.clone(),
    };
    for memory in scoped(tx, &local, None, None, SCOPE_ITEMS_MAX)? {
        collector.push(
            ApprovedScope::Project,
            kind_of(&memory.origin),
            &memory.text,
        );
    }
    for entry in kept_research(tx, project)? {
        collector.push(ApprovedScope::Project, ApprovedKind::Research, &entry.text);
    }
    for memory in scoped(tx, &MemoryScope::Global, None, None, SCOPE_ITEMS_MAX)? {
        collector.push(ApprovedScope::Global, kind_of(&memory.origin), &memory.text);
    }
    let memories = ApprovedMemories {
        items: collector.items,
        complete: collector.complete,
    };
    assert!(memories.items.len() <= LAUNCH_ITEMS_MAX);
    assert!(memories.render().len() <= LAUNCH_BYTES_MAX + RENDER_HEADER_BYTES_MAX);
    Ok(memories)
}

#[cfg(test)]
mod tests;
