use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, SCORECARD_CODES, ScorecardSaved};
use crazyagent::scorecard::{OFFSET_MINUTES_MAX, Scorecard, Settings};
use serde::Serialize;
use std::sync::Arc;

/// How a scorecard settings save ended, as the window reads it: saved at a new version, or
/// stale because the settings changed since the window read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScorecardSave {
    Saved { version: u64 },
    Stale { current: u64 },
}

/// The bridge failure for an engine scorecard error: a known scorecard code reaches the
/// window as itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
fn scorecard_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        SCORECARD_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("scorecard_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    fn scorecard_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(scorecard_error)
    }

    /// Reads the go/no-go scorecard in the viewer's local days, run without the bridge
    /// lock. Input: the viewer's offset east of UTC in minutes. Output: the scorecard, or
    /// `scorecard_request_invalid` for an offset past `OFFSET_MINUTES_MAX`.
    pub fn scorecard_report(
        &self,
        offset_minutes: i32,
    ) -> Result<Unlocked<Scorecard>, BridgeError> {
        if !(-OFFSET_MINUTES_MAX..=OFFSET_MINUTES_MAX).contains(&offset_minutes) {
            return Err(failure("scorecard_request_invalid"));
        }
        let client = self.scorecard_client()?;
        Ok(Box::pin(async move {
            client
                .scorecard_report(offset_minutes)
                .await
                .map_err(scorecard_error)
        }))
    }

    /// Saves the scorecard settings, naming the version the window read, run without the
    /// bridge lock. Inputs: the settings and the version. Output: saved or stale.
    pub fn scorecard_set(
        &self,
        settings: Settings,
        version: u64,
    ) -> Result<Unlocked<ScorecardSave>, BridgeError> {
        let client = self.scorecard_client()?;
        Ok(Box::pin(async move {
            match client
                .scorecard_set(settings, version)
                .await
                .map_err(scorecard_error)?
            {
                ScorecardSaved::Saved(version) => Ok(ScorecardSave::Saved { version }),
                ScorecardSaved::Stale(current) => Ok(ScorecardSave::Stale { current }),
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scorecard code the engine answers reaches the window as itself; anything else
    /// is an unknown outcome with the engine code behind it and raw engine text withheld.
    #[test]
    fn engine_scorecard_codes_pass_through_unchanged() {
        for code in SCORECARD_CODES {
            assert_eq!(scorecard_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            scorecard_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = scorecard_error(EngineError("scorecard_state_torn".into()));
        assert_eq!(unlisted.code, "scorecard_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("scorecard_state_torn"));
        let raw = scorecard_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "scorecard_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }

    /// A save outcome reaches the window as a tagged object it can switch on.
    #[test]
    fn a_save_outcome_serializes_with_its_kind() {
        assert_eq!(
            serde_json::to_value(ScorecardSave::Saved { version: 2 }).unwrap(),
            serde_json::json!({"kind": "saved", "version": 2})
        );
        assert_eq!(
            serde_json::to_value(ScorecardSave::Stale { current: 3 }).unwrap(),
            serde_json::json!({"kind": "stale", "current": 3})
        );
    }
}
