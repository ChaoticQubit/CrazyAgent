use crate::app_plugins::manifest::{Manifest, Runs};
use crate::app_plugins::{Problem, RunsView};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

pub mod local;
pub mod settings;
pub mod vectors;

/// The lowercase hex SHA-256 of some bytes. Input: the bytes. Output: 64 hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    use std::fmt::Write;
    let digest = sha2::Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String does not fail");
    }
    assert_eq!(hex.len(), 64);
    hex
}

/// The wall clock in milliseconds since the Unix epoch, 0 before it.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
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

/// How far the chosen plugin has embedded the indexed sessions and memories, for the
/// Search settings page. Inputs: the connection and the engine root. Output: the counts;
/// this stub reports none until the L3 vectors (feat/v3-hybrid) land.
pub fn progress(connection: &Connection, root: &Path) -> Progress {
    let _ = (connection, root);
    Progress {
        embedded: 0,
        total: 0,
    }
}

/// The search order after fusing full text with vectors: chunk ids best first, and a note
/// the answer carries when the vector part could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hybrid {
    pub ids: Vec<i64>,
    pub note: Option<&'static str>,
}

/// The most texts one call to the built-in local runtime embeds.
const LOCAL_BATCH: usize = 64;
/// How long a local runtime whose open failed waits before the next open, since every open
/// re-hashes the downloaded files.
const LOCAL_RETRY_MS: i64 = 10 * 60 * 1000;

/// The local runtime opened for one engine root, or the time its last open or embed failed.
type LocalSlot = Option<(PathBuf, Result<Arc<local::LocalModel>, i64>)>;
static LOCAL: Mutex<LocalSlot> = Mutex::new(None);

/// The plugin as the vectors see it: its key, dimensions and batch, the built-in runtime's
/// from [`local::DIMS`]. Input: the loaded manifest. Output: the plugin.
fn plugin_of(manifest: &Manifest) -> vectors::Plugin {
    match &manifest.runs {
        Runs::Local { .. } => {
            vectors::Plugin::new(&manifest.name, &manifest.sha256, local::DIMS, LOCAL_BATCH)
        }
        Runs::Http(http) => vectors::Plugin::new(
            &manifest.name,
            &manifest.sha256,
            usize::from(http.dimensions),
            usize::from(http.batch),
        ),
    }
}

/// The local runtime already open for `root`, never opening it, so a search on the tick
/// never waits on the re-hash. Input: the engine root. Output: the model, or none.
fn opened_local(root: &Path) -> Option<Arc<local::LocalModel>> {
    let slot = LOCAL.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_ref() {
        Some((at, Ok(model))) if at == root => Some(Arc::clone(model)),
        _ => None,
    }
}

/// Whether both pinned files of the local runtime are on disk at their pinned sizes, read
/// from metadata only. Input: the engine root. Output: the answer.
fn local_present(root: &Path) -> bool {
    let place = local::folder(root);
    local::PINNED.iter().all(|pin| {
        std::fs::symlink_metadata(place.join(pin.name))
            .is_ok_and(|found| found.is_file() && found.len() == pin.bytes)
    })
}

/// Opens `root`'s local runtime on the indexer thread, outside the lock, and at most once
/// per [`LOCAL_RETRY_MS`] after a failure. Inputs: the engine root and the time. Output:
/// the model, or none while it is missing or failing.
fn open_local(root: &Path, now_ms: i64) -> Option<Arc<local::LocalModel>> {
    {
        let slot = LOCAL.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some((at, Ok(model))) if at == root => return Some(Arc::clone(model)),
            Some((at, Err(failed))) if at == root && now_ms - failed < LOCAL_RETRY_MS => {
                return None;
            }
            _ => {}
        }
    }
    if !local_present(root) {
        return None;
    }
    let opened = local::LocalModel::open(root)
        .map(Arc::new)
        .map_err(|_| now_ms);
    let model = opened.as_ref().ok().cloned();
    *LOCAL.lock().unwrap_or_else(PoisonError::into_inner) = Some((root.to_owned(), opened));
    model
}

/// Marks `root`'s local runtime failed after an embed error, so it is opened again later.
/// Inputs: the engine root and the time. Output: none.
fn local_failed(root: &Path, now_ms: i64) {
    *LOCAL.lock().unwrap_or_else(PoisonError::into_inner) = Some((root.to_owned(), Err(now_ms)));
}

/// Embeds the project's chunks that have no vector for the chosen plugin yet, then the
/// app-wide memories, on the indexer thread outside any transaction and within
/// [`vectors::BOUNDS`]. No chosen plugin embeds nothing and keeps every vector (leader QB).
/// Inputs: the indexer's connection, the engine root and the project. Output: how many
/// chunks were embedded.
pub fn embed_pending(connection: &mut Connection, root: &Path, project: &ProjectId) -> usize {
    let Some(loaded) = settings::chosen(connection, root) else {
        return 0;
    };
    let plugin = plugin_of(&loaded.manifest);
    match &loaded.manifest.runs {
        Runs::Local { .. } => {
            let Some(model) = open_local(root, now_ms()) else {
                return 0;
            };
            let mut broke = false;
            let mut embed = |texts: &[&str]| {
                model.embed(texts).map_err(|code| {
                    broke = true;
                    EngineError(code.to_owned())
                })
            };
            let embedded =
                vectors::embed_round(connection, project, &plugin, &vectors::BOUNDS, &mut embed);
            if broke {
                local_failed(root, now_ms());
            }
            embedded
        }
        Runs::Http(_) => 0,
    }
}

/// Fuses the full-text order with the chosen plugin's vector order, on the tick and with
/// no network. Inputs: the connection, the engine root, the project, the source, the query
/// and the full-text chunk ids best first. Output: the fused ids and an optional note; this
/// stub keeps the full-text order.
pub fn hybrid(
    connection: &Connection,
    root: &Path,
    project: &ProjectId,
    source: crate::search_index::Source,
    query: &str,
    full_text: &[i64],
) -> Hybrid {
    let _ = (connection, root, project, source, query);
    Hybrid {
        ids: full_text.to_vec(),
        note: None,
    }
}
