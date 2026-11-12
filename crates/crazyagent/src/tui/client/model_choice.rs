use super::settings::problem;
use super::*;
use crate::engine::EngineError;
use crate::engine::owner::ModelAdded;
use crate::model_choice::rules::{Rule, RuleWritten, Rules};
use crate::model_choice::{Models, Slot, Written};
use serde_json::Value;

/// One model choice request a Models screen sends (DEC-150): read a harness's section at one
/// scope, with the rules on the app page; set or reset one slot naming the version the owner
/// read; add or remove an owner id; or set one rule naming its version. No project means the
/// app-wide values. Runs on the owner worker as a Settings job.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) enum ModelChoiceQuery {
    Read {
        harness: String,
        project: Option<ProjectId>,
    },
    Set {
        harness: String,
        project: Option<ProjectId>,
        slot: Slot,
        value: Value,
        version: u64,
    },
    Reset {
        harness: String,
        project: Option<ProjectId>,
        slot: Slot,
        version: u64,
    },
    Add {
        harness: String,
        model: String,
    },
    Remove {
        harness: String,
        model: String,
    },
    SetRule {
        rule: Rule,
        version: u64,
    },
}

/// What the engine answered a model choice request with: the section with the rules (none
/// on a project page), what a slot set or reset did, what an add or remove did with the
/// app-wide section after it, or what a rule set did.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) enum ModelChoiceReply {
    Read {
        models: Models,
        rules: Option<Rules>,
    },
    Written(Written),
    Added(ModelAdded, Models),
    Removed(bool, Models),
    RuleWritten(RuleWritten),
}

/// One model choice reply for the screens: the query it answers and the engine's answer, or
/// a bounded plain code.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) struct ModelChoiceEvent {
    pub query: ModelChoiceQuery,
    pub result: Result<ModelChoiceReply, String>,
}

impl ModelChoiceEvent {
    /// The reply for a request that reached no trusted connection. Takes the query.
    /// Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: ModelChoiceQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl NativeOwner {
    /// Runs one model choice request through the trusted owner controller. Inputs: the
    /// query and the lost-connection flag. Output: the event; a transport failure sets the
    /// flag so the shell reconnects.
    pub(super) async fn model_choice(
        &mut self,
        query: ModelChoiceQuery,
        lost: &mut bool,
    ) -> ModelChoiceEvent {
        let result = match self.settings_controller() {
            Ok(controller) => run(controller, &query)
                .await
                .map_err(|error| problem(&error, lost)),
            Err(code) => Err(code),
        };
        ModelChoiceEvent { query, result }
    }
}

async fn run(
    controller: &OwnerController,
    query: &ModelChoiceQuery,
) -> Result<ModelChoiceReply, EngineError> {
    match query {
        ModelChoiceQuery::Read { harness, project } => {
            let models = controller
                .model_choice_values(harness, project.clone())
                .await?;
            let rules = match project {
                None => Some(controller.model_choice_rules().await?),
                Some(_) => None,
            };
            Ok(ModelChoiceReply::Read { models, rules })
        }
        ModelChoiceQuery::Set {
            harness,
            project,
            slot,
            value,
            version,
        } => controller
            .model_choice_set(harness, project.clone(), *slot, value.clone(), *version)
            .await
            .map(ModelChoiceReply::Written),
        ModelChoiceQuery::Reset {
            harness,
            project,
            slot,
            version,
        } => controller
            .model_choice_reset(harness, project.clone(), *slot, *version)
            .await
            .map(ModelChoiceReply::Written),
        ModelChoiceQuery::Add { harness, model } => controller
            .model_choice_add(harness, model)
            .await
            .map(|(added, models)| ModelChoiceReply::Added(added, models)),
        ModelChoiceQuery::Remove { harness, model } => controller
            .model_choice_remove(harness, model)
            .await
            .map(|(removed, models)| ModelChoiceReply::Removed(removed, models)),
        ModelChoiceQuery::SetRule { rule, version } => controller
            .model_choice_set_rule(*rule, *version)
            .await
            .map(ModelChoiceReply::RuleWritten),
    }
}
