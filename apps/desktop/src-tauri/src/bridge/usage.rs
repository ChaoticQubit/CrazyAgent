use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, USAGE_CODES};
use crazyagent::protocol::CredentialRef;
use crazyagent::usage::WindowKind;
use crazyagent::usage::report::UsageReport;
use std::sync::Arc;

fn usage_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        USAGE_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("usage_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    fn usage_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(usage_error)
    }

    /// Reads the usage page's numbers, run without the bridge lock. Inputs: the range start
    /// in milliseconds, None for all time, and the viewer's offset east of UTC in minutes.
    /// Output: totals, groups, days, lines changed and every account's limits.
    pub fn usage_report(
        &self,
        since_ms: Option<i64>,
        offset_minutes: i32,
    ) -> Result<Unlocked<UsageReport>, BridgeError> {
        let client = self.usage_client()?;
        Ok(Box::pin(async move {
            client
                .usage_report(since_ms, offset_minutes)
                .await
                .map_err(usage_error)
        }))
    }

    /// Asks the engine to poll every signed-in account's limits now, run without the
    /// bridge lock. Output: done once the request is stored.
    pub fn usage_refresh(&self) -> Result<Unlocked<()>, BridgeError> {
        let client = self.usage_client()?;
        Ok(Box::pin(async move {
            client.usage_refresh().await.map_err(usage_error)
        }))
    }

    /// Sets or clears the owner's manual reset time of one account's window, run without
    /// the bridge lock. Inputs: the account id, "five_hour" or "weekly", and the time.
    pub fn usage_set_reset(
        &self,
        account: String,
        window: String,
        at_ms: Option<i64>,
    ) -> Result<Unlocked<()>, BridgeError> {
        let account = CredentialRef::new(account).map_err(|_| failure("usage_request_invalid"))?;
        let window = match window.as_str() {
            "five_hour" => WindowKind::FiveHour,
            "weekly" => WindowKind::Weekly,
            _ => return Err(failure("usage_request_invalid")),
        };
        let client = self.usage_client()?;
        Ok(Box::pin(async move {
            client
                .usage_set_reset(account, window, at_ms)
                .await
                .map_err(usage_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every usage code the engine answers reaches the window as itself; anything else is
    /// an unknown outcome with the engine code behind it and raw engine text withheld.
    #[test]
    fn engine_usage_codes_pass_through_unchanged() {
        for code in USAGE_CODES {
            assert_eq!(usage_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            usage_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = usage_error(EngineError("usage_state_torn".into()));
        assert_eq!(unlisted.code, "usage_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("usage_state_torn"));
        let raw = usage_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "usage_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }
}
