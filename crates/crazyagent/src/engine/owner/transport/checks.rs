use super::tracker::{SettingChoices, SettingsSaved};
use super::*;
#[cfg(unix)]
use crate::checks::gate;
use crate::checks::profile::{HOLDOUT_FILE_BYTES_MAX, PROFILE_INPUT_BYTES_MAX};
use crate::checks::queue::{self, JobState};
use crate::checks::settings::{GateMode, RemovedTests};
use crate::checks::{
    self as engine_checks, Actor, AdmittedProfile, ChecksChoice, ChecksSettings, ProfileId,
    ProfilePage, Proposal, Scope, StoredProfile, Trigger, record, settings,
};
use crate::landing::{self, ChangeSetId};
use crate::protocol::encode_frame;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use trackwright_core::runs::{RunState, TestId, Verdict};

const REQUEST_BYTES_MAX: usize = 48 * 1024;
pub(super) const CHECKS_TIMEOUT: Duration = Duration::from_secs(30);
/// The most jobs a `runs` reply lists for one change set, newest first.
pub const RUNS_JOBS_MAX: usize = 64;
/// The most checks a `runs` reply shows the newest run of, in check order.
pub const RUNS_LATEST_MAX: usize = 48;
/// The most gaps a `gate` reply lists: one per required check and holdout, one more each
/// for the skips and removed tests of a passing one, and the change's own two.
pub const GATE_GAPS_MAX: usize =
    2 * (crate::checks::profile::CHECKS_MAX + crate::checks::profile::HOLDOUTS_MAX) + 2;
/// The most paths or test names one gap of a `gate` reply names; `paths_total` counts all.
pub const GATE_PATHS_SHOWN: usize = 2;

const _: () = assert!(PROFILE_INPUT_BYTES_MAX + 4096 <= REQUEST_BYTES_MAX);
const _: () = assert!(HOLDOUT_FILE_BYTES_MAX.div_ceil(3) * 4 + 1024 <= REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(RUNS_JOBS_MAX <= crate::checks::queue::JOBS_LISTED_MAX);
const _: () = assert!(
    RUNS_LATEST_MAX >= crate::checks::profile::CHECKS_MAX + crate::checks::profile::HOLDOUTS_MAX
);
const _: () =
    assert!(RUNS_JOBS_MAX * 512 + RUNS_LATEST_MAX * 640 <= crate::protocol::FRAME_BYTES_MAX);
#[cfg(unix)]
const _: () = assert!(GATE_PATHS_SHOWN <= gate::PATHS_MAX);
#[cfg(unix)]
const _: () = assert!(
    GATE_GAPS_MAX * (2 * GATE_PATHS_SHOWN * crate::checks::integrity::TEXT_BYTES_MAX + 384) + 1024
        <= crate::protocol::FRAME_BYTES_MAX
);

/// Every code a Checks request can answer an owner client with: the checks store's, queue's
/// and record's refusals and job reasons, the gate's, the owner questions a proposal raises,
/// this protocol's own, and the owner connection's it travels on.
pub const CHECKS_CODES: &[&str] = &[
    "changes_not_found",
    "changes_stale",
    "changes_unavailable",
    "checks_batch_unknown",
    "checks_change_set_accepted",
    "checks_change_set_discarded",
    "checks_change_set_moved",
    "checks_change_set_unknown",
    "checks_check_unknown",
    "checks_command_missing",
    "checks_cwd_missing",
    "checks_engine_stopped",
    "checks_failed",
    "checks_holdout_collides",
    "checks_holdout_invalid",
    "checks_holdout_missing",
    "checks_holdouts_full",
    "checks_new_skips",
    "checks_no_profile",
    "checks_not_on_request",
    "checks_not_run",
    "checks_nothing_to_run",
    "checks_outcome_unknown",
    "checks_owner_only",
    "checks_profile_invalid",
    "checks_profile_revoked",
    "checks_profile_unknown",
    "checks_profiles_full",
    "checks_project_missing",
    "checks_protected_change",
    "checks_queue_full",
    "checks_rate_limited",
    "checks_record_invalid",
    "checks_reply_bounds",
    "checks_request_bounds",
    "checks_request_invalid",
    "checks_requests_off",
    "checks_run_exists",
    "checks_run_invalid",
    "checks_run_not_found",
    "checks_sandbox_unavailable",
    "checks_setting_invalid",
    "checks_settings_stale",
    "checks_snapshot_failed",
    "checks_snapshot_link",
    "checks_snapshot_spent",
    "checks_snapshot_store_invalid",
    "checks_snapshot_too_large",
    "checks_stale",
    "checks_tests_removed",
    "checks_unavailable",
    "checks_unavailable_off_macos",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
    "questions_answer_invalid",
    "questions_ask_invalid",
    "questions_asker_full",
    "questions_delivery_full",
    "questions_full",
    "questions_in_terminal",
    "questions_not_found",
    "questions_not_terminal",
    "questions_notes_invalid",
    "questions_option_invalid",
    "questions_outcome_unknown",
    "questions_owner_turn_invalid",
    "questions_page_unavailable",
    "questions_project_missing",
    "questions_reply_bounds",
    "questions_request_bounds",
    "questions_request_invalid",
    "questions_text_invalid",
    "questions_unavailable",
    "versioned_item_invalid",
    "versioned_unavailable",
];

/// One owner Checks request, sent as `{"checks": {"op": <op>, "args": {...}}}`. Every op
/// names its project; `run`, `cancel`, `runs` and `gate` also name a change set of that
/// project.
/// A profile travels as its JSON text and a holdout file as base64; none of the owner's
/// replies carries holdout bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ChecksRequest {
    ProfilePropose {
        project: ProjectId,
        profile: String,
    },
    Profiles {
        project: ProjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
    },
    ProfileShow {
        project: ProjectId,
        id: ProfileId,
    },
    ProfileApprove {
        project: ProjectId,
        id: ProfileId,
    },
    ProfileRevoke {
        project: ProjectId,
        id: ProfileId,
    },
    HoldoutStore {
        project: ProjectId,
        base64: String,
    },
    SettingsShow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
    },
    SettingsSave {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        values: ChecksChoice,
        version: u64,
    },
    Run {
        project: ProjectId,
        change_set: ChangeSetId,
        #[serde(default)]
        checks: Vec<String>,
    },
    Cancel {
        project: ProjectId,
        change_set: ChangeSetId,
    },
    Runs {
        project: ProjectId,
        change_set: ChangeSetId,
    },
    Gate {
        project: ProjectId,
        change_set: ChangeSetId,
    },
}

/// What a checks settings screen shows for one scope (the app, or one project): the
/// project (None for the app), the keys this scope sets, the version a save sends back,
/// what each unset key takes (the app layer for a project, the proposal for the app), the
/// values in force, and every key's options with its proposed default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecksSettingsView {
    pub project: Option<ProjectId>,
    pub values: ChecksChoice,
    pub version: u64,
    pub below: ChecksSettings,
    pub effective: ChecksSettings,
    pub choices: Vec<SettingChoices>,
}

/// One queued, running, recorded or cancelled check of a change set as the owner sees it:
/// its batch, check id, trigger, who asked (`owner`, `master:<id>` or `agent:<id>`), where
/// it stands, its `tk_runs` row once recorded, the code it was cancelled with and when it
/// was queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckJobView {
    pub batch: String,
    pub check: String,
    pub trigger: Trigger,
    pub actor: String,
    pub state: JobState,
    pub run: Option<i64>,
    pub reason: Option<String>,
    pub queued_ms: i64,
}

/// The newest recorded run of one check on a change set: the check, the trackwright run id
/// `run_show` takes, its batch, state and verdict, the commit and tree it covered, the
/// profile and approval it ran under, how many protected paths the change set touched,
/// whether it was a holdout run and when it finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRunView {
    pub check: String,
    pub run_id: TestId,
    pub batch: String,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub commit: String,
    pub tree: String,
    pub profile: ProfileId,
    pub profile_approved_ms: i64,
    pub protected_touched: u32,
    pub holdout: bool,
    pub finished_ms: Option<i64>,
}

/// One gap the gate finds at a change set's head as the owner sees it: its code, the check
/// it is about (None for the change or the project as a whole), the run it read as
/// `<KEY>-R<n>`, at most `GATE_PATHS_SHOWN` of its paths or test names and how many there
/// are in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckGapView {
    pub code: String,
    pub check: Option<String>,
    pub run: Option<String>,
    pub paths: Vec<String>,
    pub paths_total: u32,
}

/// What Accept would meet at a change set's head now, read without keeping a note: the gate
/// and removed tests settings in effect, the head, the current profile, the code Accept
/// would be refused with (None when it may proceed), at most `GATE_GAPS_MAX` gaps and how
/// many there are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckGateView {
    pub gate: GateMode,
    pub removed_tests: RemovedTests,
    pub head: String,
    pub profile: Option<ProfileId>,
    pub refusal: Option<String>,
    pub gaps: Vec<CheckGapView>,
    pub gaps_total: u32,
}

/// The engine's answer to one Checks request, one kind per op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChecksReply {
    Proposed {
        proposal: Proposal,
    },
    Profiles {
        page: ProfilePage,
    },
    Profile {
        profile: StoredProfile,
    },
    Approved {
        profile: AdmittedProfile,
    },
    Revoked {},
    HoldoutStored {
        digest: String,
    },
    Settings {
        settings: ChecksSettingsView,
    },
    Saved {
        saved: SettingsSaved,
    },
    Queued {
        batch: String,
    },
    Cancelled {
        jobs: u32,
    },
    Runs {
        jobs: Vec<CheckJobView>,
        latest: Vec<CheckRunView>,
    },
    Gate {
        gate: CheckGateView,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    checks: ChecksRequest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Done { reply: ChecksReply },
    Rejected { code: String },
}

fn unknown() -> EngineError {
    failure("checks_outcome_unknown")
}

impl OwnerClient {
    /// Sends one Checks request as the owner. Input: the request. Output: the engine's
    /// reply, or the refusal code, one of `CHECKS_CODES`.
    pub async fn checks(&self, request: ChecksRequest) -> Result<ChecksReply, EngineError> {
        tokio::time::timeout(self.timeout.max(CHECKS_TIMEOUT), async {
            let frame = encode_frame(&Envelope { checks: request })
                .map_err(|_| failure("checks_request_bounds"))?;
            if frame.len() > REQUEST_BYTES_MAX + 4 {
                return Err(failure("checks_request_bounds"));
            }
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("checks_unavailable"))?;
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
                .map_err(|_| failure("checks_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("checks_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Done { reply } => Ok(reply),
                Reply::Rejected { code } => Err(EngineError(code)),
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
        Ok(reply) => Reply::Done { reply },
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "checks_reply_bounds".into(),
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
) -> Result<ChecksReply, EngineError> {
    let request = parse(bytes)?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    tokio::task::spawn_blocking(move || run(&root, request))
        .await
        .map_err(|_| failure("checks_unavailable"))?
}

/// Reads one Checks request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the request, or `checks_request_bounds` or
/// `checks_request_invalid`.
fn parse(bytes: &[u8]) -> Result<ChecksRequest, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("checks_request_bounds"));
    }
    let Envelope { checks } =
        serde_json::from_slice(bytes).map_err(|_| failure("checks_request_invalid"))?;
    Ok(checks)
}

/// Runs one authenticated Checks request as the owner against the engine database in the
/// engine root; a queued run starts the project's runners. Inputs: the engine root and the
/// request. Output: the reply, or the store's, queue's or record's refusal.
fn run(root: &Path, request: ChecksRequest) -> Result<ChecksReply, EngineError> {
    let db = landing::open(root)?;
    let owner = Actor::Owner;
    Ok(match request {
        ChecksRequest::ProfilePropose { project, profile } => ChecksReply::Proposed {
            proposal: engine_checks::propose(&db, &project, profile.as_bytes(), owner)?,
        },
        ChecksRequest::Profiles { project, after } => ChecksReply::Profiles {
            page: engine_checks::profiles(&db, &project, after)?,
        },
        ChecksRequest::ProfileShow { project, id } => ChecksReply::Profile {
            profile: engine_checks::show(&db, &project, &id)?
                .ok_or_else(|| failure("checks_profile_unknown"))?,
        },
        ChecksRequest::ProfileApprove { project, id } => ChecksReply::Approved {
            profile: engine_checks::approve(&db, &project, &id, owner)?,
        },
        ChecksRequest::ProfileRevoke { project, id } => {
            engine_checks::revoke(&db, &project, &id, owner)?;
            ChecksReply::Revoked {}
        }
        ChecksRequest::HoldoutStore { project, base64 } => {
            let bytes = STANDARD
                .decode(base64.as_bytes())
                .map_err(|_| failure("checks_holdout_invalid"))?;
            ChecksReply::HoldoutStored {
                digest: engine_checks::store_holdout_file(&db, &project, &bytes, owner)?,
            }
        }
        ChecksRequest::SettingsShow { project } => ChecksReply::Settings {
            settings: settings_view(&db, project)?,
        },
        ChecksRequest::SettingsSave {
            project,
            values,
            version,
        } => ChecksReply::Saved {
            saved: save_settings(&db, project, &values, version)?,
        },
        ChecksRequest::Run {
            project,
            change_set,
            checks,
        } => {
            of_project(&db, &project, &change_set)?;
            let batch = queue::submit(&db, &change_set, Trigger::OnRequest, owner, &checks)?;
            queue::start(&db, &change_set);
            ChecksReply::Queued {
                batch: batch.as_str().to_owned(),
            }
        }
        ChecksRequest::Cancel {
            project,
            change_set,
        } => {
            of_project(&db, &project, &change_set)?;
            ChecksReply::Cancelled {
                jobs: queue::cancel(&db, &change_set)?,
            }
        }
        ChecksRequest::Runs {
            project,
            change_set,
        } => {
            of_project(&db, &project, &change_set)?;
            runs_view(&db, &change_set)?
        }
        ChecksRequest::Gate {
            project,
            change_set,
        } => {
            let head = of_project(&db, &project, &change_set)?;
            ChecksReply::Gate {
                gate: gate_view(&db, &change_set, &head)?,
            }
        }
    })
}

/// Refuses a change set that is not the project's, so a request reaches a change set only
/// under its own project. Inputs: the database, the project and the change set. Output: the
/// change set's head, or `checks_change_set_unknown`.
fn of_project(
    db: &Connection,
    project: &ProjectId,
    change_set: &ChangeSetId,
) -> Result<String, EngineError> {
    match landing::change_set(db, change_set)? {
        Some((found, view)) if &found == project => Ok(view.head),
        _ => Err(failure("checks_change_set_unknown")),
    }
}

/// Every checks settings key as a settings screen lists it, in TI-4's shape: no key takes a
/// custom number. Output: the keys.
fn choices() -> Vec<SettingChoices> {
    settings::choices()
        .into_iter()
        .map(|choice| SettingChoices {
            key: choice.key.to_owned(),
            options: choice.options,
            proposed: choice.default,
            custom: None,
        })
        .collect()
}

/// What a checks settings screen shows for one scope, read in one transaction: the app
/// layer over the proposal, or a project's override over the app layer. Inputs: the
/// database and the project, none for the app. Output: the view, or
/// `checks_project_missing` or `checks_unavailable`.
fn settings_view(
    db: &Connection,
    project: Option<ProjectId>,
) -> Result<ChecksSettingsView, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred)
        .map_err(|_| failure("checks_unavailable"))?;
    let (app, app_version) = settings::read(&tx, &Scope::App)?;
    let none = ChecksChoice::default();
    let (values, version, below, effective) = match &project {
        None => (
            app,
            app_version,
            ChecksSettings::default(),
            ChecksSettings::resolve(&app, &none),
        ),
        Some(project) => {
            engine_checks::project_known(&tx, project)?;
            let (own, version) = settings::read(&tx, &Scope::Project(project.clone()))?;
            let effective = ChecksSettings::resolve(&app, &own);
            assert_eq!(settings::effective(&tx, project)?, effective);
            (
                own,
                version,
                ChecksSettings::resolve(&app, &none),
                effective,
            )
        }
    };
    Ok(ChecksSettingsView {
        project,
        values,
        version,
        below,
        effective,
        choices: choices(),
    })
}

/// Saves one scope's checks settings at the version the owner's screen read. Inputs: the
/// database, the project (none for the app), the values and the version. Output: saved at
/// the new version, or stale with the scope's current version, or `checks_setting_invalid`,
/// `checks_project_missing` or `checks_unavailable`.
fn save_settings(
    db: &Connection,
    project: Option<ProjectId>,
    values: &ChecksChoice,
    version: u64,
) -> Result<SettingsSaved, EngineError> {
    let scope = project.map_or(Scope::App, Scope::Project);
    match settings::write(db, &scope, values, version, Actor::Owner) {
        Ok(saved) => {
            assert!(saved > version);
            Ok(SettingsSaved::Saved { version: saved })
        }
        Err(refused) if refused.0 == "checks_settings_stale" => {
            let (_, current) = settings::read(db, &scope)?;
            Ok(SettingsSaved::Stale { current })
        }
        Err(refused) => Err(refused),
    }
}

/// A change set's jobs, newest first and at most `RUNS_JOBS_MAX`, with the newest run of
/// each check, at most `RUNS_LATEST_MAX`. Inputs: the database and the change set. Output:
/// the reply, or `checks_unavailable`.
fn runs_view(db: &Connection, change_set: &ChangeSetId) -> Result<ChecksReply, EngineError> {
    let jobs: Vec<CheckJobView> = queue::jobs(db, change_set)?
        .into_iter()
        .take(RUNS_JOBS_MAX)
        .map(|job| CheckJobView {
            batch: job.batch.as_str().to_owned(),
            check: job.check,
            trigger: job.trigger,
            actor: job.actor.name(),
            state: job.state,
            run: job.run,
            reason: job.reason,
            queued_ms: job.queued_ms,
        })
        .collect();
    let latest = record::latest(db, change_set)?
        .into_iter()
        .take(RUNS_LATEST_MAX)
        .map(|run| {
            Ok(CheckRunView {
                check: run.check,
                run_id: run.id,
                batch: run.batch.as_str().to_owned(),
                state: run.state,
                verdict: run.verdict,
                commit: run.commit,
                tree: run.tree,
                profile: run.profile,
                profile_approved_ms: run.profile_approved_ms,
                protected_touched: u32::try_from(run.protected_touched.len())
                    .map_err(|_| failure("checks_unavailable"))?,
                holdout: run.holdout,
                finished_ms: run.finished_ms,
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    assert!(jobs.len() <= RUNS_JOBS_MAX && latest.len() <= RUNS_LATEST_MAX);
    Ok(ChecksReply::Runs { jobs, latest })
}

/// What Accept would meet at a change set's head, read through the gate without keeping a
/// note, with at most `GATE_GAPS_MAX` gaps of at most `GATE_PATHS_SHOWN` paths each.
/// Inputs: the database, the change set and its head. Output: the view, or
/// `changes_not_found`, `changes_stale` or `checks_unavailable`.
#[cfg(unix)]
fn gate_view(
    db: &Connection,
    change_set: &ChangeSetId,
    head: &str,
) -> Result<CheckGateView, EngineError> {
    let note = gate::assess(db, change_set, head)?;
    assert_eq!(note.head, head);
    let refusal = gate::refusal(&note).map(str::to_owned);
    let gaps_total = u32::try_from(note.gaps.len()).map_err(|_| failure("checks_unavailable"))?;
    let gaps: Vec<CheckGapView> = note
        .gaps
        .into_iter()
        .take(GATE_GAPS_MAX)
        .map(|gap| CheckGapView {
            code: gap.code,
            check: gap.check,
            run: gap.run,
            paths: gap.paths.into_iter().take(GATE_PATHS_SHOWN).collect(),
            paths_total: gap.paths_total,
        })
        .collect();
    assert!(gaps.len() <= GATE_GAPS_MAX);
    assert!(gaps.iter().all(|gap| gap.paths.len() <= GATE_PATHS_SHOWN));
    Ok(CheckGateView {
        gate: note.gate,
        removed_tests: note.removed_tests,
        head: note.head,
        profile: note.profile,
        refusal,
        gaps,
        gaps_total,
    })
}

/// The gate reads integrity blocks only on unix, so off unix no gate view exists. Inputs:
/// unused. Output: `checks_unavailable`.
#[cfg(not(unix))]
fn gate_view(_: &Connection, _: &ChangeSetId, _: &str) -> Result<CheckGateView, EngineError> {
    Err(failure("checks_unavailable"))
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
