use std::io;
use std::process::{Child, Command};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// The variable that hands the session shim the descriptor number of its warden's end.
pub const WARDEN_FD: &str = "CRAZYAGENT_WARDEN_FD";

/// How long [`Warden::fire`] waits for the warden to report that it stopped its run.
pub const FIRE_TIME: Duration = Duration::from_secs(2);

#[cfg(target_os = "macos")]
const PROFILE_BYTES_MAX: u64 = 1024 * 1024;
#[cfg(target_os = "macos")]
const REPORT_BYTES_MAX: usize = 8;
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
}

/// No warden exists off macOS: Bubblewrap's PID namespace and the Windows job end a run's
/// processes themselves, and the Linux Landlock helper keeps none.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub enum Warden {}

/// Starts an agent command, with a warden when the command runs the session shim under
/// `sandbox-exec` with a profile that confines signals to its own sandbox. Input: the
/// command. Output: the child and its warden, or no warden for any other command, inside
/// which a sandbox-wide kill could reach the owner's own processes.
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
    Ok((child, Some(Warden { engine })))
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

#[cfg(target_os = "macos")]
impl Warden {
    /// Shuts the engine's end so the warden stops every process left in its run's sandbox,
    /// then waits up to `wait` for its report: armed, then fired, then closed as the kill
    /// reaches the warden too. Input: the wait. Output: Ok, `warden_unarmed` when the warden
    /// found signals not confined at start, or `warden_unconfirmed` otherwise.
    pub fn fire(self, wait: Duration) -> Result<(), &'static str> {
        use std::io::Read;

        let deadline = Instant::now() + wait;
        let mut engine = self.engine;
        if engine.shutdown(std::net::Shutdown::Write).is_err() {
            return Err("warden_unconfirmed");
        }
        let mut report = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || engine.set_read_timeout(Some(left)).is_err() {
                return Err("warden_unconfirmed");
            }
            let mut buffer = [0_u8; REPORT_BYTES_MAX];
            match engine.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) if report.len() + count <= REPORT_BYTES_MAX => {
                    report.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(_) | Err(_) => return Err("warden_unconfirmed"),
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

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod fixture;
#[cfg(all(test, target_os = "macos"))]
mod tests;
