#![cfg(unix)]

use crazyagent::engine::EngineError;
use crazyagent::keep_awake::freeze::{self, FreezeApp, Frozen, Running};
use crazyagent::keep_awake::mute::{self, Tool};
use crazyagent::keep_awake::{Leftover, reconcile};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);
const STEP: Duration = Duration::from_millis(50);
const QUIET: Duration = Duration::from_millis(400);
const LONG_AGO: &str = "Thu Jan  1 00:00:00 1970";

/// Copies of the system's sleep and bash in a folder of the row's own, so every stand-in
/// runs an executable outside the system folders, as an owner's app does, and the freeze
/// checks treat it as one.
struct Bin {
    _disk: tempfile::TempDir,
    folder: PathBuf,
}

impl Bin {
    fn new() -> Self {
        let disk = tempfile::tempdir().unwrap();
        let folder = disk.path().canonicalize().unwrap().join("apps");
        std::fs::create_dir(&folder).unwrap();
        assert!(!folder.to_string_lossy().contains(char::is_whitespace));
        std::fs::copy("/bin/sleep", folder.join("app-sleep")).unwrap();
        std::fs::copy("/bin/bash", folder.join("app-shell")).unwrap();
        Self {
            _disk: disk,
            folder,
        }
    }

    fn sleep(&self) -> PathBuf {
        self.folder.join("app-sleep")
    }

    fn shell(&self) -> PathBuf {
        self.folder.join("app-shell")
    }

    fn named(&self, name: &str) -> PathBuf {
        let program = self.folder.join(name);
        std::fs::copy(self.sleep(), &program).unwrap();
        program
    }
}

/// A real process that stands in for an app, an engine or an agent: a copied sleep, or a
/// copied bash that starts children and prints their pids, one labelled line each. Every
/// one is killed and reaped when the row ends, stopped or not.
struct StandIn {
    child: Child,
    helpers: Vec<(String, u32)>,
}

impl StandIn {
    fn sleep(bin: &Bin) -> Self {
        Self::start(Command::new(bin.sleep()).arg("600").stdout(Stdio::null()))
    }

    fn with_helper(bin: &Bin) -> Self {
        let script = format!("{} 600 & echo helper $!; wait", bin.sleep().display());
        Self::shell(bin, &script, 1)
    }

    fn shell(bin: &Bin, script: &str, lines: usize) -> Self {
        let mut command = Command::new(bin.shell());
        command.args(["-c", script]).stdout(Stdio::piped());
        let mut started = Self::start(&mut command);
        let out = started.child.stdout.take().unwrap();
        let mut reader = BufReader::new(out);
        for _ in 0..lines {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (label, pid) = line.trim().split_once(' ').unwrap();
            let pid: u32 = pid.parse().unwrap();
            assert!(pid > 1);
            started.helpers.push((label.to_owned(), pid));
        }
        started
    }

    fn start(command: &mut Command) -> Self {
        command.stdin(Stdio::null()).stderr(Stdio::null());
        let child = crazyagent::spawn_gate::spawn(command).unwrap();
        assert!(child.id() > 1);
        Self {
            child,
            helpers: Vec::new(),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn helper(&self) -> u32 {
        self.labelled("helper")
    }

    fn labelled(&self, label: &str) -> u32 {
        self.helpers
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, pid)| *pid)
            .unwrap_or_else(|| panic!("a stand-in started with a {label} child"))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        for (_, helper) in &self.helpers {
            signal("-KILL", *helper);
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

/// One listed instance of an app as the platform would list it: the name, the id, the pid
/// and the executable it runs.
fn listed(name: &str, pid: u32, path: &Path) -> Running {
    Running {
        app: FreezeApp {
            name: name.into(),
            id: format!("com.example.{}", name.to_ascii_lowercase()),
        },
        pid,
        path: path.to_path_buf(),
    }
}

/// No agent root: the rows that are not about roots pass this.
fn no_roots() -> BTreeSet<u32> {
    BTreeSet::new()
}

/// Task 17 smallest real check: the engine froze an app (main process and helper), then
/// died. The next engine start thaws it from the journal on disk alone, and never signals
/// a journaled pid whose start time, executable or boot session no longer match.
#[test]
fn a_frozen_app_is_thawed_after_an_engine_crash_only_if_its_identity_matches() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let app = StandIn::with_helper(&bin);
    let later = StandIn::sleep(&bin);
    let other = StandIn::sleep(&bin);
    let older = StandIn::sleep(&bin);
    let mut journal = freeze::journal(root.path()).unwrap();
    let notes = listed("Notes", app.pid(), &bin.shell());
    let frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();
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
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let app = StandIn::sleep(&bin);
    let later = StandIn::sleep(&bin);
    let (reader, writer) = std::io::pipe().unwrap();
    let mut engine = {
        let mut command = Command::new(bin.sleep());
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
    let notes = listed("Notes", app.pid(), &bin.sleep());
    let frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();
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
    let bin = Bin::new();
    let engine = StandIn::with_helper(&bin);
    let notes = StandIn::sleep(&bin);
    let finder = StandIn::sleep(&bin);
    let own = StandIn::sleep(&bin);
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

    let picked = freeze::pickable(&running, engine.pid(), &no_roots()).unwrap();

    let notes_app = FreezeApp {
        name: "Notes".into(),
        id: "com.example.notes".into(),
    };
    assert_eq!(picked, vec![notes_app]);
}

/// rv-secreview T17 r1 F1: the never-freeze checks hold per process, not per app id. Two
/// instances of one app, the first listed an agent's (below the engine): only the owner's
/// instance may be frozen, `freeze` refuses the agent's when asked directly, and a pid
/// that runs another program than the listed app is never frozen.
#[test]
fn only_the_instances_that_may_be_frozen_are_frozen_not_every_instance_of_the_id() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::with_helper(&bin);
    let owners = StandIn::sleep(&bin);
    let agents = listed("Notes", engine.helper(), &bin.sleep());
    let mine = listed("Notes", owners.pid(), &bin.sleep());
    let running = [agents.clone(), mine.clone()];

    let allowed = freeze::allowed(&running, engine.pid(), &no_roots()).unwrap();

    assert_eq!(
        allowed,
        vec![mine.clone()],
        "the owner's instance only, though the agent's is listed first"
    );
    assert_eq!(
        freeze::pickable(&running, engine.pid(), &no_roots()).unwrap(),
        vec![mine.app.clone()]
    );
    let mut journal = freeze::journal(root.path()).unwrap();
    assert!(
        freeze::freeze(&mut journal, &agents, engine.pid(), &no_roots()).is_err(),
        "freeze checks the process again"
    );
    let mut moved = mine.clone();
    moved.path = PathBuf::from("/Applications/Other.app/Contents/MacOS/Other");
    assert!(
        freeze::freeze(&mut journal, &moved, engine.pid(), &no_roots()).is_err(),
        "a pid that runs another program than the listed app is never frozen"
    );
    assert!(journal.entries().is_empty());
    let frozen = freeze::freeze(&mut journal, &mine, engine.pid(), &no_roots()).unwrap();
    let pids: Vec<u32> = frozen.procs.iter().map(|process| process.pid).collect();
    assert_eq!(pids, vec![owners.pid()]);
    assert!(settles(|| stopped(owners.pid())));
    assert!(stays(|| !stopped(engine.helper()) && !stopped(engine.pid())));
    drop(journal);
    assert_eq!(reconcile(root.path()), Leftover::Kinds(Vec::new()));
    assert!(settles(|| !stopped(owners.pid())));
}

/// Leader 01:57 (sec r1 F3, until T17b): no process at or below a root the engine hands
/// in, such as a tmux server it knows, may be frozen; the same instance with no root is.
#[test]
fn an_instance_below_an_agent_root_is_never_frozen() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let server = StandIn::with_helper(&bin);
    let inside = listed("Notes", server.helper(), &bin.sleep());
    let server_itself = listed("Server", server.pid(), &bin.shell());
    let roots = BTreeSet::from([server.pid()]);

    let allowed = freeze::allowed(
        &[inside.clone(), server_itself.clone()],
        engine.pid(),
        &roots,
    )
    .unwrap();

    assert!(allowed.is_empty(), "{allowed:?}");
    let mut journal = freeze::journal(root.path()).unwrap();
    assert!(freeze::freeze(&mut journal, &inside, engine.pid(), &roots).is_err());
    assert!(freeze::freeze(&mut journal, &server_itself, engine.pid(), &roots).is_err());
    assert!(journal.entries().is_empty());
    assert!(stays(|| !stopped(server.helper()) && !stopped(server.pid())));
    assert_eq!(
        freeze::allowed(std::slice::from_ref(&inside), engine.pid(), &no_roots()).unwrap(),
        vec![inside],
        "the root is what refuses it"
    );
}

/// rv-secreview T17 r1 F4, leader 01:57 (2): a descendant that may never be frozen (a
/// system program, CrazyAgent by name) keeps running together with everything below it;
/// the app's own processes pause.
#[test]
fn never_freeze_descendants_keep_running_with_their_whole_subtree() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let sleep = bin.sleep().display().to_string();
    let ours = bin.named("crazyagent-tool").display().to_string();
    let script = format!(
        "{sleep} 600 & echo plain $!\n\
         /bin/sh -c '{sleep} 600 & echo inner $!; wait' & echo system $!\n\
         {ours} 600 & echo ours $!\n\
         wait"
    );
    let app = StandIn::shell(&bin, &script, 4);
    let notes = listed("Notes", app.pid(), &bin.shell());
    let mut journal = freeze::journal(root.path()).unwrap();

    let frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();

    let mut pids: Vec<u32> = frozen.procs.iter().map(|process| process.pid).collect();
    pids.sort_unstable();
    let mut want = vec![app.pid(), app.labelled("plain")];
    want.sort_unstable();
    assert_eq!(pids, want, "the app and its own child only");
    assert!(settles(|| stopped(app.pid()) && stopped(app.labelled("plain"))));
    for label in ["system", "inner", "ours"] {
        let pid = app.labelled(label);
        assert!(stays(|| !stopped(pid)), "{label} keeps running");
    }
    drop(journal);
    assert_eq!(reconcile(root.path()), Leftover::Kinds(Vec::new()));
    assert!(settles(
        || !stopped(app.pid()) && !stopped(app.labelled("plain"))
    ));
}

/// rv-secreview r1 F2 = rv-code r1 F1: a thaw that cannot read who runs now (ps did not
/// start or finish) is not a gone process. The entry stays, nothing is signalled, and a
/// later thaw that can read thaws the app.
#[test]
fn a_thaw_that_cannot_read_who_runs_keeps_the_app_journaled_until_it_can() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let app = StandIn::sleep(&bin);
    let mut journal = freeze::journal(root.path()).unwrap();
    let notes = listed("Notes", app.pid(), &bin.sleep());
    let frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();
    assert!(settles(|| stopped(app.pid())));
    let unread = |_: &[u32]| -> Result<BTreeMap<u32, freeze::Proc>, EngineError> {
        Err(EngineError(
            "keep_awake_freeze_identity: ps did not finish".into(),
        ))
    };

    assert!(
        freeze::thaw_with(&frozen.entry(), unread).is_err(),
        "an unread identity is not a gone process"
    );
    assert!(
        journal
            .settle(freeze::KIND, |entry| freeze::thaw_with(entry, unread))
            .is_err()
    );

    assert_eq!(journal.entries().len(), 1, "the entry stays for a later try");
    assert!(stays(|| stopped(app.pid())), "nothing was sent");
    journal.settle(freeze::KIND, freeze::thaw).unwrap();
    assert!(journal.entries().is_empty());
    assert!(settles(|| !stopped(app.pid())));
}

/// rv-secreview r1 F2, time zone branch: the start time that names a frozen process is
/// read in UTC, so a laptop that changes its time zone while an app is frozen still finds
/// the app's identity the same.
#[cfg(target_os = "macos")]
#[test]
fn the_identity_start_time_is_read_in_utc_so_a_time_zone_change_keeps_it() {
    let bin = Bin::new();
    let app = StandIn::sleep(&bin);
    let started = |zone: &str| {
        let out = crazyagent::spawn_gate::spawn(
            Command::new("/bin/ps")
                .args(["-o", "lstart=", "-p", &app.pid().to_string()])
                .env_clear()
                .env("TZ", zone)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .wait_with_output()
        .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let utc = started("UTC0");
    assert_ne!(utc, started("Pacific/Kiritimati"), "the zones differ");

    let identity = freeze::identity(app.pid()).unwrap();

    assert_eq!(identity.start, utc);
}

/// A stand-in sound program for one tool: a shell script that answers reads from a state
/// file, sets it, refuses any other call, refuses the unmute while a `refuse` file exists,
/// and logs every call it gets.
struct Sound {
    tool: Tool,
    state: PathBuf,
    log: PathBuf,
    refuse: PathBuf,
    unmute: &'static str,
}

impl Sound {
    fn new(folder: &Path, tool: &str, muted: bool) -> Self {
        let state = folder.join("state");
        let log = folder.join("calls");
        let refuse = folder.join("refuse");
        std::fs::write(&state, if muted { "1" } else { "0" }).unwrap();
        let (s, l, r) = (state.display(), log.display(), refuse.display());
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
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{l}'\nif [ -e '{r}' ] && [ \"$*\" = '{unmute}' ]; then exit 3; fi\ncase \"$*\" in\n{cases}\n*) exit 2 ;;\nesac\n"
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
            refuse,
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

/// How many entries the lid-closed journal holds now, read without its lock. Input: the
/// engine root. Output: the count, or none while the file cannot be read.
fn journaled(root: &Path) -> Option<usize> {
    freeze::journal(root)
        .ok()
        .map(|journal| journal.entries().len())
}

/// The sound tool this computer does not use (pactl on macOS, osascript elsewhere), so even
/// a guard left by a crashed row can never reach the real sound tool.
fn foreign() -> &'static str {
    if cfg!(target_os = "macos") {
        "pactl"
    } else {
        "osascript"
    }
}

/// Task 17 row: the lid-closed lane mutes the sound when a hold covers the closed lid,
/// with the state journaled first, puts it back when the lid opens and settles the entry,
/// and finish ends the lane within the stop wait.
#[test]
fn the_lane_mutes_while_covered_and_puts_the_sound_back_when_uncovered() {
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap();
    let folder = root.join("bin");
    std::fs::create_dir(&folder).unwrap();
    let sound = Sound::new(&folder, foreign(), false);
    let program = PathBuf::from(env!("CARGO_BIN_EXE_crazyagent"));
    let mut lane = freeze::Lane::start(
        root.clone(),
        program,
        Some(sound.tool.clone()),
        std::process::id(),
    )
    .unwrap();

    lane.cover(freeze::Cover {
        freeze: false,
        mute: true,
        apps: Vec::new(),
    });

    assert!(settles(|| sound.muted()), "covered: the sound is muted");
    assert!(
        settles(|| journaled(&root) == Some(1)),
        "the state before is journaled"
    );
    lane.uncover();
    assert!(settles(|| !sound.muted()), "uncovered: the sound is back");
    assert!(settles(|| journaled(&root) == Some(0)), "the entry settles");
    assert!(
        sound.unmuted_ever(),
        "put back by setting the journaled state"
    );
    lane.finish();
    assert!(lane.wait(Instant::now() + WAIT), "finish ends the lane");
}

/// rv-secreview r1 F2 retry, rv-code r1 F1: a put back that fails stays journaled, the lane
/// tries it again on its own while the engine runs, says so after three failed tries, and
/// clears that once a later try puts everything back.
#[test]
fn the_lane_tries_a_failed_put_back_again_and_reports_it_after_three_tries() {
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap();
    let folder = root.join("bin");
    std::fs::create_dir(&folder).unwrap();
    let sound = Sound::new(&folder, foreign(), false);
    let program = PathBuf::from(env!("CARGO_BIN_EXE_crazyagent"));
    let mut lane = freeze::Lane::start(
        root.clone(),
        program,
        Some(sound.tool.clone()),
        std::process::id(),
    )
    .unwrap();
    lane.cover(freeze::Cover {
        freeze: false,
        mute: true,
        apps: Vec::new(),
    });
    assert!(settles(|| sound.muted() && journaled(&root) == Some(1)));
    std::fs::write(&sound.refuse, "").unwrap();

    lane.uncover();

    assert!(
        settles(|| lane.stuck() == vec![mute::KIND.to_owned()]),
        "three failed tries raise the notice"
    );
    assert!(sound.muted(), "still muted");
    assert_eq!(journaled(&root), Some(1), "the entry stays");
    std::fs::remove_file(&sound.refuse).unwrap();
    assert!(
        settles(|| !sound.muted()),
        "a later try puts the sound back with no new order"
    );
    assert!(settles(|| journaled(&root) == Some(0)), "the entry settles");
    assert!(settles(|| lane.stuck().is_empty()), "the notice clears");
    lane.finish();
    assert!(lane.wait(Instant::now() + WAIT), "finish ends the lane");
}
