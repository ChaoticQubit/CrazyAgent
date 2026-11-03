use super::*;
use crate::protocol::encode_frame;
use crate::tracker::settings::{TicketOverride, TicketSettings};
use serde::{Deserialize, Serialize};
use trackwright_core::model::{OpKind, ProjectKey};

const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub(super) const TRACKER_TIMEOUT: Duration = Duration::from_secs(30);

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
    "actor_invalid",
    "actor_not_engine",
    "actor_other_project",
    "agent_edit_refused",
    "args_invalid",
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
    "cursor_invalid",
    "cycle_days_invalid",
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
    "notify_too_wide",
    "op_unavailable",
    "open_tickets_full",
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
    "peer_owner_mismatch",
    "policy_in_settings",
    "policy_value_invalid",
    "priority_invalid",
    "project_exists",
    "project_key_invalid",
    "project_not_attached",
    "project_unknown",
    "rate_limited",
    "request_id_reused",
    "request_too_large",
    "short_id_invalid",
    "stale_version",
    "status_closed_at_create",
    "status_invalid",
    "store_failed",
    "store_not_private",
    "store_path_refused",
    "store_schema_newer",
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
    "tracker_reply_bounds",
    "tracker_request_bounds",
    "tracker_request_invalid",
    "tracker_unavailable",
    "unknown_field",
    "unknown_op",
    "use_close",
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
