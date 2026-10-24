pub mod interactive;
pub mod launch;
pub mod transcript;

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const SEARCH_DIRECTORIES_MAX: usize = 128;
const PATH_BYTES_MAX: usize = 1024;
const SYSTEM_DIRECTORIES: [&str; 4] = [
    "/opt/homebrew/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/usr/local/bin",
    "/usr/bin",
];
const HOME_DIRECTORIES: [&str; 6] = [
    ".local/bin",
    ".npm-global/bin",
    ".bun/bin",
    ".cargo/bin",
    ".volta/bin",
    "bin",
];

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
}

/// Where the catalog looks for harness executables.
/// Inputs are explicit so the engine and tests choose the search space; only
/// absolute directories are kept and nothing found is ever executed.
#[derive(Clone, Debug)]
pub struct Search {
    directories: Vec<PathBuf>,
    home: Option<PathBuf>,
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
        let registered: Vec<(String, PathBuf)> = registered
            .into_iter()
            .filter(|(_, path)| usable_path(path))
            .collect();
        assert!(directories.len() <= SEARCH_DIRECTORIES_MAX);
        assert!(directories.iter().all(|directory| directory.is_absolute()));
        Self {
            directories,
            home,
            registered,
        }
    }

    /// Builds a search from this process's PATH and HOME with no registered executables.
    /// Output: the search the engine uses for harnesses it does not launch itself.
    pub fn from_environment() -> Self {
        let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        directories.extend(SYSTEM_DIRECTORIES.iter().map(PathBuf::from));
        Self::new(
            directories,
            std::env::var_os("HOME").map(PathBuf::from),
            Vec::new(),
        )
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
        if let Some((_, path)) = self.registered.iter().find(|(id, _)| id == spec.id) {
            return executable_file(path).then(|| path.clone());
        }
        let mut directories: Vec<PathBuf> = self.directories.clone();
        if let Some(home) = &self.home {
            directories.extend(HOME_DIRECTORIES.iter().map(|relative| home.join(relative)));
            directories.extend(spec.locations.iter().map(|relative| home.join(relative)));
        }
        for binary in spec.binaries {
            assert!(!binary.contains('/'));
            for directory in &directories {
                let candidate = directory.join(binary);
                if executable_file(&candidate) {
                    assert!(candidate.is_absolute());
                    return Some(candidate);
                }
            }
        }
        None
    }
}

fn usable_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path.as_os_str() != OsStr::new("/")
}

fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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
            }
        })
        .collect();
    assert_eq!(entries.len(), specs().len());
    entries
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
        account_kinds: &[
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::Local,
        ],
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
