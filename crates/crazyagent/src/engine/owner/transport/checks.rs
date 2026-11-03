use super::tracker::{SettingChoices, SettingsSaved};
use super::*;
use crate::checks::profile::{HOLDOUT_FILE_BYTES_MAX, PROFILE_INPUT_BYTES_MAX};
use crate::checks::queue::JobState;
use crate::checks::{
    AdmittedProfile, ChecksChoice, ChecksSettings, ProfileId, ProfilePage, Proposal, StoredProfile,
    Trigger,
};
use crate::landing::ChangeSetId;
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};
use trackwright_core::runs::{RunState, TestId, Verdict};

const REQUEST_BYTES_MAX: usize = 48 * 1024;
pub(super) const CHECKS_TIMEOUT: Duration = Duration::from_secs(30);
/// The most jobs a `runs` reply lists for one change set, newest first.
pub const RUNS_JOBS_MAX: usize = 64;

const _: () = assert!(PROFILE_INPUT_BYTES_MAX + 4096 <= REQUEST_BYTES_MAX);
const _: () = assert!(HOLDOUT_FILE_BYTES_MAX.div_ceil(3) * 4 + 1024 <= REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(RUNS_JOBS_MAX <= crate::checks::queue::JOBS_LISTED_MAX);

/// Every code a Checks request can answer an owner client with: the checks store's, queue's
/// and record's refusals and job reasons, the gate's, the owner questions a proposal raises,
/// this protocol's own, and the owner connection's it travels on.
pub const CHECKS_CODES: &[&str] = &[
    "changes_not_found",
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
/// names its project; `run`, `cancel` and `runs` also name a change set of that project.
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
        todo!()
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
    todo!()
}

/// Reads one Checks request from an owner frame body, at most REQUEST_BYTES_MAX bytes.
/// Input: the body. Output: the request, or `checks_request_bounds` or
/// `checks_request_invalid`.
fn parse(bytes: &[u8]) -> Result<ChecksRequest, EngineError> {
    todo!()
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
