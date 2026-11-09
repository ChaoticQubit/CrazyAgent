use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub mod elf;
pub mod extra_paths;
pub mod hidden_paths;
pub mod ide_shell;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod model_proxy;
pub mod policy;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_agent;
pub mod toolchains;
pub mod warden;
#[cfg(target_os = "windows")]
pub mod windows;

/// The macOS Seatbelt launcher every project agent process runs under.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

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

/// Why a sandbox with hidden folders does not start on Linux or Windows: their boundaries
/// grant whole roots and cannot keep a folder inside one out of reach.
pub const HIDDEN_NEEDS_SEATBELT: &str = "hidden folders need the macOS sandbox";

/// Why a native-mode lead's own tmux server does not start off macOS: only Seatbelt holds
/// its one-socket grant, and DEC-094 keeps team agents macOS-only in V1.
pub const TMUX_SERVER_NEEDS_SEATBELT: &str = "native teammates need the macOS sandbox";

/// Why a Full access boundary cannot run off macOS: only macOS qualifies Full access in V1.
pub const FULL_ACCESS_NEEDS_MACOS: &str = "Full access agents run only on macOS in this version";

const PATH_BYTES_MAX: usize = 1024;
const READ_ROOTS_MAX: usize = 16;
const WORKTREES_MAX: usize = 8;
/// The most folders one sandbox hides: every hidden folder a project saves
/// ([`hidden_paths::HIDDEN_PATHS_MAX`]) plus the two a step adds itself at most, such as the
/// graph step's `.crazyagent/`, or a native lead's part of the relay mailbox and its own temp
/// folder, which its teammates' sandbox hides.
pub const HIDDEN_MAX: usize = hidden_paths::HIDDEN_PATHS_MAX + 2;
/// The most files and folders one sandbox keeps out by name: the graph step's secret-like and
/// git-ignored project paths (MAP-2). A project with more builds no graph.
pub const KEPT_OUT_MAX: usize = 1024;
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
/// The folders of a Claude Code config home whose scripts Claude Code 2.1.309 runs ahead of
/// every Bash command: its shell snapshot and the session hook scripts (TPH-2).
#[cfg(unix)]
const CLAUDE_BASH_PRELUDES: [&str; 2] = ["shell-snapshots", "session-env"];
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
const TOOL_FILES_MAX: usize = 4;
const EXECS_MAX: usize = 4;
const GIT_LINK_BYTES_MAX: u64 = 2 * PATH_BYTES_MAX as u64;
const REMOTES_MAX: usize = 8;
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

const ENGINE_SECRET_FOLDERS: &[&str] =
    &["account-keys", crate::terminal::FOLDER, crate::ide::FOLDER];

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
/// [`agent_env_refused`] accepts, one of the fixed [`AGENT_GIT_ENV`] pairs, or a model key
/// name holding the local placeholder, never a real provider key. No variable may hold a
/// key relay token: any process of the owner's user can read another's environment, so a
/// harness reads its token from the key file in its own home. Inputs: the name and value.
/// Output: true when allowed.
pub fn agent_variable_allowed(name: &OsStr, value: &OsStr) -> bool {
    let token = crate::model_gateway::relay::TOKEN_PREFIX.as_bytes();
    if value
        .as_encoded_bytes()
        .windows(token.len())
        .any(|window| window == token)
    {
        return false;
    }
    if AGENT_GIT_ENV
        .iter()
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
/// `HOME`, `TMPDIR` and `TMPPREFIX` always stay as `base` set them), then `TMPPREFIX` from
/// [`shell_tmp_prefix`] when `TMPDIR` is set, then [`AGENT_GIT_ENV`] and [`AGENT_TLS_ENV`].
/// Inputs: the base and harness variables. Output: the variables, or `Invalid` when any
/// one fails [`agent_variable_allowed`].
pub fn agent_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
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
        if name == "HOME" || name == "TMPDIR" || name == "TMPPREFIX" {
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
    for (name, value) in AGENT_GIT_ENV.into_iter().chain(AGENT_TLS_ENV) {
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
/// harness program and its arguments.
#[cfg(unix)]
macro_rules! warden_then_harness {
    () => {
        r#"if (defined(my $fd = delete $ENV{CRAZYAGENT_WARDEN_FD})) { my $engine = getppid(); $fd =~ /\A[0-9]{1,6}\z/ && $engine > 1 or die "crazyagent: no warden\n"; defined(my $warden = fork()) or die "crazyagent: no warden: $!\n"; if (!$warden) { POSIX::setsid(); POSIX::close($_) for 0, 1, 2; $SIG{PIPE} = "IGNORE"; open(my $line, "+<&=", $fd) or POSIX::_exit(1); my $armed = !kill(0, $engine) && $!{EPERM}; syswrite($line, $armed ? "A" : "U"); $armed or POSIX::_exit(0); my $byte; 1 while sysread($line, $byte, 1); syswrite($line, "F"); kill(9, -1); POSIX::_exit(1) } POSIX::close($fd) } exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n""#
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
/// that signal the pane's group do not end it.
#[cfg(target_os = "macos")]
pub const KEEPER: [&str; 6] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-MSocket",
    "-e",
    r#"$^F = 1023; my $why = ""; $SIG{$_} = "IGNORE" for qw(INT QUIT TSTP TTIN TTOU PIPE); $SIG{HUP} = sub { $why ||= "HUP" }; $SIG{TERM} = sub { $why ||= "TERM" }; $SIG{CHLD} = sub { 1 }; socketpair(my $keep, my $line, AF_UNIX, SOCK_STREAM, PF_UNSPEC) or die "crazyagent: no warden: $!\n"; defined(my $harness = fork()) or die "crazyagent: no warden: $!\n"; if (!$harness) { $SIG{$_} = "DEFAULT" for qw(INT QUIT TSTP TTIN TTOU PIPE HUP TERM CHLD); close($keep); $ENV{CRAZYAGENT_WARDEN_FD} = fileno($line); exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n" } close($line); until ($why || waitpid($harness, POSIX::WNOHANG()) > 0) { select(undef, undef, undef, 1) } shutdown($keep, 1); my ($report, $wanted) = ("", ""); vec($wanted, fileno($keep), 1) = 1; my $until = time + 4; while (time < $until && length($report) < 8) { select(my $ready = $wanted, undef, undef, 0.25) > 0 or next; sysread($keep, my $more, 8) or last; $report .= $more } POSIX::_exit(0) if $report eq "AF" || $report eq "U" || $why eq "HUP"; $SIG{TERM} = "IGNORE"; $SIG{HUP} = "DEFAULT"; sleep(3600) while 1"#,
    "--",
];

/// Puts a pane's sandboxed command under the [`KEEPER`] and the [`PANE_SHIM`], so stopping
/// the pane ends every process its agent started. Input: the argv `sandbox-exec -f
/// <profile> <program> <arguments>`. Output: the kept argv, or none for any other argv.
#[cfg(target_os = "macos")]
pub fn kept_pane(argv: Vec<String>) -> Option<Vec<String>> {
    if argv.len() < 4 || argv[0] != SANDBOX_EXEC || argv[1] != "-f" {
        return None;
    }
    let length = argv.len();
    let mut kept: Vec<String> = KEEPER.iter().map(|part| (*part).to_owned()).collect();
    let mut rest = argv.into_iter();
    kept.extend(rest.by_ref().take(3));
    kept.extend(PANE_SHIM.iter().map(|part| (*part).to_owned()));
    kept.extend(rest);
    assert_eq!(kept.len(), KEEPER.len() + PANE_SHIM.len() + length);
    assert_eq!(kept[KEEPER.len()], SANDBOX_EXEC);
    Some(kept)
}

/// Takes a pane command back out of the [`KEEPER`] and the [`PANE_SHIM`], for a pane that
/// starts in a server already running inside its sandbox, where neither can run. Input: an
/// argv. Output: the `sandbox-exec -f <profile> <program> <arguments>` that [`kept_pane`]
/// kept when keeping it again rebuilds exactly this argv, or none for any other argv.
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
    (kept_pane(plain.clone()).as_deref() == Some(argv)).then_some(plain)
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
/// it never binds or listens on an [`OWNER_PORTS`] port or one [`ProjectSandbox::keep_ports`]
/// keeps, otherwise only the listed remotes, each one port on loopback or on any host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNetwork {
    Open,
    Only(Vec<SandboxRemote>),
}

/// One outbound TCP destination a tools-off agent may still reach. Seatbelt
/// filters by port only, either on loopback or on any host.
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

/// The launch refusal when the Linux sockets setting starts no limited-network agent.
pub const LINUX_LIMITED_NETWORK_OFF: &str = "linux_limited_network_off";

/// The launch refusal when, under `TcpOnly`, a harness that looks host names up is not a
/// glibc program: only glibc's resolver asks over TCP, so it could not reach its model.
pub const LINUX_RESOLVER_NEEDS_UDP: &str = "linux_resolver_needs_udp";

/// The launch refusal when, under `TcpOnly`, a harness that looks host names up is a script
/// whose interpreter cannot be checked ([`elf::Lookup::Unjudged`]), such as a script that
/// runs another script: the check follows a `#!` line one level only and never guesses.
pub const LINUX_HARNESS_UNCHECKED: &str = "linux_harness_unchecked";

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
    network: SandboxNetwork,
    linux_sockets: LinuxSockets,
    kept_ports: Vec<u16>,
    owner_home: PathBuf,
    engine_secrets: Vec<PathBuf>,
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
    setup: Option<TeammatesSetup>,
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
            network,
            linux_sockets: LinuxSockets::default(),
            kept_ports: Vec::new(),
            owner_home: owner,
            engine_secrets: Vec::new(),
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
            setup: None,
        })
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

    /// Names the engine runtime folder whose secrets this agent must never read or write:
    /// the engine database, the owner socket, the GitHub sign-in and settings files, the
    /// saved account keys, the terminal server folder and every engine socket and socket
    /// folder found in it now. The agent's own home and temp dirs may sit inside it, but no
    /// root granted so far may hold or sit inside one of those secrets.
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

    /// Confines the engine's own git, run with a linked lead's rights, to adding the lead's
    /// native teammates' linked worktrees (OP-9b): it writes only the teammates' folder of
    /// the lead's worktree, the project repository's `.git/worktrees` records and the refs
    /// and reflogs of `crazyagent/<task>--<name>` teammate branches with their `crazyagent`
    /// folders, beside the lead's home and temp dir; the project stays readable. Makes the teammates' folder as a plain
    /// folder. Input: the lead's canonical linked worktree in `worktrees/`, whose `.git`
    /// link names a record in `.git/worktrees`. Output: the confined sandbox, `Invalid` or
    /// the disk error.
    pub fn confine_to_teammates_setup(mut self, worktree: &Path) -> Result<Self, SandboxError> {
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

    /// Keeps one file or folder inside a granted root out of the agent's reach, read and
    /// write, as a hidden folder is; the graph step keeps the project's secret-like and
    /// git-ignored paths from graphify (MAP-2). Input: a clean absolute path below a root, of
    /// any kind or absent. Output: the narrowed sandbox, or `Invalid` past [`KEPT_OUT_MAX`].
    /// Only Seatbelt holds it, so the Linux and Windows boundaries refuse such a sandbox.
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
    /// `allow_unix_socket` gives, for its own socket in its short temp folder, and read
    /// access to the tmux program's folder when no system root covers it. No other Unix
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
    /// (relay mailbox forgery C1). Inputs: the three sockets, the canonical tmux program and
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
        teammates
            .read_only
            .extend(CLAUDE_BASH_PRELUDES.iter().map(|folder| home.join(folder)));
        teammates.read_only.extend(
            self.scratch
                .iter()
                .map(|scratch| scratch.join(toolchains::GIT_FOLDER)),
        );
        teammates.fixed = vec![home.clone(), self.agent_tmp.clone(), mailbox];
        teammates.fixed.extend(shared.iter().cloned());
        teammates.shared = shared;
        teammates.teammates = true;
        if teammates.writes(&home) {
            return Err(invalid("a teammates' writable root holds the lead's home"));
        }
        let lead = self
            .allow_tmux_server(own, tmux)?
            .reach_unix_socket(callers)?
            .reach_unix_socket(mates)?
            .shut_unix_socket(&binding)?;
        Ok((lead, teammates))
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
            (Some(clone), _) => teammates_clone_denies(clone),
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
        let folder = tmux
            .parent()
            .ok_or_else(|| invalid("the tmux program must sit in a folder"))?;
        let covered = SYSTEM_READ_ROOTS
            .iter()
            .any(|root| folder.starts_with(root))
            || self.read_roots.iter().any(|root| folder.starts_with(root));
        if !covered {
            self = self.allow_read(folder)?;
        }
        self = if served_only {
            self.serve_unix_socket(socket)?
        } else {
            self.allow_unix_socket(socket)?
        };
        assert!(self.writes(socket.parent().unwrap_or(socket)));
        Ok(self)
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
    /// folders, the git hooks, config and its lock, `.git/modules` and, for an agent confined
    /// to a worktree, its admin folder's `modules`, where git keeps submodule repositories.
    /// Every platform boundary keeps these read-only, except that Windows guards the config
    /// but not its lock (`windows::GIT_CONFIG_LOCK`).
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let git = self.project_root.join(".git");
        let mut paths = vec![
            self.project_root.join("context"),
            self.project_root.join(".crazyagent").join("context"),
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
    /// teammates the team folders they share in the lead's home.
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
    /// sysctl is refused only with both the named sysctl deny and the `process-info-pidinfo`
    /// deny; a process of the same sandbox stays readable, and `nohup`, which asks that of a
    /// process outside to detach from the console, keeps it by its own path. Output: the text.
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
        text.push_str("(deny process-info-pidinfo)\n");
        text.push_str("(allow process-info* (target same-sandbox))\n");
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
        if self.setup.is_some() {
            text.push_str(&format!(
                "(allow file-write*\n  (regex #\"^{}/\\.git/(logs/)?refs/heads/{}(/[a-z0-9-]+--[a-z0-9-]+(\\.lock)?)?$\"))\n",
                seatbelt_pattern(&self.project_root),
                regex_escape(crate::agent_git::LINKED_PREFIX.trim_end_matches('/'))
            ));
        }
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
                "\n  (regex #\"^{}/(.*/)?({credentials})(/.*)?$\")",
                seatbelt_pattern(root)
            ));
        }
        text.push_str(")\n(deny mach-lookup");
        for service in KEYCHAIN_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        if !self.hidden.is_empty() || !self.kept_out.is_empty() {
            text.push_str("(deny file-read* file-write* process-exec file-link file-clone");
            for folder in self.hidden.iter().chain(&self.kept_out) {
                if clean_absolute(folder) {
                    text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
                }
            }
            text.push_str(")\n");
        }
        for file in self.revealed.iter().filter(|file| clean_absolute(file)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                file.display()
            ));
        }
        let project = self.project_root.display();
        text.push_str(&format!(
            "(deny file-write*\n  (subpath \"{project}/context\")\n  (subpath \"{project}/.crazyagent/context\")\n  (subpath \"{project}/.git/hooks\")\n  (literal \"{project}/.git/config\")\n  (literal \"{project}/.git/config.lock\")\n  (literal \"{project}/.git/index\")\n  (literal \"{project}/.git\")\n  (literal \"{project}/.git/commondir\")\n  (literal \"{project}/.git/config.worktree\")\n  (subpath \"{project}/.git/info\")\n  (literal \"{project}/.git/shallow\")\n  (literal \"{project}/.git/objects\")\n  (literal \"{project}/.git/objects/info\")\n  (literal \"{project}/.git/objects/info/alternates\")\n  (literal \"{project}/.git/objects/info/http-alternates\")\n  (subpath \"{project}/.git/modules\")\n  (literal \"{project}/.crazyagent\"))\n"
        ));
        if let Some(scratch) = &self.scratch {
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{}\"))\n",
                scratch.display()
            ));
        }
        text.push_str(&self.git_link_denies());
        text.push_str(&self.submodule_denies());
        text.push_str(&self.nested_git_denies());
        if let Some(checkout) = &self.checkout {
            text.push_str(&checkout.object_denies());
            text.push_str(&format!(
                "(deny file-write*\n  (literal \"{}/HEAD\"))\n",
                checkout.admin.display()
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
        for socket in &self.reached {
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
        assert_eq!(
            text.matches("unix-socket").count(),
            2 * self.sockets.len() + self.served.len() + self.reached.len()
        );
        let mut last: Vec<String> = self
            .read_only
            .iter()
            .map(|folder| format!("(deny file-write* (subpath \"{}\"))", folder.display()))
            .collect();
        last.push(launch);
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
        assert!(text.len() <= PROFILE_BYTES_MAX);
        text
    }

    /// The Seatbelt rules that keep the owner's git reading the metadata the engine made.
    /// No agent may move the project folder, the git worktrees folders or a worktree, which
    /// would carry protected files out of their denied paths and back. A worktree's `.git`
    /// file and its `commondir` and `gitdir` files may be created once, as `git worktree add`
    /// does, but never rewritten or removed, and no `config.worktree` may be written.
    /// Input: the policy. Output: the rule text.
    fn git_link_denies(&self) -> String {
        let project = self.project_root.display();
        let pattern = seatbelt_pattern(&self.project_root);
        let admin = format!("{pattern}/\\.git/worktrees/[^/]+");
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut moves = format!(
            "(deny file-write-unlink\n  (literal \"{project}\")\n  (literal \"{project}/.git/worktrees\")\n  (literal \"{project}/worktrees\")\n  (literal \"{project}/.crazyagent/worktrees\")\n  (regex #\"^{admin}$\")\n  (regex #\"^{checkouts}$\")"
        );
        let mut links = format!(
            "(deny file-write-unlink file-write-data\n  (regex #\"^{admin}/(commondir|gitdir)$\")\n  (regex #\"^{checkouts}/\\.git$\")"
        );
        for worktree in &self.worktrees {
            moves.push_str(&format!("\n  (literal \"{}\")", worktree.display()));
            links.push_str(&format!(
                "\n  (literal \"{}\")",
                worktree.join(".git").display()
            ));
        }
        let text = format!(
            "{moves})\n{links})\n(deny file-write*\n  (regex #\"^{admin}/config\\.worktree$\"))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 5);
        text
    }

    /// The Seatbelt rule that keeps the submodule store out of agent hands. Git keeps a
    /// submodule's own repository, with the config and hooks a later git would run, under
    /// `.git/modules`; no agent may write there, in the common git folder or a worktree
    /// admin folder, so none can plant a submodule whose metadata the engine or the owner
    /// then enters. The `.gitmodules` file and gitlinks stay writable tracked content: the
    /// engine's own git ignores them with `--ignore-submodules` and never recurses. The
    /// name matches in any letter case, since the project disk may be case insensitive.
    /// Input: the policy. Output: the rule text.
    fn submodule_denies(&self) -> String {
        let pattern = seatbelt_pattern(&self.project_root);
        let dot_git = "\\.[gG][iI][tT]";
        let modules = "[mM][oO][dD][uU][lL][eE][sS]";
        let text = format!(
            "(deny file-write*\n  (regex #\"^{pattern}/{dot_git}(/worktrees/[^/]+)?/{modules}(/.*)?$\"))\n"
        );
        assert_eq!(text.matches("(regex #\"^/").count(), 1);
        text
    }

    /// The Seatbelt rule that keeps an agent from planting a nested git repository inside a
    /// writable working tree, which the owner's own unsandboxed git (its terminal or the
    /// V2 IDE) would later enter as a submodule and run under the agent's own config (SM1).
    /// No entry named `.git`, in any letter case, as a folder, file or link, may be created
    /// below a writable working tree, except that tree's own top-level `.git` and the
    /// engine's worktree links. The project's own `.git` and each worktree's own `.git` sit
    /// at a tree's root, not below it, so the `.+/` guard never matches them. A whole folder
    /// already holding a `.git` could once be renamed in as a path-blind move Seatbelt
    /// cannot inspect; that source is removed by also denying `.git` at any depth, top level
    /// included, in every non-working-tree writable root (the admin folder, object store,
    /// home, temp and scratch), so no agent can create a repository anywhere to rename from
    /// (SM1-R). Input: the policy. Output: the rule text.
    fn nested_git_denies(&self) -> String {
        let dot_git = "\\.[gG][iI][tT]";
        let (working, other) = self.git_deny_roots();
        let mut nested = String::new();
        for root in &working {
            nested.push_str(&format!(
                "\n      (regex #\"^{}/.+/{dot_git}(/.*)?$\")",
                seatbelt_pattern(root)
            ));
        }
        for root in &other {
            nested.push_str(&format!(
                "\n      (regex #\"^{}/(.+/)?{dot_git}(/.*)?$\")",
                seatbelt_pattern(root)
            ));
        }
        let pattern = seatbelt_pattern(&self.project_root);
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut exempt = format!("\n    (require-not (regex #\"^{checkouts}/{dot_git}$\"))");
        if let Some(clone) = &self.clone {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/{}/[^/]+/{dot_git}$\"))",
                seatbelt_pattern(clone),
                regex_escape(crate::agent_git::TEAMMATES_FOLDER)
            ));
        }
        if let Some(setup) = &self.setup {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/[^/]+/{dot_git}$\"))",
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
                    + 1
                    + usize::from(self.clone.is_some())
                    + usize::from(self.setup.is_some())
        );
        assert!(text.ends_with(")\n"));
        text
    }

    /// Splits the agent's writable roots into the working-tree roots, where the root's own
    /// top-level `.git` is a legitimate repository link the engine adds, and every other
    /// writable root, where no `.git` may ever appear. A `.git` anywhere below a working
    /// tree, and a `.git` at any depth (top level included) in an admin folder, object
    /// store, home, temp or scratch root, or a team folder a native lead shares with its
    /// teammates, is denied so no agent can create a repository to rename whole into a
    /// worktree (OP-9 r2).
    fn git_deny_roots(&self) -> (Vec<&Path>, Vec<&Path>) {
        let mut working: Vec<&Path> = Vec::new();
        let mut other: Vec<&Path> = Vec::new();
        assert!(self.confinements() <= 1);
        match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => {
                working.push(checkout.worktree.as_path());
                other.push(checkout.admin.as_path());
                other.push(checkout.objects.as_path());
            }
            (None, Some(clone), _) => working.push(clone.as_path()),
            (None, None, Some(setup)) => {
                working.push(setup.folder.as_path());
                other.push(setup.records.as_path());
            }
            (None, None, None) => working.push(self.project_root.as_path()),
        }
        other.push(self.agent_home.as_path());
        other.push(self.agent_tmp.as_path());
        if let Some(scratch) = &self.scratch {
            other.push(scratch.as_path());
        }
        other.extend(self.shared.iter().map(PathBuf::as_path));
        for worktree in &self.worktrees {
            working.push(worktree.as_path());
        }
        (working, other)
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses.
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
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&staging)?;
        let written = file
            .write_all(self.profile().as_bytes())
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
    /// temp folder when granted, else the temp dir), PATH with the owner's rust proxies, the
    /// C locale and [`toolchains::agent_env`]. A harness's PATH also starts with the agent's
    /// own `getconf` and `git` ([`toolchains::with_agent_git`]); an engine command's never
    /// does. Takes whether the command is a harness. Returns the variables.
    fn base_env(&self, harness: bool) -> Vec<(&'static str, OsString)> {
        let homes = toolchains::ToolHomes::at(&self.owner_home);
        let tmp = self.scratch.as_ref().unwrap_or(&self.agent_tmp);
        let path = toolchains::agent_path(&homes, AGENT_PATH);
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
        assert_eq!(command.get_program(), SANDBOX_EXEC);
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

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux, AppContainer with a job
/// object on Windows.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
    unconfined: bool,
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
    /// [`windows::WindowsSandbox::install`]). Output: the sandbox bound to that profile, the
    /// install refusal, or `Unavailable` where this system has no boundary.
    pub fn install(sandbox: ProjectSandbox, profile: &Path) -> Result<Self, SandboxError> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.hidden.is_empty() || !sandbox.kept_out.is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(target_os = "windows")]
        {
            let helper = windows::existing_helper(
                &std::env::current_exe()?.with_file_name(windows::HELPER_NAME),
            )?;
            let grants = windows::grants_folder()?;
            let windows =
                windows::WindowsSandbox::install(sandbox.clone(), &helper, profile, &grants)?;
            assert!(profile.is_file());
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
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
                linux::LinuxSandbox::detect(sandbox.clone(), &helper)?
            };
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
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
    /// objects the owner and the other agents share: no write in `info/` or `pack/`, no
    /// new, moved or removed fan-out folder, no new object file by any route (create,
    /// rename or link) and no rewrite, removal or replacement of one, and no symlink
    /// anywhere in the store. Git reads a loose object before a packed one without checking
    /// its bytes, so any object an agent could add would stand in for the owner's own;
    /// object writes are the engine's, at turn end. Seatbelt regexes take no `{n}` counts,
    /// so the hex digits repeat. Input: the checkout. Output: the rule text.
    fn object_denies(&self) -> String {
        const HEX: &str = "[0-9a-fA-F]";
        let objects = self.objects.display();
        let pattern = seatbelt_pattern(&self.objects);
        let text = format!(
            "(deny file-write*\n  (subpath \"{objects}/info\")\n  (subpath \"{objects}/pack\"))\n(deny file-write-unlink\n  (literal \"{objects}\")\n  (regex #\"^{pattern}/{HEX}{HEX}$\"))\n(deny file-write-unlink file-write-data\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (regex #\"^{pattern}/{HEX}{HEX}$\")\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (require-all (subpath \"{objects}\") (vnode-type SYMLINK)))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 4);
        assert!(text.contains("(vnode-type SYMLINK)"));
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
    let fallback = format!("{}/{}", seatbelt_pattern(clone), any_case(".git.git"));
    let git = format!("{}/{}", seatbelt_pattern(clone), any_case(".git"));
    let objects = format!("{git}/{}", any_case("objects"));
    let info = format!("{objects}/{}", any_case("info"));
    let pack = format!("{objects}/{}", any_case("pack"));
    let index = format!("{pack}/{}", any_case("multi-pack-index"));
    let text = format!(
        "(deny file-write*\n  (regex #\"^{git}$\")\n  (regex #\"^{fallback}(/.*)?$\")\n  (regex #\"^{pack}$\")\n  (regex #\"^{objects}/[0-9a-fA-F][0-9a-fA-F]$\")\n  (regex #\"^{index}\")\n  (regex #\"^{info}/({}|{})$\")\n  (regex #\"^{git}/{}$\"))\n(deny file-write-unlink\n  (regex #\"^{objects}$\")\n  (regex #\"^{info}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{objects}/\") (vnode-type SYMLINK)))\n",
        any_case("alternates"),
        any_case("http-alternates"),
        any_case("commondir")
    );
    assert_eq!(text.matches("(regex #\"^/").count(), 10);
    assert!(text.contains("(vnode-type SYMLINK)"));
    text
}

/// The writes a clone lead's teammates may not make in the lead's clone (OP-9): anything but
/// the teammates' folder, the clone's object store, the inside of each worktree record git
/// keeps in `.git/worktrees`, and the refs and reflogs of `crazyagent/<task>--<name>`
/// teammate branches; nor the links that tie a teammate worktree to the clone, which the
/// engine writes with the lead's rights, each name in any letter case; nor the teammates'
/// folder entry, nor make, move or remove anything directly in it, such as a teammate's
/// folder, where the engine's lead-rights `git worktree add` writes (OP9B-C1). It must
/// follow every write allow, since the last match wins.
/// Input: the clone. Output: the rule text.
fn teammates_clone_denies(clone: &Path) -> String {
    let path = clone.display();
    let root = seatbelt_pattern(clone);
    let folder = crate::agent_git::TEAMMATES_FOLDER;
    let teammates = any_case(folder);
    let git = any_case(".git");
    let branches = regex_escape(crate::agent_git::LINKED_PREFIX);
    let text = format!(
        "(deny file-write*\n  (require-all\n    (subpath \"{path}\")\n    (require-not (subpath \"{path}/{folder}\"))\n    (require-not (subpath \"{path}/.git/objects\"))\n    (require-not (regex #\"^{root}/\\.git/worktrees/[^/]+/.+$\"))\n    (require-not (regex #\"^{root}/\\.git/(logs/)?refs/heads/{branches}[a-z0-9-]+--[a-z0-9-]+(\\.lock)?$\"))))\n(deny file-write*\n  (regex #\"^{root}/{teammates}$\")\n  (regex #\"^{root}/{teammates}/[^/]+/{git}$\")\n  (regex #\"^{root}/{git}/{}/[^/]+/({}|{})$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
        any_case("worktrees"),
        any_case("commondir"),
        any_case("gitdir")
    );
    assert_eq!(text.matches("(regex #\"^/").count(), 6);
    assert_eq!(text.matches("(subpath \"/").count(), 3);
    text
}

/// Spells plain text as a Seatbelt regex that matches it in any letter case, for names a
/// case-insensitive volume finds however they are typed. APFS folds case by Unicode rules,
/// so `k` also matches U+212A KELVIN SIGN and `s` U+017F LATIN SMALL LETTER LONG S, the
/// only other letters that fold to ASCII; those two are spelled as groups, so each matches
/// its whole UTF-8 sequence (LL2S-2). Input: text without a quote or backslash. Output: the
/// pattern text, without anchors.
fn any_case(text: &str) -> String {
    regex_escape(text)
        .chars()
        .map(|character| {
            if character.eq_ignore_ascii_case(&'k') {
                "(k|K|\u{212a})".to_owned()
            } else if character.eq_ignore_ascii_case(&'s') {
                "(s|S|\u{17f})".to_owned()
            } else if character.is_ascii_alphabetic() {
                format!(
                    "[{}{}]",
                    character.to_ascii_lowercase(),
                    character.to_ascii_uppercase()
                )
            } else {
                character.to_string()
            }
        })
        .collect()
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
/// instructions or Claude Code's `settings.json`, none under `skills` but Codex's own
/// `.system` skills, which Codex installs at its start and the engine removes before each
/// Codex launch, and no move or removal of the `skills` folder, so no folder built elsewhere
/// is moved in, nor a flag on `skills` or anywhere in `.system` that would keep the engine
/// from removing it (CTX-M1 r1), each name in any letter case. Its own run cannot change the
/// text it runs with until the next launch rewrites it. They must follow every write allow.
/// Input: the home. Output: the rule text.
fn launch_denies(home: &Path) -> String {
    use crate::agent_home::Harness;
    use crate::launch_context::STATE;
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
    ];
    let files = [
        Harness::Claude.instruction_file(),
        Harness::Codex.instruction_file(),
        "AGENTS.override.md",
        "config.toml",
        "settings.json",
    ];
    let mut text = String::from("(deny file-write*\n");
    for folder in folders {
        text.push_str(&format!(
            "  (regex #\"^{home}/{}(/.*)?$\")\n",
            any_case(folder)
        ));
    }
    for file in files {
        text.push_str(&format!("  (regex #\"^{home}/{}$\")\n", any_case(file)));
    }
    text.push_str(&format!(
        "  (regex #\"^{skills}/{}\"))\n(deny {PINNED_WRITES} (regex #\"^{skills}$\"))\n(deny file-write-flags (regex #\"^{skills}/{}(/.*)?$\"))\n",
        other_than(crate::launch_context::CODEX_SYSTEM_SKILLS),
        any_case(crate::launch_context::CODEX_SYSTEM_SKILLS)
    ));
    assert_eq!(text.matches("(regex #\"^/").count(), 15);
    text
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

/// Whether every deny rule comes after the profile's last write allow, since Seatbelt applies
/// the last matching rule and a deny a later allow overrides no longer holds (CTX-M1 F1).
/// Inputs: the profile text and the deny rules as written in it. Output: false when any
/// deny is missing or comes before a write allow.
fn denies_follow_write_allows(text: &str, denies: &[String]) -> bool {
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
#[cfg(any(target_os = "windows", test))]
fn windows_clean_text(text: &str) -> bool {
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

#[cfg(test)]
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
