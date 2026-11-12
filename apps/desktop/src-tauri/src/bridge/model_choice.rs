use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{MODEL_CHOICE_PROTOCOL_CODES, ModelAdded, OwnerClient};
use crazyagent::model_choice::rules::{FallbackRule, PickRule, Rule, RuleWritten, Rules};
use crazyagent::model_choice::{
    ALLOWED_MAX, CATALOG_MODELS_MAX, CUSTOM_MAX, MODEL_BYTES_MAX, Models, Slot,
    UNQUALIFIED_REASON_BYTES_MAX, Written, shipped,
};
use crazyagent::protocol::ProjectId;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

/// One harness's Models section as the window shows it: the engine's section, the project
/// it was read for (None on the app page), and the versions and published references the
/// shipped model list was checked against, both empty for a harness without a list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelsPage {
    pub models: Models,
    pub project_id: Option<String>,
    pub qualified_versions: Vec<String>,
    pub sources: Vec<String>,
}

/// The bridge failure for an engine model choice error: a known code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it. Input: the
/// engine error. Output: the bridge failure.
pub fn model_choice_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        MODEL_CHOICE_PROTOCOL_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("model_choice_outcome_unknown"),
        &error.0,
    )
}

/// Reads what a slot set or reset did: Ok once saved, `model_choice_stale` when the slot
/// changed since the owner read it, or the engine's refusal code. Input: the engine reply.
/// Output: the result.
pub fn written(written: Written) -> Result<(), BridgeError> {
    match written {
        Written::Saved { .. } => Ok(()),
        Written::Stale { .. } => Err(failure("model_choice_stale")),
        Written::Refused { code, .. } => Err(model_choice_error(EngineError(code))),
    }
}

/// Reads what a rule set did: Ok once saved, `model_choice_stale` when the rule changed
/// since the owner read it. Input: the engine reply. Output: the result.
pub fn rule_written(written: RuleWritten) -> Result<(), BridgeError> {
    match written {
        RuleWritten::Saved { .. } => Ok(()),
        RuleWritten::Stale { .. } => Err(failure("model_choice_stale")),
    }
}

/// A slot as the window names it. Input: the word. Output: the slot, or
/// `model_choice_request_invalid`.
pub fn window_slot(slot: &str) -> Result<Slot, BridgeError> {
    Slot::ALL
        .into_iter()
        .find(|known| known.as_str() == slot)
        .ok_or_else(|| failure("model_choice_request_invalid"))
}

fn window_id(id: &Value) -> bool {
    id.as_str()
        .is_some_and(|id| (1..=MODEL_BYTES_MAX).contains(&id.len()))
}

/// A slot value as the window sends it, bounded before the engine checks it against the
/// harness: one model id of at most `MODEL_BYTES_MAX` bytes for the master and agent slots,
/// a list of at most `ALLOWED_MAX` such ids for the allowed slot. Clearing a slot is a reset,
/// never a null. Inputs: the slot and the value. Output: the value, or
/// `model_choice_value_invalid`.
pub fn window_value(slot: Slot, value: Value) -> Result<Value, BridgeError> {
    let fits = match slot {
        Slot::Master | Slot::Agent => window_id(&value),
        Slot::Allowed => value
            .as_array()
            .is_some_and(|ids| ids.len() <= ALLOWED_MAX && ids.iter().all(window_id)),
    };
    if fits {
        Ok(value)
    } else {
        Err(failure("model_choice_value_invalid"))
    }
}

/// A model id the owner typed, bounded before the engine checks it against the harness's
/// id rule. Input: the id. Output: the id, or `model_choice_value_invalid`.
pub fn window_model(model: String) -> Result<String, BridgeError> {
    if (1..=MODEL_BYTES_MAX).contains(&model.len()) {
        Ok(model)
    } else {
        Err(failure("model_choice_value_invalid"))
    }
}

/// A rule and its value as the window names them. Inputs: `pick` or `fallback` and the
/// value word. Output: the rule, or `model_choice_request_invalid`.
pub fn window_rule(rule: &str, value: &str) -> Result<Rule, BridgeError> {
    let rule = match (rule, value) {
        ("pick", "strict") => Rule::Pick(PickRule::Strict),
        ("pick", "hint") => Rule::Pick(PickRule::Hint),
        ("pick", "default") => Rule::Pick(PickRule::Default),
        ("fallback", "notice") => Rule::Fallback(FallbackRule::Notice),
        ("fallback", "stop") => Rule::Fallback(FallbackRule::Stop),
        ("fallback", "silent") => Rule::Fallback(FallbackRule::Silent),
        _ => return Err(failure("model_choice_request_invalid")),
    };
    Ok(rule)
}

fn window_harness(harness: String) -> Result<String, BridgeError> {
    super::harness_settings::window_harness(harness)
        .map_err(|_| failure("model_choice_request_invalid"))
}

fn window_project(project_id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    project_id
        .map(|id| ProjectId::new(id).map_err(|_| failure("model_choice_project_unknown")))
        .transpose()
}

/// The window's page from the engine's section: every slot once in page order and at most
/// the catalog's ids plus the owner's, joined with the shipped list's versions and
/// references. Inputs: the section, the harness asked for and the project id. Output: the
/// page, or `owner_read_failed` for a section that breaks that shape.
pub fn page_of(
    models: Models,
    harness: &str,
    project_id: Option<String>,
) -> Result<ModelsPage, BridgeError> {
    let slots: Vec<Slot> = models.rows.iter().map(|row| row.slot).collect();
    if slots != Slot::ALL
        || models.candidates.len() > CATALOG_MODELS_MAX + CUSTOM_MAX
        || models.harness != harness
        || models
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > UNQUALIFIED_REASON_BYTES_MAX)
    {
        return Err(failure("owner_read_failed"));
    }
    let catalog = shipped(harness);
    let page = ModelsPage {
        qualified_versions: catalog
            .as_ref()
            .map(|catalog| catalog.qualified_versions.clone())
            .unwrap_or_default(),
        sources: catalog.map(|catalog| catalog.sources).unwrap_or_default(),
        models,
        project_id,
    };
    assert_eq!(page.models.rows.len(), Slot::ALL.len());
    Ok(page)
}

impl NativeBridge {
    fn model_choice_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(model_choice_error)
    }

    /// Reads one harness's Models section app-wide or for one project, run without the
    /// bridge lock (DEC-150). Inputs: the harness id and the project id, None for the app.
    /// Output: the page, or the engine's refusal.
    pub fn model_choice(
        &self,
        harness: String,
        project_id: Option<String>,
    ) -> Result<Unlocked<ModelsPage>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id.clone())?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            let models = client
                .model_choice_values(&harness, project)
                .await
                .map_err(model_choice_error)?;
            page_of(models, &harness, project_id)
        }))
    }

    /// Sets one model slot app-wide or for one project, naming the version the owner read,
    /// run without the bridge lock. Inputs: the harness, the project id, None for the app,
    /// the slot word, the value and the version. Output: Ok once saved;
    /// `model_choice_stale` when it changed since, or the engine's refusal.
    pub fn set_model_choice(
        &self,
        harness: String,
        project_id: Option<String>,
        slot: String,
        value: Value,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id)?;
        let slot = window_slot(&slot)?;
        let value = window_value(slot, value)?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            written(
                client
                    .model_choice_set(&harness, project, slot, value, version)
                    .await
                    .map_err(model_choice_error)?,
            )
        }))
    }

    /// Clears one model slot at one scope, naming the version the owner read, run without
    /// the bridge lock. Inputs: as `set_model_choice`, without a value. Output: as
    /// `set_model_choice`.
    pub fn reset_model_choice(
        &self,
        harness: String,
        project_id: Option<String>,
        slot: String,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id)?;
        let slot = window_slot(&slot)?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            written(
                client
                    .model_choice_reset(&harness, project, slot, version)
                    .await
                    .map_err(model_choice_error)?,
            )
        }))
    }

    /// Adds a model id the owner typed for a harness; it waits for its test launch. Run
    /// without the bridge lock. Inputs: the harness and the id. Output: pending, listed
    /// when the harness's list already has it, or verified when it already passed; or the
    /// engine's refusal, such as `model_choice_value_invalid`.
    pub fn add_model(
        &self,
        harness: String,
        model: String,
    ) -> Result<Unlocked<ModelAdded>, BridgeError> {
        let harness = window_harness(harness)?;
        let model = window_model(model)?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            let (added, _) = client
                .model_choice_add(&harness, &model)
                .await
                .map_err(model_choice_error)?;
            Ok(added)
        }))
    }

    /// Removes a model id the owner added for a harness, run without the bridge lock.
    /// Inputs: the harness and the id. Output: whether it was there, or the engine's
    /// refusal.
    pub fn remove_model(
        &self,
        harness: String,
        model: String,
    ) -> Result<Unlocked<bool>, BridgeError> {
        let harness = window_harness(harness)?;
        let model = window_model(model)?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            let (removed, _) = client
                .model_choice_remove(&harness, &model)
                .await
                .map_err(model_choice_error)?;
            Ok(removed)
        }))
    }

    /// Reads the owner's two app-wide model rules with their versions, run without the
    /// bridge lock. Output: the rules, or the engine's refusal.
    pub fn model_rules(&self) -> Result<Unlocked<Rules>, BridgeError> {
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            client
                .model_choice_rules()
                .await
                .map_err(model_choice_error)
        }))
    }

    /// Sets one model rule, naming the version the owner read, run without the bridge
    /// lock. Inputs: `pick` or `fallback`, the value word and the version. Output: Ok once
    /// saved; `model_choice_stale` when it changed since, or the engine's refusal.
    pub fn set_model_rule(
        &self,
        rule: String,
        value: String,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let rule = window_rule(&rule, &value)?;
        let client = self.model_choice_client()?;
        Ok(Box::pin(async move {
            rule_written(
                client
                    .model_choice_set_rule(rule, version)
                    .await
                    .map_err(model_choice_error)?,
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crazyagent::harness_settings::{Access, Source};
    use crazyagent::model_choice::{Candidate, Origin, Row};
    use serde_json::json;

    /// Every model choice code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind it.
    #[test]
    fn engine_model_choice_codes_pass_through_unchanged() {
        assert!(!MODEL_CHOICE_PROTOCOL_CODES.is_empty());
        for code in MODEL_CHOICE_PROTOCOL_CODES {
            assert_eq!(model_choice_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            model_choice_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = model_choice_error(EngineError("model_choice_torn".into()));
        assert_eq!(unlisted.code, "model_choice_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("model_choice_torn"));
        let raw = model_choice_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "model_choice_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }

    fn row(slot: Slot) -> Row {
        Row {
            slot,
            here: None,
            effective: Value::Null,
            source: Source::Default,
            version: 3,
            access: Access::Editable,
        }
    }

    /// A saved write is Ok, a stale one asks for a fresh read, and a refused one carries the
    /// engine's code, for slots and rules alike.
    #[test]
    fn a_write_is_saved_stale_or_refused_with_the_engine_code() {
        assert!(
            written(Written::Saved {
                row: row(Slot::Master)
            })
            .is_ok()
        );
        assert_eq!(
            written(Written::Stale {
                row: row(Slot::Master)
            })
            .unwrap_err()
            .code,
            "model_choice_stale"
        );
        assert_eq!(
            written(Written::Refused {
                code: "model_choice_not_supported".into(),
                row: row(Slot::Agent)
            })
            .unwrap_err()
            .code,
            "model_choice_not_supported"
        );
        let rules = Rules {
            pick: PickRule::Strict,
            pick_version: 0,
            fallback: FallbackRule::Notice,
            fallback_version: 0,
        };
        assert!(
            rule_written(RuleWritten::Saved {
                rules: rules.clone()
            })
            .is_ok()
        );
        assert_eq!(
            rule_written(RuleWritten::Stale { rules }).unwrap_err().code,
            "model_choice_stale"
        );
    }

    /// The window names a slot by its word, sends one bounded id or a list of at most
    /// `ALLOWED_MAX` bounded ids, never a null, and names each rule value; anything else is
    /// refused before the engine is asked.
    #[test]
    fn the_window_sends_a_named_slot_a_bounded_value_and_a_named_rule() {
        for slot in Slot::ALL {
            assert_eq!(window_slot(slot.as_str()).unwrap(), slot);
        }
        for word in ["", "Master", "boss", "allowed "] {
            assert_eq!(
                window_slot(word).unwrap_err().code,
                "model_choice_request_invalid"
            );
        }
        let longest = "a".repeat(MODEL_BYTES_MAX);
        let full: Vec<String> = (0..ALLOWED_MAX).map(|n| format!("m{n}")).collect();
        for (slot, value) in [
            (Slot::Master, json!("opus")),
            (Slot::Agent, json!(longest)),
            (Slot::Allowed, json!([])),
            (Slot::Allowed, json!(full)),
        ] {
            assert_eq!(window_value(slot, value.clone()).unwrap(), value);
        }
        let over: Vec<String> = (0..=ALLOWED_MAX).map(|n| format!("m{n}")).collect();
        for (slot, value) in [
            (Slot::Master, Value::Null),
            (Slot::Master, json!("")),
            (Slot::Agent, json!("a".repeat(MODEL_BYTES_MAX + 1))),
            (Slot::Agent, json!(["opus"])),
            (Slot::Allowed, json!("opus")),
            (Slot::Allowed, json!(over)),
            (Slot::Allowed, json!([1])),
            (Slot::Allowed, json!([""])),
        ] {
            assert_eq!(
                window_value(slot, value.clone()).unwrap_err().code,
                "model_choice_value_invalid",
                "{slot:?} {value}"
            );
        }
        assert_eq!(window_model("my-model".into()).unwrap(), "my-model");
        for model in [String::new(), "a".repeat(MODEL_BYTES_MAX + 1)] {
            assert_eq!(
                window_model(model).unwrap_err().code,
                "model_choice_value_invalid"
            );
        }
        assert_eq!(
            window_rule("pick", "hint").unwrap(),
            Rule::Pick(PickRule::Hint)
        );
        assert_eq!(
            window_rule("fallback", "silent").unwrap(),
            Rule::Fallback(FallbackRule::Silent)
        );
        for (rule, value) in [("pick", "notice"), ("fallback", "hint"), ("Pick", "hint")] {
            assert_eq!(
                window_rule(rule, value).unwrap_err().code,
                "model_choice_request_invalid"
            );
        }
        assert_eq!(
            window_harness("Claude".into()).unwrap_err().code,
            "model_choice_request_invalid"
        );
        assert_eq!(
            window_project(Some(String::new())).unwrap_err().code,
            "model_choice_project_unknown"
        );
    }

    /// A section with every slot once in order joins the shipped list's versions and
    /// references; a section that drops, repeats or reorders a slot, names another harness
    /// or lists too many ids is refused.
    #[test]
    fn a_section_must_hold_every_slot_once_in_order_for_the_harness_asked() {
        let models = |rows: Vec<Row>, harness: &str, candidates: usize| Models {
            harness: harness.into(),
            rows,
            candidates: (0..candidates)
                .map(|n| Candidate {
                    id: format!("m{n}"),
                    origin: Origin::Catalog,
                })
                .collect(),
            qualified: true,
            reason: None,
        };
        let all = || Slot::ALL.into_iter().map(row).collect::<Vec<_>>();
        let page = page_of(models(all(), "claude-code", 2), "claude-code", None).unwrap();
        let shipped = shipped("claude-code").unwrap();
        assert_eq!(page.qualified_versions, shipped.qualified_versions);
        assert_eq!(page.sources, shipped.sources);
        let other = page_of(models(all(), "opencode", 0), "opencode", Some("p".into())).unwrap();
        assert!(other.qualified_versions.is_empty() && other.sources.is_empty());
        assert_eq!(other.project_id.as_deref(), Some("p"));
        let mut swapped = all();
        swapped.swap(0, 1);
        for broken in [
            models(all()[..2].to_vec(), "claude-code", 0),
            models(swapped, "claude-code", 0),
            models(all(), "codex", 0),
            models(all(), "claude-code", CATALOG_MODELS_MAX + CUSTOM_MAX + 1),
            Models {
                qualified: false,
                reason: Some("x".repeat(UNQUALIFIED_REASON_BYTES_MAX + 1)),
                ..models(all(), "claude-code", 0)
            },
        ] {
            assert_eq!(
                page_of(broken, "claude-code", None).unwrap_err().code,
                "owner_read_failed"
            );
        }
    }
}
