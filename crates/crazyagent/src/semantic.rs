use crate::app_plugins::{Problem, RunsView};
use serde::{Deserialize, Serialize};

pub mod local;
pub mod settings;

/// The wall clock in milliseconds since the Unix epoch, 0 before it.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

/// What agents' search ranks with: full text only, or full text fused with the vectors of
/// one enabled embedding plugin (DEC-156). An HTTP plugin is never chosen on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchChoice {
    FullText,
    Plugin { name: String },
}

/// One embedding plugin the Search settings can choose: its name and title, how it runs,
/// whether it can be used now with the plain reason when not, and, for an HTTP plugin,
/// what leaves this computer for its host once chosen (empty for the local runtime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOption {
    pub name: String,
    pub title: String,
    pub runs: RunsView,
    pub usable: bool,
    pub reason: Option<String>,
    pub sends: Vec<String>,
}

/// The built-in local model's one-time download: not on this computer (with its size),
/// downloading, ready, or failed with the plain reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelState {
    NotDownloaded { size_bytes: u64 },
    Downloading { done_bytes: u64, size_bytes: u64 },
    Ready { size_bytes: u64 },
    Failed { problem: Problem, size_bytes: u64 },
}

/// How far the chosen plugin has embedded the indexed sessions and memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub embedded: u64,
    pub total: u64,
}

/// The Search settings page: the saved choice at its version, every choosable plugin, the
/// local model's download state and the embedding progress of the choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchView {
    pub version: u64,
    pub choice: SearchChoice,
    pub options: Vec<SearchOption>,
    pub model: ModelState,
    pub progress: Progress,
}

/// What an HTTP embedding plugin sends to its host once chosen, as the confirm lists it
/// (DEC-156 addendum, leader Q6).
pub const SENDS: [&str; 4] = [
    "Session text",
    "Tool inputs and outputs",
    "Memories",
    "Search queries",
];
