use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const FRAME_BYTES_MAX: usize = 65536;
/// The most owner and runner requests the engine serves at once in its exchange, and the
/// most connections it reads a handshake from at once. Clients that run requests side by
/// side, such as the desktop's plain reads, size themselves under it. Each holds at most
/// one `FRAME_BYTES_MAX` frame.
pub const OWNER_CLIENTS_MAX: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub String);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// True when the text is exactly one plain path component that names one child, never
/// itself, its parent or another root: not empty, `.`, `..`, a separator, a root or a
/// Windows drive like `c:`, no trailing dot or space, which Windows strips, and no Windows
/// device name. A colon stays allowed, for identities and as a Unix file name; on Windows
/// `name:x` is a stream of `name`, so a Windows folder name needs none. Input: the text.
/// Output: whether it names one child.
pub(crate) fn single_component(text: &str) -> bool {
    let mut parts = std::path::Path::new(text).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(part)), None) if part == std::ffi::OsStr::new(text)
    ) && !text.ends_with(['.', ' '])
        && !windows_device(text)
}

/// True when Windows opens the name as a device instead of a file: CON, PRN, AUX, NUL,
/// COM1 to COM9 or LPT1 to LPT9 in any case, alone, before an extension or before one
/// trailing colon, with or without spaces before the dot. Input: one path component.
/// Output: whether it is reserved on Windows.
pub(crate) fn windows_device(name: &str) -> bool {
    let name = name.strip_suffix(':').unwrap_or(name);
    let stem = name
        .split_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end_matches(' ');
    WINDOWS_DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

fn validate_identity(value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        || value.contains("..")
        || !single_component(value)
    {
        return Err(ProtocolError(
            "identity must be 1..128 safe ASCII bytes".into(),
        ));
    }
    assert!(!value.is_empty());
    assert!(value.len() <= 128);
    Ok(())
}

macro_rules! identity {
    ($($name:ident),+) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
                let value = value.into();
                validate_identity(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str { &self.0 }
        }

        impl TryFrom<String> for $name {
            type Error = ProtocolError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::new(value) }
        }
    )+};
}

identity!(
    ProjectId,
    SessionId,
    TaskId,
    PlanId,
    ConfigId,
    SnapshotId,
    BaselineId,
    EnvironmentId,
    ClientId,
    RequestId,
    ControllerEpoch,
    CredentialRef
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cursor(u64);

impl Cursor {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn value(&self) -> u64 {
        self.0
    }
}

impl TryFrom<String> for Cursor {
    type Error = ProtocolError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parsed: u64 = value
            .parse()
            .map_err(|_| ProtocolError("invalid cursor".into()))?;
        if parsed.to_string() != value {
            return Err(ProtocolError("noncanonical cursor".into()));
        }
        Ok(Self(parsed))
    }
}

impl From<Cursor> for String {
    fn from(value: Cursor) -> Self {
        value.0.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    pub name: String,
    pub repositories: Vec<String>,
}

impl ProjectSpec {
    /// Validates a project name and explicit repository declarations. A name may not hold
    /// controls or characters that hide or reorder what the owner reads (bidi marks, zero-width,
    /// tag characters), the set `memory::hidden_text` finds; an emoji joiner stays allowed.
    /// Takes an untrusted specification without accessing its paths.
    /// Returns the existing validation error and preserves empty-list compatibility.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || crate::memory::hidden_text(&self.name)
            || self.repositories.len() > 64
            || self.repositories.iter().any(|p| {
                p.is_empty()
                    || p.len() > 4096
                    || p.chars().any(char::is_control)
                    || p.contains("://")
            })
        {
            return Err(ProtocolError("invalid project specification".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContext {
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub config_id: ConfigId,
    pub snapshot_id: SnapshotId,
    pub baseline_id: BaselineId,
    pub allowed_destinations: Vec<CredentialRef>,
    pub budget_ref: CredentialRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Command {
    #[serde(skip_deserializing)]
    OwnerModelAccess {
        expected_config_id: ConfigId,
        expected_source_hash: String,
        edit: crate::config::ModelAccessEdit,
    },
    #[serde(skip_deserializing)]
    OwnerConversation {
        mutation: crate::conversation::ConversationMutation,
    },
    #[serde(skip_deserializing)]
    OwnerProfile {
        profile: crate::owner_profile::OwnerProfile,
    },
    #[serde(skip_deserializing)]
    OwnerSandbox {
        settings: crate::sandbox_settings::SandboxSettings,
    },
    #[serde(skip_deserializing)]
    OwnerAgentCleanup {
        settings: crate::cleanup_settings::CleanupSettings,
    },
    #[serde(skip_deserializing)]
    OwnerAgentQuit {
        settings: crate::quit_settings::QuitSettings,
    },
    #[serde(skip_deserializing)]
    OwnerMasterSteps {
        settings: crate::master_step_limit::MasterStepSettings,
    },
    #[serde(skip_deserializing)]
    OwnerMasterTime {
        settings: crate::master_time_limit::MasterTimeSettings,
    },
    #[serde(skip_deserializing)]
    OwnerMemory {
        action: crate::memory::library::LibraryAction,
    },
    #[serde(skip_deserializing)]
    ProposeBaseline {
        draft: crate::verification::BaselineDraft,
    },
    #[serde(skip_deserializing)]
    OwnerVerification {
        action: crate::verification::OwnerVerification,
    },
    #[serde(skip_deserializing)]
    Knowledge {
        action: crate::memory::KnowledgeAction,
    },
    #[serde(skip_deserializing)]
    Assets {
        action: crate::assets::AssetAction,
    },
    #[serde(skip_deserializing)]
    ProjectSetup {
        action: crate::project_setup::SetupAction,
    },
    #[serde(skip_deserializing)]
    ProjectsHome {
        action: crate::projects_home::HomeAction,
    },
    #[serde(skip_deserializing)]
    OwnerSetup {
        action: crate::setup::SetupAction,
    },
    #[serde(skip_deserializing)]
    Onboarding {
        action: crate::onboarding::OnboardingAction,
    },
    #[serde(skip_deserializing)]
    Team {
        action: crate::team::TeamAction,
    },
    #[serde(skip_deserializing)]
    AccountSchedule {
        change: crate::scheduling::ScheduleChange,
        version: u64,
    },
    ProposePlan {
        plan: crate::coordination::WorkPlan,
    },
    CoordinationState {
        plan_id: PlanId,
    },
    TaskPlan {
        task_id: TaskId,
    },
    #[serde(skip_deserializing)]
    OwnerCoordination {
        action: crate::coordination::OwnerAction,
    },
    RouteWorkItem {
        plan_id: PlanId,
        item_id: TaskId,
    },
    Mailbox {
        plan_id: PlanId,
        recipient: SessionId,
        after: Cursor,
    },
    CreateProject {
        project_id: ProjectId,
        spec: ProjectSpec,
    },
    Snapshot,
    Events {
        after: Cursor,
    },
    PinTask {
        context: ExecutionContext,
    },
    MigrateTask {
        task_id: TaskId,
        config_id: ConfigId,
    },
    TaskContext {
        task_id: TaskId,
    },
    TaskConfig {
        task_id: TaskId,
    },
    AdmitRepository {
        project_id: ProjectId,
        repository: String,
    },
    AllocateWorktree {
        task_id: TaskId,
        repository: String,
        owner_session: SessionId,
        native_descendant: Option<String>,
    },
    WorktreeLeases {
        task_id: TaskId,
    },
    ReleaseWorktree {
        task_id: TaskId,
        repository: String,
        owner_session: SessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRequest {
    pub protocol_version: u16,
    pub client_id: ClientId,
    pub request_id: RequestId,
    pub expected_revision: Cursor,
    pub command: Command,
}

impl WireRequest {
    /// Validates supported commands, draft content and wire bounds.
    /// Takes an untrusted request and returns structural validation.
    /// Host-only authority variants remain unavailable to wire decoding.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError("unsupported protocol version".into()));
        }
        match &self.command {
            Command::OwnerConversation { mutation } => {
                mutation
                    .validate()
                    .map_err(|e| ProtocolError(e.to_string()))?;
            }
            Command::ProposeBaseline { draft } => {
                draft.validate().map_err(|e| ProtocolError(e.to_string()))?;
            }
            Command::CreateProject { spec, .. } => {
                spec.validate()?;
            }
            Command::PinTask { context } if context.allowed_destinations.len() > 64 => {
                return Err(ProtocolError("too many destinations".into()));
            }
            Command::AdmitRepository { repository, .. }
            | Command::AllocateWorktree { repository, .. }
            | Command::ReleaseWorktree { repository, .. } => {
                if repository.is_empty()
                    || repository.len() > 4096
                    || repository.chars().any(char::is_control)
                    || !std::path::Path::new(repository).is_absolute()
                {
                    return Err(ProtocolError("invalid repository path".into()));
                }
                if let Command::AllocateWorktree {
                    native_descendant: Some(native),
                    ..
                } = &self.command
                {
                    validate_identity(native)?;
                }
            }
            _ => {}
        }
        let bytes = serde_json::to_vec(self).map_err(|e| ProtocolError(e.to_string()))?;
        if bytes.len() > FRAME_BYTES_MAX {
            return Err(ProtocolError("oversized request".into()));
        }
        assert_eq!(self.protocol_version, PROTOCOL_VERSION);
        assert!(bytes.len() <= FRAME_BYTES_MAX);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredProject {
    pub project_id: ProjectId,
    pub spec: ProjectSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub cursor: Cursor,
    pub request_id: RequestId,
    pub client_id: ClientId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Outcome {
    Verification {
        state: Box<crate::verification::VerificationState>,
    },
    Nominations {
        nominations: Vec<crate::memory::Nomination>,
    },
    GlobalCandidates {
        candidates: Vec<crate::memory::GlobalCandidate>,
    },
    Knowledge {
        entries: Vec<crate::memory::KnowledgeEntry>,
    },
    Extraction {
        batch: crate::memory::ExtractionBatch,
    },
    Assets {
        snapshot: Box<crate::assets::AssetSnapshot>,
    },
    Coordination {
        state: Box<crate::coordination::PlanState>,
    },
    Messages {
        messages: Vec<crate::coordination::DeliveredMessage>,
    },
    Route {
        decision: crate::routing::RouteDecision,
    },
    Applied,
    Snapshot {
        projects: Vec<StoredProject>,
        tasks: Vec<ExecutionContext>,
    },
    Events {
        events: Vec<Event>,
    },
    ResyncRequired,
    Rejected {
        code: String,
    },
    Task {
        context: ExecutionContext,
    },
    Configuration {
        config: Box<crate::config::EffectiveConfig>,
    },
    Worktrees {
        leases: Vec<crate::worktrees::WorktreeLease>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireReply {
    pub request_id: RequestId,
    pub revision: Cursor,
    pub outcome: Outcome,
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|e| ProtocolError(e.to_string()))?;
    if bytes.is_empty() || bytes.len() > FRAME_BYTES_MAX {
        return Err(ProtocolError("invalid frame length".into()));
    }
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    assert!(frame.len() <= FRAME_BYTES_MAX + 4);
    assert!(frame.len() > 4);
    Ok(frame)
}

pub fn decode_request(frame: &[u8]) -> Result<WireRequest, ProtocolError> {
    if frame.len() < 4 || frame.len() > FRAME_BYTES_MAX + 4 {
        return Err(ProtocolError("invalid frame length".into()));
    }
    let size = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    if size == 0 || size != frame.len() - 4 {
        return Err(ProtocolError("invalid frame length".into()));
    }
    let request: WireRequest =
        serde_json::from_slice(&frame[4..]).map_err(|e| ProtocolError(e.to_string()))?;
    if request.client_id.as_str().starts_with("owner:") {
        return Err(ProtocolError("reserved_owner_identity".into()));
    }
    request.validate()?;
    assert_eq!(size, frame.len() - 4);
    assert!(size <= FRAME_BYTES_MAX);
    Ok(request)
}

/// The code an owner window may show for a failure: the code itself when it is 1 to 96
/// lowercase ASCII letters, digits or underscores, else [`TEXT_WITHHELD`], so error text,
/// paths or provider output never reach a window. Input: the raw failure code. Output: the
/// code to show.
pub fn plain_code(raw: &str) -> &str {
    let plain = (1..=96).contains(&raw.len())
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if plain { raw } else { TEXT_WITHHELD }
}

/// The fixed code shown in place of a failure whose code is not plain.
pub const TEXT_WITHHELD: &str = "engine_text_withheld";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_codes_reach_an_owner_window() {
        for code in [
            "claude_host_deadline",
            "owner_busy",
            "a",
            "x9_",
            &"a".repeat(96),
        ] {
            assert_eq!(plain_code(code), code);
        }
        for raw in [
            "",
            "No approved account in the assigned pool",
            "unclassified-native-host-error",
            "master_runtime_cleanup_uncertain:codex_host_kill_uncertain",
            "open /Users/mira/.config/token: permission denied",
            "Owner_Busy",
            "caf\u{e9}_code",
            &"a".repeat(97),
        ] {
            assert_eq!(plain_code(raw), TEXT_WITHHELD, "{raw}");
        }
        assert_eq!(plain_code(TEXT_WITHHELD), TEXT_WITHHELD);
    }

    pub fn request() -> WireRequest {
        WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("desktop").unwrap(),
            request_id: RequestId::new("operation-1").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        }
    }

    #[test]
    fn rejects_invalid_request() {
        assert!(ProjectId::new("").is_err());
        assert!(ProjectId::new("../secret").is_err());
        assert!(ProjectId::new("a".repeat(129)).is_err());
        assert!(serde_json::from_str::<SessionId>("\"x y\"").is_err());
    }

    #[test]
    fn a_lone_dot_is_not_an_identity() {
        assert!(ProjectId::new(".").is_err());
        assert!(serde_json::from_str::<ProjectId>("\".\"").is_err());
        assert!(ProjectId::new("v1.2").is_ok());
    }

    #[test]
    fn only_one_plain_path_component_counts_as_one() {
        for text in ["", ".", "..", "a/b", "/a", "a/", "./a", "a/."] {
            assert!(!single_component(text), "{text}");
        }
        for text in ["a", "v1.2", "a..b", "owner:abc"] {
            assert!(single_component(text), "{text}");
        }
    }

    #[test]
    fn an_identity_cannot_alias_a_folder_through_windows_name_normalization() {
        for text in [
            "foo.", "foo ", "con", "CON", "Con", "nul", "NUL.txt", "con.log", "nul .txt", "aux",
            "prn", "com1", "COM9", "lpt1", "LPT9",
        ] {
            assert!(
                !single_component(text),
                "single_component accepted {text:?}"
            );
            assert!(ProjectId::new(text).is_err(), "ProjectId accepted {text:?}");
        }
        for text in [
            "com10", "lpt10", "console", "connie", "nuly", "a.con", "v1.2",
        ] {
            assert!(single_component(text), "single_component refused {text:?}");
            assert!(ProjectId::new(text).is_ok(), "ProjectId refused {text:?}");
        }
    }

    #[test]
    fn a_device_name_with_a_trailing_colon_is_still_a_device_and_no_identity() {
        for text in ["con:", "CON:", "com1:", "LPT9:", "nul.txt:"] {
            assert!(windows_device(text), "windows_device missed {text:?}");
            assert!(
                !single_component(text),
                "single_component accepted {text:?}"
            );
            assert!(ProjectId::new(text).is_err(), "ProjectId accepted {text:?}");
        }
        for text in ["owner:abc", "console:", "com10:"] {
            assert!(!windows_device(text), "windows_device refused {text:?}");
        }
        assert!(
            ClientId::new("owner:abc").is_ok(),
            "an identity keeps its colon"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_prefix_is_not_an_identity() {
        for text in ["c:", "c:x", "z:"] {
            assert!(!single_component(text), "{text}");
            assert!(ProjectId::new(text).is_err(), "{text}");
        }
        assert!(ClientId::new("owner:abc").is_ok());
    }

    #[test]
    fn rejects_unsupported_version_and_secret_fields() {
        let mut req = request();
        req.protocol_version = 2;
        assert!(req.validate().is_err());
        let mut value = serde_json::to_value(request()).unwrap();
        value["credential_value"] = "forbidden".into();
        assert!(serde_json::from_value::<WireRequest>(value).is_err());
    }

    #[test]
    fn frames_are_bounded_and_exact() {
        assert!(encode_frame(&"x".repeat(FRAME_BYTES_MAX)).is_err());
        let mut frame = encode_frame(&request()).unwrap();
        assert_eq!(decode_request(&frame).unwrap(), request());
        frame[3] ^= 1;
        assert!(decode_request(&frame).is_err());
    }

    #[test]
    fn cursor_has_canonical_string_shape() {
        assert_eq!(
            serde_json::to_string(&Cursor::new(u64::MAX)).unwrap(),
            format!("\"{}\"", u64::MAX)
        );
        assert!(serde_json::from_str::<Cursor>("\"01\"").is_err());
    }

    #[test]
    fn a_project_name_that_hides_or_reorders_characters_is_refused() {
        let spec = |name: &str| ProjectSpec {
            name: name.into(),
            repositories: Vec::new(),
        };
        for name in [
            "invoice\u{202e}fdp.exe",
            "Harbor\u{2066}Notes",
            "Harbor\u{200b}Notes",
            "Harbor\u{e0041}",
            "Harbor\u{061c}Notes",
        ] {
            assert_eq!(
                spec(name).validate().unwrap_err().0,
                "invalid project specification",
                "{name:?}"
            );
        }
        for name in [
            "Harbor Notes",
            "Café 日本",
            "Lab \u{1f469}\u{200d}\u{1f4bb}",
        ] {
            assert!(spec(name).validate().is_ok(), "{name:?}");
        }
    }
}
