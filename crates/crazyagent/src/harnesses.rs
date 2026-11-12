pub(crate) mod codex_folder;
pub mod finding;
pub mod interactive;
pub mod launch;
pub mod leftovers;
pub mod terminal_session;
pub mod transcript;
pub mod update;

#[cfg(test)]
mod account_fit_tests;
#[cfg(all(test, unix))]
mod finding_tests;
#[cfg(test)]
mod probe_tests;

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const SEARCH_DIRECTORIES_MAX: usize = 128;
const PATH_BYTES_MAX: usize = 1024;
#[cfg(target_os = "linux")]
const SYSTEM_DIRECTORIES: [&str; 4] = [
    "/opt/homebrew/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/usr/local/bin",
    "/usr/bin",
];
#[cfg(not(target_os = "linux"))]
const SYSTEM_DIRECTORIES: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];
const HOME_DIRECTORIES: [&str; 10] = [
    ".local/bin",
    ".npm-global/bin",
    ".bun/bin",
    ".cargo/bin",
    "bin",
    ".nvm/current/bin",
    ".local/share/fnm/aliases/default/bin",
    "Library/Application Support/fnm/aliases/default/bin",
    "Library/pnpm",
    ".local/share/pnpm",
];
/// Version-manager shim folders. Their files start the real program by their own name, so a
/// launch of the resolved file runs the shim, not the harness: they are named in the places
/// looked but never used, and the owner chooses the real file instead.
pub const SHIM_DIRECTORIES: [&str; 3] = [".volta/bin", ".asdf/shims", ".local/share/mise/shims"];
/// The real programs behind version-manager shims, never used as a harness file.
pub const SHIM_PROGRAMS: [&str; 3] = ["volta-shim", "mise", "asdf"];
const NVM_VERSIONS: &str = ".nvm/versions/node";
const NVM_DEFAULT_ALIAS: &str = ".nvm/alias/default";
const NVM_ENTRIES_MAX: usize = 64;
const NVM_VERSIONS_MAX: usize = 8;
const NVM_ALIAS_BYTES_MAX: u64 = 64;
const HOME_PLACES_MAX: usize = HOME_DIRECTORIES.len() + NVM_VERSIONS_MAX;
const NODE_PREFIX_DEPTH_MAX: usize = 32;
const NODE_PREFIX_BYTES_MAX: usize = 1024;
const LOCATIONS_MAX: usize = 8;
/// The most files one harness search hands to the pin checks ([`Search::candidates`]).
pub const CANDIDATES_MAX: usize = 16;

/// Names a kind of account a harness can run on.
/// Input: none. Output: the plain wire name shared by the TUI and desktop clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Subscription,
    ApiKey,
    OpenAiCompatibleUrl,
    Local,
}

/// States whether a harness can be used from CrazyAgent on this machine.
/// Limited carries a plain reason the app shows to the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HarnessStatus {
    Ready,
    NotInstalled,
    Limited { reason: String },
}

impl HarnessStatus {
    /// Renders the status in the plain words the clients show.
    /// Input: the status. Output: "Ready", "Not installed" or "Limited control: <reason>".
    pub fn label(&self) -> String {
        match self {
            Self::Ready => "Ready".into(),
            Self::NotInstalled => "Not installed".into(),
            Self::Limited { reason } => format!("Limited control: {reason}"),
        }
    }
}

/// States whether the owner can switch a harness's tools on and off.
/// Limited carries the plain reason the switch is not available.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolControl {
    Switchable,
    Limited { reason: String },
}

/// States whether a harness can run a team agent, the master or a sub-agent.
/// MasterOnly runs the project master, with no live terminal, and never a team agent;
/// Unavailable carries the plain reason, such as no reader for its replies yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TeamUse {
    Available,
    MasterOnly { reason: String },
    Unavailable { reason: String },
}

/// Describes one harness as documented by its vendor, independent of this machine.
/// `binaries` are executable names searched in order; `locations` are extra
/// install directories relative to HOME; `launch` is None when CrazyAgent
/// has a launch adapter and otherwise the plain reason it does not.
#[derive(Clone, Copy, Debug)]
pub struct HarnessSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub binaries: &'static [&'static str],
    pub locations: &'static [&'static str],
    pub headless: &'static str,
    pub tools_limited: Option<&'static str>,
    pub model_selection: bool,
    pub account_kinds: &'static [AccountKind],
    pub launch: Option<&'static str>,
}

/// One catalog row the engine serves to the clients.
/// Combines the static spec with what was found on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessEntry {
    pub id: String,
    pub name: String,
    pub status: HarnessStatus,
    pub status_label: String,
    pub executable: Option<PathBuf>,
    pub headless: String,
    pub tools: ToolControl,
    pub model_selection: bool,
    pub account_kinds: Vec<AccountKind>,
    pub team: TeamUse,
    pub aliases: Vec<String>,
}

/// Where the catalog looks for harness executables.
/// Inputs are explicit so the engine and tests choose the search space; only
/// absolute directories are kept and nothing found is ever executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    directories: Vec<PathBuf>,
    home: Option<PathBuf>,
    home_places: Vec<PathBuf>,
    registered: Vec<(String, PathBuf)>,
}

impl Search {
    /// Builds a search from PATH directories, an optional HOME and engine-registered executables.
    /// Relative or oversized directories are dropped. Output: the bounded search space.
    pub fn new(
        directories: Vec<PathBuf>,
        home: Option<PathBuf>,
        registered: Vec<(String, PathBuf)>,
    ) -> Self {
        let directories: Vec<PathBuf> = directories
            .into_iter()
            .filter(|directory| usable_path(directory))
            .take(SEARCH_DIRECTORIES_MAX)
            .collect();
        let home = home.filter(|home| usable_path(home));
        let home_places = home.as_deref().map(home_places).unwrap_or_default();
        let registered: Vec<(String, PathBuf)> = registered
            .into_iter()
            .filter(|(_, path)| usable_path(path))
            .collect();
        assert!(directories.len() <= SEARCH_DIRECTORIES_MAX);
        assert!(directories.iter().all(|directory| directory.is_absolute()));
        assert!(home_places.len() <= HOME_PLACES_MAX);
        Self {
            directories,
            home,
            home_places,
            registered,
        }
    }

    /// Builds the one search engine start, the catalog, Check again and sign-in all use: the
    /// PATH folders, then the system folders no other user can change, then the HOME folders
    /// (see [`home_places`]). Inputs: the PATH value, the system folders and HOME.
    /// Output: the bounded search.
    pub fn engine(path: Option<&OsStr>, system: &[PathBuf], home: Option<PathBuf>) -> Self {
        let mut directories: Vec<PathBuf> = path
            .map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default();
        for folder in system {
            if !directories.contains(folder)
                && crate::service::changeable_by_others(folder).is_none()
            {
                directories.push(folder.clone());
            }
        }
        Self::new(directories, home, Vec::new())
    }

    /// Builds the engine search from this process's PATH and HOME and the built-in system
    /// folders, with no registered executables. Output: the search ([`Search::engine`]).
    pub fn from_environment() -> Self {
        let system: Vec<PathBuf> = SYSTEM_DIRECTORIES.iter().map(PathBuf::from).collect();
        Self::engine(
            std::env::var_os("PATH").as_deref(),
            &system,
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }

    /// Finds the executable entry of one catalog harness, its links kept, exactly as the
    /// catalog finds it: shims are passed over ([`is_shim`]). Input: the catalog id.
    /// Output: the absolute entry, or none for an alias, an unknown id or a harness found
    /// nowhere this search looks.
    pub fn find(&self, id: &str) -> Option<PathBuf> {
        let spec = spec(id).filter(|spec| spec.id == id)?;
        let found = self.locate(spec)?;
        assert!(found.is_absolute());
        Some(found)
    }

    /// Every executable file this search finds for one harness, in search order, shims
    /// included, for the pin to check one by one. Input: the catalog id. Output: at most
    /// [`CANDIDATES_MAX`] absolute files; empty for an alias or unknown id.
    pub fn candidates(&self, id: &str) -> Vec<PathBuf> {
        let Some(spec) = spec(id).filter(|spec| spec.id == id) else {
            return Vec::new();
        };
        self.hits(spec, CANDIDATES_MAX, true)
    }

    /// The folders this search looks in for one harness, in order, then the version-manager
    /// shim folders it never uses, for the owner's "where we looked" line. Input: the catalog
    /// id. Output: the folders, bounded; empty for an alias or unknown id.
    pub fn looked(&self, id: &str) -> Vec<PathBuf> {
        let Some(spec) = spec(id).filter(|spec| spec.id == id) else {
            return Vec::new();
        };
        let mut folders = self.places(spec);
        if let Some(home) = &self.home {
            for shim in SHIM_DIRECTORIES
                .iter()
                .map(|relative| below(home, relative))
            {
                if !folders.contains(&shim) {
                    folders.push(shim);
                }
            }
        }
        assert!(
            folders.len()
                <= SEARCH_DIRECTORIES_MAX
                    + HOME_PLACES_MAX
                    + LOCATIONS_MAX
                    + SHIM_DIRECTORIES.len()
        );
        folders
    }

    /// The HOME this search looks under, for showing folders below it as `~`.
    /// Output: the absolute HOME, or none.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The folders searched for one spec, in order: the search folders, the HOME folders and
    /// the spec's own install folders under HOME. Input: the spec. Output: the folders.
    fn places(&self, spec: &HarnessSpec) -> Vec<PathBuf> {
        assert!(spec.locations.len() <= LOCATIONS_MAX);
        let mut folders: Vec<PathBuf> = self.directories.clone();
        folders.extend(self.home_places.iter().cloned());
        if let Some(home) = &self.home {
            folders.extend(spec.locations.iter().map(|relative| below(home, relative)));
        }
        folders
    }

    /// Adds an executable the engine was started with, which wins over any search hit.
    /// Inputs: catalog id and absolute path. Output: the updated search.
    pub fn with_registered(mut self, id: &str, path: PathBuf) -> Self {
        if usable_path(&path) {
            self.registered.push((id.to_owned(), path));
        }
        self
    }

    fn locate(&self, spec: &HarnessSpec) -> Option<PathBuf> {
        self.hits(spec, 1, false).pop()
    }

    /// The executable files one spec names in the folders this search looks in, in search
    /// order, each once; a registered file alone when one is registered. Inputs: the spec,
    /// the most to return and whether shims ([`is_shim`]) are kept. Output: at most `limit`.
    fn hits(&self, spec: &HarnessSpec, limit: usize, shims: bool) -> Vec<PathBuf> {
        assert!((1..=CANDIDATES_MAX).contains(&limit));
        if let Some((_, path)) = self.registered.iter().find(|(id, _)| id == spec.id) {
            return executable_file(path)
                .then(|| path.clone())
                .into_iter()
                .collect();
        }
        let directories = self.places(spec);
        let mut found: Vec<PathBuf> = Vec::new();
        for binary in spec.binaries {
            assert!(!binary.contains('/'));
            for directory in &directories {
                let candidate = directory.join(binary);
                if found.contains(&candidate)
                    || !executable_file(&candidate)
                    || (!shims && is_shim(self.home(), &candidate))
                {
                    continue;
                }
                assert!(candidate.is_absolute());
                found.push(candidate);
                if found.len() == limit {
                    return found;
                }
            }
        }
        assert!(found.len() <= limit);
        found
    }
}

/// Whether a harness file is a version-manager shim, which starts the real program by its own
/// name and so never runs as the harness: it sits in a shim folder under HOME
/// ([`SHIM_DIRECTORIES`]), or its real file is a shim program ([`SHIM_PROGRAMS`]).
/// Inputs: HOME and the file. Output: true for a shim.
pub fn is_shim(home: Option<&Path>, path: &Path) -> bool {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let program = real
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SHIM_PROGRAMS.contains(&name));
    program
        || home.is_some_and(|home| {
            SHIM_DIRECTORIES
                .iter()
                .map(|relative| below(home, relative))
                .any(|folder| path.starts_with(&folder) || real.starts_with(&folder))
        })
}

/// Whether a folder is one of the version-manager shim folders under HOME, for the "where we
/// looked" line. Inputs: HOME and the folder. Output: true for a shim folder.
pub fn is_shim_folder(home: Option<&Path>, folder: &Path) -> bool {
    home.is_some_and(|home| {
        SHIM_DIRECTORIES
            .iter()
            .any(|relative| below(home, relative) == folder)
    })
}

/// The HOME folders a harness search looks in after PATH and the system folders: the usual
/// user bin folders, nvm's current link, fnm's default alias, pnpm's global bin, then nvm's
/// installed Node versions with the default alias first and the newest after it. No shell
/// is run and no shell file is read. Input: an absolute HOME. Output: the folders, bounded.
pub(crate) fn home_places(home: &Path) -> Vec<PathBuf> {
    assert!(home.is_absolute());
    let mut folders: Vec<PathBuf> = HOME_DIRECTORIES
        .iter()
        .map(|relative| below(home, relative))
        .collect();
    folders.extend(nvm_versions(home));
    assert!(folders.len() <= HOME_PLACES_MAX);
    folders
}

/// The `bin` folders of nvm's installed Node versions: the one the default alias names
/// first, then the newest. Reads at most [`NVM_ENTRIES_MAX`] folder entries and
/// [`NVM_ALIAS_BYTES_MAX`] alias bytes. Input: HOME. Output: at most [`NVM_VERSIONS_MAX`].
fn nvm_versions(home: &Path) -> Vec<PathBuf> {
    let versions = below(home, NVM_VERSIONS);
    let Ok(entries) = std::fs::read_dir(&versions) else {
        return Vec::new();
    };
    let mut found: Vec<((u64, u64, u64), String)> = entries
        .take(NVM_ENTRIES_MAX)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((node_version(&name)?, name))
        })
        .collect();
    found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    if let Some(alias) = nvm_default(home) {
        let named = |name: &str| {
            let bare = name.trim_start_matches('v');
            bare == alias || bare.starts_with(&format!("{alias}."))
        };
        found.sort_by_key(|(_, name)| !named(name));
    }
    found.truncate(NVM_VERSIONS_MAX);
    let folders: Vec<PathBuf> = found
        .into_iter()
        .map(|(_, name)| versions.join(name).join("bin"))
        .collect();
    assert!(folders.len() <= NVM_VERSIONS_MAX);
    folders
}

/// Reads a Node version folder name such as `v22.11.0`. Input: the name.
/// Output: the major, minor and patch numbers, or none for any other name.
fn node_version(name: &str) -> Option<(u64, u64, u64)> {
    let mut parts = name.strip_prefix('v')?.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

/// The version nvm's default alias names when it is a plain version such as `22` or
/// `v22.11.0`; a named alias such as `lts/*` is not followed. The alias is read only when it
/// is a regular file, opened without following a link or waiting on a pipe, at most
/// [`NVM_ALIAS_BYTES_MAX`] bytes. Input: HOME. Output: the version without its `v`, or none.
fn nvm_default(home: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    transcript::open_plain(&below(home, NVM_DEFAULT_ALIAS))?
        .take(NVM_ALIAS_BYTES_MAX)
        .read_to_string(&mut text)
        .ok()?;
    let alias = text.trim().trim_start_matches('v');
    (!alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.'))
    .then(|| alias.to_owned())
}

/// Joins a `/`-separated relative folder onto a base one part at a time.
/// Inputs: the base and the relative folder. Output: the joined path.
fn below(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |at, part| at.join(part))
}

/// The npm prefix a Node script harness sits in, so a sign-in or an agent launch can put the
/// `node` beside it on PATH: for a real file under `<prefix>/lib/node_modules/`, the prefix
/// whose `bin` is an absolute folder of at most 1024 bytes with no `:`, holding a `node` file
/// only the owner and the system can change, and none of them in a `.crazyagent` folder
/// ([`in_crazyagent_folder`]). Input: the harness's real file. Output: the prefix, or none.
pub fn node_prefix(program: &Path) -> Option<PathBuf> {
    let prefix = program
        .ancestors()
        .take(NODE_PREFIX_DEPTH_MAX)
        .find(|folder| folder.ends_with("lib/node_modules"))
        .and_then(|modules| modules.parent()?.parent())?;
    let bin = prefix.join("bin");
    let node = bin.join("node");
    let usable = bin.is_absolute()
        && bin
            .to_str()
            .is_some_and(|text| !text.contains(':') && text.len() <= NODE_PREFIX_BYTES_MAX)
        && node.is_file()
        && !in_crazyagent_folder(program)
        && std::fs::canonicalize(&node).is_ok_and(|real| !in_crazyagent_folder(&real))
        && crate::service::changeable_by_others(&node).is_none();
    usable.then(|| prefix.to_path_buf())
}

/// Whether a path runs through a `.crazyagent` folder, where every project keeps the
/// worktrees its agents write, so no harness file or `node` may come from there.
/// Input: the path. Output: the answer.
pub fn in_crazyagent_folder(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str() == crate::projects_home::CRAZYAGENT_DIRECTORY)
}

fn usable_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path.as_os_str() != OsStr::new("/")
}

fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && runnable(&metadata))
}

/// Reports whether file metadata grants any execute permission.
/// Takes the metadata. Returns true when any of the three execute bits is set.
#[cfg(unix)]
fn runnable(metadata: &std::fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

/// Reports that a file may be run where the platform has no execute bit; the launch
/// decides by extension. Takes the metadata, unused. Returns true.
#[cfg(not(unix))]
fn runnable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Builds the catalog rows for every known harness, in owner priority order.
/// Input: the search space. Output: one entry per spec with its detected status.
pub fn catalog(search: &Search) -> Vec<HarnessEntry> {
    let entries: Vec<HarnessEntry> = specs()
        .iter()
        .map(|spec| {
            let executable = search.locate(spec);
            let status = match (&executable, spec.launch, spec.tools_limited) {
                (None, _, _) => HarnessStatus::NotInstalled,
                (Some(_), Some(reason), _) | (Some(_), None, Some(reason)) => {
                    HarnessStatus::Limited {
                        reason: reason.into(),
                    }
                }
                (Some(_), None, None) => HarnessStatus::Ready,
            };
            HarnessEntry {
                id: spec.id.into(),
                name: spec.name.into(),
                status_label: status.label(),
                status,
                executable,
                headless: spec.headless.into(),
                tools: spec
                    .tools_limited
                    .map_or(ToolControl::Switchable, |reason| ToolControl::Limited {
                        reason: reason.into(),
                    }),
                model_selection: spec.model_selection,
                account_kinds: spec.account_kinds.to_vec(),
                team: match interactive::support(spec.id) {
                    interactive::Support::Terminal => TeamUse::Available,
                    interactive::Support::HeadlessOnly { reason }
                        if crate::team::master_backend(spec.id).is_some() =>
                    {
                        TeamUse::MasterOnly {
                            reason: reason.into(),
                        }
                    }
                    interactive::Support::HeadlessOnly { reason }
                    | interactive::Support::Unavailable { reason } => TeamUse::Unavailable {
                        reason: reason.into(),
                    },
                },
                aliases: CONNECTION_NAMES
                    .iter()
                    .filter(|(_, catalog)| *catalog == spec.id)
                    .map(|(name, _)| (*name).to_owned())
                    .collect(),
            }
        })
        .collect();
    assert_eq!(entries.len(), specs().len());
    entries
}

/// The engine's last harness probe and the search it ran with, so a catalog read answers at
/// once instead of waiting on the filesystem. A read older than `fresh_for` is still served and
/// starts one background re-probe; a changed search or an owner's check probes before answering.
pub struct ProbeCache {
    fresh_for: Duration,
    last: Mutex<Option<Probe>>,
    probing: AtomicBool,
}

struct Probe {
    search: Search,
    entries: Vec<HarnessEntry>,
    started: Instant,
}

impl ProbeCache {
    /// Builds an empty cache. Input: how long a probe counts as fresh. Output: the shared cache.
    pub fn new(fresh_for: Duration) -> Arc<Self> {
        Arc::new(Self {
            fresh_for,
            last: Mutex::new(None),
            probing: AtomicBool::new(false),
        })
    }

    /// Answers one catalog read: the cached probe for the same search at once, starting a
    /// background re-probe when it is older than `fresh_for`, or a new probe when nothing is
    /// cached for this search or the owner asked to check now. Inputs: the search and whether to
    /// probe now. Output: the catalog, or an error when the probe task failed.
    pub async fn entries(
        self: &Arc<Self>,
        search: Search,
        check: bool,
    ) -> Result<Vec<HarnessEntry>, tokio::task::JoinError> {
        if !check && let Some((entries, stale)) = self.cached(&search) {
            if stale {
                self.refresh(search);
            }
            return Ok(entries);
        }
        let started = Instant::now();
        let probed = probe(search.clone()).await?;
        self.store(search, probed.clone(), started);
        Ok(probed)
    }

    /// Reports whether a background re-probe is running. Output: true while one runs.
    pub fn probing(&self) -> bool {
        self.probing.load(Ordering::SeqCst)
    }

    fn cached(&self, search: &Search) -> Option<(Vec<HarnessEntry>, bool)> {
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        last.as_ref()
            .filter(|probe| probe.search == *search)
            .map(|probe| {
                (
                    probe.entries.clone(),
                    probe.started.elapsed() >= self.fresh_for,
                )
            })
    }

    fn refresh(self: &Arc<Self>, search: Search) {
        if self.probing.swap(true, Ordering::SeqCst) {
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            if let Ok(entries) = probe(search.clone()).await {
                cache.store(search, entries, started);
            }
            cache.probing.store(false, Ordering::SeqCst);
        });
    }

    fn store(&self, search: Search, entries: Vec<HarnessEntry>, started: Instant) {
        assert_eq!(entries.len(), specs().len());
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if last.as_ref().is_none_or(|probe| probe.started <= started) {
            *last = Some(Probe {
                search,
                entries,
                started,
            });
        }
    }
}

async fn probe(search: Search) -> Result<Vec<HarnessEntry>, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || catalog(&search)).await
}

/// Finds a harness spec by its catalog id or by the harness name stored on a
/// model connection or team plan ("claude" for Claude Code).
/// Input: id such as "codex". Output: the spec, or None for an unknown id.
pub fn spec(id: &str) -> Option<&'static HarnessSpec> {
    let id = CONNECTION_NAMES
        .iter()
        .find(|(name, _)| *name == id)
        .map_or(id, |(_, catalog)| catalog);
    specs().iter().find(|spec| spec.id == id)
}

/// Tells whether two harness names, as a team plan, agent row or connection stores them,
/// name the same harness: one catalog entry by its id or an alias ("claude" and
/// "claude-code"), or the same text for names the catalog does not know.
/// Inputs: the two names. Output: the answer.
pub fn same(one: &str, other: &str) -> bool {
    match (spec(one), spec(other)) {
        (Some(one), Some(other)) => one.id == other.id,
        _ => one == other,
    }
}

/// The catalog id of a harness name as a team plan, agent row or connection stores it, or
/// the name itself when the catalog does not know it.
/// Input: the stored name. Output: one spelling per harness.
pub fn canonical(id: &str) -> &str {
    spec(id).map_or(id, |spec| spec.id)
}

/// Tells whether a harness can run on one account (DEC-141 PA-4), the one rule every master,
/// team agent and research job uses. Inputs: a catalog id or alias and the stored connection.
/// Output: true for a sign-in of that harness itself, or for an HTTP endpoint account whose
/// wire format it speaks (Codex any OpenAI format, Claude Code Anthropic Messages, RCLI all);
/// the harness an endpoint account was stored with is ignored.
pub fn account_fits(harness: &str, connection: &crate::config::ModelConnection) -> bool {
    use crate::config::{ModelAuthentication, ModelEndpoint};
    let Some(id) = spec(harness).map(|spec| spec.id) else {
        return false;
    };
    let protocol = connection.protocol.as_str();
    match (&connection.endpoint, &connection.auth) {
        (ModelEndpoint::Native {}, ModelAuthentication::NativeManaged { .. }) => {
            protocol == "native"
                && id != "rcli"
                && spec(connection.harness.as_str()).is_some_and(|own| own.id == id)
        }
        (
            ModelEndpoint::Http { .. },
            ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {},
        ) => match id {
            "codex" => protocol.starts_with("openai"),
            "claude-code" => protocol == "anthropic-messages",
            "rcli" => true,
            _ => false,
        },
        _ => false,
    }
}

/// The base URL a harness is given for an endpoint account. Claude Code adds `/v1/messages`
/// to the URL it is given (2.1.322), so an Anthropic URL stored with its `/v1` loses it there;
/// every other harness takes the URL as stored. Inputs: the catalog id or alias and the stored
/// URL. Output: the URL to hand over.
pub fn endpoint_base_url<'a>(harness: &str, url: &'a str) -> &'a str {
    let url = url.trim_end_matches('/');
    match spec(harness).map(|spec| spec.id) {
        Some("claude-code") => url.strip_suffix("/v1").unwrap_or(url),
        _ => url,
    }
}

/// Starts the engine key relay a keyed endpoint account is reached through (DEC-141 PA-4):
/// the saved key is read from the engine folder into engine memory only, and the harness
/// gets the relay's loopback URL and placeholder token instead. Inputs: the engine folder
/// that holds `account-keys`, the provider base URL as the harness takes it and the key
/// reference. Output: the running relay, `account_key_missing` when no key is saved, or the
/// key store's or relay's refusal.
pub(crate) fn key_relay(
    engine: &Path,
    base_url: &str,
    key: &crate::protocol::CredentialRef,
) -> Result<crate::model_gateway::relay::Relay, crate::engine::EngineError> {
    let secret = crate::storage::accounts::read_key_file(engine, key)?
        .ok_or_else(|| crate::engine::EngineError("account_key_missing".into()))?;
    crate::model_gateway::relay::Relay::start(base_url, secret)
}

const CONNECTION_NAMES: [(&str, &str); 1] = [("claude", "claude-code")];

const NO_ADAPTER: &str = "installed, but CrazyAgent cannot start it yet";
const ALL_ACCOUNTS: &[AccountKind] = &[
    AccountKind::Subscription,
    AccountKind::ApiKey,
    AccountKind::OpenAiCompatibleUrl,
    AccountKind::Local,
];
const KEY_URL_LOCAL: &[AccountKind] = &[
    AccountKind::ApiKey,
    AccountKind::OpenAiCompatibleUrl,
    AccountKind::Local,
];

/// Lists every harness CrazyAgent knows, owner-named harnesses first in priority order.
/// Input: none. Output: the static specs; facts and sources are recorded in harnesses.md.
pub fn specs() -> &'static [HarnessSpec] {
    &SPECS
}

static SPECS: [HarnessSpec; 23] = [
    HarnessSpec {
        id: "codex",
        name: "Codex",
        binaries: &["codex"],
        locations: &[],
        headless: "codex app-server: JSON-RPC over stdio",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: None,
    },
    HarnessSpec {
        id: "claude-code",
        name: "Claude Code",
        binaries: &["claude"],
        locations: &[".claude/local"],
        headless: "claude --print with stream-json input and output",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: None,
    },
    HarnessSpec {
        id: "opencode",
        name: "OpenCode",
        binaries: &["opencode"],
        locations: &[".opencode/bin"],
        headless: "opencode run --format json, or opencode serve over HTTP",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "gemini-cli",
        name: "Gemini CLI",
        binaries: &["gemini"],
        locations: &[],
        headless: "gemini --prompt with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "cursor-agent",
        name: "Cursor CLI",
        binaries: &["cursor-agent"],
        locations: &[".cursor/bin"],
        headless: "cursor-agent --print with --output-format stream-json",
        tools_limited: Some("tools are allowed or denied only through Cursor's permission file"),
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "fx",
        name: "fx by Vercel",
        binaries: &["fx"],
        locations: &[".fx/bin"],
        headless: "fx ask --json: one request, JSON result on stdout",
        tools_limited: Some("no documented switch to turn tools off"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "antigravity",
        name: "Antigravity",
        binaries: &["agy", "antigravity"],
        locations: &[],
        headless: "agy --print: one prompt, plain text reply",
        tools_limited: Some("tools cannot be switched off, only approved automatically"),
        model_selection: true,
        account_kinds: &[AccountKind::Subscription],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "openclaw",
        name: "OpenClaw",
        binaries: &["openclaw"],
        locations: &[],
        headless: "openclaw agent --message through its gateway",
        tools_limited: Some("tools are set in OpenClaw's own configuration"),
        model_selection: true,
        account_kinds: &[
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::Local,
        ],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "pi",
        name: "Pi",
        binaries: &["pi"],
        locations: &[],
        headless: "pi --mode json or --mode rpc over stdio",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "rcli",
        name: "RCLI",
        binaries: &["crazyagent-rcli"],
        locations: &[],
        headless: "CrazyAgent's bundled RCLI host",
        tools_limited: Some("runs without tools"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some("runs only as a CrazyAgent master"),
    },
    HarnessSpec {
        id: "amp",
        name: "Amp",
        binaries: &["amp"],
        locations: &[".amp/bin"],
        headless: "amp --execute with --stream-json",
        tools_limited: None,
        model_selection: false,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "goose",
        name: "Goose",
        binaries: &["goose"],
        locations: &[],
        headless: "goose run --text with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "aider",
        name: "Aider",
        binaries: &["aider"],
        locations: &[],
        headless: "aider --message: one prompt, plain text reply",
        tools_limited: Some("file edits cannot be switched off apart from a dry run"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "qwen-code",
        name: "Qwen Code",
        binaries: &["qwen"],
        locations: &[],
        headless: "qwen --prompt with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "crush",
        name: "Crush",
        binaries: &["crush"],
        locations: &[],
        headless: "crush run: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "droid",
        name: "Factory Droid",
        binaries: &["droid"],
        locations: &[".factory/bin"],
        headless: "droid exec with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::OpenAiCompatibleUrl,
        ],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "kiro",
        name: "Kiro CLI",
        binaries: &["kiro-cli", "kiro"],
        locations: &[],
        headless: "kiro-cli chat --no-interactive",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "copilot",
        name: "GitHub Copilot CLI",
        binaries: &["copilot"],
        locations: &[],
        headless: "copilot --prompt: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "cline",
        name: "Cline CLI",
        binaries: &["cline"],
        locations: &[],
        headless: "cline with --json output",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "continue",
        name: "Continue CLI",
        binaries: &["cn"],
        locations: &[],
        headless: "cn --print: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "openhands",
        name: "OpenHands CLI",
        binaries: &["openhands"],
        locations: &[],
        headless: "openhands --headless with a task",
        tools_limited: Some("headless mode always runs its tools"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "auggie",
        name: "Auggie",
        binaries: &["auggie"],
        locations: &[],
        headless: "auggie --print: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "kimi",
        name: "Kimi CLI",
        binaries: &["kimi"],
        locations: &[],
        headless: "kimi --print with stream-json output",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
];
