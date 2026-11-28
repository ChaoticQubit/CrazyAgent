use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// The moved staging helper names its folder from the id its caller gives, so the engine's
/// own id source stays with the engine: the folder carries the id's first 24 characters,
/// is private, and a second folder from the same id is refused with the store code.
#[cfg(unix)]
#[test]
fn the_moved_helper_takes_its_id_from_the_caller() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("setup");
    let id = "0123456789abcdef0123456789abcdef";
    let made = staging_folder(&parent, "pack", id).unwrap();
    assert_eq!(made, parent.join(".pack-0123456789abcdef01234567"));
    assert!(made.is_dir());
    assert_eq!(
        std::fs::metadata(&made).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        staging_folder(&parent, "pack", id),
        Err(InstallError("setup_store_unavailable"))
    );
}

/// An id too short to name a staging folder is a caller bug, so it stops the helper
/// before anything is made.
#[cfg(unix)]
#[test]
#[should_panic(expected = "id.len() >= 24")]
fn a_staging_id_shorter_than_24_characters_stops_the_helper() {
    let parent = tempfile::tempdir().unwrap();
    let _ = staging_folder(parent.path(), "pack", "0123456789abcdef");
}

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
    let report = run_bounded(Command::new("/bin/echo").args(["tmux", "3.4"]), limit, true).unwrap();
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

/// A setup command that exits is heard through one watch armed on the calling thread, not
/// found by a poll, and its report is kept.
#[test]
fn a_setup_command_that_exits_is_heard_through_one_watch() {
    let before = crate::exit_wait::tests::hooks::arms();
    #[cfg(unix)]
    let mut command = Command::new("/bin/echo");
    #[cfg(unix)]
    command.arg("tmux 3.4");
    #[cfg(windows)]
    let mut command = Command::new(powershell());
    #[cfg(windows)]
    command.args(["-NoProfile", "-Command", "'tmux 3.4'"]);
    let report = run_bounded(&mut command, Duration::from_secs(30), true).unwrap();
    assert!(report.starts_with(b"tmux 3.4"));
    assert_eq!(crate::exit_wait::tests::hooks::arms() - before, 1);
}

/// A setup command past its limit is killed, then reaped once through its watch, after its
/// waiter answers that it exited, even with that waiter held back from its wait until the
/// reap releases it. Linux takes the waiter thread too, with process descriptors refused.
#[cfg(unix)]
#[test]
fn a_setup_command_past_its_limit_is_received_then_reaped_once() {
    use crate::exit_wait::{Heard, tests::hooks};
    #[cfg(target_os = "linux")]
    hooks::refuse_pidfd(Some(rustix::io::Errno::NOSYS));
    hooks::hold(true);
    let (before, _) = hooks::reaps();
    let stopped = run_bounded(
        Command::new("/bin/sleep").arg("5"),
        Duration::from_millis(200),
        false,
    );
    hooks::hold(false);
    #[cfg(target_os = "linux")]
    hooks::refuse_pidfd(None);
    assert_eq!(stopped, Err(InstallError("setup_timeout")));
    let (after, heard) = hooks::reaps();
    assert_eq!(after - before, 1);
    assert_eq!(heard, Some(Heard::Exited));
}

/// With process descriptors refused, a setup command's exit is still heard, through the
/// waiter thread, and the command succeeds.
#[cfg(target_os = "linux")]
#[test]
fn a_refused_pidfd_still_hears_the_exit() {
    use crate::exit_wait::{Heard, tests::hooks};
    hooks::refuse_pidfd(Some(rustix::io::Errno::NOSYS));
    let ran = run_bounded(
        &mut Command::new("/bin/true"),
        Duration::from_secs(30),
        false,
    );
    hooks::refuse_pidfd(None);
    assert_eq!(ran, Ok(Vec::new()));
    assert_eq!(hooks::reaps().1, Some(Heard::Exited));
}

/// A setup command that exits at once while a process it left behind still
/// holds its standard output is stopped at the limit, not held until that process ends.
#[cfg(unix)]
#[test]
fn a_report_held_open_by_a_background_process_stops_at_the_limit() {
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            Command::new("/bin/sh").args(["-c", "/bin/sleep 5 & echo tmux 3.4"]),
            Duration::from_millis(500),
            true
        ),
        Err(InstallError("setup_timeout"))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// A process a setup command leaves holding its report past the limit is stopped with the
/// command's process group: the call returns `setup_timeout` within 3 s and the holder is
/// gone soon after, not left to run out its 30 s. A shell loop bounded by `run_bounded`
/// watches the holder, so no row sleeps.
#[cfg(unix)]
#[test]
fn a_report_holder_past_the_limit_is_stopped_with_its_group() {
    let folder = tempfile::tempdir().unwrap();
    let holder = folder.path().join("holder");
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    r#"/bin/sleep 30 & echo $! > "$1"; echo tmux 3.4"#,
                    "sh"
                ])
                .arg(&holder),
            Duration::from_millis(500),
            true
        ),
        Err(InstallError("setup_timeout"))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = std::fs::read_to_string(&holder).unwrap();
    let pid = pid.trim();
    assert!(pid.parse::<u32>().is_ok_and(|pid| pid > 1), "{pid}");
    let gone = r#"while kill -0 "$1" 2>/dev/null && ! ps -o stat= -p "$1" | grep -q Z; do /bin/sleep 0.05; done"#;
    assert_eq!(
        run_bounded(
            Command::new("/bin/sh").args(["-c", gone, "sh", pid]),
            Duration::from_secs(5),
            false
        ),
        Ok(Vec::new()),
        "the holder {pid} outlived its command's process group"
    );
}

/// A process a failing setup command leaves behind is stopped with the command's process
/// group, with or without a kept report: the call returns `setup_install_failed` and the
/// holder is gone soon after, not left to run out its 30 s.
#[cfg(unix)]
#[test]
fn a_holder_left_by_a_failed_setup_command_is_stopped_with_its_group() {
    for capture in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let holder = folder.path().join("holder");
        let started = Instant::now();
        assert_eq!(
            run_bounded(
                Command::new("/bin/sh")
                    .args(["-c", r#"/bin/sleep 30 & echo $! > "$1"; exit 1"#, "sh"])
                    .arg(&holder),
                Duration::from_secs(5),
                capture
            ),
            Err(InstallError("setup_install_failed")),
            "capture {capture}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "capture {capture}"
        );
        let pid = std::fs::read_to_string(&holder).unwrap();
        let pid = pid.trim();
        assert!(pid.parse::<u32>().is_ok_and(|pid| pid > 1), "{pid}");
        let gone = r#"while kill -0 "$1" 2>/dev/null && ! ps -o stat= -p "$1" | grep -q Z; do /bin/sleep 0.05; done"#;
        assert_eq!(
            run_bounded(
                Command::new("/bin/sh").args(["-c", gone, "sh", pid]),
                Duration::from_secs(5),
                false
            ),
            Ok(Vec::new()),
            "the holder {pid} outlived its failed command's process group, capture {capture}"
        );
    }
}

/// Windows PowerShell under the `SystemRoot` folder. Input: none. Output: its path.
#[cfg(windows)]
fn powershell() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").expect("the SystemRoot variable is set");
    std::path::PathBuf::from(root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// On Windows, a setup command past its limit with no report kept is ended at the limit and
/// reaped once through its watch, not waited on until its own exit 5 s later.
#[cfg(windows)]
#[test]
fn a_setup_command_past_its_limit_is_ended_and_reaped_once_on_windows() {
    use crate::exit_wait::tests::hooks;
    let root = std::env::var_os("SystemRoot").expect("the SystemRoot variable is set");
    let ping = std::path::PathBuf::from(root)
        .join("System32")
        .join("PING.EXE");
    let (before, _) = hooks::reaps();
    let started = Instant::now();
    let stopped = run_bounded(
        Command::new(ping).args(["-n", "6", "127.0.0.1"]),
        Duration::from_millis(200),
        false,
    );
    assert_eq!(stopped, Err(InstallError("setup_timeout")));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(hooks::reaps().0 - before, 1);
}

/// On Windows, a process a setup command starts that holds the report past the limit is
/// ended with the command: the call returns `setup_timeout` and the holder, a `PING.EXE`
/// that inherits the report pipe, is gone soon after, not left to run out its 60 s. A
/// PowerShell loop bounded by `run_bounded` watches the holder, so no row sleeps.
#[cfg(windows)]
#[test]
fn a_report_holder_past_the_limit_is_ended_with_its_command_on_windows() {
    let folder = tempfile::tempdir().unwrap();
    let holder = folder.path().join("holder");
    let quoted = holder.display().to_string().replace('\'', "''");
    let start = format!(
        "$i = New-Object System.Diagnostics.ProcessStartInfo \
         ([IO.Path]::Combine($env:SystemRoot, 'System32', 'PING.EXE')), '-n 60 127.0.0.1'; \
         $i.UseShellExecute = $false; \
         $p = [System.Diagnostics.Process]::Start($i); \
         Set-Content -LiteralPath '{quoted}' -Value $p.Id; \
         'tmux 3.4'"
    );
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            Command::new(powershell()).args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                start.as_str()
            ]),
            Duration::from_secs(10),
            true
        ),
        Err(InstallError("setup_timeout"))
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    let pid = std::fs::read_to_string(&holder)
        .unwrap_or_else(|error| panic!("{}: {error}", holder.display()));
    let pid = pid.trim();
    assert!(pid.parse::<u32>().is_ok_and(|pid| pid > 4), "{pid}");
    let gone = format!(
        "while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) \
         {{ Start-Sleep -Milliseconds 50 }}"
    );
    assert_eq!(
        run_bounded(
            Command::new(powershell()).args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                gone.as_str()
            ]),
            Duration::from_secs(10),
            false
        ),
        Ok(Vec::new()),
        "the holder {pid} outlived its command"
    );
}

/// On Windows, a process a setup command starts is ended with the command even when the
/// command exits cleanly inside its limit: the call returns its empty report and the
/// `PING.EXE` it started, which would run 60 s, is gone soon after. A PowerShell loop
/// bounded by `run_bounded` watches it, so no row sleeps.
#[cfg(windows)]
#[test]
fn a_process_left_by_a_clean_setup_command_is_ended_with_it_on_windows() {
    let folder = tempfile::tempdir().unwrap();
    let holder = folder.path().join("holder");
    let quoted = holder.display().to_string().replace('\'', "''");
    let start = format!(
        "$i = New-Object System.Diagnostics.ProcessStartInfo \
         ([IO.Path]::Combine($env:SystemRoot, 'System32', 'PING.EXE')), '-n 60 127.0.0.1'; \
         $i.UseShellExecute = $false; \
         $p = [System.Diagnostics.Process]::Start($i); \
         Set-Content -LiteralPath '{quoted}' -Value $p.Id"
    );
    assert_eq!(
        run_bounded(
            Command::new(powershell()).args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                start.as_str()
            ]),
            Duration::from_secs(10),
            false
        ),
        Ok(Vec::new())
    );
    let pid = std::fs::read_to_string(&holder)
        .unwrap_or_else(|error| panic!("{}: {error}", holder.display()));
    let pid = pid.trim();
    assert!(pid.parse::<u32>().is_ok_and(|pid| pid > 4), "{pid}");
    let gone = format!(
        "while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) \
         {{ Start-Sleep -Milliseconds 50 }}"
    );
    assert_eq!(
        run_bounded(
            Command::new(powershell()).args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                gone.as_str()
            ]),
            Duration::from_secs(10),
            false
        ),
        Ok(Vec::new()),
        "the process {pid} outlived its clean command"
    );
}
