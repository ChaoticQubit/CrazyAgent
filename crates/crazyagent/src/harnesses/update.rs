mod recipes;
mod run;
#[cfg(all(test, unix))]
#[path = "update/tests/scripted.rs"]
mod scripted;
mod state;
#[cfg(all(test, unix))]
mod tests;
mod turn;
pub(crate) use turn::{Turns, wait_max};

#[cfg(not(windows))]
pub use run::run;
pub use run::{
    AS_ROOT, CHECK_TIME, LOOKUP_FAILED, POINTER_TIME, Ran, SHOWN_BYTES_MAX, START_FAILED, TIMEOUT,
    UPDATE_TIME, VERSION_TIME, lookup, masked, refused_user,
};
pub(crate) use state::needs;
#[cfg(test)]
pub use state::set_settings;
pub use state::{
    BUSY, CHECK_FAILED, CHECK_ONLY, INSTALL_ALREADY, INSTALL_FAILED, INSTALL_LIST_INVALID,
    INSTALL_NOT_FOUND, INSTALL_OFF, NOT_INSTALLED, SETTINGS_UNREADABLE, UNKNOWN_HARNESS,
    UPDATE_FAILED, UPDATE_OFF, Updates, initialize, settings,
};

use crate::harnesses;
use kernel_processes::install::{InstallPaths, InstallRecipe, Verb};
use recipes::{Brew, BrewKind, Npm, SelfUpdate, recipe};
#[cfg(all(test, unix))]
use recipes::{Recipe, install_names, table_names};
#[cfg(all(test, unix))]
use run::read_pointer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf, Prefix};

pub const VERSION_BYTES_MAX: usize = 64;
pub const ARGS_MAX: usize = 5;
/// The Homebrew prefixes the engine trusts for install, check and update on this platform,
/// as the tool resolver looks in them: Apple silicon's and Intel's everywhere, Linuxbrew's
/// only on Linux, so a Mac never reads or runs a brew from the home automount.
#[cfg(target_os = "linux")]
pub const BREW_PREFIXES: [&str; 3] = ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"];
/// The Homebrew prefixes the engine trusts for install, check and update on this platform,
/// as the tool resolver looks in them: Apple silicon's and Intel's; Linuxbrew's is Linux only.
#[cfg(not(target_os = "linux"))]
pub const BREW_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];
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
const UPDATE_SHELLS: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];
const SHELL_FLAGS: &str = "eupx";
const NODE_FLAGS: [&str; 3] = ["--no-warnings", "--no-deprecation", "--enable-source-maps"];

const _: () = assert!(3 + NPM_FLAGS_MAX <= ARGS_MAX);
const _: () = assert!(VERSION_PARTS_MAX * 2 <= VERSION_BYTES_MAX);

/// How a harness on this machine was installed, which decides the one updater the engine may
/// run for it. Built from the real path of its PATH entry only; nothing is executed to tell.
/// The names inside are always names from the recipes of the bundled harness manifests.
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
/// `sign_in_min` the oldest version CrazyAgent signs in with, for a harness that signs in,
/// `run_min` the oldest version CrazyAgent drives at all, from the manifest's
/// `version.minimum`, shown as "needs X or newer" beside the Update line, and `waiting` the
/// job this running job waits behind, since the two would change the same folder: a harness
/// by its catalog id, a setup job by its name ([`crate::setup::job_name`]).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_min: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
}

/// What an installed harness lacks before it can run, from the tool resolver
/// (`kernel_processes::finding::resolve_entry`): the program its first line asks for, such as `node`
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
/// the tool resolver's places a harness runs from and, on Windows, whether the engine's token
/// is elevated. Tests pass temp folders; the engine passes the real ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub home: PathBuf,
    pub brew_prefixes: Vec<PathBuf>,
    pub casks: bool,
    pub npm: Option<NpmInstall>,
    pub places: kernel_processes::finding::Places,
    #[cfg(windows)]
    pub elevated: bool,
}

/// The npm an npm harness installs with and the node entry it runs, found by the one
/// Node test of setup (`crate::setup::node_usable::usable`), the folders its run puts first (Node's
/// own, then the prefix's `bin`) and the engine-owned prefix it installs into, never global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmInstall {
    pub node: PathBuf,
    pub program: PathBuf,
    pub search: Vec<PathBuf>,
    pub prefix: PathBuf,
}

/// One command the engine may start: an absolute program, arguments that are all strings of
/// the recipes read from the bundled harness manifests, and the folders that go first on the child's search path. `prefix`
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
/// version and the most bytes a reply may have. Only the `update` block of a bundled harness
/// manifest holds any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    pub url: &'static str,
    pub format: PointerFormat,
    pub bytes_max: u64,
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
/// The path is absolute and plain: roots and folder names, after at most a drive prefix such
/// as `C:`, on Windows as plain drive text ([`kernel_sandbox::paths::windows_clean_text`]), never a
/// UNC, verbatim, device or forward-slash path, `.` or `..`.
/// Inputs: the harness id, the real path and the roots. Output: the match or a refusal code.
fn found<'a>(id: &str, real: &Path, roots: &'a Roots) -> Result<Found<'a>, &'static str> {
    let id = harnesses::canonical(id);
    if id == BUNDLED {
        return Err(WITH_APP);
    }
    let recipe = recipe(id).ok_or(UNKNOWN_INSTALL)?;
    let plain = real.is_absolute()
        && real.components().all(|part| match part {
            Component::Prefix(prefix) => matches!(prefix.kind(), Prefix::Disk(_)),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        });
    if !plain {
        return Err(UNKNOWN_INSTALL);
    }
    #[cfg(windows)]
    if !real
        .to_str()
        .is_some_and(kernel_sandbox::paths::windows_clean_text)
    {
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
/// prefix is the engine's own; on Windows, where npm keeps no npm in a prefix, the engine's
/// npm with that prefix pinned and searched after Node's folder; else the npm in that
/// prefix's `bin`. Inputs: the prefix, the roots and the arguments. Output: the command.
fn npm_for(prefix: &Path, roots: &Roots, args: Vec<&'static str>) -> Fixed {
    match roots.npm.as_ref() {
        Some(engine) if engine.prefix == prefix => engine_npm_fixed(engine, args),
        Some(engine) if cfg!(windows) => Fixed {
            program: engine.program.clone(),
            args,
            search: engine
                .node
                .parent()
                .map(Path::to_path_buf)
                .into_iter()
                .chain([prefix.to_path_buf()])
                .collect(),
            prefix: Some(prefix.to_path_buf()),
        },
        _ => npm_fixed(prefix, args),
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

/// The kernel install recipe of one bundled Homebrew name. Inputs: the name's row and the
/// verb. Output: the recipe.
fn brew_recipe(brew: &Brew, verb: Verb) -> InstallRecipe {
    let name = brew.name.to_owned();
    match brew.kind {
        BrewKind::Cask => InstallRecipe::BrewCask { name, verb },
        BrewKind::Formula => InstallRecipe::BrewFormula { name, verb },
    }
}

/// The kernel install recipe of one bundled npm package: the package, the version or tag its
/// spec names after `@` (none for a bare spec) and the vendor's flags. Inputs: the package's
/// row and the verb. Output: the recipe, or none for a spec that is not the package alone or
/// followed by `@` and a version.
fn npm_recipe(npm: &Npm, verb: Verb) -> Option<InstallRecipe> {
    let version = match npm.spec.strip_prefix(npm.package)? {
        "" => "",
        rest => rest
            .strip_prefix('@')
            .filter(|version| !version.is_empty())?,
    };
    Some(InstallRecipe::NpmGlobal {
        package: npm.package.to_owned(),
        version: version.to_owned(),
        verb,
        flags: npm.flags.iter().map(|flag| (*flag).to_owned()).collect(),
    })
}

/// The kernel install recipe of a harness's own updater, run as the program name its PATH
/// entry has. Inputs: the name and the updater's row. Output: the recipe.
fn own_recipe(program: &str, own: &SelfUpdate) -> InstallRecipe {
    InstallRecipe::SelfUpdate {
        program: program.to_owned(),
        args: own.args.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

/// Passes a built update or install command only when it is exactly the one step the
/// kernel's install template builds from its recipe ([`kernel_processes::install::steps`]):
/// the same absolute program and the same arguments in order, with npm's prefix as the
/// template's prefix. Inputs: the command and its recipe. Output: the same command, or
/// `TOOL_UNSAFE`.
fn matched(fixed: Fixed, recipe: &InstallRecipe) -> Result<Fixed, &'static str> {
    let absolute =
        fixed.program.is_absolute() && fixed.prefix.as_deref().is_none_or(Path::is_absolute);
    if !absolute {
        return Err(TOOL_UNSAFE);
    }
    let paths = InstallPaths {
        program: Some(fixed.program.clone()),
        npm_prefix: fixed.prefix.clone(),
        ..InstallPaths::default()
    };
    let built = kernel_processes::install::steps(recipe, &paths).map_err(|_| TOOL_UNSAFE)?;
    let args: Vec<OsString> = fixed.args.iter().map(OsString::from).collect();
    match built.as_slice() {
        [step] if step.program == fixed.program && step.args == args => Ok(fixed),
        _ => Err(TOOL_UNSAFE),
    }
}

fn npm_install_args(npm: &'static Npm) -> Vec<&'static str> {
    let mut args = vec!["install", "-g"];
    args.extend(npm.flags);
    args.push(npm.spec);
    args
}

/// Passes a built command only when its program is a tool this user can trust and one the
/// tool resolver would run: no part of its path, as written or real, that agents or another
/// user can change, an interpreter its first line names vetted the same way, and no link on
/// its way to the real file that could be moved before the command runs the path as written
/// ([`kernel_processes::finding::Places::link_hops_kept`]). Inputs: the command and the resolver's
/// places. Output: the same command, or the refusal code of the tool check.
fn checked(
    fixed: Fixed,
    places: &kernel_processes::finding::Places,
) -> Result<Fixed, &'static str> {
    trusted(&fixed.program)?;
    vetted(&fixed, places)?;
    if !places.link_hops_kept(&fixed.program) {
        return Err(TOOL_UNSAFE);
    }
    assert!(fixed.program.is_absolute());
    assert!(fixed.args.len() <= ARGS_MAX);
    assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
    assert!(fixed.prefix.as_deref().is_none_or(Path::is_absolute));
    Ok(fixed)
}

/// Passes a command only when the tool resolver would run its program and, at each level
/// whose script asks `env` for an interpreter by name, the copy `env` takes: the first one
/// the command's own search folders hold, since the child's PATH looks there first. That
/// copy must resolve, a link whose target is not there yet included, and the walk goes on
/// from it; a copy behind it never runs. Each of those folders that exists must be one a
/// lookup would trust ([`kernel_processes::finding::Places::kept_for_children`]), so no copy can appear
/// there before the run (RV-DS241-203). Every interpreter on the way must run the script it
/// is given and nothing it looks up ([`runs_its_script`]). Inputs: the command and the
/// resolver's places. Output: the real file the resolver resolved the program to and
/// checked, or `TOOL_UNSAFE`.
fn vetted(
    fixed: &Fixed,
    places: &kernel_processes::finding::Places,
) -> Result<PathBuf, &'static str> {
    let kept = fixed.search.iter().all(|folder| {
        std::fs::symlink_metadata(folder).is_err() || places.kept_for_children(folder)
    });
    if !kept {
        return Err(TOOL_UNSAFE);
    }
    let resolved = kernel_processes::finding::resolve_entry(&fixed.program, places)
        .map_err(|_| TOOL_UNSAFE)?;
    let program = resolved.program.clone();
    let mut at = resolved;
    for _ in 0..=kernel_processes::finding::INTERPRETER_DEPTH_MAX {
        let kernel_processes::finding::Kind::Script {
            interpreter,
            arguments,
            by_name,
        } = at.kind
        else {
            return Ok(program);
        };
        at = if by_name {
            taken_copy(fixed, &interpreter.entry, places)?.unwrap_or(*interpreter)
        } else {
            *interpreter
        };
        if !runs_its_script(&at.program, &arguments) {
            return Err(TOOL_UNSAFE);
        }
    }
    Err(TOOL_UNSAFE)
}

/// Whether an interpreter an update command's script names runs that script and nothing it
/// looks up by name, read by its real file: a shell given only `-e`, `-u`, `-p` and `-x`
/// flags, as stock Homebrew's `#!/bin/bash -pu` (privileged mode reads no startup or
/// environment files, so it only narrows), or Node (`node.exe` on Windows) given only flags
/// that change no code it runs. Any other program, such as a launcher
/// that runs a later word (`lockf`, `xargs`, `perl -S`), a shell given `-c` or another word,
/// and Node given `-e`, is refused, so no shipped update command needs more (rv-sec3 B18 F2).
/// Inputs: the interpreter's real file and the first line's words before the script.
/// Output: the answer.
fn runs_its_script(interpreter: &Path, arguments: &[String]) -> bool {
    let name = interpreter.file_name().and_then(std::ffi::OsStr::to_str);
    match name {
        Some(name) if UPDATE_SHELLS.contains(&name) => arguments.iter().all(|word| {
            word.strip_prefix('-').is_some_and(|flags| {
                !flags.is_empty() && flags.chars().all(|flag| SHELL_FLAGS.contains(flag))
            })
        }),
        Some(name)
            if name == "node" || name == "nodejs" || (cfg!(windows) && name == "node.exe") =>
        {
            arguments
                .iter()
                .all(|word| NODE_FLAGS.contains(&word.as_str()))
        }
        _ => false,
    }
}

/// The copy of an interpreter named by name that `env` takes in a command's run: the first
/// entry of that name in the command's search folders, a dangling link included, since an
/// agent could fill its target before the run. A first copy the run's user cannot start,
/// which `env` would pass over, does not resolve ([`kernel_processes::finding::resolve_entry`]), so the
/// command fails closed rather than judging the copy behind it (rv-sec3 B18 F1). Inputs:
/// the command, the interpreter the resolver found and the places. Output: that copy
/// resolved, none when no search folder holds the name, or `TOOL_UNSAFE` when the first
/// copy does not resolve.
fn taken_copy(
    fixed: &Fixed,
    found: &Path,
    places: &kernel_processes::finding::Places,
) -> Result<Option<kernel_processes::finding::Resolved>, &'static str> {
    let name = found.file_name().ok_or(TOOL_UNSAFE)?;
    let Some(copy) = fixed
        .search
        .iter()
        .map(|folder| folder.join(name))
        .find(|copy| std::fs::symlink_metadata(copy).is_ok())
    else {
        return Ok(None);
    };
    let taken = kernel_processes::finding::resolve_entry(&copy, places).map_err(|_| TOOL_UNSAFE)?;
    assert_eq!(taken.entry.file_name(), Some(name));
    Ok(Some(taken))
}

/// Refuses every harness command on Windows while the engine's token is elevated, the twin
/// of the Unix refusal to run an updater as root ([`refused_user`]): once the install is
/// known and before any tool is checked, so an elevated owner never reads that an update
/// needs an administrator. Input: the roots. Output: Ok, or `AS_ROOT`.
fn refused_here(roots: &Roots) -> Result<(), &'static str> {
    #[cfg(windows)]
    if let Some(code) = run::refused_token(roots.elevated) {
        return Err(code);
    }
    #[cfg(not(windows))]
    let _ = roots;
    Ok(())
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
    let found = found(id, real, roots)?;
    refused_here(roots)?;
    match found {
        Found::Own(own) => {
            let folder = entry.parent().ok_or(TOOL_MISSING)?;
            if kernel_sandbox::paths::canonical(entry).ok().as_deref() != Some(real) {
                return Err(UNKNOWN_INSTALL);
            }
            let name = entry
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(TOOL_UNSAFE)?;
            let fixed = Fixed {
                program: entry.to_path_buf(),
                args: own.args.to_vec(),
                search: vec![folder.to_path_buf()],
                prefix: None,
            };
            checked(matched(fixed, &own_recipe(name, own))?, &roots.places)
        }
        Found::Brew { prefix, brew } => checked(
            matched(
                brew_fixed(prefix, vec!["upgrade", flag(brew.kind), brew.name]),
                &brew_recipe(brew, Verb::Update),
            )?,
            &roots.places,
        ),
        Found::Npm {
            prefix,
            modules,
            npm,
        } => {
            let recipe = npm_recipe(npm, Verb::Update).ok_or(TOOL_UNSAFE)?;
            let fixed = checked(
                matched(npm_for(&prefix, roots, npm_install_args(npm)), &recipe)?,
                &roots.places,
            )?;
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
    let found = found(id, real, roots)?;
    if !matches!(found, Found::Own(_)) {
        refused_here(roots)?;
    }
    match found {
        Found::Own(_) => Ok(None),
        Found::Brew { prefix, brew } => {
            let args = match brew.kind {
                BrewKind::Cask => vec!["outdated", flag(brew.kind), GREEDY, "--json=v2", brew.name],
                BrewKind::Formula => vec!["outdated", flag(brew.kind), "--json=v2", brew.name],
            };
            checked(brew_fixed(prefix, args), &roots.places).map(Some)
        }
        Found::Npm { prefix, npm, .. } => checked(
            npm_for(&prefix, roots, vec!["view", npm.package, "version"]),
            &roots.places,
        )
        .map(Some),
    }
}

/// Builds the one install command of a harness that is not installed: Homebrew first with
/// the table's installable name (casks only where `roots.casks`), else the found npm
/// (`roots.npm`) with the table's package and the vendor's flags into the engine's prefix.
/// An untrusted tool is skipped; an npm harness with no usable Node is refused as needing
/// Node. Inputs: the harness id and the roots. Output: the command, or the refusal code.
pub fn install_command(id: &str, roots: &Roots) -> Result<Fixed, &'static str> {
    refused_here(roots)?;
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
        let template = brew_recipe(brew, Verb::Install);
        roots.brew_prefixes.iter().find_map(|prefix| {
            let fixed = matched(brew_fixed(prefix, args()), &template).ok()?;
            checked(fixed, &roots.places).ok()
        })
    });
    let with_npm = || {
        let npm = recipe.npm.as_ref()?;
        let engine = roots.npm.as_ref()?;
        let template = npm_recipe(npm, Verb::Install)?;
        let fixed = matched(engine_npm_fixed(engine, npm_install_args(npm)), &template).ok()?;
        checked(fixed, &roots.places).ok()
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

/// Checks that a program is safe to start as this user on Windows: a plain file that only
/// SYSTEM, the administrators, TrustedInstaller and this user can change, nor any folder
/// above it as written or at its real place ([`crazyagent_win_sandbox::program_trusted`]).
/// Input: the absolute program path. Output: Ok, `TOOL_MISSING` for a relative or missing
/// program, or `TOOL_UNSAFE`.
#[cfg(windows)]
fn trusted(program: &Path) -> Result<(), &'static str> {
    if !program.is_absolute() || std::fs::metadata(program).is_err() {
        return Err(TOOL_MISSING);
    }
    crazyagent_win_sandbox::program_trusted(program).map_err(|_| TOOL_UNSAFE)
}

/// Refuses every tool where the engine has no safe way to check and start one.
/// Input: the program path, unused. Output: the refusal code.
#[cfg(not(any(unix, windows)))]
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

/// Tells whether this user owns a folder on Windows, read from a handle opened without
/// following a link, so a global npm install there needs no administrator. Input: the
/// folder. Output: true only for a folder that exists, is no link or junction and whose
/// owner is this user's SID.
#[cfg(windows)]
fn owned(folder: &Path) -> bool {
    use crate::platform::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let Ok(held) = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(folder)
    else {
        return false;
    };
    let folder_read = held.metadata().is_ok_and(|metadata| metadata.is_dir());
    let facts = crazyagent_win_sandbox::private::facts(&held);
    let user = crazyagent_win_sandbox::private::user_sid();
    match (facts, user) {
        (Ok(facts), Ok(user)) => folder_read && !facts.reparse && facts.owner == user,
        _ => false,
    }
}

/// Reports no folder as this user's where ownership cannot be read.
/// Input: the folder, unused. Output: false.
#[cfg(not(any(unix, windows)))]
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

/// Asks an installed harness for its version under the engine's own home
/// ([`state::home_folder`]), as the updates check does, and only where the tool resolver
/// would run it and every interpreter its first line names (`version_if_runs`), so the usage
/// User-Agent read never starts a file or an interpreter copy agents or other users can
/// change. Input: the harness executable. Output: the version, None when the harness is
/// refused or cannot be asked, prints no plain version, or the home is unknown.
pub(crate) fn version_under_home(executable: &Path) -> Option<String> {
    let home = state::home_folder()?;
    state::version_if_runs(executable, &home, &crate::tools::tools_places(None))
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

/// The line both clients show beside Update for a harness older than the oldest version
/// CrazyAgent drives (DEC-166, Task 21). Inputs: the harness name and its versions row.
/// Output: the line, or None without a plain floor, without an installed version, or for an
/// installed version not known to be below the floor.
pub fn needs_newer_line(harness: &str, versions: &HarnessVersions) -> Option<String> {
    let minimum = versions
        .run_min
        .as_deref()
        .filter(|minimum| plain_version(minimum))?;
    let installed = versions.installed.as_deref()?;
    if !below(installed, minimum) {
        return None;
    }
    Some(format!(
        "{harness} {installed} is too old for CrazyAgent; it needs {minimum} or newer."
    ))
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// On Windows a harness installed under a drive path is named by how it was installed, as
    /// on Unix, and its update and its check are refused as missing their tool while the npm
    /// they would run is not there. A verbatim, UNC or forward-slash path names no install.
    #[test]
    fn a_windows_npm_install_is_named_and_its_missing_npm_is_refused() {
        let roots = Roots {
            home: PathBuf::from(r"C:\Users\unit"),
            brew_prefixes: Vec::new(),
            casks: false,
            npm: None,
            places: crate::tools::tools_new(Vec::new(), None),
            elevated: false,
        };
        let real =
            Path::new(r"C:\Users\unit\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js");
        assert_eq!(
            method("codex", real, &roots),
            Method::Npm {
                package: "@openai/codex".into()
            }
        );
        assert_eq!(
            update_command("codex", real, real, &roots),
            Err(TOOL_MISSING)
        );
        assert_eq!(check_command("codex", real, &roots), Err(TOOL_MISSING));
        for refused in [
            r"\\?\C:\Users\unit\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js",
            r"\\server\share\npm\node_modules\@openai\codex\bin\codex.js",
            r"C:/Users/unit/AppData/Roaming/npm/node_modules/@openai/codex/bin/codex.js",
        ] {
            assert_eq!(
                method("codex", Path::new(refused), &roots),
                Method::Unknown,
                "{refused}"
            );
        }
    }
    /// On Windows an npm install outside the engine prefix, such as the owner's own
    /// `%APPDATA%\npm`, is checked and updated in place by the engine's npm with its own prefix
    /// pinned, since npm puts no npm of its own in a prefix there; the engine prefix keeps the
    /// engine's search.
    #[test]
    fn a_windows_npm_install_outside_the_engine_prefix_runs_the_engine_npm_with_its_prefix() {
        let node = PathBuf::from(r"C:\Program Files\nodejs");
        let engine = NpmInstall {
            node: node.join("node.exe"),
            program: node.join("npm.cmd"),
            search: vec![node.clone(), PathBuf::from(r"C:\engine\setup\harnesses")],
            prefix: PathBuf::from(r"C:\engine\setup\harnesses"),
        };
        let roots = Roots {
            home: PathBuf::from(r"C:\Users\unit"),
            brew_prefixes: Vec::new(),
            casks: false,
            npm: Some(engine.clone()),
            places: crate::tools::tools_new(Vec::new(), None),
            elevated: false,
        };
        let owner = PathBuf::from(r"C:\Users\unit\AppData\Roaming\npm");
        let args = vec!["view", "@openai/codex", "version"];
        assert_eq!(
            npm_for(&owner, &roots, args.clone()),
            Fixed {
                program: engine.program.clone(),
                args: args.clone(),
                search: vec![node.clone(), owner.clone()],
                prefix: Some(owner.clone()),
            }
        );
        assert_eq!(
            npm_for(&engine.prefix, &roots, args.clone()),
            Fixed {
                program: engine.program.clone(),
                args,
                search: engine.search.clone(),
                prefix: Some(engine.prefix.clone()),
            }
        );
    }
    /// With the engine's home the profile folder on Windows, a harness that installed itself
    /// under it is named as self-updating, and its update is refused as unsafe once the users
    /// group may write the file, whatever the resolver would say about it.
    #[test]
    fn a_windows_self_updating_install_is_named_and_one_others_may_write_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let home = kernel_sandbox::paths::canonical(folder.path()).unwrap();
        let installed = home.join(".codex").join("packages").join("standalone");
        std::fs::create_dir_all(&installed).unwrap();
        let entry = installed.join("codex.exe");
        std::fs::write(&entry, "").unwrap();
        icacls(&entry, &["/grant", "*S-1-5-32-545:(W)"]);
        let roots = Roots {
            home,
            brew_prefixes: Vec::new(),
            casks: false,
            npm: None,
            places: crate::tools::tools_new(Vec::new(), None),
            elevated: false,
        };
        assert_eq!(method("codex", &entry, &roots), Method::SelfUpdate);
        assert_eq!(
            update_command("codex", &entry, &entry, &roots),
            Err(TOOL_UNSAFE)
        );
    }

    /// An elevated engine is refused once the install is known and before any tool is
    /// checked: an update, a check and an install all answer that the engine runs as an
    /// administrator, where the same roots unelevated answer as before. The engine's own
    /// roots carry the token's real elevation.
    #[test]
    fn an_elevated_engine_is_refused_before_every_other_answer() {
        let mut roots = Roots {
            home: PathBuf::from(r"C:\Users\unit"),
            brew_prefixes: Vec::new(),
            casks: false,
            npm: None,
            places: crate::tools::tools_new(Vec::new(), None),
            elevated: false,
        };
        let real =
            Path::new(r"C:\Users\unit\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js");
        assert_eq!(install_command("codex", &roots), Err(NEEDS_NODE));
        roots.elevated = true;
        assert_eq!(update_command("codex", real, real, &roots), Err(AS_ROOT));
        assert_eq!(check_command("codex", real, &roots), Err(AS_ROOT));
        assert_eq!(install_command("codex", &roots), Err(AS_ROOT));
        assert_eq!(
            Roots::system().elevated,
            crazyagent_win_sandbox::elevated().unwrap()
        );
    }

    /// Runs icacls on one path and asserts it succeeded, with its error output when not.
    /// Inputs: the path and the icacls arguments.
    /// Output: none.
    fn icacls(path: &Path, change: &[&str]) {
        let mut command = std::process::Command::new("icacls");
        command.arg(path).args(change);
        let done = kernel_processes::spawn_gate::collected(&mut command).unwrap();
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    /// A tool the engine would start passes the Windows trust check when only the system and
    /// this user may change it, as the curl Windows ships; a relative or missing tool is
    /// missing, and one the users group may write is unsafe.
    #[test]
    fn a_windows_tool_is_trusted_only_while_no_other_account_may_change_it() {
        let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("curl.exe");
        assert_eq!(trusted(&shipped), Ok(()));
        assert_eq!(trusted(Path::new("curl.exe")), Err(TOOL_MISSING));
        let folder = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
        let tool = folder.path().join("tool.exe");
        assert_eq!(trusted(&tool), Err(TOOL_MISSING));
        std::fs::write(&tool, "").unwrap();
        assert_eq!(trusted(&tool), Ok(()));
        icacls(&tool, &["/grant", "*S-1-5-32-545:(W)"]);
        assert_eq!(trusted(&tool), Err(TOOL_UNSAFE));
    }

    /// A global npm install needs no administrator only in a folder this user owns: a folder
    /// whose owner is set to this user is owned, one the administrators own is not, and
    /// neither is a file or a missing path.
    #[test]
    fn a_windows_folder_is_owned_only_when_this_user_owns_it() {
        let folder = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
        let modules = folder.path().join("node_modules");
        std::fs::create_dir(&modules).unwrap();
        let me = kernel_processes::spawn_gate::collected(&mut std::process::Command::new("whoami"))
            .unwrap();
        let me = String::from_utf8(me.stdout).unwrap();
        icacls(&modules, &["/setowner", me.trim()]);
        assert!(owned(&modules));
        icacls(&modules, &["/setowner", "*S-1-5-32-544"]);
        assert!(!owned(&modules));
        let file = folder.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(!owned(&file));
        assert!(!owned(&folder.path().join("gone")));
    }
}
