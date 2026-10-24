use super::{InstallError, Version};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_BYTES_MAX: u64 = 4096;
const POLL: Duration = Duration::from_millis(50);
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs one setup command with no input and a time limit. Input: the command, the limit,
/// and whether to keep its standard output, which is only for short version reports;
/// install output goes nowhere, so it can never fill a pipe. Output: up to 4 KiB of
/// standard output, `setup_timeout` after killing it at the limit, or
/// `setup_install_failed` when it cannot start or exits with a failure.
pub fn run_bounded(
    command: &mut Command,
    limit: Duration,
    capture: bool,
) -> Result<Vec<u8>, InstallError> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|_| InstallError("setup_install_failed"))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError(if outcome.is_ok() {
                    "setup_timeout"
                } else {
                    "setup_install_failed"
                }));
            }
        }
    };
    if !status.success() {
        return Err(InstallError("setup_install_failed"));
    }
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(OUTPUT_BYTES_MAX)
            .read_to_end(&mut output)
            .map_err(|_| InstallError("setup_version_unreadable"))?;
    }
    assert!(output.len() as u64 <= OUTPUT_BYTES_MAX);
    Ok(output)
}

/// Reads a tool's version from its version report, such as `tmux 3.4` or
/// `uv 0.9.5 (0a1b2c3 2025-10-01)`: the first word holding a digit, without a leading `v`.
/// Output: the version, or `setup_version_unreadable`.
pub fn reported_version(output: &[u8]) -> Result<Version, InstallError> {
    let unreadable = InstallError("setup_version_unreadable");
    let text = std::str::from_utf8(output).map_err(|_| unreadable)?;
    let word = text
        .split_whitespace()
        .find(|word| word.bytes().any(|b| b.is_ascii_digit()))
        .ok_or(unreadable)?;
    let word = word
        .strip_prefix('v')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(word);
    Version::parse(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_reports_from_tmux_uv_and_graphify_read_the_same_way() {
        for (report, version) in [
            ("tmux 3.4\n", "3.4"),
            ("tmux next-3.5\n", "next-3.5"),
            ("uv 0.9.5 (0a1b2c3 2025-10-01)\n", "0.9.5"),
            ("graphify v0.9.9\n", "0.9.9"),
            ("0.9.9", "0.9.9"),
        ] {
            assert_eq!(
                reported_version(report.as_bytes()).unwrap().as_str(),
                version
            );
        }
        for report in ["", "tmux\n", "tmux 3.4;rm\n"] {
            assert_eq!(
                reported_version(report.as_bytes()),
                Err(InstallError("setup_version_unreadable"))
            );
        }
        assert_eq!(
            reported_version(&[0xff, b'3']),
            Err(InstallError("setup_version_unreadable"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_setup_command_keeps_its_report_fails_plainly_and_stops_at_its_limit() {
        let limit = Duration::from_secs(10);
        let report =
            run_bounded(Command::new("/bin/echo").args(["tmux", "3.4"]), limit, true).unwrap();
        assert_eq!(report, b"tmux 3.4\n");
        assert_eq!(
            run_bounded(Command::new("/bin/echo").arg("quiet"), limit, false),
            Ok(Vec::new())
        );
        assert_eq!(
            run_bounded(&mut Command::new("/bin/false"), limit, false),
            Err(InstallError("setup_install_failed"))
        );
        assert_eq!(
            run_bounded(
                &mut Command::new("/nonexistent/crazyagent-setup"),
                limit,
                false
            ),
            Err(InstallError("setup_install_failed"))
        );
        let started = Instant::now();
        assert_eq!(
            run_bounded(
                Command::new("/bin/sleep").arg("30"),
                Duration::from_millis(200),
                false
            ),
            Err(InstallError("setup_timeout"))
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
