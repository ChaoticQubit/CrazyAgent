use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const FRAME_BYTES_MAX: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub String);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

fn validate_identity(value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        || value.contains("..")
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
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError("unsupported protocol version".into()));
        }
        match &self.command {
            Command::CreateProject { spec, .. } => {
                if spec.name.trim().is_empty()
                    || spec.name.len() > 256
                    || spec.name.chars().any(char::is_control)
                    || spec.repositories.len() > 64
                    || spec.repositories.iter().any(|p| {
                        p.is_empty()
                            || p.len() > 4096
                            || p.chars().any(char::is_control)
                            || p.contains("://")
                    })
                {
                    return Err(ProtocolError("invalid project specification".into()));
                }
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
    request.validate()?;
    assert_eq!(size, frame.len() - 4);
    assert!(size <= FRAME_BYTES_MAX);
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
