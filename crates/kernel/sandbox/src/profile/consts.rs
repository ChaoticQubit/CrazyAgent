use crate::lines::HELD_NOT_PUT_BACK_EXIT;
use crate::paths::PATH_BYTES_MAX;

/// Why a sandbox with hidden folders does not start on Windows, nor one with kept-out paths
/// or open tops on Linux or Windows: those boundaries cannot keep a path inside a granted
/// root out of reach as Seatbelt does. Linux holds hidden folders on its bubblewrap
/// backends and names its own refusals otherwise (`linux::HIDDEN_NEEDS_NAMESPACES`,
/// `linux::HIDDEN_MOUNTED_AGAIN`).
pub const HIDDEN_NEEDS_SEATBELT: &str = "hidden folders need the macOS sandbox";

/// Whether this system's boundary cannot hold a hidden folder, so a sandbox with one does not
/// start ([`HIDDEN_NEEDS_SEATBELT`]), a save that hides a folder is refused and neither app
/// offers the setting (#590): true on Windows only. Linux holds them, and each launch there
/// refuses with its own code where the host cannot (V244-FWD-V243-4).
pub const HIDDEN_UNHELD: bool = cfg!(target_os = "windows");

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

pub const READ_ROOTS_MAX: usize = 16;

/// The most folder links one sandbox makes readable by name beside its roots: the links
/// its process is started through.
pub const LINKS_MAX: usize = 4;

pub const WORKTREES_MAX: usize = 8;

/// The most files and folders one sandbox keeps out by name: the graph step's secret-like and
/// git-ignored project paths (MAP-2). A project with more builds no graph.
pub const KEPT_OUT_MAX: usize = 1024;

/// The opening of the Seatbelt rule for paths kept out: contents, folder listings and
/// attribute values are denied with every write, run, link and clone, while names, sizes
/// and dates stay readable, so a tool that only stats such a path does not fail.
pub const KEPT_OUT_DENY: &str =
    "(deny file-read-data file-read-xattr file-write* process-exec file-link file-clone";

/// The longest quoted string or regex, in raw bytes, Seatbelt's profile reader takes: one
/// byte more fails the whole profile with `Error reading string`, whatever the bytes are
/// (probe 2026-11-12: 1023 read, 1024 refused, backslashes and multibyte letters alike).
pub const SBPL_STRING_MAX: usize = 1023;

/// The most exact Unix sockets one agent may bind and connect: its own Codex app-server's.
pub const SOCKETS_MAX: usize = 2;

/// The most exact Unix socket grants one agent holds, of every use together: a native lead's
/// own tmux server, its caller socket and its teammates' tmux server.
pub const SOCKET_GRANTS_MAX: usize = 4;

/// The writes that move, remove, replace, re-permission or flag a folder's own node while
/// its contents stay writable: every writable root, the scratch root and a native lead's
/// home for its teammates. A flag such as `uchg` would freeze the folder for every other
/// writer and for the engine (CTX-M1 r1).
pub const FIXED_WRITES: &str =
    "file-write-unlink file-write-create file-write-mode file-write-acl file-write-flags";

/// The writes that move, remove, re-permission or flag a folder's own node but still let it
/// be made where it is missing: an agent home's `skills` folder, which Codex makes in a fresh
/// home for its own skills, while no folder built elsewhere replaces the one holding the
/// engine's copies and no flag keeps the engine from removing what is in it (CTX-M1 r1).
pub const PINNED_WRITES: &str = "file-write-unlink file-write-mode file-write-acl file-write-flags";

/// The folders of a Claude Code config home that its agent team shares, the members'
/// mailboxes and task lists. Claude Code 2.1.309 keeps them under each member's config home,
/// with no variable of their own, so a native lead's teammates write the lead's (TPH-2).
pub const CLAUDE_TEAM_SHARED: [&str; 2] = ["teams", "tasks"];

/// The Seatbelt pattern of a lock folder's path, `<file>.lock`: the only folder a teammate
/// may still make in a shared team folder, where it may make no other folder or link.
pub const LOCK_FOLDER: &str = r#"\.lock$"#;

/// The longest Unix socket path macOS binds, `sun_path` less its closing NUL.
pub const SOCKET_PATH_BYTES_MAX: usize = 103;

pub const TOOL_ROOTS_MAX: usize = 24;

pub const TOOL_FILES_MAX: usize = 5;

pub const EXECS_MAX: usize = 4;

pub const GIT_LINK_BYTES_MAX: u64 = 2 * PATH_BYTES_MAX as u64;

#[cfg(unix)]
pub const PROFILE_BYTES_MAX: usize = 64 * 1024;

pub const AGENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin";

pub const SYSTEM_READ_ROOTS: &[&str] = &[
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

pub const METADATA_LITERALS: &[&str] = &[
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

pub const MACH_SERVICES: &[&str] = &[
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

pub const KEYCHAIN_SERVICES: &[&str] = &[
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

pub const CREDENTIAL_NAMES: &[&str] = &[
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

pub const ENGINE_SECRET_FILES: &[&str] = &[
    "engine.db",
    "engine.db-wal",
    "engine.db-shm",
    "engine.db-journal",
    "owner.sock",
    "github.json",
    "github.toml",
];

pub const RUNTIME_ENTRIES_MAX: usize = 1024;

pub const AGENT_KEY_VARIABLES: &[&str] = &["ANTHROPIC_API_KEY"];

pub const AGENT_ENV_MAX: usize = 128;

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

/// The engine refusal code of a Linux launch the strict setting refuses ([`LinuxStrict`]).
pub const LINUX_NOT_FULL: &str = "linux_sandbox_not_full";

/// The engine refusal code of a Linux launch whose project hides folders on a computer
/// where bubblewrap cannot start agents in namespaces of their own.
pub const LINUX_HIDDEN_NEEDS_NAMESPACES: &str = "linux_hidden_needs_namespaces";

/// The engine refusal code of a Linux launch whose project hides a folder that this
/// computer mounts a second time where it shows that folder.
pub const LINUX_HIDDEN_MOUNTED_AGAIN: &str = "linux_hidden_mounted_again";

/// The engine refusal code of a Linux launch whose project hides folders on a computer
/// where the sandbox helper cannot cut the agent's mounts off from this computer's.
pub const LINUX_HIDDEN_MOUNTS_NOT_PRIVATE: &str = "linux_hidden_mounts_not_private";

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

#[cfg(test)]
mod tests;
