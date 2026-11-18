use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OVERVIEW_CODES, OwnerClient};
use crazyagent::project_overview::{Apply, Overview, README_BYTES_MAX, TAB_BYTES_MAX, Tab};
use crazyagent::protocol::ProjectId;
use crazyagent::roadmap_status::Refresh;
use std::sync::Arc;

/// The most texts one Overview carries beside its README: a file and a proposal per tab.
const TEXTS_MAX: usize = 2 * Tab::ALL.len();
/// The most JSON bytes one byte of text can become: a control character as a six-byte escape.
const JSON_BYTES_PER_BYTE_MAX: usize = 6;
/// The most bytes of one Overview read handed to the window: every text at its engine bound,
/// escaped at the worst rate, and room for the other fields.
const OVERVIEW_BYTES_MAX: usize =
    JSON_BYTES_PER_BYTE_MAX * (README_BYTES_MAX as usize + TEXTS_MAX * TAB_BYTES_MAX) + 65_536;

const _: () = assert!(README_BYTES_MAX <= 1024 * 1024);
const _: () = assert!(OVERVIEW_BYTES_MAX <= 8 * 1024 * 1024);

/// Turns an engine refusal of an Overview call into the window's failure: a listed code
/// passes as itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the failure.
pub(super) fn overview_error(error: EngineError) -> BridgeError {
    assert!(!error.0.is_empty(), "an engine refusal carries a code");
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        OVERVIEW_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("overview_outcome_unknown"),
        &error.0,
    )
}

fn project(id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("overview_request_invalid"))
}

impl NativeBridge {
    fn overview_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(overview_error)
    }

    /// Reads a project's Overview, run without the bridge lock: the README, the four tabs
    /// with their text, version and proposal, the competitor state, the run state and the
    /// two settings. A reply that does not hold the four tabs is an unknown outcome. Input:
    /// the project id. Output: the Overview, or the refusal.
    pub fn overview_read(&self, project_id: String) -> Result<Unlocked<Overview>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            let overview = client
                .overview_read(project)
                .await
                .map_err(overview_error)?;
            if overview.tabs.len() != Tab::ALL.len() {
                return Err(caused("overview_outcome_unknown", "overview_reply_shape"));
            }
            bounded_by(overview, OVERVIEW_BYTES_MAX)
        }))
    }

    /// Asks for one writing run of a project's tabs now, run without the bridge lock.
    /// Input: the project id. Output: done once the engine took the ask, or the refusal.
    pub fn overview_refresh(&self, project_id: String) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            client
                .overview_refresh(project)
                .await
                .map_err(overview_error)
        }))
    }

    /// Saves the owner's text for one tab, run without the bridge lock. A text over the tab
    /// bound is refused here before the engine is asked, and the engine checks it again.
    /// Inputs: the project id, the tab, the text and the version the editor opened at.
    /// Output: done, or the refusal, `overview_tab_changed` when the tab moved on.
    pub fn overview_save(
        &self,
        project_id: String,
        tab: Tab,
        text: String,
        version: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        if text.len() > TAB_BYTES_MAX {
            return Err(failure("overview_tab_too_large"));
        }
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            assert!(text.len() <= TAB_BYTES_MAX);
            client
                .overview_save(project, tab, text, version)
                .await
                .map_err(overview_error)
        }))
    }

    /// Accepts a refresh's proposal for one tab, run without the bridge lock. Inputs: the
    /// project id, the tab, the tab version the shown diff was made from and the version of
    /// the proposal shown. Output: done, or the refusal, `overview_proposal_changed` when a
    /// newer proposal took its place.
    pub fn overview_accept(
        &self,
        project_id: String,
        tab: Tab,
        version: String,
        proposal_version: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            client
                .overview_accept(project, tab, version, proposal_version)
                .await
                .map_err(overview_error)
        }))
    }

    /// Rejects a refresh's proposal for one tab and keeps the tab's text, run without the
    /// bridge lock. Inputs: the project id and the tab. Output: done, or the refusal.
    pub fn overview_reject(
        &self,
        project_id: String,
        tab: Tab,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            client
                .overview_reject(project, tab)
                .await
                .map_err(overview_error)
        }))
    }

    /// Saves when a project's tabs are written, run without the bridge lock. Inputs: the
    /// project id and the setting. Output: done, or the refusal.
    pub fn overview_set_refresh(
        &self,
        project_id: String,
        refresh: Refresh,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            client
                .overview_set_refresh(project, refresh)
                .await
                .map_err(overview_error)
        }))
    }

    /// Saves what a refresh does with a tab, run without the bridge lock. Inputs: the
    /// project id and the setting. Output: done, or the refusal.
    pub fn overview_set_apply(
        &self,
        project_id: String,
        apply: Apply,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.overview_client()?;
        Ok(Box::pin(async move {
            client
                .overview_set_apply(project, apply)
                .await
                .map_err(overview_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Overview code the engine answers reaches the window as itself; anything else
    /// is an unknown outcome with the engine code behind it and raw engine text withheld.
    #[test]
    fn engine_overview_codes_pass_through_unchanged() {
        assert!(OVERVIEW_CODES.len() >= 10);
        for code in OVERVIEW_CODES {
            assert_eq!(overview_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            overview_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = overview_error(EngineError("overview_state_torn".into()));
        assert_eq!(unlisted.code, "overview_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("overview_state_torn"));
        let raw = overview_error(EngineError("open /Users/mira/notes: denied".into()));
        assert_eq!(raw.code, "overview_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }

    /// A project id that is not one is refused before any engine call.
    #[test]
    fn a_project_id_that_is_not_one_is_a_request_the_bridge_refuses() {
        assert_eq!(
            project("../secret".into()).unwrap_err().code,
            "overview_request_invalid"
        );
        assert_eq!(project("alpha".into()).unwrap().as_str(), "alpha");
    }

    /// The read bound holds every text at the engine's bound, even escaped at the worst rate.
    #[test]
    fn the_read_bound_fits_an_overview_with_every_text_at_its_engine_bound() {
        let text = "\u{1}".repeat(TAB_BYTES_MAX);
        let escaped = serde_json::to_vec(&text).unwrap().len();
        assert!(escaped <= JSON_BYTES_PER_BYTE_MAX * TAB_BYTES_MAX + 2);
        let texts = README_BYTES_MAX as usize + TEXTS_MAX * TAB_BYTES_MAX;
        assert!(OVERVIEW_BYTES_MAX >= JSON_BYTES_PER_BYTE_MAX * texts);
        assert_eq!(TEXTS_MAX, 8);
    }
}
