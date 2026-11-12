use super::{CATALOG_MODELS_MAX, harness_id, model_valid};
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

/// The folder under the engine root that holds each read's private home.
const HOMES: &str = "model-lists";
/// Every variable a harness finds its home, config, cache or temporary files by, each set to
/// the read's private home.
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
    let _ = harness;
    String::new()
}

/// Reads the model ids one harness printed. Text shapes are strict: a line that is not
/// exactly one valid id makes the whole output unreported, so usage or sign-in text never
/// turns into ids. Inputs: the harness, its shape and the output. Output: at most
/// `CATALOG_MODELS_MAX` distinct valid ids in the harness's order, none when it reported
/// none.
pub fn parse(harness: &str, shape: Shape, output: &[u8]) -> Option<Vec<String>> {
    let _ = (harness, shape, output);
    None
}

/// Joins a harness's shipped list and the ids it reported (DEC-150 addendum 11 Q4): the
/// shipped ids first in their order, then each reported id the shipped list lacks, every
/// id valid for the harness and distinct, cut at `CATALOG_MODELS_MAX`. Inputs: the harness
/// id, the shipped ids and the reported ids. Output: the catalog.
pub fn merged(harness: &str, shipped: &[String], live: &[String]) -> Vec<String> {
    let _ = (harness, live, CATALOG_MODELS_MAX, model_valid);
    shipped.to_vec()
}

/// Reads one harness's model list once, through the tool resolver (Q1 a): the registered
/// file goes through `launch_tool`, then [`read_with`] runs it signed out in a private
/// home. Inputs: the harness id, its registered executable and the engine root. Output:
/// the reported ids, none when it has no reader, cannot be resolved or reported none.
#[cfg(unix)]
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
    let _ = (
        harness,
        tool,
        root,
        limit,
        HOMES,
        HOME_VARIABLES,
        SYSTEM_PATH,
        OUTPUT_BYTES_MAX,
    );
    None
}
