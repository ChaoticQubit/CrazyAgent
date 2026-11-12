//! The owner's notification center (DEC-136a): one engine store every client reads, the
//! desktop bell, the TUI list and later remote channels. Action items stay pending until the
//! engine sees their state fixed (`reconcile`); Info items are only read or unread. Each item
//! has a stable id, a kind, an optional project, one line of text and a typed target page.

use crate::engine::EngineError;
use crate::landing::ChangeSetId;
use crate::protocol::{CredentialRef, Outcome, ProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

pub(crate) mod events;
pub mod reconcile;
mod sources;

pub const TEXT_BYTES_MAX: usize = 240;
pub const SUBJECT_BYTES_MAX: usize = 256;
pub const TARGET_BYTES_MAX: usize = 1024;
pub const PAGE_MAX: u16 = 50;
pub const FOLD_IDS_MAX: usize = 32;
pub const ACTION_IDS_MAX: usize = 256;
pub const RETENTION_DAYS_MIN: u16 = 1;
pub const RETENTION_DAYS_MAX: u16 = 365;
pub const RETENTION_DAYS_DEFAULT: u16 = 30;
pub const ROWS_MAX: i64 = 5_000;
pub const COUNT_MAX: u32 = 9_999;
pub const LISTED_BYTES_MAX: usize = 48 * 1024;
pub const TICK_MS: i64 = 2_000;
pub const VERSION_BYTES_MAX: usize = 64;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const ENGINE_LOST_GAP_MS: i64 = 60_000;
const ITEM_BYTES_MAX: usize = TEXT_BYTES_MAX * 6 + TARGET_BYTES_MAX + FOLD_IDS_MAX * 21 + 512;

const _: () = assert!(LISTED_BYTES_MAX + 4096 <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(ITEM_BYTES_MAX * 4 < LISTED_BYTES_MAX);
const _: () = assert!(PAGE_MAX as usize * 4 <= ACTION_IDS_MAX && FOLD_IDS_MAX <= ACTION_IDS_MAX);
const _: () = assert!(RETENTION_DAYS_MIN <= RETENTION_DAYS_DEFAULT);
const _: () = assert!(RETENTION_DAYS_DEFAULT <= RETENTION_DAYS_MAX);
const _: () = assert!(ROWS_MAX > 10 * reconcile::SOURCE_ROWS_MAX as i64);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS owner_notices (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    action INTEGER NOT NULL CHECK(action IN (0,1)),
    project TEXT,
    subject TEXT NOT NULL CHECK(length(subject) BETWEEN 1 AND 256),
    text TEXT NOT NULL CHECK(length(text) BETWEEN 1 AND 240),
    target TEXT NOT NULL CHECK(length(target) <= 1024),
    created_ms INTEGER NOT NULL,
    read_ms INTEGER,
    resolved_ms INTEGER,
    dismissed_ms INTEGER,
    CHECK(action = 1 OR (resolved_ms IS NULL AND dismissed_ms IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS owner_notices_open
    ON owner_notices(kind, subject) WHERE resolved_ms IS NULL;
CREATE INDEX IF NOT EXISTS owner_notices_project ON owner_notices(project, id);
CREATE TABLE IF NOT EXISTS owner_notice_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    retention_days INTEGER NOT NULL CHECK(retention_days BETWEEN 1 AND 365),
    banners TEXT NOT NULL CHECK(banners IN ('off','action_only','all'))
);
CREATE TABLE IF NOT EXISTS owner_notice_kinds (
    kind TEXT PRIMARY KEY,
    delivery TEXT NOT NULL CHECK(delivery IN ('on','bell_only','off'))
);
CREATE TABLE IF NOT EXISTS owner_notice_cursors (
    source TEXT PRIMARY KEY,
    last INTEGER NOT NULL
);";

const COLUMNS: &str = "id,kind,action,project,text,target,created_ms,read_ms,resolved_ms";

/// Whether an item waits on the owner until its state is fixed (Action) or only tells
/// something (Info).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeClass {
    Action,
    Info,
}

/// Every kind of notification, the same names on every client and in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Question,
    AccountSignIn,
    ResearchPool,
    MemoryReview,
    ChangeSetReview,
    AgentFinished,
    AgentFailed,
    AgentStopped,
    MasterOff,
    ChangeSetLanded,
    ChangeSetConflict,
    ChangeSetRolledBack,
    AccountNearLimit,
    AccountAtLimit,
    PoolFailover,
    AppUpdateReady,
    EngineRestarted,
    EngineLost,
    HarnessUpdate,
    TeamAlert,
}

impl NoticeKind {
    pub const ALL: [Self; 20] = [
        Self::Question,
        Self::AccountSignIn,
        Self::ResearchPool,
        Self::MemoryReview,
        Self::ChangeSetReview,
        Self::AgentFinished,
        Self::AgentFailed,
        Self::AgentStopped,
        Self::MasterOff,
        Self::ChangeSetLanded,
        Self::ChangeSetConflict,
        Self::ChangeSetRolledBack,
        Self::AccountNearLimit,
        Self::AccountAtLimit,
        Self::PoolFailover,
        Self::AppUpdateReady,
        Self::EngineRestarted,
        Self::EngineLost,
        Self::HarnessUpdate,
        Self::TeamAlert,
    ];

    /// The kind's class. Input: the kind. Output: Action for the five kinds the owner must
    /// act on, Info for the rest.
    pub fn class(self) -> NoticeClass {
        match self {
            Self::Question
            | Self::AccountSignIn
            | Self::ResearchPool
            | Self::MemoryReview
            | Self::ChangeSetReview => NoticeClass::Action,
            _ => NoticeClass::Info,
        }
    }

    /// The kind's stored name, the same as its snake case wire name. Input: the kind.
    /// Output: the fixed name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::AccountSignIn => "account_sign_in",
            Self::ResearchPool => "research_pool",
            Self::MemoryReview => "memory_review",
            Self::ChangeSetReview => "change_set_review",
            Self::AgentFinished => "agent_finished",
            Self::AgentFailed => "agent_failed",
            Self::AgentStopped => "agent_stopped",
            Self::MasterOff => "master_off",
            Self::ChangeSetLanded => "change_set_landed",
            Self::ChangeSetConflict => "change_set_conflict",
            Self::ChangeSetRolledBack => "change_set_rolled_back",
            Self::AccountNearLimit => "account_near_limit",
            Self::AccountAtLimit => "account_at_limit",
            Self::PoolFailover => "pool_failover",
            Self::AppUpdateReady => "app_update_ready",
            Self::EngineRestarted => "engine_restarted",
            Self::EngineLost => "engine_lost",
            Self::HarnessUpdate => "harness_update",
            Self::TeamAlert => "team_alert",
        }
    }

    /// The kind's short label, the same words on every client (DEC-141). Input: the kind.
    /// Output: the fixed label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Question => "Question",
            Self::AccountSignIn => "Account sign-in",
            Self::ResearchPool => "Waiting for a pool",
            Self::MemoryReview => "Memories to review",
            Self::ChangeSetReview => "Change set to review",
            Self::AgentFinished => "Agent finished",
            Self::AgentFailed => "Agent failed",
            Self::AgentStopped => "Agent stopped",
            Self::MasterOff => "Master turned off",
            Self::ChangeSetLanded => "Change set landed",
            Self::ChangeSetConflict => "Change set conflict",
            Self::ChangeSetRolledBack => "Change set rolled back",
            Self::AccountNearLimit => "Account near its limit",
            Self::AccountAtLimit => "Account at its limit",
            Self::PoolFailover => "Pool failover",
            Self::AppUpdateReady => "App update ready",
            Self::EngineRestarted => "Engine restarted",
            Self::EngineLost => "Engine lost",
            Self::HarnessUpdate => "Harness update",
            Self::TeamAlert => "Team alert",
        }
    }

    /// Reads a stored kind name. Input: the name. Output: the kind, or none for a name this
    /// engine does not know.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == value)
    }
}

/// The page a click on an item opens, the same on every client: each client maps it to its
/// own screen. Never an address, since item text can come from agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "page", rename_all = "snake_case")]
pub enum NoticeTarget {
    Questions {
        project_id: ProjectId,
        question: Option<i64>,
        agent: Option<i64>,
    },
    AccountSignIn {
        account: CredentialRef,
    },
    ProjectPool {
        project_id: ProjectId,
    },
    Memories {
        project_id: ProjectId,
    },
    ChangeSet {
        project_id: ProjectId,
        change_set: Option<ChangeSetId>,
    },
    Agents {
        project_id: ProjectId,
    },
    Conversation {
        project_id: ProjectId,
    },
    Accounts,
    Usage,
    Updates,
    Harnesses,
    Home,
}

impl NoticeTarget {
    /// The project the page belongs to. Input: the target. Output: its project, or none
    /// for an app-wide page.
    pub fn project(&self) -> Option<&ProjectId> {
        match self {
            Self::Questions { project_id, .. }
            | Self::ProjectPool { project_id }
            | Self::Memories { project_id }
            | Self::ChangeSet { project_id, .. }
            | Self::Agents { project_id }
            | Self::Conversation { project_id } => Some(project_id),
            Self::AccountSignIn { .. }
            | Self::Accounts
            | Self::Usage
            | Self::Updates
            | Self::Harnesses
            | Self::Home => None,
        }
    }
}

/// One row of the bell, the project view or the TUI list: one item, or the newest item of
/// a fold with how many it holds, how many are unread and pending, and up to
/// `FOLD_IDS_MAX` of their ids, newest first. `banner` says whether a macOS notification may
/// show it under the owner's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    pub id: i64,
    pub kind: NoticeKind,
    pub class: NoticeClass,
    pub project_id: Option<ProjectId>,
    pub text: String,
    pub target: NoticeTarget,
    pub created_ms: i64,
    pub read_ms: Option<i64>,
    pub resolved_ms: Option<i64>,
    pub folded: u32,
    pub unread: u32,
    pub pending: u32,
    pub ids: Vec<i64>,
    pub banner: bool,
}

/// Which items a read lists: every one, the unread ones, the pending Actions, or the
/// resolved Actions (Done).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeFilter {
    All,
    Unread,
    Pending,
    Done,
}

/// One read of the store: the filter, one project or all, one kind or all, whether same
/// kind items of a project fold into one row, the paging bounds by id, newest first, and at
/// most `PAGE_MAX` rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeQuery {
    pub filter: NoticeFilter,
    pub project_id: Option<ProjectId>,
    pub kind: Option<NoticeKind>,
    pub fold: bool,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub limit: u16,
}

/// The bell's two badges over a read's project scope: unread items and pending Actions,
/// each at most `COUNT_MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeCounts {
    pub unread: u32,
    pub pending: u32,
}

/// One read's rows, the counts of its scope, whether more rows follow, and the newest id
/// in the store, which a client keeps to show only newer items as banners.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticePage {
    pub items: Vec<Notice>,
    pub counts: NoticeCounts,
    pub more: bool,
    pub newest: i64,
}

/// How one kind reaches the owner: in the bell with a banner, in the bell only, or not at
/// all. Action kinds are never Off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    On,
    BellOnly,
    Off,
}

impl Delivery {
    const ALL: [Self; 3] = [Self::On, Self::BellOnly, Self::Off];

    fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::BellOnly => "bell_only",
            Self::Off => "off",
        }
    }
}

/// Which items show a macOS notification while the app is in the background: none, the
/// Action kinds only, or all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Banners {
    Off,
    ActionOnly,
    All,
}

impl Banners {
    const ALL: [Self; 3] = [Self::Off, Self::ActionOnly, Self::All];

    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ActionOnly => "action_only",
            Self::All => "all",
        }
    }
}

/// One kind's delivery choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindDelivery {
    pub kind: NoticeKind,
    pub delivery: Delivery,
}

/// Settings > Notifications: each kind's delivery in `NoticeKind::ALL` order, how many days
/// Done items and read Info items are kept, and the macOS banners choice. Before the first
/// save: every kind On, 30 days, All.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeSettings {
    pub kinds: Vec<KindDelivery>,
    pub retention_days: u16,
    pub banners: Banners,
}

impl Default for NoticeSettings {
    fn default() -> Self {
        Self {
            kinds: NoticeKind::ALL
                .into_iter()
                .map(|kind| KindDelivery {
                    kind,
                    delivery: Delivery::On,
                })
                .collect(),
            retention_days: RETENTION_DAYS_DEFAULT,
            banners: Banners::All,
        }
    }
}

impl NoticeSettings {
    /// One kind's delivery. Input: the kind. Output: its choice, On when it is not listed.
    pub fn delivery(&self, kind: NoticeKind) -> Delivery {
        self.kinds
            .iter()
            .find(|item| item.kind == kind)
            .map_or(Delivery::On, |item| item.delivery)
    }

    /// Whether an item of one kind may show a macOS notification. Input: the kind. Output:
    /// true when the kind is On and the banners choice takes its class.
    pub fn banner(&self, kind: NoticeKind) -> bool {
        let on = self.delivery(kind) == Delivery::On;
        let class = match self.banners {
            Banners::Off => false,
            Banners::ActionOnly => kind.class() == NoticeClass::Action,
            Banners::All => true,
        };
        on && class
    }

    /// Whether the settings can be saved: each kind once, no Action kind Off, and the
    /// retention days within bounds. Input: the settings. Output: the answer.
    fn valid(&self) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        let unique = self.kinds.iter().all(|item| seen.insert(item.kind));
        let whole = unique && seen.len() == NoticeKind::ALL.len();
        let never_off = self
            .kinds
            .iter()
            .all(|item| item.kind.class() == NoticeClass::Info || item.delivery != Delivery::Off);
        let days = (RETENTION_DAYS_MIN..=RETENTION_DAYS_MAX).contains(&self.retention_days);
        whole && never_off && days
    }
}

/// An event only a client sees, reported to the store: an app update ready to install with
/// its version, or the engine lost and found again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NoticeReport {
    AppUpdateReady { version: String },
    EngineLost,
}

/// One folded row as a client listed it: its kind, its project, the filter it was listed
/// under and its newest id, so a change reaches every item the row holds and none newer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeFold {
    pub kind: NoticeKind,
    pub project_id: Option<ProjectId>,
    pub filter: NoticeFilter,
    pub newest: i64,
}

/// What a fold change does to every item of its folds: mark read, mark unread or delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoldChange {
    Read,
    Unread,
    Delete,
}

/// What a client changes in the store: marks items read or unread, deletes them (a pending
/// Action is hidden until its state clears), does one of those to every item of folded
/// rows, marks every item of one project or all read, saves the settings, or reports a
/// client event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NoticeAction {
    Read {
        ids: Vec<i64>,
    },
    Unread {
        ids: Vec<i64>,
    },
    Delete {
        ids: Vec<i64>,
    },
    Folds {
        folds: Vec<NoticeFold>,
        change: FoldChange,
    },
    ReadAll {
        project_id: Option<ProjectId>,
    },
    SaveSettings {
        settings: NoticeSettings,
    },
    Report {
        report: NoticeReport,
    },
}

/// One Info event an engine producer records: its kind, project, a key that makes the same
/// event count once, its text and the page it opens.
pub struct InfoNotice<'a> {
    pub kind: NoticeKind,
    pub project_id: Option<&'a ProjectId>,
    pub subject: &'a str,
    pub text: &'a str,
    pub target: NoticeTarget,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Creates the store's tables when missing. Input: the engine database. Output: Ok, or
/// the SQLite failure.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(error)
}

/// Item text as every client shows it: hidden characters escaped, local paths cut to their
/// last part, one line of at most `TEXT_BYTES_MAX` bytes. Input: the text. Output: the
/// line, maybe empty.
pub(crate) fn shown(text: &str) -> String {
    let words = crate::landing::scrubbed(text, &[]);
    let mut end = words.len().min(TEXT_BYTES_MAX);
    while !words.is_char_boundary(end) {
        end -= 1;
    }
    let line = words[..end].trim_end().to_owned();
    assert!(line.len() <= TEXT_BYTES_MAX);
    assert!(!line.contains('\n'));
    line
}

/// The stored form of a target. Input: the target. Output: its JSON, at most
/// `TARGET_BYTES_MAX` bytes.
pub(crate) fn target_text(target: &NoticeTarget) -> Result<String, EngineError> {
    let text = serde_json::to_string(target).map_err(error)?;
    assert!(text.len() <= TARGET_BYTES_MAX);
    assert!(text.starts_with('{'));
    Ok(text)
}

/// Lists one page of the store for a read: the rows the query asks for, newest first and
/// within `LISTED_BYTES_MAX`, the counts of its project scope and the newest id. Inputs: the
/// engine database and the query. Output: the page, or `notification_query_invalid` for a
/// limit outside 1 to `PAGE_MAX` or a negative bound.
pub fn page(connection: &Connection, query: &NoticeQuery) -> Result<NoticePage, EngineError> {
    validate_query(query)?;
    let settings = settings(connection)?;
    let rows = if query.fold {
        folded_rows(connection, query)?
    } else {
        plain_rows(connection, query)?
    };
    let limit = usize::from(query.limit);
    let mut more = rows.len() > limit;
    let mut items = Vec::with_capacity(rows.len().min(limit));
    let mut bytes = 0;
    for mut item in rows.into_iter().take(limit) {
        item.banner = settings.banner(item.kind);
        let size = serde_json::to_vec(&item).map_err(error)?.len() + 1;
        assert!(size <= ITEM_BYTES_MAX);
        if bytes + size > LISTED_BYTES_MAX - 1024 {
            more = true;
            break;
        }
        bytes += size;
        items.push(item);
    }
    let page = NoticePage {
        items,
        counts: counts(connection, query.project_id.as_ref())?,
        more,
        newest: newest(connection)?,
    };
    assert!(page.items.len() <= limit);
    assert!(serde_json::to_vec(&page).map_err(error)?.len() <= LISTED_BYTES_MAX);
    Ok(page)
}

/// Refuses a query a client cannot send: a page limit outside 1 to `PAGE_MAX` or a negative
/// id bound. Input: the query. Output: Ok, or `notification_query_invalid`.
pub fn validate_query(query: &NoticeQuery) -> Result<(), EngineError> {
    let bounds = [query.before, query.after].into_iter().flatten();
    if !(1..=PAGE_MAX).contains(&query.limit) || bounds.into_iter().any(|id| id < 0) {
        return Err(failure("notification_query_invalid"));
    }
    Ok(())
}

/// The SQL test one filter adds to a read. Input: the filter. Output: its condition.
fn filter_sql(filter: NoticeFilter) -> &'static str {
    match filter {
        NoticeFilter::All => "1",
        NoticeFilter::Unread => "read_ms IS NULL",
        NoticeFilter::Pending => "action=1 AND resolved_ms IS NULL",
        NoticeFilter::Done => "resolved_ms IS NOT NULL",
    }
}

/// The condition every row of a read meets: visible, in the filter, in the project and of
/// the kind when the query names them (`?1` and `?2`). Input: the query. Output: the SQL.
fn scope_sql(query: &NoticeQuery) -> String {
    format!(
        "dismissed_ms IS NULL AND ({}) AND (?1 IS NULL OR project=?1) AND (?2 IS NULL OR kind=?2)",
        filter_sql(query.filter)
    )
}

/// Reads the query's rows one item each, newest first, one past the limit. Inputs: the
/// database and the query. Output: the items.
fn plain_rows(connection: &Connection, query: &NoticeQuery) -> Result<Vec<Notice>, EngineError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM owner_notices WHERE {} AND (?3 IS NULL OR id<?3)
         AND (?4 IS NULL OR id>?4) ORDER BY id DESC LIMIT ?5",
        scope_sql(query)
    );
    let mut statement = connection.prepare(&sql).map_err(error)?;
    let rows = statement
        .query_map(
            params![
                query.project_id.as_ref().map(ProjectId::as_str),
                query.kind.map(NoticeKind::name),
                query.before,
                query.after,
                i64::from(query.limit) + 1
            ],
            stored,
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let items: Vec<Notice> = rows.into_iter().filter_map(item).collect();
    assert!(items.len() <= usize::from(query.limit) + 1);
    assert!(items.windows(2).all(|pair| pair[0].id > pair[1].id));
    Ok(items)
}

/// Reads the query's rows one fold each, by each fold's newest id, one past the limit: same
/// project and kind fold into one row. Inputs: the database and the query. Output: the
/// rows.
fn folded_rows(connection: &Connection, query: &NoticeQuery) -> Result<Vec<Notice>, EngineError> {
    let sql = format!(
        "SELECT MAX(id), COUNT(*), SUM(read_ms IS NULL), SUM(action=1 AND resolved_ms IS NULL),
         project, kind FROM owner_notices WHERE {} GROUP BY project, kind
         HAVING (?3 IS NULL OR MAX(id)<?3) AND (?4 IS NULL OR MAX(id)>?4)
         ORDER BY MAX(id) DESC LIMIT ?5",
        scope_sql(query)
    );
    type Group = (i64, i64, i64, i64, Option<String>, String);
    let groups: Vec<Group> = connection
        .prepare(&sql)
        .map_err(error)?
        .query_map(
            params![
                query.project_id.as_ref().map(ProjectId::as_str),
                query.kind.map(NoticeKind::name),
                query.before,
                query.after,
                i64::from(query.limit) + 1
            ],
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
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut items = Vec::with_capacity(groups.len());
    for (newest, count, unread, pending, project, kind) in groups {
        let Some(mut item) = row_at(connection, newest)? else {
            continue;
        };
        assert!(count >= 1 && (0..=count).contains(&unread) && (0..=count).contains(&pending));
        item.folded = clamp(count);
        item.unread = clamp(unread);
        item.pending = clamp(pending);
        item.ids = fold_ids(connection, query, project.as_deref(), &kind)?;
        assert_eq!(item.ids.first(), Some(&item.id));
        items.push(item);
    }
    assert!(items.len() <= usize::from(query.limit) + 1);
    Ok(items)
}

/// The newest ids of one fold, at most `FOLD_IDS_MAX`. Inputs: the database, the query,
/// the fold's project and kind. Output: the ids, newest first.
fn fold_ids(
    connection: &Connection,
    query: &NoticeQuery,
    project: Option<&str>,
    kind: &str,
) -> Result<Vec<i64>, EngineError> {
    let sql = format!(
        "SELECT id FROM owner_notices WHERE {} AND project IS ?3 AND kind=?4 ORDER BY id DESC LIMIT ?5",
        scope_sql(query)
    );
    let ids: Vec<i64> = connection
        .prepare(&sql)
        .map_err(error)?
        .query_map(
            params![
                query.project_id.as_ref().map(ProjectId::as_str),
                query.kind.map(NoticeKind::name),
                project,
                kind,
                FOLD_IDS_MAX as i64
            ],
            |row| row.get(0),
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(!ids.is_empty());
    assert!(ids.len() <= FOLD_IDS_MAX);
    Ok(ids)
}

type Stored = (
    i64,
    String,
    i64,
    Option<String>,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
);

/// Reads one stored row's columns in `COLUMNS` order. Input: the row. Output: the values.
fn stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

/// One stored row as a single item. A row of a kind, project or target this engine does
/// not read is skipped. Input: the values. Output: the item, or none.
fn item(row: Stored) -> Option<Notice> {
    let (id, kind, action, project, text, target, created_ms, read_ms, resolved_ms) = row;
    let kind = NoticeKind::parse(&kind)?;
    let project_id = match project {
        Some(project) => Some(ProjectId::new(project).ok()?),
        None => None,
    };
    let target: NoticeTarget = serde_json::from_str(&target).ok()?;
    let class = kind.class();
    assert_eq!(action == 1, class == NoticeClass::Action);
    assert!(class == NoticeClass::Action || resolved_ms.is_none());
    let pending = class == NoticeClass::Action && resolved_ms.is_none();
    Some(Notice {
        id,
        kind,
        class,
        project_id,
        text,
        target,
        created_ms,
        read_ms,
        resolved_ms,
        folded: 1,
        unread: u32::from(read_ms.is_none()),
        pending: u32::from(pending),
        ids: vec![id],
        banner: false,
    })
}

/// Reads one row by id as an item. Inputs: the database and the id. Output: the item, or
/// none when it is gone or unreadable.
fn row_at(connection: &Connection, id: i64) -> Result<Option<Notice>, EngineError> {
    let row = connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM owner_notices WHERE id=?1"),
            [id],
            stored,
        )
        .optional()
        .map_err(error)?;
    Ok(row.and_then(item))
}

fn clamp(count: i64) -> u32 {
    u32::try_from(count.clamp(0, i64::from(COUNT_MAX))).expect("clamped to COUNT_MAX")
}

/// The bell's badges over one project or every item. Inputs: the database and the project.
/// Output: the unread and pending counts.
fn counts(
    connection: &Connection,
    project: Option<&ProjectId>,
) -> Result<NoticeCounts, EngineError> {
    let (unread, pending): (i64, i64) = connection
        .query_row(
            "SELECT COALESCE(SUM(read_ms IS NULL),0), COALESCE(SUM(action=1 AND resolved_ms IS NULL),0)
             FROM owner_notices WHERE dismissed_ms IS NULL AND (?1 IS NULL OR project=?1)",
            [project.map(ProjectId::as_str)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    assert!(unread >= 0 && pending >= 0);
    Ok(NoticeCounts {
        unread: clamp(unread),
        pending: clamp(pending),
    })
}

fn newest(connection: &Connection) -> Result<i64, EngineError> {
    let newest: i64 = connection
        .query_row("SELECT COALESCE(MAX(id),0) FROM owner_notices", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    assert!(newest >= 0);
    Ok(newest)
}

/// Loads Settings > Notifications. Input: the engine database, writable or read-only.
/// Output: the stored settings, the defaults before the first save, or
/// `notification_settings_invalid` for a stored value this engine does not know.
pub fn settings(connection: &Connection) -> Result<NoticeSettings, EngineError> {
    let mut settings = NoticeSettings::default();
    let stored: Option<(i64, String)> = connection
        .query_row(
            "SELECT retention_days, banners FROM owner_notice_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let invalid = || failure("notification_settings_invalid");
    if let Some((days, banners)) = stored {
        settings.retention_days = u16::try_from(days)
            .ok()
            .filter(|days| (RETENTION_DAYS_MIN..=RETENTION_DAYS_MAX).contains(days))
            .ok_or_else(invalid)?;
        settings.banners = Banners::ALL
            .into_iter()
            .find(|choice| choice.name() == banners)
            .ok_or_else(invalid)?;
    }
    let mut statement = connection
        .prepare("SELECT kind, delivery FROM owner_notice_kinds")
        .map_err(error)?;
    let rows: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    for (kind, delivery) in rows {
        let Some(kind) = NoticeKind::parse(&kind) else {
            continue;
        };
        let delivery = Delivery::ALL
            .into_iter()
            .find(|choice| choice.name() == delivery)
            .ok_or_else(invalid)?;
        let at = NoticeKind::ALL
            .iter()
            .position(|known| *known == kind)
            .expect("a known kind");
        settings.kinds[at].delivery = delivery;
    }
    if !settings.valid() {
        return Err(invalid());
    }
    Ok(settings)
}

/// Saves Settings > Notifications over the stored ones. Inputs: the transaction and the
/// settings. Output: Applied, or a `notification_settings_invalid` rejection that changes
/// nothing for a kind missing or listed twice, an Action kind Off, or retention days out
/// of bounds.
fn save(tx: &Transaction<'_>, settings: &NoticeSettings) -> Result<Outcome, EngineError> {
    if !settings.valid() {
        return Ok(rejected("notification_settings_invalid"));
    }
    let stored = tx
        .execute(
            "INSERT INTO owner_notice_settings(singleton,retention_days,banners) VALUES(1,?1,?2)
             ON CONFLICT(singleton) DO UPDATE SET retention_days=excluded.retention_days, banners=excluded.banners",
            params![settings.retention_days, settings.banners.name()],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    for item in &settings.kinds {
        tx.execute(
            "INSERT INTO owner_notice_kinds(kind,delivery) VALUES(?1,?2)
             ON CONFLICT(kind) DO UPDATE SET delivery=excluded.delivery",
            params![item.kind.name(), item.delivery.name()],
        )
        .map_err(error)?;
    }
    let ordered = NoticeSettings {
        kinds: NoticeKind::ALL
            .into_iter()
            .map(|kind| KindDelivery {
                kind,
                delivery: settings.delivery(kind),
            })
            .collect(),
        ..settings.clone()
    };
    assert_eq!(self::settings(tx)?, ordered);
    Ok(Outcome::Applied)
}

/// Applies one owner change to the store. Inputs: the transaction, the change and the
/// time. Output: Applied, or a rejection that changes nothing: `notification_ids_invalid`
/// for no ids or more than `ACTION_IDS_MAX`, `notification_folds_invalid` for no folds,
/// more than `PAGE_MAX` or a negative newest id, `notification_settings_invalid`, or
/// `notification_report_invalid`.
pub(crate) fn apply(
    tx: &Transaction<'_>,
    action: &NoticeAction,
    now: i64,
) -> Result<Outcome, EngineError> {
    let ids = match action {
        NoticeAction::Read { ids }
        | NoticeAction::Unread { ids }
        | NoticeAction::Delete { ids } => Some(ids),
        _ => None,
    };
    if ids.is_some_and(|ids| ids.is_empty() || ids.len() > ACTION_IDS_MAX) {
        return Ok(rejected("notification_ids_invalid"));
    }
    match action {
        NoticeAction::Read { ids } => each(
            tx,
            ids,
            "UPDATE owner_notices SET read_ms=?2 WHERE id=?1 AND read_ms IS NULL AND dismissed_ms IS NULL",
            Some(now),
        )?,
        NoticeAction::Unread { ids } => each(
            tx,
            ids,
            "UPDATE owner_notices SET read_ms=NULL WHERE id=?1 AND dismissed_ms IS NULL",
            None,
        )?,
        NoticeAction::Delete { ids } => delete(tx, ids, now)?,
        NoticeAction::Folds { folds, change } => {
            let bounded = !folds.is_empty() && folds.len() <= usize::from(PAGE_MAX);
            if !bounded || folds.iter().any(|fold| fold.newest < 0) {
                return Ok(rejected("notification_folds_invalid"));
            }
            for fold in folds {
                fold_change(tx, fold, *change, now)?;
            }
        }
        NoticeAction::ReadAll { project_id } => {
            tx.execute(
                "UPDATE owner_notices SET read_ms=?2 WHERE read_ms IS NULL AND dismissed_ms IS NULL
                 AND (?1 IS NULL OR project=?1)",
                params![project_id.as_ref().map(ProjectId::as_str), now],
            )
            .map_err(error)?;
        }
        NoticeAction::SaveSettings { settings } => return save(tx, settings),
        NoticeAction::Report { report } => return reported(tx, report, now),
    }
    Ok(Outcome::Applied)
}

/// Runs one statement for each id as `?1`, with the time as `?2` when given. Inputs: the
/// transaction, the ids, the statement and the time. Output: Ok, or the SQLite failure.
fn each(tx: &Transaction<'_>, ids: &[i64], sql: &str, now: Option<i64>) -> Result<(), EngineError> {
    assert!(!ids.is_empty() && ids.len() <= ACTION_IDS_MAX);
    assert!(now.is_none_or(|now| now >= 0));
    let mut statement = tx.prepare(sql).map_err(error)?;
    for id in ids {
        match now {
            Some(now) => statement.execute(params![id, now]),
            None => statement.execute(params![id]),
        }
        .map_err(error)?;
    }
    Ok(())
}

/// Does one change to every visible item of one fold: its kind and project, in its filter,
/// at or below its newest id; a delete removes Info items and resolved Actions and hides
/// pending Actions (decision 2). The store's `ROWS_MAX` bounds the rows it reaches. Inputs:
/// the transaction, the fold, the change and the time. Output: Ok, or the SQLite failure.
fn fold_change(
    tx: &Transaction<'_>,
    fold: &NoticeFold,
    change: FoldChange,
    now: i64,
) -> Result<(), EngineError> {
    assert!(fold.newest >= 0);
    assert!(now >= 0);
    let scope = format!(
        "dismissed_ms IS NULL AND ({}) AND kind=?1 AND project IS ?2 AND id<=?3",
        filter_sql(fold.filter)
    );
    let kind = fold.kind.name();
    let project = fold.project_id.as_ref().map(ProjectId::as_str);
    let run = |sql: String, timed: bool| -> Result<usize, EngineError> {
        let ran = if timed {
            tx.execute(&sql, params![kind, project, fold.newest, now])
        } else {
            tx.execute(&sql, params![kind, project, fold.newest])
        };
        ran.map_err(error)
    };
    match change {
        FoldChange::Read => {
            run(
                format!("UPDATE owner_notices SET read_ms=?4 WHERE {scope} AND read_ms IS NULL"),
                true,
            )?;
        }
        FoldChange::Unread => {
            run(
                format!("UPDATE owner_notices SET read_ms=NULL WHERE {scope}"),
                false,
            )?;
        }
        FoldChange::Delete => {
            run(
                format!(
                    "DELETE FROM owner_notices WHERE {scope} AND NOT (action=1 AND resolved_ms IS NULL)"
                ),
                false,
            )?;
            run(
                format!(
                    "UPDATE owner_notices SET dismissed_ms=?4 WHERE {scope} AND action=1 AND resolved_ms IS NULL"
                ),
                true,
            )?;
        }
    }
    Ok(())
}

/// Deletes items: an Info item or a resolved Action goes, a pending Action is hidden until
/// its state clears (decision 2). Inputs: the transaction, the ids and the time. Output: Ok,
/// or the SQLite failure.
fn delete(tx: &Transaction<'_>, ids: &[i64], now: i64) -> Result<(), EngineError> {
    each(
        tx,
        ids,
        "DELETE FROM owner_notices WHERE id=?1 AND NOT (action=1 AND resolved_ms IS NULL)",
        None,
    )?;
    each(
        tx,
        ids,
        "UPDATE owner_notices SET dismissed_ms=?2
         WHERE id=?1 AND action=1 AND resolved_ms IS NULL AND dismissed_ms IS NULL",
        Some(now),
    )
}

/// Records a client's report as an Info item: an app update once per version, the engine
/// lost at most once a minute. Inputs: the transaction, the report and the time. Output:
/// Applied, or `notification_report_invalid` for a version that is not 1 to
/// `VERSION_BYTES_MAX` letters, digits, dots, dashes and pluses.
fn reported(tx: &Transaction<'_>, report: &NoticeReport, now: i64) -> Result<Outcome, EngineError> {
    match report {
        NoticeReport::AppUpdateReady { version } => {
            if !plain_version(version) {
                return Ok(rejected("notification_report_invalid"));
            }
            let subject = format!("app_update:{version}");
            let text = format!("CrazyAgent {version} is ready to install.");
            record(
                tx,
                &app_info(
                    NoticeKind::AppUpdateReady,
                    &subject,
                    &text,
                    NoticeTarget::Updates,
                ),
                now,
            )?;
        }
        NoticeReport::EngineLost => {
            let last: Option<i64> = tx
                .query_row(
                    "SELECT MAX(created_ms) FROM owner_notices WHERE kind=?1",
                    [NoticeKind::EngineLost.name()],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if last.is_some_and(|last| now - last <= ENGINE_LOST_GAP_MS) {
                return Ok(Outcome::Applied);
            }
            let subject = format!("engine_lost:{now}");
            let text = "CrazyAgent lost the engine for a while and connected to it again.";
            record(
                tx,
                &app_info(NoticeKind::EngineLost, &subject, text, NoticeTarget::Home),
                now,
            )?;
        }
    }
    Ok(Outcome::Applied)
}

/// Whether a reported version is plain: 1 to `VERSION_BYTES_MAX` letters, digits, dots,
/// dashes and pluses. Input: the version. Output: the answer.
pub(crate) fn plain_version(version: &str) -> bool {
    let plain = version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    !version.is_empty() && version.len() <= VERSION_BYTES_MAX && plain
}

fn app_info<'a>(
    kind: NoticeKind,
    subject: &'a str,
    text: &'a str,
    target: NoticeTarget,
) -> InfoNotice<'a> {
    InfoNotice {
        kind,
        project_id: None,
        subject,
        text,
        target,
    }
}

/// Records one Info event, once per kind and subject while its item exists, unless the
/// owner turned the kind Off; the oldest finished rows make room past `ROWS_MAX`. Inputs:
/// the transaction, the event and the time. Output: the new id, or none when it was Off,
/// had no text or was already stored.
pub(crate) fn record(
    tx: &Connection,
    info: &InfoNotice<'_>,
    now: i64,
) -> Result<Option<i64>, EngineError> {
    assert!(!tx.is_autocommit());
    assert_eq!(info.kind.class(), NoticeClass::Info);
    assert_eq!(info.target.project(), info.project_id);
    assert!(!info.subject.is_empty() && info.subject.len() <= SUBJECT_BYTES_MAX);
    if settings(tx)?.delivery(info.kind) == Delivery::Off {
        return Ok(None);
    }
    let text = shown(info.text);
    if text.is_empty() {
        return Ok(None);
    }
    let added = tx
        .execute(
            "INSERT INTO owner_notices(kind,action,project,subject,text,target,created_ms)
             VALUES (?1,0,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
            params![
                info.kind.name(),
                info.project_id.map(ProjectId::as_str),
                info.subject,
                text,
                target_text(&info.target)?,
                now
            ],
        )
        .map_err(error)?;
    if added == 0 {
        return Ok(None);
    }
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    cap(tx)?;
    Ok(Some(id))
}

/// Records one Info event for a producer that must never fail because of it, inside or
/// outside the producer's own transaction: the write runs under its own savepoint, and a
/// failure rolls back only that write and is logged by its plain code. Inputs: the
/// producer's connection, the event and the time.
pub fn note(connection: &Connection, info: &InfoNotice<'_>, now: i64) {
    let result = connection
        .execute_batch("SAVEPOINT owner_notice")
        .map_err(error)
        .and_then(|()| {
            let written = record(connection, info, now);
            let end = if written.is_ok() {
                "RELEASE owner_notice"
            } else {
                "ROLLBACK TO owner_notice; RELEASE owner_notice"
            };
            connection.execute_batch(end).map_err(error)?;
            written.map(drop)
        });
    if let Err(problem) = result {
        let code: String = problem.0.chars().take(200).collect();
        eprintln!("notification_not_recorded: {code}");
    }
}

/// Removes what the owner no longer needs: resolved Actions and read Info items older than
/// the retention days, then the oldest finished rows past `ROWS_MAX`. Inputs: the
/// transaction, the settings and the time. Output: Ok, or the SQLite failure.
pub(crate) fn prune(
    tx: &Transaction<'_>,
    settings: &NoticeSettings,
    now: i64,
) -> Result<(), EngineError> {
    assert!((RETENTION_DAYS_MIN..=RETENTION_DAYS_MAX).contains(&settings.retention_days));
    let cutoff = now - i64::from(settings.retention_days) * DAY_MS;
    tx.execute(
        "DELETE FROM owner_notices WHERE resolved_ms IS NOT NULL AND resolved_ms < ?1",
        [cutoff],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM owner_notices WHERE action=0 AND read_ms IS NOT NULL AND read_ms < ?1",
        [cutoff],
    )
    .map_err(error)?;
    cap(tx)
}

/// Whether `prune` would remove anything now. Inputs: the database, the settings and the
/// time. Output: the answer.
pub(crate) fn prune_due(
    connection: &Connection,
    settings: &NoticeSettings,
    now: i64,
) -> Result<bool, EngineError> {
    let cutoff = now - i64::from(settings.retention_days) * DAY_MS;
    let (old, rows): (bool, i64) = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_notices WHERE (resolved_ms IS NOT NULL AND resolved_ms < ?1)
                 OR (action=0 AND read_ms IS NOT NULL AND read_ms < ?1)),
             (SELECT COUNT(*) FROM owner_notices)",
            [cutoff],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?;
    assert!(rows >= 0);
    Ok(old || rows > ROWS_MAX)
}

/// Drops the oldest rows that wait on nobody, Info items and resolved Actions, while the
/// store holds more than `ROWS_MAX`. Input: the transaction. Output: Ok, or the SQLite
/// failure.
fn cap(tx: &Connection) -> Result<(), EngineError> {
    let rows: i64 = tx
        .query_row("SELECT COUNT(*) FROM owner_notices", [], |row| row.get(0))
        .map_err(error)?;
    let over = rows - ROWS_MAX;
    if over <= 0 {
        return Ok(());
    }
    let dropped = tx
        .execute(
            "DELETE FROM owner_notices WHERE id IN (SELECT id FROM owner_notices
             WHERE NOT (action=1 AND resolved_ms IS NULL) ORDER BY id LIMIT ?1)",
            [over],
        )
        .map_err(error)?;
    assert!(dropped as i64 <= over);
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod reconcile_tests;

#[cfg(test)]
mod events_tests;
