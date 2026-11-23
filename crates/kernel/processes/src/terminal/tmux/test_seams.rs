use super::super::LeadPaths;
use super::{Confinement, TmuxHooks};
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::process::{Command, Stdio};

/// The folder of the engine root the engine's terminal sockets live in.
pub const FOLDER: &str = "terminal";

/// The system sandbox runner.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[cfg(target_os = "macos")]
macro_rules! warden_then_harness {
    () => {
        r#"if (defined(my $fd = delete $ENV{CRAZYAGENT_WARDEN_FD})) { my $engine = getppid(); $fd =~ /\A[0-9]{1,6}\z/ && $engine > 1 or die "crazyagent: no warden\n"; defined(my $warden = fork()) or die "crazyagent: no warden: $!\n"; if (!$warden) { POSIX::setsid(); POSIX::close($_) for 0, 1, 2; $SIG{PIPE} = "IGNORE"; open(my $line, "+<&=", $fd) or POSIX::_exit(1); my $armed = !kill(0, $engine) && $!{EPERM}; syswrite($line, $armed ? "A" : "U"); $armed or POSIX::_exit(0); my $byte; while (sysread($line, $byte, 1)) { if ($byte eq "S") { my $stopped = 1; for (1 .. 3) { $stopped &&= syscall(37, -1, POSIX::SIGSTOP(), 0) == 0; select(undef, undef, undef, 0.05) } syswrite($line, "s") if $stopped } elsif ($byte eq "C") { kill("CONT", -1); syswrite($line, "c") } } syswrite($line, "F"); kill(9, -1); POSIX::_exit(1) } POSIX::close($fd) } exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n""#
    };
}

#[cfg(target_os = "macos")]
/// The shim a kept pane runs inside its sandbox before the harness.
pub const PANE_SHIM: [&str; 5] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-e",
    warden_then_harness!(),
    "--",
];

#[cfg(target_os = "macos")]
/// A kept pane's first process, outside the sandbox, which ends every process it kept.
pub const KEEPER: [&str; 6] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-MSocket",
    "-e",
    r#"$^F = 1023; my $why = ""; my ($every, $late, $record) = split(/ /, delete($ENV{CRAZYAGENT_KEEPER_DEADLINE}) // "", 3); !defined($record) || ($every =~ /\A[1-9][0-9]{0,3}\z/ && $late =~ /\A[1-9][0-9]{0,2}\z/ && $record =~ m{\A/}) or die "crazyagent: no deadline\n"; my $state = delete($ENV{CRAZYAGENT_KEEPER_STATE}); if (@ARGV > 2 && $ARGV[1] eq "/usr/bin/sandbox-exec") { !defined($state) or die "crazyagent: two state records\n"; $state = shift(@ARGV) } !defined($state) || $state =~ m{\A/[^\n\0]{1,1000}\z} or die "crazyagent: no state record\n"; my ($next, $armed, $missed, $want) = (time + ($every // 0), 0, 0, ""); $SIG{$_} = "IGNORE" for qw(INT QUIT TSTP TTIN TTOU PIPE USR1 USR2); if (defined($state)) { $SIG{USR1} = sub { $want = "S" }; $SIG{USR2} = sub { $want = "C" } } $SIG{HUP} = sub { $why ||= "HUP" }; $SIG{TERM} = sub { $why ||= "TERM" }; $SIG{CHLD} = sub { 1 }; socketpair(my $keep, my $line, AF_UNIX, SOCK_STREAM, PF_UNSPEC) or die "crazyagent: no warden: $!\n"; defined(my $harness = fork()) or die "crazyagent: no warden: $!\n"; if (!$harness) { $SIG{$_} = "DEFAULT" for qw(INT QUIT TSTP TTIN TTOU PIPE HUP TERM CHLD USR1 USR2); close($keep); $ENV{CRAZYAGENT_WARDEN_FD} = fileno($line); exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n" } close($line); my ($seen, $open, $wanted) = ("", 1, ""); vec($wanted, fileno($keep), 1) = 1; until ($why || waitpid($harness, POSIX::WNOHANG()) > 0) { if (length($want)) { syswrite($keep, $want) if $open; $want = "" } my $ready = $open ? $wanted : ""; if (select($ready, undef, undef, 1) > 0 && defined(my $got = sysread($keep, my $more, 8))) { $open = 0 if !$got; $seen .= $more // ""; if ($seen =~ s/([sc]+)\z// && defined($state)) { my $now = uc(substr($1, -1)); unlink("$state.new"); if (sysopen(my $out, "$state.new", POSIX::O_WRONLY() | POSIX::O_CREAT() | POSIX::O_EXCL(), 0600)) { syswrite($out, $now); close($out); rename("$state.new", $state) or unlink("$state.new") } } } next unless defined($record) && time >= $next; $next = time + $every; my $ahead = 0; if (my @at = lstat($record)) { if (-f _ && $at[4] == $< && $at[7] <= 19 && open(my $in, "<", $record)) { $ahead = (<$in> // "") =~ /\A([0-9]{1,19})\z/ && time < $1 } } if ($ahead) { ($armed, $missed) = (1, 0) } elsif ($armed && ++$missed >= $late) { $why = "LATE" } } kill("KILL", $harness) if $why eq "LATE"; shutdown($keep, 1); my $report = ""; my $until = time + 4; while ($open && time < $until && length($report) < 8) { select(my $ready = $wanted, undef, undef, 0.25) > 0 or next; sysread($keep, my $more, 8) or last; $report .= $more } my $said = $seen . $report; POSIX::_exit(0) if $said =~ /\AA[sc]*F\z/ || $said eq "U" || $why eq "HUP"; POSIX::_exit(3) if $why eq "LATE"; $SIG{TERM} = "IGNORE"; $SIG{HUP} = "DEFAULT"; sleep(3600) while 1"#,
    "--",
];

#[cfg(target_os = "macos")]
/// The variable a lead server's keeper reads its deadline from.
pub const KEEPER_DEADLINE: &str = "CRAZYAGENT_KEEPER_DEADLINE";

#[cfg(target_os = "macos")]
/// The variable naming a lead server keeper's freeze record.
pub const KEEPER_STATE: &str = "CRAZYAGENT_KEEPER_STATE";

#[cfg(target_os = "macos")]
const KEEPER_STATE_BYTES_MAX: usize = 1001;

#[cfg(target_os = "macos")]
/// Puts a pane's sandboxed command under the keeper and shim. Inputs: the argv and the
/// freeze record. Output: the kept argv, or none for any other argv or record.
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

#[cfg(target_os = "macos")]
/// Takes a kept pane command back out of the keeper and shim. Input: an argv. Output: the
/// plain argv, or none when the argv is not one `kept_pane` builds.
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

/// Whether a variable name may never reach a pane. Input: the name. Output: true when
/// refused.
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

/// The confinement a row's host is opened with: the system sandbox runner, the keeper and
/// shim words, the pane variable gate and the keeper's undo, copied from the sandbox the
/// engine runs. Output: the value.
pub fn test_confinement() -> Confinement {
    Confinement {
        sandbox_exec: SANDBOX_EXEC,
        #[cfg(target_os = "macos")]
        keeper: KEEPER,
        #[cfg(target_os = "macos")]
        keeper_deadline_variable: KEEPER_DEADLINE,
        #[cfg(target_os = "macos")]
        keeper_state_variable: KEEPER_STATE,
        #[cfg(target_os = "macos")]
        pane_shim: PANE_SHIM,
        variable_allowed: |name, _| !agent_env_refused(name),
        #[cfg(target_os = "macos")]
        unkept_pane,
    }
}

/// The team hooks a row's host is opened with: the socket peer and the parent map read for
/// real, and the team state layout's paths. Output: the hooks.
pub fn test_hooks() -> TmuxHooks {
    TmuxHooks {
        peer_pid: test_peer_pid,
        process_parents: test_process_parents,
        agent_profile: test_layout::agent_profile,
        agent_teammates_profile: test_layout::agent_teammates_profile,
        agent_servers_state: test_layout::agent_servers_state,
        lead: test_lead_paths(),
    }
}

/// The lead socket and record paths the kernel rows use, laid out as the team's state
/// names them. Output: the paths.
pub fn test_lead_paths() -> LeadPaths {
    LeadPaths {
        agent_home: test_layout::agent_home,
        tmp_path: test_layout::tmp_path,
        agent_teammates_socket_name: test_layout::agent_teammates_socket_name,
        agent_teammates_server_pid: test_layout::agent_teammates_server_pid,
        agent_servers_deadline: test_layout::agent_servers_deadline,
    }
}

/// The team state layout the kernel rows open servers in: byte-equal copies of the paths
/// crazyagent's team runtime and sandbox name, which its own rows hold equal.
pub mod test_layout {
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    #[cfg(target_os = "macos")]
    const TMP_PARENT: &str = "/private/tmp";
    #[cfg(not(target_os = "macos"))]
    const TMP_PARENT: &str = "/tmp";
    const TMP_PREFIX: &str = "ca-";
    const TMP_HASH_BYTES: usize = 8;

    /// An agent's sandbox profile. Inputs: the team state folder and the agent id. Output:
    /// `<state>/profiles/agent-<id>.sb`.
    pub fn agent_profile(state: &Path, id: i64) -> PathBuf {
        state.join("profiles").join(format!("agent-{id}.sb"))
    }

    /// A lead's teammates' sandbox profile. Inputs: the state folder and the lead's id.
    /// Output: `<state>/profiles/agent-<id>-teammates.sb`.
    pub fn agent_teammates_profile(state: &Path, id: i64) -> PathBuf {
        state
            .join("profiles")
            .join(format!("agent-{id}-teammates.sb"))
    }

    /// A lead server's keeper state record. Inputs: the state folder, the lead's id and
    /// whether it is the teammates' server. Output: the path.
    pub fn agent_servers_state(state: &Path, id: i64, mates: bool) -> PathBuf {
        let server = if mates { "mates" } else { "own" };
        state
            .join("profiles")
            .join(format!("agent-{id}-{server}.state"))
    }

    /// An agent's home folder. Inputs: the state folder and the agent id. Output: the path.
    pub fn agent_home(state: &Path, id: i64) -> PathBuf {
        state
            .join("agents")
            .join(format!("agent-{id}"))
            .join("home")
    }

    /// The record naming a lead's teammates' socket. Inputs: the state folder and the
    /// lead's id. Output: the path.
    pub fn agent_teammates_socket_name(state: &Path, id: i64) -> PathBuf {
        state
            .join("profiles")
            .join(format!("agent-{id}-teammates.socket"))
    }

    /// The record of a lead's teammates' server pid. Inputs: the state folder and the
    /// lead's id. Output: the path.
    pub fn agent_teammates_server_pid(state: &Path, id: i64) -> PathBuf {
        state
            .join("profiles")
            .join(format!("agent-{id}-teammates-server.pid"))
    }

    /// The record of a lead's servers' deadline. Inputs: the state folder and the lead's
    /// id. Output: the path.
    pub fn agent_servers_deadline(state: &Path, id: i64) -> PathBuf {
        state
            .join("profiles")
            .join(format!("agent-{id}-servers.deadline"))
    }

    /// An agent's short temp folder: `ca-` and 16 hex digits of its home's hash in the
    /// system temp folder. Input: the home. Output: the path.
    pub fn tmp_path(home: &Path) -> PathBuf {
        let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
        let hash: String = digest[..TMP_HASH_BYTES]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = Path::new(TMP_PARENT).join(format!("{TMP_PREFIX}{hash}"));
        assert_eq!(
            path.as_os_str().len(),
            TMP_PARENT.len() + 1 + TMP_PREFIX.len() + 2 * TMP_HASH_BYTES
        );
        path
    }
}

/// The pid at the other end of a socket, read from the kernel. Input: the stream. Output:
/// the pid, or none when it cannot be read.
#[cfg(target_os = "macos")]
fn test_peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .ok()
        .and_then(|pid| u32::try_from(pid).ok())
}

/// The pid at the other end of a socket, read from the kernel. Input: the stream. Output:
/// the pid, or none when it cannot be read.
#[cfg(target_os = "linux")]
fn test_peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .and_then(|credentials| u32::try_from(credentials.pid()).ok())
}

/// The pid at the other end of a socket where none is read. Input: unused. Output: none.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn test_peer_pid(_stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    None
}

/// Every process's parent, from one `ps` table. Output: the map, or none when the table
/// cannot be read.
fn test_process_parents() -> Option<std::collections::HashMap<u32, u32>> {
    let output = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid="])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?;
    if !output.status.success() || output.stdout.len() > 1 << 22 {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            fields.next().is_none().then_some((pid, parent))
        })
        .collect()
}
