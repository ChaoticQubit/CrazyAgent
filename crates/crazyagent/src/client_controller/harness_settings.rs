use super::*;
use crate::harness_settings::{CatalogPage, Page, Written};
use crate::protocol::ProjectId;
use serde_json::Value;

impl OwnerController {
    fn harness_settings_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads one page of a harness's settings catalog. Inputs: the harness id and the
    /// `next` of the page before, none for the first. Output: the page, or a
    /// `HARNESS_SETTINGS_CODES` refusal.
    pub async fn harness_settings_catalog(
        &self,
        harness: &str,
        after: Option<u32>,
    ) -> Result<CatalogPage, EngineError> {
        self.harness_settings_client()?
            .harness_settings_catalog(harness, after)
            .await
    }

    /// Reads one page of a harness's settings app-wide or for one project. Inputs: the
    /// harness, the project, none for the app, and the `next` of the page before. Output:
    /// the page, or a `HARNESS_SETTINGS_CODES` refusal.
    pub async fn harness_settings_values(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        after: Option<u32>,
    ) -> Result<Page, EngineError> {
        self.harness_settings_client()?
            .harness_settings_values(harness, project, after)
            .await
    }

    /// Sets one harness setting, naming the version the owner read. Inputs: the harness,
    /// the project, none for the app, the key, the value and the version. Output: Saved,
    /// Stale or Refused with the key's row, or a `HARNESS_SETTINGS_CODES` refusal.
    pub async fn harness_settings_set(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        key: &str,
        value: Value,
        version: u64,
    ) -> Result<Written, EngineError> {
        self.harness_settings_client()?
            .harness_settings_set(harness, project, key, value, version)
            .await
    }

    /// Resets one harness setting, naming the version the owner read. Inputs: as
    /// `harness_settings_set`, without a value. Output: as `harness_settings_set`.
    pub async fn harness_settings_reset(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        key: &str,
        version: u64,
    ) -> Result<Written, EngineError> {
        self.harness_settings_client()?
            .harness_settings_reset(harness, project, key, version)
            .await
    }
}
