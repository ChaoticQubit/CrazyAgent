use super::project_of;
use super::settings::{self, CiEvidence, ManualEvidence};
use crate::protocol::ProjectId;
use rusqlite::{OptionalExtension, Transaction, params};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use trackwright_core::coverage;
use trackwright_core::host::{
    self as core_host, Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links,
    Paths, Sight, TicketView,
};
use trackwright_core::model::{Actor, CheckKind, OpKind, PolicyKey, ProjectKey, Refusal, codes};
use trackwright_core::policy;
use trackwright_core::runs::{RunWriters, SourceRef};
use trackwright_core::standalone::Standalone;
use trackwright_core::tests_model::{CaseProtectedVisibility, TestId, sees_protected};

/// A target that exists in the host's tables but belongs to another project.
pub const LINK_CROSS_PROJECT: &str = "link_cross_project";
/// A target this project does not have and never linked.
pub const LINK_BROKEN: &str = "link_broken";
/// The engine could not read its own tables.
pub const HOST_FAILED: &str = "host_failed";
/// A tracker project with no engine project behind it.
pub const PROJECT_NOT_ATTACHED: &str = "project_not_attached";
/// An actor the engine never issues.
pub const ACTOR_NOT_ENGINE: &str = "actor_not_engine";
/// An engine agent or master acting on another project's tickets.
pub const ACTOR_OTHER_PROJECT: &str = "actor_other_project";
/// Evidence on a manual or ci check from anyone but the owner while its setting is `owner`.
pub const EVIDENCE_OWNER_ONLY: &str = "evidence_owner_only";
/// Evidence on a ci check from an agent while `ci_evidence` is `engine`.
pub const EVIDENCE_ENGINE_ONLY: &str = "evidence_engine_only";
/// The core's `policy_set`: ticket policy changes through the engine's ticket settings.
pub const POLICY_IN_SETTINGS: &str = "policy_in_settings";

const OWNER: &str = "owner";
const ENGINE_PREFIX: &str = "engine:";
const GONE: &str = "gone";
const HEAD_BYTES_MAX: u64 = 512;
/// The folder under a project's `.crazyagent/` that `export` writes into.
pub const EXPORTS_DIRECTORY: &str = "exports";
const WORKTREES_SCANNED_MAX: i64 = 4096;

/// The link kinds the engine declares, each resolved against its own table in the same
/// transaction as the core's write.
pub const KINDS: [LinkKind; 7] = [
    LinkKind {
        name: "agent",
        label: "Agent",
    },
    LinkKind {
        name: "task",
        label: "Task",
    },
    LinkKind {
        name: "worktree",
        label: "Worktree",
    },
    LinkKind {
        name: "branch",
        label: "Branch",
    },
    LinkKind {
        name: "change_set",
        label: "Change set",
    },
    LinkKind {
        name: "memory",
        label: "Memory",
    },
    LinkKind {
        name: "question",
        label: "Question",
    },
];

const AGENT: &str = "SELECT project, role, status FROM team_agents WHERE id = ?1";
const MASTER_AGENT: &str = "SELECT project FROM team_agents WHERE id = ?1 AND role = 'master'";
const MASTER_SESSION: &str = "SELECT project FROM master_sessions WHERE id = ?1";
const TASK: &str = "SELECT project, status FROM team_tasks WHERE id = ?1";
const WORKTREE: &str = "SELECT project, agent FROM team_worktrees WHERE worktree = ?1";
const WORKTREES: &str = "SELECT project, agent, git_dir FROM team_worktrees
    ORDER BY project = ?1 DESC, agent LIMIT ?2";
const CHANGE_SET: &str = "SELECT project, state, pruned FROM landing_change_sets WHERE id = ?1";
const CURRENT_SOURCE: &str = "WITH linked AS (
    SELECT l.target FROM tk_links l
      JOIN tk_tickets t ON t.id = l.ticket
      JOIN tk_projects p ON p.id = t.project
     WHERE p.key = ?1 AND t.number = ?2 AND l.kind = 'change_set'
     ORDER BY l.created_ms DESC, l.target LIMIT 1)
SELECT c.id, c.base, c.head,
       (SELECT r.tree FROM check_runs r WHERE r.change_set = c.id AND r.commit_id = c.head
         ORDER BY r.run DESC LIMIT 1)
  FROM linked JOIN landing_change_sets c ON c.id = linked.target
 WHERE c.project = ?3";
const MEMORY: &str = "SELECT project FROM memory_library WHERE id = ?1";
const KNOWLEDGE: &str = "SELECT project FROM knowledge_heads WHERE id = ?1";
const QUESTION: &str = "SELECT project, status FROM owner_questions WHERE id = ?1";
const LINKED: &str = "SELECT EXISTS(SELECT 1 FROM tk_links l
    JOIN tk_tickets t ON t.id = l.ticket JOIN tk_projects p ON p.id = t.project
    WHERE p.key = ?1 AND l.kind = ?2 AND l.target = ?3)";
const HOLDOUT_RUN: &str = "SELECT holdout FROM check_runs WHERE short = ?1";

/// CrazyAgent as trackwright's host: links resolve against the team, landing, memory and
/// question tables of `engine.db`, `change` evidence follows the change sets, and authority
/// checks the engine-set actor before the project's policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct Host;

/// What the engine's tables say about one target: its project (None for an app-wide
/// memory), whether it still exists, its label and its state.
struct Found {
    project: Option<String>,
    live: bool,
    label: String,
    state: String,
}

fn failed(_: rusqlite::Error) -> Refusal {
    Refusal::host(HOST_FAILED, "the engine could not read its own records")
}

fn not_engine() -> Refusal {
    Refusal::host(ACTOR_NOT_ENGINE, "CrazyAgent sets every tracker actor")
}

fn other_project() -> Refusal {
    Refusal::host(
        ACTOR_OTHER_PROJECT,
        "this agent works on another project's tickets",
    )
}

/// Reads a team id written the one way the engine writes it: decimal, no sign, no leading
/// zero. Input: the text. Output: the id, or None.
fn number(text: &str) -> Option<i64> {
    let id = text.parse::<i64>().ok().filter(|id| *id >= 1)?;
    (id.to_string() == text).then_some(id)
}

/// Reads the branch a worktree's HEAD names, at most `HEAD_BYTES_MAX` bytes of it. Input:
/// the worktree's git folder. Output: the branch, or None for a detached or unreadable HEAD.
pub(super) fn checked_out(git_dir: &Path) -> Option<String> {
    let mut head = String::new();
    std::fs::File::open(git_dir.join("HEAD"))
        .ok()?
        .take(HEAD_BYTES_MAX)
        .read_to_string(&mut head)
        .ok()?;
    head.strip_prefix("ref: refs/heads/")
        .map(|branch| branch.trim_end_matches('\n').to_owned())
}

fn found(project: Option<String>, label: String, state: &str) -> Found {
    Found {
        project,
        live: true,
        label,
        state: state.to_owned(),
    }
}

impl Host {
    /// Looks one target up in the engine's tables. Inputs: the transaction, the engine
    /// project (to prefer its own worktree for a branch), the kind and the target.
    /// Output: what the tables hold, None when no row names it.
    fn find(
        tx: &Transaction<'_>,
        engine: &ProjectId,
        kind: &str,
        target: &str,
    ) -> Result<Option<Found>, Refusal> {
        let one = |sql: &str| -> Result<Option<(String, String)>, Refusal> {
            let Some(id) = number(target) else {
                return Ok(None);
            };
            tx.query_row(sql, [id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(failed)
        };
        Ok(match kind {
            "agent" => {
                let Some(id) = number(target) else {
                    return Ok(None);
                };
                tx.query_row(AGENT, [id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .optional()
                .map_err(failed)?
                .map(|(project, role, status)| {
                    found(Some(project), format!("Agent {id} ({role})"), &status)
                })
            }
            "task" => one(TASK)?
                .map(|(project, status)| found(Some(project), format!("Task {target}"), &status)),
            "question" => one(QUESTION)?.map(|(project, status)| {
                found(Some(project), format!("Question {target}"), &status)
            }),
            "worktree" => tx
                .query_row(WORKTREE, [target], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()
                .map_err(failed)?
                .map(|(project, agent)| {
                    found(Some(project), format!("Worktree of agent {agent}"), "open")
                }),
            "branch" => {
                let mut statement = tx.prepare(WORKTREES).map_err(failed)?;
                let rows = statement
                    .query_map(params![engine.as_str(), WORKTREES_SCANNED_MAX], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(failed)?;
                let mut hit = None;
                for row in rows {
                    let (project, agent, git_dir) = row.map_err(failed)?;
                    if checked_out(Path::new(&git_dir)).as_deref() == Some(target) {
                        hit = Some(found(
                            Some(project),
                            format!("{target} (agent {agent})"),
                            "checked out",
                        ));
                        break;
                    }
                }
                hit
            }
            "change_set" => tx
                .query_row(CHANGE_SET, [target], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })
                .optional()
                .map_err(failed)?
                .map(|(project, state, pruned)| Found {
                    project: Some(project),
                    live: !pruned,
                    label: "Change set".to_owned(),
                    state: if pruned { GONE.to_owned() } else { state },
                }),
            "memory" => {
                let library: Option<Option<String>> = tx
                    .query_row(MEMORY, [target], |row| row.get(0))
                    .optional()
                    .map_err(failed)?;
                match library {
                    Some(project) => Some(found(project, "Memory".to_owned(), "saved")),
                    None => tx
                        .query_row(KNOWLEDGE, [target], |row| row.get::<_, String>(0))
                        .optional()
                        .map_err(failed)?
                        .map(|project| found(Some(project), "Knowledge".to_owned(), "saved")),
                }
            }
            _ => {
                return Err(Refusal::new(
                    codes::LINK_KIND_UNKNOWN,
                    format!("CrazyAgent links no {kind}"),
                ));
            }
        })
    }
}

impl Links for Host {
    fn kinds(&self) -> &[LinkKind] {
        &KINDS
    }

    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        if !KINDS.iter().any(|declared| declared.name == kind) {
            return Err(Refusal::new(
                codes::LINK_KIND_UNKNOWN,
                format!("CrazyAgent links no {kind}"),
            ));
        }
        let engine = project_of(tx, project)?;
        let linked: bool = tx
            .query_row(LINKED, params![project.as_str(), kind, target], |row| {
                row.get(0)
            })
            .map_err(failed)?;
        let gone = || LinkState {
            live: false,
            label: target.to_owned(),
            state: GONE.to_owned(),
        };
        match Self::find(tx, &engine, kind, target)? {
            Some(found)
                if found
                    .project
                    .as_deref()
                    .is_none_or(|owner| owner == engine.as_str()) =>
            {
                Ok(LinkState {
                    live: found.live,
                    label: found.label,
                    state: found.state,
                })
            }
            Some(_) if linked => Ok(gone()),
            Some(_) => Err(Refusal::host(
                LINK_CROSS_PROJECT,
                format!("that {kind} belongs to another project"),
            )),
            None if linked => Ok(gone()),
            None => Err(Refusal::host(
                LINK_BROKEN,
                format!("this project has no {kind} {target}"),
            )),
        }
    }
}

impl Evidence for Host {
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        match check.kind {
            CheckKind::Manual | CheckKind::Ci => Standalone.check(tx, project, check),
            CheckKind::Test => Ok(coverage::test_check(tx, self, project, check)?.unwrap_or(
                EvidenceState {
                    satisfied: false,
                    detail: "a test check names a test case".to_owned(),
                },
            )),
            CheckKind::Change => {
                let engine = project_of(tx, project)?;
                let row: Option<(String, String, bool)> = tx
                    .query_row(CHANGE_SET, [&check.reference], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .optional()
                    .map_err(failed)?;
                let (satisfied, detail) = match row {
                    Some((owner, _, _)) if owner != engine.as_str() => {
                        (false, "not in this project".to_owned())
                    }
                    Some((_, _, true)) | None => (false, GONE.to_owned()),
                    Some((_, state, false)) => (state == "accepted", state),
                };
                Ok(EvidenceState { satisfied, detail })
            }
        }
    }

    fn tracks_source(&self) -> bool {
        true
    }

    /// The source a ticket's work is at: the head of the change set it links most recently,
    /// with that head's tree as the engine's checks recorded it. Inputs: the transaction,
    /// the project and the ticket. Output: the source, None when the ticket links no change
    /// set of this project or no check has recorded its head yet, or `host_failed`.
    fn current_source(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        ticket: &TicketView,
    ) -> Result<Option<SourceRef>, Refusal> {
        let engine = project_of(tx, project)?;
        let row: Option<(String, String, String, Option<String>)> = tx
            .query_row(
                CURRENT_SOURCE,
                params![project.as_str(), ticket.id.number(), engine.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(failed)?;
        Ok(row.and_then(|(change_set, base, head, tree)| {
            Some(SourceRef::Git {
                repository: engine.as_str().to_owned(),
                commit: head,
                tree: tree?,
                base: Some(base),
                change_set: Some(change_set),
            })
        }))
    }
}

/// Checks that an actor is one the engine issues and, for an agent or a master, that it
/// works on this project. Inputs: the transaction, the engine project and the actor.
/// Output: Ok, `actor_not_engine` or `actor_other_project`.
fn member(tx: &Transaction<'_>, engine: &ProjectId, actor: &Actor) -> Result<(), Refusal> {
    let text = actor.as_str();
    if text == OWNER {
        return Ok(());
    }
    if let Some(source) = text.strip_prefix(ENGINE_PREFIX) {
        return if source.is_empty() {
            Err(not_engine())
        } else {
            Ok(())
        };
    }
    let owner: Option<String> = if let Some(session) = text.strip_prefix("master:session:") {
        tx.query_row(MASTER_SESSION, [session], |row| row.get(0))
            .optional()
            .map_err(failed)?
    } else if let Some(id) = text.strip_prefix("master:") {
        let id = number(id).ok_or_else(not_engine)?;
        tx.query_row(MASTER_AGENT, [id], |row| row.get(0))
            .optional()
            .map_err(failed)?
    } else if let Some(id) = text.strip_prefix("agent:") {
        let id = number(id).ok_or_else(not_engine)?;
        tx.query_row(AGENT, [id], |row| row.get(0))
            .optional()
            .map_err(failed)?
    } else {
        return Err(not_engine());
    };
    match owner {
        Some(owner) if owner == engine.as_str() => Ok(()),
        _ => Err(other_project()),
    }
}

impl Authority for Host {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        if op == OpKind::PolicySet {
            return Err(Refusal::host(
                POLICY_IN_SETTINGS,
                "ticket policy is set in CrazyAgent's ticket settings",
            ));
        }
        let engine = project_of(tx, project)?;
        member(tx, &engine, actor)?;
        Standalone.actor_may(tx, project, actor, op, ticket)
    }

    fn actors_verified(&self) -> bool {
        true
    }

    fn evidence_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        check: &CheckView,
    ) -> Result<(), Refusal> {
        let kind = check.kind;
        if actor.as_str() == OWNER || matches!(kind, CheckKind::Change | CheckKind::Test) {
            return Ok(());
        }
        let engine = project_of(tx, project)?;
        let chosen = settings::effective(tx, &engine)
            .map_err(|_| Refusal::host(HOST_FAILED, "the engine could not read its settings"))?;
        let owner_only = || {
            Err(Refusal::host(
                EVIDENCE_OWNER_ONLY,
                format!("only the owner records evidence for a {kind} check"),
            ))
        };
        match (kind, chosen.manual_evidence, chosen.ci_evidence) {
            (CheckKind::Manual, ManualEvidence::Owner, _) => owner_only(),
            (CheckKind::Ci, _, CiEvidence::Owner) => owner_only(),
            (CheckKind::Ci, _, CiEvidence::Engine)
                if !actor.as_str().starts_with(ENGINE_PREFIX) =>
            {
                Err(Refusal::host(
                    EVIDENCE_ENGINE_ONLY,
                    "only CrazyAgent's own runs record evidence for a ci check",
                ))
            }
            _ => Ok(()),
        }
    }

    fn policy_locked(&self, setting: PolicyKey) -> Option<&'static str> {
        locked(setting)
    }

    fn run_sight(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        run: &TestId,
    ) -> Result<Sight, Refusal> {
        if sees_protected(actor) {
            return Ok(Sight::Full);
        }
        let holdout: Option<bool> = tx
            .query_row(HOLDOUT_RUN, [run.to_string()], |row| row.get(0))
            .optional()
            .map_err(failed)?;
        if holdout != Some(true) {
            return Ok(Sight::Full);
        }
        let visibility = policy::load(tx, project)
            .map_err(|_| Refusal::host(HOST_FAILED, "the engine could not read the policy"))?
            .case_protected_visibility;
        Ok(match visibility {
            CaseProtectedVisibility::Full => Sight::Full,
            CaseProtectedVisibility::Hidden | CaseProtectedVisibility::Summary => Sight::Outcome,
        })
    }
}

/// The one value the engine allows for a core policy key: `run_writers` is `engine_only`,
/// since only the engine's checks record automated runs and the gate counts only those.
/// Every other key is free. Input: the key. Output: the locked value word, or None.
pub(crate) fn locked(setting: PolicyKey) -> Option<&'static str> {
    (setting == PolicyKey::RunWriters).then(|| RunWriters::EngineOnly.as_str())
}

/// Finds a project's `.crazyagent/` folder from its stored setup and checks it is a real
/// folder, never a link. Inputs: the transaction, the engine project and the refusal code
/// of the operation. Output: the folder, or the refusal.
pub(crate) fn state_of(
    tx: &Transaction<'_>,
    engine: &ProjectId,
    code: &'static str,
) -> Result<PathBuf, Refusal> {
    let setup = crate::project_setup::load_setup(tx, engine)
        .map_err(|_| Refusal::host(HOST_FAILED, "the engine could not read the project"))?
        .ok_or_else(|| Refusal::host(HOST_FAILED, "the engine has no folder for this project"))?;
    let folder = setup.project_folder();
    if !folder.is_absolute() {
        return Err(Refusal::host(
            HOST_FAILED,
            "the engine stores a relative project folder",
        ));
    }
    let state = folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    real_folder(&state, code, "the project's .crazyagent folder")?;
    assert!(state.starts_with(&folder));
    Ok(state)
}

/// Checks that a path is a real folder, not a link and not missing. Inputs: the path, the
/// refusal code and the folder's name in words. Output: Ok, or the refusal.
fn real_folder(path: &Path, code: &'static str, named: &str) -> Result<(), Refusal> {
    match std::fs::symlink_metadata(path) {
        Ok(listed) if listed.file_type().is_dir() => Ok(()),
        _ => Err(Refusal::new(
            code,
            format!("{named} is missing, a link or not a folder"),
        )),
    }
}

/// Makes `.crazyagent/exports/` at the first export, readable by the owner only. A name
/// already there is left for `real_folder` to check. Inputs: the folder and the refusal
/// code. Output: Ok, or the refusal.
fn exports_folder(path: &Path, code: &'static str) -> Result<(), Refusal> {
    #[cfg(unix)]
    let created = std::fs::DirBuilder::new().mode(0o700).create(path);
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().create(path);
    match created {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err(Refusal::new(
            code,
            "the project's exports folder could not be made",
        )),
    }
}

impl Paths for Host {
    /// Confines `export` to `<project>/.crazyagent/exports/` and the import ops to
    /// `<project>/.crazyagent/context/`, after the actor check `actor_may` makes, so an
    /// agent never names a file outside its own project's folders. Inputs: the
    /// transaction, the key, the actor, the operation and the requested relative path.
    /// Output: the path to open, or `export_path_refused` / `import_path_refused`.
    fn resolve_path(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal> {
        let (code, below) = match op {
            OpKind::Export => (codes::EXPORT_PATH_REFUSED, EXPORTS_DIRECTORY),
            OpKind::ImportPreview | OpKind::ImportApply => (
                codes::IMPORT_PATH_REFUSED,
                crate::project_setup::CONTEXT_DIRECTORY,
            ),
            other => panic!("{other} opens no file"),
        };
        let engine = project_of(tx, project)?;
        member(tx, &engine, actor)?;
        let state = state_of(tx, &engine, code)?;
        let root = state.join(below);
        if op == OpKind::Export {
            exports_folder(&root, code)?;
        }
        real_folder(
            &root,
            code,
            &format!("the project's .crazyagent/{below} folder"),
        )?;
        let path = core_host::confined(&root, requested).map_err(|why| {
            Refusal::new(
                code,
                format!("{why}; name a .md file inside .crazyagent/{below}"),
            )
        })?;
        assert!(path.starts_with(&root));
        assert!(path.starts_with(&state));
        Ok(path)
    }
}
