use super::ProfileHooks;
use crate::error::{SandboxError, invalid};
#[cfg(target_os = "macos")]
use crate::paths::clean_absolute;
use crate::paths::{SYSTEM_SECRET_PATHS, any_case, at_or_below, owner_secret_paths, regex_escape};
use crate::profile::consts::*;
use crate::profile::consts::{
    ADMIN_LINKS, CREDENTIAL_NAMES, ENGINE_SECRET_FILES, FIXED_WRITES, GIT_FALLBACK,
    MULTI_PACK_INDEX, PINNED_WRITES, RUNTIME_ENTRIES_MAX, SBPL_STRING_MAX, STORE_ALTERNATES,
};
#[cfg(target_os = "macos")]
use crate::profile::consts::{MACH_SERVICES, METADATA_LITERALS, SYSTEM_READ_ROOTS};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

/// Escapes a plain absolute path for a Seatbelt regex, so every character matches itself.
/// Input: a path that passes `clean_absolute`, so it holds no quote or backslash.
/// Output: the pattern text, without anchors.
pub fn seatbelt_pattern(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(text.starts_with('/'));
    regex_escape(&text)
}

/// The Seatbelt rules that keep an agent from starting a macOS app: no exec of a main
/// executable in any app bundle's `Contents/MacOS`, wherever it sits and in any letter
/// case, since a GUI app started inside the agent profile aborts at launch and macOS shows
/// a crash report for every attempt (Chrome crash popups, decision C1, OP-24). The
/// interpreter a framework Python's launcher starts, `<name>.framework/Versions/<v>/
/// Resources/Python.app/Contents/MacOS/Python`, stays allowed, except in the folders the
/// agent can write, where it could plant a bundle under that name; their Data volume path
/// is denied too. Command-line tools a bundle ships under `Contents/Resources` or
/// `Contents/Developer` stay allowed, as does each exact program the engine launches from
/// outside those folders. It must follow the exec allow, since the last matching rule
/// wins. Inputs: the agent's writable roots and those programs. Output: the rule text.
pub fn app_exec_denies<'a>(
    writable: impl Iterator<Item = &'a PathBuf>,
    launched: impl Iterator<Item = &'a PathBuf>,
) -> String {
    let data = format!(
        "/{}/{}/{}",
        any_case("System"),
        any_case("Volumes"),
        any_case("Data")
    );
    let app = format!(
        ".+{}/{}/{}/",
        any_case(".app"),
        any_case("Contents"),
        any_case("MacOS")
    );
    let mut rule = format!(
        "(deny process-exec\n  (regex #\"^/{app}\"))\n(allow process-exec\n  (regex #\"^/.+\\.framework/Versions/[^/]+/Resources/Python\\.app/Contents/MacOS/Python$\"))\n(deny process-exec"
    );
    for root in writable {
        let root = root.display().to_string();
        rule.push_str(&format!(
            "\n  (regex #\"^({data})?{}/{app}\")",
            any_case(&root)
        ));
    }
    rule.push_str(")\n");
    let launched: Vec<&PathBuf> = launched.collect();
    if !launched.is_empty() {
        rule.push_str("(allow process-exec");
        for program in launched {
            rule.push_str(&format!("\n  (literal \"{}\")", program.display()));
        }
        rule.push_str(")\n");
    }
    rule
}

/// The Seatbelt rules that keep an Own branch agent from repointing the object store its
/// clone borrows (AL-1, DEC102-F1): no write to its `objects/info/alternates` or
/// `http-alternates`, its git folder's `commondir` or the git folder entry itself, which
/// it may never replace, rename or relink, no move or removal of its `objects` or
/// `objects/info` folder, no symlink in its store, and nothing at `.git.git` beside its
/// git folder, the fallback repository git's upload-pack serves when the git folder fails
/// git's repository test (RL-1), no `objects/pack/multi-pack-index` file, folder or
/// companion, which can name a pack outside the store (RL-2), and no write to the
/// `objects/pack` or `objects/<xx>` folder entries, so no folder built elsewhere is moved
/// in over one with its contents (RL-3), each name in any letter case, while its own loose
/// objects and packs inside those folders stay writable. The engine makes those folders
/// before each launch (`landing::ready_agent_clone`). The engine's fetch checks the same
/// store before it runs (w-landing). Input: the clone. Output: the rule text.
pub fn clone_store_denies(clone: &Path) -> String {
    let fallback = format!("{}/{}", seatbelt_pattern(clone), any_case(GIT_FALLBACK));
    let git = format!("{}/{}", seatbelt_pattern(clone), any_case(".git"));
    let objects = format!("{git}/{}", any_case("objects"));
    let info = format!("{objects}/{}", any_case("info"));
    let pack = format!("{objects}/{}", any_case("pack"));
    let index = format!("{pack}/{}", any_case(MULTI_PACK_INDEX));
    let [alternates, http_alternates] = STORE_ALTERNATES.map(any_case);
    let [commondir, _] = ADMIN_LINKS.map(any_case);
    let below = at_or_below(&fallback);
    let text = format!(
        "(deny file-write*\n  (regex #\"^{git}$\")\n  {below}\n  (regex #\"^{pack}$\")\n  (regex #\"^{objects}/[0-9a-fA-F][0-9a-fA-F]$\")\n  (regex #\"^{index}\")\n  (regex #\"^{info}/({alternates}|{http_alternates})$\")\n  (regex #\"^{git}/{commondir}$\"))\n(deny file-write-unlink\n  (regex #\"^{objects}$\")\n  (regex #\"^{info}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{objects}/\") (vnode-type SYMLINK)))\n"
    );
    assert_eq!(text.matches("(regex #\"^/").count(), 10);
    assert!(text.contains("(vnode-type SYMLINK)"));
    text
}

/// The writes a clone lead's teammates may not make in the lead's clone (OP-9): anything but
/// the teammates' folder, the clone's object store, the inside of each worktree record git
/// keeps in `.git/worktrees`, and the refs and reflogs of the lead's own `<branch>--<name>`
/// teammate branches, none when the lead's branch is unknown, never the lead's branch
/// (AGENT-REF-ALLOW-NARROW); nor the links that tie a teammate worktree to the clone, which
/// the engine writes with the lead's rights, each name in any letter case; nor the
/// teammates' folder entry, nor make, move or remove anything directly in it, such as a
/// teammate's folder, where the engine's lead-rights `git worktree add` writes (OP9B-C1).
/// It must follow every write allow, since the last match wins.
/// Inputs: the profile hooks, the clone and the lead's branch. Output: the rule text.
pub fn teammates_clone_denies(hooks: &ProfileHooks, clone: &Path, lead: Option<&str>) -> String {
    let path = clone.display();
    let root = seatbelt_pattern(clone);
    let folder = hooks.teammates_folder;
    let teammates = any_case(folder);
    let git = any_case(".git");
    let branches = lead
        .map(|lead| {
            format!(
                "\n    (require-not (regex #\"^{root}/\\.git/(logs/)?refs/heads/{}--[a-z0-9-]+(\\.lock)?$\"))",
                regex_escape(lead)
            )
        })
        .unwrap_or_default();
    let text = format!(
        "(deny file-write*\n  (require-all\n    (subpath \"{path}\")\n    (require-not (subpath \"{path}/{folder}\"))\n    (require-not (subpath \"{path}/.git/objects\"))\n    (require-not (regex #\"^{root}/\\.git/worktrees/[^/]+/.+$\")){branches}))\n(deny file-write*\n  (regex #\"^{root}/{teammates}$\")\n  (regex #\"^{root}/{teammates}/[^/]+/{git}$\")\n  (regex #\"^{root}/{git}/{}/[^/]+/({}|{})$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
        any_case("worktrees"),
        any_case("commondir"),
        any_case("gitdir")
    );
    assert_eq!(
        text.matches("(regex #\"^/").count(),
        5 + usize::from(lead.is_some())
    );
    assert_eq!(text.matches("(subpath \"/").count(), 3);
    text
}

/// Seatbelt regex bodies `{prefix}(a|b|...){suffix}` that together match each of
/// `alternatives`, packed whole, in order, so each body fits [`SBPL_STRING_MAX`] bytes; one
/// alternative too long beside the prefix keeps a body of its own, which [`sbpl_overlong`]
/// then names. Inputs: the prefix, the regex fragments and the suffix. Output: the bodies.
pub fn regex_bodies(prefix: &str, alternatives: &[String], suffix: &str) -> Vec<String> {
    let body = |group: &[&str]| format!("{prefix}({}){suffix}", group.join("|"));
    let mut bodies = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    for alternative in alternatives {
        let mut wider = group.clone();
        wider.push(alternative);
        if !group.is_empty() && body(&wider).len() > SBPL_STRING_MAX {
            bodies.push(body(&group));
            group.clear();
        }
        group.push(alternative);
    }
    if !group.is_empty() {
        bodies.push(body(&group));
    }
    assert!(bodies.len() <= alternatives.len());
    bodies
}

/// The quoted strings and regexes of a Seatbelt profile longer than its reader takes
/// ([`SBPL_STRING_MAX`] bytes), every one, so a profile that would not parse is refused
/// before it is written. Input: the profile text, whose strings hold no quote. Output: the
/// strings too long, none for a profile that parses; an odd quote count names the rest.
#[cfg(unix)]
pub fn sbpl_overlong(profile: &str) -> Vec<&str> {
    let parts: Vec<&str> = profile.split('"').collect();
    let mut overlong: Vec<&str> = parts
        .iter()
        .skip(1)
        .step_by(2)
        .copied()
        .filter(|part| part.len() > SBPL_STRING_MAX)
        .collect();
    if parts.len().is_multiple_of(2) {
        overlong.push(parts[parts.len() - 1]);
    }
    overlong
}

/// Spells a Seatbelt regex for one path component other than `name` in any letter case,
/// with whatever follows it: a component that differs from `name` at some character, stops
/// short of it or runs past it. Input: a name of letters, digits and dots. Output: the
/// pattern text, a group without anchors.
pub fn other_than(name: &str) -> String {
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '.')
    );
    let characters: Vec<char> = name.chars().collect();
    let mut alternatives = Vec::with_capacity(2 * characters.len());
    for at in 0..characters.len() {
        let prefix = any_case(&characters[..at].iter().collect::<String>());
        let character = characters[at];
        alternatives.push(format!(
            "{prefix}[^{}{}/]",
            character.to_ascii_lowercase(),
            character.to_ascii_uppercase()
        ));
        if at > 0 {
            alternatives.push(format!("{prefix}(/|$)"));
        }
    }
    alternatives.push(format!("{}[^/]", any_case(name)));
    assert_eq!(alternatives.len(), 2 * characters.len());
    format!("({})", alternatives.join("|"))
}

/// The Seatbelt rule that pins one writable root itself (CTX-M1): no agent may move, replace,
/// remove or re-permission a folder it writes in. A Seatbelt rename checks only the two
/// paths it names, so a root moved under another root and back would carry every path a
/// rule protects below it, such as the launch files in an agent's home (LL-2), out of that
/// rule and back. It must follow every write allow, since the last match wins. Input: the
/// root. Output: the rule text.
pub fn root_pin(root: &Path) -> String {
    format!("(deny {FIXED_WRITES} (literal \"{}\"))\n", root.display())
}

/// The Seatbelt filters for a `.git` at any depth below `root`, top level included, in every
/// spelling a case-insensitive volume finds ([`any_case`]): the part of [`ProjectSandbox::nested_git_denies`] for a root that is not a
/// working tree (SM1-R), as [`at_or_below`] spells it. Input: the root. Output: the filters.
pub fn any_depth_git(root: &Path) -> String {
    at_or_below(&format!(
        "{}/(.+/)?{}",
        seatbelt_pattern(root),
        any_case(".git")
    ))
}

/// The Seatbelt rule that refuses reading, writing, linking or cloning any path with a part
/// made of ASCII, [`crate::paths::ASCII_FOLDS`] points and at least one code
/// point HFS+ ignores in names, since such a volume opens `.g\u{200c}it/config` as
/// `.git/config` past every name deny (SA vid 202). A part in another script keeps its
/// joiners. Output: the rule, ending in a line break.
pub fn hfs_ignored_deny() -> String {
    let ignored: Vec<String> = crate::paths::HFS_IGNORED
        .iter()
        .map(char::to_string)
        .collect();
    let folds: Vec<String> = crate::paths::ASCII_FOLDS
        .iter()
        .map(|(point, _)| point.to_string())
        .collect();
    let ignored = ignored.join("|");
    let part = format!("([ -.0-~]|{}|{ignored})", folds.join("|"));
    let rule = format!(
        "(deny file-read-data file-write* file-link file-clone\n  (regex #\"^(.*/)?{part}*({ignored}){part}*(/.*)?$\"))\n"
    );
    assert!(rule.len() <= SBPL_STRING_MAX && rule.matches('"').count() == 2);
    rule
}

/// The Seatbelt profile the engine primes a uv tool cache under (T8 C3): the system's
/// read-only roots and the uv binary to read and run, the fresh priming folder as the only
/// place to write, `/dev/null` and the random devices, no network and no user folder.
/// Inputs: the canonical priming folder and the canonical uv binary. Output: the text.
#[cfg(target_os = "macos")]
pub fn priming_profile(folder: &Path, uv: &Path) -> String {
    assert!(clean_absolute(folder) && clean_absolute(uv));
    let mut text = String::from(
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow sysctl-read)\n(allow mach-lookup",
    );
    for service in MACH_SERVICES {
        text.push_str(&format!("\n  (global-name \"{service}\")"));
    }
    text.push_str(")\n(allow file-read* file-map-executable");
    for root in SYSTEM_READ_ROOTS {
        text.push_str(&format!("\n  (subpath \"{root}\")"));
    }
    text.push_str(&format!(
        "\n  (literal \"{}\"))\n(allow file-read*",
        uv.display()
    ));
    for literal in METADATA_LITERALS {
        text.push_str(&format!("\n  (literal \"{literal}\")"));
    }
    text.push_str(&format!(
        ")\n(allow file-read-metadata\n  (path-ancestors \"{0}\")\n  (path-ancestors \"{1}\"))\n(allow file-read* file-write*\n  (subpath \"{0}\"))\n",
        folder.display(),
        uv.display()
    ));
    text.push_str(
        "(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\"))\n(allow file-read*\n  (literal \"/dev/random\")\n  (literal \"/dev/urandom\"))\n",
    );
    assert!(text.starts_with("(version 1)\n(deny default)\n"));
    assert!(!text.contains("(allow network") && text.matches("file-write*").count() == 1);
    text
}

/// Names the credential files and folders the sandbox guards as they appear in a path: the
/// owner's under the home, the system's without their leading `/` and the credential names
/// no allowed root may hold. Takes the profile hooks. Returns the names.
pub fn credential_places(hooks: &ProfileHooks) -> impl Iterator<Item = &'static str> {
    let owner: Vec<&'static str> = owner_secret_paths(&(hooks.access.harness_folders)()).collect();
    owner
        .into_iter()
        .chain(CREDENTIAL_NAMES.iter().copied())
        .chain(
            SYSTEM_SECRET_PATHS
                .iter()
                .map(|path| path.trim_start_matches('/')),
        )
}

/// The engine runtime's secrets: the fixed secret files and folders, plus every Unix socket
/// directly in the runtime and every runtime folder that directly holds one (owner
/// generations, runner roots, the terminal server). Inputs: the profile hooks and the canonical runtime folder.
/// Output: the secret paths, or a refusal for a runtime or folder over the entry bound.
pub fn engine_secrets_in(
    hooks: &ProfileHooks,
    runtime: &Path,
) -> Result<Vec<PathBuf>, SandboxError> {
    let mut secrets: Vec<PathBuf> = ENGINE_SECRET_FILES
        .iter()
        .chain(hooks.key_folders)
        .chain(hooks.engine_secret_folders)
        .map(|name| runtime.join(name))
        .collect();
    for entry in bounded_entries(runtime)? {
        if !secrets.contains(&entry) && engine_socket_place(&entry)? {
            secrets.push(entry);
        }
    }
    Ok(secrets)
}

#[cfg(unix)]
fn engine_socket_place(path: &Path) -> Result<bool, SandboxError> {
    use std::os::unix::fs::FileTypeExt;
    let Some(kind) = present_kind(path)? else {
        return Ok(false);
    };
    if kind.is_socket() {
        return Ok(true);
    }
    if !kind.is_dir() {
        return Ok(false);
    }
    let entries = match bounded_entries(path) {
        Err(SandboxError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        other => other?,
    };
    for entry in entries {
        if present_kind(&entry)?.is_some_and(|kind| kind.is_socket()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn present_kind(path: &Path) -> Result<Option<fs::FileType>, SandboxError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata.file_type())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn engine_socket_place(_path: &Path) -> Result<bool, SandboxError> {
    Ok(false)
}

fn bounded_entries(folder: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(folder)? {
        if entries.len() == RUNTIME_ENTRIES_MAX {
            return Err(invalid("engine runtime folder has too many entries"));
        }
        entries.push(entry?.path());
    }
    Ok(entries)
}

/// The engine secret folder a master's home may be carved back out of, the home alone: an
/// rcli master's configured SDK home directly in its folder, or a listed master session's
/// home in the native profiles folder ([`listed_master_folder`]). Inputs: the profile hooks, the engine root and
/// the canonical home. Output: the folder, or none for any other home.
pub fn carved_folder(hooks: &ProfileHooks, engine: &Path, home: &Path) -> Option<PathBuf> {
    let homes = engine.join(hooks.configured_sdk_homes_folder);
    if home.parent() == Some(homes.as_path()) {
        return Some(homes);
    }
    listed_master_folder(hooks, engine, home)
}

/// The native profiles folder when the home is a listed master session's home,
/// `<listed key of account>/home/<session>/<harness>/<account>` in it, so in that account's
/// own listed slot, whose home holds no sign-in's record (DEC-180 Task 20). Inputs: the
/// profile hooks, the engine root and the canonical home. Output: the folder, or none for any other home.
#[cfg(unix)]
fn listed_master_folder(hooks: &ProfileHooks, engine: &Path, home: &Path) -> Option<PathBuf> {
    let profiles = engine.join(hooks.native_profiles_folder);
    let parts: Vec<Component> = home.strip_prefix(&profiles).ok()?.components().collect();
    let [
        Component::Normal(key),
        Component::Normal(slot),
        Component::Normal(_),
        Component::Normal(_),
        Component::Normal(account),
    ] = parts.as_slice()
    else {
        return None;
    };
    let listed = (hooks.listed_slot_key)(account.to_str()?);
    let slot_home = profiles.join(key).join(slot);
    (*key == listed.as_str() && *slot == "home" && (hooks.holds_no_sign_in)(&slot_home))
        .then_some(profiles)
}

/// Carves no listed master home off Unix, where a listed master session's home is never made
/// (its listed_home refuses with `launch_account_home_unavailable`). Takes the profile hooks,
/// the engine root and the home, all unused. Returns none.
#[cfg(not(unix))]
fn listed_master_folder(_hooks: &ProfileHooks, _engine: &Path, _home: &Path) -> Option<PathBuf> {
    None
}

/// The Seatbelt rules that keep an agent from changing what its harness loads from its own
/// home as instructions, skills, hooks or settings (LL-2): no write in the launch folder or
/// the relay plugin the engine writes before each launch, nor in the subagents, commands,
/// output styles and rules folders Claude Code reads or the `.agents` folder whose skills
/// Codex reads from its home, none to the global instruction files of Claude Code and Codex,
/// the Codex override that would replace them, the Codex `config.toml` holding its developer
/// instructions or Claude Code's `settings.json`, nor to a zsh, bash or sh startup file,
/// which every shell its harness starts there runs first (SA vid 12), none under `skills`
/// but Codex's own `.system` skills, which Codex installs at its start and the engine
/// removes before each Codex launch, and no move or removal of the `skills` folder, so no
/// folder built elsewhere
/// is moved in, nor a flag on `skills` or anywhere in `.system` that would keep the engine
/// from removing it (CTX-M1 r1), each name in any letter case. Its own run cannot change the
/// text it runs with until the next launch rewrites it. They also keep the agent's own
/// keychain and keychain settings, which the engine sets up with `security` calls before a
/// launch, out of its reach (DEC-146): no write under `Library/Keychains` nor to
/// `Library/Preferences/com.apple.security.plist`, and no move, removal, mode change or link
/// in place of `Library`, `Library/Preferences` or any other folder a launch keeps only part
/// of, nor of the cargo cache links the launch keeps, nor a link it makes at their names
/// ([`crate::agent_home::pinned_entries`], DEC-173), as [`root_pin`] keeps the home itself,
/// so no link an agent plants leads one of those calls, a launch's removal of what it does
/// not keep or a later cargo outside the home. They deny writes to the launch lock too, so
/// no run links, removes or replaces it, since a launch refuses a lock with a second link
/// (r3). Every home gets them; an agent home gets [`agent_config_denies`] as well. They must
/// follow every write allow. Inputs: the profile hooks and the home. Output: the rule text.
pub fn launch_denies(hooks: &ProfileHooks, home: &Path) -> String {
    let home = seatbelt_pattern(home);
    let skills = format!("{home}/{}", any_case("skills"));
    let folders = [
        hooks.launch_state,
        hooks.relay_plugin_folder,
        "agents",
        "commands",
        "output-styles",
        "rules",
        ".agents",
        "Library/Keychains",
    ];
    let files = [
        hooks.claude_instruction_file,
        hooks.codex_instruction_file,
        "AGENTS.override.md",
        "config.toml",
        "settings.json",
        "Library/Preferences/com.apple.security.plist",
        hooks.launch_lock,
    ];
    let mut text = String::from("(deny file-write*\n");
    for folder in folders {
        text.push_str(&format!(
            "  {}\n",
            at_or_below(&format!("{home}/{}", any_case(folder)))
        ));
    }
    for file in files {
        text.push_str(&format!("  (regex #\"^{home}/{}$\")\n", any_case(file)));
    }
    text.push_str(&format!(
        "  (regex #\"^{home}/{}$\")\n",
        shell_startup(hooks)
    ));
    let system = any_case(hooks.codex_system_skills);
    text.push_str(&format!(
        "  (regex #\"^{skills}/{}\"))\n(deny {PINNED_WRITES} (regex #\"^{skills}$\"))\n(deny file-write-flags\n  (regex #\"^{skills}/{system}$\")\n  (regex #\"^{skills}/{system}/\"))\n",
        other_than(hooks.codex_system_skills)
    ));
    let library = format!("{home}/{}", any_case("Library"));
    let walked: Vec<String> = (hooks.pinned_entries)()
        .iter()
        .map(|entry| format!("{home}/{}", any_case(entry)))
        .collect();
    assert!(walked.contains(&library));
    assert!(walked.contains(&format!("{home}/{}", any_case(".cargo/registry"))));
    assert!(walked.contains(&format!("{home}/{}", any_case(".config"))));
    let spelled = |indent: &str| {
        walked
            .iter()
            .map(|folder| format!("\n{indent}(regex #\"^{folder}$\")"))
            .collect::<String>()
    };
    text.push_str(&format!(
        "(deny {PINNED_WRITES}{})\n(deny file-write-create\n  (require-all\n    (require-any{})\n    (vnode-type SYMLINK)))\n",
        spelled("  "),
        spelled("      "),
    ));
    assert_eq!(text.matches("(regex #\"^/").count(), 20 + 2 * walked.len());
    text
}

/// The names [`agent_config_denies`] keeps a confined agent from writing in its own home,
/// each with all it holds: cargo's config and subcommands, git's global config in both
/// places it reads one, and npm's and yarn's user config.
const AGENT_CONFIG: [&str; 8] = [
    ".cargo/config",
    ".cargo/config.toml",
    ".cargo/bin",
    ".config/git",
    ".gitconfig",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
];

/// The folders of an agent home an [`AGENT_CONFIG`] name lies in. Each is made by
/// [`ProjectSandbox::install`] in a home the engine made and kept by every launch, and an
/// agent may neither make, move nor remove one, so no folder built elsewhere is moved in
/// with config past the names (RV-DS241-186).
const CONFIG_PARENTS: [&str; 2] = [".cargo", ".config"];

/// Makes each [`CONFIG_PARENTS`] folder of an agent home a real 0700 folder where nothing
/// holds its name, since the agent's profile keeps it from making one (RV-DS241-186). With
/// `clear`, for a home strictly below the engine's data root ([`ProjectSandbox::anchor_home`]),
/// what holds a name as anything else, as a link an earlier run planted, goes first, and a
/// name that is still no folder refuses; without it nothing is ever removed and such a name
/// stays for the launch's reset to judge (rv-code2 b8 F1). Each step goes through the home's
/// open handle and follows no link. Inputs: the profile hooks, the home and `clear`. Output: unit, or `Invalid`
/// when one cannot be made.
#[cfg(unix)]
pub fn make_config_parents(
    hooks: &ProfileHooks,
    home: &Path,
    clear: bool,
) -> Result<(), SandboxError> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::os::fd::AsFd;
    let refused = || invalid("a config folder of the agent home could not be made");
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = rustix::fs::open(home, folder, Mode::empty()).map_err(|_| refused())?;
    let is_folder = |name: &str| {
        rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
    };
    let remove = |name: &str| {
        (hooks.toolchain.remove_home_entry)(parent.as_fd(), name, "config_parent")
            .map_err(|_| refused())
    };
    for name in CONFIG_PARENTS {
        match is_folder(name) {
            Ok(true) => continue,
            Ok(false) if !clear => continue,
            Ok(false) => remove(name)?,
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return Err(refused()),
        }
        match rustix::fs::mkdirat(&parent, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(refused()),
        }
        if clear && is_folder(name) != Ok(true) {
            return Err(refused());
        }
    }
    Ok(())
}

/// The Seatbelt rules an agent home gets on top of [`launch_denies`], which a home the owner
/// keeps for their own tools does without, as the IDE terminal's does
/// ([`ProjectSandbox::keep_owner_config`], RV-DS241-188). No write to an [`AGENT_CONFIG`]
/// name in any letter case, so a run, or a process of it still alive after the next
/// launch's reset, plants no config or program its own tools or a later run with more
/// access follow (RV-DS241-137, -138, -186). No [`CONFIG_PARENTS`] folder made, so none
/// built elsewhere takes the name. No flag or ACL on an entry the next launch keeps and the
/// engine uses, nor on anything in one ([`crate::agent_home::kept_entries`]), so none holds
/// against the engine. Copies the agent's tools make elsewhere in its home and in its caches
/// still take flags (RV-DS241-187), and the next reset clears those as it removes them.
/// They must follow every write allow. Inputs: the profile hooks and the home. Output: the rule text.
pub fn agent_config_denies(hooks: &ProfileHooks, home: &Path) -> String {
    let pinned = (hooks.pinned_entries)();
    assert!(
        CONFIG_PARENTS
            .iter()
            .all(|parent| pinned.iter().any(|entry| entry == *parent))
    );
    for (parent, _) in AGENT_CONFIG.iter().filter_map(|name| name.split_once('/')) {
        assert!(CONFIG_PARENTS.contains(&parent));
    }
    let home = seatbelt_pattern(home);
    let spelled = |names: &[&str], below: bool| {
        names
            .iter()
            .map(|name| {
                let pattern = format!("{home}/{}", any_case(name));
                if below {
                    format!("\n  {}", at_or_below(&pattern))
                } else {
                    format!("\n  (regex #\"^{pattern}$\")")
                }
            })
            .collect::<String>()
    };
    let kept = (hooks.kept_entries)();
    let kept: Vec<&str> = kept.iter().map(String::as_str).collect();
    let text = format!(
        "(deny file-write*{})\n(deny file-write-create{})\n(deny file-write-flags file-write-acl{})\n",
        spelled(&AGENT_CONFIG, true),
        spelled(&CONFIG_PARENTS, false),
        spelled(&kept, true)
    );
    assert_eq!(
        text.matches("(regex #\"^/").count(),
        AGENT_CONFIG.len() + CONFIG_PARENTS.len() + kept.len()
    );
    assert!(!text.contains("(/.*)"));
    text
}

/// Spells the Seatbelt pattern of the shell startup files directly in a home, each in any
/// letter case: zsh's, alone or in the compiled form zsh runs in its place, and bash's and
/// sh's (SA vid 12). Input: the profile hooks. Output: the pattern, a group without anchors.
fn shell_startup(hooks: &ProfileHooks) -> String {
    let spelled = |names: &[&str]| {
        names
            .iter()
            .map(|name| any_case(name))
            .collect::<Vec<_>>()
            .join("|")
    };
    let pattern = format!(
        "(({})({})?|{})",
        spelled(hooks.zsh_startup),
        any_case(hooks.zsh_compiled),
        spelled(hooks.sh_startup),
    );
    assert!(pattern.len() <= SBPL_STRING_MAX / 2);
    pattern
}

/// The base write denies a profile gives again after it grants writes once more to the agent
/// home and other folders, as the IDE profile does for its server's data folders: each one
/// pinned ([`root_pin`]), no `.git` at any depth in any of them ([`any_depth_git`]), the
/// home's launch files ([`launch_denies`]) and no flag or ACL on anything in the home, which
/// the engine walks and rewrites before each start, so none keeps the owner's IDE from
/// starting (rv-sa241 b8 L1); never [`agent_config_denies`], since that home is one the
/// owner keeps ([`ProjectSandbox::keep_owner_config`], RV-DS241-188). They must follow that
/// grant, since the last match wins. Inputs: the profile hooks, the home and the other folders. Output: the
/// rule text.
#[cfg(unix)]
pub fn regranted_denies(hooks: &ProfileHooks, home: &Path, folders: &[&Path]) -> String {
    let mut text = launch_denies(hooks, home);
    text.push_str(&format!(
        "(deny file-write-flags file-write-acl {})\n",
        at_or_below(&seatbelt_pattern(home))
    ));
    let mut nested = String::new();
    for root in std::iter::once(home).chain(folders.iter().copied()) {
        text.push_str(&root_pin(root));
        nested.push_str(&format!("\n  {}", any_depth_git(root)));
    }
    text.push_str(&format!("(deny file-write*{nested})\n"));
    text
}

/// Reads one git link file the engine's own `git worktree add` wrote: a plain file of at
/// most 2 KiB holding `prefix`, one absolute or relative path and a newline. Input: the
/// file and its prefix. Output: the path text, or `Invalid` for any other file.
pub fn git_link(path: &Path, prefix: &str) -> Result<String, SandboxError> {
    use std::io::Read;
    let refused = || invalid("the worktree's git link is not a plain git link file");
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > GIT_LINK_BYTES_MAX {
        return Err(refused());
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(GIT_LINK_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|_| refused())?;
    let target = text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix('\n'))
        .filter(|target| !target.is_empty() && !target.contains('\n'))
        .ok_or_else(refused)?;
    Ok(target.to_owned())
}

pub fn borrowed<'a>(env: &'a [(&'static str, OsString)]) -> Vec<(&'static str, &'a OsStr)> {
    env.iter()
        .map(|(name, value)| (*name, value.as_os_str()))
        .collect()
}

/// Makes sure a store a profile keeps read-only is a plain folder the engine made: creates
/// it empty, mode 0700 on unix, or keeps an entry already there only when it is a real
/// folder, never a file or a link. Disk state is input here, so a wrong entry is an error.
/// Input: the store path. Output: Ok, or `InvalidData` for any other entry at the path.
pub fn ensure_store(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let created =
        std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(path);
    #[cfg(not(unix))]
    let created = fs::DirBuilder::new().create(path);
    match created {
        Err(problem) if problem.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.is_dir() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a submodule store must be a plain folder",
                ))
            }
        }
        made => made,
    }
}

/// Whether every deny rule comes after the profile's last write allow, since Seatbelt applies
/// the last matching rule and a deny a later allow overrides no longer holds (CTX-M1 F1).
/// Inputs: the profile text and the deny rules as written in it. Output: false when any
/// deny is missing or comes before a write allow.
pub fn denies_follow_write_allows(text: &str, denies: &[String]) -> bool {
    let last_write_allow = text
        .rfind("(allow file-write*")
        .max(text.rfind("(allow file-read* file-write*"));
    denies
        .iter()
        .all(|deny| text.rfind(deny.as_str()) > last_write_allow)
}

/// The Seatbelt rules that keep a shared team folder free of links and moved-in folders: no
/// link may be made there and no folder but a lock, so no teammate places a path the lead's
/// own Claude Code writes through (TPH-2 C). Input: the folder. Output: the rules.
pub fn shared_denies(folder: &Path) -> String {
    let folder = folder.display();
    format!(
        "(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type SYMLINK)))\n(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type DIRECTORY) (require-not (regex #\"{LOCK_FOLDER}\"))))\n"
    )
}

/// The npm package folder a Node script harness sits in: `<prefix>/lib/node_modules/<name>`,
/// or `<prefix>/lib/node_modules/@scope/<name>` for a scoped package, which holds the
/// harness's own dependencies. Inputs: the prefix and the program under it. Output: the
/// folder, or none when the program is not inside one.
pub fn node_package(prefix: &Path, program: &Path) -> Option<PathBuf> {
    let modules = prefix.join("lib").join("node_modules");
    let mut parts = program.strip_prefix(&modules).ok()?.components();
    let first = parts.next()?.as_os_str();
    let package = if first.to_str()?.starts_with('@') {
        modules.join(first).join(parts.next()?.as_os_str())
    } else {
        modules.join(first)
    };
    (program != package.as_path() && program.starts_with(&package)).then_some(package)
}

#[cfg(all(test, unix))]
mod tests;
