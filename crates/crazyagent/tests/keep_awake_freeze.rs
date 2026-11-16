#![cfg(unix)]

use crazyagent::keep_awake::freeze::{self, FreezeApp, Frozen, Running};
use crazyagent::keep_awake::mute::{self, Tool};
use crazyagent::keep_awake::{Leftover, reconcile};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);
const STEP: Duration = Duration::from_millis(50);
const QUIET: Duration = Duration::from_millis(400);
const LONG_AGO: &str = "Thu Jan  1 00:00:00 1970";

/// A real process that stands in for an app, an engine or an agent: `/bin/sleep`, or a
/// shell with one `/bin/sleep` child when `helper` is set. Both are killed and reaped
/// when the row ends, stopped or not.
struct StandIn {
    child: Child,
    helper: Option<u32>,
}

impl StandIn {
    fn sleep() -> Self {
        Self::start(Command::new("/bin/sleep").arg("600").stdout(Stdio::null()))
    }

    fn with_helper() -> Self {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "/bin/sleep 600 & echo $!; wait"])
            .stdout(Stdio::piped());
        let mut started = Self::start(&mut command);
        let out = started.child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(out).read_line(&mut line).unwrap();
        let helper: u32 = line.trim().parse().unwrap();
        assert!(helper > 1);
        started.helper = Some(helper);
        started
    }

    fn start(command: &mut Command) -> Self {
        command.stdin(Stdio::null()).stderr(Stdio::null());
        let child = crazyagent::spawn_gate::spawn(command).unwrap();
        assert!(child.id() > 1);
        Self {
            child,
            helper: None,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn helper(&self) -> u32 {
        self.helper.expect("a stand-in started with a helper")
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        if let Some(helper) = self.helper {
            signal("-KILL", helper);
        }
        self.kill();
    }
}

/// Sends one signal with `/bin/kill`, as any process of the owner could.
fn signal(name: &str, pid: u32) {
    let status = crazyagent::spawn_gate::spawn(
        Command::new("/bin/kill")
            .args([name, &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(status.success() || name == "-KILL");
}

/// Tells whether a process is stopped by a signal, from the system's own process state.
fn stopped(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let state = stat.rsplit_once(')').map(|(_, rest)| rest.trim_start());
        state.is_some_and(|rest| rest.starts_with('T'))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = crazyagent::spawn_gate::spawn(
            Command::new("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .wait_with_output()
        .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().starts_with('T')
    }
}

/// Waits at most `WAIT` for a condition. Output: true once it holds.
fn settles(mut holds: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if holds() {
            return true;
        }
        std::thread::sleep(STEP);
    }
    holds()
}

/// Checks a condition for `QUIET`, so a late signal has time to land. Output: true when it
/// held every time.
fn stays(mut holds: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + QUIET;
    while Instant::now() < until {
        if !holds() {
            return false;
        }
        std::thread::sleep(STEP);
    }
    holds()
}

/// Stops a stand-in the way Keep awake would, outside Keep awake, and waits until it is.
fn stop(pid: u32) {
    signal("-STOP", pid);
    assert!(settles(|| stopped(pid)));
}

/// The identity of a live process with its start time moved, as a reused pid looks.
fn reused(pid: u32) -> freeze::Proc {
    let mut process = freeze::identity(pid).unwrap();
    assert!(!process.start.is_empty());
    process.start = LONG_AGO.into();
    process
}

/// The identity of a live process with another executable, as a reused pid looks.
fn renamed(pid: u32) -> freeze::Proc {
    let mut process = freeze::identity(pid).unwrap();
    assert!(!process.exe.is_empty());
    process.exe = "/Applications/Other.app/Contents/MacOS/Other".into();
    process
}

/// Waits at most `WAIT` for a child to exit. Output: its status, or none while it runs.
fn exited(child: &mut Child) -> Option<ExitStatus> {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(STEP);
    }
    child.try_wait().unwrap()
}

/// Task 17 smallest real check: the engine froze an app (main process and helper), then
/// died. The next engine start thaws it from the journal on disk alone, and never signals
/// a journaled pid whose start time, executable or boot session no longer match.
#[test]
fn a_frozen_app_is_thawed_after_an_engine_crash_only_if_its_identity_matches() {
    let root = tempfile::tempdir().unwrap();
    let app = StandIn::with_helper();
    let later = StandIn::sleep();
    let other = StandIn::sleep();
    let older = StandIn::sleep();
    let mut journal = freeze::journal(root.path()).unwrap();
    let frozen = freeze::freeze(&mut journal, "Notes", app.pid()).unwrap();
    let mut pids: Vec<u32> = frozen.procs.iter().map(|process| process.pid).collect();
    pids.sort_unstable();
    let mut want = vec![app.pid(), app.helper()];
    want.sort_unstable();
    assert_eq!(pids, want, "the app and its helper are both frozen");
    assert!(settles(|| stopped(app.pid()) && stopped(app.helper())));
    let boot = freeze::boot_session().unwrap();
    assert!(!boot.is_empty());
    assert_eq!(frozen.boot, boot);
    assert_eq!(journal.entries(), &[frozen.entry()]);
    let mismatched = [
        ("Reused", boot.clone(), reused(later.pid())),
        ("Renamed", boot.clone(), renamed(other.pid())),
        (
            "Last boot",
            "another-boot".to_owned(),
            freeze::identity(older.pid()).unwrap(),
        ),
    ];
    for (name, boot, process) in mismatched {
        stop(process.pid);
        let record = Frozen {
            app: name.into(),
            boot,
            procs: vec![process],
        };
        journal.record(record.entry()).unwrap();
    }
    drop(journal);

    assert_eq!(reconcile(root.path()), Leftover::Kinds(Vec::new()));

    assert!(settles(|| !stopped(app.pid()) && !stopped(app.helper())));
    assert!(
        stays(|| stopped(later.pid())),
        "a reused pid is never signalled"
    );
    assert!(
        stays(|| stopped(other.pid())),
        "another executable is never signalled"
    );
    assert!(
        stays(|| stopped(older.pid())),
        "a pid of another boot is never signalled"
    );
    let after = freeze::journal(root.path()).unwrap();
    assert!(after.entries().is_empty(), "every entry settles");
}

/// Leader 23:13 crash-thaw A: the engine is killed while an app is frozen. The thaw guard,
/// which waits on a pipe only the engine holds, thaws the app once the engine is gone,
/// never signals a reused pid, settles the journal and exits.
#[test]
fn an_engine_killed_while_an_app_is_frozen_leaves_the_guard_to_thaw_it() {
    let root = tempfile::tempdir().unwrap();
    let app = StandIn::sleep();
    let later = StandIn::sleep();
    let (reader, writer) = std::io::pipe().unwrap();
    let mut engine = {
        let mut command = Command::new("/bin/sleep");
        command.arg("600").stdout(writer);
        StandIn::start(&mut command)
    };
    let mut guard = {
        let program = Path::new(env!("CARGO_BIN_EXE_crazyagent"));
        let mut command = freeze::guard_command(program, root.path());
        command
            .stdin(reader)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crazyagent::spawn_gate::spawn(&mut command).unwrap()
    };
    let mut journal = freeze::journal(root.path()).unwrap();
    let frozen = freeze::freeze(&mut journal, "Notes", app.pid()).unwrap();
    assert_eq!(frozen.procs.len(), 1);
    assert!(settles(|| stopped(app.pid())));
    let boot = freeze::boot_session().unwrap();
    stop(later.pid());
    let record = Frozen {
        app: "Reused".into(),
        boot,
        procs: vec![reused(later.pid())],
    };
    journal.record(record.entry()).unwrap();
    drop(journal);
    assert!(
        stays(|| guard.try_wait().unwrap().is_none() && stopped(app.pid())),
        "the guard waits while the engine runs"
    );

    engine.kill();

    let status = exited(&mut guard);
    assert!(status.is_some_and(|status| status.success()), "{status:?}");
    assert!(settles(|| !stopped(app.pid())));
    assert!(
        stays(|| stopped(later.pid())),
        "a reused pid is never signalled"
    );
    let after = freeze::journal(root.path()).unwrap();
    assert!(after.entries().is_empty(), "every entry settles");
}

/// Task 17 row: CrazyAgent, its engine, a process above the engine (the terminal that
/// started it), an agent below it, a system process and pid 1 never show in the picker;
/// an ordinary app of the owner does.
#[test]
fn the_engine_and_agents_cannot_be_picked() {
    let engine = StandIn::with_helper();
    let notes = StandIn::sleep();
    let finder = StandIn::sleep();
    let own = StandIn::sleep();
    let app = |name: &str, id: &str, pid: u32, path: &str| Running {
        app: FreezeApp {
            name: name.into(),
            id: id.into(),
        },
        pid,
        path: PathBuf::from(path),
    };
    let running = [
        app(
            "Engine",
            "test.engine",
            engine.pid(),
            "/Applications/Engine.app/Contents/MacOS/Engine",
        ),
        app(
            "Agent",
            "test.agent",
            engine.helper(),
            "/Applications/Agent.app/Contents/MacOS/Agent",
        ),
        app(
            "Terminal",
            "test.terminal",
            std::process::id(),
            "/Applications/Terminal.app/Contents/MacOS/Terminal",
        ),
        app(
            "CrazyAgent",
            "dev.crazyagent.desktop",
            own.pid(),
            "/Applications/CrazyAgent.app/Contents/MacOS/CrazyAgent",
        ),
        app(
            "Finder",
            "com.apple.finder",
            finder.pid(),
            "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder",
        ),
        app("launchd", "com.apple.launchd", 1, "/sbin/launchd"),
        app(
            "Notes",
            "com.example.notes",
            notes.pid(),
            "/Applications/Notes.app/Contents/MacOS/Notes",
        ),
    ];

    let picked = freeze::pickable(&running, engine.pid()).unwrap();

    let notes_app = FreezeApp {
        name: "Notes".into(),
        id: "com.example.notes".into(),
    };
    assert_eq!(picked, vec![notes_app]);
}

/// A stand-in sound program for one tool: a shell script that answers reads from a state
/// file, sets it, refuses any other call, and logs every call it gets.
struct Sound {
    tool: Tool,
    state: PathBuf,
    log: PathBuf,
    unmute: &'static str,
}

impl Sound {
    fn new(folder: &Path, tool: &str, muted: bool) -> Self {
        let state = folder.join("state");
        let log = folder.join("calls");
        std::fs::write(&state, if muted { "1" } else { "0" }).unwrap();
        let (s, l) = (state.display(), log.display());
        let read = format!("[ \"$(cat '{s}')\" = 1 ]");
        let (cases, unmute) = match tool {
            "osascript" => (
                format!(
                    "\"-e output muted of (get volume settings)\") if {read}; then echo true; else echo false; fi ;;\n\
                     \"-e set volume with output muted\") echo 1 > '{s}' ;;\n\
                     \"-e set volume without output muted\") echo 0 > '{s}' ;;"
                ),
                "-e set volume without output muted",
            ),
            "wpctl" => (
                format!(
                    "\"inspect @DEFAULT_AUDIO_SINK@\") echo 'id 52, type PipeWire:Interface:Node'; echo '  * node.name = \"alsa_output.analog-stereo\"' ;;\n\
                     \"get-volume 52\") if {read}; then echo 'Volume: 0.40 [MUTED]'; else echo 'Volume: 0.40'; fi ;;\n\
                     \"set-mute 52 1\") echo 1 > '{s}' ;;\n\
                     \"set-mute 52 0\") echo 0 > '{s}' ;;"
                ),
                "set-mute 52 0",
            ),
            "pactl" => (
                format!(
                    "\"get-default-sink\") echo alsa_output.analog-stereo ;;\n\
                     \"get-sink-mute alsa_output.analog-stereo\") if {read}; then echo 'Mute: yes'; else echo 'Mute: no'; fi ;;\n\
                     \"set-sink-mute alsa_output.analog-stereo 1\") echo 1 > '{s}' ;;\n\
                     \"set-sink-mute alsa_output.analog-stereo 0\") echo 0 > '{s}' ;;"
                ),
                "set-sink-mute alsa_output.analog-stereo 0",
            ),
            _ => unreachable!("one of the three sound tools"),
        };
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{l}'\ncase \"$*\" in\n{cases}\n*) exit 2 ;;\nesac\n"
        );
        let program = folder.join(tool);
        std::fs::write(&program, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tool = match tool {
            "osascript" => Tool::Osascript(program),
            "wpctl" => Tool::Wpctl(program),
            _ => Tool::Pactl(program),
        };
        Self {
            tool,
            state,
            log,
            unmute,
        }
    }

    fn muted(&self) -> bool {
        std::fs::read_to_string(&self.state).unwrap().trim() == "1"
    }

    fn unmuted_ever(&self) -> bool {
        let calls = std::fs::read_to_string(&self.log).unwrap_or_default();
        calls.lines().any(|call| call == self.unmute)
    }
}

/// Task 17 row: mute journals the sound state before it mutes, and restore puts that state
/// back: an owner who was muted stays muted and is never sent an unmute; one who was not
/// gets the sound back. The same for osascript, wpctl and pactl, each a stand-in.
#[test]
fn mute_restores_the_previous_state_not_unmuted() {
    for tool in ["osascript", "wpctl", "pactl"] {
        for was_muted in [true, false] {
            let disk = tempfile::tempdir().unwrap();
            let folder = disk.path().join("bin");
            std::fs::create_dir(&folder).unwrap();
            let sound = Sound::new(&folder, tool, was_muted);
            let mut journal = freeze::journal(disk.path()).unwrap();

            let journaled = mute::mute(&mut journal, &sound.tool).unwrap();

            assert!(
                journaled,
                "{tool} {was_muted}: the state is journaled first"
            );
            assert!(
                sound.muted(),
                "{tool} {was_muted}: muted while the lid is closed"
            );
            let entry = journal.entries().last().unwrap().clone();
            assert_eq!(entry.kind, mute::KIND);
            assert_eq!(journal.entries().len(), 1);

            mute::restore(&entry, &sound.tool).unwrap();

            assert_eq!(
                sound.muted(),
                was_muted,
                "{tool}: the state before the lid closed is back"
            );
            if was_muted {
                assert!(
                    !sound.unmuted_ever(),
                    "{tool}: a muted owner is never unmuted"
                );
            }
        }
    }
}
