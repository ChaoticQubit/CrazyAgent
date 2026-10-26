use std::io;
use std::process::{Child, Command};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// The variable that hands the session shim the descriptor number of its warden's end.
pub const WARDEN_FD: &str = "CRAZYAGENT_WARDEN_FD";

/// How long [`Warden::fire`] waits for the warden to report that it stopped its run.
pub const FIRE_TIME: Duration = Duration::from_secs(2);

/// How long [`spawn`] waits for the warden's first report, which it sends once it has left
/// the harness group.
#[cfg(target_os = "macos")]
const ARM_TIME: Duration = Duration::from_secs(2);

#[cfg(target_os = "macos")]
const PROFILE_BYTES_MAX: u64 = 1024 * 1024;
#[cfg(target_os = "macos")]
const REPORT_BYTES_MAX: usize = 8;
#[cfg(target_os = "macos")]
const HEAR_INTERVAL: Duration = Duration::from_millis(1);
#[cfg(target_os = "macos")]
const CONFINED_SIGNALS: &str = "(allow signal (target same-sandbox))";

/// The engine's end of one run's warden: a process the session shim forks inside the run's
/// Seatbelt sandbox before it starts the harness. Once this end is shut, or closed, as the
/// kernel closes it when the engine dies, the warden sends SIGKILL to every process in that
/// sandbox, sessions that left the harness group included, and the profile keeps the signal
/// from every process outside it.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct Warden {
    engine: std::os::unix::net::UnixStream,
    report: Vec<u8>,
}

/// No warden exists off macOS: Bubblewrap's PID namespace and the Windows job end a run's
/// processes themselves, and the Linux Landlock helper keeps none.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub enum Warden {}

/// Starts an agent command, with a warden when the command runs the session shim under
/// `sandbox-exec` with a profile that confines signals to its own sandbox, and waits for the
/// warden's first report, so no signal to the harness group sent after this returns can
/// reach the warden. Input: the command. Output: the child and its warden, or no warden for
/// any other command, inside which a sandbox-wide kill could reach the owner's own processes.
#[cfg(target_os = "macos")]
pub fn spawn(command: &mut Command) -> io::Result<(Child, Option<Warden>)> {
    use std::os::fd::{AsRawFd, OwnedFd};

    if !wardable(command)? {
        return crate::spawn_gate::spawn(command).map(|child| (child, None));
    }
    let (engine, warden) = crate::spawn_gate::unix_pair_std()?;
    let handed = OwnedFd::from(warden);
    command.env(WARDEN_FD, handed.as_raw_fd().to_string());
    let child = crate::spawn_gate::spawn_handing(command, handed)?;
    Ok((child, Some(Warden::reported(engine, ARM_TIME))))
}

/// Starts an agent command where no warden exists. Input: the command. Output: the child
/// and no warden.
#[cfg(not(target_os = "macos"))]
pub fn spawn(command: &mut Command) -> io::Result<(Child, Option<Warden>)> {
    crate::spawn_gate::spawn(command).map(|child| (child, None))
}

/// Tells whether a command starts the session shim under `sandbox-exec -f <profile>` whose
/// profile denies by default and allows signals only inside its own sandbox. Input: the
/// command. Output: true only then, or the error reading the profile.
#[cfg(target_os = "macos")]
fn wardable(command: &Command) -> io::Result<bool> {
    use std::io::Read;

    let arguments: Vec<&std::ffi::OsStr> = command.get_args().collect();
    let shim = arguments.get(2..2 + super::SESSION_SHIM.len());
    if command.get_program() != super::SANDBOX_EXEC
        || arguments.first().copied() != Some(std::ffi::OsStr::new("-f"))
        || shim.is_none_or(|shim| {
            shim.iter()
                .zip(super::SESSION_SHIM)
                .any(|(at, part)| *at != part)
        })
    {
        return Ok(false);
    }
    let mut text = String::new();
    std::fs::File::open(arguments[1])?
        .take(PROFILE_BYTES_MAX + 1)
        .read_to_string(&mut text)?;
    Ok(
        u64::try_from(text.len()).is_ok_and(|length| length <= PROFILE_BYTES_MAX)
            && confines_signals(&text),
    )
}

/// Tells whether a Seatbelt profile denies by default and names signals only in the one rule
/// that keeps them inside the sandbox. Input: the profile text. Output: true only then.
#[cfg(target_os = "macos")]
fn confines_signals(profile: &str) -> bool {
    let lines: Vec<&str> = profile.lines().map(str::trim).collect();
    lines.contains(&"(deny default)")
        && !profile.contains("(allow default")
        && lines
            .iter()
            .filter(|line| line.contains("signal"))
            .all(|line| *line == CONFINED_SIGNALS)
}

/// What one bounded wait on the engine's end of a warden found.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum Heard {
    Bytes(usize),
    Closed,
    Silent,
}

/// Waits until `deadline` for the warden to write or to close its end, polling the
/// nonblocking end every [`HEAR_INTERVAL`], since macOS refuses a read timeout on a socket
/// whose peer has closed. Inputs: the nonblocking engine end, the buffer and the deadline.
/// Output: how many bytes came, that the warden closed its end, or silence until the
/// deadline or a read error.
#[cfg(target_os = "macos")]
fn hear(
    mut engine: &std::os::unix::net::UnixStream,
    buffer: &mut [u8],
    deadline: Instant,
) -> Heard {
    use std::io::Read;

    assert!(!buffer.is_empty());
    loop {
        match engine.read(buffer) {
            Ok(0) => return Heard::Closed,
            Ok(count) => return Heard::Bytes(count),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(HEAR_INTERVAL);
            }
            Err(_) => return Heard::Silent,
        }
    }
}

#[cfg(target_os = "macos")]
impl Warden {
    /// Waits up to `wait` for the warden's first report, which it writes only after it has
    /// started its own session. Inputs: the engine's end and the wait. Output: the warden
    /// holding that report, or none when it did not come in time, which [`Warden::fire`]
    /// then reports as unconfirmed.
    fn reported(engine: std::os::unix::net::UnixStream, wait: Duration) -> Self {
        let mut report = Vec::new();
        let mut first = [0_u8; 1];
        if engine.set_nonblocking(true).is_ok()
            && hear(&engine, &mut first, Instant::now() + wait) == Heard::Bytes(1)
        {
            report.push(first[0]);
        }
        assert!(report.len() <= 1);
        Self { engine, report }
    }

    /// Shuts the engine's end so the warden stops every process left in its run's sandbox,
    /// then waits up to `wait` for its report: armed, then fired, then closed as the kill
    /// reaches the warden too; a warden that already closed its end leaves its report to
    /// decide. Input: the wait. Output: Ok, `warden_unarmed` when the warden found signals
    /// not confined at start, or `warden_unconfirmed` otherwise.
    pub fn fire(self, wait: Duration) -> Result<(), &'static str> {
        let deadline = Instant::now() + wait;
        let Self { engine, mut report } = self;
        if engine.set_nonblocking(true).is_err() {
            return Err("warden_unconfirmed");
        }
        match engine.shutdown(std::net::Shutdown::Write) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
            Err(_) => return Err("warden_unconfirmed"),
        }
        let mut buffer = [0_u8; REPORT_BYTES_MAX];
        loop {
            match hear(&engine, &mut buffer, deadline) {
                Heard::Closed => break,
                Heard::Bytes(count) if report.len() + count <= REPORT_BYTES_MAX => {
                    report.extend_from_slice(&buffer[..count]);
                }
                Heard::Bytes(_) | Heard::Silent => return Err("warden_unconfirmed"),
            }
        }
        match report.as_slice() {
            b"AF" => Ok(()),
            b"U" => Err("warden_unarmed"),
            _ => Err("warden_unconfirmed"),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Warden {
    /// Cannot be called: no warden exists here. Input: the wait, unused.
    pub fn fire(self, _wait: Duration) -> Result<(), &'static str> {
        match self {}
    }
}

/// Fires a run's warden, when it has one, for up to [`FIRE_TIME`] and reports on stderr when
/// the warden does not confirm that it stopped the run's sandbox. Inputs: the warden and the
/// label of the run's owner, such as `harness`. Output: nothing.
pub fn fire_reported(warden: Option<Warden>, owner: &str) {
    if let Some(warden) = warden
        && let Err(code) = warden.fire(FIRE_TIME)
    {
        eprintln!("{owner}_warden_unconfirmed cause={code}");
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod fixture;
#[cfg(all(test, target_os = "macos"))]
mod tests;
