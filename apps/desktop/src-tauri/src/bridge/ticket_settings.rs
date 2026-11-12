use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    OwnerClient, SettingChoices, SettingsSaved, TRACKER_CODES, TicketSettingsView,
};
use crazyagent::protocol::ProjectId;
use crazyagent::tracker::settings::{TicketOverride, TicketSettings};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Every ticket setting in the order the screens show it: the tracker policy keys, then who
/// may record manual and CI check results.
pub const TICKET_KEYS: [&str; 8] = [
    "close",
    "create",
    "agent_edit",
    "cycle_days",
    "rollover",
    "import",
    "manual_evidence",
    "ci_evidence",
];
const OPTIONS_MAX: usize = 8;
const CYCLE_DAYS_MAX: u64 = 90;
const VALUES_BYTES_MAX: usize = 1024;

/// The bounds of a custom number of days, as the window shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CustomRange {
    pub min: u8,
    pub max: u8,
}

/// One ticket setting as the window shows it: every option in order, the proposed default,
/// the custom bounds (`cycle_days` only), the value this scope sets (null when it takes the
/// scope below), the value below and the value in force.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketSettingRow {
    pub key: &'static str,
    pub options: Vec<Value>,
    pub proposed: Value,
    pub custom: Option<CustomRange>,
    pub here: Value,
    pub below: Value,
    pub effective: Value,
}

/// One scope's ticket settings as the window shows them: the project (None for the app),
/// the version a save sends back and every setting in `TICKET_KEYS` order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketSettingsPage {
    pub project_id: Option<String>,
    pub version: u64,
    pub keys: Vec<TicketSettingRow>,
}

/// The bridge failure for an engine tracker error: a known code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it. Input: the
/// engine error. Output: the bridge failure.
pub fn ticket_settings_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        TRACKER_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("tracker_outcome_unknown"),
        &error.0,
    )
}

fn unreadable() -> BridgeError {
    failure("owner_read_failed")
}

/// Reads what a save did: Ok once saved, `tracker_settings_stale` when the scope changed
/// since the page was read and nothing was written. Input: the engine's outcome. Output:
/// the window's outcome.
pub fn saved(saved: SettingsSaved) -> Result<(), BridgeError> {
    match saved {
        SettingsSaved::Saved { .. } => Ok(()),
        SettingsSaved::Stale { .. } => Err(failure("tracker_settings_stale")),
    }
}

/// A project id as the window sends it, None for the app. Input: the id. Output: the
/// project, or `tracker_request_invalid`.
pub fn window_project(project_id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    project_id
        .map(|id| ProjectId::new(id).map_err(|_| failure("tracker_request_invalid")))
        .transpose()
}

/// The values one scope saves, as the window sends them: an object of at most every ticket
/// key, each an option word of its key or a number of days, small enough to send. A key
/// left out takes the scope below; a null is no value and is refused. Input: the values.
/// Output: the values, or `tracker_request_invalid` or `policy_value_invalid`.
pub fn window_values(values: Value) -> Result<TicketOverride, BridgeError> {
    let Value::Object(map) = &values else {
        return Err(failure("tracker_request_invalid"));
    };
    let known = map.keys().all(|key| TICKET_KEYS.contains(&key.as_str()));
    let bytes = serde_json::to_vec(&values).map_or(usize::MAX, |bytes| bytes.len());
    if !known || map.len() > TICKET_KEYS.len() || bytes > VALUES_BYTES_MAX {
        return Err(failure("tracker_request_invalid"));
    }
    if !map
        .values()
        .all(|value| value.is_string() || value.is_u64())
    {
        return Err(failure("policy_value_invalid"));
    }
    serde_json::from_value(values).map_err(|_| failure("policy_value_invalid"))
}

fn object(value: Value) -> Result<Map<String, Value>, BridgeError> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(unreadable()),
    }
}

fn flat(settings: &TicketSettings) -> Result<Map<String, Value>, BridgeError> {
    let mut top = object(serde_json::to_value(settings).map_err(|_| unreadable())?)?;
    let mut flat = object(top.remove("policy").ok_or_else(unreadable)?)?;
    flat.extend(top);
    let every =
        flat.len() == TICKET_KEYS.len() && TICKET_KEYS.iter().all(|key| flat.contains_key(*key));
    every.then_some(flat).ok_or_else(unreadable)
}

fn set_here(values: &TicketOverride) -> Result<Map<String, Value>, BridgeError> {
    let here = object(serde_json::to_value(values).map_err(|_| unreadable())?)?;
    let known = here.keys().all(|key| TICKET_KEYS.contains(&key.as_str()));
    known.then_some(here).ok_or_else(unreadable)
}

fn allowed(choices: &SettingChoices, value: &Value) -> bool {
    choices.options.contains(value)
        || choices.custom.is_some_and(|custom| {
            value
                .as_u64()
                .is_some_and(|days| (u64::from(custom.min)..=u64::from(custom.max)).contains(&days))
        })
}

fn choices_fit(key: &str, choices: &SettingChoices) -> bool {
    let days = key == "cycle_days";
    let shaped = choices.options.iter().all(|option| {
        if days {
            option
                .as_u64()
                .is_some_and(|days| (1..=CYCLE_DAYS_MAX).contains(&days))
        } else {
            option.as_str().is_some_and(|word| !word.is_empty())
        }
    });
    let distinct = choices
        .options
        .iter()
        .map(Value::to_string)
        .collect::<BTreeSet<_>>()
        .len()
        == choices.options.len();
    let custom = choices.custom.is_none_or(|custom| {
        days && 1 <= custom.min
            && custom.min <= custom.max
            && u64::from(custom.max) <= CYCLE_DAYS_MAX
    });
    choices.key == key
        && (1..=OPTIONS_MAX).contains(&choices.options.len())
        && shaped
        && distinct
        && custom
        && allowed(choices, &choices.proposed)
}

fn row_of(
    key: &'static str,
    choices: SettingChoices,
    here: &Map<String, Value>,
    below: &Map<String, Value>,
    effective: &Map<String, Value>,
) -> Result<TicketSettingRow, BridgeError> {
    let here = here.get(key).cloned().unwrap_or(Value::Null);
    let below = below.get(key).cloned().ok_or_else(unreadable)?;
    let effective = effective.get(key).cloned().ok_or_else(unreadable)?;
    let in_force = if here.is_null() { &below } else { &here };
    let fits = choices_fit(key, &choices)
        && (here.is_null() || allowed(&choices, &here))
        && allowed(&choices, &below)
        && allowed(&choices, &effective)
        && effective == *in_force;
    if !fits {
        return Err(unreadable());
    }
    Ok(TicketSettingRow {
        key,
        custom: choices.custom.map(|custom| CustomRange {
            min: custom.min,
            max: custom.max,
        }),
        options: choices.options,
        proposed: choices.proposed,
        here,
        below,
        effective,
    })
}

/// Turns the engine's view of one scope into the window's page, refusing a view that does
/// not hold together: another scope than the one asked for, choices missing, doubled or out
/// of order, a value that is no option of its key, or values in force that are not this
/// scope's values over the scope below. Inputs: the view and the project id the window
/// asked for. Output: the page, or `owner_read_failed`.
pub fn page_of(
    view: TicketSettingsView,
    project_id: Option<String>,
) -> Result<TicketSettingsPage, BridgeError> {
    let same = view.project.as_ref().map(ProjectId::as_str) == project_id.as_deref();
    let order = view.choices.iter().map(|choices| choices.key.as_str());
    if !same || !order.eq(TICKET_KEYS) || view.values.over(&view.below) != view.effective {
        return Err(unreadable());
    }
    let here = set_here(&view.values)?;
    let below = flat(&view.below)?;
    let effective = flat(&view.effective)?;
    let keys = TICKET_KEYS
        .into_iter()
        .zip(view.choices)
        .map(|(key, choices)| row_of(key, choices, &here, &below, &effective))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TicketSettingsPage {
        project_id,
        version: view.version,
        keys,
    })
}

impl NativeBridge {
    fn ticket_settings_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(ticket_settings_error)
    }

    /// Reads the ticket settings of the app or one project, run without the bridge lock.
    /// Input: the project id, None for the app. Output: the page, or the engine's refusal,
    /// such as `project_not_attached`.
    pub fn ticket_settings(
        &self,
        project_id: Option<String>,
    ) -> Result<Unlocked<TicketSettingsPage>, BridgeError> {
        let project = window_project(project_id.clone())?;
        let client = self.ticket_settings_client()?;
        Ok(Box::pin(async move {
            let view = client
                .tracker_settings(project)
                .await
                .map_err(ticket_settings_error)?;
            page_of(view, project_id)
        }))
    }

    /// Saves one scope's ticket settings at the version the owner read, run without the
    /// bridge lock; empty values clear every key the scope set. Inputs: the project id, None
    /// for the app, the values and the version. Output: Ok once saved,
    /// `tracker_settings_stale` when the scope changed since, or the engine's refusal.
    pub fn save_ticket_settings(
        &self,
        project_id: Option<String>,
        values: Value,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = window_project(project_id)?;
        let values = window_values(values)?;
        let client = self.ticket_settings_client()?;
        Ok(Box::pin(async move {
            saved(
                client
                    .tracker_save_settings(project, values, version)
                    .await
                    .map_err(ticket_settings_error)?,
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracker_codes_pass_through_and_others_are_unknown() {
        assert_eq!(
            ticket_settings_error(EngineError("project_not_attached".into())).code,
            "project_not_attached"
        );
        let other = ticket_settings_error(EngineError("disk_on_fire".into()));
        assert_eq!(other.code, "tracker_outcome_unknown");
        assert_eq!(
            ticket_settings_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
    }

    #[test]
    fn window_values_take_only_ticket_keys_with_their_options() {
        assert_eq!(window_values(json!({})).unwrap(), TicketOverride::default());
        let set = window_values(json!({"close": "owner", "cycle_days": 10})).unwrap();
        assert!(set.close.is_some() && set.cycle_days.is_some() && set.rollover.is_none());
        for (values, code) in [
            (json!([]), "tracker_request_invalid"),
            (json!({"sandbox": "off"}), "tracker_request_invalid"),
            (json!({"close": "sometimes"}), "policy_value_invalid"),
            (json!({"cycle_days": 0}), "policy_value_invalid"),
            (json!({"cycle_days": 91}), "policy_value_invalid"),
            (json!({"cycle_days": "14"}), "policy_value_invalid"),
            (json!({"close": null}), "policy_value_invalid"),
        ] {
            assert_eq!(
                window_values(values.clone()).unwrap_err().code,
                code,
                "{values}"
            );
        }
        let long = json!({"close": "x".repeat(VALUES_BYTES_MAX)});
        assert_eq!(
            window_values(long).unwrap_err().code,
            "tracker_request_invalid"
        );
        assert_eq!(
            window_project(Some(String::new())).unwrap_err().code,
            "tracker_request_invalid"
        );
        assert_eq!(window_project(None).unwrap(), None);
    }

    #[test]
    fn a_stale_save_has_its_own_code() {
        assert!(saved(SettingsSaved::Saved { version: 3 }).is_ok());
        assert_eq!(
            saved(SettingsSaved::Stale { current: 4 }).unwrap_err().code,
            "tracker_settings_stale"
        );
    }

    #[test]
    fn the_flat_settings_name_every_ticket_key_once() {
        let keys = flat(&TicketSettings::default()).unwrap();
        let mut names: Vec<&str> = keys.keys().map(String::as_str).collect();
        let mut listed = TICKET_KEYS.to_vec();
        names.sort_unstable();
        listed.sort_unstable();
        assert_eq!(names, listed);
        assert_eq!(keys["cycle_days"], json!(14));
        assert_eq!(keys["close"], json!("evidence"));
    }
}
