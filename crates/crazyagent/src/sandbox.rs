use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub mod elf;
pub mod extra_paths;
#[cfg(all(test, target_os = "macos"))]
mod git_deny_tests;
pub mod grants;
pub mod hidden_paths;
pub mod ide_shell;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod model_proxy;
pub mod policy;
pub mod shared_folders;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_agent;
#[cfg(target_os = "macos")]
pub mod tool_cache;
pub mod toolchains;
pub mod warden;
#[cfg(target_os = "windows")]
pub mod windows;

/// The variables that point an agent's tools at its tool cache (T8): uv's cache, and the
/// pnpm store, the only store variable pnpm 12 reads (probe P5). [`agent_env`] keeps a
/// harness from replacing either once the sandbox set it.
const TOOL_CACHE_ENV: [&str; 2] = ["UV_CACHE_DIR", "pnpm_config_store_dir"];

/// The macOS Seatbelt launcher every project agent process runs under.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Whether `program` is the boundary program [`agent_command`] starts a confined agent from
/// on this system: [`SANDBOX_EXEC`] on Unix, the boundary helper beside the engine's
/// executable on Windows. Input: a command's program. Output: true for that program.
pub fn starts_at_boundary(program: &OsStr) -> bool {
    #[cfg(unix)]
    {
        program == SANDBOX_EXEC
    }
    #[cfg(target_os = "windows")]
    {
        windows::helper_beside_engine().is_ok_and(|helper| program == helper.as_os_str())
    }
}

/// The fixed ports the owner's tools listen on, which no tools-on agent may bind or listen
/// on at any address: the Codex sign-in callback and Ollama's local model server. Seatbelt
/// matches a local address by port only (`localhost` there matches every local address), so
/// it cannot keep an agent's listener on loopback; it can keep these ports free.
pub const OWNER_PORTS: [u16; 2] = [1455, 11434];

/// The most ports of the owner's configured local model endpoints one tools-on sandbox keeps
/// free beside [`OWNER_PORTS`], which bounds the profile's deny rules.
pub const KEPT_PORTS_MAX: usize = 64;

/// The loopback ports the owner's configured model endpoints listen on, which a tools-on
/// agent must not bind before the owner's model server does. An endpoint URL that does not
/// parse names no port. Input: the model access settings. Output: the ports, sorted, once
/// each.
pub fn owner_ports(access: &crate::config::ModelAccess) -> Vec<u16> {
    let mut ports: Vec<u16> = access
        .connections
        .iter()
        .filter_map(|connection| match &connection.endpoint {
            crate::config::ModelEndpoint::Http { url } => url::Url::parse(url).ok(),
            crate::config::ModelEndpoint::Native {} => None,
        })
        .filter(|url| match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        })
        .filter_map(|url| url.port_or_known_default())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    assert!(ports.len() <= access.connections.len());
    ports
}

/// Why a tools-on launch does not start: the engine's settings files exist but do not read,
/// so the owner's configured local model ports are known nowhere (OWNER-PORTS-FAIL-OPEN).
pub const OWNER_PORTS_UNREADABLE: &str = "owner_ports_settings_unreadable";

/// The owner's configured local model ports, read from the engine's two settings files at
/// each tools-on launch, as [`owner_ports`] lists them; none when no file exists. A file that
/// does not read fails closed and logs only the code, since its error text can carry the
/// file's lines. Inputs: the global and project settings files. Output: the ports, or
/// [`OWNER_PORTS_UNREADABLE`].
pub fn settings_owner_ports(global: &Path, project: &Path) -> Result<Vec<u16>, &'static str> {
    let Ok(config) = crate::config::resolve_config(global, project) else {
        eprintln!("{OWNER_PORTS_UNREADABLE}");
        return Err(OWNER_PORTS_UNREADABLE);
    };
    Ok(config
        .settings
        .model_access
        .map_or_else(Vec::new, |access| owner_ports(&access)))
}

/// Why a sandbox with hidden folders does not start on Linux or Windows: their boundaries
/// grant whole roots and cannot keep a folder inside one out of reach.
pub const HIDDEN_NEEDS_SEATBELT: &str = "hidden folders need the macOS sandbox";

/// Why a native-mode lead's own tmux server does not start off macOS: only Seatbelt holds
/// its one-socket grant, and DEC-094 keeps team agents macOS-only in V1.
pub const TMUX_SERVER_NEEDS_SEATBELT: &str = "native teammates need the macOS sandbox";

/// Why a sandbox that protects agent folders does not start on Linux or Windows: only
/// Seatbelt holds the per-path rule, and no IDE sandbox runs there in V1 (DEC-124).
pub const AGENT_FILES_NEED_SEATBELT: &str = "agent folder protection needs the macOS sandbox";

/// Why a sandbox that opens shared project folders for writing does not start on Linux or
/// Windows: only Seatbelt keeps secret files closed inside those folders (DEC-147.9).
pub const SHARED_WRITES_NEED_SEATBELT: &str = "writable shared folders need the macOS sandbox";

/// Why a Full access boundary cannot run off macOS: only macOS qualifies Full access in V1.
pub const FULL_ACCESS_NEEDS_MACOS: &str = "Full access agents run only on macOS in this version";

/// The Linux helper's exit code when its call supervisor could not take back an entry a
/// case-folding folder read as a held name, and so stopped the run (DEC-185 S2a, rv-sec3
/// F1). The supervisor crate keeps its own copy, tied to this one in the Linux sandbox; 125
/// is its other stops, and 126 and up are the shell's and the signals'.
pub const HELD_NOT_PUT_BACK_EXIT: i32 = 122;

/// What the engine shows for a Linux run that ended with [`HELD_NOT_PUT_BACK_EXIT`].
pub const HELD_NOT_PUT_BACK: &str =
    "The sandbox stopped this run: a protected name could not be put back";

/// Tells why a harness ended when its sandbox stopped it. Only the Linux call supervisor
/// ends a run with [`HELD_NOT_PUT_BACK_EXIT`], so the code means that on Linux alone.
/// Inputs: the system's name, as `std::env::consts::OS` gives it, and the exit code.
/// Output: the sentence to show, or none when the code is the harness's own.
pub fn held_stop(os: &str, code: i32) -> Option<&'static str> {
    (os == "linux" && code == HELD_NOT_PUT_BACK_EXIT).then_some(HELD_NOT_PUT_BACK)
}

/// The cause every master host gives a turn whose harness the Linux call supervisor stopped
/// because it could not take back an entry read as a held name ([`held_stop`]); the engine
/// names it `MasterReason::SandboxStopped`.
pub const HELD_STOP_CAUSE: &str = "sandbox_held_not_put_back";

const PATH_BYTES_MAX: usize = 1024;
const READ_ROOTS_MAX: usize = 16;
/// The most folder links one sandbox makes readable by name beside its roots: the links
/// its process is started through.
const LINKS_MAX: usize = 4;
const WORKTREES_MAX: usize = 8;
/// The most folders one sandbox hides: every hidden folder a project saves
/// ([`hidden_paths::HIDDEN_PATHS_MAX`]) plus the two a step adds itself at most, such as the
/// graph step's `.crazyagent/`, or a native lead's part of the relay mailbox and its own temp
/// folder, which its teammates' sandbox hides.
pub const HIDDEN_MAX: usize = hidden_paths::HIDDEN_PATHS_MAX + 2;
/// The most files and folders one sandbox keeps out by name: the graph step's secret-like and
/// git-ignored project paths (MAP-2). A project with more builds no graph.
pub const KEPT_OUT_MAX: usize = 1024;
/// The most top-level names a Chosen folders or None launch keeps open in the project
/// folder: every present entry, the engine's names and the picked ones.
pub const OPEN_TOPS_MAX: usize = 512;
/// The opening of the Seatbelt rule for paths kept out: contents, folder listings and
/// attribute values are denied with every write, run, link and clone, while names, sizes
/// and dates stay readable, so a tool that only stats such a path does not fail.
const KEPT_OUT_DENY: &str =
    "(deny file-read-data file-read-xattr file-write* process-exec file-link file-clone";
/// The longest quoted string or regex, in raw bytes, Seatbelt's profile reader takes: one
/// byte more fails the whole profile with `Error reading string`, whatever the bytes are
/// (probe 2026-11-12: 1023 read, 1024 refused, backslashes and multibyte letters alike).
pub(crate) const SBPL_STRING_MAX: usize = 1023;
/// The most exact Unix sockets one agent may bind and connect: its own Codex app-server's.
const SOCKETS_MAX: usize = 2;
/// The most exact Unix socket grants one agent holds, of every use together: a native lead's
/// own tmux server, its caller socket and its teammates' tmux server.
const SOCKET_GRANTS_MAX: usize = 4;
/// The writes that move, remove, replace, re-permission or flag a folder's own node while
/// its contents stay writable: every writable root, the scratch root and a native lead's
/// home for its teammates. A flag such as `uchg` would freeze the folder for every other
/// writer and for the engine (CTX-M1 r1).
const FIXED_WRITES: &str =
    "file-write-unlink file-write-create file-write-mode file-write-acl file-write-flags";
/// The writes that move, remove, re-permission or flag a folder's own node but still let it
/// be made where it is missing: an agent home's `skills` folder, which Codex makes in a fresh
/// home for its own skills, while no folder built elsewhere replaces the one holding the
/// engine's copies and no flag keeps the engine from removing what is in it (CTX-M1 r1).
const PINNED_WRITES: &str = "file-write-unlink file-write-mode file-write-acl file-write-flags";
/// The folders of a Claude Code config home that its agent team shares, the members'
/// mailboxes and task lists. Claude Code 2.1.309 keeps them under each member's config home,
/// with no variable of their own, so a native lead's teammates write the lead's (TPH-2).
pub const CLAUDE_TEAM_SHARED: [&str; 2] = ["teams", "tasks"];
/// The Seatbelt pattern of a lock folder's path, `<file>.lock`: the only folder a teammate
/// may still make in a shared team folder, where it may make no other folder or link.
const LOCK_FOLDER: &str = r#"\.lock$"#;
/// The longest Unix socket path macOS binds, `sun_path` less its closing NUL.
const SOCKET_PATH_BYTES_MAX: usize = 103;
const TOOL_ROOTS_MAX: usize = 24;
const TOOL_FILES_MAX: usize = 5;
const EXECS_MAX: usize = 4;
const GIT_LINK_BYTES_MAX: u64 = 2 * PATH_BYTES_MAX as u64;
const REMOTES_MAX: usize = 8;
#[cfg(unix)]
const PROFILE_BYTES_MAX: usize = 64 * 1024;
const AGENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin";

const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/Library/Preferences",
    "/Library/Developer/CommandLineTools",
    "/private/etc",
    "/private/var/db",
    "/private/var/select",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/opt/homebrew/lib",
    "/opt/homebrew/libexec",
    "/opt/homebrew/opt",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/Caskroom",
    "/opt/homebrew/share",
    "/opt/homebrew/etc",
    "/opt/homebrew/include",
    "/usr/local/bin",
    "/usr/local/lib",
    "/usr/local/opt",
    "/usr/local/Cellar",
    "/usr/local/share",
    "/usr/local/etc",
    "/usr/local/include",
];

const METADATA_LITERALS: &[&str] = &[
    "/",
    "/etc",
    "/var",
    "/tmp",
    "/private",
    "/private/var",
    "/opt",
    "/opt/homebrew",
    "/usr/local",
    "/Library",
    "/Library/Developer",
];

const MACH_SERVICES: &[&str] = &[
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.system.DirectoryService.libinfo_v1",
    "com.apple.PowerManagement.control",
    "com.apple.bsd.dirhelper",
    "com.apple.system.logger",
    "com.apple.logd",
    "com.apple.diagnosticd",
    "com.apple.system.notification_center",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.ocspd",
    "com.apple.networkd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.SystemConfiguration.configd",
];

const KEYCHAIN_SERVICES: &[&str] = &[
    "com.apple.SecurityServer",
    "com.apple.securityd",
    "com.apple.securityd.xpc",
    "com.apple.secd",
    "com.apple.securityd.ckks",
    "com.apple.securityd.sos",
    "com.apple.security.agent",
    "com.apple.security.agent.login",
    "com.apple.security.authhost",
    "com.apple.security.keychainsyncingoveridsproxy",
    "com.apple.security.cloudkeychainproxy3",
    "com.apple.security.keychain-circle-notification",
];

const OWNER_SECRET_PATHS: &[&str] = &[
    "Library/Keychains",
    ".ssh",
    ".gnupg",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".config/git/credentials",
    ".aws",
    ".docker",
    ".kube",
    ".codex",
    ".claude",
    ".claude.json",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".pypirc",
    ".config/gcloud",
];

const SYSTEM_SECRET_PATHS: &[&str] = &[
    "/Library/Keychains",
    "/private/var/db/SystemKey",
    "/opt/homebrew/etc/npmrc",
    "/usr/local/etc/npmrc",
];

const CREDENTIAL_NAMES: &[&str] = &[
    ".npmrc",
    "etc/npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
    "credentials.toml",
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gcloud",
    ".docker",
    ".kube",
    "pip.conf",
    ".yarnrc.yml",
];

const ENGINE_SECRET_FILES: &[&str] = &[
    "engine.db",
    "engine.db-wal",
    "engine.db-shm",
    "engine.db-journal",
    "owner.sock",
    "github.json",
    "github.toml",
];

const ENGINE_SECRET_FOLDERS: &[&str] = &[
    "account-keys",
    crate::terminal::FOLDER,
    crate::ide::FOLDER,
    crate::engine::bootstrap::NATIVE_PROFILES_FOLDER,
    crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER,
    "plugin-keys",
    #[cfg(unix)]
    crate::storage::migrations::BACKUPS,
];

const RUNTIME_ENTRIES_MAX: usize = 1024;

/// Why a sandbox root is refused when it equals, holds or sits inside a secret path from
/// [`ProjectSandbox::secret_paths`]: an allow-list boundary such as Landlock or bubblewrap
/// cannot carve the secret back out of a granted root.
pub const SECRET_ROOT_REFUSED: &str = "sandbox roots must not hold or sit in a secret path";

const AGENT_KEY_VARIABLES: &[&str] = &["ANTHROPIC_API_KEY"];

const AGENT_ENV_MAX: usize = 128;

const REFUSED_VARIABLES: &[&str] = &[
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "PERL5LIB",
    "PERL5OPT",
    "PERLLIB",
    "PERL5DB",
];

const REFUSED_PREFIXES: &[&str] = &[
    "GH_",
    "GITHUB_",
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "AWS_",
    "AZURE_",
    "CRAZYAGENT_",
];

const REFUSED_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_TOKEN",
    "_SECRET",
    "_SECRET_KEY",
    "_PASSWORD",
    "_ACCESS_KEY",
    "_CREDENTIALS",
];

/// The git settings every agent process gets, sandboxed or at Full access: no system git
/// config (whose `credential.helper=osxkeychain` would ask the owner's Keychain), no
/// credential helper at all, no terminal prompt, and no bare repository found by
/// discovery (`safe.bareRepository=explicit`, SR1-2), so a folder another agent laid out
/// as one never runs its config, while `--git-dir` still opens it. They are set after the
/// harness's own variables, so a harness cannot replace them.
pub const AGENT_GIT_ENV: [(&str, &str); 7] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "credential.helper"),
    ("GIT_CONFIG_VALUE_0", ""),
    ("GIT_CONFIG_KEY_1", "safe.bareRepository"),
    ("GIT_CONFIG_VALUE_1", "explicit"),
];

/// The settings every agent process gets, sandboxed or at Full access, that keep its tools
/// from reading config an earlier run left in its home, which a launch resets
/// (RV-DS241-137, sec r1 F1): no global git config, as the engine's own git runs, so a
/// `.gitconfig` or `.config/git/config` there never runs its hooks or programs, no Python
/// user site, so a `.pth` file or `usercustomize` module there never runs as Python starts,
/// and no npm user config, so an `.npmrc` there never hands its `node-options` or
/// `script-shell` to a script. They are set after the harness's own variables, so a harness
/// cannot replace them, and never in the owner's IDE terminal, whose home is the owner's own
/// and no launch resets ([`owner_terminal_env`], code r1 F3).
pub const AGENT_HOME_ENV: [(&str, &str); 3] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("PYTHONNOUSERSITE", "1"),
    ("NPM_CONFIG_USERCONFIG", "/dev/null"),
];

/// The root certificates every sandboxed agent process trusts for TLS on macOS: the system
/// bundle, named in `SSL_CERT_FILE`. A harness that reads its roots from the keychain, as
/// Codex does through rustls-native-certs, finds none there because every agent sandbox
/// denies the keychain services, and fails every HTTPS request. Elsewhere the harness's
/// own platform store is left as it is.
#[cfg(target_os = "macos")]
pub const AGENT_TLS_ENV: [(&str, &str); 1] = [("SSL_CERT_FILE", "/private/etc/ssl/cert.pem")];

/// No fixed TLS setting outside macOS: the platform's own certificate store is reachable.
#[cfg(not(target_os = "macos"))]
pub const AGENT_TLS_ENV: [(&str, &str); 0] = [];

/// Tells whether an environment variable name may never reach an agent process: GitHub and
/// git credential settings, the owner's SSH agent, cloud and provider keys, tokens,
/// secrets, passwords, the perl variables that would make a pane's keeper, a perl outside
/// the sandbox, load other code, and the engine's own `CRAZYAGENT_` settings, including the model key
/// name `ANTHROPIC_API_KEY`, which passes only with the local placeholder (see
/// [`agent_variable_allowed`]). Input: the variable name. Output: true when refused.
pub fn agent_env_refused(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let upper = name.to_ascii_uppercase();
    REFUSED_VARIABLES.contains(&upper.as_str())
        || REFUSED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || REFUSED_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix))
}

/// Tells whether one variable may reach an agent process with this value: a name
/// [`agent_env_refused`] accepts, one of the fixed [`AGENT_GIT_ENV`] pairs, a model key
/// name holding the local placeholder, never a real provider key, or the launch's lineage
/// token in its own variable. No variable may hold a key relay token: any process of the
/// owner's user can read another's environment, so a harness reads its token from the key
/// file in its own home. Inputs: the name and value. Output: true when allowed.
pub fn agent_variable_allowed(name: &OsStr, value: &OsStr) -> bool {
    let token = crate::model_gateway::relay::TOKEN_PREFIX.as_bytes();
    if value
        .as_encoded_bytes()
        .windows(token.len())
        .any(|window| window == token)
    {
        return false;
    }
    if name == crate::harnesses::leftovers::LINEAGE_VARIABLE {
        return value
            .to_str()
            .is_some_and(crate::harnesses::leftovers::LineageToken::valid);
    }
    if AGENT_GIT_ENV
        .iter()
        .chain(AGENT_HOME_ENV.iter())
        .any(|(git, fixed)| name == *git && value == *fixed)
    {
        return true;
    }
    if name
        .to_str()
        .is_some_and(|name| AGENT_KEY_VARIABLES.contains(&name))
    {
        return value == crate::model_gateway::relay::LOCAL_PLACEHOLDER;
    }
    !agent_env_refused(name)
}

/// The zsh temp-file prefix inside an agent's temp folder. zsh writes here-documents to
/// `$TMPPREFIX`, `/tmp/zsh` by default, which no agent sandbox can write, so every agent
/// shell gets its own. Input: the agent's temp folder. Output: the prefix path.
pub fn shell_tmp_prefix(tmp: &Path) -> PathBuf {
    tmp.join("zsh")
}

/// Builds the environment of every agent process in launch order: `base`, then the
/// harness's variables (a later value replaces an earlier one and None removes it, but
/// `HOME`, `TMPDIR` and `TMPPREFIX`, and the tool cache's [`TOOL_CACHE_ENV`] when `base`
/// sets them, always stay as `base` set them), then `TMPPREFIX` from
/// [`shell_tmp_prefix`] when `TMPDIR` is set, then [`AGENT_GIT_ENV`], [`AGENT_TLS_ENV`] and
/// [`AGENT_HOME_ENV`].
/// Inputs: the base and harness variables. Output: the variables, or `Invalid` when any
/// one fails [`agent_variable_allowed`].
pub fn agent_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    built_env(base, harness, &AGENT_HOME_ENV)
}

/// Builds the environment of the owner's confined IDE terminal as [`agent_env`] does for an
/// agent, but without [`AGENT_HOME_ENV`]: its home is the owner's own, kept across
/// terminals, so the global git config, Python user site and npm config the owner sets up
/// there stay in use (RV-DS241-137 code r1 F3), and its profile leaves their files writable
/// ([`ProjectSandbox::keep_owner_config`], RV-DS241-188). Inputs: the base and terminal
/// variables. Output: the variables, or `Invalid` when any one fails
/// [`agent_variable_allowed`].
pub fn owner_terminal_env<'a>(
    base: &[(&str, &OsStr)],
    terminal: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    built_env(base, terminal, &[])
}

/// Builds the environment [`agent_env`] describes, setting `home` after the fixed git and
/// TLS settings. Inputs: the base and harness variables and the home settings. Output: the
/// variables, or `Invalid` when any one fails [`agent_variable_allowed`].
fn built_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
    home: &[(&'static str, &'static str)],
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    let refused = || invalid("agent environment holds an owner secret");
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    let set = |env: &mut Vec<(OsString, OsString)>, name: &OsStr, value: Option<&OsStr>| {
        let at = env.iter().position(|(known, _)| known == name);
        match (at, value) {
            (Some(at), Some(value)) => env[at].1 = value.to_owned(),
            (Some(at), None) => {
                env.remove(at);
            }
            (None, Some(value)) => env.push((name.to_owned(), value.to_owned())),
            (None, None) => {}
        }
    };
    for (name, value) in base {
        if !agent_variable_allowed(OsStr::new(name), value) {
            return Err(refused());
        }
        set(&mut env, OsStr::new(name), Some(value));
    }
    for (name, value) in harness {
        if value.is_some_and(|value| !agent_variable_allowed(name, value)) {
            return Err(refused());
        }
        let pinned_cache = TOOL_CACHE_ENV.iter().any(|pinned| name == *pinned)
            && base.iter().any(|(known, _)| name == *known);
        if name == "HOME" || name == "TMPDIR" || name == "TMPPREFIX" || pinned_cache {
            continue;
        }
        set(&mut env, name, value);
    }
    let prefix = env
        .iter()
        .find(|(name, _)| name == "TMPDIR")
        .map(|(_, tmp)| shell_tmp_prefix(Path::new(tmp)));
    if let Some(prefix) = prefix {
        set(&mut env, OsStr::new("TMPPREFIX"), Some(prefix.as_os_str()));
    }
    let fixed = AGENT_GIT_ENV
        .into_iter()
        .chain(AGENT_TLS_ENV)
        .chain(home.iter().copied());
    for (name, value) in fixed {
        set(&mut env, OsStr::new(name), Some(OsStr::new(value)));
    }
    if env.len() > AGENT_ENV_MAX {
        return Err(invalid("agent environment holds too many variables"));
    }
    assert!(
        env.iter()
            .all(|(name, value)| agent_variable_allowed(name, value))
    );
    Ok(env)
}

/// The end of the agent shims: forks the run's warden when handed a [`warden::WARDEN_FD`],
/// keeping that descriptor and variable from the harness, then replaces the shim with the
/// harness program and its arguments. Asked `S` on its line, the warden stops every other
/// process in the sandbox three times over, through the kill call that spares its sender,
/// and answers `s` only when every pass took; asked `C`, it continues them and answers `c`
/// (DEC-158). At the line's end it kills them all, as before.
#[cfg(unix)]
macro_rules! warden_then_harness {
    () => {
        r#"if (defined(my $fd = delete $ENV{CRAZYAGENT_WARDEN_FD})) { my $engine = getppid(); $fd =~ /\A[0-9]{1,6}\z/ && $engine > 1 or die "crazyagent: no warden\n"; defined(my $warden = fork()) or die "crazyagent: no warden: $!\n"; if (!$warden) { POSIX::setsid(); POSIX::close($_) for 0, 1, 2; $SIG{PIPE} = "IGNORE"; open(my $line, "+<&=", $fd) or POSIX::_exit(1); my $armed = !kill(0, $engine) && $!{EPERM}; syswrite($line, $armed ? "A" : "U"); $armed or POSIX::_exit(0); my $byte; while (sysread($line, $byte, 1)) { if ($byte eq "S") { my $stopped = 1; for (1 .. 3) { $stopped &&= syscall(37, -1, POSIX::SIGSTOP(), 0) == 0; select(undef, undef, undef, 0.05) } syswrite($line, "s") if $stopped } elsif ($byte eq "C") { kill("CONT", -1); syswrite($line, "c") } } syswrite($line, "F"); kill(9, -1); POSIX::_exit(1) } POSIX::close($fd) } exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n""#
    };
}

/// The session shim a headless agent starts through inside `sandbox-exec`, followed by the
/// harness program and its arguments. It joins the engine's process group, starts a new
/// session and replaces itself with the harness, so the agent holds no controlling terminal
/// and its pid stays its process group. macOS can still see the group it just left for a
/// moment under load, so a refused new session is asked again for about a second before
/// it exits unstarted. Handed a [`warden::WARDEN_FD`], it first forks the run's warden
/// (see [`warden::Warden`]) and keeps that descriptor and variable from the harness.
#[cfg(unix)]
pub const SESSION_SHIM: [&str; 5] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-e",
    concat!(
        r#"setpgrp(0, getpgrp(getppid())) or die "crazyagent: no new session: $!\n"; my $tries = 0; until (POSIX::setsid() > 0) { $!{EPERM} && ++$tries < 200 or die "crazyagent: no new session: $!\n"; select(undef, undef, undef, 0.005) } "#,
        warden_then_harness!()
    ),
    "--",
];

/// The shim a pane agent starts through inside `sandbox-exec`, followed by the harness
/// program and its arguments: the [`SESSION_SHIM`] without its new session, since a pane's
/// harness keeps the pane's terminal and process group. Handed a [`warden::WARDEN_FD`] by
/// the pane's [`KEEPER`], it forks the run's warden first.
#[cfg(target_os = "macos")]
pub const PANE_SHIM: [&str; 5] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-e",
    warden_then_harness!(),
    "--",
];

/// A pane's first process, outside the sandbox, followed by the sandboxed command it keeps.
/// It starts that command holding one end of a socket pair as the run's warden descriptor
/// and waits until the command exits or the pane is asked to stop with hangup or terminate.
/// Then it shuts its end, so the warden kills every process left in the sandbox, sessions
/// of their own included, and it exits only once the warden confirmed that or found
/// signals not confined; otherwise it keeps the pane open, ignoring terminate, until a
/// hangup. A confining profile keeps every agent signal from it, and the terminal keys
/// that signal the pane's group do not end it. Given [`KEEPER_DEADLINE`], as a native lead's
/// server keeper is, it also reads the engine's deadline record and, once one read found the
/// deadline ahead, stops after that many reads in a row found it passed or unreadable
/// (FBTMUX-P1-R1-L1): it kills its command, the server, itself from outside the sandbox, so
/// a killed warden cannot keep the server, then has the warden end the rest, and exits
/// with status 3 when the warden does not confirm. Given the engine's freeze record, as the
/// word before a kept pane's `sandbox-exec` (see [`kept_pane`]) or in [`KEEPER_STATE`] for a
/// native lead's server keeper, never both, user signal 1 asks the warden to stop every
/// process in the sandbox and user signal 2 to continue them, and each answer is written to
/// that record as `S` or `C` (DEC-158); without it both signals are ignored. Its deadline reads count wall time, so answers do not hasten
/// them. Given [`KEEPER_LIFELINE`], as the server keepers of a lead on an engine key relay
/// are, it also reads its standard input, the engine's end of a pipe its command never gets,
/// and once that closes, as at the engine's death, it stops as at a passed deadline (B6b).
#[cfg(target_os = "macos")]
pub const KEEPER: [&str; 6] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-MSocket",
    "-e",
    r#"$^F = 1023; my $why = ""; my ($every, $late, $record) = split(/ /, delete($ENV{CRAZYAGENT_KEEPER_DEADLINE}) // "", 3); !defined($record) || ($every =~ /\A[1-9][0-9]{0,3}\z/ && $late =~ /\A[1-9][0-9]{0,2}\z/ && $record =~ m{\A/}) or die "crazyagent: no deadline\n"; my $state = delete($ENV{CRAZYAGENT_KEEPER_STATE}); if (@ARGV > 2 && $ARGV[1] eq "/usr/bin/sandbox-exec") { !defined($state) or die "crazyagent: two state records\n"; $state = shift(@ARGV) } !defined($state) || $state =~ m{\A/[^\n\0]{1,1000}\z} or die "crazyagent: no state record\n"; my $life = delete($ENV{CRAZYAGENT_KEEPER_LIFELINE}); !defined($life) || $life eq "0" or die "crazyagent: no lifeline\n"; my ($next, $armed, $missed, $want) = (time + ($every // 0), 0, 0, ""); $SIG{$_} = "IGNORE" for qw(INT QUIT TSTP TTIN TTOU PIPE USR1 USR2); if (defined($state)) { $SIG{USR1} = sub { $want = "S" }; $SIG{USR2} = sub { $want = "C" } } $SIG{HUP} = sub { $why ||= "HUP" }; $SIG{TERM} = sub { $why ||= "TERM" }; $SIG{CHLD} = sub { 1 }; socketpair(my $keep, my $line, AF_UNIX, SOCK_STREAM, PF_UNSPEC) or die "crazyagent: no warden: $!\n"; defined(my $harness = fork()) or die "crazyagent: no warden: $!\n"; if (!$harness) { $SIG{$_} = "DEFAULT" for qw(INT QUIT TSTP TTIN TTOU PIPE HUP TERM CHLD USR1 USR2); close($keep); if (defined($life)) { open(STDIN, "<", "/dev/null") or die "crazyagent: no null input: $!\n" } $ENV{CRAZYAGENT_WARDEN_FD} = fileno($line); exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n" } close($line); my ($seen, $open, $wanted) = ("", 1, ""); vec($wanted, fileno($keep), 1) = 1; until ($why || waitpid($harness, POSIX::WNOHANG()) > 0) { if (length($want)) { syswrite($keep, $want) if $open; $want = "" } my $ready = $open ? $wanted : ""; vec($ready, 0, 1) = 1 if defined($life); my $count = select($ready, undef, undef, 1); if ($count > 0 && defined($life) && vec($ready, 0, 1)) { my $got = sysread(STDIN, my $any, 64); $why ||= "GONE" if defined($got) ? !$got : !$!{EINTR} } if ($count > 0 && $open && vec($ready, fileno($keep), 1) && defined(my $got = sysread($keep, my $more, 8))) { $open = 0 if !$got; $seen .= $more // ""; if ($seen =~ s/([sc]+)\z// && defined($state)) { my $now = uc(substr($1, -1)); unlink("$state.new"); if (sysopen(my $out, "$state.new", POSIX::O_WRONLY() | POSIX::O_CREAT() | POSIX::O_EXCL(), 0600)) { syswrite($out, $now); close($out); rename("$state.new", $state) or unlink("$state.new") } } } next unless defined($record) && time >= $next; $next = time + $every; my $ahead = 0; if (my @at = lstat($record)) { if (-f _ && $at[4] == $< && $at[7] <= 19 && open(my $in, "<", $record)) { $ahead = (<$in> // "") =~ /\A([0-9]{1,19})\z/ && time < $1 } } if ($ahead) { ($armed, $missed) = (1, 0) } elsif ($armed && ++$missed >= $late) { $why = "LATE" } } kill("KILL", $harness) if $why eq "LATE" || $why eq "GONE"; shutdown($keep, 1); my $report = ""; my $until = time + 4; while ($open && time < $until && length($report) < 8) { select(my $ready = $wanted, undef, undef, 0.25) > 0 or next; sysread($keep, my $more, 8) or last; $report .= $more } my $said = $seen . $report; POSIX::_exit(0) if $said =~ /\AA[sc]*F\z/ || $said eq "U" || $why eq "HUP"; POSIX::_exit(3) if $why eq "LATE" || $why eq "GONE"; $SIG{TERM} = "IGNORE"; $SIG{HUP} = "DEFAULT"; sleep(3600) while 1"#,
    "--",
];

/// The variable a native lead's server keeper takes its deadline reads from
/// (FBTMUX-P1-R1-L1): the seconds between two reads, how many reads in a row past the
/// deadline end the server, and the engine's absolute deadline record, separated by single
/// spaces. The [`KEEPER`] removes it before the command starts; a kept pane has none.
#[cfg(target_os = "macos")]
pub const KEEPER_DEADLINE: &str = "CRAZYAGENT_KEEPER_DEADLINE";

/// The variable that asks a native lead's server [`KEEPER`] to watch its standard input as
/// the engine's lifeline, for a lead on an engine key relay (B6b); `0` is the only value it
/// takes. The keeper removes it and starts its command with `/dev/null` as input.
#[cfg(target_os = "macos")]
pub const KEEPER_LIFELINE: &str = "CRAZYAGENT_KEEPER_LIFELINE";

/// The variable that names the engine's absolute freeze record to a native lead's server
/// [`KEEPER`], whose warden then stops and continues the sandbox on the engine's signals
/// (DEC-158). The keeper removes it before the command starts. A pane's variables never
/// carry it, since the pane variable gate refuses the engine's own; a kept pane takes the
/// record as an argument instead (see [`kept_pane`]).
#[cfg(target_os = "macos")]
pub const KEEPER_STATE: &str = "CRAZYAGENT_KEEPER_STATE";

/// The longest freeze record a [`KEEPER`] takes, in bytes: its leading slash and at most
/// 1000 more, as the keeper's own check reads it.
#[cfg(target_os = "macos")]
const KEEPER_STATE_BYTES_MAX: usize = 1001;

/// Puts a pane's sandboxed command under the [`KEEPER`] and the [`PANE_SHIM`], so stopping
/// the pane ends every process its agent started. Inputs: the argv `sandbox-exec -f
/// <profile> <program> <arguments>` and the engine's freeze record, which goes to the keeper
/// as the word before `sandbox-exec` and never reaches the command (DEC-158). Output: the
/// kept argv, or none for any other argv or a record that is not an absolute path of at
/// most [`KEEPER_STATE_BYTES_MAX`] bytes on one line.
#[cfg(target_os = "macos")]
pub fn kept_pane(argv: Vec<String>, state: Option<&Path>) -> Option<Vec<String>> {
    if argv.len() < 4 || argv[0] != SANDBOX_EXEC || argv[1] != "-f" {
        return None;
    }
    let state = match state {
        None => None,
        Some(record) => {
            let word = record.to_str()?;
            if !word.starts_with('/')
                || !(2..=KEEPER_STATE_BYTES_MAX).contains(&word.len())
                || word.contains(['\n', '\0'])
            {
                return None;
            }
            Some(word.to_owned())
        }
    };
    let stated = usize::from(state.is_some());
    let length = argv.len();
    let mut kept: Vec<String> = KEEPER.iter().map(|part| (*part).to_owned()).collect();
    kept.extend(state);
    let mut rest = argv.into_iter();
    kept.extend(rest.by_ref().take(3));
    kept.extend(PANE_SHIM.iter().map(|part| (*part).to_owned()));
    kept.extend(rest);
    assert_eq!(kept.len(), KEEPER.len() + stated + PANE_SHIM.len() + length);
    assert_eq!(kept[KEEPER.len() + stated], SANDBOX_EXEC);
    assert!(stated == 0 || kept[KEEPER.len()].starts_with('/'));
    Some(kept)
}

/// Takes a pane command back out of the [`KEEPER`] and the [`PANE_SHIM`], for a pane that
/// starts in a server already running inside its sandbox, where neither can run. Input: an
/// argv. Output: the `sandbox-exec -f <profile> <program> <arguments>` that [`kept_pane`]
/// kept without a freeze record when keeping it again rebuilds exactly this argv, or none
/// for any other argv, one kept with a freeze record included.
#[cfg(target_os = "macos")]
pub fn unkept_pane(argv: &[String]) -> Option<Vec<String>> {
    let shim = KEEPER.len() + 3;
    let program = shim + PANE_SHIM.len();
    if argv.len() <= program || argv[KEEPER.len()] != SANDBOX_EXEC || argv[KEEPER.len() + 1] != "-f"
    {
        return None;
    }
    let mut plain = argv[KEEPER.len()..shim].to_vec();
    plain.extend_from_slice(&argv[program..]);
    (kept_pane(plain.clone(), None).as_deref() == Some(argv)).then_some(plain)
}

/// Builds the one command every sandboxed headless agent process starts from: `-f
/// <profile>`, the [`SESSION_SHIM`] and `<program>` under `sandbox-exec`, a cleared
/// environment holding exactly [`agent_env`], and its own process group. Inputs: the
/// installed profile, the absolute program, the base variables and the harness's variables
/// (None removes one). Output: the command, or `Invalid` when a variable fails
/// [`agent_variable_allowed`].
#[cfg(unix)]
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let env = agent_env(base, harness)?;
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-f")
        .arg(profile)
        .args(SESSION_SHIM)
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0);
    assert_eq!(command.get_program(), SANDBOX_EXEC);
    assert_eq!(
        command.get_args().nth(2 + SESSION_SHIM.len()),
        Some(program.as_os_str())
    );
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// Builds the command a Full access agent process starts from (DEC-102): the
/// [`SESSION_SHIM`] and `<program>` with no `sandbox-exec` and no profile, so the agent has
/// the owner's user permissions, yet the same cleared environment holding exactly
/// [`agent_env`] and its own process group as a sandboxed agent. Inputs: the absolute
/// program, the base variables and the harness's variables (None removes one). Output: the
/// command, or `Invalid` when a variable fails [`agent_variable_allowed`].
#[cfg(unix)]
pub fn unconfined_command<'a>(
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    if !clean_absolute(program) {
        return Err(invalid("agent program must be an absolute plain path"));
    }
    let env = agent_env(base, harness)?;
    let mut command = Command::new(SESSION_SHIM[0]);
    command
        .args(&SESSION_SHIM[1..])
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0);
    assert_eq!(command.get_program(), SESSION_SHIM[0]);
    assert_eq!(
        command.get_args().nth(SESSION_SHIM.len() - 1),
        Some(program.as_os_str())
    );
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// Refuses a Full access agent on Windows (DEC-102): the level is unavailable there, so no
/// command starts without the Windows boundary. Inputs: as the Unix version. Output:
/// always `Unavailable` with [`FULL_ACCESS_NEEDS_MACOS`].
#[cfg(target_os = "windows")]
pub fn unconfined_command<'a>(
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let _ = (program, base, harness.into_iter().count());
    Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()))
}

/// Builds the one command every Windows agent process starts from: the boundary helper next
/// to the engine's executable with `--profile <profile> -- <program>`, and a cleared
/// environment holding exactly [`agent_env`]. Inputs: the installed boundary profile, the
/// absolute program, the base variables and the harness's variables (None removes one).
/// Output: the command, `Unavailable` without the helper, or `Invalid` for an unsafe input.
#[cfg(target_os = "windows")]
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    windows::agent_command(profile, program, base, harness)
}

/// Network reach of a sandboxed agent: open when the agent's tools are on, except that
/// it never binds or listens on an [`OWNER_PORTS`] port, one [`ProjectSandbox::keep_ports`]
/// keeps or one of [`model_proxy::PROXY_PORTS`] or [`model_proxy::RELAY_PORTS`], otherwise
/// only the listed remotes, each one port on loopback or on any host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNetwork {
    Open,
    Only(Vec<SandboxRemote>),
}

/// One outbound TCP destination a tools-off agent may still reach. Seatbelt
/// filters by port only, either on loopback or on any host, so `accounts::account_url`
/// refuses an endpoint on another machine on a port a cloud metadata service answers on
/// when it is saved (MACOS-ANYHOST-METADATA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxRemote {
    Loopback(u16),
    AnyHost(u16),
}

impl SandboxRemote {
    /// The destination a tools-off agent needs for one model endpoint: the URL's port on
    /// loopback for a local server, otherwise that port on any host. Input: an http or
    /// https URL. Output: the remote, or a refusal for another scheme, host or port.
    pub fn for_endpoint(url: &str) -> Result<Self, SandboxError> {
        let parsed = url::Url::parse(url).map_err(|_| invalid("endpoint URL is invalid"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid("endpoint URL must be http or https"));
        }
        let port = parsed
            .port_or_known_default()
            .filter(|port| *port != 0)
            .ok_or_else(|| invalid("endpoint port is missing"))?;
        let loopback = match parsed.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => return Err(invalid("endpoint host is missing")),
        };
        assert_ne!(port, 0);
        Ok(if loopback {
            Self::Loopback(port)
        } else {
            Self::AnyHost(port)
        })
    }
}

/// How a limited-network agent's sockets are held on Linux, the owner's setting "Linux
/// limited-network sockets". Landlock limits outbound TCP by port only, so a UDP, ping or
/// other socket would reach any host. `TcpOnly` (the default) refuses every socket but TCP
/// and has names looked up over TCP, `Refuse` starts no limited-network agent, and
/// `TcpPorts` keeps only Landlock's TCP port rules. Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSockets {
    #[default]
    TcpOnly,
    Refuse,
    TcpPorts,
}

/// Whether a Linux agent runs under the call supervisor where this Linux holds it, the
/// owner's setting "Linux call supervisor" (DEC-179 addendum 2). `On` (the default) adds
/// the guards a launch needs: ports on this computer only, no new `.git` outside the
/// agent's own git folders, signals only to programs it started. `Off` keeps the launch
/// and every line as they were before the supervisor. Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSupervisor {
    #[default]
    On,
    Off,
}

/// Whether a Linux launch that does not hold the full Linux sandbox is refused, the owner's
/// setting "Refuse agents without the full Linux sandbox" (DEC-185). `Off` (the default)
/// starts it with what this Linux holds and the lines that say what it cannot; `On` refuses
/// it unless [`linux_holds_fully`] does. Team agents need the full sandbox whatever it says.
/// Other systems ignore it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxStrict {
    #[default]
    Off,
    On,
}

/// The engine refusal code of a Linux launch the strict setting refuses ([`LinuxStrict`]).
pub const LINUX_NOT_FULL: &str = "linux_sandbox_not_full";

/// Why [`InstalledSandbox::install`] refuses a Linux launch under [`LinuxStrict::On`]: it
/// does not hold the full Linux sandbox ([`linux_holds_fully`]).
pub const LINUX_NOT_FULL_LINE: &str = "this Linux does not hold the full sandbox for this launch, and the setting 'Refuse agents without the full Linux sandbox' is on";

/// Tells whether a launch holds the full Linux sandbox (DEC-185): a process space of its own
/// (the nested backend), the call supervisor running, and its network limits held, so none
/// of the gap lines would show for it. An open network has no limit to hold; a limited one
/// needs TCP-only sockets and, for a port on any host, a network of its own (DEC-179 SBX-2).
/// Inputs: what the launch got and its sandbox. Output: true when it holds all of it.
pub fn linux_holds_fully(
    nested: bool,
    supervised: bool,
    routed: bool,
    sandbox: &ProjectSandbox,
) -> bool {
    assert!(
        !routed || nested,
        "only a nested launch has a network of its own"
    );
    if !nested || !supervised {
        return false;
    }
    let SandboxNetwork::Only(remotes) = &sandbox.network else {
        return true;
    };
    if sandbox.linux_sockets != LinuxSockets::TcpOnly {
        return false;
    }
    remotes.iter().all(|remote| match remote {
        SandboxRemote::Loopback(port) => *port >= BRIDGE_PORT_MIN,
        SandboxRemote::AnyHost(_) => routed,
    })
}

/// Tells whether a launch refusal is the strict setting's ([`LINUX_NOT_FULL_LINE`]), so a
/// caller can give it its own code ([`LINUX_NOT_FULL`]). Input: the refusal. Output: the
/// answer.
pub fn linux_not_full(error: &SandboxError) -> bool {
    matches!(error, SandboxError::Unavailable(reason) if reason == LINUX_NOT_FULL_LINE)
}

/// The launch refusal when the Linux sockets setting starts no limited-network agent.
pub const LINUX_LIMITED_NETWORK_OFF: &str = "linux_limited_network_off";

/// Why a sandbox with an open network does not start on a Linux that cannot keep ports
/// free. Only Landlock network rules (Linux 6.7 or newer) keep an agent's listeners off the
/// owner's fixed ports, the kept local model ports and the model proxy ports; without them
/// the launch is refused instead of started unprotected (DEC-137 addendum 2).
pub const LINUX_OPEN_NETWORK_NEEDS_LANDLOCK: &str = "keeping the owner's ports free of an \
     open-network agent needs Landlock network rules (Linux 6.7 or newer)";

/// The launch refusal when, under `TcpOnly`, a harness that looks host names up is not a
/// glibc program: only glibc's resolver asks over TCP, so it could not reach its model.
pub const LINUX_RESOLVER_NEEDS_UDP: &str = "linux_resolver_needs_udp";

/// The launch refusal when, under `TcpOnly`, a harness that looks host names up is a script
/// whose interpreter cannot be checked ([`elf::Lookup::Unjudged`]), such as a script that
/// runs another script: the check follows a `#!` line one level only and never guesses.
pub const LINUX_HARNESS_UNCHECKED: &str = "linux_harness_unchecked";

/// The `.git` folders whose contents the owner's git reads and runs: hook scripts, rebase
/// and sequencer plans, and remote and branch definitions. No macOS or Linux agent writes
/// in them: Seatbelt denies each with everything below it, and Linux never grants one
/// (RV-DS241-85).
pub const GIT_STATE_TREES: [&str; 6] = [
    "hooks",
    "rebase-merge",
    "rebase-apply",
    "remotes",
    "branches",
    "sequencer",
];

/// The single `.git` files no macOS or Linux agent writes: the config, the index, their
/// locks, and the links git follows to a common dir or a worktree config. Seatbelt denies
/// each by name and Linux never grants one (RV-DS241-85).
pub const GIT_STATE_FILES: [&str; 6] = [
    "config",
    "config.lock",
    "index",
    "index.lock",
    "commondir",
    "config.worktree",
];

/// The links in a linked worktree's admin folder that tie it to the project and to its
/// worktree: no confined agent rewrites, removes or relinks one. Seatbelt denies each by
/// name, letting a worktree-adding profile create one, and the Linux call supervisor holds
/// each (DEC-185 S2a).
pub const ADMIN_LINKS: [&str; 2] = ["commondir", "gitdir"];

/// The files in a linked worktree's admin folder no confined agent writes: the branch it
/// stands on and the config git reads for that worktree alone. Seatbelt denies each by name
/// and the Linux call supervisor holds each (DEC-185 S2a).
pub const ADMIN_WRITES: [&str; 2] = ["HEAD", "config.worktree"];

/// The files in an object store's `info` folder that send git to another store: no
/// confined agent writes one. Seatbelt denies each by name in the project's store, the
/// store its checkout shares and a clone's own, and the Linux call supervisor holds each
/// in a clone (DEC-185 S2b).
pub const STORE_ALTERNATES: [&str; 2] = ["alternates", "http-alternates"];

/// The repository git serves beside a clone's git folder when that folder fails git's
/// repository test (RL-1): nothing is made there. Seatbelt denies it by name and the Linux
/// call supervisor holds it whole (DEC-185 S2b).
pub const GIT_FALLBACK: &str = ".git.git";

/// The pack folder's multi-pack-index, which can name a pack outside the store (RL-2): no
/// confined agent makes one. Seatbelt denies every name it begins, and the Linux call
/// supervisor holds the file and its `.d` chain folder, the two names git reads
/// (DEC-185 S2b).
pub const MULTI_PACK_INDEX: &str = "multi-pack-index";

/// The lowest port a loopback bridge carries on Linux: inside a network namespace of its
/// own an agent's helper has no capability to listen below it.
pub const BRIDGE_PORT_MIN: u16 = 1024;

/// The cloud metadata and credential services a VM reaches outside the link-local range,
/// which no routed agent and no engine tunnel may reach (CLOUD-METADATA-ADDRS): Alibaba ECS
/// and Azure's WireServer over IPv4; AWS IMDS, AWS EKS Pod Identity, Google, OCI, Akamai and
/// Scaleway over IPv6. Only these addresses, so the rest of their ranges, such as Tailscale's
/// 100.100.100.100, stays reachable.
pub(crate) const CLOUD_METADATA: [IpAddr; 8] = [
    IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
    IpAddr::V4(Ipv4Addr::new(168, 63, 129, 16)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023)),
    IpAddr::V6(Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x00c1, 0, 0, 0, 0, 0xa9fe, 0xa9fe)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0xa9fe, 0xa9fe, 0, 0, 0, 0, 0x0001)),
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0x0042, 0, 0, 0, 0, 0, 0x0042)),
];

/// Tells whether `address` is a cloud's metadata or credential service: link-local
/// (169.254.0.0/16, fe80::/10) or one of [`CLOUD_METADATA`], also as an IPv4 address inside
/// an IPv6 one. No engine tunnel or account probe dials one (ACCOUNTS-PROBE-MAY-DIAL).
/// Input: the address. Output: true when it is one.
pub(crate) fn metadata_address(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    };
    CLOUD_METADATA.contains(&address)
        || match address {
            IpAddr::V4(ip) => ip.is_link_local(),
            IpAddr::V6(ip) => ip.is_unicast_link_local(),
        }
}

/// What the owner is told when a port below [`BRIDGE_PORT_MIN`] keeps agents on this
/// computer's network on a Linux that could give them one of their own: on the project's
/// Agent access page and in the agent's messages at its launch. Input: the port. Output:
/// the sentences, the port named.
pub fn linux_unbridged_line(port: u16) -> String {
    format!(
        "On this Linux, port {port} is below {BRIDGE_PORT_MIN}, so agents that may reach it share this computer's network: every port they may reach on this computer is open to them on every host. CrazyAgent keeps agents to this computer only for ports {BRIDGE_PORT_MIN} and up."
    )
}

/// What a launch tells the owner in the agent's messages when a Linux that nests agents
/// still leaves it on this computer's network: for an agent that could have a network of
/// its own ([`ProjectSandbox::routed`]) on a host that does not route, [`LINUX_UNROUTED_LINE`]
/// or, with no loopback port or the supervisor on, [`LINUX_UNROUTED_ANY_HOST_LINE`]; none
/// when it routes; then [`linux_unbridged_line`] for the lowest port below
/// [`BRIDGE_PORT_MIN`], else under None [`LINUX_PORTS_ANY_HOST_LINE`]. Inputs: this Linux,
/// the network choice, the sandbox as launched. Output: the sentences, or none.
pub fn linux_unbridged_notice_on(
    host: Option<LinuxHost>,
    choice: policy::AgentNetwork,
    sandbox: &ProjectSandbox,
) -> Option<String> {
    let host = host.filter(|host| host.nested)?;
    let SandboxNetwork::Only(remotes) = &sandbox.network else {
        return None;
    };
    let reaches = remotes
        .iter()
        .any(|remote| matches!(remote, SandboxRemote::Loopback(_)));
    if sandbox.routed().is_some() {
        if host.routed {
            return None;
        }
        let wide = reaches && !host.supervised;
        let line = if wide {
            LINUX_UNROUTED_LINE
        } else {
            LINUX_UNROUTED_ANY_HOST_LINE
        };
        return Some(line.to_owned());
    }
    let low = remotes
        .iter()
        .filter_map(|remote| match remote {
            SandboxRemote::Loopback(port) if *port < BRIDGE_PORT_MIN => Some(*port),
            _ => None,
        })
        .min();
    let any = remotes
        .iter()
        .any(|remote| matches!(remote, SandboxRemote::AnyHost(_)));
    let tcp = sandbox.linux_sockets == LinuxSockets::TcpOnly;
    if let Some(port) = low.filter(|_| host.routed && any && tcp) {
        return Some(linux_unbridged_line(port));
    }
    if let Some(port) = sandbox.unbridged_port() {
        assert!(port < BRIDGE_PORT_MIN);
        return Some(linux_unbridged_line(port));
    }
    let unsaid = choice == policy::AgentNetwork::None && sandbox.bridged_ports().is_none();
    (reaches && unsaid).then(|| LINUX_PORTS_ANY_HOST_LINE.to_owned())
}

/// [`linux_unbridged_notice_on`] for this computer as it was last read, under the owner's
/// call supervisor setting ([`LinuxHost::under`]). Inputs: the project's network choice,
/// the agent's sandbox as launched and the setting, none when it cannot be read, which
/// counts as no supervisor. Output: the sentences, or none.
pub fn linux_unbridged_notice(
    choice: policy::AgentNetwork,
    sandbox: &ProjectSandbox,
    calls: Option<LinuxSupervisor>,
) -> Option<String> {
    let host = linux_host_kept().map(|host| host.under(calls));
    linux_unbridged_notice_on(host, choice, sandbox)
}

/// What a launch tells the owner in the agent's messages when this Linux nests agents but
/// could not give this one the network of its own its ports allow (DEC-179 SBX-2): why it
/// has none, the AppArmor profile and root included (rv-sa241 NOTE-2, NOTE-3), and what
/// that leaves open.
pub const LINUX_UNROUTED_LINE: &str = "On this Linux, this agent has no network of its own: slirp4netns or the nf_tables iptables is missing here, did not start or is kept out by this computer's AppArmor profile, or CrazyAgent runs as root, so every port it may reach is open to it on every host, this computer included.";

/// [`LINUX_UNROUTED_LINE`] for an agent with no loopback port, or one the call supervisor
/// keeps to this computer (rv-sa241 7b LOW-1 and N1): its ports on any host then reach
/// this computer too.
pub const LINUX_UNROUTED_ANY_HOST_LINE: &str = "On this Linux, this agent has no network of its own: slirp4netns or the nf_tables iptables is missing here, did not start or is kept out by this computer's AppArmor profile, or CrazyAgent runs as root, so the ports it may reach on any host include this computer's own.";

/// What a limited network cannot hold on Linux, shown there only, under the network choice
/// in the desktop and the TUI (RV-DS241-83): Landlock limits outbound TCP by port, not by
/// host, so a port an agent may reach on this computer is open to it on every host.
pub const LINUX_PORTS_ANY_HOST_LINE: &str = "On Linux, network limits hold ports, not hosts: a port agents may reach on this computer, such as the model's proxy port or a port you allow, is open to them on every host.";

/// What a writable project folder cannot hold on a Linux where no call supervisor runs,
/// among the engine's lines under the access level in the desktop and the TUI (RV-DS241-85,
/// DEC-179 T4c): no Landlock rule stops an agent from making a git repository inside a
/// folder it may change.
pub const LINUX_NESTED_GIT_LINE: &str = "On Linux, agents can make a git repository inside any project folder they may change, and your own git or IDE runs its settings when you open that folder. Check agent-made folders for a .git before you open them.";

/// What a Linux agent can still do to the owner's other programs where this machine gives
/// it neither a process namespace of its own nor the Landlock signal scope (Linux before
/// 6.12 without usable user namespaces), shown there only, under the access level in the
/// desktop and the TUI (SA vid 4, DEC-089).
pub const LINUX_SIGNALS_LINE: &str = "On this Linux, agents can see which programs you run and stop or signal them, such as a dev server or CrazyAgent itself. Linux 6.12 or newer stops the signals.";

/// What a Linux agent can still see where the kernel scopes its signals but this machine
/// gives it no process namespace of its own, shown there only, under the access level in
/// the desktop and the TUI (RV-DS241-86).
pub const LINUX_PROCESS_LIST_LINE: &str =
    "On this Linux, agents can see which programs you run and how they were started.";

/// What a writable project folder still cannot hold where the call supervisor runs: it
/// refuses every new `.git` entry but the agent's own, yet an agent may change a git
/// repository already in the project. Shown under the access level when the project may
/// hold one ([`linux_other_git`]) and in the New Project catalog (DEC-179 T4c).
pub const LINUX_KEPT_GIT_LINE: &str = "On Linux, agents can change a git repository already inside a project folder they may change, and your own git or IDE runs its settings when you open that folder. Check folders with a .git inside the project before you open them.";

/// What the call supervisor changes for agents where it runs (DEC-179 T4c): it makes the
/// calls it guards for them, so a program's own Landlock limits do not hold for those
/// (QUESTION-16:21), and it refuses a create, even of a file already there, under a `.git`
/// the agent did not make (rv-sec3 N3).
pub const LINUX_SUPERVISED_LINE: &str = "On this Linux, CrazyAgent makes some calls for agents, such as making a file or folder or connecting, so a program an agent starts can't narrow those with limits of its own. In a .git folder CrazyAgent didn't make, agents can't create files and some writes to files already there are refused, so git commands that write fail in a repository nested in the project.";

/// Why no call supervisor runs where CrazyAgent runs as root (rv-sec3 N1): dumpability does
/// not keep a root agent away from it, so none starts and the lines after this one hold.
pub const LINUX_ROOT_LINE: &str = "CrazyAgent runs as root on this Linux, so it starts no call supervisor and agents can do what the lines below say. Run CrazyAgent as your own user to turn it on.";

/// What a limited network that reaches websites still cannot hold where the call supervisor
/// keeps ports on this computer to it (DEC-179 T4c): agents look up names over TCP, so port
/// 53 is open to them on every host.
pub const LINUX_DNS_ANY_HOST_LINE: &str =
    "On this Linux, agents can also reach port 53, used to look up names, on every host.";

/// Why agents share the owner's process space where AppArmor keeps unprivileged programs
/// out of user namespaces (Ubuntu 23.10 and newer) and this machine nests none, shown before
/// the process or signals line it explains, with the deb package that adds the profile
/// allowing it (DEC-179 SBX-3, addendum 7).
pub const LINUX_APPARMOR_LINE: &str = "On this Linux, AppArmor keeps agents from getting a process space of their own. CrazyAgent's .deb package adds the AppArmor profile that allows it: install it, or if it is installed, run sudo dpkg-reconfigure crazy-agent.";

/// What this Linux holds for an agent beyond its Landlock file and network rules: `nested`
/// when it starts agents in a process namespace of their own, `scoped` when the kernel
/// keeps their signals inside their own Landlock domain (Linux 6.12 or newer),
/// `supervised` when the call supervisor holds every guard here, as launches start it,
/// `root` when CrazyAgent runs as root, where it starts no supervisor (rv-sec3 N1),
/// `armored` when AppArmor's user namespace restriction is why it nests none (SBX-3), and
/// `routed` when it gives an agent limited to ports on any host a network of its own (SBX-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxHost {
    pub nested: bool,
    pub scoped: bool,
    pub supervised: bool,
    pub root: bool,
    pub armored: bool,
    pub routed: bool,
}

/// The network a Linux that nests agents gives one limited to ports on any host
/// ([`ProjectSandbox::routed`]): `any`, the ports it leaves through on any host, and
/// `bridged`, the loopback ports carried to this computer as for a bridged agent, each list
/// rising and each port once (DEC-179 SBX-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub any: Vec<u16>,
    pub bridged: Vec<u16>,
}

impl LinuxHost {
    /// What this Linux holds under the owner's Linux call supervisor setting: supervised
    /// only while it is On, and run as root only while it is not Off, so the root line says
    /// why no supervisor runs. Input: the setting, none when it cannot be read, which shows
    /// every line. Output: the facts the access lines read; what is kept is never changed.
    pub fn under(self, calls: Option<LinuxSupervisor>) -> Self {
        assert!(!(self.root && self.supervised));
        let held = Self {
            supervised: self.supervised && calls == Some(LinuxSupervisor::On),
            root: self.root && calls != Some(LinuxSupervisor::Off),
            ..self
        };
        assert!(self.supervised || !held.supervised);
        assert!(!(held.root && held.supervised));
        held
    }
}

/// What this computer holds as [`LinuxHost`] says, read through the sandbox helper beside
/// this executable, again once what is kept is a minute old. No input. Output: none on
/// another system; on Linux the facts, each false when it cannot be read, so its gap line
/// is shown.
pub fn linux_host() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host_now())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What this computer holds as [`linux_host`] last read it, however long ago, so a caller
/// inside a database transaction starts no helper; it reads only when nothing was read
/// yet. No input. Output: as [`linux_host`].
pub fn linux_host_kept() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host_kept())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Tells whether a project may hold a git repository besides its own top-level `.git`,
/// which an agent may still change under the call supervisor: on Linux what a bounded read
/// of the folder finds; elsewhere yes, where no Linux line shows. Input: the project
/// folder. Output: true when one may be there.
pub fn linux_other_git(project: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::holds_other_git(project)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = project;
        true
    }
}

/// Refusal to build or install a project sandbox, with a plain reason.
#[derive(Debug)]
pub enum SandboxError {
    Invalid(String),
    Unavailable(String),
    Io(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "sandbox refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "sandbox unavailable: {reason}"),
            Self::Io(error) => write!(f, "sandbox I/O: {error}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<io::Error> for SandboxError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// The Seatbelt boundary for one agent of one project: writes only inside the
/// project folder (never its `context/` or git hooks and config), the agent's
/// worktrees and its own home and temp dirs, never the launch files, instructions,
/// skills or settings its harness loads from that home, reads of system paths plus
/// those roots, and network per [`SandboxNetwork`].
#[derive(Debug, Clone)]
pub struct ProjectSandbox {
    project_root: PathBuf,
    agent_home: PathBuf,
    agent_tmp: PathBuf,
    worktrees: Vec<PathBuf>,
    read_roots: Vec<PathBuf>,
    tool_roots: Vec<PathBuf>,
    tool_files: Vec<PathBuf>,
    execs: Vec<PathBuf>,
    scratch: Option<PathBuf>,
    hidden: Vec<PathBuf>,
    kept_out: Vec<PathBuf>,
    revealed: Vec<PathBuf>,
    links: Vec<PathBuf>,
    network: SandboxNetwork,
    linux_sockets: LinuxSockets,
    kept_ports: Vec<u16>,
    owner_home: PathBuf,
    engine_secrets: Vec<PathBuf>,
    carved_home: Option<PathBuf>,
    checkout: Option<Checkout>,
    clone: Option<PathBuf>,
    sockets: Vec<PathBuf>,
    served: Vec<PathBuf>,
    reached: Vec<PathBuf>,
    shut: Vec<PathBuf>,
    read_only: Vec<PathBuf>,
    fixed: Vec<PathBuf>,
    shared: Vec<PathBuf>,
    teammates: bool,
    answers: Option<PathBuf>,
    setup: Option<TeammatesSetup>,
    tool_cache: Option<(PathBuf, bool)>,
    lead_branch: Option<String>,
    adds_worktrees: bool,
    agent_files: Option<policy::AgentFilesRule>,
    granted: Vec<PathBuf>,
    owner_tmp: Option<PathBuf>,
    project_secrets: Option<PathBuf>,
    secrets_revealed: Vec<PathBuf>,
    shared_writes: Vec<PathBuf>,
    open_tops: Option<Vec<String>>,
    node_bin: Option<PathBuf>,
    owner_config: bool,
    job_folder: bool,
    #[cfg(unix)]
    engine_home: Option<bool>,
}

/// What the engine's git may write, with a linked lead's rights, while it adds the lead's
/// native teammates' linked worktrees (OP-9b): the teammates' folder inside the lead's
/// worktree and the project repository's worktree records.
#[derive(Debug, Clone)]
struct TeammatesSetup {
    folder: PathBuf,
    records: PathBuf,
}

/// The one git worktree a confined agent writes in place of the whole project folder,
/// with the two parts of the project repository its commits need: the worktree's own
/// admin folder under `.git/worktrees` and the shared object store.
#[derive(Debug, Clone)]
struct Checkout {
    worktree: PathBuf,
    admin: PathBuf,
    objects: PathBuf,
}

impl ProjectSandbox {
    /// Validates canonical, existing, quote-free directories that do not contain the
    /// owner's home, do not sit inside `~/.codex` or `~/.claude`, and neither hold nor sit
    /// inside an owner or system secret path. Output: the sandbox, or `Invalid`.
    pub fn new(
        project_root: &Path,
        agent_home: &Path,
        agent_tmp: &Path,
        network: SandboxNetwork,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets: Vec<PathBuf> = owner_secrets(&owner).collect();
        for root in [project_root, agent_home, agent_tmp] {
            granted_root(root, &owner, &secrets)?;
        }
        if let SandboxNetwork::Only(remotes) = &network
            && (remotes.len() > REMOTES_MAX
                || remotes.iter().any(|remote| match remote {
                    SandboxRemote::Loopback(port) | SandboxRemote::AnyHost(port) => *port == 0,
                }))
        {
            return Err(invalid("network remotes must be 1..8 nonzero ports"));
        }
        Ok(Self {
            project_root: project_root.to_path_buf(),
            agent_home: agent_home.to_path_buf(),
            agent_tmp: agent_tmp.to_path_buf(),
            worktrees: Vec::new(),
            read_roots: Vec::new(),
            tool_roots: Vec::new(),
            tool_files: Vec::new(),
            execs: Vec::new(),
            scratch: None,
            hidden: Vec::new(),
            kept_out: Vec::new(),
            revealed: Vec::new(),
            links: Vec::new(),
            network,
            linux_sockets: LinuxSockets::default(),
            kept_ports: Vec::new(),
            owner_home: owner,
            engine_secrets: Vec::new(),
            carved_home: None,
            checkout: None,
            clone: None,
            sockets: Vec::new(),
            served: Vec::new(),
            reached: Vec::new(),
            shut: Vec::new(),
            read_only: Vec::new(),
            fixed: Vec::new(),
            shared: Vec::new(),
            teammates: false,
            answers: None,
            setup: None,
            tool_cache: None,
            lead_branch: None,
            adds_worktrees: false,
            agent_files: None,
            granted: Vec::new(),
            owner_tmp: None,
            project_secrets: None,
            secrets_revealed: Vec::new(),
            shared_writes: Vec::new(),
            open_tops: None,
            node_bin: None,
            owner_config: false,
            job_folder: false,
            #[cfg(unix)]
            engine_home: None,
        })
    }

    /// Adds the narrow extras the owner allowed the project's agents (F08): each socket
    /// grant as a connect-and-read grant of its exact path in a list of its own, never bind,
    /// write or unlink; each port grant as an outbound loopback port when the network is
    /// limited (an open network reaches it already); and the temp folder grant as the
    /// owner's private temp folder, written as a root and handed over as `TMPDIR`, with every
    /// socket node in it kept unwritable. Inputs: the saved grants and the owner's temp
    /// folder as [`grants::owner_temp_folder`] gives it, none when it cannot be given.
    /// Output: the widened sandbox, or `Invalid`; sockets and the temp folder need Seatbelt.
    pub fn grant_access(
        mut self,
        grants: &[policy::AccessGrant],
        owner_tmp: Option<&Path>,
    ) -> Result<Self, SandboxError> {
        if grants.len() > policy::GRANTS_MAX {
            return Err(invalid("too many access grants"));
        }
        for grant in grants {
            if !grant.plain() {
                return Err(invalid("an access grant is not plain"));
            }
            match grant {
                policy::AccessGrant::Socket { path } => {
                    if !cfg!(target_os = "macos") {
                        return Err(invalid("unix socket grants need Seatbelt"));
                    }
                    let folder = path.parent().filter(|folder| {
                        fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir())
                    });
                    let known = self
                        .sockets
                        .iter()
                        .chain(&self.served)
                        .chain(&self.reached)
                        .chain(&self.shut)
                        .chain(&self.granted)
                        .any(|socket| socket == path);
                    if folder.is_none() || known || self.granted.len() >= policy::GRANT_SOCKETS_MAX
                    {
                        return Err(invalid("a socket grant cannot be held"));
                    }
                    self.granted.push(path.clone());
                }
                policy::AccessGrant::Port { port } => {
                    if let SandboxNetwork::Only(remotes) = &mut self.network {
                        let remote = SandboxRemote::Loopback(*port);
                        if !remotes.contains(&remote) {
                            remotes.push(remote);
                        }
                        if remotes.len() > REMOTES_MAX {
                            return Err(invalid("network remotes must be 1..8 nonzero ports"));
                        }
                    }
                }
                policy::AccessGrant::TempFolder => {
                    if !cfg!(target_os = "macos") {
                        return Err(invalid("the temp folder grant needs Seatbelt"));
                    }
                    let tmp = owner_tmp
                        .ok_or_else(|| invalid("the owner's temp folder cannot be given"))?;
                    let owner = owner_home()?;
                    let secrets: Vec<PathBuf> = owner_secrets(&owner).collect();
                    granted_root(tmp, &owner, &secrets)?;
                    clear_of_secrets(tmp, &self.engine_secrets)?;
                    self.owner_tmp = Some(tmp.to_path_buf());
                }
            }
        }
        assert!(self.granted.len() <= policy::GRANT_SOCKETS_MAX);
        Ok(self)
    }

    /// The owner's temp folder this sandbox hands over as `TMPDIR` under a temp folder
    /// grant, or none. Takes the sandbox. Returns the folder.
    pub fn owner_tmp(&self) -> Option<&Path> {
        self.owner_tmp.as_deref()
    }

    /// The exact sockets the owner's grants let this agent connect to. Takes the sandbox.
    /// Returns the paths.
    pub fn granted_sockets(&self) -> &[PathBuf] {
        &self.granted
    }

    /// Lets the agent bind and connect one Unix socket at an exact path, read and write that
    /// file and its `<path>.lock`, and read its folder's metadata, nothing else there: its
    /// own Codex app-server socket in the shared daemon folder (DEC-104 K2b). Input: a plain
    /// absolute path of letters, digits, `/`, `.`, `-` and `_` in an existing folder.
    /// Output: the widened sandbox, or `Invalid`; only Seatbelt can hold the grant.
    pub fn allow_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        if self.sockets.len() >= SOCKETS_MAX {
            return Err(invalid("too many unix sockets"));
        }
        self.sockets.push(socket.to_path_buf());
        assert!(self.sockets.len() <= SOCKETS_MAX && self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent serve one Unix socket at an exact path but never connect to it or
    /// unlink it: bind, create and change that file and its `<path>.lock`, and read its
    /// folder's metadata. A native lead's teammates run their tmux server this way, so none
    /// of them reaches it or swaps the node for a listener of its own (rv-audit S1 F1).
    /// Input: as [`Self::allow_unix_socket`]. Output: the widened sandbox, or `Invalid`.
    pub fn serve_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.served.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent connect to one Unix socket at an exact path and read it, never bind
    /// it or create, change, link or unlink a node there, such as a native lead reaching its
    /// teammates' tmux server and its teammates reaching the engine's caller socket (rv-audit
    /// S1 F1, TTDL F1). Input: as [`Self::allow_unix_socket`]. Output: the widened sandbox,
    /// or `Invalid`.
    pub fn reach_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.reached.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Keeps the agent from writing, unlinking or locking one Unix socket at an exact path
    /// inside its writable folders, with no grant to bind or reach it, such as a native
    /// lead's own tmux socket for its teammates (rv-audit S1 F1). Input: as
    /// [`Self::allow_unix_socket`]. Output: the narrowed sandbox, or `Invalid`.
    pub fn shut_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.shut.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// The Unix socket grants of every use this sandbox holds. Takes the sandbox. Returns
    /// the count.
    fn socket_grants(&self) -> usize {
        self.sockets.len() + self.served.len() + self.reached.len() + self.shut.len()
    }

    /// Checks that one more Unix socket grant fits and names a plain path no grant of this
    /// sandbox names yet: absolute, at most `SOCKET_PATH_BYTES_MAX`, of letters, digits,
    /// `/`, `.`, `-` and `_`, in an existing folder. Takes the socket. Returns Ok, or
    /// `Invalid`; off macOS no Unix socket grant exists.
    fn socket_room(&self, socket: &Path) -> Result<(), SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(invalid("unix socket grants need Seatbelt"));
        }
        if self.socket_grants() >= SOCKET_GRANTS_MAX {
            return Err(invalid("too many unix sockets"));
        }
        let granted = self
            .sockets
            .iter()
            .chain(&self.served)
            .chain(&self.reached)
            .chain(&self.shut)
            .any(|known| known == socket);
        if granted {
            return Err(invalid("the unix socket already has a grant"));
        }
        let text = socket
            .to_str()
            .ok_or_else(|| invalid("the unix socket path is not plain"))?;
        let plain = socket.is_absolute()
            && text.len() <= SOCKET_PATH_BYTES_MAX
            && !text.ends_with('/')
            && !text.contains("//")
            && text.split('/').all(|part| part != "." && part != "..")
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_'));
        let folder = socket
            .parent()
            .filter(|folder| std::fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir()));
        if !plain || folder.is_none() {
            return Err(invalid("the unix socket path is not plain"));
        }
        Ok(())
    }

    /// Keeps the ports of the owner's configured local model endpoints free of this agent's
    /// listeners, beside [`OWNER_PORTS`], when its network is open. Input: the ports, as
    /// [`owner_ports`] lists them. Output: the sandbox, or a refusal for port 0 or more than
    /// `KEPT_PORTS_MAX` ports.
    pub fn keep_ports(mut self, ports: &[u16]) -> Result<Self, SandboxError> {
        if ports.contains(&0) {
            return Err(invalid("kept ports must be nonzero"));
        }
        self.kept_ports.extend(
            ports
                .iter()
                .copied()
                .filter(|port| !OWNER_PORTS.contains(port)),
        );
        self.kept_ports.sort_unstable();
        self.kept_ports.dedup();
        if self.kept_ports.len() > KEPT_PORTS_MAX {
            return Err(invalid("at most 64 kept ports"));
        }
        Ok(self)
    }

    /// Names the engine runtime folder whose secrets no granted root may hold or sit inside:
    /// engine.db, the owner socket, the GitHub files, the account and plugin keys, the upgrade
    /// backups, the native profiles, the configured SDK homes, the terminal and IDE folders and
    /// every engine socket and socket folder in it now; the agent's home and temp may sit in it.
    /// Input: the existing runtime folder. Output: the guarded sandbox, or a refusal.
    pub fn guard_engine(mut self, runtime: &Path) -> Result<Self, SandboxError> {
        let runtime = canonical(runtime)?;
        if !clean_absolute(&runtime) || !runtime.is_dir() {
            return Err(invalid("engine runtime must be an existing plain folder"));
        }
        let secrets = engine_secrets_in(&runtime)?;
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            clear_of_secrets(root, &secrets)?;
        }
        self.engine_secrets = secrets;
        Ok(self)
    }

    /// Adds the engine root's secrets for an agent whose runtime folder is one folder in it,
    /// as an owner generation is for a master: every entry guard_engine would name for the
    /// engine root except that folder itself, where the agent's home lives. An rcli master's
    /// home may instead be one configured SDK home directly in the engine root's folder of
    /// them: that folder stays a secret and only this home is carved back out of it, so every
    /// other home, even one made later, stays denied. Call it after guard_engine on the
    /// runtime folder, whose secrets stay. Inputs: the engine root and the runtime folder.
    /// Output: the guarded sandbox, or a refusal.
    pub fn guard_engine_around(
        mut self,
        engine: &Path,
        runtime: &Path,
    ) -> Result<Self, SandboxError> {
        let engine = canonical(engine)?;
        let runtime = canonical(runtime)?;
        if !clean_absolute(&engine)
            || !engine.is_dir()
            || runtime.parent() != Some(engine.as_path())
        {
            return Err(invalid(
                "engine root must be the existing folder around the runtime",
            ));
        }
        let homes = engine.join(crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER);
        let carved =
            (self.agent_home.parent() == Some(homes.as_path())).then(|| self.agent_home.clone());
        let secrets: Vec<PathBuf> = engine_secrets_in(&engine)?
            .into_iter()
            .filter(|secret| *secret != runtime)
            .collect();
        let around_home: Vec<PathBuf> = secrets
            .iter()
            .filter(|secret| **secret != homes)
            .cloned()
            .collect();
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            if carved.as_ref() == Some(root) {
                clear_of_secrets(root, &around_home)?;
            } else {
                clear_of_secrets(root, &secrets)?;
            }
        }
        for secret in secrets {
            if !self.engine_secrets.contains(&secret) {
                self.engine_secrets.push(secret);
            }
        }
        assert!(!self.engine_secrets.contains(&runtime));
        if let Some(home) = carved {
            assert!(self.engine_secrets.contains(&homes));
            assert!(
                self.secret_paths()
                    .iter()
                    .filter(|secret| **secret != homes)
                    .all(|secret| !home.starts_with(secret) && !secret.starts_with(&home))
            );
            self.carved_home = Some(home);
        }
        Ok(self)
    }

    /// Every path this sandbox denies to the agent although an allowed root might cover
    /// it: the owner's Keychains, SSH, GPG, cloud, gh, git, Codex and Claude credentials,
    /// the system Keychains and, once guarded, the engine runtime's secrets.
    pub fn secret_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = owner_secrets(&self.owner_home).collect();
        paths.extend(self.engine_secrets.iter().cloned());
        paths
    }

    /// Adds one writable worktree the agent may also start in, for a checkout the
    /// engine keeps outside the project folder. Input: a canonical existing folder that
    /// neither holds nor sits inside a path from [`Self::secret_paths`].
    /// Output: the widened sandbox, or a refusal for an unsafe or extra root.
    pub fn allow_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.worktrees.len() >= WORKTREES_MAX {
            return Err(invalid("too many worktrees"));
        }
        granted_root(worktree, &owner_home()?, &self.secret_paths())?;
        self.worktrees.push(worktree.to_path_buf());
        assert!(self.worktrees.len() <= WORKTREES_MAX);
        Ok(self)
    }

    /// Lets git create a checkout's `.git` link in the project's worktrees folders and the
    /// `gitdir` and `commondir` files of its admin folder, which no other profile may plant
    /// (SECAUDIT-WORKTREE-ADMIN-PLANT). Only the engine's own worktree-add step installs it,
    /// and its callers confine and reinstall the profile before any agent runs.
    /// Output: the sandbox with that exemption.
    #[cfg(unix)]
    pub(crate) fn adding_worktrees(mut self) -> Self {
        self.adds_worktrees = true;
        self
    }

    /// Confines the agent's writes to its own worktree, its `.git/worktrees` admin folder
    /// except its HEAD, which only the engine moves, and the object store, beside its home,
    /// temp dir and extra worktrees; the project stays readable. Makes the admin folder's
    /// `modules` store so every platform guards a real folder there. Input: a canonical
    /// worktree in `worktrees/` whose `.git` link and admin folder name each other.
    /// Output: the confined sandbox, `Invalid` or the disk error.
    pub fn confine_to_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = crate::projects_home::state_folder(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let git = self.project_root.join(".git");
        let admin = canonical(&worktree.join(git_link(&worktree.join(".git"), "gitdir: ")?))?;
        if !clean_absolute(&admin) || admin.parent() != Some(git.join("worktrees").as_path()) {
            return Err(invalid(
                "the worktree's git folder must sit in .git/worktrees",
            ));
        }
        let back = canonical(&admin.join(git_link(&admin.join("gitdir"), "")?))?;
        if back != worktree.join(".git") {
            return Err(invalid("the worktree's git folder names another worktree"));
        }
        let objects = git.join("objects");
        if !objects.is_dir() || canonical(&objects)? != objects {
            return Err(invalid("the project's git objects must be a plain folder"));
        }
        let secrets = self.secret_paths();
        for root in [worktree, admin.as_path(), objects.as_path()] {
            clear_of_secrets(root, &secrets)?;
        }
        assert!(
            [worktree, admin.as_path(), objects.as_path()]
                .iter()
                .all(|root| root.starts_with(&self.project_root) && *root != self.project_root)
        );
        ensure_store(&admin.join("modules")).map_err(|problem| match problem.kind() {
            io::ErrorKind::InvalidData => {
                invalid("the worktree's submodule store must be a plain folder")
            }
            _ => SandboxError::Io(problem),
        })?;
        self.read_roots.push(self.project_root.clone());
        self.checkout = Some(Checkout {
            worktree: worktree.to_path_buf(),
            admin,
            objects,
        });
        Ok(self)
    }

    /// Confines an Own branch or Full access agent (DEC-102) to its own clone of the project:
    /// the clone, with its git folder inside it, is the agent's only writable project path,
    /// so it commits on its own branch with full git while the project folder, its `.git`
    /// and the object store the clone borrows through `objects/info/alternates` stay
    /// read-only. Input: a canonical folder in the project's `worktrees/` folder whose
    /// `.git` is a real folder, not a link. Output: the confined sandbox, or `Invalid`.
    pub fn confine_to_clone(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = crate::projects_home::state_folder(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let git = worktree.join(".git");
        let metadata = fs::symlink_metadata(&git)?;
        if !metadata.is_dir() || canonical(&git)? != git {
            return Err(invalid("the clone's git folder must be a real folder"));
        }
        clear_of_secrets(worktree, &self.secret_paths())?;
        assert!(worktree.starts_with(&self.project_root) && worktree != self.project_root);
        self.read_roots.push(self.project_root.clone());
        self.clone = Some(worktree.to_path_buf());
        Ok(self)
    }

    /// Names the branch a clone lead works on, as the engine records it, so its native
    /// teammates write only the refs of its `<branch>--<name>` teammate branches in the
    /// clone, never the lead's own (AGENT-REF-ALLOW-NARROW). Input: an agent branch, as
    /// [`crate::agent_git::is_agent_branch`] says. Output: the sandbox, or `Invalid` before
    /// [`Self::confine_to_clone`], a second time or for any other branch.
    pub fn lead_branch(mut self, branch: &str) -> Result<Self, SandboxError> {
        if self.clone.is_none() || self.lead_branch.is_some() {
            return Err(invalid(
                "only a clone lead's sandbox names its branch, once",
            ));
        }
        if !crate::agent_git::is_agent_branch(branch) {
            return Err(invalid("the lead's branch must be an agent branch"));
        }
        self.lead_branch = Some(branch.to_owned());
        Ok(self)
    }

    /// Confines the engine's own git, run with a linked lead's rights, to adding the lead's
    /// native teammates' linked worktrees (OP-9b): it writes only the teammates' folder of
    /// the lead's worktree, the project repository's `.git/worktrees` records and the refs
    /// and reflogs of the lead's own `<branch>--<name>` teammate branches with their
    /// `crazyagent` folders (AGENT-REF-ALLOW-NARROW), beside the lead's home and temp dir;
    /// the project stays readable. Makes the teammates' folder as a plain folder. Inputs:
    /// the lead's canonical linked worktree in `worktrees/`, whose `.git` link names a
    /// record in `.git/worktrees`, and its agent branch as the engine records it. Output:
    /// the confined sandbox, `Invalid` or the disk error.
    pub fn confine_to_teammates_setup(
        mut self,
        worktree: &Path,
        branch: &str,
    ) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if !crate::agent_git::is_agent_branch(branch) {
            return Err(invalid("the lead's branch must be an agent branch"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = crate::projects_home::state_folder(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let records = self.project_root.join(".git").join("worktrees");
        let admin = canonical(&worktree.join(git_link(&worktree.join(".git"), "gitdir: ")?))?;
        if admin.parent() != Some(records.as_path()) || canonical(&records)? != records {
            return Err(invalid(
                "the lead's worktree must be a linked worktree of the project",
            ));
        }
        let folder = worktree.join(crate::agent_git::TEAMMATES_FOLDER);
        ensure_store(&folder).map_err(|problem| match problem.kind() {
            io::ErrorKind::InvalidData => invalid("the teammates' folder must be a plain folder"),
            _ => SandboxError::Io(problem),
        })?;
        if canonical(&folder)? != folder {
            return Err(invalid("the teammates' folder must be a plain folder"));
        }
        let secrets = self.secret_paths();
        for root in [folder.as_path(), records.as_path()] {
            clear_of_secrets(root, &secrets)?;
        }
        assert!(
            [folder.as_path(), records.as_path()]
                .iter()
                .all(|root| root.starts_with(&self.project_root) && *root != self.project_root)
        );
        self.read_roots.push(self.project_root.clone());
        self.setup = Some(TeammatesSetup { folder, records });
        self.lead_branch = Some(branch.to_owned());
        Ok(self)
    }

    /// Adds one extra read-only root, such as a harness install directory. Input: a
    /// canonical existing folder that neither holds nor sits inside a path from
    /// [`Self::secret_paths`]. Output: the widened sandbox, or a refusal.
    pub fn allow_read(mut self, root: &Path) -> Result<Self, SandboxError> {
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        granted_root(root, &owner_home()?, &self.secret_paths())?;
        self.read_roots.push(root.to_path_buf());
        Ok(self)
    }

    /// Lets a Node script harness find the `node` beside it, as its sign-in does: under an npm
    /// prefix ([`crate::harnesses::node_prefix`]) only `bin`, the harness's package folder and
    /// the real file `bin/node` links to become readable, Seatbelt denying credential names
    /// below each folder, and `bin` goes first on PATH; a prefix or node in the project or
    /// agent-writable is skipped. Input: the canonical program. Output: the sandbox, or a refusal.
    pub fn allow_node_for(mut self, program: &Path) -> Result<Self, SandboxError> {
        let Some(prefix) = crate::harnesses::node_prefix(program) else {
            return Ok(self);
        };
        let Some(package) = node_package(&prefix, program) else {
            return Ok(self);
        };
        let folders = [canonical(&prefix.join("bin"))?, canonical(&package)?];
        let node = canonical(&prefix.join("bin").join("node"))?;
        let agents = folders.iter().chain([&node]).any(|path| {
            crate::harnesses::in_crazyagent_folder(path)
                || path.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| path.starts_with(root) || root.starts_with(path))
        });
        if agents {
            return Ok(self);
        }
        for folder in folders {
            if !self.reads(&folder) {
                self = self.allow_node_folder(&folder)?;
            }
        }
        if !self.reads(&node) {
            self = self.allow_node_file(&node)?;
        }
        self.node_bin = Some(prefix.join("bin"));
        assert!(self.node_bin.as_deref().is_some_and(Path::is_absolute));
        Ok(self)
    }

    /// Lets a script harness's interpreters run as the tool resolver found them
    /// ([`crate::tools::Resolved::grants`]), such as the `node` an npm-built Codex asks `env`
    /// for from nvm, volta or Homebrew: each interpreter's folder becomes readable as a
    /// toolchain folder unless a system or granted folder covers it. Input: the resolved
    /// harness. Output: the sandbox, or a refusal for an interpreter in the project or in a
    /// folder the agent can write.
    pub fn allow_interpreters(
        mut self,
        resolved: &crate::tools::Resolved,
    ) -> Result<Self, SandboxError> {
        for file in resolved.grants().into_iter().skip(1) {
            let folder = file
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| invalid("an interpreter must sit in a folder"))?;
            let agents = crate::harnesses::in_crazyagent_folder(&folder)
                || folder.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| folder.starts_with(root) || root.starts_with(&folder));
            if agents {
                return Err(invalid("an interpreter sits where the agent can write"));
            }
            let covered = SYSTEM_READ_ROOTS
                .iter()
                .map(Path::new)
                .chain(self.read_roots.iter().map(PathBuf::as_path))
                .chain(self.tool_roots.iter().map(PathBuf::as_path))
                .any(|root| folder.starts_with(root));
            if !covered {
                self = self.allow_node_folder(&folder)?;
            }
        }
        Ok(self)
    }

    /// Whether a system, granted or toolchain folder already makes a path readable. Input: an
    /// absolute path. Output: the answer.
    fn reads(&self, path: &Path) -> bool {
        SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.read_roots.iter().map(PathBuf::as_path))
            .chain(self.tool_roots.iter().map(PathBuf::as_path))
            .any(|root| path.starts_with(root))
    }

    /// Grants the real `node` an npm prefix links to, such as the engine's own Node in its
    /// setup folder, read-only and runnable: under Seatbelt as one toolchain file, elsewhere
    /// its folder as a read root. Input: the canonical existing file. Output: the widened
    /// sandbox, or a refusal when it fails the toolchain file or read root checks.
    fn allow_node_file(mut self, node: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            let folder = node
                .parent()
                .ok_or_else(|| invalid("node must sit in a folder"))?;
            return self.allow_read(folder);
        }
        if self.tool_files.iter().any(|file| file == node) {
            return Ok(self);
        }
        if self.tool_files.len() >= TOOL_FILES_MAX {
            return Err(invalid("too many toolchain files"));
        }
        clear_of_secrets(node, &self.secret_paths())?;
        if !clean_absolute(node) || !node.is_file() || canonical(node)? != *node {
            return Err(invalid("toolchain files must be existing canonical files"));
        }
        self.tool_files.push(node.to_path_buf());
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants one folder of a Node script harness read-only: under Seatbelt as a toolchain
    /// folder, so every credential name below it stays denied, elsewhere as a read root.
    /// Input: a canonical existing folder. Output: the widened sandbox, or a refusal when the
    /// folder fails [`Self::allow_read`]'s checks or the toolchain folders are full.
    fn allow_node_folder(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return self.allow_read(folder);
        }
        if self.tool_roots.len() >= TOOL_ROOTS_MAX {
            return Err(invalid("too many toolchain folders"));
        }
        granted_root(folder, &owner_home()?, &self.secret_paths())?;
        self.tool_roots.push(folder.to_path_buf());
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok(self)
    }

    /// Lets the agent follow one folder link that leads to a read root it already has, such
    /// as the version link uv keeps beside a managed Python: only the link's own text becomes
    /// readable, and no deny gives way to it, since the profile writes it before every deny.
    /// Input: a clean absolute link in a real folder, outside the hidden, kept-out and
    /// [`Self::secret_paths`], leading to a read root. Output: the sandbox, or `Invalid`.
    pub fn allow_read_link(mut self, link: &Path) -> Result<Self, SandboxError> {
        if self.links.len() >= LINKS_MAX {
            return Err(invalid("too many links to read roots"));
        }
        clear_of_secrets(link, &self.secret_paths())?;
        let is_link =
            std::fs::symlink_metadata(link).is_ok_and(|metadata| metadata.file_type().is_symlink());
        let in_real_folder = link
            .parent()
            .is_some_and(|parent| canonical(parent).is_ok_and(|real| real == parent));
        if !clean_absolute(link) || !is_link || !in_real_folder {
            return Err(invalid(
                "a link to a read root must be a clean absolute link in a real folder",
            ));
        }
        let denied = self
            .hidden
            .iter()
            .chain(self.kept_out.iter())
            .any(|path| link.starts_with(path));
        if denied {
            return Err(invalid(
                "a link to a read root must lie outside every hidden and kept-out path",
            ));
        }
        let target = canonical(link)?;
        assert!(target.is_absolute());
        if !self.read_roots.contains(&target) {
            return Err(invalid("a link must lead to a read root already granted"));
        }
        self.links.push(link.to_path_buf());
        assert!(self.links.len() <= LINKS_MAX);
        Ok(self)
    }

    /// Lets the agent start one exact program the engine itself launches, such as its
    /// harness or the engine executable, from an app bundle's `Contents/MacOS`, which no
    /// agent may start otherwise (OP-24). One inside a folder the agent can write stays
    /// denied, since the agent could replace it. Input: a canonical existing regular file
    /// clear of [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid`.
    pub fn allow_exec(mut self, program: &Path) -> Result<Self, SandboxError> {
        clear_of_secrets(program, &self.secret_paths())?;
        if !clean_absolute(program) || !program.is_file() || canonical(program)? != *program {
            return Err(invalid(
                "an allowed program must be an existing canonical file",
            ));
        }
        if self.execs.iter().any(|exec| exec == program) {
            return Ok(self);
        }
        if self.execs.len() >= EXECS_MAX {
            return Err(invalid("too many allowed programs"));
        }
        self.execs.push(program.to_path_buf());
        Ok(self)
    }

    /// Lets the agent run the engine executable that serves its team tools, relay hooks and
    /// monitor: the engine's folder read-only and, by [`Self::allow_exec`], the file itself
    /// startable by exact path, even from `CrazyAgent.app/Contents/MacOS` where the macOS
    /// service runs it (OP24R2-1). Input: the engine's canonical path. Output: the widened
    /// sandbox, or `Invalid`.
    pub fn allow_engine(self, engine: &Path) -> Result<Self, SandboxError> {
        let folder = engine
            .parent()
            .ok_or_else(|| invalid("the engine must sit in a folder"))?;
        self.allow_read(folder)?.allow_exec(engine)
    }

    /// Grants a project's toolchains read-only (see [`toolchains::Toolchains::detect`]):
    /// each folder as [`Self::allow_read`] checks it, with every credential name below it
    /// denied, and each file when it is a canonical regular file clear of
    /// [`Self::secret_paths`]. Only Seatbelt grants them; the Linux and Windows boundaries
    /// leave them out. Input: the toolchains. Output: the widened sandbox, or `Invalid`.
    pub fn allow_tools(mut self, tools: &toolchains::Toolchains) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        for root in tools.roots() {
            if self.tool_roots.contains(root) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX {
                return Err(invalid("too many toolchain folders"));
            }
            granted_root(root, &owner, &secrets)?;
            self.tool_roots.push(root.clone());
        }
        for file in tools.files() {
            if self.tool_files.contains(file) {
                continue;
            }
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            clear_of_secrets(file, &secrets)?;
            if !clean_absolute(file) || !file.is_file() || canonical(file)? != *file {
                return Err(invalid("toolchain files must be existing canonical files"));
            }
            self.tool_files.push(file.clone());
        }
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants the owner's extra read-only folders for this project (see
    /// [`extra_paths::checked`]) as toolchain folders are granted, with credential names
    /// carved out, skipping each one that is gone or no longer passes
    /// [`Self::allow_read`]'s checks. Input: the saved folders. Output: the widened sandbox
    /// and how many folders were skipped.
    pub fn allow_extra_reads(mut self, paths: &[PathBuf]) -> Result<(Self, usize), SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        let mut skipped = 0;
        for path in paths {
            if self.tool_roots.contains(path) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX
                || granted_root(path, &owner, &secrets).is_err()
            {
                skipped += 1;
                continue;
            }
            self.tool_roots.push(path.clone());
        }
        assert!(skipped <= paths.len() && self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok((self, skipped))
    }

    /// Grants the agent's tool cache, made by `tool_cache::prepare`, as one more writable
    /// root: a non-working-tree root under the SM1-R rule, so no `.git` entry can be made,
    /// moved or written there, and the root of `pnpm_config_store_dir`, and of `UV_CACHE_DIR`
    /// when its uv markers are in place (T8). Inputs: the cache root, a canonical existing
    /// folder clear of [`Self::secret_paths`] and of every other root, and whether its uv
    /// markers are in place. Output: the widened sandbox, or `Invalid` for an unsafe or second
    /// cache.
    pub fn allow_tool_cache(mut self, root: &Path, uv: bool) -> Result<Self, SandboxError> {
        if self.tool_cache.is_some() {
            return Err(invalid("the sandbox already has a tool cache"));
        }
        granted_root(root, &owner_home()?, &self.secret_paths())?;
        let overlaps = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain([&self.project_root])
            .any(|other| other.starts_with(root) || root.starts_with(other));
        if overlaps {
            return Err(invalid("the tool cache must lie outside every other root"));
        }
        self.tool_cache = Some((root.to_path_buf(), uv));
        assert!(self.writes(root));
        Ok(self)
    }

    /// The program `name` the agent's PATH finds first, if the engine may run it for the
    /// agent (T8 C3), as [`Self::trusted_program`] decides. Input: a plain program name.
    /// Output: the canonical path, or None.
    pub fn tool_program(&self, name: &str) -> Option<PathBuf> {
        assert!(!name.is_empty() && !name.contains('/'));
        let homes = toolchains::ToolHomes::at(&self.owner_home);
        let path = toolchains::agent_path(&homes, AGENT_PATH);
        let found = std::env::split_paths(&path)
            .map(|folder| folder.join(name))
            .find(|candidate| candidate.is_file())?;
        self.trusted_program(&found)
    }

    /// A found program the engine may run for the agent (T8 C3): its canonical path when
    /// that lies in a read-only system or granted tool root and in no root the agent can
    /// write, so the agent cannot swap the program the engine runs. Input: the path found.
    /// Output: the canonical path, or None.
    pub fn trusted_program(&self, found: &Path) -> Option<PathBuf> {
        let program = canonical(found).ok()?;
        let read_only = SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.tool_roots.iter().map(PathBuf::as_path))
            .any(|root| program.starts_with(root));
        let writable = self
            .writable_roots()
            .any(|root| program.starts_with(root) || root.starts_with(&program));
        (read_only && !writable && program.is_file()).then_some(program)
    }

    /// Adds the agent's short writable temp folder (see [`toolchains::make_tmp`]), which
    /// becomes its `TMPDIR`. Input: a canonical existing folder clear of
    /// [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid` for an unsafe or
    /// second folder.
    pub fn allow_scratch(mut self, tmp: &Path) -> Result<Self, SandboxError> {
        if self.scratch.is_some() {
            return Err(invalid("the sandbox already has a temp folder"));
        }
        granted_root(tmp, &owner_home()?, &self.secret_paths())?;
        self.scratch = Some(tmp.to_path_buf());
        Ok(self)
    }

    /// Keeps one folder inside a granted root out of the agent's reach, read and write, as
    /// the secret paths are; the graph step hides the project's `.crazyagent/` from graphify.
    /// Input: a clean absolute path below a root, in a real parent folder, absent or a real
    /// folder. Output: the narrowed sandbox, or `Invalid`. Only Seatbelt holds it, so the
    /// Linux and Windows boundaries refuse a sandbox with hidden folders.
    pub fn hide(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.hidden.len() >= HIDDEN_MAX {
            return Err(invalid("too many hidden folders"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| folder.starts_with(root) && folder != root.as_path());
        let parent = folder.parent().filter(|parent| {
            parent
                .canonicalize()
                .is_ok_and(|real| real.as_path() == *parent)
        });
        let plain = fs::symlink_metadata(folder).map_or_else(
            |error| error.kind() == io::ErrorKind::NotFound,
            |metadata| metadata.is_dir() && !metadata.file_type().is_symlink(),
        );
        if !clean_absolute(folder) || !inside || parent.is_none() || !plain {
            return Err(invalid(
                "a hidden folder must be a plain folder path inside a granted root",
            ));
        }
        self.hidden.push(folder.to_path_buf());
        Ok(self)
    }

    /// Keeps the secret files and folders of the project folder `root` closed to the agent
    /// (DEC-147.9): contents, attribute values, writes, runs, links and clones of every path
    /// there with a name in [`shared_folders::SECRET_NAMES`], in any letter case, are denied,
    /// names and sizes stay visible, except under `root/.git` and inside the agent's own
    /// writable roots below it, its worktree or clone, never `root` itself. Each of `revealed`, the secrets the owner
    /// shared back, is readable again, never writable. The deny comes before every older
    /// deny in the profile, so none of those gives way. Inputs: a clean canonical folder the
    /// sandbox already reads and at most [`shared_folders::SECRETS_MAX`] clean paths strictly
    /// inside it. Output: the sandbox, or `Invalid` for a second call or a path out of shape.
    pub fn keep_project_secrets(
        mut self,
        root: &Path,
        revealed: &[PathBuf],
    ) -> Result<Self, SandboxError> {
        if self.project_secrets.is_some() {
            return Err(invalid("the project's secrets are already kept closed"));
        }
        let readable = self
            .read_roots
            .iter()
            .chain([&self.project_root])
            .any(|read| read == root);
        if !clean_absolute(root) || !readable || canonical(root)? != root {
            return Err(invalid(
                "the folder whose secrets stay closed must be a canonical folder the sandbox reads",
            ));
        }
        if revealed.len() > shared_folders::SECRETS_MAX
            || !revealed.iter().all(|path| {
                clean_absolute(path)
                    && path.starts_with(root)
                    && path != root
                    && !path.starts_with(root.join(".git"))
            })
        {
            return Err(invalid(
                "a shared-back secret must be a clean path inside the project folder",
            ));
        }
        self.project_secrets = Some(root.to_path_buf());
        self.secrets_revealed = revealed.to_vec();
        Ok(self)
    }

    /// Keeps every top-level entry of the [`Self::keep_project_secrets`] folder closed as
    /// [`Self::keep_out`] does but `open`, so one made after launch stays closed (L4 r1 L1);
    /// own writable roots below it stay open, never the folder itself. Input: at most
    /// [`OPEN_TOPS_MAX`] names, each [`plain_top`]. Output: the narrowed sandbox, or `Invalid`
    /// without that folder, for a second call or for a name out of shape.
    pub fn open_only_tops(mut self, open: &[String]) -> Result<Self, SandboxError> {
        if self.project_secrets.is_none() || self.open_tops.is_some() {
            return Err(invalid(
                "the open top-level names need the project folder, once",
            ));
        }
        if open.len() > OPEN_TOPS_MAX || !open.iter().all(|name| plain_top(name)) {
            return Err(invalid("an open top-level name must be one plain name"));
        }
        self.open_tops = Some(open.to_vec());
        Ok(self)
    }

    /// Lets the agent write one shared top-level folder of the project folder that git does
    /// not track (DEC-147.9 Read-write) as one more writable root, pinned in place; the secret
    /// names inside it stay closed, and every older deny still wins. Never a hidden folder, an
    /// engine folder or one named `context` ([`shared_folders::never_written`]). Input: a canonical existing folder
    /// directly inside the folder [`Self::keep_project_secrets`] named, clear of
    /// [`Self::secret_paths`] and of every other writable root. Output: the widened sandbox, or
    /// `Invalid`; only Seatbelt holds it.
    pub fn allow_shared_write(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.shared_writes.len() >= shared_folders::SHARED_WRITES_MAX {
            return Err(invalid("too many shared writable folders"));
        }
        let in_project = self
            .project_secrets
            .as_ref()
            .is_some_and(|root| folder.parent() == Some(root.as_path()));
        let kept_read_only = match (self.project_secrets.as_ref(), folder.file_name()) {
            (Some(root), Some(name)) => name
                .to_str()
                .is_none_or(|name| shared_folders::never_written(root, name)),
            _ => true,
        };
        if !in_project || kept_read_only {
            return Err(invalid(
                "a shared writable folder must be a top-level project folder kept under its secret rule",
            ));
        }
        granted_root(folder, &owner_home()?, &self.secret_paths())?;
        if self
            .writable_roots()
            .any(|root| root.starts_with(folder) || folder.starts_with(root))
        {
            return Err(invalid(
                "a shared writable folder must lie outside every other writable root",
            ));
        }
        self.shared_writes.push(folder.to_path_buf());
        assert!(self.writes(folder));
        assert!(self.shared_writes.len() <= shared_folders::SHARED_WRITES_MAX);
        Ok(self)
    }

    /// The Seatbelt rule of [`Self::open_only_tops`]: every path below the project folder is
    /// kept out but the open top-level names and the agent's own writable roots there.
    /// Output: the rule, empty when no names were set.
    fn closed_tops_rule(&self) -> String {
        let (Some(root), Some(open)) = (self.project_secrets.as_ref(), self.open_tops.as_ref())
        else {
            return String::new();
        };
        assert!(clean_absolute(root));
        let pattern = seatbelt_pattern(root);
        let mut text = format!("{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"^{pattern}/\")");
        let names: Vec<String> = open.iter().map(|name| regex_escape(name)).collect();
        for suffix in ["$", "/"] {
            for body in regex_bodies(&format!("^{pattern}/"), &names, suffix) {
                text.push_str(&format!("\n    (require-not (regex #\"{body}\"))"));
            }
        }
        for own in self.writable_roots().filter(|own| {
            own.starts_with(root) && own.as_path() != root.as_path() && clean_absolute(own)
        }) {
            text.push_str(&format!(
                "\n    (require-not (subpath \"{}\"))",
                own.display()
            ));
        }
        text.push_str("))\n");
        text
    }

    /// The Seatbelt rules of [`Self::keep_project_secrets`]: the deny of secret names in the
    /// project folder outside `.git` and the agent's own writable roots there (never the
    /// shared writable folders); no clone of any folder, and no move or removal of a folder
    /// inside a shared writable folder, so no folder that holds a secret leaves the name rule
    /// by a plain-named parent (L4 r1 H2); then the read allow of env templates
    /// ([`shared_folders::SECRET_TEMPLATES`]) and of each shared-back secret. Output: the
    /// rules, empty when no folder was named.
    fn project_secret_rules(&self) -> String {
        let Some(root) = self
            .project_secrets
            .as_ref()
            .filter(|root| clean_absolute(root))
        else {
            return String::new();
        };
        let names: Vec<String> = shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let own: String = self
            .writable_roots()
            .filter(|own| {
                own.starts_with(root)
                    && own.as_path() != root.as_path()
                    && !self.shared_writes.contains(own)
                    && clean_absolute(own)
            })
            .map(|own| format!("\n    (require-not (subpath \"{}\"))", own.display()))
            .collect();
        let prefix = format!("^{}/(.*/)?", seatbelt_pattern(root));
        let mut text = String::new();
        for body in regex_bodies(&prefix, &names, "(/|$)") {
            text.push_str(&format!(
                "{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"{body}\")\n    (require-not (subpath \"{}/.git\")){own}))\n",
                root.display()
            ));
        }
        text.push_str("(deny file-clone (vnode-type DIRECTORY))\n");
        for folder in self
            .shared_writes
            .iter()
            .filter(|folder| clean_absolute(folder))
        {
            text.push_str(&format!(
                "(deny file-write-unlink\n  (require-all\n    (subpath \"{}\")\n    (vnode-type DIRECTORY)))\n",
                folder.display()
            ));
        }
        text.push_str(&format!(
            "(allow file-read-data file-read-xattr\n  (regex #\"^{}/(.*/)?{}$\"))\n",
            seatbelt_pattern(root),
            fold_case(shared_folders::SECRET_TEMPLATES)
        ));
        let revealed: Vec<&PathBuf> = self
            .secrets_revealed
            .iter()
            .filter(|path| clean_absolute(path))
            .collect();
        if !revealed.is_empty() {
            text.push_str("(allow file-read-data file-read-xattr");
            for path in revealed {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text
    }

    /// The project folder whose secret names this sandbox keeps closed, if any.
    pub fn project_secrets(&self) -> Option<&Path> {
        self.project_secrets.as_deref()
    }

    /// The shared top-level project folders this sandbox lets the agent write.
    pub fn shared_writes(&self) -> &[PathBuf] {
        &self.shared_writes
    }

    /// Holds the project's agent folders choice in this sandbox (DEC-124): a Sandboxed IDE
    /// terminal keeps to what the owner chose for the IDE. Only Seatbelt holds it, so the
    /// Linux and Windows boundaries refuse a sandbox with a choice other than Allow all
    /// changes. Input: the rule. Output: the sandbox.
    pub fn protect_agent_files(mut self, rule: policy::AgentFilesRule) -> Self {
        self.agent_files = Some(rule);
        self
    }

    /// Marks the home as one the owner keeps for their own tools, which no launch resets, as
    /// the IDE terminal's and the IDE server's are: the profile leaves out
    /// [`agent_config_denies`], so `npm login`, `git config --global` and `cargo install`
    /// write there and copies keep their flags (RV-DS241-188, -187), and [`Self::install`]
    /// makes no folder there, as [`owner_terminal_env`] leaves out [`AGENT_HOME_ENV`].
    /// Output: the sandbox.
    pub fn keep_owner_config(mut self) -> Self {
        self.owner_config = true;
        self
    }

    /// Names the engine's data root the agent home is judged by, as a master turn's home may
    /// be a configured RCLI home the owner chose in one of their own folders (never HOME
    /// itself or a folder holding it, which [`Self::new`] refuses): [`Self::install`] makes
    /// and removes nothing in a home not strictly below the root, so a link a dotfile manager
    /// made there stays, and clears what holds a config folder's name in one that is
    /// (rv-code2 b8 F1). The profile keeps every deny either way. Input: the canonical data
    /// root. Output: the sandbox.
    #[cfg(unix)]
    pub fn anchor_home(mut self, engine_root: &Path) -> Self {
        assert!(engine_root.is_absolute());
        let below = self.agent_home != engine_root && self.agent_home.starts_with(engine_root);
        self.engine_home = Some(below);
        self
    }

    /// Keeps one file or folder inside a granted root from being read, written or run: its
    /// name, size and dates stay visible, its contents, folder listing and attribute values
    /// do not, so a tool that stats an import does not fail; the graph step keeps the
    /// project's secret-like and git-ignored paths from graphify (MAP-2). Input: a clean
    /// absolute path below a root, of any kind or absent. Output: the narrowed sandbox, or
    /// `Invalid` past [`KEPT_OUT_MAX`]. Only Seatbelt holds it, so the Linux and Windows
    /// boundaries refuse such a sandbox.
    pub fn keep_out(mut self, path: &Path) -> Result<Self, SandboxError> {
        if self.kept_out.len() >= KEPT_OUT_MAX {
            return Err(invalid("too many paths kept out"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| path.starts_with(root) && path != root.as_path());
        if !clean_absolute(path) || !inside {
            return Err(invalid(
                "a path kept out must be a clean absolute path inside a granted root",
            ));
        }
        self.kept_out.push(path.to_path_buf());
        Ok(self)
    }

    /// Whether [`Self::keep_out`] takes the form of `path`, a clean absolute path, so a
    /// caller can check every path before it makes anything. Input: the path. Output: the
    /// answer; a path kept out must still lie inside a granted root.
    pub fn keepable(path: &Path) -> bool {
        clean_absolute(path)
    }

    /// Lets a native-mode lead run its own tmux server (DEC-104): an exact socket grant, as
    /// `allow_unix_socket` gives, for its own socket in its short temp folder, and the tmux
    /// program itself, readable and startable as one file, never its folder: the bundled one
    /// sits in the app's `Contents/MacOS` beside the engine (DEC-157). No other Unix
    /// socket becomes reachable, the engine's own tmux server included, and the server and
    /// every engine client on it stay inside this profile; its teammates run in a server of
    /// their own (see [`Self::serve_tmux_server`]). Only Seatbelt holds it. Input: the socket
    /// (`terminal::harness_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn allow_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, false)
    }

    /// Lets a native lead's teammates run their own tmux server (rv-audit S1 F1): as
    /// [`Self::allow_tmux_server`], but the socket is served only, as
    /// [`Self::serve_unix_socket`] gives, so no teammate pane the server forks connects to
    /// it, unlinks it or binds a server of its own there. Input: the socket
    /// (`terminal::teammates_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn serve_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, true)
    }

    /// Splits a native-mode lead's sandbox into the lead's and its teammates' (DEC-104,
    /// rv-audit S1 F1, TTDL F1). The lead runs its own tmux server at `own` and only reaches
    /// the engine's caller socket at `callers` and its teammates' server at `mates`. The
    /// teammates serve `mates`, only reach `callers` and are shut out of `own`, so no agent
    /// binds, blocks or replaces the caller socket the engine answers on (DEC-111 Q18,
    /// rv-secreview RH-1) and no teammate holds a listener or places a node where the lead
    /// connects. They read but never write the launch folders the engine installs in the
    /// lead's home, its hooks, skills and relay plugin, which the lead's Claude Code runs
    /// (TPH), nor move, replace or re-permission that home itself (TPH r1 F1), nor the
    /// shell snapshot and session scripts its Bash commands run first (TPH-2). The teammates
    /// run in their own home and only read the lead's, except the team folders Claude Code
    /// shares, where they make no link or folder but a lock, and never write the `git` on the
    /// lead's path (TPH-2 C). They neither read nor write the lead's own temp folder in the
    /// short temp folder they share ([`toolchains::native_lead_tmp`], TPH-2 residual), so no
    /// teammate reads, races or plants a file the lead stages there. Both are shut out of the
    /// name the engine binds the caller socket at before it moves it over `callers`, so
    /// neither swaps its own listener in there (rv-secreview RH2R2-1). The lead's part of the
    /// relay mailbox is hidden from the teammates but for its `relay.json`, which they only
    /// read, and they never move or re-permission the mailbox or the temp folder holding it
    /// (relay mailbox forgery C1). Neither the lead nor a teammate moves the tool mailbox, so
    /// no answer is written outside the rule that keeps answers the engine's. Inputs: the
    /// three sockets, the canonical tmux program and
    /// the teammates' home beside the lead's, its team folders already made, with the
    /// mailbox in the agent temp folder. Output: the lead's and the teammates' sandboxes,
    /// `Invalid`, or `Unavailable` off macOS.
    #[cfg(unix)]
    pub fn native_team(
        self,
        own: &Path,
        mates: &Path,
        callers: &Path,
        tmux: &Path,
        mates_home: &Path,
    ) -> Result<(Self, Self), SandboxError> {
        let home = self.agent_home.clone();
        granted_root(mates_home, &self.owner_home, &self.secret_paths())?;
        if mates_home.starts_with(&home) || home.starts_with(mates_home) {
            return Err(invalid("the teammates' home must sit beside the lead's"));
        }
        let shared: Vec<PathBuf> = CLAUDE_TEAM_SHARED
            .iter()
            .map(|folder| home.join(folder))
            .collect();
        for folder in &shared {
            granted_root(folder, &self.owner_home, &self.secret_paths())?;
        }
        let binding = crate::team::caller::binding_path(callers);
        let mailbox = self.agent_tmp.join(crate::team::tool::MAILBOX_FOLDER);
        let lead_part = mailbox.join(crate::coord::relay::LEAD_PART);
        let mut teammates = self
            .clone()
            .serve_tmux_server(mates, tmux)?
            .reach_unix_socket(callers)?
            .shut_unix_socket(own)?
            .shut_unix_socket(&binding)?
            .allow_read(&home)?
            .hide(&lead_part)?;
        let lead_tmp = teammates
            .scratch
            .as_deref()
            .map(toolchains::native_lead_tmp)
            .ok_or_else(|| invalid("a native team needs the agent temp folder"))?;
        teammates = teammates.hide(&lead_tmp)?;
        teammates.agent_home = mates_home.to_path_buf();
        teammates.revealed = vec![lead_part.join(crate::coord::relay::ROSTER_FILE)];
        teammates.read_only = vec![
            home.join(crate::launch_context::STATE),
            home.join(crate::coord::relay::PLUGIN_FOLDER),
        ];
        teammates.read_only.extend(
            crate::agent_home::CLAUDE_BASH_PRELUDES
                .iter()
                .map(|folder| home.join(folder)),
        );
        teammates.read_only.extend(
            self.scratch
                .iter()
                .map(|scratch| scratch.join(toolchains::GIT_FOLDER)),
        );
        let answers = mailbox.clone();
        teammates.answers = Some(answers.clone());
        teammates.fixed = vec![home.clone(), self.agent_tmp.clone(), mailbox];
        teammates.fixed.extend(shared.iter().cloned());
        teammates.shared = shared;
        teammates.teammates = true;
        if teammates.writes(&home) {
            return Err(invalid("a teammates' writable root holds the lead's home"));
        }
        let mut lead = self
            .allow_tmux_server(own, tmux)?
            .reach_unix_socket(callers)?
            .reach_unix_socket(mates)?
            .shut_unix_socket(&binding)?;
        lead.fixed.push(answers.clone());
        lead.answers = Some(answers);
        Ok((lead, teammates))
    }

    /// The writes no agent of a native team may make to the engine's answers in the tool
    /// mailbox the team shares, a reply or the part file it is written under, in any spelling
    /// the volume folds together. An agent may still remove a reply, as its tool server does
    /// once it has read one. Output: the rule text, empty for any other profile.
    fn answers_denies(&self) -> String {
        let Some(mailbox) = &self.answers else {
            return String::new();
        };
        let rule = format!(
            "(regex #\"^{}/[^/]+({}|{})$\")",
            seatbelt_pattern(mailbox),
            any_case(crate::team::tool::REPLY_SUFFIX),
            any_case(crate::team::tool::ANSWER_PART_SUFFIX),
        );
        format!("(deny file-write* {rule})\n(allow file-write-unlink {rule})\n")
    }

    /// The writes a native lead's teammates may not make in the lead's worktree (OP-9,
    /// OP-9b), none for any other profile: in the lead's own clone, those
    /// [`teammates_clone_denies`] names; in a linked worktree, anything but the teammates'
    /// folder, and the lead's worktree record and the object store, since the engine
    /// commits the teammates' turns, nor the teammates' folder entry, nor make, move or
    /// remove anything directly in it, such as a teammate's folder, where the engine's
    /// setup git adds each teammate worktree (OP-9 r2, OP9B-C1). Output: the rule text,
    /// empty when none applies.
    fn teammates_denies(&self) -> String {
        if !self.teammates {
            return String::new();
        }
        match (&self.clone, &self.checkout) {
            (Some(clone), _) => teammates_clone_denies(clone, self.lead_branch.as_deref()),
            (None, Some(checkout)) => {
                let worktree = checkout.worktree.display();
                let root = seatbelt_pattern(&checkout.worktree);
                let teammates = any_case(crate::agent_git::TEAMMATES_FOLDER);
                let text = format!(
                    "(deny file-write*\n  (require-all\n    (subpath \"{worktree}\")\n    (require-not (subpath \"{worktree}/{}\"))))\n(deny file-write*\n  (subpath \"{}\")\n  (subpath \"{}\")\n  (regex #\"^{root}/{teammates}$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
                    crate::agent_git::TEAMMATES_FOLDER,
                    checkout.admin.display(),
                    checkout.objects.display()
                );
                assert_eq!(text.matches("(subpath \"/").count(), 4);
                text
            }
            (None, None) => String::new(),
        }
    }

    /// Grants one tmux server in the agent temp folder, served and reached or served only.
    /// Takes the socket, the canonical tmux program and whether the socket is served only.
    /// Returns the widened sandbox, `Invalid`, or `Unavailable` off macOS.
    fn tmux_server(
        mut self,
        socket: &Path,
        tmux: &Path,
        served_only: bool,
    ) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(SandboxError::Unavailable(TMUX_SERVER_NEEDS_SEATBELT.into()));
        }
        let scratch = self
            .scratch
            .clone()
            .ok_or_else(|| invalid("a tmux server needs the agent temp folder"))?;
        if self
            .sockets
            .iter()
            .chain(&self.served)
            .any(|known| known.parent() == Some(scratch.as_path()))
        {
            return Err(invalid("the sandbox already has a tmux server"));
        }
        if !clean_absolute(socket)
            || socket.parent() != Some(scratch.as_path())
            || socket.as_os_str().len() > SOCKET_PATH_BYTES_MAX
        {
            return Err(invalid("the tmux socket must sit in the agent temp folder"));
        }
        clear_of_secrets(socket, &self.secret_paths())?;
        if !clean_absolute(tmux) || !tmux.is_file() || canonical(tmux)? != tmux {
            return Err(invalid("the tmux program must be a canonical file"));
        }
        if !self.tool_files.iter().any(|file| file == tmux) {
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            self.tool_files.push(tmux.to_path_buf());
        }
        self = self.allow_exec(tmux)?;
        assert!(self.tool_files.iter().any(|file| file == tmux));
        self = if served_only {
            self.serve_unix_socket(socket)?
        } else {
            self.allow_unix_socket(socket)?
        };
        assert!(self.writes(socket.parent().unwrap_or(socket)));
        Ok(self)
    }

    /// Marks the project folder as one the engine made for one job of its own, a plugin
    /// server's check or a context-mode install, not a project: on Windows it holds no project
    /// guards, so the boundary leaves it writable at its top (DEC-193); Seatbelt and Landlock
    /// are unchanged. Input: a sandbox confined to no worktree, clone or setup. Output: it, marked.
    pub fn as_job_folder(mut self) -> Self {
        assert_eq!(self.confinements(), 0);
        assert!(!self.job_folder);
        self.job_folder = true;
        self
    }

    /// Whether the project folder holds the project guards of [`Self::protected_paths`]:
    /// true but for a folder marked by [`Self::as_job_folder`]. Output: the answer.
    pub fn holds_project_guards(&self) -> bool {
        !self.job_folder
    }

    /// The project folder this sandbox confines writes to.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The agent's managed home directory.
    pub fn agent_home(&self) -> &Path {
        &self.agent_home
    }

    /// The agent's managed temp directory.
    pub fn agent_tmp(&self) -> &Path {
        &self.agent_tmp
    }

    /// The extra read-only roots, such as harness install directories.
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    /// The network reach this sandbox grants the agent.
    pub fn network(&self) -> &SandboxNetwork {
        &self.network
    }

    /// Sets how this agent's sockets are held on Linux when its network is limited. Input:
    /// the owner's setting. Output: the sandbox.
    pub fn limit_sockets(mut self, sockets: LinuxSockets) -> Self {
        self.linux_sockets = sockets;
        self
    }

    /// How this agent's sockets are held on Linux when its network is limited.
    pub fn linux_sockets(&self) -> LinuxSockets {
        self.linux_sockets
    }

    /// The loopback ports a Linux that nests agents carries for this one over a bridge, the
    /// agent then having a network namespace of its own: the sockets setting is `TcpOnly`
    /// and the limited network names loopback ports only, each [`BRIDGE_PORT_MIN`] or
    /// higher. Output: the ports in rising order, or none when the agent keeps the shared
    /// network.
    pub fn bridged_ports(&self) -> Option<Vec<u16>> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.is_empty() {
            return None;
        }
        let mut ports = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => ports.push(*port),
                _ => return None,
            }
        }
        ports.sort_unstable();
        ports.dedup();
        assert!(!ports.is_empty() && ports.len() <= REMOTES_MAX);
        assert!(!self.resolves_over_tcp());
        Some(ports)
    }

    /// The network a Linux that nests agents gives this one of its own, leaving only
    /// through its ports on any host: the sockets setting is `TcpOnly`, the limited network
    /// names at least one port on any host and every other remote is a loopback port
    /// [`BRIDGE_PORT_MIN`] or higher, bridged as [`ProjectSandbox::bridged_ports`] does.
    /// Output: the ports, or none when the agent keeps the shared network.
    pub fn routed(&self) -> Option<Routed> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.len() > REMOTES_MAX {
            return None;
        }
        let mut any = Vec::with_capacity(remotes.len());
        let mut bridged = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::AnyHost(port) => any.push(*port),
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => bridged.push(*port),
                SandboxRemote::Loopback(_) => return None,
            }
        }
        if any.is_empty() {
            return None;
        }
        any.sort_unstable();
        any.dedup();
        bridged.sort_unstable();
        bridged.dedup();
        assert!(!any.contains(&0));
        assert!(any.len() + bridged.len() <= REMOTES_MAX);
        assert!(self.bridged_ports().is_none());
        assert!(self.resolves_over_tcp());
        Some(Routed { any, bridged })
    }

    /// The port that alone keeps this agent on this computer's network on a Linux that
    /// nests agents: the sockets setting is `TcpOnly` and the limited network names
    /// loopback ports only, but one lies below [`BRIDGE_PORT_MIN`]. Output: the lowest such
    /// port, or none when the agent is bridged or shares the network for another reason.
    pub fn unbridged_port(&self) -> Option<u16> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly {
            return None;
        }
        let mut lowest: Option<u16> = None;
        for remote in remotes {
            let SandboxRemote::Loopback(port) = remote else {
                return None;
            };
            if *port < BRIDGE_PORT_MIN && lowest.is_none_or(|low| *port < low) {
                lowest = Some(*port);
            }
        }
        assert!(lowest.is_none() || self.bridged_ports().is_none());
        lowest
    }

    /// True when, on Linux, this agent looks host names up over TCP: the setting is
    /// `TcpOnly` and its limited network names a remote on any host. It then may reach TCP
    /// port 53 and gets `RES_OPTIONS=use-vc`; loopback-only and empty networks get neither.
    pub fn resolves_over_tcp(&self) -> bool {
        self.linux_sockets == LinuxSockets::TcpOnly
            && matches!(&self.network, SandboxNetwork::Only(remotes)
                if remotes.iter().any(|remote| matches!(remote, SandboxRemote::AnyHost(_))))
    }

    /// Why the Linux sockets setting refuses to start `program` in this sandbox:
    /// [`LINUX_LIMITED_NETWORK_OFF`] for any limited network under `Refuse`, and
    /// [`LINUX_RESOLVER_NEEDS_UDP`] under `TcpOnly` when the agent resolves names and the
    /// program, or the interpreter a script names, is not a glibc one, and
    /// [`LINUX_HARNESS_UNCHECKED`] when that interpreter cannot be checked ([`elf::lookup`]).
    /// The setting never changes silently. Input: the harness program. Output: the refusal
    /// code, or None.
    pub fn linux_refusal(&self, program: &Path) -> Option<&'static str> {
        match (&self.network, self.linux_sockets) {
            (SandboxNetwork::Open, _) => None,
            (SandboxNetwork::Only(_), LinuxSockets::Refuse) => Some(LINUX_LIMITED_NETWORK_OFF),
            _ if self.resolves_over_tcp() => match elf::lookup(program) {
                elf::Lookup::OverTcp => None,
                elf::Lookup::NotOverTcp => Some(LINUX_RESOLVER_NEEDS_UDP),
                elf::Lookup::Unjudged => Some(LINUX_HARNESS_UNCHECKED),
            },
            _ => None,
        }
    }

    /// Paths no agent may write although they sit in a writable root: both `context/`
    /// folders, the project instructions file every later agent and the master get as the
    /// owner's text (SA vid 11), the git hooks, config and its lock, `.git/modules` and, for
    /// an agent confined to a worktree, its admin folder's `modules`, where git keeps
    /// submodule repositories.
    /// Every platform boundary keeps these read-only, except that Windows guards the config
    /// but not its lock (`windows::GIT_CONFIG_LOCK`).
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let git = self.project_root.join(".git");
        let mut paths = vec![
            self.project_root.join("context"),
            self.project_root.join(".crazyagent").join("context"),
            self.project_root
                .join(".crazyagent")
                .join(crate::agent_home::PROJECT_FILE),
            git.join("hooks"),
            git.join("config"),
            git.join("config.lock"),
            git.join("modules"),
        ];
        if let Some(checkout) = &self.checkout {
            paths.push(checkout.admin.join("modules"));
        }
        paths
    }

    /// How many confinements the sandbox holds: its own worktree, its own clone or the
    /// teammates' setup, at most one. Output: the count.
    fn confinements(&self) -> usize {
        usize::from(self.checkout.is_some())
            + usize::from(self.clone.is_some())
            + usize::from(self.setup.is_some())
    }

    /// Every folder this sandbox lets the agent write: the project folder, or for a
    /// confined agent its worktree, that worktree's git admin folder and the git object
    /// store, or for a clone-confined agent its clone alone, then its own home and temp
    /// dirs, its short temp folder, then its extra worktrees, then for a native lead's
    /// teammates the team folders they share in the lead's home, then its tool cache, the
    /// owner's temp folder when granted and each shared writable folder.
    pub fn writable_roots(&self) -> impl Iterator<Item = &PathBuf> {
        assert!(self.confinements() <= 1);
        let project: Vec<&PathBuf> = match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => vec![&checkout.worktree, &checkout.admin, &checkout.objects],
            (None, Some(clone), _) => vec![clone],
            (None, None, Some(setup)) => vec![&setup.folder, &setup.records],
            (None, None, None) => vec![&self.project_root],
        };
        project
            .into_iter()
            .chain([&self.agent_home, &self.agent_tmp])
            .chain(self.scratch.iter())
            .chain(self.worktrees.iter())
            .chain(self.shared.iter())
            .chain(self.tool_cache.iter().map(|(root, _)| root))
            .chain(self.owner_tmp.iter())
            .chain(self.shared_writes.iter())
    }

    /// The read-only toolchain folders, Seatbelt only, with credential names carved out.
    pub fn tool_roots(&self) -> &[PathBuf] {
        &self.tool_roots
    }

    /// Whether the agent could write `path` or move it away: true when the path is one of
    /// [`Self::writable_roots`] or inside one. Input: an absolute canonical path. Output:
    /// the answer.
    pub fn writes(&self, path: &Path) -> bool {
        self.writable_roots().any(|root| path.starts_with(root))
    }

    /// The Seatbelt profile text for this agent. Reading another process's arguments through
    /// sysctl is refused only with both the named sysctl deny and the explicit `process-info*`
    /// deny. What stays open: every read of a process of the same sandbox, the list of process
    /// ids, which carries only ids, and for `nohup`, by its own path, the one read of a process
    /// outside that it needs to detach from the console. Output: the text.
    pub fn profile(&self) -> String {
        let mut text = String::from("(version 1)\n(deny default)\n");
        text.push_str("(allow process-exec)\n(allow process-fork)\n");
        text.push_str(&app_exec_denies(
            self.writable_roots(),
            self.execs.iter().filter(|program| !self.writes(program)),
        ));
        text.push_str("(allow signal (target same-sandbox))\n");
        text.push_str("(allow sysctl-read)\n");
        text.push_str("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))\n");
        text.push_str("(deny process-info*)\n");
        text.push_str("(allow process-info* (target same-sandbox))\n");
        text.push_str("(allow process-info-listpids)\n");
        text.push_str("(allow process-info-pidinfo (process-path \"/usr/bin/nohup\"))\n");
        text.push_str("(allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n");
        text.push_str("(allow ipc-posix-sem)\n");
        text.push_str(
            "(allow ipc-posix-shm-read* (ipc-posix-name \"apple.shm.notification_center\"))\n",
        );
        text.push_str("(allow pseudo-tty)\n");
        text.push_str("(allow mach-lookup");
        for service in MACH_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read* file-map-executable");
        for root in SYSTEM_READ_ROOTS {
            text.push_str(&format!("\n  (subpath \"{root}\")"));
        }
        text.push_str(")\n(allow file-read*");
        for literal in METADATA_LITERALS {
            text.push_str(&format!("\n  (literal \"{literal}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read-metadata");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            text.push_str(&format!("\n  (path-ancestors \"{}\")", root.display()));
        }
        text.push_str(")\n(allow file-read* file-map-executable");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
        {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        for file in &self.tool_files {
            text.push_str(&format!("\n  (literal \"{}\")", file.display()));
        }
        text.push_str(")\n(allow file-write*");
        for root in self.writable_roots() {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        text.push_str(")\n");
        assert!(self.setup.is_none() || self.lead_branch.is_some());
        if let (Some(_), Some(branch)) = (&self.setup, &self.lead_branch) {
            text.push_str(&format!(
                "(allow file-write*\n  (regex #\"^{}/\\.git/(logs/)?refs/heads/({}|{}--[a-z0-9-]+(\\.lock)?)$\"))\n",
                seatbelt_pattern(&self.project_root),
                regex_escape(crate::agent_git::LINKED_PREFIX.trim_end_matches('/')),
                regex_escape(branch)
            ));
        }
        assert!(self.links.len() <= LINKS_MAX);
        for link in self.links.iter().filter(|link| clean_absolute(link)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                link.display()
            ));
        }
        text.push_str(&self.project_secret_rules());
        text.push_str("(deny file-read* file-write*");
        for secret in self.secret_paths() {
            if clean_absolute(&secret) {
                text.push_str(&format!("\n  (subpath \"{}\")", secret.display()));
            }
        }
        let credentials = CREDENTIAL_NAMES
            .iter()
            .map(|name| regex_escape(name))
            .collect::<Vec<_>>()
            .join("|");
        for root in &self.tool_roots {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{}/(.*/)?({credentials})", seatbelt_pattern(root)))
            ));
        }
        text.push_str(")\n(deny mach-lookup");
        for service in KEYCHAIN_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        if let Some((home, homes)) = self
            .carved_home
            .as_ref()
            .and_then(|home| Some((home, home.parent()?)))
            .filter(|(home, _)| clean_absolute(home))
        {
            assert!(self.engine_secrets.iter().any(|secret| secret == homes));
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (subpath \"{}\"))\n(allow file-read-metadata\n  (literal \"{}\"))\n",
                home.display(),
                homes.display()
            ));
        }
        if !self.hidden.is_empty() {
            text.push_str("(deny file-read* file-write* process-exec file-link file-clone");
            for folder in self.hidden.iter().filter(|folder| clean_absolute(folder)) {
                text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
            }
            text.push_str(")\n");
        }
        if !self.kept_out.is_empty() {
            text.push_str(KEPT_OUT_DENY);
            for path in self.kept_out.iter().filter(|path| clean_absolute(path)) {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text.push_str(&self.closed_tops_rule());
        for file in self.revealed.iter().filter(|file| clean_absolute(file)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                file.display()
            ));
        }
        text.push_str(&self.project_denies());
        if let Some(scratch) = &self.scratch {
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{}\"))\n",
                scratch.display()
            ));
        }
        if let Some(tmp) = &self.owner_tmp {
            let tmp = tmp.display();
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{tmp}\"))\n(deny file-write*\n  (require-all\n    (subpath \"{tmp}\")\n    (vnode-type SOCKET)))\n"
            ));
        }
        text.push_str(&self.git_link_denies());
        text.push_str(&self.submodule_denies());
        text.push_str(&self.nested_git_denies());
        text.push_str(&self.ancestry_denies());
        if let Some(checkout) = &self.checkout {
            text.push_str(&checkout.object_denies());
            let [head, _] = ADMIN_WRITES.map(any_case);
            text.push_str(&format!(
                "(deny file-write*\n  (regex #\"^{}/{head}$\"))\n",
                seatbelt_pattern(&checkout.admin)
            ));
        }
        if let Some(clone) = &self.clone {
            text.push_str(&clone_store_denies(clone));
        }
        text.push_str(
            "(allow file-read*\n  (literal \"/dev\")\n  (literal \"/dev/random\")\n  (literal \"/dev/urandom\")\n  (literal \"/dev/stdin\")\n  (literal \"/dev/stdout\")\n  (literal \"/dev/stderr\"))\n(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\")\n  (literal \"/dev/tty\")\n  (literal \"/dev/ptmx\")\n  (literal \"/dev/dtracehelper\")\n  (subpath \"/dev/fd\"))\n(allow file-read-metadata file-ioctl\n  (regex #\"^/dev/ttys[0-9]+$\"))\n(allow file-read* file-write-data\n  (require-all\n    (regex #\"^/dev/ttys[0-9]+$\")\n    (extension \"com.apple.sandbox.pty\")))\n",
        );
        for socket in &self.sockets {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n"
            ));
        }
        for socket in &self.served {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(deny file-write-unlink (literal \"{path}\"))\n"
            ));
        }
        for socket in self.reached.iter().chain(&self.granted) {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* (literal \"{path}\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n(deny file-write* (literal \"{path}\"))\n"
            ));
        }
        for socket in &self.shut {
            let path = socket.display();
            text.push_str(&format!(
                "(deny file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n"
            ));
        }
        for folder in &self.read_only {
            text.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                folder.display()
            ));
        }
        let launch = launch_denies(&self.agent_home);
        text.push_str(&launch);
        let config = if self.owner_config {
            String::new()
        } else {
            agent_config_denies(&self.agent_home)
        };
        text.push_str(&config);
        for root in self.writable_roots() {
            text.push_str(&root_pin(root));
        }
        for folder in &self.fixed {
            text.push_str(&format!(
                "(deny {FIXED_WRITES} (literal \"{}\"))\n",
                folder.display()
            ));
        }
        for folder in &self.shared {
            text.push_str(&shared_denies(folder));
        }
        let teammates = self.teammates_denies();
        text.push_str(&teammates);
        let answers = self.answers_denies();
        text.push_str(&answers);
        let agent_files = self
            .agent_files
            .map(|rule| rule.seatbelt(&self.project_root))
            .unwrap_or_default();
        text.push_str(&agent_files);
        let ignored = hfs_ignored_deny();
        text.push_str(&ignored);
        assert_eq!(
            text.matches("unix-socket").count(),
            2 * self.sockets.len() + self.served.len() + self.reached.len() + self.granted.len()
        );
        let mut last: Vec<String> = self
            .read_only
            .iter()
            .map(|folder| format!("(deny file-write* (subpath \"{}\"))", folder.display()))
            .collect();
        last.push(launch);
        if !config.is_empty() {
            last.push(config);
        }
        last.extend(self.writable_roots().map(|root| root_pin(root)));
        last.extend(
            self.fixed
                .iter()
                .map(|folder| format!("(deny {FIXED_WRITES} (literal \"{}\"))", folder.display())),
        );
        last.extend(self.shared.iter().map(|folder| shared_denies(folder)));
        if !teammates.is_empty() {
            last.push(teammates);
        }
        if !answers.is_empty() {
            last.push(answers);
        }
        if !agent_files.is_empty() {
            last.push(agent_files);
        }
        last.push(ignored);
        assert!(
            denies_follow_write_allows(&text, &last),
            "a read-only, root, fixed, shared or teammates' write deny must follow every write allow, since the last match wins"
        );
        let denied_last = |operations: &str, socket: &PathBuf| {
            text.rfind(&format!(
                "(deny {operations} (literal \"{}\"))",
                socket.display()
            )) > text.rfind("(allow file-write*")
        };
        assert!(
            self.served
                .iter()
                .all(|socket| denied_last("file-write-unlink", socket))
                && self
                    .reached
                    .iter()
                    .chain(&self.granted)
                    .all(|socket| denied_last("file-write*", socket)),
            "a served socket's unlink deny and a reached socket's write deny must follow every write allow, since the last match wins"
        );
        let network = text.len();
        match &self.network {
            SandboxNetwork::Open => {
                text.push_str(
                    "(allow network-outbound (remote ip \"*:*\"))\n(allow network-inbound (local ip \"*:*\"))\n(allow network-bind (local ip \"*:*\"))\n(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                );
                let owner = OWNER_PORTS
                    .iter()
                    .chain(&self.kept_ports)
                    .copied()
                    .chain(model_proxy::PROXY_PORTS)
                    .chain(model_proxy::RELAY_PORTS)
                    .map(|port| format!(" (local ip \"*:{port}\")"))
                    .collect::<String>();
                text.push_str(&format!(
                    "(deny network-bind{owner})\n(deny network-inbound{owner})\n"
                ));
                text.push_str(
                    "(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))\n",
                );
            }
            SandboxNetwork::Only(remotes) => {
                for remote in remotes {
                    let target = match remote {
                        SandboxRemote::Loopback(port) => format!("localhost:{port}"),
                        SandboxRemote::AnyHost(port) => format!("*:{port}"),
                    };
                    text.push_str(&format!(
                        "(allow network-outbound (remote ip \"{target}\"))\n"
                    ));
                }
                if remotes
                    .iter()
                    .any(|remote| matches!(remote, SandboxRemote::AnyHost(_)))
                {
                    text.push_str(
                        "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                    );
                }
            }
        }
        assert!(text.starts_with("(version 1)\n(deny default)\n"));
        assert!(
            text.find("(deny process-exec") > text.find("(allow process-exec)"),
            "the app bundle exec deny must follow the exec allow, since the last match wins"
        );
        assert!(!text.contains("(subpath \"/dev\")"));
        assert!(!text[network..].contains("network*") && !text[network..].contains("unix-socket"));
        assert!(
            MACH_SERVICES
                .iter()
                .all(|service| !KEYCHAIN_SERVICES.contains(service))
        );
        text
    }

    /// The Seatbelt rule that keeps the files the engine and the owner's git read in the
    /// project folder out of agent hands: both `context/` folders, the git hooks and the
    /// rebase, sequencer, remotes and branches state, each with everything below it, and the
    /// entries `.git`, `.crazyagent`, the project instructions file `.crazyagent/instructions.md`
    /// (SA vid 11), the git config and its lock, the index and its lock, `commondir`,
    /// `config.worktree`, `objects` and `objects/info`, none of which an agent may write or
    /// hard link under another name it could write through (SA vid 11 r2). Each name below the
    /// project folder matches in any letter case, so the rule does not depend on how Seatbelt
    /// folds case on the case-insensitive volume, where git finds a name however it is typed
    /// (RV-DS241-15). `.git/info`, `.git/shallow`, the alternates files and `.git/modules`
    /// are kept the same way by [`Self::ancestry_denies`] and [`Self::submodule_denies`].
    /// Input: the policy. Output: the rule text.
    fn project_denies(&self) -> String {
        let project = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let state = any_case(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let context = any_case("context");
        let objects = format!("{git}/{}", any_case("objects"));
        let mut below = vec![context.clone(), format!("{state}/{context}")];
        below.extend(GIT_STATE_TREES.map(|name| format!("{git}/{}", any_case(name))));
        let mut entries = vec![
            git.clone(),
            format!("{state}/{}", any_case(crate::agent_home::PROJECT_FILE)),
            state,
            format!("{objects}/{}", any_case("info")),
            objects,
        ];
        entries.extend(GIT_STATE_FILES.map(|name| format!("{git}/{}", any_case(name))));
        let mut text = String::from("(deny file-write* file-link");
        for path in &below {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{project}/{path}"))
            ));
        }
        for path in &entries {
            text.push_str(&format!("\n  (regex #\"^{project}/{path}$\")"));
        }
        text.push_str(")\n");
        assert_eq!(below.len() + entries.len(), 19);
        assert_eq!(text.matches("(regex #\"^/").count(), 19);
        text
    }

    /// The Seatbelt rules that keep the owner's git reading the metadata the engine made.
    /// No agent may move the project folder, the git worktrees folders or a worktree, which
    /// would carry protected files out of their denied paths and back. A worktree's `.git`
    /// file and its `commondir` and `gitdir` files may be created once, as `git worktree add`
    /// does, only by a profile that adds worktrees ([`Self::adding_worktrees`] or a lead's
    /// teammates' setup), since the owner's `git worktree repair` would point a real
    /// checkout at an admin folder planted with them; they are never rewritten, removed,
    /// re-permissioned, flagged, given an ACL or a time, or hard linked under a name an agent
    /// could change them through, so the owner's git keeps reading them (RV-DS241-185, rv-sa241
    /// NV-1, LINUX-TEAM-TIME-CALLS: a profile adding none denies every write, since Apple's
    /// `utimensat` sets times past `file-write-times`), and no `config.worktree` may be
    /// written. No link may be made anywhere in the git folder, in any letter case: git's
    /// `worktree prune`, which the owner's next commit runs as maintenance, empties whatever
    /// folder a link at `.git/worktrees` or `.git/worktrees/<name>` names
    /// (SECAUDIT-WORKTREE-ADMIN-PLANT). Each
    /// name below the project root, and each extra worktree's `.git` link, matches in any
    /// letter case, as in `clone_store_denies`, since the case-insensitive disk finds it
    /// however it is typed (DEC-123 rv-secreview L2, RV-DS241-15).
    /// Input: the policy. Output: the rule text.
    fn git_link_denies(&self) -> String {
        let project = self.project_root.display();
        let pattern = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let worktrees = any_case("worktrees");
        let state = any_case(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let admin = format!("{pattern}/{git}/{worktrees}/[^/]+");
        let checkouts = format!("{pattern}(/{state})?/{worktrees}/[^/]+");
        let mut moves = format!(
            "(deny file-write-unlink\n  (literal \"{project}\")\n  (regex #\"^{pattern}/{git}/{worktrees}$\")\n  (regex #\"^{pattern}(/{state})?/{worktrees}$\")\n  (regex #\"^{admin}$\")\n  (regex #\"^{checkouts}$\")"
        );
        let denied = if self.adds_worktrees || self.setup.is_some() {
            "file-write-unlink file-write-data file-write-mode file-write-flags file-write-acl file-write-times file-link"
        } else {
            "file-write* file-link"
        };
        let [commondir, gitdir] = ADMIN_LINKS.map(any_case);
        let mut links = format!(
            "(deny {denied}\n  (regex #\"^{admin}/({commondir}|{gitdir})$\")\n  (regex #\"^{checkouts}/{git}$\")"
        );
        for worktree in &self.worktrees {
            moves.push_str(&format!("\n  (literal \"{}\")", worktree.display()));
            links.push_str(&format!(
                "\n  (regex #\"^{}/{git}$\")",
                seatbelt_pattern(worktree)
            ));
        }
        let [_, worktree_config] = ADMIN_WRITES.map(any_case);
        let text = format!(
            "{moves})\n{links})\n(deny file-write*\n  (regex #\"^{admin}/{worktree_config}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{pattern}/{git}/\") (vnode-type SYMLINK)))\n"
        );
        assert_eq!(
            text.matches("(regex #\"^/").count(),
            8 + self.worktrees.len()
        );
        assert!(text.contains("(vnode-type SYMLINK)"));
        assert!(!text.contains("/.crazyagent/worktrees\""));
        text
    }

    /// The Seatbelt rule that keeps the submodule store out of agent hands. Git keeps a
    /// submodule's own repository, with the config and hooks a later git would run, under
    /// `.git/modules`; no agent may write there, in the common git folder or a worktree
    /// admin folder, so none can plant a submodule whose metadata the engine or the owner
    /// then enters. The `.gitmodules` file and gitlinks stay writable tracked content: the
    /// engine's own git ignores them with `--ignore-submodules` and never recurses. The
    /// name matches in any letter case ([`any_case`], the long s included), since the
    /// project disk may be case insensitive. Input: the policy. Output: the rule text.
    fn submodule_denies(&self) -> String {
        let pattern = seatbelt_pattern(&self.project_root);
        let dot_git = any_case(".git");
        let worktrees = any_case("worktrees");
        let modules = any_case("modules");
        let text = format!(
            "(deny file-write*\n  {})\n",
            at_or_below(&format!(
                "{pattern}/{dot_git}(/{worktrees}/[^/]+)?/{modules}"
            ))
        );
        assert_eq!(text.matches("(regex #\"^/").count(), 1);
        text
    }

    /// The Seatbelt rule that keeps an agent from planting a nested git repository inside a
    /// writable working tree, which the owner's own unsandboxed git (its terminal or the
    /// V2 IDE) would later enter as a submodule and run under the agent's own config (SM1).
    /// No entry named `.git`, in every spelling a case-insensitive volume finds ([`any_case`]),
    /// as a folder, file or link, may be created below a writable working tree, except that
    /// tree's own top-level `.git` and, only in a profile built by [`Self::adding_worktrees`],
    /// the engine's worktree links, given back by their exact name in either case, which any
    /// other process could point at git data of its own (SECAUDIT-WORKTREE-ADMIN-PLANT).
    /// The project's own `.git` and each worktree's own `.git` sit
    /// at a tree's root, not below it, so the `.+/` guard never matches them. A whole folder
    /// already holding a `.git` could once be renamed in as a path-blind move Seatbelt
    /// cannot inspect; that source is removed by also denying `.git` at any depth, top level
    /// included, in every writable root that is not a working tree ([`Self::git_deny_roots`],
    /// the owner's temp folder and shared writable folders included), so no agent can create
    /// a repository anywhere to rename from (SM1-R, RV-DS241-16). The only `.git` entries in agent roots are engine-made links to exactly
    /// `/dev/null`: the uv markers in a tool cache's buckets (T8 C1), which an agent cannot
    /// make, remove or point elsewhere, and which, moved with their bucket into a tree, git
    /// never enters or stages. Input: the policy. Output: the rule text.
    fn nested_git_denies(&self) -> String {
        let dot_git = any_case(".git");
        let own_git = "\\.[gG][iI][tT]";
        let (working, other) = self.git_deny_roots();
        let mut nested = String::new();
        for root in &working {
            nested.push_str(&format!(
                "\n      {}",
                at_or_below(&format!("{}/.+/{dot_git}", seatbelt_pattern(root)))
            ));
        }
        for root in &other {
            nested.push_str(&format!("\n      {}", any_depth_git(root)));
        }
        let pattern = seatbelt_pattern(&self.project_root);
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut exempt = String::new();
        if self.adds_worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{checkouts}/{own_git}$\"))"
            ));
        }
        if let Some(clone) = &self.clone {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(clone),
                regex_escape(crate::agent_git::TEAMMATES_FOLDER)
            ));
        }
        if let Some(setup) = &self.setup {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(&setup.folder)
            ));
        }
        for worktree in &self.worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (literal \"{}\"))",
                worktree.join(".git").display()
            ));
        }
        let text =
            format!("(deny file-write*\n  (require-all\n    (require-any{nested}){exempt}))\n");
        assert!(
            text.matches("(regex #\"^/").count()
                == working.len()
                    + other.len()
                    + usize::from(self.adds_worktrees)
                    + usize::from(self.clone.is_some())
                    + usize::from(self.setup.is_some())
        );
        assert!(text.ends_with(")\n"));
        text
    }

    /// The Seatbelt rule that keeps an agent from writing what git reads ancestry or objects
    /// from beside the commits, in every letter case the volume folds together (`any_case`,
    /// the Kelvin sign and long s included): a commit-graph file, single or split, and an
    /// `alternates` or `http-alternates` file in the project's object store or the one its
    /// checkout shares (land-19 r1 M1, r2 L1), and the project's `.git/info` (which holds
    /// `grafts`) and `.git/shallow` (land-19 r2 M1). A forged graph or graft could claim an
    /// agent commit is on the landing tip. This is the only rule on those names, and it holds
    /// a case variant made where the name does not exist yet, as after a clone or
    /// `init --template=`, which git then reads through the case-insensitive volume. Engine
    /// git reads no graph or graft; this also keeps the owner's own git from being handed one.
    /// Input: the policy. Output: the rule text.
    fn ancestry_denies(&self) -> String {
        let git = format!(
            "{}/{}",
            seatbelt_pattern(&self.project_root),
            any_case(".git")
        );
        let mut stores = vec![format!("{git}/{}", any_case("objects"))];
        if let Some(checkout) = &self.checkout {
            stores.push(seatbelt_pattern(&checkout.objects));
        }
        let info = any_case("info");
        let mut text = format!(
            "(deny file-write*\n  (regex #\"^{git}/({info}|{})$\")\n  (regex #\"^{git}/{info}/\")",
            any_case("shallow")
        );
        let [alternates, http_alternates] = STORE_ALTERNATES.map(any_case);
        let graph = format!("{}{}?", any_case("commit-graph"), any_case("s"));
        for store in &stores {
            text.push_str(&format!(
                "\n  (regex #\"^{store}/{info}/({graph}|{alternates}|{http_alternates})$\")\n  (regex #\"^{store}/{info}/{graph}/\")"
            ));
        }
        text.push_str(")\n");
        assert_eq!(text.matches("(regex #\"^/").count(), 2 * stores.len() + 2);
        text
    }

    /// Splits the agent's writable roots into the working-tree roots, where the root's own
    /// top-level `.git` is a legitimate repository link the engine adds, and every other
    /// writable root, where no `.git` may ever appear. The working trees are the project
    /// folder, worktree, clone or setup folder the agent works in and its extra worktrees;
    /// every other root comes from [`Self::writable_roots`] itself, so a root granted there
    /// is under the rule at once: an admin folder, object store or setup records, home, temp
    /// or scratch, a team folder a native lead shares with its teammates, the tool cache (T8,
    /// whose only `.git` entries are the engine's `/dev/null` links), the owner's temp folder
    /// and every shared writable folder (RV-DS241-16). A `.git` anywhere below a working
    /// tree, and a `.git` at any depth (top level included) in every other root, is denied so
    /// no agent can create a repository the owner's git enters there or rename one whole
    /// into a worktree (OP-9 r2, SM1-R). Output: the working roots and the other roots.
    fn git_deny_roots(&self) -> (Vec<&Path>, Vec<&Path>) {
        assert!(self.confinements() <= 1);
        let mut working: Vec<&Path> = vec![match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => checkout.worktree.as_path(),
            (None, Some(clone), _) => clone.as_path(),
            (None, None, Some(setup)) => setup.folder.as_path(),
            (None, None, None) => self.project_root.as_path(),
        }];
        working.extend(self.worktrees.iter().map(PathBuf::as_path));
        let other: Vec<&Path> = self
            .writable_roots()
            .map(PathBuf::as_path)
            .filter(|root| !working.contains(root))
            .collect();
        assert!(
            working
                .iter()
                .all(|root| self.writable_roots().any(|writable| writable == root))
        );
        assert!(other.len() >= 2);
        (working, other)
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses. A profile holding a string Seatbelt
    /// cannot read, or longer than [`PROFILE_BYTES_MAX`], is refused as `Invalid`, never written.
    /// An agent home first gets the folders its profile keeps it from making
    /// ([`make_config_parents`]), unless it lies outside the engine's data root
    /// ([`Self::anchor_home`]).
    #[cfg(unix)]
    pub fn install(&self, path: &Path) -> Result<(), SandboxError> {
        let parent = path
            .parent()
            .filter(|parent| clean_absolute(parent) && parent.is_dir())
            .ok_or_else(|| invalid("profile directory must exist"))?;
        let parent = parent.canonicalize()?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.ends_with(".sb") && name.len() <= 128)
            .ok_or_else(|| invalid("profile name must end in .sb"))?;
        let target = parent.join(name);
        if self.writable_roots().any(|root| target.starts_with(root)) {
            return Err(invalid(
                "profile must live outside the sandbox's writable roots",
            ));
        }
        if !self.owner_config && self.engine_home != Some(false) {
            make_config_parents(&self.agent_home, self.engine_home == Some(true))?;
        }
        let profile = self.profile();
        if !sbpl_overlong(&profile).is_empty() {
            return Err(invalid(
                "a path or name in the sandbox profile is longer than Seatbelt reads",
            ));
        }
        if profile.len() > PROFILE_BYTES_MAX {
            return Err(invalid("the sandbox profile is larger than its 64 KiB cap"));
        }
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&staging)?;
        let written = file
            .write_all(profile.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&staging, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    }

    /// Refuses to write a Seatbelt profile, which Windows cannot load; Windows agents start
    /// through [`InstalledSandbox`] and the AppContainer boundary. Takes the path, unused.
    /// Returns `Unavailable` with [`windows::SEATBELT_MISSING`].
    #[cfg(target_os = "windows")]
    pub fn install(&self, _path: &Path) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable(windows::SEATBELT_MISSING.into()))
    }

    /// A command that runs `program` under the installed profile with a cleared
    /// environment: HOME and TMPDIR point at the agent's own dirs, and the
    /// process leads its own group so the whole tree can be stopped.
    #[cfg(unix)]
    pub fn command(&self, profile: &Path, program: &Path) -> Command {
        let base = self.base_env(false);
        let mut command = agent_command(
            profile,
            program,
            &borrowed(&base),
            std::iter::empty::<(&OsStr, Option<&OsStr>)>(),
        )
        .expect("the base agent environment holds no owner secret");
        command.current_dir(&self.project_root);
        command
    }

    /// The variables every command this sandbox builds starts with: HOME, TMPDIR (the short
    /// temp folder when granted, else the temp dir), PATH with the owner's rust proxies after
    /// any [`Self::allow_node_for`] folder, the C locale and [`toolchains::agent_env`]. Only a
    /// harness's PATH starts with the agent's own `getconf` and `git`
    /// ([`toolchains::with_agent_git`]). Takes whether it is a harness. Returns the variables.
    fn base_env(&self, harness: bool) -> Vec<(&'static str, OsString)> {
        let homes = toolchains::ToolHomes::at(&self.owner_home);
        let tmp = self
            .owner_tmp
            .as_ref()
            .or(self.scratch.as_ref())
            .unwrap_or(&self.agent_tmp);
        let path = toolchains::agent_path(&homes, AGENT_PATH);
        let path = match self.node_bin.as_deref().and_then(Path::to_str) {
            Some(bin) => format!("{bin}:{path}"),
            None => path,
        };
        let path = if harness {
            toolchains::with_agent_git(&self.agent_home, path)
        } else {
            path
        };
        let mut env = vec![
            ("HOME", self.agent_home.clone().into_os_string()),
            ("TMPDIR", tmp.clone().into_os_string()),
            ("PATH", OsString::from(path)),
            ("LANG", OsString::from("C")),
            ("LC_ALL", OsString::from("C")),
        ];
        env.extend(toolchains::agent_env(&self.agent_home, &homes));
        if let Some((cache, uv)) = &self.tool_cache {
            if *uv {
                env.push((TOOL_CACHE_ENV[0], cache.join("uv").into_os_string()));
            }
            env.push((TOOL_CACHE_ENV[1], cache.join("pnpm-store").into_os_string()));
        }
        env
    }

    /// Rebuilds a harness launch command (absolute program, arguments,
    /// explicitly set environment, working directory inside the project) so it
    /// runs under the installed profile. HOME and TMPDIR always point at the
    /// agent's own dirs, whatever the harness set.
    pub fn wrap(&self, profile: &Path, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(true);
        let mut command = agent_command(profile, program, &borrowed(&base), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert!(starts_at_boundary(command.get_program()));
        Ok(command)
    }

    /// Rebuilds a harness launch command for a Full access agent (DEC-102) as [`Self::wrap`]
    /// does, with the same checks and environment, but started by [`unconfined_command`]
    /// with no Seatbelt profile. Input: the harness command. Output: the command, or
    /// `Invalid` for an unsafe command.
    #[cfg(unix)]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(false);
        let mut command = unconfined_command(program, &borrowed(&base), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert_ne!(command.get_program(), SANDBOX_EXEC);
        Ok(command)
    }

    /// Refuses a Full access harness command on Windows, where the level is unavailable.
    /// Input: the harness command. Output: always `Unavailable`.
    #[cfg(not(unix))]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let _ = harness;
        Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()))
    }

    fn harness_start<'a>(
        &'a self,
        harness: &'a Command,
    ) -> Result<(&'a Path, &'a Path), SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness.get_current_dir().unwrap_or(&self.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.project_root)
                .chain(self.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        Ok((program, cwd))
    }
}

/// One operating system's process boundary for a project agent: Seatbelt on macOS, Landlock
/// or bubblewrap on Linux, AppContainer with a job object on Windows. Each keeps the
/// [`ProjectSandbox`] policy: writes only in its writable roots minus the protected paths,
/// reads of system paths and its roots, and network per [`SandboxNetwork`].
pub trait Sandbox: Send + Sync {
    /// Rebuilds a harness launch command (absolute program, arguments, set environment,
    /// working directory inside the project or a worktree) to start inside the boundary.
    /// Output: the wrapped command, `Invalid` for an unsafe command, or `Unavailable` when
    /// this machine cannot hold the boundary.
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError>;
}

/// What a run's stop checks, once its process group is killed, of the processes the run
/// left outside that group: nothing where its boundary ends them all, its Full access
/// lineage, or a sandboxed lineage where a confined process that starts its own session
/// outlives the group kill (LINUX-KILL-GROUP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageCheck {
    Unneeded,
    FullAccess,
    Sandboxed,
}

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux, AppContainer with a job
/// object on Windows.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
    unconfined: bool,
    calls: LinuxSupervisor,
    #[cfg(target_os = "linux")]
    linux: linux::LinuxSandbox,
    #[cfg(target_os = "windows")]
    windows: windows::WindowsSandbox,
}

impl InstalledSandbox {
    /// Installs the sandbox's profile at `profile` (see [`ProjectSandbox::install`]); on
    /// Linux, picks the Landlock helper next to the engine's executable or bubblewrap; on
    /// Windows, checks the helper next to the engine's executable first, then writes its
    /// boundary profile with the engine's grants folder (see
    /// [`windows::WindowsSandbox::install`]). `calls` and `strict` are the owner's "Linux call
    /// supervisor" and [`LinuxStrict`] choices, which every launch must pass and only Linux
    /// acts on. Output: the sandbox bound to that profile, the install refusal, or
    /// `Unavailable` where this system has no boundary.
    pub fn install(
        sandbox: ProjectSandbox,
        profile: &Path,
        calls: LinuxSupervisor,
        strict: LinuxStrict,
    ) -> Result<Self, SandboxError> {
        #[cfg(not(target_os = "linux"))]
        let _ = strict;
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.hidden.is_empty() || !sandbox.kept_out.is_empty() || sandbox.open_tops.is_some()
        {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.shared_writes.is_empty() {
            return Err(SandboxError::Unavailable(
                SHARED_WRITES_NEED_SEATBELT.into(),
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if sandbox
            .agent_files
            .is_some_and(|rule| rule.choice != policy::IdeAgentFiles::AllowAll)
        {
            return Err(SandboxError::Unavailable(AGENT_FILES_NEED_SEATBELT.into()));
        }
        #[cfg(target_os = "windows")]
        {
            let helper = windows::existing_helper(&windows::helper_beside_engine()?)?;
            let grants = windows::grants_folder()?;
            let windows =
                windows::WindowsSandbox::install(sandbox.clone(), &helper, profile, &grants)?;
            assert!(profile.is_file());
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
                calls,
                windows,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            sandbox.install(profile)?;
            assert!(profile.is_file());
            #[cfg(target_os = "linux")]
            let linux = {
                let helper = std::env::current_exe()?.with_file_name(linux::HELPER_NAME);
                let data = crate::tools::engine_root();
                linux::LinuxSandbox::detect(sandbox.clone(), &helper)?.supervise(calls, data)
            };
            #[cfg(target_os = "linux")]
            if strict == LinuxStrict::On && !linux.holds_fully() {
                return Err(SandboxError::Unavailable(LINUX_NOT_FULL_LINE.into()));
            }
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
                calls,
                #[cfg(target_os = "linux")]
                linux,
            })
        }
    }

    /// Marks this boundary as a Full access project's (DEC-102): every command it wraps then
    /// starts with no Seatbelt profile (see [`ProjectSandbox::wrap_unconfined`]), keeping
    /// the agent environment, the worktree start and the process group. Only macOS runs
    /// Full access; elsewhere [`Self::wrap`] refuses an unconfined boundary. Output: the
    /// marked boundary.
    pub fn unconfined(mut self) -> Self {
        self.unconfined = true;
        self
    }

    /// Whether this boundary runs its commands with no Seatbelt profile.
    pub fn is_unconfined(&self) -> bool {
        self.unconfined
    }

    /// What a run's stop inside this boundary checks of the processes it left outside its
    /// group: its Full access lineage when unconfined, a sandboxed one on Linux where the
    /// backend leaves a process in a session of its own running (`LinuxSandbox::ends_every_session`),
    /// else nothing, as the warden, a process namespace or the job ends them all. Output: the
    /// check.
    pub fn lineage_check(&self) -> LineageCheck {
        if self.unconfined {
            return LineageCheck::FullAccess;
        }
        #[cfg(target_os = "linux")]
        if !self.linux.ends_every_session() {
            return LineageCheck::Sandboxed;
        }
        LineageCheck::Unneeded
    }

    /// The program every wrapped command starts: `sandbox-exec` on macOS, or the session
    /// shim's `perl` for a Full access boundary, the Landlock helper or bwrap on Linux, the
    /// boundary helper on Windows.
    pub fn launcher(&self) -> &Path {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            return Path::new(SESSION_SHIM[0]);
        }
        #[cfg(target_os = "linux")]
        {
            self.linux.launcher()
        }
        #[cfg(target_os = "windows")]
        {
            self.windows.launcher()
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Path::new(SANDBOX_EXEC)
        }
    }

    /// The sandbox this profile was generated from.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed profile file: the one `sandbox-exec` loads, or the boundary profile the
    /// Windows helper reads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// The owner's "Linux call supervisor" choice this boundary was installed with; only
    /// Linux acts on it.
    pub fn linux_supervisor(&self) -> LinuxSupervisor {
        self.calls
    }

    /// Rebuilds a harness command to run inside this boundary; see [`ProjectSandbox::wrap`]
    /// on macOS, [`linux::LinuxSandbox`] on Linux and [`windows::WindowsSandbox`] on Windows.
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if self.unconfined {
            return Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()));
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            let command = self.sandbox.wrap_unconfined(harness)?;
            assert_eq!(command.get_program(), self.launcher().as_os_str());
            return Ok(command);
        }
        #[cfg(target_os = "linux")]
        let command = Sandbox::wrap(&self.linux, harness)?;
        #[cfg(target_os = "windows")]
        let command = Sandbox::wrap(&self.windows, harness)?;
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let command = {
            let command = self.sandbox.wrap(&self.profile, harness)?;
            assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
            command
        };
        assert_eq!(command.get_program(), self.launcher().as_os_str());
        Ok(command)
    }
}

impl Sandbox for InstalledSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        InstalledSandbox::wrap(self, harness)
    }
}

impl Checkout {
    /// The Seatbelt rules that keep a confined agent from adding to or changing the git
    /// objects the owner and the other agents share: no write in `info/` or `pack/`, nor of
    /// either folder entry, in any letter case, no new, moved or removed fan-out folder, no
    /// new object file by any route (create, rename or link) and no rewrite, removal or
    /// replacement of one, no hard link of one under a name it could change it through
    /// (rv-sa241 NV-1), no symlink anywhere in the store, and no mode, flag or ACL change
    /// of anything in it, which would keep the owner's git and every landing from reading an
    /// object (RV-DS241-185). Git reads a loose object before a packed one without checking
    /// its bytes, so any object an agent could add would stand in for the owner's own;
    /// object writes are the engine's, at turn end.
    /// Seatbelt regexes take no `{n}` counts, so the hex digits repeat. Input: the checkout.
    /// Output: the rule text.
    fn object_denies(&self) -> String {
        const HEX: &str = "[0-9a-fA-F]";
        let objects = self.objects.display();
        let pattern = seatbelt_pattern(&self.objects);
        let held = at_or_below(&format!(
            "{pattern}/({}|{})",
            any_case("info"),
            any_case("pack")
        ));
        let text = format!(
            "(deny file-write*\n  {held})\n(deny file-write-unlink\n  (literal \"{objects}\")\n  (regex #\"^{pattern}/{HEX}{HEX}$\"))\n(deny file-write-unlink file-write-data file-link\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (regex #\"^{pattern}/{HEX}{HEX}$\")\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (require-all (subpath \"{objects}\") (vnode-type SYMLINK)))\n(deny file-write-mode file-write-flags file-write-acl\n  (subpath \"{objects}\"))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 5);
        assert!(text.contains("(vnode-type SYMLINK)"));
        assert!(text.contains("(deny file-write-mode file-write-flags file-write-acl"));
        assert!(text.contains("(deny file-write-unlink file-write-data file-link"));
        text
    }
}

/// Reads one git link file the engine's own `git worktree add` wrote: a plain file of at
/// most 2 KiB holding `prefix`, one absolute or relative path and a newline. Input: the
/// file and its prefix. Output: the path text, or `Invalid` for any other file.
fn git_link(path: &Path, prefix: &str) -> Result<String, SandboxError> {
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

/// Escapes a plain absolute path for a Seatbelt regex, so every character matches itself.
/// Input: a path that passes `clean_absolute`, so it holds no quote or backslash.
/// Output: the pattern text, without anchors.
pub(crate) fn seatbelt_pattern(path: &Path) -> String {
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
fn app_exec_denies<'a>(
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
fn clone_store_denies(clone: &Path) -> String {
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
/// Inputs: the clone and the lead's branch. Output: the rule text.
fn teammates_clone_denies(clone: &Path, lead: Option<&str>) -> String {
    let path = clone.display();
    let root = seatbelt_pattern(clone);
    let folder = crate::agent_git::TEAMMATES_FOLDER;
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

/// Spells plain text as a Seatbelt regex that matches every name a case-insensitive volume
/// finds under it: each letter in both cases with every code point that folds onto it or
/// onto a run of two or three letters ([`spelled`]), so the rule never leans on how Seatbelt
/// folds names itself (SA vid 202). A stretch with more than [`SPELLINGS_MAX`] spellings, a
/// long run of `s` or `f` in path text, is spelled as the [`superset`], which matches more,
/// so the result holds in a deny. Input: text without a quote or backslash. Output: the
/// pattern text, without anchors.
pub(crate) fn any_case(text: &str) -> String {
    spelled(&regex_escape(text), false)
}

/// A Seatbelt regex fragment matched as [`any_case`] matches plain text, every character
/// that is not a letter, regex syntax included, staying as written; a letter run next to a
/// wildcard also takes a fold reaching into it ([`spelled_run`]). Every stretch is listed
/// whole, so a rule that gives access back stays exact. Input: a fragment with no bracket
/// class or quote. Output: the pattern.
pub(crate) fn fold_case(pattern: &str) -> String {
    assert!(!pattern.contains('[') && !pattern.contains('"'));
    spelled(pattern, true)
}

/// The most spellings one stretch of letters between two places no fold spans lists one by
/// one; past it path text takes the [`superset`] (B5 r2).
const SPELLINGS_MAX: usize = 8;

/// Spells text run by run from [`crate::instruction_files::ASCII_FOLDS`]: each run of ASCII
/// letters through [`spelled_run`], every other character through [`spelled_char`]. In a
/// regex fragment a letter a quantifier follows stands alone, and no fold may join it to a
/// letter beside it. Inputs: the text and whether it is a regex fragment. Output: the pattern.
fn spelled(text: &str, regex: bool) -> String {
    let characters: Vec<char> = text.chars().collect();
    let letter = |at: usize| characters.get(at).is_some_and(char::is_ascii_alphabetic);
    let quantified =
        |at: usize| regex && matches!(characters.get(at + 1), Some('?' | '*' | '+' | '{'));
    let mut pattern = String::with_capacity(text.len() * 8);
    let mut at = 0;
    while at < characters.len() {
        if !letter(at) || quantified(at) {
            assert!(!letter(at) || !joins_fold(&characters, at));
            pattern.push_str(&spelled_char(characters[at]));
            at += 1;
            continue;
        }
        let start = at;
        while letter(at) && !quantified(at) {
            at += 1;
        }
        let before = regex && start > 0 && matches!(characters[start - 1], '*' | '+');
        let after = regex
            && characters.get(at) == Some(&'.')
            && matches!(characters.get(at + 1), Some('*' | '+'));
        pattern.push_str(&spelled_run(&characters[start..at], before, after, regex));
    }
    pattern
}

/// Tells whether a fold of two or three letters would join the letter at `at` to the letter
/// before it or to the one after its quantifier. Inputs: the characters and the letter's
/// place. Output: true when one would.
fn joins_fold(characters: &[char], at: usize) -> bool {
    let joined = |left: Option<&char>, right: Option<&char>| match (left, right) {
        (Some(left), Some(right)) if left.is_ascii_alphabetic() && right.is_ascii_alphabetic() => {
            let pair = format!("{left}{right}").to_ascii_lowercase();
            crate::instruction_files::ASCII_FOLDS
                .iter()
                .any(|(_, folded)| folded.contains(pair.as_str()))
        }
        _ => false,
    };
    let previous = at.checked_sub(1).and_then(|before| characters.get(before));
    joined(previous, characters.get(at)) || joined(characters.get(at), characters.get(at + 2))
}

/// Spells one character as a case-insensitive volume finds it: an ASCII letter in both
/// cases, with every code point that folds onto it alone, the non-ASCII ones in a group so
/// each matches its whole UTF-8 sequence (LL2S-2). Input: the character. Output: the pattern.
fn spelled_char(character: char) -> String {
    let lower = character.to_ascii_lowercase();
    let mut alternatives = vec![lower.to_string()];
    if character.is_ascii_alphabetic() {
        alternatives.push(character.to_ascii_uppercase().to_string());
    }
    for (point, folded) in crate::instruction_files::ASCII_FOLDS {
        if folded.len() == 1 && folded.starts_with(lower) {
            alternatives.push(point.to_string());
        }
    }
    if alternatives.len() == 1 {
        character.to_string()
    } else if alternatives
        .iter()
        .all(|alternative| alternative.is_ascii())
    {
        format!("[{}]", alternatives.concat())
    } else {
        format!("({})", alternatives.join("|"))
    }
}

/// Spells one run of ASCII letters with every fold of two or three letters inside it and,
/// next to a wildcard, every fold reaching past its first or last letter into the wildcard.
/// The run splits where no fold spans, each stretch listing all its spellings, or past
/// [`SPELLINGS_MAX`] taking the [`superset`] unless it must stay exact. Inputs: the letters,
/// whether a wildcard comes before and after them and whether every stretch must be listed
/// whole. Output: the pattern text.
fn spelled_run(run: &[char], before: bool, after: bool, exact: bool) -> String {
    let lower: String = run.iter().map(char::to_ascii_lowercase).collect();
    let count = run.len();
    let mut folds: Vec<(usize, usize, char)> = Vec::new();
    for (point, folded) in crate::instruction_files::ASCII_FOLDS {
        let width = folded.len();
        if width < 2 {
            continue;
        }
        for at in (0..count).filter(|&at| lower[at..].starts_with(folded)) {
            folds.push((at, at + width, point));
        }
        for split in 1..width {
            if before && lower.starts_with(&folded[split..]) {
                folds.push((0, width - split, point));
            }
            if after && lower.ends_with(&folded[..split]) {
                folds.push((count - split, count, point));
            }
        }
        for inner in 1..width.saturating_sub(count) {
            if before && after && folded[inner..inner + count] == lower {
                folds.push((0, count, point));
            }
        }
    }
    folds.sort_unstable();
    folds.dedup();
    let mut pieces: Vec<(usize, usize, String)> = (0..count)
        .map(|at| (at, at + 1, spelled_char(run[at])))
        .collect();
    pieces.extend(
        folds
            .into_iter()
            .map(|(start, end, point)| (start, end, point.to_string())),
    );
    let cuts: Vec<usize> = (0..=count)
        .filter(|&cut| {
            !pieces
                .iter()
                .any(|(start, end, _)| *start < cut && cut < *end)
        })
        .collect();
    let mut pattern = String::new();
    for stretch in cuts.windows(2) {
        let (at, to) = (stretch[0], stretch[1]);
        let (count, fewest) = tally(&pieces, at, to);
        assert!(count >= 1 && (1..=to - at).contains(&fewest));
        if count > SPELLINGS_MAX {
            assert!(!exact, "a pattern stretch lists every spelling");
            pattern.push_str(&superset(&pieces, at, to, fewest));
            continue;
        }
        let mut spellings = Vec::new();
        spell_stretch(&pieces, at, to, String::new(), &mut spellings);
        assert_eq!(spellings.len(), count);
        if let [only] = spellings.as_slice() {
            pattern.push_str(only);
        } else {
            pattern.push_str(&format!("({})", spellings.join("|")));
        }
    }
    pattern
}

/// Counts the spellings of the letters from `at` to `to` that the pieces give, up to one
/// past [`SPELLINGS_MAX`], and the fewest pieces one spelling takes, place by place from the
/// end, so no long run is listed to be counted. Inputs: the pieces as start, end and
/// pattern, and the stretch. Output: the count and the fewest pieces.
fn tally(pieces: &[(usize, usize, String)], at: usize, to: usize) -> (usize, usize) {
    assert!(at < to);
    let mut counts = vec![0; to - at + 1];
    let mut fewest = vec![0; to - at + 1];
    counts[to - at] = 1;
    for place in (at..to).rev() {
        let ends: Vec<usize> = pieces
            .iter()
            .filter(|(start, end, _)| *start == place && *end <= to)
            .map(|(_, end, _)| end - at)
            .collect();
        assert!(!ends.is_empty());
        counts[place - at] = ends
            .iter()
            .fold(0, |sum, &end| (sum + counts[end]).min(SPELLINGS_MAX + 1));
        fewest[place - at] = 1 + ends.iter().map(|&end| fewest[end]).min().unwrap_or(0);
    }
    (counts[0], fewest[0])
}

/// Spells the letters from `at` to `to` as one of the stretch's pieces at each place, at
/// least `fewest` places and at most one per letter, the extra places optional, since
/// Seatbelt reads no `{m,n}` count (probe 2026-11-15). It matches every spelling and more,
/// so it holds only in a deny. Inputs: the pieces, the stretch and its fewest pieces.
/// Output: the pattern.
fn superset(pieces: &[(usize, usize, String)], at: usize, to: usize, fewest: usize) -> String {
    let mut alternatives: Vec<&str> = Vec::new();
    for (start, end, piece) in pieces {
        if at <= *start && *end <= to && !alternatives.contains(&piece.as_str()) {
            alternatives.push(piece);
        }
    }
    let place = format!("({})", alternatives.join("|"));
    let optional = format!("{place}?");
    let pattern = place.repeat(fewest) + &optional.repeat(to - at - fewest);
    assert!(fewest <= to - at && pattern.len() <= (to - at) * optional.len());
    pattern
}

/// Lists every spelling of the letters from `at` to `to` that the pieces give, in order.
/// Inputs: the pieces as start, end and pattern, the stretch, the spelling so far and the
/// list it adds to. Output: none; the list grows.
fn spell_stretch(
    pieces: &[(usize, usize, String)],
    at: usize,
    to: usize,
    so_far: String,
    spellings: &mut Vec<String>,
) {
    if at == to {
        spellings.push(so_far);
        return;
    }
    assert!(at < to);
    for (start, end, piece) in pieces {
        if *start == at && *end <= to {
            assert!(start < end);
            spell_stretch(pieces, *end, to, format!("{so_far}{piece}"), spellings);
        }
    }
}

/// Whether `name` can be one top-level name [`ProjectSandbox::open_only_tops`] keeps open:
/// one path part, never `.` or `..`, at most 255 bytes, with no control character, quote or
/// backslash, so it sits in a Seatbelt pattern as written. Takes the name. Returns the answer.
pub(crate) fn plain_top(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '"' | '\\' | '\''))
}

/// Seatbelt regex bodies `{prefix}(a|b|...){suffix}` that together match each of
/// `alternatives`, packed whole, in order, so each body fits [`SBPL_STRING_MAX`] bytes; one
/// alternative too long beside the prefix keeps a body of its own, which [`sbpl_overlong`]
/// then names. Inputs: the prefix, the regex fragments and the suffix. Output: the bodies.
fn regex_bodies(prefix: &str, alternatives: &[String], suffix: &str) -> Vec<String> {
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
pub(crate) fn sbpl_overlong(profile: &str) -> Vec<&str> {
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

/// Escapes plain text for a Seatbelt regex, so every character matches itself. Input: text
/// without a quote or backslash. Output: the pattern text, without anchors.
fn regex_escape(text: &str) -> String {
    assert!(!text.contains('"') && !text.contains('\\'));
    let mut pattern = String::with_capacity(text.len() * 2);
    for character in text.chars() {
        if ".^$*+?()[]{}|".contains(character) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

fn borrowed<'a>(env: &'a [(&'static str, OsString)]) -> Vec<(&'static str, &'a OsStr)> {
    env.iter()
        .map(|(name, value)| (*name, value.as_os_str()))
        .collect()
}

/// The Seatbelt profile the engine primes a uv tool cache under (T8 C3): the system's
/// read-only roots and the uv binary to read and run, the fresh priming folder as the only
/// place to write, `/dev/null` and the random devices, no network and no user folder.
/// Inputs: the canonical priming folder and the canonical uv binary. Output: the text.
#[cfg(target_os = "macos")]
pub(crate) fn priming_profile(folder: &Path, uv: &Path) -> String {
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

fn invalid(reason: &str) -> SandboxError {
    SandboxError::Invalid(reason.into())
}

/// Makes sure a store a profile keeps read-only is a plain folder the engine made: creates
/// it empty, mode 0700 on unix, or keeps an entry already there only when it is a real
/// folder, never a file or a link. Disk state is input here, so a wrong entry is an error.
/// Input: the store path. Output: Ok, or `InvalidData` for any other entry at the path.
pub(crate) fn ensure_store(path: &Path) -> io::Result<()> {
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

/// The owner's home folder from the user profile, canonical when it resolves, which
/// no sandbox root may contain. Takes nothing. Returns the folder, or `Invalid` when the
/// profile names none or a relative one.
#[cfg(target_os = "windows")]
fn owner_home() -> Result<PathBuf, SandboxError> {
    let home = std::env::home_dir().ok_or_else(|| invalid("owner account is unknown"))?;
    if !clean_absolute(&home) {
        return Err(invalid("owner home must be absolute"));
    }
    Ok(canonical(&home).unwrap_or(home))
}

#[cfg(unix)]
fn owner_home() -> Result<PathBuf, SandboxError> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|error| SandboxError::Io(io::Error::from(error)))?
        .ok_or_else(|| invalid("owner account is unknown"))?;
    let home = user.dir;
    if !clean_absolute(&home) {
        return Err(invalid("owner home must be absolute"));
    }
    Ok(home.canonicalize().unwrap_or(home))
}

/// Names the credential files and folders the sandbox guards as they appear in a path: the
/// owner's under the home, the system's without their leading `/` and the credential names
/// no allowed root may hold. Takes nothing. Returns the names.
pub(crate) fn credential_places() -> impl Iterator<Item = &'static str> {
    OWNER_SECRET_PATHS
        .iter()
        .chain(CREDENTIAL_NAMES)
        .copied()
        .chain(
            SYSTEM_SECRET_PATHS
                .iter()
                .map(|path| path.trim_start_matches('/')),
        )
}

fn owner_secrets(owner: &Path) -> impl Iterator<Item = PathBuf> {
    OWNER_SECRET_PATHS
        .iter()
        .map(move |path| {
            path.split('/')
                .fold(owner.to_path_buf(), |at, part| at.join(part))
        })
        .chain(SYSTEM_SECRET_PATHS.iter().map(PathBuf::from))
}

/// The engine runtime's secrets: the fixed secret files and folders, plus every Unix socket
/// directly in the runtime and every runtime folder that directly holds one (owner
/// generations, runner roots, the terminal server). Input: the canonical runtime folder.
/// Output: the secret paths, or a refusal for a runtime or folder over the entry bound.
fn engine_secrets_in(runtime: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let mut secrets: Vec<PathBuf> = ENGINE_SECRET_FILES
        .iter()
        .chain(ENGINE_SECRET_FOLDERS)
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

fn clear_of_secrets(root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    if secrets
        .iter()
        .any(|secret| root.starts_with(secret) || secret.starts_with(root))
    {
        return Err(invalid(SECRET_ROOT_REFUSED));
    }
    Ok(())
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
/// follow every write allow. Input: the home. Output: the rule text.
fn launch_denies(home: &Path) -> String {
    use crate::agent_home::Harness;
    use crate::launch_context::{LOCK, STATE};
    let home = seatbelt_pattern(home);
    let skills = format!("{home}/{}", any_case("skills"));
    let folders = [
        STATE,
        crate::coord::relay::PLUGIN_FOLDER,
        "agents",
        "commands",
        "output-styles",
        "rules",
        ".agents",
        "Library/Keychains",
    ];
    let files = [
        Harness::Claude.instruction_file(),
        Harness::Codex.instruction_file(),
        "AGENTS.override.md",
        "config.toml",
        "settings.json",
        "Library/Preferences/com.apple.security.plist",
        LOCK,
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
    text.push_str(&format!("  (regex #\"^{home}/{}$\")\n", shell_startup()));
    let system = any_case(crate::launch_context::CODEX_SYSTEM_SKILLS);
    text.push_str(&format!(
        "  (regex #\"^{skills}/{}\"))\n(deny {PINNED_WRITES} (regex #\"^{skills}$\"))\n(deny file-write-flags\n  (regex #\"^{skills}/{system}$\")\n  (regex #\"^{skills}/{system}/\"))\n",
        other_than(crate::launch_context::CODEX_SYSTEM_SKILLS)
    ));
    let library = format!("{home}/{}", any_case("Library"));
    let walked: Vec<String> = crate::agent_home::pinned_entries()
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
/// open handle and follows no link. Inputs: the home and `clear`. Output: unit, or `Invalid`
/// when one cannot be made.
#[cfg(unix)]
fn make_config_parents(home: &Path, clear: bool) -> Result<(), SandboxError> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    let refused = || invalid("a config folder of the agent home could not be made");
    let folder = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = rustix::fs::open(home, folder, Mode::empty()).map_err(|_| refused())?;
    let is_folder = |name: &str| {
        rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
    };
    let remove =
        |name: &str| crate::team::cleanup::remove_home_entry(&parent, name).map_err(|_| refused());
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
/// They must follow every write allow. Input: the home. Output: the rule text.
fn agent_config_denies(home: &Path) -> String {
    let pinned = crate::agent_home::pinned_entries();
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
    let kept = crate::agent_home::kept_entries();
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
/// sh's (SA vid 12). Output: the pattern, a group without anchors.
fn shell_startup() -> String {
    let spelled = |names: &[&str]| {
        names
            .iter()
            .map(|name| any_case(name))
            .collect::<Vec<_>>()
            .join("|")
    };
    let pattern = format!(
        "(({})({})?|{})",
        spelled(&crate::agent_home::ZSH_STARTUP[..]),
        any_case(crate::agent_home::ZSH_COMPILED),
        spelled(&crate::agent_home::SH_STARTUP[..]),
    );
    assert!(pattern.len() <= SBPL_STRING_MAX / 2);
    pattern
}

/// Spells a Seatbelt regex for one path component other than `name` in any letter case,
/// with whatever follows it: a component that differs from `name` at some character, stops
/// short of it or runs past it. Input: a name of letters, digits and dots. Output: the
/// pattern text, a group without anchors.
fn other_than(name: &str) -> String {
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
fn root_pin(root: &Path) -> String {
    format!("(deny {FIXED_WRITES} (literal \"{}\"))\n", root.display())
}

/// The Seatbelt filter for a path and everything below it: one `^X(/|$)` in place of
/// `^X(/.*)?$`, whose `(/.*)` tail libsandbox fails to compile again in lower case after a
/// fold group with no bracket class or wildcard before it, as `^/x/(s|S)(/.*)?$`
/// (V241-SANDBOX-REGEX, V243-FARM2-PRODUCT-SMALL). One regex keeps the profile under its cap
/// for long roots, where an `X$` and `X/` pair did not (V243-PROFILE-CAP-ROW, DEC-195 Q6).
/// Input: the pattern after `^`, an absolute path first. Output: the filter.
pub(crate) fn at_or_below(pattern: &str) -> String {
    assert!(pattern.starts_with('/'));
    format!("(regex #\"^{pattern}(/|$)\")")
}

/// The Seatbelt filters for a `.git` at any depth below `root`, top level included, in every
/// spelling a case-insensitive volume finds ([`any_case`]): the part of [`ProjectSandbox::nested_git_denies`] for a root that is not a
/// working tree (SM1-R), as [`at_or_below`] spells it. Input: the root. Output: the filters.
fn any_depth_git(root: &Path) -> String {
    at_or_below(&format!(
        "{}/(.+/)?{}",
        seatbelt_pattern(root),
        any_case(".git")
    ))
}

/// The base write denies a profile gives again after it grants writes once more to the agent
/// home and other folders, as the IDE profile does for its server's data folders: each one
/// pinned ([`root_pin`]), no `.git` at any depth in any of them ([`any_depth_git`]), the
/// home's launch files ([`launch_denies`]) and no flag or ACL on anything in the home, which
/// the engine walks and rewrites before each start, so none keeps the owner's IDE from
/// starting (rv-sa241 b8 L1); never [`agent_config_denies`], since that home is one the
/// owner keeps ([`ProjectSandbox::keep_owner_config`], RV-DS241-188). They must follow that
/// grant, since the last match wins. Inputs: the home and the other folders. Output: the
/// rule text.
#[cfg(unix)]
pub(crate) fn regranted_denies(home: &Path, folders: &[&Path]) -> String {
    let mut text = launch_denies(home);
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

/// The Seatbelt rule that refuses reading, writing, linking or cloning any path with a part
/// made of ASCII, [`crate::instruction_files::ASCII_FOLDS`] points and at least one code
/// point HFS+ ignores in names, since such a volume opens `.g\u{200c}it/config` as
/// `.git/config` past every name deny (SA vid 202). A part in another script keeps its
/// joiners. Output: the rule, ending in a line break.
pub(crate) fn hfs_ignored_deny() -> String {
    let ignored: Vec<String> = crate::instruction_files::HFS_IGNORED
        .iter()
        .map(char::to_string)
        .collect();
    let folds: Vec<String> = crate::instruction_files::ASCII_FOLDS
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

/// Whether every deny rule comes after the profile's last write allow, since Seatbelt applies
/// the last matching rule and a deny a later allow overrides no longer holds (CTX-M1 F1).
/// Inputs: the profile text and the deny rules as written in it. Output: false when any
/// deny is missing or comes before a write allow.
pub(crate) fn denies_follow_write_allows(text: &str, denies: &[String]) -> bool {
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
fn shared_denies(folder: &Path) -> String {
    let folder = folder.display();
    format!(
        "(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type SYMLINK)))\n(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type DIRECTORY) (require-not (regex #\"{LOCK_FOLDER}\"))))\n"
    )
}

fn granted_root(root: &Path, owner: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    let depth = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if !clean_absolute(root) || depth < 2 {
        return Err(invalid("sandbox roots must be absolute, deep and plain"));
    }
    if owner.starts_with(root) {
        return Err(invalid("sandbox roots must not contain the owner home"));
    }
    for private in [".codex", ".claude"] {
        if root.starts_with(owner.join(private)) {
            return Err(invalid("sandbox roots must not be owner harness homes"));
        }
    }
    clear_of_secrets(root, secrets)?;
    if !root.is_dir() || canonical(root)? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
        })
}

/// Tells whether a Windows path is absolute and plain: a drive prefix such as `C:` (never a
/// UNC, verbatim or device prefix), then folder names joined by `\`, as
/// [`windows_clean_text`] checks. Input: the path. Output: true when plain.
#[cfg(target_os = "windows")]
fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| match component {
            Component::Prefix(prefix) => matches!(prefix.kind(), std::path::Prefix::Disk(_)),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        })
        && path.to_str().is_some_and(windows_clean_text)
}

/// Checks Windows path text on any system: a drive letter, `:\`, then nonempty folder names
/// joined by `\`, each neither `.` nor `..` nor ending in a space or dot, without control
/// characters, quotes, `/`, `:` or the characters Windows forbids in names, at most 1024
/// bytes. Input: the text. Output: true when plain.
#[cfg(any(not(unix), test))]
pub(crate) fn windows_clean_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    if text.len() > PATH_BYTES_MAX
        || bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return false;
    }
    text[3..].split('\\').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with(' ')
            && !part.ends_with('.')
            && !part.chars().any(|c| {
                c.is_control() || matches!(c, '"' | '\'' | '/' | ':' | '<' | '>' | '|' | '?' | '*')
            })
    })
}

/// The canonical form of an existing path. On Windows the verbatim `\\?\` prefix that
/// `canonicalize` adds to a drive path is removed, so the result equals the plain drive path;
/// any other verbatim form stays as it is and fails [`clean_absolute`]. Input: the path.
/// Output: the canonical path, or the I/O error.
/// The npm package folder a Node script harness sits in: `<prefix>/lib/node_modules/<name>`,
/// or `<prefix>/lib/node_modules/@scope/<name>` for a scoped package, which holds the
/// harness's own dependencies. Inputs: the prefix and the program under it. Output: the
/// folder, or none when the program is not inside one.
fn node_package(prefix: &Path, program: &Path) -> Option<PathBuf> {
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

pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    let resolved = path.canonicalize()?;
    #[cfg(target_os = "windows")]
    if let Some(plain) = resolved
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(resolved)
}

#[cfg(test)]
mod windows_path_tests {
    use super::windows_clean_text;

    #[test]
    fn a_plain_drive_path_is_clean_on_every_system() {
        for text in [
            r"C:\Users\owner\project",
            r"d:\work\agent home\tmp",
            r"C:\a",
        ] {
            assert!(windows_clean_text(text), "{text}");
        }
    }

    #[test]
    fn unc_verbatim_device_and_relative_paths_are_refused() {
        for text in [
            r"\\server\share\project",
            r"\\?\C:\project",
            r"\\?\UNC\server\share",
            r"\\.\PhysicalDrive0",
            r"C:project",
            r"project\folder",
            r"\project",
            "C:/project",
            r"C:\",
        ] {
            assert!(!windows_clean_text(text), "{text}");
        }
    }

    #[test]
    fn dots_quotes_streams_and_aliases_are_refused() {
        for text in [
            r"C:\project\..\owner",
            r"C:\project\.\context",
            r"C:\project\context.",
            r"C:\project\context ",
            r"C:\project\\context",
            r"C:\project\",
            r"C:\project\file.txt:stream",
            "C:\\project\\it's",
            "C:\\project\\\"quoted\"",
            "C:\\project\\line\nbreak",
            r"C:\project\a/b",
            r"C:\project\*",
            r"C:\project\a?b",
            r"C:\project\a|b",
        ] {
            assert!(!windows_clean_text(text), "{text:?}");
        }
    }

    #[test]
    fn a_path_longer_than_the_limit_is_refused() {
        let long = format!(r"C:\{}", "a".repeat(super::PATH_BYTES_MAX));
        assert!(!windows_clean_text(&long));
        let fits = format!(r"C:\{}", "a".repeat(super::PATH_BYTES_MAX - 3));
        assert!(windows_clean_text(&fits));
    }
}

#[cfg(all(test, unix))]
mod linux_sockets_tests {
    use super::*;
    use crate::sandbox::elf::tests::executable;

    struct Case {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }

    fn case() -> Case {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        fs::write(
            root.join("glibc"),
            executable(true, false, Some("/lib64/ld-linux-x86-64.so.2")),
        )
        .unwrap();
        fs::write(
            root.join("musl"),
            executable(true, false, Some("/lib/ld-musl-x86_64.so.1")),
        )
        .unwrap();
        fs::write(root.join("static"), executable(true, false, None)).unwrap();
        for (script, interpreter) in [
            ("on_glibc", root.join("glibc")),
            ("on_musl", root.join("musl")),
            ("on_script", root.join("on_glibc")),
            ("on_relative", PathBuf::from("node")),
        ] {
            fs::write(root.join(script), format!("#!{}\n", interpreter.display())).unwrap();
        }
        Case { _temp: temp, root }
    }

    fn sandbox(case: &Case, network: SandboxNetwork, sockets: LinuxSockets) -> ProjectSandbox {
        ProjectSandbox::new(
            &case.root.join("project"),
            &case.root.join("home"),
            &case.root.join("tmp"),
            network,
        )
        .unwrap()
        .limit_sockets(sockets)
    }

    fn model() -> SandboxNetwork {
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)])
    }

    fn local() -> SandboxNetwork {
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(8080)])
    }

    #[test]
    fn the_setting_defaults_to_tcp_only_and_keeps_its_wire_names() {
        let case = case();
        let fresh = ProjectSandbox::new(
            &case.root.join("project"),
            &case.root.join("home"),
            &case.root.join("tmp"),
            model(),
        )
        .unwrap();
        assert_eq!(fresh.linux_sockets(), LinuxSockets::TcpOnly);
        for (sockets, name) in [
            (LinuxSockets::TcpOnly, "\"tcp_only\""),
            (LinuxSockets::Refuse, "\"refuse\""),
            (LinuxSockets::TcpPorts, "\"tcp_ports\""),
        ] {
            assert_eq!(serde_json::to_string(&sockets).unwrap(), name);
            assert_eq!(serde_json::from_str::<LinuxSockets>(name).unwrap(), sockets);
        }
        assert!(serde_json::from_str::<LinuxSockets>("\"udp\"").is_err());
    }

    #[test]
    fn tcp_only_starts_a_glibc_harness_and_refuses_one_that_cannot_ask_over_tcp() {
        let case = case();
        let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
        assert!(named.resolves_over_tcp());
        assert_eq!(named.linux_refusal(&case.root.join("glibc")), None);
        for program in ["musl", "static", "on_musl", "absent"] {
            assert_eq!(
                named.linux_refusal(&case.root.join(program)),
                Some(LINUX_RESOLVER_NEEDS_UDP),
                "{program}"
            );
        }
        for network in [
            local(),
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Open,
        ] {
            let quiet = sandbox(&case, network, LinuxSockets::TcpOnly);
            assert!(!quiet.resolves_over_tcp());
            assert_eq!(quiet.linux_refusal(&case.root.join("musl")), None);
        }
    }

    #[test]
    fn tcp_only_judges_a_script_by_its_interpreter_and_names_a_deeper_chain_plainly() {
        let case = case();
        let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
        assert_eq!(named.linux_refusal(&case.root.join("on_glibc")), None);
        assert_eq!(
            named.linux_refusal(&case.root.join("on_musl")),
            Some(LINUX_RESOLVER_NEEDS_UDP)
        );
        for program in ["on_script", "on_relative"] {
            assert_eq!(
                named.linux_refusal(&case.root.join(program)),
                Some(LINUX_HARNESS_UNCHECKED),
                "{program}"
            );
        }
        let quiet = sandbox(&case, local(), LinuxSockets::TcpOnly);
        assert_eq!(quiet.linux_refusal(&case.root.join("on_script")), None);
        let unchecked = crate::config::plain_problem(LINUX_HARNESS_UNCHECKED).unwrap();
        for part in [
            "cannot check",
            "'TCP only'",
            "does not start",
            "'TCP ports only'",
            "reach any host",
            "Settings",
        ] {
            assert!(unchecked.contains(part), "{unchecked}");
        }
    }

    #[test]
    fn refuse_starts_no_limited_network_agent() {
        let case = case();
        for network in [model(), local(), SandboxNetwork::Only(Vec::new())] {
            let refused = sandbox(&case, network, LinuxSockets::Refuse);
            assert!(!refused.resolves_over_tcp());
            assert_eq!(
                refused.linux_refusal(&case.root.join("glibc")),
                Some(LINUX_LIMITED_NETWORK_OFF)
            );
        }
        let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::Refuse);
        assert_eq!(open.linux_refusal(&case.root.join("glibc")), None);
    }

    #[test]
    fn only_a_nested_supervised_launch_with_its_network_held_is_full() {
        let case = case();
        let tcp = |network| sandbox(&case, network, LinuxSockets::TcpOnly);
        for held in [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            local(),
        ] {
            let held = tcp(held);
            assert!(linux_holds_fully(true, true, false, &held));
            assert!(
                !linux_holds_fully(false, true, false, &held),
                "no process space"
            );
            assert!(
                !linux_holds_fully(true, false, false, &held),
                "no supervisor"
            );
        }
        let hosted = tcp(model());
        assert!(linux_holds_fully(true, true, true, &hosted), "routed");
        assert!(
            !linux_holds_fully(true, true, false, &hosted),
            "a port on any host from this computer's network"
        );
        let low = tcp(SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(80),
        ]));
        assert!(low.routed().is_none());
        assert!(
            !linux_holds_fully(true, true, true, &low),
            "a loopback port below the bridge keeps it on this computer's network"
        );
        for sockets in [LinuxSockets::TcpPorts, LinuxSockets::Refuse] {
            let other = sandbox(&case, local(), sockets);
            assert!(!linux_holds_fully(true, true, true, &other), "{sockets:?}");
        }
        let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::TcpPorts);
        assert!(
            linux_holds_fully(true, true, false, &open),
            "an open network has no limit to hold"
        );
        let below = tcp(SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]));
        assert!(
            !linux_holds_fully(true, true, false, &below),
            "a loopback port below the bridge is not bridged"
        );
    }

    #[test]
    fn a_full_launch_is_one_no_network_line_names_on_a_host_that_holds_everything() {
        let case = case();
        let host = LinuxHost {
            nested: true,
            scoped: true,
            supervised: true,
            root: false,
            armored: false,
            routed: true,
        };
        let networks = [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            local(),
            model(),
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]),
            SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(80),
            ]),
            SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(8080),
            ]),
        ];
        for network in networks {
            let launched = sandbox(&case, network, LinuxSockets::TcpOnly);
            let routed = launched.routed().is_some();
            let full = linux_holds_fully(true, true, routed, &launched);
            for choice in [
                policy::AgentNetwork::None,
                policy::AgentNetwork::Https,
                policy::AgentNetwork::Open,
            ] {
                let line = linux_unbridged_notice_on(Some(host), choice, &launched);
                assert_eq!(
                    full,
                    line.is_none(),
                    "{:?} under {choice:?}: {line:?}",
                    launched.network
                );
            }
        }
    }

    #[test]
    fn the_strict_refusal_has_its_own_code_and_reads_plainly() {
        assert!(linux_not_full(&SandboxError::Unavailable(
            LINUX_NOT_FULL_LINE.into()
        )));
        assert!(!linux_not_full(&SandboxError::Unavailable("other".into())));
        assert!(!linux_not_full(&SandboxError::Invalid(
            LINUX_NOT_FULL_LINE.into()
        )));
        let line = crate::config::plain_problem(LINUX_NOT_FULL);
        assert!(line.is_some(), "the strict refusal has a plain sentence");
        let line = line.unwrap();
        for part in [
            "Refuse agents without the full Linux sandbox",
            "Agent access",
            "Settings",
        ] {
            assert!(line.contains(part), "{line}");
        }
    }

    #[test]
    fn each_refusal_reads_plainly_and_names_the_setting() {
        let udp = crate::config::plain_problem(LINUX_RESOLVER_NEEDS_UDP).unwrap();
        for part in [
            "cannot look up names over TCP",
            "'TCP only'",
            "'TCP ports only'",
            "Settings",
            "UDP and other sockets reach any host",
        ] {
            assert!(udp.contains(part), "{udp}");
        }
        let off = crate::config::plain_problem(LINUX_LIMITED_NETWORK_OFF).unwrap();
        for part in [
            "Linux limited-network sockets",
            "does not start",
            "Settings",
        ] {
            assert!(off.contains(part), "{off}");
        }
    }

    #[test]
    fn tcp_ports_starts_any_harness_and_moves_no_lookup_to_tcp() {
        let case = case();
        let ports = sandbox(&case, model(), LinuxSockets::TcpPorts);
        assert!(!ports.resolves_over_tcp());
        for program in [
            "glibc",
            "musl",
            "static",
            "on_glibc",
            "on_musl",
            "on_script",
            "on_relative",
        ] {
            assert_eq!(
                ports.linux_refusal(&case.root.join(program)),
                None,
                "{program}"
            );
        }
    }
}

#[cfg(test)]
mod held_stop_tests {
    use super::*;

    /// A Linux run the call supervisor stopped because it could not take back an entry
    /// read as a held name is told so; the same code on another system, and every other
    /// code on Linux (125, the supervisor's other stops, among them) stays the harness's
    /// own (DEC-185 S2a, rv-sec3 F1).
    #[test]
    fn a_linux_run_stopped_with_a_held_name_not_put_back_is_told_so() {
        let said = held_stop("linux", HELD_NOT_PUT_BACK_EXIT);
        assert_eq!(said, Some(HELD_NOT_PUT_BACK));
        for code in [0, 1, 3, 125, 126, 137] {
            assert_eq!(held_stop("linux", code), None, "{code}");
        }
        for os in ["macos", "windows"] {
            assert_eq!(held_stop(os, HELD_NOT_PUT_BACK_EXIT), None, "{os}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod order_tests {
    use super::*;

    /// CTX-M1 F1: the order check the profile asserts holds for a profile as the engine
    /// writes it, and fails once a write allow follows a root pin or once a pin is gone, so
    /// the profile's assert can trip.
    #[test]
    fn a_root_pin_a_later_write_allow_overrides_fails_the_order_check() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let sandbox = ProjectSandbox::new(
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Open,
        )
        .unwrap();
        let pins: Vec<String> = sandbox
            .writable_roots()
            .map(|root| root_pin(root))
            .collect();
        assert_eq!(pins.len(), 3);
        let text = sandbox.profile();
        let allow = format!(
            "(allow file-read* file-write* (subpath \"{}\"))\n",
            root.join("home").display()
        );

        assert!(denies_follow_write_allows(&text, &pins));
        assert!(!denies_follow_write_allows(
            &format!("{text}{allow}"),
            &pins
        ));
        assert!(!denies_follow_write_allows(
            &text.replace(&pins[1], ""),
            &pins
        ));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod access_grant_tests {
    use super::*;
    use policy::AccessGrant;

    fn case() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "owner-tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        (temp, root)
    }

    /// F08: a socket grant is a connect-only rule ahead of the network section with its
    /// write deny after every write allow, a port grant joins a limited network on
    /// loopback, and the temp folder grant becomes a writable root handed over as TMPDIR
    /// with every socket node in it kept unwritable.
    #[test]
    fn grants_add_a_connect_only_socket_a_loopback_port_and_the_owner_s_temp_folder() {
        let (_temp, root) = case();
        let socket = root.join("owner-tmp").join("discord-ipc-0");
        let owner_tmp = root.join("owner-tmp");
        let sandbox = ProjectSandbox::new(
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        )
        .unwrap()
        .grant_access(
            &[
                AccessGrant::Socket {
                    path: socket.clone(),
                },
                AccessGrant::Port { port: 47_821 },
                AccessGrant::TempFolder,
            ],
            Some(&owner_tmp),
        )
        .unwrap();
        assert_eq!(
            sandbox.network(),
            &SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(47_821)
            ])
        );
        assert_eq!(sandbox.granted_sockets(), std::slice::from_ref(&socket));
        assert_eq!(sandbox.owner_tmp(), Some(owner_tmp.as_path()));
        assert!(sandbox.writes(&owner_tmp.join("note.txt")));
        let text = sandbox.profile();
        let path = socket.display();
        assert!(text.contains(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{path}\")))"
        )));
        assert!(!text.contains(&format!(
            "(allow network-bind (local unix-socket (path-literal \"{path}\")))"
        )));
        assert!(text.contains(&format!("(deny file-write* (literal \"{path}\"))")));
        assert!(text.contains("(allow network-outbound (remote ip \"localhost:47821\"))"));
        assert!(text.contains(&format!(
            "(deny file-write*\n  (require-all\n    (subpath \"{}\")\n    (vnode-type SOCKET)))",
            owner_tmp.display()
        )));
        let env = sandbox.base_env(true);
        let tmpdir = env
            .iter()
            .find(|(name, _)| *name == "TMPDIR")
            .map(|(_, value)| value.clone());
        assert_eq!(tmpdir, Some(owner_tmp.clone().into_os_string()));
    }

    /// F08: a grant beyond its kind's bound, a socket in a missing folder, a temp folder
    /// grant without the owner's folder and a port that overfills a limited network are
    /// refused; an open network takes a port grant as it is.
    #[test]
    fn grants_that_cannot_be_held_are_refused() {
        let (_temp, root) = case();
        let fresh = |network| {
            ProjectSandbox::new(
                &root.join("project"),
                &root.join("home"),
                &root.join("tmp"),
                network,
            )
            .unwrap()
        };
        let socket = |name: &str| AccessGrant::Socket {
            path: root.join("owner-tmp").join(name),
        };
        let only = || SandboxNetwork::Only(Vec::new());
        assert!(
            fresh(only())
                .grant_access(&[socket("a"), socket("b"), socket("c")], None)
                .is_err()
        );
        let missing = AccessGrant::Socket {
            path: root.join("gone").join("x"),
        };
        assert!(fresh(only()).grant_access(&[missing], None).is_err());
        assert!(
            fresh(only())
                .grant_access(&[AccessGrant::TempFolder], None)
                .is_err()
        );
        let full: Vec<SandboxRemote> = (1..=REMOTES_MAX as u16)
            .map(SandboxRemote::AnyHost)
            .collect();
        assert!(
            fresh(SandboxNetwork::Only(full))
                .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
                .is_err()
        );
        let open = fresh(SandboxNetwork::Open)
            .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
            .unwrap();
        assert_eq!(open.network(), &SandboxNetwork::Open);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod profile_string_tests {
    use super::*;

    /// v2.4.0 gate: the folded secret names, 1300 bytes as one regex, split into bodies each
    /// Seatbelt reads, every name kept whole and in order; a list that fits stays one body
    /// in the shape it had, and no names give no body.
    #[test]
    fn regex_bodies_pack_whole_names_under_the_reader_limit() {
        let names: Vec<String> = shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let prefix = format!("^/private/tmp/{}/(.*/)?", "p".repeat(200));
        let bodies = regex_bodies(&prefix, &names, "(/.*)?$");
        assert!(bodies.len() >= 2, "{bodies:#?}");
        assert!(bodies.iter().all(|body| body.len() <= SBPL_STRING_MAX));
        let mut at = 0;
        for body in &bodies {
            let group = &body[prefix.len() + 1..body.len() - "(/.*)?$".len() - 1];
            let start = at;
            while at < names.len() && names[start..=at].join("|").len() <= group.len() {
                at += 1;
            }
            assert_eq!(names[start..at].join("|"), group);
        }
        assert_eq!(at, names.len());
        assert_eq!(
            regex_bodies("^/r/", &["a".into(), "b".into()], "(/.*)?$"),
            vec!["^/r/(a|b)(/.*)?$".to_owned()]
        );
        assert!(regex_bodies("^/r/", &[], "(/.*)?$").is_empty());
    }

    /// The guard names every string past the limit, none in a profile that parses, and an
    /// unclosed quote too.
    #[test]
    fn sbpl_overlong_names_every_string_past_the_reader_limit() {
        let fits = "a".repeat(SBPL_STRING_MAX);
        let long = "b".repeat(SBPL_STRING_MAX + 1);
        let longer = "c".repeat(2 * SBPL_STRING_MAX);
        let text = format!(
            "(deny file-read* (subpath \"{fits}\") (literal \"{long}\") (regex #\"{longer}\"))"
        );
        assert_eq!(sbpl_overlong(&text), vec![long.as_str(), longer.as_str()]);
        assert!(sbpl_overlong(&format!("(literal \"{fits}\")")).is_empty());
        assert_eq!(sbpl_overlong("(literal \"/open"), vec!["/open"]);
    }

    /// SA vid 202: a name is spelled with every code point the case-insensitive volume folds
    /// onto its letters, also those for two or three letters (U+00DF and U+1E9E for `ss`,
    /// U+FB00 to U+FB06 for `ff`, `fi`, `fl`, `ffi`, `ffl` and `st`), and the Greek question
    /// mark and varia for `;` and the backtick, and U+0131 for `i` as NTFS maps it. In a
    /// pattern a fold reaches into a wildcard
    /// next to the letters, never into a literal or an alternation, so the env templates the
    /// secret rule gives back stay exact.
    #[test]
    fn a_name_is_spelled_with_every_fold_of_the_volume_also_for_two_or_three_letters() {
        let spelled = [
            ("config", "[cC][oO][nN]([fF](i|I|\u{131})|\u{fb01})[gG]"),
            (
                "instructions.md",
                concat!(
                    "(i|I|\u{131})[nN]((s|S|\u{17f})[tT]|\u{fb05}|\u{fb06})",
                    "[rR][uU][cC][tT](i|I|\u{131})[oO][nN](s|S|\u{17f})\\.[mM][dD]",
                ),
            ),
            (
                ".ssh",
                "\\.((s|S|\u{17f})(s|S|\u{17f})|\u{df}|\u{1e9e})[hH]",
            ),
            (
                "ffi",
                "([fF][fF](i|I|\u{131})|[fF]\u{fb01}|\u{fb00}(i|I|\u{131})|\u{fb03})",
            ),
            (".git", "\\.[gG](i|I|\u{131})[tT]"),
            ("k", "(k|K|\u{212a})"),
            (";`", "(;|\u{37e})(`|\u{1fef})"),
        ];
        for (name, pattern) in spelled {
            assert_eq!(any_case(name), pattern, "{name}");
        }
        assert_eq!(
            fold_case(".*service"),
            ".*((s|S|\u{17f})|\u{df}|\u{1e9e})[eE][rR][vV](i|I|\u{131})[cC][eE]"
        );
        assert_eq!(
            fold_case("of.*"),
            "[oO]([fF]|\u{fb00}|\u{fb01}|\u{fb02}|\u{fb03}|\u{fb04}).*"
        );
        assert_eq!(
            fold_case("secrets?"),
            "(s|S|\u{17f})[eE][cC][rR][eE][tT](s|S|\u{17f})?"
        );
        let templates = fold_case(shared_folders::SECRET_TEMPLATES);
        assert!(!templates.contains(['\u{df}', '\u{1e9e}']));
    }

    /// B5 r2 (rv-sec3 F1, rv-code4 F1): path text with a long fold run, such as a project
    /// folder named with 7 `s` or 10 `f` or a top-level folder with 60, is spelled with no
    /// panic and no runaway list: a stretch past the spelling limit takes one of its pieces
    /// at each place, its fewest pieces' worth of places and the rest up to one per letter
    /// optional, since Seatbelt reads no `{m,n}` count. A pattern keeps every stretch exact.
    #[test]
    fn a_long_fold_run_in_path_text_is_spelled_in_bounded_length_never_a_panic() {
        let s = "((s|S|\u{17f})|\u{df}|\u{1e9e})";
        assert_eq!(any_case("ssss"), format!("{s}{s}{s}?{s}?"));
        let f = "([fF]|\u{fb00})";
        assert_eq!(any_case("ffffff"), format!("{f}{f}{f}{f}?{f}?{f}?"));
        let root = format!("/Users/o/Glasssssss/{}/{}", "f".repeat(10), "s".repeat(60));
        let spelled = any_case(&root);
        assert!(spelled.len() <= 20 * root.len(), "{}", spelled.len());
        assert!(!spelled.contains('{'), "{spelled}");
        assert!(std::panic::catch_unwind(|| fold_case("sssss")).is_err());
    }

    /// The default secret rule and 64 long open top-level names give a profile with no string
    /// past the limit; a project folder so deep that one secret name no longer fits, and the
    /// most open names at the longest length, which pass the 64 KiB cap, are each refused at
    /// install with `Invalid`, never a panic, before a profile sandbox-exec cannot load is
    /// written.
    #[test]
    fn a_profile_keeps_every_string_seatbelt_reads_or_is_refused_at_install() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (home, tmp, profiles) = (base.join("home"), base.join("tmp"), base.join("sb"));
        let project = base.join("project");
        for folder in [&home, &tmp, &profiles, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        let open: Vec<String> = (0..64)
            .map(|at| format!("{at:02}{}", "n".repeat(40)))
            .collect();
        let wide = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Open)
            .unwrap()
            .keep_project_secrets(&project, &[])
            .unwrap()
            .open_only_tops(&open)
            .unwrap();
        let text = wide.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.matches("(require-not (regex #\"").count() >= 2);
        assert!(text.matches(KEPT_OUT_DENY).count() >= 3);
        wide.install(&profiles.join("wide.sb")).unwrap();

        let most: Vec<String> = (0..OPEN_TOPS_MAX)
            .map(|at| format!("{at:03}{}", "n".repeat(252)))
            .collect();
        let crowded = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
            .unwrap()
            .keep_project_secrets(&project, &[])
            .unwrap()
            .open_only_tops(&most)
            .unwrap();
        let text = crowded.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.len() > PROFILE_BYTES_MAX);
        let target = profiles.join("crowded.sb");
        match crowded.install(&target) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, "the sandbox profile is larger than its 64 KiB cap")
            }
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());

        let mut deep = base.join("deep");
        while deep.as_os_str().len() < 1000 {
            let room = 1000 - deep.as_os_str().len();
            deep = deep.join("d".repeat(room.clamp(1, 200)));
        }
        fs::create_dir_all(&deep).unwrap();
        let narrow = ProjectSandbox::new(&deep, &home, &tmp, SandboxNetwork::Only(Vec::new()))
            .unwrap()
            .keep_project_secrets(&deep, &[])
            .unwrap();
        assert!(!sbpl_overlong(&narrow.profile()).is_empty());
        let target = profiles.join("deep.sb");
        match narrow.install(&target) {
            Err(SandboxError::Invalid(reason)) => assert_eq!(
                reason,
                "a path or name in the sandbox profile is longer than Seatbelt reads"
            ),
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());
    }

    /// V243-FARM2-PRODUCT-SMALL: the agent home's config and kept-entry denies and the
    /// regranted flag and `.git` denies match a path and everything below it as one `X(/|$)`
    /// regex, never with a `(/.*)` tail, which libsandbox fails to compile again in lower case
    /// after a fold group with no bracket class before it (V243-PROFILE-CAP-ROW).
    #[test]
    fn home_denies_match_at_or_below_without_an_optional_tail() {
        let home = Path::new("/private/tmp/.tmpA1b2C3/agent/home");
        let data = Path::new("/private/tmp/.tmpA1b2C3/data");
        let config = agent_config_denies(home);
        assert!(!config.contains("(/.*)"), "{config}");
        let regranted = regranted_denies(home, &[data]);
        assert!(!regranted.contains("(/.*)"), "{regranted}");
        let flagged = seatbelt_pattern(home);
        assert!(
            regranted.contains(&format!(
                "(deny file-write-flags file-write-acl (regex #\"^{flagged}(/|$)\"))"
            )),
            "{regranted}"
        );
        let git = any_case(".git");
        for root in [home, data] {
            let below = format!("{}/(.+/)?{git}", seatbelt_pattern(root));
            assert!(
                regranted.contains(&format!("(regex #\"^{below}(/|$)\")")),
                "{regranted}"
            );
        }
    }

    /// V243-FARM2-PRODUCT-SMALL: no regex of a closed-tops agent's profile, a clone's store
    /// denies or a home's config denies holds a `(/.*)` tail. libsandbox fails its second,
    /// lower-case compile of one after a fold group with no bracket class or wildcard before
    /// it, as `^/x/(s|S)(/.*)?$` (probed 11-17), so each is one `X(/|$)` regex instead
    /// (V243-PROFILE-CAP-ROW). The HFS rule alone keeps a tail, after its leading `(.*/)?`,
    /// which compiles.
    #[test]
    fn no_profile_regex_has_a_slash_tail() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (home, tmp, project) = (base.join("home"), base.join("tmp"), base.join("project"));
        for folder in [&home, &tmp, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        let sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
            .unwrap()
            .keep_project_secrets(&project, &[])
            .unwrap()
            .open_only_tops(&["notes".to_string()])
            .unwrap();
        let text = [
            sandbox.profile(),
            clone_store_denies(&base.join("clone")),
            agent_config_denies(&home),
        ]
        .concat();
        let hfs = hfs_ignored_deny();
        assert_eq!(text.matches(hfs.as_str()).count(), 1);
        let text = text.replace(hfs.as_str(), "");
        let bodies: Vec<&str> = text
            .split("#\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"').map(|(body, _)| body))
            .collect();
        assert!(bodies.len() > 20, "{text}");
        let tailed: Vec<&&str> = bodies
            .iter()
            .filter(|body| body.contains("(/.*)"))
            .collect();
        assert!(tailed.is_empty(), "{tailed:#?}");
        let below = bodies.iter().filter(|body| body.ends_with("(/|$)")).count();
        assert!(below >= 20, "{below}");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod firmlink_tests {
    use super::*;

    /// V243-FIRMLINK-SANDBOX-ROOT (sweep #3): a root named through /System/Volumes/Data is
    /// refused, since Seatbelt matches only the short name and every rule written in the long
    /// one would never match; an extra read path typed that way is checked under its short
    /// name, so one holding the engine's data is refused.
    #[test]
    fn a_root_named_through_the_data_volume_is_refused_and_a_read_path_is_checked_short() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "engine"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let long = |path: &Path| PathBuf::from(format!("/System/Volumes/Data{}", path.display()));
        assert!(long(&root.join("project")).is_dir());
        let network = || SandboxNetwork::Only(Vec::new());
        let (home, tmp) = (root.join("home"), root.join("tmp"));
        let refused = ProjectSandbox::new(&long(&root.join("project")), &home, &tmp, network());
        assert!(
            matches!(&refused, Err(SandboxError::Invalid(reason)) if reason.contains("canonical")),
            "{refused:?}"
        );
        assert!(ProjectSandbox::new(&root.join("project"), &home, &tmp, network()).is_ok());
        assert_eq!(
            extra_paths::checked(long(&root).to_str().unwrap(), &root.join("engine")),
            Err("read_path_engine_data")
        );
    }
}
