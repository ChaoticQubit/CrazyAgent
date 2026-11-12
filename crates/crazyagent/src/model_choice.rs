use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::harness_settings::{
    Access, HARNESS_BYTES_MAX, SOURCE_BYTES_MAX, SOURCES_MAX, Scope, Source, VERSIONS_MAX,
};
use crate::storage::versioned::{ITEM_BYTES_MAX, Item};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub mod rules;
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

/// The model catalogs the engine ships, by harness id, each a version-qualified JSON
/// document built from the harness's published model reference (DEC-150 Q1).
const SHIPPED: &[(&str, &str)] = &[
    (
        "claude-code",
        include_str!("../assets/model-catalogs/claude-code.json"),
    ),
    ("codex", include_str!("../assets/model-catalogs/codex.json")),
];
const CATALOG_JSON_MAX: usize = 64 * 1024;
const LONG_CONTEXT_SUFFIX: &str = "[1m]";
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

/// Checks one model id before it can reach a command line or a `-c model="..."` line, the
/// one rule the store saves by and every agent launch starts by: at most `MODEL_BYTES_MAX`
/// bytes, an ASCII letter or digit first, then letters, digits and `-_.:/@+`. A Claude
/// Code id, passed as its own argument, may end in the `[1m]` long-context suffix its
/// catalog lists, as in `opus[1m]`; no other brackets pass. Inputs: the catalog harness
/// id and the model id. Output: true when it may be used.
pub fn model_valid(harness: &str, model: &str) -> bool {
    let base = match harness {
        "claude-code" => model.strip_suffix(LONG_CONTEXT_SUFFIX).unwrap_or(model),
        _ => model,
    };
    model.len() <= MODEL_BYTES_MAX
        && base
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && base
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
}

/// One harness's shipped model catalog: the harness, the exact versions it was checked
/// against, the published references it was built from, and its model ids in page order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalog {
    pub harness: String,
    pub qualified_versions: Vec<String>,
    pub sources: Vec<String>,
    pub models: Vec<String>,
}

/// Lists the model ids a harness's shipped catalog offers, the candidates the store checks
/// every choice against beside the owner's verified ids (DEC-150 Q1); a live read of the
/// harness's own list comes later. Input: the harness name or alias. Output: the ids,
/// valid and distinct, none for a harness without a catalog.
pub fn catalog(harness: &str) -> Vec<String> {
    let ids = shipped(harness)
        .map(|catalog| catalog.models)
        .unwrap_or_default();
    if let Some(id) = harness_id(harness) {
        assert_catalog(id, &ids);
    }
    ids
}

/// Reads the shipped model catalog of a harness name or alias. A shipped catalog out of
/// contract is a broken build. Input: the name. Output: the catalog, none for a harness
/// without one.
pub fn shipped(name: &str) -> Option<ModelCatalog> {
    let id = harness_id(name)?;
    let (_, text) = SHIPPED.iter().find(|(harness, _)| *harness == id)?;
    let catalog = parse_catalog(text.as_bytes());
    assert!(
        catalog
            .as_ref()
            .is_some_and(|catalog| catalog.harness == id),
        "{id}"
    );
    catalog
}

/// Reads one model catalog and checks it against the contract: a harness CrazyAgent starts
/// agents on, 1 to `VERSIONS_MAX` exact versions, 1 to `SOURCES_MAX` https sources and 1
/// to `CATALOG_MODELS_MAX` distinct ids valid for the harness. Input: the JSON bytes.
/// Output: the catalog, or none when it breaks the contract.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Option<ModelCatalog> {
    if bytes.len() > CATALOG_JSON_MAX {
        return None;
    }
    let catalog: ModelCatalog = serde_json::from_slice(bytes).ok()?;
    let harness = catalog.harness.as_str();
    let known = harness_id(harness) == Some(harness) && supported(harness, Slot::Agent);
    let mut versions_seen = BTreeSet::new();
    let versions = (1..=VERSIONS_MAX).contains(&catalog.qualified_versions.len())
        && catalog.qualified_versions.iter().all(|version| {
            crate::harness_settings::installed::version_valid(version)
                && versions_seen.insert(version.as_str())
        });
    let sources = (1..=SOURCES_MAX).contains(&catalog.sources.len())
        && catalog.sources.iter().all(|source| {
            source.len() <= SOURCE_BYTES_MAX
                && source.starts_with("https://")
                && source.bytes().all(|byte| byte.is_ascii_graphic())
        });
    let mut models_seen = BTreeSet::new();
    let models = (1..=CATALOG_MODELS_MAX).contains(&catalog.models.len())
        && catalog
            .models
            .iter()
            .all(|id| model_valid(harness, id) && models_seen.insert(id.as_str()));
    (known && versions && sources && models).then_some(catalog)
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
