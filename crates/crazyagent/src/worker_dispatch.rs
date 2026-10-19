use crate::protocol::{Cursor, PlanId, RequestId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRef {
    pub plan_id: PlanId,
    pub item_id: TaskId,
    pub reservation_cursor: Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRunner {
    pub(crate) root: PathBuf,
    pub(crate) registration_id: RequestId,
    pub(crate) attempt: AttemptRef,
    pub(crate) slot: u8,
}

impl PreparedRunner {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registration_id(&self) -> &RequestId {
        &self.registration_id
    }

    pub fn attempt(&self) -> &AttemptRef {
        &self.attempt
    }

    pub fn slot(&self) -> u8 {
        self.slot
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerBootIdentity {
    pub runner_id: RequestId,
    pub pid: u32,
    pub started_at_unix_ns: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerReady {
    pub registration_id: RequestId,
    pub attempt: AttemptRef,
    pub boot: RunnerBootIdentity,
    pub database_instance: String,
    pub engine_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerRequest {
    Ready {
        protocol_version: u16,
        registration_id: RequestId,
        boot: RunnerBootIdentity,
    },
}

pub(crate) const RUNNERS_MAX: usize = 128;
pub(crate) const RUNNER_BOOTSTRAP_MAX: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunnerBootstrap {
    pub version: u8,
    pub registration_id: RequestId,
    pub attempt: AttemptRef,
    pub slot: u8,
}

impl RunnerBootstrap {
    pub(crate) fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if self.version != 1
            || usize::from(self.slot) >= RUNNERS_MAX
            || self.attempt.reservation_cursor.value() == 0
        {
            return Err(crate::engine::EngineError(
                "runner_bootstrap_invalid".into(),
            ));
        }
        Ok(())
    }
}

impl RunnerBootIdentity {
    pub fn validate(&self) -> Result<(), crate::engine::EngineError> {
        if self.pid == 0
            || self.started_at_unix_ns.len() > 39
            || !matches!(self.started_at_unix_ns.parse::<u128>(), Ok(value) if value > 0)
        {
            return Err(crate::engine::EngineError("runner_boot_invalid".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerReply {
    Ready { identity: RunnerReady },
}
