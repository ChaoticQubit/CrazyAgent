mod run;
mod state;
#[cfg(all(test, unix))]
mod tests;

pub use run::{
    AS_ROOT, CHECK_TIME, LOOKUP_FAILED, POINTER_TIME, Ran, SHOWN_BYTES_MAX, START_FAILED, TIMEOUT,
    UPDATE_TIME, VERSION_TIME, lookup, masked, refused_user, run,
};
pub(crate) use state::needs;
pub use state::{
    BUSY, CHECK_FAILED, CHECK_ONLY, INSTALL_ALREADY, INSTALL_FAILED, INSTALL_LIST_INVALID,
    INSTALL_NOT_FOUND, INSTALL_OFF, NOT_INSTALLED, SETTINGS_UNREADABLE, UNKNOWN_HARNESS,
    UPDATE_FAILED, UPDATE_OFF, Updates, initialize, set_settings, settings,
};

use crate::harnesses;
#[cfg(all(test, unix))]
use run::read_pointer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

pub const VERSION_BYTES_MAX: usize = 64;
pub const ARGS_MAX: usize = 5;
pub const BREW_PREFIXES: [&str; 3] = ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"];
pub const POINTER_BYTES_MAX: u64 = 256 * 1024;
pub const OUTPUT_BYTES_MAX: usize = 64 * 1024;

pub const UNKNOWN_INSTALL: &str = "harness_update_unknown_install";
pub const WITH_APP: &str = "harness_update_with_app";
pub const TOOL_MISSING: &str = "harness_update_tool_missing";
pub const TOOL_UNSAFE: &str = "harness_update_tool_unsafe";
pub const NEEDS_ADMIN: &str = "harness_update_needs_admin";
pub const UNSUPPORTED_HERE: &str = "harness_update_unsupported_here";
pub const CHECK_UNREADABLE: &str = "harness_update_check_unreadable";
pub const NO_INSTALLER: &str = "harness_install_no_tool";
pub const NEEDS_NODE: &str = "harness_install_needs_node";

const VERSION_WORDS_MAX: usize = 8;
const VERSION_PARTS_MAX: usize = 6;
const PATH_PARTS_MAX: usize = 64;
const NPM_FLAGS_MAX: usize = 1;
const BUNDLED: &str = "rcli";
/// The macOS group of administrators, whose members can already act as root.
#[cfg(unix)]
const ADMIN_GROUP: u32 = 80;
/// Makes `brew outdated` list a cask that updates itself, which it leaves out otherwise
/// while a named `brew upgrade` of the same cask does upgrade it.
const GREEDY: &str = "--greedy-auto-updates";
const MODULES: &str = "node_modules";

const _: () = assert!(3 + NPM_FLAGS_MAX <= ARGS_MAX);
const _: () = assert!(VERSION_PARTS_MAX * 2 <= VERSION_BYTES_MAX);

/// How a harness on this machine was installed, which decides the one updater the engine may
/// run for it. Built from the real path of its PATH entry only; nothing is executed to tell.
/// The names inside are always names from the compiled table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Method {
    SelfUpdate,
    Homebrew { cask: String },
    HomebrewFormula { formula: String },
    Npm { package: String },
    WithApp,
    Unknown,
}

/// What the last check for a newer version found. `NoCheck` is a self-updating install with
/// online lookups off; `Failed` carries a refusal code, never text from a tool or a server.
/// A version inside `Newer` is short and holds only version characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Latest {
    NotChecked,
    NoCheck,
    UpToDate,
    Newer { version: String },
    Failed { code: String },
}

/// How the last update or install of one harness ended. `output` is the masked last lines of
/// the tool, `from` and `to` are the installed versions before and after, `code` is set when
/// it did not end well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastUpdate {
    pub ok: bool,
    pub code: Option<String>,
    pub output: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub ended_ms: i64,
}

/// Everything a client shows on one harness row about versions. `command` is the exact update
/// line of an installed harness, `install` the exact install line of a missing one, `refusal`
/// the code for why a button is not offered, `guide` the vendor's page for that case,
/// `needs` what an installed harness lacks before it can run, as the tool resolver reads it,
/// and `sign_in_min` the oldest version CrazyAgent signs in with, for a harness that signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessVersions {
    pub id: String,
    pub method: Method,
    pub installed: Option<String>,
    pub latest: Latest,
    pub command: Option<String>,
    pub install: Option<String>,
    pub refusal: Option<String>,
    pub guide: Option<String>,
    pub running: bool,
    pub last: Option<LastUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<HarnessNeeds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_min: Option<String>,
}

/// What an installed harness lacks before it can run, from the tool resolver
/// (`crate::tools::resolve_entry`): the program its first line asks for, such as `node`
/// for an npm-built Codex, and the resolver's one owner line for it. No path is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessNeeds {
    pub program: Option<String>,
    pub line: String,
}

/// The owner's choice for harness updates: check and update, check only, or neither.
/// The default is the first option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    #[default]
    CheckAndUpdate,
    CheckOnly,
    Off,
}

/// The owner's choice for when a check runs: on the button only, or also each time the
/// Harnesses page opens. The default is the button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckWhen {
    #[default]
    OnButton,
    OnOpen,
}

/// The owner's choice for reading a vendor's public release pointer to learn the newest
/// version of a self-updating install. Off means the engine never makes that request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineLookup {
    #[default]
    On,
    Off,
}

/// The owner's choice for offering Install on a harness that is not installed.
/// Off shows the vendor's guide only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFromApp {
    #[default]
    On,
    Off,
}

/// The four harness update settings as one saved row and one wire value.
/// Every field defaults to its first option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    pub mode: UpdateMode,
    pub when: CheckWhen,
    pub online: OnlineLookup,
    pub install: InstallFromApp,
}

/// The folders an install is recognized by: the owner's home, the Homebrew prefixes in the
/// order they are tried, whether Homebrew casks can be installed on this platform, the npm
/// that installs npm harnesses into the engine's own prefix, when a usable Node is found, and
/// the tool resolver's places a harness runs from. Tests pass temp folders; the engine
/// passes the real ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub home: PathBuf,
    pub brew_prefixes: Vec<PathBuf>,
    pub casks: bool,
    pub npm: Option<NpmInstall>,
    pub places: crate::tools::Places,
}

/// The npm an npm harness installs with and the node entry it runs, found by the one
/// Node test of setup (`crate::setup::node::usable`), the folders its run puts first (Node's
/// own, then the prefix's `bin`) and the engine-owned prefix it installs into, never global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmInstall {
    pub node: PathBuf,
    pub program: PathBuf,
    pub search: Vec<PathBuf>,
    pub prefix: PathBuf,
}

/// One command the engine may start: an absolute program, arguments that are all strings of
/// the compiled table, and the folders that go first on the child's search path. `prefix`
/// is set for npm alone: the prefix the harness was found in, which the runner pins as
/// npm's global prefix so a prefix in the owner's npmrc never moves the install.
/// Nothing in it ever comes from a file, the environment or the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixed {
    pub program: PathBuf,
    pub args: Vec<&'static str>,
    pub search: Vec<PathBuf>,
    pub prefix: Option<PathBuf>,
}

/// How a vendor's release pointer names the newest version: the whole body, or one string
/// field of a JSON object with a fixed prefix in front of the version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerFormat {
    PlainVersion,
    JsonTag {
        field: &'static str,
        prefix: &'static str,
    },
}

/// A vendor's public release pointer: the fixed HTTPS address, how its reply names the
/// version and the most bytes a reply may have. Only the compiled table holds any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    pub url: &'static str,
    pub format: PointerFormat,
    pub bytes_max: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrewKind {
    Formula,
    Cask,
}

struct Brew {
    kind: BrewKind,
    name: &'static str,
    install: bool,
}

struct Npm {
    scope: &'static str,
    name: &'static str,
    package: &'static str,
    spec: &'static str,
    flags: &'static [&'static str],
}

struct SelfUpdate {
    root: &'static str,
    args: &'static [&'static str],
    pointer: Pointer,
}

struct Recipe {
    id: &'static str,
    own: Option<SelfUpdate>,
    brews: &'static [Brew],
    npm: Option<Npm>,
    guide: &'static str,
}

/// A Homebrew formula the app may install and update. Input: its name in homebrew/core.
/// Output: the table entry.
const fn formula(name: &'static str) -> Brew {
    Brew {
        kind: BrewKind::Formula,
        name,
        install: true,
    }
}

/// A Homebrew cask the app may install and update. Input: its token in homebrew/cask.
/// Output: the table entry.
const fn cask(name: &'static str) -> Brew {
    Brew {
        kind: BrewKind::Cask,
        name,
        install: true,
    }
}

/// A Homebrew name the app recognizes and updates but never installs (deprecated, or a
/// second channel). Inputs: the kind and the name. Output: the table entry.
const fn known(kind: BrewKind, name: &'static str) -> Brew {
    Brew {
        kind,
        name,
        install: false,
    }
}

/// An npm package the app may install and update. Inputs: the scope ("" for none), the
/// name inside it, the full package name, the install spec and the vendor's documented
/// flags. Output: the table entry.
const fn npm(
    scope: &'static str,
    name: &'static str,
    package: &'static str,
    spec: &'static str,
    flags: &'static [&'static str],
) -> Option<Npm> {
    assert!(flags.len() <= NPM_FLAGS_MAX);
    Some(Npm {
        scope,
        name,
        package,
        spec,
        flags,
    })
}

const CLAUDE_POINTER: Pointer = Pointer {
    url: "https://downloads.claude.ai/claude-code-releases/latest",
    format: PointerFormat::PlainVersion,
    bytes_max: 64,
};
const CODEX_POINTER: Pointer = Pointer {
    url: "https://releases.openai.com/codex/channels/latest",
    format: PointerFormat::JsonTag {
        field: "tag_name",
        prefix: "rust-v",
    },
    bytes_max: POINTER_BYTES_MAX,
};
const _: () = assert!(
    CLAUDE_POINTER.bytes_max <= POINTER_BYTES_MAX && CODEX_POINTER.bytes_max <= POINTER_BYTES_MAX
);

static RECIPES: [Recipe; 22] = [
    Recipe {
        id: "claude-code",
        own: Some(SelfUpdate {
            root: ".local/share/claude/versions",
            args: &["update"],
            pointer: CLAUDE_POINTER,
        }),
        brews: &[
            cask("claude-code"),
            known(BrewKind::Cask, "claude-code@latest"),
        ],
        npm: npm(
            "@anthropic-ai",
            "claude-code",
            "@anthropic-ai/claude-code",
            "@anthropic-ai/claude-code@latest",
            &[],
        ),
        guide: "https://code.claude.com/docs/en/setup",
    },
    Recipe {
        id: "codex",
        own: Some(SelfUpdate {
            root: ".codex/packages/standalone",
            args: &["update"],
            pointer: CODEX_POINTER,
        }),
        brews: &[cask("codex")],
        npm: npm("@openai", "codex", "@openai/codex", "@openai/codex", &[]),
        guide: "https://developers.openai.com/codex/cli",
    },
    Recipe {
        id: "opencode",
        own: None,
        brews: &[formula("opencode")],
        npm: npm("@opencode", "cli", "@opencode/cli", "@opencode/cli", &[]),
        guide: "https://opencode.ai/docs/",
    },
    Recipe {
        id: "gemini-cli",
        own: None,
        brews: &[known(BrewKind::Formula, "gemini-cli")],
        npm: npm(
            "@google",
            "gemini-cli",
            "@google/gemini-cli",
            "@google/gemini-cli",
            &[],
        ),
        guide: "https://github.com/google-gemini/gemini-cli#readme",
    },
    Recipe {
        id: "cursor-agent",
        own: None,
        brews: &[cask("cursor-cli")],
        npm: None,
        guide: "https://cursor.com/docs/cli/installation",
    },
    Recipe {
        id: "fx",
        own: None,
        brews: &[],
        npm: None,
        guide: "https://vercel.com/docs/ai-gateway/coding-agents/fx",
    },
    Recipe {
        id: "antigravity",
        own: None,
        brews: &[cask("antigravity-cli")],
        npm: None,
        guide: "https://antigravity.google/docs/getting-started",
    },
    Recipe {
        id: "openclaw",
        own: None,
        brews: &[formula("openclaw-cli")],
        npm: npm(
            "",
            "openclaw",
            "openclaw",
            "openclaw@latest",
            &["--allow-scripts=openclaw"],
        ),
        guide: "https://docs.openclaw.ai/install",
    },
    Recipe {
        id: "pi",
        own: None,
        brews: &[formula("pi-coding-agent")],
        npm: npm(
            "@earendil-works",
            "pi-coding-agent",
            "@earendil-works/pi-coding-agent",
            "@earendil-works/pi-coding-agent",
            &["--ignore-scripts"],
        ),
        guide: "https://pi.dev/",
    },
    Recipe {
        id: "amp",
        own: None,
        brews: &[],
        npm: npm(
            "@sourcegraph",
            "amp",
            "@sourcegraph/amp",
            "@sourcegraph/amp",
            &[],
        ),
        guide: "https://ampcode.com/manual",
    },
    Recipe {
        id: "goose",
        own: None,
        brews: &[formula("block-goose-cli")],
        npm: None,
        guide: "https://goose-docs.ai/docs/getting-started/installation",
    },
    Recipe {
        id: "aider",
        own: None,
        brews: &[formula("aider")],
        npm: None,
        guide: "https://aider.chat/docs/install.html",
    },
    Recipe {
        id: "qwen-code",
        own: None,
        brews: &[formula("qwen-code")],
        npm: npm(
            "@qwen-code",
            "qwen-code",
            "@qwen-code/qwen-code",
            "@qwen-code/qwen-code@latest",
            &[],
        ),
        guide: "https://github.com/QwenLM/qwen-code#readme",
    },
    Recipe {
        id: "crush",
        own: None,
        brews: &[],
        npm: npm(
            "@charmland",
            "crush",
            "@charmland/crush",
            "@charmland/crush",
            &[],
        ),
        guide: "https://github.com/charmbracelet/crush#readme",
    },
    Recipe {
        id: "droid",
        own: None,
        brews: &[cask("droid")],
        npm: npm("", "droid", "droid", "droid", &[]),
        guide: "https://docs.factory.ai/cli/getting-started/quickstart",
    },
    Recipe {
        id: "kiro",
        own: None,
        brews: &[cask("kiro-cli")],
        npm: None,
        guide: "https://kiro.dev/docs/getting-started/installation/",
    },
    Recipe {
        id: "copilot",
        own: None,
        brews: &[
            cask("copilot-cli"),
            known(BrewKind::Cask, "copilot-cli@prerelease"),
        ],
        npm: npm(
            "@github",
            "copilot",
            "@github/copilot",
            "@github/copilot",
            &[],
        ),
        guide: "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli",
    },
    Recipe {
        id: "cline",
        own: None,
        brews: &[known(BrewKind::Formula, "cline")],
        npm: npm("", "cline", "cline", "cline", &[]),
        guide: "https://docs.cline.bot/usage/cli-overview",
    },
    Recipe {
        id: "continue",
        own: None,
        brews: &[],
        npm: npm(
            "@continuedev",
            "cli",
            "@continuedev/cli",
            "@continuedev/cli",
            &[],
        ),
        guide: "https://github.com/continuedev/continue/tree/main/extensions/cli",
    },
    Recipe {
        id: "openhands",
        own: None,
        brews: &[],
        npm: None,
        guide: "https://docs.openhands.dev/openhands/usage/cli/installation",
    },
    Recipe {
        id: "auggie",
        own: None,
        brews: &[],
        npm: npm(
            "@augmentcode",
            "auggie",
            "@augmentcode/auggie",
            "@augmentcode/auggie",
            &[],
        ),
        guide: "https://docs.augmentcode.com/cli/setup-auggie/install-auggie-cli",
    },
    Recipe {
        id: "kimi",
        own: None,
        brews: &[formula("kimi-code"), known(BrewKind::Formula, "kimi-cli")],
        npm: npm(
            "@moonshot-ai",
            "kimi-code",
            "@moonshot-ai/kimi-code",
            "@moonshot-ai/kimi-code",
            &[],
        ),
        guide: "https://moonshotai.github.io/kimi-code/en/guides/getting-started",
    },
];
const _: () = assert!(RECIPES.len() == 22);

/// Every package name, install spec and flag in the table, for the tests that hold the table
/// to plain names. Output: the strings, in table order.
#[cfg(all(test, unix))]
fn table_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for recipe in &RECIPES {
        names.extend(recipe.brews.iter().map(|brew| brew.name));
        if let Some(npm) = &recipe.npm {
            names.extend([npm.package, npm.spec]);
            names.extend(npm.flags);
        }
    }
    names
}

/// The Homebrew names of one harness the app may install, for the test that holds every row
/// to at most one. Input: a catalog id. Output: the names, empty without a recipe.
#[cfg(all(test, unix))]
fn install_names(id: &str) -> Vec<&'static str> {
    recipe(id).map_or(Vec::new(), |recipe| {
        recipe
            .brews
            .iter()
            .filter(|brew| brew.install)
            .map(|brew| brew.name)
            .collect()
    })
}

fn recipe(id: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|recipe| recipe.id == id)
}

const fn flag(kind: BrewKind) -> &'static str {
    match kind {
        BrewKind::Formula => "--formula",
        BrewKind::Cask => "--cask",
    }
}

const fn cellar(kind: BrewKind) -> &'static str {
    match kind {
        BrewKind::Formula => "Cellar",
        BrewKind::Cask => "Caskroom",
    }
}

/// Where a recognized install lives and which table entry it matched.
enum Found<'a> {
    Own(&'static SelfUpdate),
    Brew {
        prefix: &'a Path,
        brew: &'static Brew,
    },
    Npm {
        prefix: PathBuf,
        modules: PathBuf,
        npm: &'static Npm,
    },
}

/// Matches a harness's real path against its table row, on whole path parts only, in the
/// order own installer, Homebrew, npm (a formula may wrap an npm package inside its Cellar).
/// Inputs: the harness id, the real path and the roots. Output: the match or a refusal code.
fn found<'a>(id: &str, real: &Path, roots: &'a Roots) -> Result<Found<'a>, &'static str> {
    let id = harnesses::canonical(id);
    if id == BUNDLED {
        return Err(WITH_APP);
    }
    let recipe = recipe(id).ok_or(UNKNOWN_INSTALL)?;
    let plain = real.is_absolute()
        && real
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    if !plain {
        return Err(UNKNOWN_INSTALL);
    }
    if let Some(own) = &recipe.own
        && roots.home.is_absolute()
        && real.starts_with(roots.home.join(own.root))
    {
        return Ok(Found::Own(own));
    }
    for prefix in &roots.brew_prefixes {
        for brew in recipe.brews {
            let folder = prefix.join(cellar(brew.kind)).join(brew.name);
            if real
                .strip_prefix(&folder)
                .is_ok_and(|rest| rest != Path::new(""))
            {
                return Ok(Found::Brew { prefix, brew });
            }
        }
    }
    if let Some(npm) = &recipe.npm
        && let Some((prefix, modules)) = npm_folders(npm, real)
    {
        return Ok(Found::Npm {
            prefix,
            modules,
            npm,
        });
    }
    Err(UNKNOWN_INSTALL)
}

/// Finds the npm prefix of a real path that sits inside the table's package: the first
/// `node_modules` part must be followed by the scope and the package name (or the name plus
/// `-` and a platform suffix), or for an unscoped package by exactly its name.
/// Inputs: the table entry and the real path. Output: the prefix and its `node_modules`.
fn npm_folders(npm: &Npm, real: &Path) -> Option<(PathBuf, PathBuf)> {
    let parts: Vec<&OsStr> = real
        .components()
        .take(PATH_PARTS_MAX + 1)
        .map(Component::as_os_str)
        .collect();
    if parts.len() > PATH_PARTS_MAX {
        return None;
    }
    let at = parts.iter().position(|part| *part == OsStr::new(MODULES))?;
    let part = |index: usize| parts.get(index).and_then(|part| part.to_str());
    let inside = if npm.scope.is_empty() {
        part(at + 1) == Some(npm.name)
    } else {
        part(at + 1) == Some(npm.scope)
            && part(at + 2).is_some_and(|name| {
                name == npm.name
                    || name
                        .strip_prefix(npm.name)
                        .is_some_and(|rest| rest.starts_with('-'))
            })
    };
    if !inside {
        return None;
    }
    let lib = at > 0 && parts[at - 1] == OsStr::new("lib");
    let prefix: PathBuf = parts[..at - usize::from(lib)].iter().collect();
    let modules: PathBuf = parts[..=at].iter().collect();
    assert!(modules.starts_with(&prefix));
    assert!(modules.ends_with(MODULES));
    Some((prefix, modules))
}

/// Tells how a harness was installed from the real path of its PATH entry alone.
/// Inputs: the harness id or alias, the real path and the roots. Output: the method;
/// `Unknown` for anything the table does not name exactly.
pub fn method(id: &str, real: &Path, roots: &Roots) -> Method {
    match found(id, real, roots) {
        Ok(Found::Own(_)) => Method::SelfUpdate,
        Ok(Found::Brew { brew, .. }) => match brew.kind {
            BrewKind::Cask => Method::Homebrew {
                cask: brew.name.to_owned(),
            },
            BrewKind::Formula => Method::HomebrewFormula {
                formula: brew.name.to_owned(),
            },
        },
        Ok(Found::Npm { npm, .. }) => Method::Npm {
            package: npm.package.to_owned(),
        },
        Err(WITH_APP) => Method::WithApp,
        Err(_) => Method::Unknown,
    }
}

fn brew_fixed(prefix: &Path, args: Vec<&'static str>) -> Fixed {
    let bin = prefix.join("bin");
    Fixed {
        program: bin.join("brew"),
        args,
        search: vec![bin],
        prefix: None,
    }
}

fn npm_fixed(prefix: &Path, args: Vec<&'static str>) -> Fixed {
    let bin = prefix.join("bin");
    Fixed {
        program: bin.join("npm"),
        args,
        search: vec![bin],
        prefix: Some(prefix.to_path_buf()),
    }
}

/// The npm command for one npm prefix: the engine's npm with its search folders when the
/// prefix is the engine's own, else the npm in that prefix's `bin`. Inputs: the prefix, the
/// roots and the arguments. Output: the command, not yet checked.
fn npm_for(prefix: &Path, roots: &Roots, args: Vec<&'static str>) -> Fixed {
    match roots.npm.as_ref().filter(|engine| engine.prefix == prefix) {
        Some(engine) => engine_npm_fixed(engine, args),
        None => npm_fixed(prefix, args),
    }
}

fn engine_npm_fixed(engine: &NpmInstall, args: Vec<&'static str>) -> Fixed {
    assert!(!engine.search.is_empty() && engine.search.len() <= 2);
    assert!(engine.prefix.is_absolute());
    Fixed {
        program: engine.program.clone(),
        args,
        search: engine.search.clone(),
        prefix: Some(engine.prefix.clone()),
    }
}

fn npm_install_args(npm: &'static Npm) -> Vec<&'static str> {
    let mut args = vec!["install", "-g"];
    args.extend(npm.flags);
    args.push(npm.spec);
    args
}

/// Passes a built command only when its program is a tool this user can trust.
/// Input: the command. Output: the same command, or the refusal code of the tool check.
fn checked(fixed: Fixed) -> Result<Fixed, &'static str> {
    trusted(&fixed.program)?;
    assert!(fixed.program.is_absolute());
    assert!(fixed.args.len() <= ARGS_MAX);
    assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
    assert!(fixed.prefix.as_deref().is_none_or(Path::is_absolute));
    Ok(fixed)
}

/// Builds the one update command of an installed harness: its own updater, or the Homebrew
/// or npm that installed it, by absolute path. A self-update runs the PATH entry, the
/// launcher the vendor's updater repoints, never the versioned real file, and only while
/// that entry still resolves to the real path the method was decided from.
/// Inputs: the harness id, its PATH entry, the entry's real path and the roots.
/// Output: the command, or the refusal code.
pub fn update_command(
    id: &str,
    entry: &Path,
    real: &Path,
    roots: &Roots,
) -> Result<Fixed, &'static str> {
    match found(id, real, roots)? {
        Found::Own(own) => {
            let folder = entry.parent().ok_or(TOOL_MISSING)?;
            if std::fs::canonicalize(entry).ok().as_deref() != Some(real) {
                return Err(UNKNOWN_INSTALL);
            }
            checked(Fixed {
                program: entry.to_path_buf(),
                args: own.args.to_vec(),
                search: vec![folder.to_path_buf()],
                prefix: None,
            })
        }
        Found::Brew { prefix, brew } => checked(brew_fixed(
            prefix,
            vec!["upgrade", flag(brew.kind), brew.name],
        )),
        Found::Npm {
            prefix,
            modules,
            npm,
        } => {
            let fixed = checked(npm_for(&prefix, roots, npm_install_args(npm)))?;
            if owned(&modules) {
                Ok(fixed)
            } else {
                Err(NEEDS_ADMIN)
            }
        }
    }
}

/// Builds the command that asks the installing tool for a newer version without installing.
/// Inputs: the harness id, the real path and the roots. Output: the command, None for an
/// install that updates itself (it has no check-only command), or the refusal code.
pub fn check_command(id: &str, real: &Path, roots: &Roots) -> Result<Option<Fixed>, &'static str> {
    match found(id, real, roots)? {
        Found::Own(_) => Ok(None),
        Found::Brew { prefix, brew } => {
            let args = match brew.kind {
                BrewKind::Cask => vec!["outdated", flag(brew.kind), GREEDY, "--json=v2", brew.name],
                BrewKind::Formula => vec!["outdated", flag(brew.kind), "--json=v2", brew.name],
            };
            checked(brew_fixed(prefix, args)).map(Some)
        }
        Found::Npm { prefix, npm, .. } => checked(npm_for(
            &prefix,
            roots,
            vec!["view", npm.package, "version"],
        ))
        .map(Some),
    }
}

/// Builds the one install command of a harness that is not installed: Homebrew first with
/// the table's installable name (casks only where `roots.casks`), else the found npm
/// (`roots.npm`) with the table's package and the vendor's flags into the engine's prefix.
/// An untrusted tool is skipped; an npm harness with no usable Node is refused as needing
/// Node. Inputs: the harness id and the roots. Output: the command, or the refusal code.
pub fn install_command(id: &str, roots: &Roots) -> Result<Fixed, &'static str> {
    let id = harnesses::canonical(id);
    if id == BUNDLED {
        return Err(WITH_APP);
    }
    let recipe = recipe(id).ok_or(UNKNOWN_INSTALL)?;
    let brew = recipe
        .brews
        .iter()
        .find(|brew| brew.install && (brew.kind == BrewKind::Formula || roots.casks));
    let with_brew = brew.and_then(|brew| {
        let args = || vec!["install", flag(brew.kind), brew.name];
        roots
            .brew_prefixes
            .iter()
            .find_map(|prefix| checked(brew_fixed(prefix, args())).ok())
    });
    let with_npm = || {
        let npm = recipe.npm.as_ref()?;
        let engine = roots.npm.as_ref()?;
        checked(engine_npm_fixed(engine, npm_install_args(npm))).ok()
    };
    let refusal = if recipe.npm.is_some() && roots.npm.is_none() {
        NEEDS_NODE
    } else {
        NO_INSTALLER
    };
    let fixed = with_brew.or_else(with_npm).ok_or(refusal)?;
    assert!(fixed.args.first() == Some(&"install"));
    assert!(fixed.args.len() <= ARGS_MAX);
    assert!(fixed.program.is_absolute());
    Ok(fixed)
}

/// Checks that a program is safe to start as this user: it resolves to a regular runnable
/// file owned by root or this user that neither the group nor everyone can write, and no one
/// else can swap it through a folder everyone can write. A folder its group can write is
/// accepted on macOS only for the administrators (Homebrew's folders), who can already act
/// as root, since every local user shares the staff group; elsewhere only for this user's
/// own group. Input: the absolute program path. Output: Ok or the refusal code.
#[cfg(unix)]
fn trusted(program: &Path) -> Result<(), &'static str> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !program.is_absolute() {
        return Err(TOOL_MISSING);
    }
    let target = std::fs::canonicalize(program).map_err(|_| TOOL_MISSING)?;
    let metadata = std::fs::metadata(&target).map_err(|_| TOOL_MISSING)?;
    let user = rustix::process::geteuid().as_raw();
    let group = rustix::process::getegid().as_raw();
    let trusted_group = if cfg!(target_os = "macos") {
        ADMIN_GROUP
    } else {
        group
    };
    let open_folder = |path: &Path| {
        path.parent()
            .and_then(|folder| std::fs::metadata(folder).ok())
            .is_none_or(|folder| {
                let mode = folder.permissions().mode();
                mode & 0o002 != 0 || (mode & 0o020 != 0 && folder.gid() != trusted_group)
            })
    };
    let mode = metadata.permissions().mode();
    let safe = metadata.is_file()
        && mode & 0o111 != 0
        && mode & 0o022 == 0
        && (metadata.uid() == 0 || metadata.uid() == user)
        && !open_folder(program)
        && !open_folder(&target);
    if safe { Ok(()) } else { Err(TOOL_UNSAFE) }
}

/// Refuses every tool where the engine has no safe way to check and start one.
/// Input: the program path, unused. Output: the refusal code.
#[cfg(not(unix))]
fn trusted(_program: &Path) -> Result<(), &'static str> {
    Err(UNSUPPORTED_HERE)
}

/// Tells whether this user owns a folder, so a global npm install needs no administrator.
/// Input: the folder. Output: true only for a folder that exists and is this user's.
#[cfg(unix)]
fn owned(folder: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(folder).is_ok_and(|metadata| {
        metadata.is_dir() && metadata.uid() == rustix::process::geteuid().as_raw()
    })
}

/// Reports no folder as this user's where ownership cannot be read.
/// Input: the folder, unused. Output: false.
#[cfg(not(unix))]
fn owned(_folder: &Path) -> bool {
    false
}

/// Writes a command as the one line the owner confirms: the program path and the arguments
/// with single spaces. Input: the command. Output: the line.
pub fn shown(fixed: &Fixed) -> String {
    assert!(fixed.args.len() <= ARGS_MAX);
    assert!(
        fixed
            .args
            .iter()
            .all(|arg| !arg.is_empty() && !arg.contains(char::is_whitespace))
    );
    let mut line = fixed.program.display().to_string();
    for arg in &fixed.args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}

/// The vendor's install and update page for a harness.
/// Input: the harness id or alias. Output: the HTTPS link, None for RCLI and unknown ids.
pub fn guide(id: &str) -> Option<&'static str> {
    recipe(harnesses::canonical(id)).map(|recipe| recipe.guide)
}

/// The vendor release pointer of a harness that can update itself.
/// Input: the harness id or alias. Output: the pointer, None for every other harness.
pub fn pointer(id: &str) -> Option<&'static Pointer> {
    recipe(harnesses::canonical(id))?
        .own
        .as_ref()
        .map(|own| &own.pointer)
}

/// Splits a version into its numbers. Input: text. Output: one to six numbers when the text
/// is only ASCII digits joined by single dots, else None.
fn numbers(text: &str) -> Option<Vec<u64>> {
    if text.len() > VERSION_BYTES_MAX {
        return None;
    }
    let parts: Vec<&str> = text.split('.').take(VERSION_PARTS_MAX + 1).collect();
    if parts.len() > VERSION_PARTS_MAX {
        return None;
    }
    parts
        .iter()
        .map(|part| {
            let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
            digits.then(|| part.parse().ok()).flatten()
        })
        .collect()
}

/// Tells whether text is a plain version: two to six numbers joined by single dots, at most
/// `VERSION_BYTES_MAX` bytes and nothing else. Input: text. Output: the answer.
pub fn plain_version(text: &str) -> bool {
    numbers(text).is_some_and(|parts| parts.len() >= 2)
}

/// Tells whether text is a version Homebrew may print for an outdated package: it starts
/// with a digit and holds only ASCII letters, digits and `.`, `-`, `_`, `+`, `,`.
/// Input: text. Output: the answer.
fn package_version(text: &str) -> bool {
    text.len() <= VERSION_BYTES_MAX
        && text
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_+,".contains(&byte))
}

/// Compares two versions number by number, a missing number counting as zero.
/// Inputs: the candidate and the installed version. Output: true when the candidate is higher;
/// an installed version that is not numbers counts as zero.
fn higher(candidate: &str, installed: &str) -> bool {
    let candidate = numbers(candidate).unwrap_or_default();
    let installed = numbers(installed).unwrap_or_default();
    assert!(candidate.len() <= VERSION_PARTS_MAX && installed.len() <= VERSION_PARTS_MAX);
    (0..VERSION_PARTS_MAX)
        .map(|index| {
            let one = candidate.get(index).copied().unwrap_or(0);
            let other = installed.get(index).copied().unwrap_or(0);
            one.cmp(&other)
        })
        .find(|order| order.is_ne())
        .is_some_and(|order| order.is_gt())
}

/// Tells whether an installed version reads lower than a minimum, number by number with a
/// missing number counting as zero. Inputs: the installed version and the plain minimum.
/// Output: true only when the installed version is plain and lower; an installed version that
/// is not plain is unknown, so never below.
pub fn below(installed: &str, minimum: &str) -> bool {
    assert!(plain_version(minimum), "minimum {minimum:?}");
    plain_version(installed) && higher(minimum, installed)
}

fn unreadable() -> Latest {
    Latest::Failed {
        code: CHECK_UNREADABLE.to_owned(),
    }
}

/// Turns a candidate version into the check result. Inputs: the candidate and the installed
/// version. Output: `Newer` when it is a plain version and higher, `UpToDate` when it is not
/// higher, unreadable when it is not a plain version.
fn compared(candidate: &str, installed: &str) -> Latest {
    if !plain_version(candidate) {
        return unreadable();
    }
    if !higher(candidate, installed) {
        return Latest::UpToDate;
    }
    assert!(plain_version(candidate));
    assert!(candidate.len() <= VERSION_BYTES_MAX);
    Latest::Newer {
        version: candidate.to_owned(),
    }
}

/// Reads the installed version from a harness's `--version` output: among the first words of
/// the first line, the first that is a plain version after one optional leading `v`.
/// Input: the output. Output: the version, None when no word has that shape.
pub fn installed_version(output: &str) -> Option<String> {
    let line = output.lines().next()?;
    let version = line
        .split_whitespace()
        .take(VERSION_WORDS_MAX)
        .map(|word| word.strip_prefix('v').unwrap_or(word))
        .find(|word| plain_version(word))?;
    assert!(version.len() <= VERSION_BYTES_MAX);
    Some(version.to_owned())
}

/// Reads `brew outdated --json=v2` for one package. Inputs: the output, the list to look in
/// (`casks` or `formulae`) and the table name. Output: `Newer` with Homebrew's version when
/// the package is listed, `UpToDate` when it is not, unreadable for any other reply.
fn outdated(output: &str, list: &str, name: &str) -> Latest {
    if output.len() > OUTPUT_BYTES_MAX {
        return unreadable();
    }
    let Some(start) = output.find('{') else {
        return unreadable();
    };
    let mut values = serde_json::Deserializer::from_str(&output[start..]).into_iter::<Value>();
    let Some(Ok(value)) = values.next() else {
        return unreadable();
    };
    let Some(rows) = value.get(list).and_then(Value::as_array) else {
        return unreadable();
    };
    let listed = rows
        .iter()
        .find(|row| row.get("name").and_then(Value::as_str) == Some(name));
    let Some(row) = listed else {
        return Latest::UpToDate;
    };
    match row.get("current_version").and_then(Value::as_str) {
        Some(version) if package_version(version) => Latest::Newer {
            version: version.to_owned(),
        },
        _ => unreadable(),
    }
}

/// Turns the output of a check command into the check result. Inputs: the install method,
/// the tool's output and the installed version. Output: the result; a method with no check
/// command is `NotChecked`.
pub fn latest(method: &Method, output: &str, installed: &str) -> Latest {
    match method {
        Method::Homebrew { cask } => outdated(output, "casks", cask),
        Method::HomebrewFormula { formula } => outdated(output, "formulae", formula),
        Method::Npm { .. } => compared(output.lines().next().unwrap_or("").trim(), installed),
        Method::SelfUpdate | Method::WithApp | Method::Unknown => Latest::NotChecked,
    }
}

/// Turns a vendor pointer's reply into the check result, keeping nothing but one plain
/// version. Inputs: the pointer, the reply body and the installed version. Output: `Newer`,
/// `UpToDate`, or unreadable for a body that is too large, not text or not exactly a version.
pub fn latest_from_pointer(pointer: &Pointer, body: &[u8], installed: &str) -> Latest {
    assert!(pointer.bytes_max <= POINTER_BYTES_MAX);
    if body.len() as u64 > pointer.bytes_max {
        return unreadable();
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return unreadable();
    };
    match pointer.format {
        PointerFormat::PlainVersion => {
            let line = text.strip_suffix('\n').unwrap_or(text);
            compared(line.trim_matches(' '), installed)
        }
        PointerFormat::JsonTag { field, prefix } => {
            let Ok(value) = serde_json::from_str::<Value>(text) else {
                return unreadable();
            };
            let tag = value.get(field).and_then(Value::as_str);
            match tag.and_then(|tag| tag.strip_prefix(prefix)) {
                Some(version) => compared(version, installed),
                None => unreadable(),
            }
        }
    }
}
