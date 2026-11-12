use super::{CATALOG_MODELS_MAX, harness_id, model_valid};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

#[cfg(all(test, unix))]
mod tests;

/// How long one harness has to print its model list (DEC-150 addendum 11).
pub const LIMIT: Duration = Duration::from_secs(5);
/// The most bytes of one harness's model list the engine keeps; Codex prints about 750 KB.
pub const OUTPUT_BYTES_MAX: usize = 2 * 1024 * 1024;
/// What a Models section says, before the harness name, while it shows no list the
/// harness reported itself.
pub const NOT_REPORTED: &str = "live list not reported by";
/// The longest line a Models section may show for an unreported list; a client refuses a
/// section with a longer one.
pub const NOTE_BYTES_MAX: usize = 128;

/// The folder under the engine root that holds each read's private home.
#[cfg(unix)]
const HOMES: &str = "model-lists";
/// Every variable a harness finds its home, config, cache or temporary files by, each set to
/// the read's private home.
#[cfg(unix)]
const HOME_VARIABLES: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "TMPDIR",
];
#[cfg(unix)]
const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// How a harness prints its model list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Codex's raw catalog: JSON `models[]` with `slug` and `visibility`, only "list" shown.
    CodexCatalog,
    /// One model id per line, nothing else on any line.
    Lines,
    /// pi's table: a `provider  model ...` header, then one row per model.
    PiTable,
    /// aider's list: `- <id>` lines under its banner.
    Dashed,
    /// JSON: an array, or an object's `models` array, of ids or objects naming one.
    JsonIds,
}

/// The arguments that make one harness print its model list without an account, and how
/// it prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reader {
    pub args: &'static [&'static str],
    pub shape: Shape,
}

/// Every harness with a non-interactive list command (M-16 research); no other harness
/// is ever started for its list.
const READERS: &[(&str, Reader)] = &[
    (
        "codex",
        Reader {
            args: &["debug", "models", "--bundled"],
            shape: Shape::CodexCatalog,
        },
    ),
    (
        "opencode",
        Reader {
            args: &["models"],
            shape: Shape::Lines,
        },
    ),
    (
        "openclaw",
        Reader {
            args: &["models", "list", "--plain"],
            shape: Shape::Lines,
        },
    ),
    (
        "crush",
        Reader {
            args: &["models"],
            shape: Shape::Lines,
        },
    ),
    (
        "cursor-agent",
        Reader {
            args: &["models"],
            shape: Shape::Lines,
        },
    ),
    (
        "antigravity",
        Reader {
            args: &["models"],
            shape: Shape::Lines,
        },
    ),
    (
        "gemini-cli",
        Reader {
            args: &["models", "list"],
            shape: Shape::Lines,
        },
    ),
    (
        "pi",
        Reader {
            args: &["--list-models"],
            shape: Shape::PiTable,
        },
    ),
    (
        "aider",
        Reader {
            args: &["--list-models", ""],
            shape: Shape::Dashed,
        },
    ),
    (
        "kiro",
        Reader {
            args: &["chat", "--list-models", "--format", "json"],
            shape: Shape::JsonIds,
        },
    ),
    (
        "auggie",
        Reader {
            args: &["models", "list", "--json"],
            shape: Shape::JsonIds,
        },
    ),
];

/// Names how a harness prints its model list. Input: the harness name or alias. Output:
/// the reader, none for a harness with no non-interactive list command.
pub fn reader(harness: &str) -> Option<Reader> {
    let id = harness_id(harness)?;
    READERS
        .iter()
        .find(|(one, _)| *one == id)
        .map(|(_, reader)| *reader)
}

/// The line a Models section shows while it lists no model the harness reported itself,
/// word for word in the desktop and the TUI (DEC-141). Input: the harness name or alias.
/// Output: the line.
pub fn not_reported(harness: &str) -> String {
    let spec = crate::harnesses::spec(harness);
    let name = spec.map_or(harness, |spec| spec.name);
    let line = format!("{NOT_REPORTED} {name}");
    assert!(spec.is_none() || line.len() <= NOTE_BYTES_MAX);
    line
}

/// Reads the model ids one harness printed. Text shapes are strict: a line that is not
/// exactly one valid id makes the whole output unreported, so usage or sign-in text never
/// turns into ids. Inputs: the harness, its shape and the output. Output: at most
/// `CATALOG_MODELS_MAX` distinct valid ids in the harness's order, none when it reported
/// none.
pub fn parse(harness: &str, shape: Shape, output: &[u8]) -> Option<Vec<String>> {
    let harness = harness_id(harness)?;
    let text = std::str::from_utf8(output).ok()?;
    let found = match shape {
        Shape::CodexCatalog => codex_ids(text)?,
        Shape::Lines => line_ids(harness, text)?,
        Shape::PiTable => pi_ids(harness, text)?,
        Shape::Dashed => dashed_ids(harness, text)?,
        Shape::JsonIds => json_ids(text)?,
    };
    let mut seen = BTreeSet::new();
    let ids: Vec<String> = found
        .into_iter()
        .filter(|id| model_valid(harness, id) && seen.insert(id.clone()))
        .take(CATALOG_MODELS_MAX)
        .collect();
    super::assert_catalog(harness, &ids);
    (!ids.is_empty()).then_some(ids)
}

fn codex_ids(text: &str) -> Option<Vec<String>> {
    let catalog: Value = serde_json::from_str(text).ok()?;
    let models = catalog.get("models")?.as_array()?;
    Some(
        models
            .iter()
            .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
            .filter_map(|model| model.get("slug").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
    )
}

fn line_ids(harness: &str, text: &str) -> Option<Vec<String>> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| one_id(harness, line))
        .collect()
}

fn pi_ids(harness: &str, text: &str) -> Option<Vec<String>> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let mut header = lines.next()?.split_whitespace();
    if header.next() != Some("provider") || header.next() != Some("model") {
        return None;
    }
    lines
        .map(|line| {
            let mut columns = line.split_whitespace();
            let provider = columns.next()?;
            let model = columns.next()?;
            one_id(harness, &format!("{provider}/{model}"))
        })
        .collect()
}

fn dashed_ids(harness: &str, text: &str) -> Option<Vec<String>> {
    text.lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|rest| one_id(harness, rest.trim()))
        .collect()
}

fn json_ids(text: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let items = match &value {
        Value::Array(items) => items,
        Value::Object(object) => object.get("models")?.as_array()?,
        _ => return None,
    };
    Some(
        items
            .iter()
            .filter_map(|item| match item {
                Value::String(id) => Some(id.as_str()),
                Value::Object(object) => ["id", "model_id", "modelId", "slug"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_str)),
                _ => None,
            })
            .map(str::to_owned)
            .collect(),
    )
}

/// Takes one word that is a whole valid model id. Inputs: the harness id and the word.
/// Output: the id, none for an empty word, several words or an invalid id.
fn one_id(harness: &str, word: &str) -> Option<String> {
    (!word.is_empty() && !word.contains(char::is_whitespace) && model_valid(harness, word))
        .then(|| word.to_owned())
}

/// Joins a harness's shipped list and the ids it reported (DEC-150 addendum 11 Q4): the
/// shipped ids first in their order, then each reported id the shipped list lacks, every
/// id valid for the harness and distinct, cut at `CATALOG_MODELS_MAX`. Inputs: the harness
/// id, the shipped ids and the reported ids. Output: the catalog.
pub fn merged(harness: &str, shipped: &[String], live: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let joined: Vec<String> = shipped
        .iter()
        .chain(live)
        .filter(|id| model_valid(harness, id) && seen.insert(id.as_str()))
        .take(CATALOG_MODELS_MAX)
        .cloned()
        .collect();
    super::assert_catalog(harness, &joined);
    joined
}

/// Reads one harness's model list once, through the tool resolver (Q1 a): the registered
/// file goes through `launch_tool`, then [`read_with`] runs it signed out in a private
/// home. Inputs: the harness id, its registered executable and the engine root. Output:
/// the reported ids, none when it has no reader, cannot be resolved or reported none.
#[cfg(unix)]
pub(crate) fn read(harness: &str, executable: &Path, root: &Path) -> Option<Vec<String>> {
    reader(harness)?;
    let places = crate::tools::Places::from_environment(Some(root));
    let tool = crate::harnesses::interactive::launch_tool(executable, &places).ok()?;
    read_with(harness, &tool, root, LIMIT)
}

/// Reports no live list where the engine cannot run a harness in a private home with its
/// own process group. Inputs: as on unix. Output: none.
#[cfg(not(unix))]
pub(crate) fn read(harness: &str, executable: &Path, root: &Path) -> Option<Vec<String>> {
    let _ = (harness, executable, root, LIMIT);
    None
}

/// Runs one resolved harness for its model list in a fresh 0700 home under the engine
/// root: env cleared but for that home and a system PATH, no input, its own process group
/// killed at exit or at the limit, at most `OUTPUT_BYTES_MAX` bytes, the home removed
/// after. Inputs: the harness id, the resolved tool, the engine root and the limit.
/// Output: the reported ids, or none.
#[cfg(unix)]
pub(crate) fn read_with(
    harness: &str,
    tool: &crate::tools::Resolved,
    root: &Path,
    limit: Duration,
) -> Option<Vec<String>> {
    assert!(!limit.is_zero());
    let reader = reader(harness)?;
    let argv = tool.argv();
    let (program, words) = argv.split_first()?;
    let home = crate::setup::process::staging_folder(&root.join(HOMES), "home").ok()?;
    let mut command = std::process::Command::new(program);
    command
        .args(words)
        .args(reader.args)
        .env_clear()
        .env("PATH", SYSTEM_PATH)
        .current_dir(&home);
    for name in HOME_VARIABLES {
        command.env(name, &home);
    }
    let output = crate::tools::probe::run_bounded(&mut command, limit, OUTPUT_BYTES_MAX);
    let _ = std::fs::remove_dir_all(&home);
    parse(harness, reader.shape, &output?)
}
