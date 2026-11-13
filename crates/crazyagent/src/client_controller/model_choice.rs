use super::*;
use crate::engine::owner::ModelAdded;
use crate::model_choice::rules::{Rule, RuleWritten, Rules};
use crate::model_choice::test_launch::Tested;
use crate::model_choice::{Models, Slot, Written};
use crate::protocol::{CredentialRef, ProjectId};
use serde_json::Value;

impl OwnerController {
    fn model_choice_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads a harness's Models section app-wide or for one project (DEC-150); `fresh`
    /// marks a Models view opening, which reads the harness's own list once first. Inputs:
    /// the harness, the project, none for the app, and `fresh`. Output: the section, or a
    /// `MODEL_CHOICE_PROTOCOL_CODES` refusal.
    pub async fn model_choice_values(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        fresh: bool,
    ) -> Result<Models, EngineError> {
        self.model_choice_client()?
            .model_choice_values(harness, project, fresh)
            .await
    }

    /// Sets one model slot, naming the version the owner read. Inputs: the harness, the
    /// project, none for the app, the slot, the value and the version. Output: Saved,
    /// Stale or Refused with the slot's row, or a `MODEL_CHOICE_PROTOCOL_CODES` refusal.
    pub async fn model_choice_set(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        slot: Slot,
        value: Value,
        version: u64,
    ) -> Result<Written, EngineError> {
        self.model_choice_client()?
            .model_choice_set(harness, project, slot, value, version)
            .await
    }

    /// Resets one model slot, naming the version the owner read. Inputs: as
    /// `model_choice_set`, without a value. Output: as `model_choice_set`.
    pub async fn model_choice_reset(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        slot: Slot,
        version: u64,
    ) -> Result<Written, EngineError> {
        self.model_choice_client()?
            .model_choice_reset(harness, project, slot, version)
            .await
    }

    /// Adds a model id the owner typed for a harness, which waits for its test launch.
    /// Inputs: the harness and the id. Output: what was done with the app-wide section,
    /// or a refusal.
    pub async fn model_choice_add(
        &self,
        harness: &str,
        model: &str,
    ) -> Result<(ModelAdded, Models), EngineError> {
        self.model_choice_client()?
            .model_choice_add(harness, model)
            .await
    }

    /// Removes a model id the owner added for a harness. Inputs: the harness and the id.
    /// Output: whether it was there, with the app-wide section, or a refusal.
    pub async fn model_choice_remove(
        &self,
        harness: &str,
        model: &str,
    ) -> Result<(bool, Models), EngineError> {
        self.model_choice_client()?
            .model_choice_remove(harness, model)
            .await
    }

    /// Runs the test launch of one pending owner id on the pool the owner picked (DEC-150
    /// M-6). Waits until the engine decides, up to about two and a half minutes. Inputs: the
    /// harness, the id and the pool. Output: the decision, its pending reason and the
    /// app-wide section, or a refusal.
    pub async fn model_choice_test(
        &self,
        harness: &str,
        model: &str,
        pool: CredentialRef,
    ) -> Result<(Tested, Option<String>, Models), EngineError> {
        self.model_choice_client()?
            .model_choice_test(harness, model, pool)
            .await
    }

    /// Reads the owner's two model rules with their versions (DEC-150 Q2). Output: the
    /// rules, or a `MODEL_CHOICE_PROTOCOL_CODES` refusal.
    pub async fn model_choice_rules(&self) -> Result<Rules, EngineError> {
        self.model_choice_client()?.model_choice_rules().await
    }

    /// Sets one model rule, naming the version the owner read. Inputs: the rule with its
    /// value and the version. Output: the saved or stale rules, or a refusal.
    pub async fn model_choice_set_rule(
        &self,
        rule: Rule,
        version: u64,
    ) -> Result<RuleWritten, EngineError> {
        self.model_choice_client()?
            .model_choice_set_rule(rule, version)
            .await
    }
}
