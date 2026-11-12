use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::questions::{Ask, EngineSource, Settlement};
use crate::team::{bound_text, now_ms};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub mod binding;
#[cfg(unix)]
pub mod gate;
#[cfg(unix)]
pub mod integrity;
pub mod markers;
pub mod profile;
pub mod queue;
pub mod record;
pub mod runner;
pub mod settings;
pub mod snapshot;

#[cfg(all(test, unix))]
mod tests {
    #[cfg(target_os = "macos")]
    mod gate;
    mod profile;
    #[cfg(target_os = "macos")]
    mod record;
}

pub use profile::{
    AdmittedCheck, AdmittedProfile, CheckKind, CheckProfile, CheckSpec, HoldoutSpec, NetworkMode,
    ProfileId, ReportFormat, Trigger,
};
pub use settings::{ChecksChoice, ChecksSettings, Scope};

/// The most profiles one project keeps. Past it the oldest waiting master or agent proposal
/// makes room; the owner's proposals and approved and revoked profiles stay as the record of
/// admissions.
pub const PROFILES_KEPT_MAX: i64 = 256;
/// The most holdout files one project stores.
pub const HOLDOUT_FILES_STORED_MAX: i64 = 1024;
/// The most proposals one master or agent keeps waiting in a project; its next one makes
/// room by removing its own oldest.
pub const PENDING_PER_PROPOSER_MAX: i64 = 16;
/// The most profile summaries one `profiles` page lists; two pages hold every kept profile.
pub const PROFILES_PAGE_MAX: usize = 128;

const _: () = assert!(2 * PROFILES_PAGE_MAX as i64 >= PROFILES_KEPT_MAX);

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS check_profiles (
    project TEXT NOT NULL,
    id TEXT NOT NULL,
    body TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    proposed_ms INTEGER NOT NULL,
    approved_ms INTEGER,
    revoked_ms INTEGER,
    question INTEGER,
    PRIMARY KEY (project, id)
);
CREATE TABLE IF NOT EXISTS check_profile_current (
    project TEXT PRIMARY KEY,
    profile TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS check_holdout_files (
    project TEXT NOT NULL,
    digest TEXT NOT NULL,
    bytes BLOB NOT NULL,
    stored_ms INTEGER NOT NULL,
    PRIMARY KEY (project, digest)
);
CREATE TABLE IF NOT EXISTS check_settings (
    scope TEXT PRIMARY KEY,
    body TEXT NOT NULL,
    version INTEGER NOT NULL
);";
/// The questions refusals that mean the owner already has the most questions waiting.
const QUESTIONS_FULL: [&str; 2] = ["questions_asker_full", "questions_full"];

/// A refused checks request: one plain code such as `checks_profile_invalid`, never protected
/// check material.
pub type CheckRefusal = EngineError;

/// Who acts on check profiles and settings, as the engine sets it from the connection that
/// asked, never from the request: the owner, or a team master or sub-agent by its team agent
/// id. Stored as `owner`, `master:<id>` and `agent:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Owner,
    Master(i64),
    Agent(i64),
}

/// Where a stored profile stands in its project: proposed and never approved, the current
/// one, approved once but replaced, or revoked by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileState {
    Proposed,
    Current,
    Approved,
    Revoked,
}

/// One stored profile as the owner reviews it, with who proposed it, when it was last
/// approved or revoked, and the owner question its last master or agent proposal raised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProfile {
    pub id: ProfileId,
    pub project: ProjectId,
    pub profile: CheckProfile,
    pub proposed_by: String,
    pub proposed_ms: i64,
    pub approved_ms: Option<i64>,
    pub revoked_ms: Option<i64>,
    pub question: Option<u64>,
    pub state: ProfileState,
}

/// One stored profile as a list of profiles shows it, without its body: its id and state,
/// who proposed it and when, when it was last approved or revoked, its owner question, and
/// how many checks and holdouts it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSummary {
    pub id: ProfileId,
    pub state: ProfileState,
    pub proposed_by: String,
    pub proposed_ms: i64,
    pub approved_ms: Option<i64>,
    pub revoked_ms: Option<i64>,
    pub question: Option<u64>,
    pub checks: u32,
    pub holdouts: u32,
}

/// One page of a project's stored profiles, newest first, and the cursor of the page after
/// it, none on the last page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePage {
    pub profiles: Vec<ProfileSummary>,
    pub next: Option<u64>,
}

/// One `check_profiles` row as stored, with whether it is the project's current profile.
struct ProfileRow {
    body: String,
    proposed_by: String,
    proposed_ms: i64,
    approved_ms: Option<i64>,
    revoked_ms: Option<i64>,
    question: Option<i64>,
    current: bool,
}

/// What a proposal does about its owner question once stored: nothing, or ask, returning the
/// profile's earlier question instead while that one still waits.
enum Asking {
    No,
    Ask(Option<u64>),
}

/// The outcome of a proposal: the profile's identity, and the owner question it raised when
/// a master or agent proposed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProfileId,
    pub question: Option<u64>,
}

impl Actor {
    /// The stored actor name. Output: `owner`, `master:<id>` or `agent:<id>`.
    pub fn name(self) -> String {
        match self {
            Self::Owner => "owner".into(),
            Self::Master(id) => format!("master:{id}"),
            Self::Agent(id) => format!("agent:{id}"),
        }
    }

    /// Who proposed, as the owner reads it. Output: the words.
    fn spoken(self) -> String {
        match self {
            Self::Owner => "the owner".into(),
            Self::Master(id) => format!("the master (agent {id})"),
            Self::Agent(id) => format!("agent {id}"),
        }
    }
}

/// Creates the check profile, holdout file, settings, queue and run binding tables. Input:
/// the engine connection. Output: Ok, or the storage error.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(SCHEMA)
        .map_err(|error| EngineError(error.to_string()))?;
    queue::initialize(connection)
}

/// Stores a proposed profile by content. A master's or agent's proposal of a profile that is
/// not the current one raises one owner question, kept on the profile until approve, revoke
/// or eviction settles it; a later proposal while it waits returns it. When the owner already
/// has the most questions waiting, the profile is stored without one. The owner's own
/// proposal raises none, and only the owner proposes in a project whose trust level allows no
/// agent proposals (threat model D5). Inputs: the engine connection, the project, the profile
/// JSON and the actor. Output: the proposal, or `checks_profile_invalid`,
/// `checks_owner_only`, `checks_holdout_missing`, `checks_profile_revoked`,
/// `checks_profiles_full`, `checks_project_missing`, another questions refusal, or
/// `checks_unavailable`.
pub fn propose(
    db: &Connection,
    project: &ProjectId,
    body: &[u8],
    actor: Actor,
) -> Result<Proposal, CheckRefusal> {
    let profile = CheckProfile::import(body)?;
    let canonical = profile.canonical()?;
    let id = profile.identity()?;
    let tx = immediate(db)?;
    project_known(&tx, project)?;
    if actor != Actor::Owner {
        let trust = crate::trust::level(&tx, project.as_str()).map_err(unavailable)?;
        if !crate::trust::Enforcement::for_level(trust).agent_profile_proposals {
            return Err(failure("checks_owner_only"));
        }
    }
    for digest in profile
        .holdouts
        .iter()
        .flat_map(|holdout| holdout.files.values())
    {
        if !holdout_stored(&tx, project, digest)? {
            return Err(failure("checks_holdout_missing"));
        }
    }
    let mut evicted = Vec::new();
    let asking = match stored(&tx, project, &id)? {
        Some(known) if known.state == ProfileState::Revoked && actor != Actor::Owner => {
            return Err(failure("checks_profile_revoked"));
        }
        Some(known) if actor == Actor::Owner || known.state == ProfileState::Current => Asking::No,
        Some(known) => Asking::Ask(known.question),
        None => {
            evicted = make_room(&tx, project, actor)?;
            tx.execute(
                "INSERT INTO check_profiles(project,id,body,proposed_by,proposed_ms) VALUES (?1,?2,?3,?4,?5)",
                params![
                    project.as_str(),
                    id.as_str(),
                    String::from_utf8(canonical).map_err(unavailable)?,
                    actor.name(),
                    now_ms()
                ],
            )
            .map_err(unavailable)?;
            if actor == Actor::Owner {
                Asking::No
            } else {
                Asking::Ask(None)
            }
        }
    };
    tx.commit().map_err(unavailable)?;
    dismiss_all(db, project, &evicted);
    let question = match asking {
        Asking::No => None,
        Asking::Ask(Some(earlier)) if question_waiting(db, project, earlier)? => Some(earlier),
        Asking::Ask(_) => ask_owner(db, project, &id, &profile, actor)?,
    };
    assert!(question.is_none() || actor != Actor::Owner);
    Ok(Proposal { id, question })
}

/// Approves a stored profile as the project's current one, as a new admission whose time is
/// later than every earlier approval in the project, and settles its owner question.
/// Approving the current profile again changes nothing; approving a revoked one restores it.
/// Inputs: the engine connection, the project, the profile id and the actor. Output: the
/// admitted profile, or `checks_owner_only`, `checks_profile_unknown` or `checks_unavailable`.
pub fn approve(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
    actor: Actor,
) -> Result<AdmittedProfile, CheckRefusal> {
    owner_only(actor)?;
    let tx = immediate(db)?;
    let known = stored(&tx, project, id)?.ok_or_else(|| failure("checks_profile_unknown"))?;
    if known.state == ProfileState::Current {
        let approved_ms = known.approved_ms.expect("the current profile was approved");
        return Ok(AdmittedProfile {
            id: known.id,
            project: known.project,
            approved_ms,
            profile: known.profile,
        });
    }
    let last: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(approved_ms),0) FROM check_profiles WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let approved_ms = now_ms().max(last.saturating_add(1));
    tx.execute(
        "UPDATE check_profiles SET approved_ms=?3, revoked_ms=NULL WHERE project=?1 AND id=?2",
        params![project.as_str(), id.as_str(), approved_ms],
    )
    .map_err(unavailable)?;
    tx.execute(
        "INSERT INTO check_profile_current(project,profile) VALUES (?1,?2)
         ON CONFLICT(project) DO UPDATE SET profile=excluded.profile",
        params![project.as_str(), id.as_str()],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    assert!(approved_ms > last);
    dismiss_all(db, project, known.question.as_slice());
    Ok(AdmittedProfile {
        id: known.id,
        project: known.project,
        approved_ms,
        profile: known.profile,
    })
}

/// Revokes a stored profile and settles its owner question; a revoked current profile leaves
/// the project with none. Inputs: the engine connection, the project, the profile id and the
/// actor. Output: Ok, or `checks_owner_only`, `checks_profile_unknown` or `checks_unavailable`.
pub fn revoke(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
    actor: Actor,
) -> Result<(), CheckRefusal> {
    owner_only(actor)?;
    let tx = immediate(db)?;
    let known = stored(&tx, project, id)?.ok_or_else(|| failure("checks_profile_unknown"))?;
    let changed = tx
        .execute(
            "UPDATE check_profiles SET revoked_ms=COALESCE(revoked_ms,?3) WHERE project=?1 AND id=?2",
            params![project.as_str(), id.as_str(), now_ms()],
        )
        .map_err(unavailable)?;
    tx.execute(
        "DELETE FROM check_profile_current WHERE project=?1 AND profile=?2",
        params![project.as_str(), id.as_str()],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    assert_eq!(changed, 1);
    dismiss_all(db, project, known.question.as_slice());
    Ok(())
}

/// Reads one stored profile. Inputs: the engine connection, the project and the profile id.
/// Output: the profile, none when the project holds no such profile, or `checks_unavailable`.
pub fn show(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
) -> Result<Option<StoredProfile>, CheckRefusal> {
    stored(db, project, id)
}

/// Lists one page of a project's stored profiles, newest first, at most
/// `PROFILES_PAGE_MAX`, without their bodies. Inputs: the engine connection, the project and
/// the `next` of the page before, none for the first. Output: the page, or
/// `checks_project_missing` or `checks_unavailable`.
pub fn profiles(
    db: &Connection,
    project: &ProjectId,
    after: Option<u64>,
) -> Result<ProfilePage, CheckRefusal> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(unavailable)?;
    project_known(&tx, project)?;
    let before = after.map_or(i64::MAX, |after| i64::try_from(after).unwrap_or(i64::MAX));
    let limit = i64::try_from(PROFILES_PAGE_MAX + 1).expect("the page bound fits in i64");
    let rows: Vec<(i64, String)> = tx
        .prepare(
            "SELECT rowid, id FROM check_profiles WHERE project=?1 AND rowid < ?2
             ORDER BY rowid DESC LIMIT ?3",
        )
        .map_err(unavailable)?
        .query_map(params![project.as_str(), before, limit], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    let more = rows.len() > PROFILES_PAGE_MAX;
    let shown = &rows[..rows.len().min(PROFILES_PAGE_MAX)];
    let mut profiles = Vec::with_capacity(shown.len());
    for (_, id) in shown {
        let id = ProfileId::new(id.clone()).map_err(unavailable)?;
        let known = stored(&tx, project, &id)?.ok_or_else(|| failure("checks_unavailable"))?;
        profiles.push(ProfileSummary {
            id: known.id,
            state: known.state,
            proposed_by: known.proposed_by,
            proposed_ms: known.proposed_ms,
            approved_ms: known.approved_ms,
            revoked_ms: known.revoked_ms,
            question: known.question,
            checks: u32::try_from(known.profile.checks.len()).map_err(unavailable)?,
            holdouts: u32::try_from(known.profile.holdouts.len()).map_err(unavailable)?,
        });
    }
    let next = match shown.last() {
        Some((rowid, _)) if more => Some(u64::try_from(*rowid).map_err(unavailable)?),
        _ => None,
    };
    assert!(profiles.len() <= PROFILES_PAGE_MAX);
    assert!(next.is_none() || profiles.len() == PROFILES_PAGE_MAX);
    Ok(ProfilePage { profiles, next })
}

/// The project's current admitted profile. Inputs: the engine connection or a transaction
/// on it, and the project. Output: the profile, none when the owner approved none or revoked
/// the current one, or `checks_unavailable`.
pub fn current_profile(
    db: &Connection,
    project: &ProjectId,
) -> Result<Option<AdmittedProfile>, CheckRefusal> {
    let current: Option<String> = db
        .query_row(
            "SELECT profile FROM check_profile_current WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let Some(current) = current else {
        return Ok(None);
    };
    let id = ProfileId::new(current).map_err(unavailable)?;
    let known = stored(db, project, &id)?.ok_or_else(|| failure("checks_unavailable"))?;
    assert_eq!(known.state, ProfileState::Current);
    Ok(Some(AdmittedProfile {
        id: known.id,
        project: known.project,
        approved_ms: known.approved_ms.expect("the current profile was approved"),
        profile: known.profile,
    }))
}

/// Stores one holdout file the owner gives, by content. Inputs: the engine connection, the
/// project, the bytes and the actor. Output: the bytes' SHA-256 hex digest a profile names,
/// or `checks_owner_only`, `checks_holdout_invalid`, `checks_holdouts_full`,
/// `checks_project_missing` or `checks_unavailable`.
pub fn store_holdout_file(
    db: &Connection,
    project: &ProjectId,
    bytes: &[u8],
    actor: Actor,
) -> Result<String, CheckRefusal> {
    owner_only(actor)?;
    if bytes.is_empty() || bytes.len() > profile::HOLDOUT_FILE_BYTES_MAX {
        return Err(failure("checks_holdout_invalid"));
    }
    let digest = format!("{:x}", Sha256::digest(bytes));
    let tx = immediate(db)?;
    project_known(&tx, project)?;
    if !holdout_stored(&tx, project, &digest)? {
        let held: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM check_holdout_files WHERE project=?1",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if held >= HOLDOUT_FILES_STORED_MAX {
            return Err(failure("checks_holdouts_full"));
        }
        tx.execute(
            "INSERT INTO check_holdout_files(project,digest,bytes,stored_ms) VALUES (?1,?2,?3,?4)",
            params![project.as_str(), digest, bytes, now_ms()],
        )
        .map_err(unavailable)?;
    }
    tx.commit().map_err(unavailable)?;
    assert_eq!(digest.len(), 64);
    Ok(digest)
}

/// The bytes of an admitted holdout's files, to copy into its run's snapshot. Inputs: the
/// engine connection, the project and the check. Output: each relative path with its bytes,
/// empty for an ordinary check, or `checks_holdout_missing` when a file is not stored in
/// the project or its bytes no longer match the digest, or `checks_unavailable`.
pub fn holdout_files(
    db: &Connection,
    project: &ProjectId,
    check: &AdmittedCheck,
) -> Result<BTreeMap<String, Vec<u8>>, CheckRefusal> {
    let Some(files) = &check.holdout else {
        return Ok(BTreeMap::new());
    };
    let mut found = BTreeMap::new();
    for (path, digest) in files {
        let bytes: Vec<u8> = db
            .query_row(
                "SELECT bytes FROM check_holdout_files WHERE project=?1 AND digest=?2",
                params![project.as_str(), digest],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?
            .ok_or_else(|| failure("checks_holdout_missing"))?;
        if format!("{:x}", Sha256::digest(&bytes)) != *digest {
            return Err(failure("checks_holdout_missing"));
        }
        found.insert(path.clone(), bytes);
    }
    assert_eq!(found.len(), files.len());
    Ok(found)
}

/// Raises the owner question for a master's or agent's proposal: where to review it, what it
/// runs, and that an answer here changes nothing. Holdouts are counted, never described,
/// since the questions page sits in the project folder. The question is kept on the profile,
/// and dismissed at once when the profile was revoked, made current or removed meanwhile.
/// Inputs: the connection, the project, the profile, its id and the proposer. Output: the
/// question id, none when the owner already has the most questions waiting, or the other
/// questions refusals.
fn ask_owner(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
    profile: &CheckProfile,
    actor: Actor,
) -> Result<Option<u64>, CheckRefusal> {
    let checks: Vec<String> = profile
        .checks
        .iter()
        .map(|check| format!("{}: {}", check.id, check.argv.join(" ")))
        .collect();
    let context = format!(
        "Checks:\n{}\nHoldouts: {}\nProtected paths: {}",
        checks.join("\n"),
        profile.holdouts.len(),
        profile.protected_paths.join(", ")
    );
    let ask = Ask {
        question: format!(
            "Check profile {} was proposed by {}. Review it in Project Settings > Checks and approve or revoke it there; an answer here changes nothing.",
            &id.as_str()[..12],
            actor.spoken()
        ),
        kind: crate::questions::QuestionKind::Text,
        options: Vec::new(),
        recommended: None,
        context: Some(bound_text(&context, crate::questions::CONTEXT_BYTES_MAX)),
    };
    let question = match crate::questions::raise(db, project, &EngineSource::Checks, &ask) {
        Ok(question) => question,
        Err(problem) if QUESTIONS_FULL.contains(&problem.0.as_str()) => return Ok(None),
        Err(problem) => return Err(problem),
    };
    let linked = db
        .execute(
            "UPDATE check_profiles SET question=?3 WHERE project=?1 AND id=?2 AND revoked_ms IS NULL
             AND NOT EXISTS(SELECT 1 FROM check_profile_current c WHERE c.project=?1 AND c.profile=?2)",
            params![
                project.as_str(),
                id.as_str(),
                i64::try_from(question).map_err(unavailable)?
            ],
        )
        .map_err(unavailable)?;
    if linked == 0 {
        dismiss_all(db, project, &[question]);
        return Ok(None);
    }
    assert_eq!(linked, 1);
    Ok(Some(question))
}

/// Whether one of the project's owner questions still waits for the owner, read outside any
/// open transaction since the questions log opens its own. Inputs: the connection, the
/// project and the question id. Output: true while pending, or the questions refusal.
fn question_waiting(
    db: &Connection,
    project: &ProjectId,
    question: u64,
) -> Result<bool, CheckRefusal> {
    Ok(crate::questions::pending(db, project)?
        .iter()
        .any(|waiting| waiting.id == question))
}

/// Dismisses the owner questions of settled profiles after the change committed, naming the
/// version each pending one holds. A question no longer pending stays as the owner left it; a
/// stale or failed dismissal is logged by code, never returned, since the change it follows
/// is already stored. Inputs: the connection, the project and the question ids. Output: none.
fn dismiss_all(db: &Connection, project: &ProjectId, questions: &[u64]) {
    if questions.is_empty() {
        return;
    }
    let waiting = match crate::questions::pending(db, project) {
        Ok(waiting) => waiting,
        Err(problem) => {
            eprintln!("checks_question_unsettled code={}", problem.0);
            return;
        }
    };
    for &question in questions {
        let Some(pending) = waiting.iter().find(|asked| asked.id == question) else {
            continue;
        };
        match crate::questions::dismiss(db, question, pending.version) {
            Ok(Settlement::Settled) => {}
            Ok(Settlement::Stale) => {
                eprintln!("checks_question_unsettled question={question} code=stale")
            }
            Err(problem) if problem.0 == "questions_not_found" => {}
            Err(problem) => eprintln!(
                "checks_question_unsettled question={question} code={}",
                problem.0
            ),
        }
    }
}

/// Reads one stored profile inside the caller's connection or transaction. Inputs: the
/// connection, the project and the id. Output: the profile, none, or `checks_unavailable`
/// when a stored row no longer reads as a valid profile.
fn stored(
    db: &Connection,
    project: &ProjectId,
    id: &ProfileId,
) -> Result<Option<StoredProfile>, CheckRefusal> {
    let row = db
        .query_row(
            "SELECT p.body, p.proposed_by, p.proposed_ms, p.approved_ms, p.revoked_ms, p.question,
                    EXISTS(SELECT 1 FROM check_profile_current c WHERE c.project=p.project AND c.profile=p.id)
             FROM check_profiles p WHERE p.project=?1 AND p.id=?2",
            params![project.as_str(), id.as_str()],
            |row| {
                Ok(ProfileRow {
                    body: row.get(0)?,
                    proposed_by: row.get(1)?,
                    proposed_ms: row.get(2)?,
                    approved_ms: row.get(3)?,
                    revoked_ms: row.get(4)?,
                    question: row.get(5)?,
                    current: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(unavailable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let profile = CheckProfile::import(row.body.as_bytes()).map_err(unavailable)?;
    if profile.identity()? != *id {
        return Err(failure("checks_unavailable"));
    }
    let state = match (row.revoked_ms, row.approved_ms) {
        (Some(_), _) => ProfileState::Revoked,
        (None, None) => ProfileState::Proposed,
        (None, Some(_)) if row.current => ProfileState::Current,
        (None, Some(_)) => ProfileState::Approved,
    };
    assert!(!row.current || state == ProfileState::Current);
    Ok(Some(StoredProfile {
        id: id.clone(),
        project: project.clone(),
        profile,
        proposed_by: row.proposed_by,
        proposed_ms: row.proposed_ms,
        approved_ms: row.approved_ms,
        revoked_ms: row.revoked_ms,
        question: row
            .question
            .map(u64::try_from)
            .transpose()
            .map_err(unavailable)?,
        state,
    }))
}

/// Makes room for one more profile in the project: a master or agent at
/// `PENDING_PER_PROPOSER_MAX` waiting proposals loses its own oldest, and a project at
/// `PROFILES_KEPT_MAX` loses its oldest waiting master or agent proposal. Inputs: the
/// transaction, the project and the proposer. Output: the owner questions of the removed
/// proposals, or `checks_profiles_full` when the project is full and nothing may go.
fn make_room(
    tx: &Transaction<'_>,
    project: &ProjectId,
    actor: Actor,
) -> Result<Vec<u64>, CheckRefusal> {
    let mut questions = Vec::new();
    if actor != Actor::Owner {
        let name = actor.name();
        let waiting: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM check_profiles WHERE project=?1 AND proposed_by=?2
                 AND approved_ms IS NULL AND revoked_ms IS NULL",
                params![project.as_str(), name],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if waiting >= PENDING_PER_PROPOSER_MAX {
            let removed = evict(tx, project, Some(name.as_str()))?
                .expect("a proposer at its bound has a waiting proposal");
            questions.extend(removed);
        }
    }
    let kept: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM check_profiles WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if kept >= PROFILES_KEPT_MAX {
        let removed = evict(tx, project, None)?.ok_or_else(|| failure("checks_profiles_full"))?;
        questions.extend(removed);
    }
    assert!(questions.len() <= 2);
    Ok(questions)
}

/// Removes the project's oldest waiting master or agent proposal, of one proposer when named;
/// the owner's proposals never go. Inputs: the transaction, the project and the proposer's
/// stored name. Output: none when nothing may go, else the removed proposal's owner question.
fn evict(
    tx: &Transaction<'_>,
    project: &ProjectId,
    proposer: Option<&str>,
) -> Result<Option<Option<u64>>, CheckRefusal> {
    let oldest: Option<(i64, Option<i64>)> = tx
        .query_row(
            "SELECT rowid, question FROM check_profiles
             WHERE project=?1 AND approved_ms IS NULL AND revoked_ms IS NULL
             AND proposed_by<>'owner' AND (?2 IS NULL OR proposed_by=?2)
             ORDER BY proposed_ms, rowid LIMIT 1",
            params![project.as_str(), proposer],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((rowid, question)) = oldest else {
        return Ok(None);
    };
    let removed = tx
        .execute("DELETE FROM check_profiles WHERE rowid=?1", [rowid])
        .map_err(unavailable)?;
    assert_eq!(removed, 1);
    Ok(Some(
        question
            .map(u64::try_from)
            .transpose()
            .map_err(unavailable)?,
    ))
}

fn holdout_stored(
    db: &Connection,
    project: &ProjectId,
    digest: &str,
) -> Result<bool, CheckRefusal> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM check_holdout_files WHERE project=?1 AND digest=?2)",
        params![project.as_str(), digest],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Refuses a project the engine does not know. Inputs: the connection and the project.
/// Output: Ok, or `checks_project_missing` or `checks_unavailable`.
/// Refuses a project the engine does not know. Inputs: the connection and the project.
/// Output: Ok, or `checks_project_missing` or `checks_unavailable`.
pub(crate) fn project_known(db: &Connection, project: &ProjectId) -> Result<(), CheckRefusal> {
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !known {
        return Err(failure("checks_project_missing"));
    }
    Ok(())
}

fn owner_only(actor: Actor) -> Result<(), CheckRefusal> {
    match actor {
        Actor::Owner => Ok(()),
        Actor::Master(_) | Actor::Agent(_) => Err(failure("checks_owner_only")),
    }
}

fn immediate(db: &Connection) -> Result<Transaction<'_>, CheckRefusal> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)
}

fn unavailable<E>(_: E) -> CheckRefusal {
    failure("checks_unavailable")
}

#[cfg(all(test, target_os = "macos"))]
#[path = "checks/tests/runner.rs"]
mod runner_tests;
