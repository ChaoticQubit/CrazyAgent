use super::*;
use crate::protocol::encode_frame;
use crate::tracker::settings::{
    self, CiEvidence, ManualEvidence, Scope, TicketOverride, TicketSettings,
};
use crate::tracker::{self as engine_tracker, TRACKER_UNAVAILABLE, render};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use trackwright_core::catalog::{self, FieldType};
use trackwright_core::model::{
    AgentEditPolicy, ClosePolicy, CreatePolicy, CycleDays, ImportPolicy, OpKind, PolicyKey,
    ProjectKey, RolloverPolicy, ShortId,
};
use trackwright_core::ops;
use trackwright_core::policy::CYCLE_DAYS_CUSTOM;
use trackwright_core::reply::Reply as CoreReply;
use trackwright_core::requests::Request;
use trackwright_core::store;
use trackwright_core::tests_model::{
    CaseProtectedVisibility, RunRetention, RunWriters, SkippedCounts, TestEvidence,
};

const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub(super) const TRACKER_TIMEOUT: Duration = Duration::from_secs(30);
const TRACKED: &str = "SELECT project, key FROM tracker_projects ORDER BY project LIMIT ?1";
const TRACKED_COUNT: &str = "SELECT count(*) FROM tracker_projects";
const TRACKED_SHOWN_BYTES_MAX: usize = 400;

/// The most projects that have tickets, so the project list fits one reply.
pub const TRACKED_PROJECTS_MAX: usize = 128;

const _: () = assert!(
    TRACKED_PROJECTS_MAX * TRACKED_SHOWN_BYTES_MAX + 64 <= crate::protocol::FRAME_BYTES_MAX
);

/// The largest core reply envelope one Tracker reply carries; a longer page ends with the
/// core's cursor, so the client asks for the next page.
pub const TRACKER_REPLY_BYTES_MAX: usize = 48 * 1024;

const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(TRACKER_REPLY_BYTES_MAX + 2048 <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(TRACKER_REPLY_BYTES_MAX <= trackwright_core::reply::REPLY_BYTES_MAX);

/// Every code a Tracker request can answer an owner client with: the tracker core's own
/// (`trackwright_core::model::codes::ALL`, inside a refused envelope), the engine host's,
/// this protocol's, and the owner connection's it travels on. Sorted, each once.
pub const TRACKER_CODES: &[&str] = &[
    "active_plan_exists",
    "actor_invalid",
    "actor_not_engine",
    "actor_other_project",
    "agent_edit_refused",
    "already_claimed",
    "args_invalid",
    "case_link_exists",
    "case_link_unknown",
    "case_links_full",
    "case_protected",
    "case_unknown",
    "cases_full",
    "changes_unavailable",
    "check_exists",
    "check_unknown",
    "checks_full",
    "close_needs_evidence",
    "close_owner_only",
    "close_reason_open",
    "create_owner_only",
    "criteria_full",
    "criterion_has_checks",
    "criterion_needs_check",
    "criterion_unknown",
    "cross_project",
    "cursor_invalid",
    "cycle_closed",
    "cycle_days_invalid",
    "cycle_overlaps",
    "cycle_unknown",
    "cycles_full",
    "dependencies_full",
    "dependency_cycle",
    "dependency_exists",
    "dependency_invalid",
    "dependency_unknown",
    "evidence_engine_only",
    "evidence_not_for_agents",
    "evidence_owner_only",
    "export_path_refused",
    "export_too_large",
    "field_invalid",
    "host_failed",
    "import_format_unknown",
    "import_item_needs_version",
    "import_item_unknown",
    "import_keys_collide",
    "import_not_text",
    "import_path_refused",
    "import_too_large",
    "link_broken",
    "link_cross_project",
    "link_exists",
    "link_kind_unknown",
    "link_unknown",
    "links_full",
    "missing_field",
    "not_claimant",
    "notify_too_wide",
    "op_unavailable",
    "open_tickets_full",
    "option_needs_host",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_only",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "parent_invalid",
    "peer_owner_mismatch",
    "plan_closed",
    "plan_item_exists",
    "plan_item_unknown",
    "plan_items_full",
    "plan_unknown",
    "plans_full",
    "policy_in_settings",
    "policy_locked",
    "policy_value_invalid",
    "priority_invalid",
    "project_exists",
    "project_key_invalid",
    "project_not_attached",
    "project_unknown",
    "rate_limited",
    "release_unknown",
    "request_id_reused",
    "request_too_large",
    "result_name_repeated",
    "results_empty",
    "results_too_many",
    "run_batch_invalid",
    "run_closed",
    "run_finish_invalid",
    "run_integrity_invalid",
    "run_policy_invalid",
    "run_results_full",
    "run_source_invalid",
    "run_unknown",
    "run_writer_refused",
    "runs_full",
    "set_case_exists",
    "set_case_unknown",
    "set_cases_full",
    "set_unknown",
    "sets_full",
    "short_id_invalid",
    "stable_name_taken",
    "stale_version",
    "status_closed_at_create",
    "status_invalid",
    "steps_full",
    "store_failed",
    "store_not_private",
    "store_path_refused",
    "store_schema_newer",
    "test_id_invalid",
    "text_empty",
    "text_hidden_characters",
    "text_line_break",
    "text_too_long",
    "ticket_closed",
    "ticket_open",
    "ticket_unknown",
    "tickets_full",
    "tracker_outcome_unknown",
    "tracker_project_missing",
    "tracker_projects_full",
    "tracker_reply_bounds",
    "tracker_request_bounds",
    "tracker_request_invalid",
    "tracker_unavailable",
    "unknown_field",
    "unknown_op",
    "use_close",
    "version_unknown",
    "versioned_item_invalid",
    "versioned_unavailable",
    "versions_full",
];

/// A CrazyAgent project that has tickets. Fields: the engine project, its tracker key and
/// the name the tracker shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedProject {
    pub project: ProjectId,
    pub key: ProjectKey,
    pub name: String,
}

/// The bounds of a setting's custom number. Fields: the smallest and largest value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomBounds {
    pub min: u8,
    pub max: u8,
}

/// The choices of one ticket setting, as the settings screens list them. Fields: the
/// setting's key, every option in the order to show, the proposed default, and the bounds
/// of a custom number (`cycle_days` only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingChoices {
    pub key: String,
    pub options: Vec<serde_json::Value>,
    pub proposed: serde_json::Value,
    pub custom: Option<CustomBounds>,
}

/// What a ticket settings screen shows for one scope (the app, or one project). Fields: the
/// project (None for the app), the keys this scope sets (an unset key uses the scope
/// below), the version a save sends back, what each unset key takes (the app values for a
/// project, the proposed defaults for the app), the values in force, and the choices of
/// every key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketSettingsView {
    pub project: Option<ProjectId>,
    pub values: TicketOverride,
    pub version: u64,
    pub below: TicketSettings,
    pub effective: TicketSettings,
    pub choices: Vec<SettingChoices>,
}

/// The outcome of saving one scope's ticket settings: saved at a new version, or stale
/// because the scope changed since the screen read it (nothing was written; read again).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum SettingsSaved {
    Saved { version: u64 },
    Stale { current: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Op {
        op: String,
        args: serde_json::Value,
    },
    Projects {},
    Attach {
        project: ProjectId,
        key: ProjectKey,
        name: String,
    },
    Settings {
        project: Option<ProjectId>,
    },
    SaveSettings {
        project: Option<ProjectId>,
        values: TicketOverride,
        version: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    tracker: Action,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Envelope { envelope: serde_json::Value },
    Projects { projects: Vec<TrackedProject> },
    Attached { project: TrackedProject },
    Settings { settings: TicketSettingsView },
    Saved { saved: SettingsSaved },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("tracker_outcome_unknown")
}

impl OwnerClient {
    /// Runs one tracker operation of trackwright's schema v1 as the owner. Inputs: the
    /// operation and its input object, exactly as schema v1 names its fields. Output: the
    /// core's reply envelope (`api`, `ok`, `data` or `code` and `message`, `next`,
    /// `cursor`); a refused operation is an envelope with `ok` false, not an error.
    pub async fn tracker_op(
        &self,
        op: OpKind,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        let action = Action::Op {
            op: op.as_str().to_owned(),
            args,
        };
        match self.tracker_exchange(action).await? {
            Reply::Envelope { envelope } => Ok(envelope),
            _ => Err(unknown()),
        }
    }

    /// Lists the projects that have tickets, by engine project. Output: the projects.
    pub async fn tracker_projects(&self) -> Result<Vec<TrackedProject>, EngineError> {
        match self.tracker_exchange(Action::Projects {}).await? {
            Reply::Projects { projects } => Ok(projects),
            _ => Err(unknown()),
        }
    }

    /// Gives an engine project its tickets under the key the owner chose, with the
    /// project's ticket settings in force. Inputs: the project, the key and the name.
    /// Output: the tracked project, or `project_exists`, `tracker_project_missing` or the
    /// core's refusal of the key or name.
    pub async fn tracker_attach(
        &self,
        project: ProjectId,
        key: ProjectKey,
        name: String,
    ) -> Result<TrackedProject, EngineError> {
        let action = Action::Attach { project, key, name };
        match self.tracker_exchange(action).await? {
            Reply::Attached { project } => Ok(project),
            _ => Err(unknown()),
        }
    }

    /// Reads the ticket settings of the app (None) or one project. Input: the scope.
    /// Output: what the settings screen shows, or `project_not_attached`.
    pub async fn tracker_settings(
        &self,
        project: Option<ProjectId>,
    ) -> Result<TicketSettingsView, EngineError> {
        match self.tracker_exchange(Action::Settings { project }).await? {
            Reply::Settings { settings } => Ok(settings),
            _ => Err(unknown()),
        }
    }

    /// Saves the ticket settings of the app (None) or one project at the version the
    /// screen read; an empty set of values is "Use app default" for every key. Inputs: the
    /// scope, the values and the version. Output: saved or stale, or
    /// `project_not_attached`.
    pub async fn tracker_save_settings(
        &self,
        project: Option<ProjectId>,
        values: TicketOverride,
        version: u64,
    ) -> Result<SettingsSaved, EngineError> {
        let action = Action::SaveSettings {
            project,
            values,
            version,
        };
        match self.tracker_exchange(action).await? {
            Reply::Saved { saved } => Ok(saved),
            _ => Err(unknown()),
        }
    }

    async fn tracker_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(TRACKER_TIMEOUT), async {
            let frame = encode_frame(&Envelope { tracker: action })
                .map_err(|_| failure("tracker_request_bounds"))?;
            if frame.len() > REQUEST_BYTES_MAX + 4 {
                return Err(failure("tracker_request_bounds"));
            }
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("tracker_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("tracker_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("tracker_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "tracker_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    let action = parse(bytes)?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, action))
        .await
        .map_err(|_| failure(TRACKER_UNAVAILABLE))?
}

/// Reads one Tracker request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the action, or `tracker_request_bounds` or
/// `tracker_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("tracker_request_bounds"));
    }
    let Envelope { tracker } =
        serde_json::from_slice(bytes).map_err(|_| failure("tracker_request_invalid"))?;
    Ok(tracker)
}

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure(TRACKER_UNAVAILABLE)
}

fn immediate(db: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)
}

/// Runs one authenticated Tracker action as the owner against the engine database in the
/// engine root. Inputs: the engine root and the action. Output: the reply, or a code of
/// `TRACKER_CODES`.
fn run(root: &Path, action: Action) -> Result<Reply, EngineError> {
    let db = crate::questions::open(root).map_err(unavailable)?;
    match action {
        Action::Op { op, args } => operate(&db, &op, &args),
        Action::Projects {} => Ok(Reply::Projects {
            projects: tracked(&db)?,
        }),
        Action::Attach { project, key, name } => attach(&db, project, key, &name),
        Action::Settings { project } => Ok(Reply::Settings {
            settings: settings_view(&db, project)?,
        }),
        Action::SaveSettings {
            project,
            values,
            version,
        } => save_settings(&db, project, &values, version),
    }
}

/// Runs one schema v1 operation through the core's dispatch in one immediate transaction,
/// as the owner, and rewrites the project's rendered view after a write it accepted. A
/// request the core cannot parse is its refused envelope. Inputs: the database, the
/// operation name and its arguments. Output: the core's envelope.
fn operate(db: &Connection, op: &str, args: &serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(args).map_err(|_| failure("tracker_request_invalid"))?;
    let (reply, written) = match Request::parse(op, &bytes) {
        Err(refusal) => (CoreReply::refused(&refusal), None),
        Ok(request) => {
            let request = request.with_reply_bytes_max(TRACKER_REPLY_BYTES_MAX);
            let kind = request.kind();
            let tx = immediate(db)?;
            let reply = ops::dispatch(
                &tx,
                &engine_tracker::Host,
                &engine_tracker::owner(),
                request,
            );
            tx.commit().map_err(unavailable)?;
            let written = (kind.is_write() && reply.is_ok())
                .then(|| target_key(kind, args))
                .flatten();
            (reply, written)
        }
    };
    assert!(reply.json().len() <= TRACKER_REPLY_BYTES_MAX);
    if let Some(key) = written {
        render::refresh(db, &key);
    }
    let envelope: serde_json::Value =
        serde_json::from_str(reply.json()).map_err(|_| failure("tracker_reply_bounds"))?;
    assert!(envelope.is_object());
    Ok(Reply::Envelope { envelope })
}

/// The project a request names: its `project` field, or the project of the ticket its
/// first ticket field names. Inputs: the operation and its arguments. Output: the key, or
/// None for a request that names neither.
fn target_key(kind: OpKind, args: &serde_json::Value) -> Option<ProjectKey> {
    let spec = catalog::spec(kind);
    assert_eq!(spec.kind, kind);
    spec.fields.iter().find_map(|field| {
        let text = args.get(field.name)?.as_str()?;
        match field.ty {
            FieldType::Project => ProjectKey::new(text).ok(),
            FieldType::Ticket => ShortId::parse(text).ok().map(|id| id.key().clone()),
            _ => None,
        }
    })
}

/// Lists the projects that have tickets, by engine project, with the name the tracker
/// shows. Input: the database. Output: the projects, at most TRACKED_PROJECTS_MAX.
fn tracked(db: &Connection) -> Result<Vec<TrackedProject>, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(unavailable)?;
    let rows = tx
        .prepare(TRACKED)
        .and_then(|mut statement| {
            statement
                .query_map([TRACKED_PROJECTS_MAX as i64 + 1], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(unavailable)?;
    assert!(
        rows.len() <= TRACKED_PROJECTS_MAX,
        "attach bounds the tracked projects"
    );
    let mut projects = Vec::with_capacity(rows.len());
    for (project, key) in rows {
        let project = ProjectId::new(project).map_err(unavailable)?;
        let key = ProjectKey::new(&key).map_err(|refusal| failure(refusal.code))?;
        let name = store::project(&tx, &key)
            .map_err(engine_tracker::engine_error)?
            .name;
        projects.push(TrackedProject { project, key, name });
    }
    assert!(
        projects
            .windows(2)
            .all(|pair| pair[0].project < pair[1].project)
    );
    Ok(projects)
}

/// Gives a set-up engine project its tickets and writes its rendered view. Inputs: the
/// database, the project, the key and the name. Output: the tracked project, or
/// `tracker_project_missing`, `tracker_projects_full`, `project_exists` or the core's
/// refusal of the key or name.
fn attach(
    db: &Connection,
    project: ProjectId,
    key: ProjectKey,
    name: &str,
) -> Result<Reply, EngineError> {
    let tx = immediate(db)?;
    if crate::project_setup::load_setup(&tx, &project)
        .map_err(unavailable)?
        .is_none()
    {
        return Err(failure("tracker_project_missing"));
    }
    let count: i64 = tx
        .query_row(TRACKED_COUNT, [], |row| row.get(0))
        .map_err(unavailable)?;
    if count >= TRACKED_PROJECTS_MAX as i64 && engine_tracker::key_of(&tx, &project)?.is_none() {
        return Err(failure("tracker_projects_full"));
    }
    engine_tracker::attach(&tx, &project, &key, name, crate::team::now_ms())?;
    let name = store::project(&tx, &key)
        .map_err(engine_tracker::engine_error)?
        .name;
    tx.commit().map_err(unavailable)?;
    render::refresh(db, &key);
    Ok(Reply::Attached {
        project: TrackedProject { project, key, name },
    })
}

/// Reads one scope's ticket settings with what each unset key takes and every key's
/// choices. Inputs: the database and the scope (None for the app). Output: the view, or
/// `project_not_attached`.
fn settings_view(
    db: &Connection,
    project: Option<ProjectId>,
) -> Result<TicketSettingsView, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(unavailable)?;
    let app = settings::stored(&tx, &Scope::App)?
        .values
        .over(&TicketSettings::default());
    let (scope, below) = match &project {
        None => (Scope::App, TicketSettings::default()),
        Some(project) => {
            if engine_tracker::key_of(&tx, project)?.is_none() {
                return Err(failure(engine_tracker::host::PROJECT_NOT_ATTACHED));
            }
            (Scope::Project(project.clone()), app)
        }
    };
    let stored = settings::stored(&tx, &scope)?;
    let effective = stored.values.over(&below);
    if let Some(project) = &project {
        assert_eq!(settings::effective(&tx, project)?, effective);
    }
    Ok(TicketSettingsView {
        project,
        values: stored.values,
        version: stored.version,
        below,
        effective,
        choices: choices(),
    })
}

/// Saves one scope's ticket settings at the version the owner's screen read. Inputs: the
/// database, the scope, the values and the version. Output: saved or stale, or
/// `project_not_attached`.
fn save_settings(
    db: &Connection,
    project: Option<ProjectId>,
    values: &TicketOverride,
    version: u64,
) -> Result<Reply, EngineError> {
    let scope = project.map_or(Scope::App, Scope::Project);
    let tx = immediate(db)?;
    let saved = match settings::set(&tx, &scope, values, version, crate::team::now_ms())? {
        Ok(saved) => {
            assert!(saved > version);
            SettingsSaved::Saved { version: saved }
        }
        Err(stale) => {
            assert_ne!(stale.current, version);
            SettingsSaved::Stale {
                current: stale.current,
            }
        }
    };
    tx.commit().map_err(unavailable)?;
    Ok(Reply::Saved { saved })
}

fn named<T: Serialize>(all: &[T]) -> Vec<serde_json::Value> {
    all.iter()
        .map(|option| serde_json::to_value(option).expect("an option is a word"))
        .collect()
}

/// Every ticket setting's choices: the core policy keys in their order, then the engine's
/// `manual_evidence` and `ci_evidence`. `run_writers` lists only the value the host locks
/// it to, so it shows locked and no save can change it. Output: every setting, one per core
/// key and two.
fn choices() -> Vec<SettingChoices> {
    let proposed =
        serde_json::to_value(TicketSettings::default()).expect("settings are plain data");
    let mut all = Vec::with_capacity(PolicyKey::ALL.len() + 2);
    for key in PolicyKey::ALL {
        let options = match key {
            PolicyKey::Close => named(ClosePolicy::ALL),
            PolicyKey::Create => named(CreatePolicy::ALL),
            PolicyKey::AgentEdit => named(AgentEditPolicy::ALL),
            PolicyKey::CycleDays => named(&CycleDays::PRESETS),
            PolicyKey::Rollover => named(RolloverPolicy::ALL),
            PolicyKey::Import => named(ImportPolicy::ALL),
            PolicyKey::TestEvidence => named(TestEvidence::ALL),
            PolicyKey::SkippedCounts => named(SkippedCounts::ALL),
            PolicyKey::CaseProtectedVisibility => named(CaseProtectedVisibility::ALL),
            PolicyKey::RunWriters => named(&[RunWriters::EngineOnly]),
            PolicyKey::RunRetention => named(RunRetention::ALL),
        };
        all.push(SettingChoices {
            key: key.as_str().to_owned(),
            options,
            proposed: proposed["policy"][key.as_str()].clone(),
            custom: (*key == PolicyKey::CycleDays).then_some(CustomBounds {
                min: CYCLE_DAYS_CUSTOM.min,
                max: CYCLE_DAYS_CUSTOM.max,
            }),
        });
    }
    for (key, options) in [
        ("manual_evidence", named(&ManualEvidence::ALL)),
        ("ci_evidence", named(&CiEvidence::ALL)),
    ] {
        all.push(SettingChoices {
            key: key.to_owned(),
            options,
            proposed: proposed[key].clone(),
            custom: None,
        });
    }
    assert_eq!(all.len(), PolicyKey::ALL.len() + 2);
    assert_eq!(
        crate::tracker::host::locked(PolicyKey::RunWriters),
        Some(RunWriters::EngineOnly.as_str())
    );
    assert!(
        all.iter()
            .all(|choice| choice.options.contains(&choice.proposed))
    );
    all
}

#[cfg(all(test, unix))]
#[path = "tracker_tests.rs"]
mod tests;
