use crate::app_plugins::manifest::{Manifest, Purpose, Runs};
use crate::app_plugins::{Problem, RunsView};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ops::Bound;
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

/// One embedding plugin the Search settings can choose: its name and title, how it runs
/// (none for a chosen plugin that is gone or refused), whether it can be used now with the
/// plain reason when not, and, for an HTTP plugin, what leaves this computer for its host
/// once chosen (empty for the local runtime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOption {
    pub name: String,
    pub title: String,
    pub runs: Option<RunsView>,
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

/// The Search settings page: the saved choice at its version, every choosable plugin, a
/// plain note when the choice fell back to full text, the local model's download state and
/// the embedding progress of the choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchView {
    pub version: u64,
    pub choice: SearchChoice,
    pub options: Vec<SearchOption>,
    pub note: Option<String>,
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
/// Search settings page. Inputs: the connection and the engine root. Output: the counts,
/// none of none for full text.
pub fn progress(connection: &Connection, root: &Path) -> Progress {
    settings::chosen(connection, root).map_or(
        Progress {
            embedded: 0,
            total: 0,
        },
        |loaded| vectors::progress(connection, &plugin_of(&loaded.manifest).key),
    )
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
/// The most document calls one project's turn sends to an HTTP plugin, the choice read
/// again before each, so turning the plugin off stops the next send.
const HTTP_CALLS_MAX: usize = 4;
/// The most projects waiting for the HTTP embedding thread.
const HTTP_WANTED_MAX: usize = 1_024;
/// How long a local runtime whose open failed waits before the next open, since every open
/// re-hashes the downloaded files.
const LOCAL_RETRY_MS: i64 = 10 * 60 * 1000;

/// The local runtime opened for one engine root with the stamp of the files it opened, or
/// the stamp of the files whose last open or embed failed and when.
type LocalSlot = Option<(
    PathBuf,
    Result<(Stamp, Arc<local::LocalModel>), (Stamp, i64)>,
)>;
static LOCAL: Mutex<LocalSlot> = Mutex::new(None);

/// The projects waiting for an HTTP plugin's turn, the one served last, whether the
/// embedding thread runs, and the wait after failed sends (rv-code2 HYB2-M1, HYB2R2-L1).
struct HttpQueue {
    wanted: BTreeSet<String>,
    last: Option<String>,
    running: bool,
    backoff: vectors::Backoff,
}
static HTTP: Mutex<HttpQueue> = Mutex::new(HttpQueue {
    wanted: BTreeSet::new(),
    last: None,
    running: false,
    backoff: vectors::Backoff::new(),
});

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

/// What identifies the local runtime's files on disk: per pinned file its size, inode where
/// the platform has one, and modified time.
pub(crate) type Stamp = Vec<(u64, u64, Option<std::time::SystemTime>)>;

/// The stamp of the local runtime's files when both are plain files at their pinned sizes,
/// read from metadata only, so a deleted or replaced model is never served from the cache
/// (rv-code2 HYB2-L1). Input: the engine root. Output: the stamp, or none.
pub(crate) fn local_stamp(root: &Path) -> Option<Stamp> {
    let place = local::folder(root);
    let stamp: Stamp = local::PINNED
        .iter()
        .map(|pin| {
            let found = std::fs::symlink_metadata(place.join(pin.name)).ok()?;
            if !found.is_file() || found.len() != pin.bytes {
                return None;
            }
            #[cfg(unix)]
            let inode = std::os::unix::fs::MetadataExt::ino(&found);
            #[cfg(not(unix))]
            let inode = 0;
            Some((found.len(), inode, found.modified().ok()))
        })
        .collect::<Option<_>>()?;
    assert_eq!(stamp.len(), local::PINNED.len());
    Some(stamp)
}

/// The cached local runtime of `root` while its files still carry the stamp it was opened
/// with; a deleted or replaced model empties the slot, which closes its file and frees the
/// disk. Inputs: the locked slot and the root. Output: the model, or none.
fn cached_local(slot: &mut LocalSlot, root: &Path) -> Option<Arc<local::LocalModel>> {
    let Some((at, Ok((stamp, model)))) = slot.as_ref() else {
        return None;
    };
    if at != root {
        return None;
    }
    if local_stamp(root).as_ref() == Some(stamp) {
        return Some(Arc::clone(model));
    }
    *slot = None;
    None
}

/// The local runtime already open for `root`, never opening it, so a search on the tick
/// never waits on the re-hash. Input: the engine root. Output: the model, or none.
fn opened_local(root: &Path) -> Option<Arc<local::LocalModel>> {
    let mut slot = LOCAL.lock().unwrap_or_else(PoisonError::into_inner);
    cached_local(&mut slot, root)
}

/// Opens `root`'s local runtime on the indexer thread, outside the lock, and at most once
/// per [`LOCAL_RETRY_MS`] after a failure of the same files; new files are tried at once.
/// Inputs: the engine root and the time. Output: the model, or none while it is missing or
/// failing.
pub(crate) fn open_local(root: &Path, now_ms: i64) -> Option<Arc<local::LocalModel>> {
    let stamp = {
        let mut slot = LOCAL.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(model) = cached_local(&mut slot, root) {
            return Some(model);
        }
        let stamp = local_stamp(root)?;
        let waiting = matches!(
            slot.as_ref(),
            Some((at, Err((failed_stamp, failed))))
                if at == root && *failed_stamp == stamp && now_ms - failed < LOCAL_RETRY_MS
        );
        if waiting {
            return None;
        }
        stamp
    };
    let opened = local::LocalModel::open(root)
        .map(|model| (stamp.clone(), Arc::new(model)))
        .map_err(|_| (stamp, now_ms));
    let model = opened.as_ref().ok().map(|(_, model)| Arc::clone(model));
    *LOCAL.lock().unwrap_or_else(PoisonError::into_inner) = Some((root.to_owned(), opened));
    model
}

/// Why a search cannot use the built-in model yet, read without opening it (rv-code2
/// HYB3-L1): its files are missing, its last open of these same files failed, or it is
/// still loading. Input: the engine root. Output: the note.
pub(crate) fn local_note(root: &Path) -> &'static str {
    let Some(stamp) = local_stamp(root) else {
        return vectors::NOTE_MODEL_MISSING;
    };
    let slot = LOCAL.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_ref() {
        Some((at, Err((failed, _)))) if at == root && *failed == stamp => {
            vectors::NOTE_MODEL_FAILED
        }
        _ => vectors::NOTE_LOADING,
    }
}

/// Marks `root`'s local runtime failed after an embed error, so it is opened again later.
/// Inputs: the engine root and the time. Output: none.
fn local_failed(root: &Path, now_ms: i64) {
    let failed = local_stamp(root).map(|stamp| (root.to_owned(), Err((stamp, now_ms))));
    *LOCAL.lock().unwrap_or_else(PoisonError::into_inner) = failed;
}

/// Closes the cached local runtime, freeing its open file. Output: none.
fn close_local() {
    *LOCAL.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Embeds the project's chunks that have no vector for the chosen plugin yet, then the
/// app-wide memories, outside any transaction and within [`vectors::BOUNDS`]. The local
/// runtime embeds here on the indexer thread; an HTTP plugin's sends run on their own
/// thread, so a slow or failing host never holds up full-text indexing (rv-code2 HYB2-M1).
/// No chosen plugin embeds nothing and keeps every vector (leader QB), and closes the local
/// runtime. Inputs: the indexer's connection, the engine root and the project. Output: how
/// many chunks were embedded here.
pub fn embed_pending(connection: &mut Connection, root: &Path, project: &ProjectId) -> usize {
    let Some(loaded) = settings::chosen(connection, root) else {
        close_local();
        return 0;
    };
    let plugin = plugin_of(&loaded.manifest);
    if matches!(loaded.manifest.runs, Runs::Http(_)) {
        close_local();
        want_http(root, project, &plugin.key);
        return 0;
    }
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
    let embedded = vectors::embed_round(connection, project, &plugin, &vectors::BOUNDS, &mut embed);
    if broke {
        local_failed(root, now_ms());
    }
    embedded
}

/// Takes the next waiting project after the one served last, wrapping to the first, so a
/// project the tick queues again during its long turn waits for the others (rv-code2
/// HYB2R2-L1). Inputs: the waiting projects and the last served. Output: the project, or
/// none when none waits.
pub(crate) fn take_next(wanted: &mut BTreeSet<String>, last: Option<&str>) -> Option<String> {
    let after = last.and_then(|last| {
        wanted
            .range::<str, _>((Bound::Excluded(last), Bound::Unbounded))
            .next()
            .cloned()
    });
    let next = after.or_else(|| wanted.first().cloned())?;
    let taken = wanted.remove(&next);
    assert!(taken);
    Some(next)
}

/// Queues a project for the HTTP embedding thread and starts the thread when none runs,
/// unless the plugin is waiting after failed sends. Inputs: the engine root, the project
/// and the plugin key. Output: none.
fn want_http(root: &Path, project: &ProjectId, key: &str) {
    let mut queue = HTTP.lock().unwrap_or_else(PoisonError::into_inner);
    if !queue.backoff.ready(key, now_ms()) {
        return;
    }
    if queue.wanted.len() < HTTP_WANTED_MAX {
        queue.wanted.insert(project.as_str().to_owned());
    }
    if queue.running {
        return;
    }
    let root = root.to_owned();
    let started = std::thread::Builder::new()
        .name("search-embed".into())
        .spawn(move || http_thread(&root));
    queue.running = started.is_ok();
}

/// What one project's HTTP turn did: nothing to send, sends that worked, or a failed send
/// to the plugin with that key.
enum Turn {
    Idle,
    Sent(String),
    Failed(String),
}

/// The HTTP embedding thread: its own connection, one turn per waiting project round robin
/// after the last served until none waits; a failed send starts the plugin's backoff and empties the queue.
/// Input: the engine root. Output: none.
fn http_thread(root: &Path) {
    let opened = Connection::open(root.join("engine.db")).and_then(|connection| {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    });
    let mut connection = opened.ok();
    loop {
        let next = {
            let mut queue = HTTP.lock().unwrap_or_else(PoisonError::into_inner);
            let next = if connection.is_some() {
                let last = queue.last.take();
                let next = take_next(&mut queue.wanted, last.as_deref());
                queue.last = next.clone().or(last);
                next
            } else {
                None
            };
            if next.is_none() {
                queue.wanted.clear();
                queue.running = false;
            }
            next
        };
        let (Some(name), Some(connection)) = (next, connection.as_mut()) else {
            return;
        };
        let Ok(project) = ProjectId::new(&name) else {
            continue;
        };
        let turn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            http_turn(connection, root, &project)
        }));
        let mut queue = HTTP.lock().unwrap_or_else(PoisonError::into_inner);
        match turn {
            Ok(Turn::Failed(key)) => {
                queue.backoff.failed(&key, now_ms());
                queue.wanted.clear();
            }
            Ok(Turn::Sent(key)) => queue.backoff.succeeded(&key),
            Ok(Turn::Idle) | Err(_) => {}
        }
    }
}

/// One project's HTTP turn: the waiting queries, then at most [`HTTP_CALLS_MAX`] one-batch
/// document calls, the chosen plugin and its confirmed host read again before every call,
/// queries included (rv-v3sec note 1). Inputs: the thread's connection, the engine root
/// and the project. Output: what the turn did.
fn http_turn(connection: &mut Connection, root: &Path, project: &ProjectId) -> Turn {
    let http_chosen = |connection: &Connection| {
        settings::chosen(connection, root)
            .filter(|loaded| matches!(loaded.manifest.runs, Runs::Http(_)))
    };
    let Some(loaded) = http_chosen(connection) else {
        return Turn::Idle;
    };
    let plugin = plugin_of(&loaded.manifest);
    let still = |connection: &Connection| {
        http_chosen(connection).is_some_and(|now| plugin_of(&now.manifest) == plugin)
    };
    let mut failed = false;
    let mut sent = false;
    let per_call = plugin.batch.min(vectors::QUERY_ROUND_MAX);
    for _ in 0..vectors::QUERY_ROUND_MAX.div_ceil(per_call) {
        if failed || !still(connection) {
            break;
        }
        let mut query = |texts: &[&str]| {
            let reply = crate::app_plugins::http::embed(root, &loaded, texts, Purpose::Query);
            failed |= reply.is_err();
            reply
        };
        let answered = vectors::embed_queries_up_to(connection, &plugin, per_call, &mut query);
        sent |= answered > 0;
        if answered < per_call {
            break;
        }
    }
    let one_call = vectors::Bounds {
        round_chunks: plugin.batch,
        ..vectors::BOUNDS
    };
    for _ in 0..HTTP_CALLS_MAX {
        if failed || !still(connection) {
            break;
        }
        let mut document = |texts: &[&str]| {
            let reply = crate::app_plugins::http::embed(root, &loaded, texts, Purpose::Document);
            failed |= reply.is_err();
            reply
        };
        let stored = vectors::embed_round(connection, project, &plugin, &one_call, &mut document);
        sent |= stored > 0;
        if stored < plugin.batch {
            break;
        }
    }
    if failed {
        Turn::Failed(plugin.key)
    } else if sent {
        Turn::Sent(plugin.key)
    } else {
        Turn::Idle
    }
}

/// Fuses the full-text order with the chosen plugin's vector order, on the tick and with
/// no network: the local runtime embeds the query only once the indexer opened it, and an
/// HTTP plugin's query vector comes from the indexer's cache, asked for on a miss. Without
/// a query vector the answer is the full-text order with a note (addendum 1b). Inputs: the
/// connection, the engine root, the project, the source, the untrusted query and the
/// full-text chunk ids best first. Output: the fused ids and the note.
pub fn hybrid(
    connection: &Connection,
    root: &Path,
    project: &ProjectId,
    source: crate::search_index::Source,
    query: &str,
    full_text: &[i64],
) -> Hybrid {
    let words_only = |note| Hybrid {
        ids: full_text.to_vec(),
        note,
    };
    let Some(loaded) = settings::chosen(connection, root) else {
        return words_only(None);
    };
    let Some(text) = vectors::query_text(query) else {
        return words_only(None);
    };
    let plugin = plugin_of(&loaded.manifest);
    let vector = match &loaded.manifest.runs {
        Runs::Local { .. } => {
            let Some(model) = opened_local(root) else {
                return words_only(Some(local_note(root)));
            };
            let reply = model.embed(&[text.as_str()]).ok();
            match reply.and_then(|reply| vectors::one_vector(reply, plugin.dimensions)) {
                Some(vector) => vector,
                None => return words_only(Some(vectors::NOTE_UNAVAILABLE)),
            }
        }
        Runs::Http(_) => match vectors::cached_query(connection, &plugin, query) {
            Some(vector) => vector,
            None => {
                let _ = vectors::ask_query(connection, &plugin.key, query, now_ms());
                return words_only(Some(vectors::NOTE_QUERY_PENDING));
            }
        },
    };
    if vector.iter().all(|value| *value == 0.0) {
        return words_only(None);
    }
    vectors::rank(
        connection,
        project,
        source,
        &plugin,
        &vector,
        full_text,
        &vectors::BOUNDS,
    )
}
