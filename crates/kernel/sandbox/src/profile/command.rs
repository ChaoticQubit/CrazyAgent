use crate::error::SandboxError;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::path::Path;
use std::process::Command;

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
        crate::windows::helper_beside_engine().is_ok_and(|helper| program == helper.as_os_str())
    }
}

/// The end of the agent shims: forks the run's warden when handed a [`warden::WARDEN_FD`], keeping
/// that descriptor and variable from the harness, then replaces the shim with the harness program
/// and its arguments. Asked `S` on its line, the warden stops every other process in the sandbox
/// three times over, through the kill call that spares its sender, and answers `s` only when every
/// pass took; asked `C`, it continues them and answers `c`. At the line's end it kills them all, as
/// before.
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

/// A pane's first process, outside the sandbox, followed by the sandboxed command it keeps. It
/// starts that command holding one end of a socket pair as the run's warden descriptor and waits
/// until the command exits or the pane is asked to stop with hangup or terminate. Then it shuts its
/// end, so the warden kills every process left in the sandbox, sessions of their own included, and
/// it exits only once the warden confirmed that or found signals not confined; otherwise it keeps
/// the pane open, ignoring terminate, until a hangup. A confining profile keeps every agent signal
/// from it, and the terminal keys that signal the pane's group do not end it. Given
/// [`KEEPER_DEADLINE`], as a native lead's server keeper is, it also reads the engine's deadline
/// record and, once one read found the deadline ahead, stops after that many reads in a row found
/// it passed or unreadable: it kills its command, the server, itself from outside the sandbox, so a
/// killed warden cannot keep the server, then has the warden end the rest, and exits with status 3
/// when the warden does not confirm. Given the engine's freeze record, as the word before a kept
/// pane's `sandbox-exec` (see [`kept_pane`]) or in [`KEEPER_STATE`] for a native lead's server
/// keeper, never both, user signal 1 asks the warden to stop every process in the sandbox and user
/// signal 2 to continue them, and each answer is written to that record as `S` or `C`; without it
/// both signals are ignored. Its deadline reads count wall time, so answers do not hasten them.
#[cfg(target_os = "macos")]
pub const KEEPER: [&str; 6] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-MSocket",
    "-e",
    r#"$^F = 1023; my $why = ""; my ($every, $late, $record) = split(/ /, delete($ENV{CRAZYAGENT_KEEPER_DEADLINE}) // "", 3); !defined($record) || ($every =~ /\A[1-9][0-9]{0,3}\z/ && $late =~ /\A[1-9][0-9]{0,2}\z/ && $record =~ m{\A/}) or die "crazyagent: no deadline\n"; my $state = delete($ENV{CRAZYAGENT_KEEPER_STATE}); if (@ARGV > 2 && $ARGV[1] eq "/usr/bin/sandbox-exec") { !defined($state) or die "crazyagent: two state records\n"; $state = shift(@ARGV) } !defined($state) || $state =~ m{\A/[^\n\0]{1,1000}\z} or die "crazyagent: no state record\n"; my ($next, $armed, $missed, $want) = (time + ($every // 0), 0, 0, ""); $SIG{$_} = "IGNORE" for qw(INT QUIT TSTP TTIN TTOU PIPE USR1 USR2); if (defined($state)) { $SIG{USR1} = sub { $want = "S" }; $SIG{USR2} = sub { $want = "C" } } $SIG{HUP} = sub { $why ||= "HUP" }; $SIG{TERM} = sub { $why ||= "TERM" }; $SIG{CHLD} = sub { 1 }; socketpair(my $keep, my $line, AF_UNIX, SOCK_STREAM, PF_UNSPEC) or die "crazyagent: no warden: $!\n"; defined(my $harness = fork()) or die "crazyagent: no warden: $!\n"; if (!$harness) { $SIG{$_} = "DEFAULT" for qw(INT QUIT TSTP TTIN TTOU PIPE HUP TERM CHLD USR1 USR2); close($keep); $ENV{CRAZYAGENT_WARDEN_FD} = fileno($line); exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n" } close($line); my ($seen, $open, $wanted) = ("", 1, ""); vec($wanted, fileno($keep), 1) = 1; until ($why || waitpid($harness, POSIX::WNOHANG()) > 0) { if (length($want)) { syswrite($keep, $want) if $open; $want = "" } my $ready = $open ? $wanted : ""; if (select($ready, undef, undef, 1) > 0 && defined(my $got = sysread($keep, my $more, 8))) { $open = 0 if !$got; $seen .= $more // ""; if ($seen =~ s/([sc]+)\z// && defined($state)) { my $now = uc(substr($1, -1)); unlink("$state.new"); if (sysopen(my $out, "$state.new", POSIX::O_WRONLY() | POSIX::O_CREAT() | POSIX::O_EXCL(), 0600)) { syswrite($out, $now); close($out); rename("$state.new", $state) or unlink("$state.new") } } } next unless defined($record) && time >= $next; $next = time + $every; my $ahead = 0; if (my @at = lstat($record)) { if (-f _ && $at[4] == $< && $at[7] <= 19 && open(my $in, "<", $record)) { $ahead = (<$in> // "") =~ /\A([0-9]{1,19})\z/ && time < $1 } } if ($ahead) { ($armed, $missed) = (1, 0) } elsif ($armed && ++$missed >= $late) { $why = "LATE" } } kill("KILL", $harness) if $why eq "LATE"; shutdown($keep, 1); my $report = ""; my $until = time + 4; while ($open && time < $until && length($report) < 8) { select(my $ready = $wanted, undef, undef, 0.25) > 0 or next; sysread($keep, my $more, 8) or last; $report .= $more } my $said = $seen . $report; POSIX::_exit(0) if $said =~ /\AA[sc]*F\z/ || $said eq "U" || $why eq "HUP"; POSIX::_exit(3) if $why eq "LATE"; $SIG{TERM} = "IGNORE"; $SIG{HUP} = "DEFAULT"; sleep(3600) while 1"#,
    "--",
];

/// The variable a native lead's server keeper takes its deadline reads from: the seconds between
/// two reads, how many reads in a row past the deadline end the server, and the engine's absolute
/// deadline record, separated by single spaces. The [`KEEPER`] removes it before the command
/// starts; a kept pane has none.
#[cfg(target_os = "macos")]
pub const KEEPER_DEADLINE: &str = "CRAZYAGENT_KEEPER_DEADLINE";

/// The variable that names the engine's absolute freeze record to a native lead's server
/// [`KEEPER`], whose warden then stops and continues the sandbox on the engine's signals. The
/// keeper removes it before the command starts. A pane's variables never carry it, since the pane
/// variable gate refuses the engine's own; a kept pane takes the record as an argument instead (see
/// [`kept_pane`]).
#[cfg(target_os = "macos")]
pub const KEEPER_STATE: &str = "CRAZYAGENT_KEEPER_STATE";

/// The longest freeze record a [`KEEPER`] takes, in bytes: its leading slash and at most
/// 1000 more, as the keeper's own check reads it.
#[cfg(target_os = "macos")]
const KEEPER_STATE_BYTES_MAX: usize = 1001;

/// Puts a pane's sandboxed command under the [`KEEPER`] and the [`PANE_SHIM`], so stopping the pane
/// ends every process its agent started. Inputs: the argv `sandbox-exec -f <profile> <program>
/// <arguments>` and the engine's freeze record, which goes to the keeper as the word before
/// `sandbox-exec` and never reaches the command. Output: the kept argv, or none for any other argv
/// or a record that is not an absolute path of at most [`KEEPER_STATE_BYTES_MAX`] bytes on one
/// line.
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

/// The `sandbox-exec` command of [`agent_command`] around a built environment. Inputs: the
/// installed profile, the absolute program and the checked variables. Output: the command.
#[cfg(unix)]
pub fn confined_command(profile: &Path, program: &Path, env: Vec<(OsString, OsString)>) -> Command {
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
    command
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

/// What a run's stop checks, once its process group is killed, of the processes the run left
/// outside that group: nothing where its boundary ends them all, its Full access lineage, or a
/// sandboxed lineage where a confined process that starts its own session outlives the group kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageCheck {
    Unneeded,
    FullAccess,
    Sandboxed,
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
