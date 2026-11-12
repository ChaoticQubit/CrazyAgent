use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::harness_settings::{Access, HARNESS_BYTES_MAX, Scope, Source};
use crate::storage::versioned::{ITEM_BYTES_MAX, Item};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub mod store;
#[cfg(all(test, unix))]
mod tests;

/// The longest model id, in bytes.
pub const MODEL_BYTES_MAX: usize = 256;
/// The most model ids one harness's model catalog may give the store.
pub const CATALOG_MODELS_MAX: usize = 64;
/// The most model ids the owner may add to one harness.
pub const CUSTOM_MAX: usize = 32;
/// The most model ids one allowed list holds.
pub const ALLOWED_MAX: usize = 32;
/// The reason a slot shows when CrazyAgent cannot choose that harness's model there.
pub const NOT_SUPPORTED: &str = "Not supported by this harness";
/// Every refusal code the model choice store returns.
pub const MODEL_CHOICE_CODES: &[&str] = &[
    "model_choice_harness_unknown",
    "model_choice_project_unknown",
    "model_choice_value_invalid",
    "model_choice_not_supported",
    "model_choice_custom_full",
    "model_choice_custom_not_pending",
    "model_choice_unavailable",
];

const ITEM_PREFIX: &str = "model-choice";
const PROJECT_BYTES_MAX: usize = 128;
/// The longest stored value as JSON: a full allowed list of the longest ids.
pub(crate) const VALUE_JSON_MAX: usize = ALLOWED_MAX * (MODEL_BYTES_MAX + 3) + 2;
const CANDIDATE_JSON_MAX: usize = MODEL_BYTES_MAX + 64;
const ROW_JSON_MAX: usize = 2 * VALUE_JSON_MAX + 512;

const _: () = assert!(
    ITEM_PREFIX.len() + HARNESS_BYTES_MAX + ":project:".len() + PROJECT_BYTES_MAX + 1 + 8
        <= ITEM_BYTES_MAX
);
const _: () = assert!(
    (CATALOG_MODELS_MAX + CUSTOM_MAX) * CANDIDATE_JSON_MAX + 3 * ROW_JSON_MAX + 1024
        <= crate::protocol::FRAME_BYTES_MAX
);

/// One model choice of a harness at one scope: the model the master runs on, the default
/// model of the agents it starts, and the models the master may pick from for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Master,
    Agent,
    Allowed,
}

impl Slot {
    /// Every slot in page order.
    pub const ALL: [Slot; 3] = [Slot::Master, Slot::Agent, Slot::Allowed];

    /// Names the slot as the store and the owner protocol write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Master => "master",
            Slot::Agent => "agent",
            Slot::Allowed => "allowed",
        }
    }
}

/// Where one candidate model id comes from: the harness's model catalog, or an id the
/// owner added, waiting for its test launch, passed or failed. Only catalog and verified
/// ids may be set, ticked or launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Catalog,
    Pending,
    Verified,
    Failed,
}

impl Origin {
    /// Tells whether an id of this origin may be chosen and launched.
    pub fn usable(self) -> bool {
        matches!(self, Origin::Catalog | Origin::Verified)
    }
}

/// One model id the owner may see for a harness, with where it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub origin: Origin,
}

/// One slot on a harness's Models section at one scope: the value set at this scope, if
/// any, the value a start uses and where it comes from, the version a set or reset names,
/// and whether the owner may change it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub slot: Slot,
    pub here: Option<Value>,
    pub effective: Value,
    pub source: Source,
    pub version: u64,
    pub access: Access,
}

/// A harness's Models section at one scope: its three slots in page order and every
/// candidate id, the catalog's first, then the owner's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    pub harness: String,
    pub rows: Vec<Row>,
    pub candidates: Vec<Candidate>,
}

/// What a set or reset did: saved as the next version, refused as stale with the current
/// row, or refused with a code and the row whose access shows the reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Written {
    Saved { row: Row },
    Stale { row: Row },
    Refused { code: String, row: Row },
}

/// Checks one model id before it can reach a command line or a `-c model="..."` line:
/// 1 to `MODEL_BYTES_MAX` bytes, starting with an ASCII letter or digit, then letters,
/// digits and `-_.:/@+`; Claude Code ids may also hold `[` and `]`, as in `opus[1m]`.
/// Inputs: the catalog harness id and the model id. Output: true when it may be used.
pub fn model_valid(harness: &str, model: &str) -> bool {
    let brackets = harness == "claude-code";
    model.len() <= MODEL_BYTES_MAX
        && model
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || b"-_.:/@+".contains(&byte)
                || (brackets && b"[]".contains(&byte))
        })
}

/// Lists the model ids a harness's model catalog offers, the candidates the store checks
/// every choice against beside the owner's verified ids. None are listed until the catalog
/// source is settled (DEC-150 Q1), so only ids the owner added and a test launch passed
/// are offered. Input: the harness name. Output: the ids, valid and distinct.
pub fn catalog(harness: &str) -> Vec<String> {
    let ids: Vec<String> = Vec::new();
    if let Some(id) = harness_id(harness) {
        assert_catalog(id, &ids);
    }
    ids
}

/// Names the catalog harness id of a harness name or alias, such as `claude` for
/// `claude-code`. Input: the name. Output: the id, none for an unknown harness.
pub fn harness_id(name: &str) -> Option<&'static str> {
    crate::harnesses::spec(name).map(|spec| spec.id)
}

/// Tells whether CrazyAgent chooses this harness's model in one slot: the master runs on
/// Claude Code and Codex with a chosen model, and team agents start only on those two.
/// Inputs: the catalog harness id and the slot. Output: the answer.
pub fn supported(harness: &str, slot: Slot) -> bool {
    match slot {
        Slot::Master => matches!(harness, "claude-code" | "codex"),
        Slot::Agent | Slot::Allowed => crate::team::launch_harness(harness).is_some(),
    }
}

/// Checks one value for a slot: a usable model id for the master and agent slots, a list
/// of at most `ALLOWED_MAX` distinct usable ids for the allowed slot. Inputs: the harness
/// id, the slot, the value and the usable ids. Output: true when the value may be saved.
pub(crate) fn value_ok(harness: &str, slot: Slot, value: &Value, usable: &[String]) -> bool {
    let known = |id: &str| model_valid(harness, id) && usable.iter().any(|one| one == id);
    match slot {
        Slot::Master | Slot::Agent => value.as_str().is_some_and(known),
        Slot::Allowed => value.as_array().is_some_and(|ids| {
            let mut seen = BTreeSet::new();
            ids.len() <= ALLOWED_MAX
                && ids
                    .iter()
                    .all(|id| id.as_str().is_some_and(|id| known(id) && seen.insert(id)))
        }),
    }
}

/// The value a slot falls back to when nothing usable is set: null for a model, so the
/// account's own model runs, and an empty list for the allowed slot.
pub(crate) fn fallback(slot: Slot) -> Value {
    match slot {
        Slot::Master | Slot::Agent => Value::Null,
        Slot::Allowed => Value::Array(Vec::new()),
    }
}

/// Picks one slot's value: the project's, then the app's, then the fallback. A model the
/// harness no longer offers is passed over; an allowed list keeps only its usable ids.
/// Inputs: the harness id, the slot, the usable ids and the stored app and project values.
/// Output: the value and where it comes from.
pub(crate) fn layered(
    harness: &str,
    slot: Slot,
    usable: &[String],
    app: Option<&Value>,
    project: Option<&Value>,
) -> (Value, Source) {
    let kept = |value: &Value| -> Option<Value> {
        match slot {
            Slot::Master | Slot::Agent => {
                value_ok(harness, slot, value, usable).then(|| value.clone())
            }
            Slot::Allowed => {
                let mut seen = BTreeSet::new();
                let ids: Vec<Value> = value
                    .as_array()?
                    .iter()
                    .filter(|id| {
                        id.as_str().is_some_and(|id| {
                            model_valid(harness, id)
                                && usable.iter().any(|one| one == id)
                                && seen.insert(id.to_owned())
                        })
                    })
                    .take(ALLOWED_MAX)
                    .cloned()
                    .collect();
                Some(Value::Array(ids))
            }
        }
    };
    if let Some(value) = project.and_then(kept) {
        return (value, Source::Project);
    }
    if let Some(value) = app.and_then(kept) {
        return (value, Source::App);
    }
    (fallback(slot), Source::Default)
}

/// Names the versioned item of one slot at one scope. Inputs: a catalog harness id, the
/// scope and the slot. Output: the item, or `versioned_item_invalid`.
pub(crate) fn item(harness: &str, scope: &Scope, slot: Slot) -> Result<Item, EngineError> {
    assert!(!harness.is_empty() && harness.len() <= HARNESS_BYTES_MAX);
    let slot = slot.as_str();
    Item::new(&match scope {
        Scope::App => format!("{ITEM_PREFIX}:{harness}:app:{slot}"),
        Scope::Project(project) => {
            format!(
                "{ITEM_PREFIX}:{harness}:project:{}:{slot}",
                project.as_str()
            )
        }
    })
}

/// Checks the model catalog a caller passes: at most `CATALOG_MODELS_MAX` distinct ids,
/// each valid for the harness. The caller filters what it reads, so a bad catalog is a
/// broken caller. Inputs: the harness id and the ids.
pub(crate) fn assert_catalog(harness: &str, catalog: &[String]) {
    assert!(catalog.len() <= CATALOG_MODELS_MAX, "{harness}");
    let mut seen = BTreeSet::new();
    for id in catalog {
        assert!(model_valid(harness, id), "{harness}: {id}");
        assert!(seen.insert(id.as_str()), "{harness}: {id}");
    }
}

pub(crate) fn unavailable<E>(_: E) -> EngineError {
    failure("model_choice_unavailable")
}
