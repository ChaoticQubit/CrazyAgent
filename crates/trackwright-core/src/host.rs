use crate::model::{
    Actor, CheckKind, Link, OpKind, PolicyKey, ProjectKey, Refusal, ShortId, Ticket, TicketKind,
    TicketStatus, codes,
};
use crate::policy;
use crate::runs::SourceRef;
use crate::store::{self, StoreError};
use crate::tests_model::TestId;
use crate::text::{self, TextField};
use rusqlite::Transaction;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The most parts a confined path may have, folders and file name together.
pub const PATH_PARTS_MAX: usize = 8;

/// One link kind a host declares, for example `branch` or `change_set`. Fields: the stored
/// name and the label a person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkKind {
    pub name: &'static str,
    pub label: &'static str,
}

/// What a host knows of one link target. Fields: whether the target still exists, the
/// label to show for it, and the host's own state text (standalone: `unknown`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkState {
    pub live: bool,
    pub label: String,
    pub state: String,
}

/// Whether a host finds a check satisfied. Fields: the answer and one line of detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceState {
    pub satisfied: bool,
    pub detail: String,
}

/// What a host sees of one ticket when it decides whether an actor may act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketView {
    pub id: ShortId,
    pub title: String,
    pub kind: TicketKind,
    pub status: TicketStatus,
    pub assignee: Option<Actor>,
    pub created_by: Actor,
}

impl TicketView {
    /// The view of a stored ticket. Input: the ticket. Output: its view.
    pub fn of(ticket: &Ticket) -> Self {
        Self {
            id: ticket.short_id.clone(),
            title: ticket.title.clone(),
            kind: ticket.kind,
            status: ticket.status,
            assignee: ticket.assignee.clone(),
            created_by: ticket.created_by.clone(),
        }
    }
}

/// What a host sees of one check when it reports whether the check is satisfied. Fields:
/// the ticket, the criterion's position on it, the check's kind and its reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckView {
    pub ticket: ShortId,
    pub criterion: u32,
    pub kind: CheckKind,
    pub reference: String,
}

/// A change a host reports on one of its link targets, for example an accepted change
/// set. Fields: the link kind and the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEvent {
    pub kind: String,
    pub target: String,
}

/// Resolves the links a host declares. Every method gets the open transaction, so a host
/// reads its own tables in the same transaction as the core's write.
pub trait Links {
    /// The link kinds this host accepts. Output: the kinds; an empty list accepts any bounded
    /// kind, as the standalone host does.
    fn kinds(&self) -> &[LinkKind];

    /// Checks one target. Inputs: the transaction, the project, the kind and the target.
    /// Output: what the host knows of it, or a refusal that blocks the link.
    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal>;
}

/// Reports whether checks are satisfied, from the host's own records.
pub trait Evidence {
    /// Checks one check. Inputs: the transaction, the project and the check. Output: its
    /// state, or a refusal.
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal>;

    /// Tells whether this host knows the source a ticket's work is at, so a project may set
    /// `test_evidence = current_source`. Output: false unless the host also gives
    /// `current_source`; the standalone host keeps this default.
    fn tracks_source(&self) -> bool {
        false
    }

    /// The source a ticket's work is at now. Under `test_evidence = current_source` a `test`
    /// check counts only results from runs of that source. Inputs: the transaction, the
    /// project and the ticket. Output: the source, None when the host does not know it (the
    /// default), or a refusal.
    fn current_source(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _ticket: &TicketView,
    ) -> Result<Option<SourceRef>, Refusal> {
        Ok(None)
    }
}

/// Decides who may run which operation.
pub trait Authority {
    /// Checks one actor and operation. Inputs: the transaction, the project, the actor,
    /// the operation and the ticket it acts on, if any. Output: Ok, or the refusal.
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal>;

    /// Tells whether the host sets every actor from its own knowledge of the caller. Output:
    /// false when actors declare themselves, as in the standalone host, so policy binds
    /// honest agents only; a `policy_get` reply says which.
    fn actors_verified(&self) -> bool;

    /// Checks one actor recording evidence for one check, before anything is written. It
    /// has no default, so every host decides who may prove its checks. Inputs: the
    /// transaction, the project, the actor and the check. Output: Ok, or the refusal.
    fn evidence_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        check: &CheckView,
    ) -> Result<(), Refusal>;

    /// Tells which value, if any, the host locks one policy setting to, for example
    /// `run_writers = engine_only` where only the engine writes runs. `policy_set` refuses
    /// any other value with `policy_locked`, even from the owner, and still takes the locked
    /// value, so the host can store it at setup. Input: the setting. Output: the one value
    /// word allowed, or None (the default) when the setting is free.
    fn policy_locked(&self, _setting: PolicyKey) -> Option<&'static str> {
        None
    }

    /// Tells how much of one run an actor may read, for example only the outcome of a
    /// holdout run the host keeps from agents. Every run read (`run_show`, `run_list`,
    /// `coverage`) applies it. Inputs: the transaction, the project, the actor and the run.
    /// Output: the sight, `Full` by default, or the refusal.
    fn run_sight(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _actor: &Actor,
        _run: &TestId,
    ) -> Result<Sight, Refusal> {
        Ok(Sight::Full)
    }
}

/// How much of one run an actor reads: `Full` shows everything; `Outcome` shows only its
/// state, verdict, times and counts, with no title, source, policy, integrity or result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sight {
    Full,
    Outcome,
}

/// Says where export and import may touch files. It has no default, so every host confines
/// the paths its callers name.
pub trait Paths {
    /// Resolves the path a caller asked for to the one file the core may open, confined to
    /// the host's folder for the project, before any file is touched. Inputs: the
    /// transaction, the project, the actor, the operation (`export`, `import_preview` or
    /// `import_apply`) and the requested path. Output: the path to open, or the refusal.
    fn resolve_path(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal>;
}

/// Everything a host supplies: links, evidence, authority and file paths. Any type with
/// the four is a host.
pub trait Host: Links + Evidence + Authority + Paths {}

impl<T: Links + Evidence + Authority + Paths> Host for T {}

/// Joins a relative path under a root folder without leaving it: every part must be a
/// plain name, never `.`, `..`, a root or a drive, at most `PATH_PARTS_MAX` parts, and every
/// folder on the way must exist below the root as a real folder, not a link. The root
/// itself is the host's and is not checked. Each folder is checked once, so a writer inside
/// the root that swaps one for a link before the file is opened is not caught; a host
/// whose root takes untrusted writers needs more than this. Inputs: the root and the
/// requested path. Output: the joined path, or why it is refused, in words safe to show a
/// caller.
pub fn confined(root: &Path, requested: &Path) -> Result<PathBuf, &'static str> {
    let mut parts = Vec::with_capacity(PATH_PARTS_MAX);
    for component in requested.components() {
        let Component::Normal(part) = component else {
            return Err("the path must be relative, with no . or .. part");
        };
        if parts.len() == PATH_PARTS_MAX {
            return Err("the path has too many parts");
        }
        parts.push(part);
    }
    let Some((name, folders)) = parts.split_last() else {
        return Err("the path names no file");
    };
    let mut path = root.to_path_buf();
    for folder in folders {
        path.push(folder);
        let listed =
            fs::symlink_metadata(&path).map_err(|_| "a folder on the path does not exist")?;
        if !listed.file_type().is_dir() {
            return Err("a folder on the path is a link or not a folder");
        }
    }
    path.push(name);
    assert!(
        path.starts_with(root),
        "a confined path stays under its root"
    );
    assert_eq!(
        path.components().count(),
        root.components().count() + parts.len()
    );
    Ok(path)
}

/// Links a ticket to a host target: the text is checked, the actor authorized, the kind
/// matched against the host's kinds and the target resolved by the host in the same
/// transaction before anything is written, so a refused link writes nothing. A closed
/// ticket still takes links. Inputs: the transaction, the host, the actor, the ticket, the
/// kind, the target and the time. Output: the link and what the host knows of its target.
pub fn link(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<(Link, LinkState), StoreError> {
    text::check(TextField::LinkKind, kind)?;
    text::check(TextField::LinkTarget, target)?;
    let ticket = store::ticket(tx, id)?;
    policy::authorize(tx, host, actor, OpKind::Link, &ticket)?;
    let kinds = host.kinds();
    if !kinds.is_empty() && !kinds.iter().any(|declared| declared.name == kind) {
        return Err(store::refusal(
            codes::LINK_KIND_UNKNOWN,
            format!("this tracker links no {kind}"),
        ));
    }
    let state = host
        .resolve(tx, id.key(), kind, target)
        .map_err(StoreError::Refused)?;
    let link = store::add_link(tx, actor, id, kind, target, now_ms)?;
    assert_eq!(link.kind, kind);
    assert_eq!(link.target, target);
    Ok((link, state))
}

/// Removes a ticket's link after authorizing the actor. Inputs: the transaction, the host,
/// the actor, the ticket, the kind, the target and the time. Output: the removed link, or
/// `link_unknown` or the refusal.
pub fn unlink(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    id: &ShortId,
    kind: &str,
    target: &str,
    now_ms: i64,
) -> Result<Link, StoreError> {
    let ticket = store::ticket(tx, id)?;
    policy::authorize(tx, host, actor, OpKind::Unlink, &ticket)?;
    let removed = store::remove_link(tx, actor, id, kind, target, now_ms)?;
    assert_eq!(removed.kind, kind);
    assert_eq!(removed.target, target);
    Ok(removed)
}
