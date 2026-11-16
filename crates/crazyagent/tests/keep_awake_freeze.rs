#![cfg(unix)]

use crazyagent::engine::EngineError;
use crazyagent::keep_awake::freeze::{self, FreezeApp, Frozen, Running};
use crazyagent::keep_awake::journal::JournalLock;
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

const STAND_IN: &str = "CRAZYAGENT_TEST_STAND_IN";
const CHILDREN: &str = "CRAZYAGENT_TEST_STAND_IN_CHILDREN";
const LISTEN: &str = "CRAZYAGENT_TEST_STAND_IN_LISTEN";
const STAND_IN_ARGS: [&str; 4] = ["--exact", "stand_in", "--nocapture", "--test-threads=1"];
const LINES_MAX: usize = 256;

/// Copies of this test program in a folder of the row's own, so every stand-in runs an
/// executable outside the system folders and outside this program's folder, as an owner's
/// app does. macOS kills a copied system program, so a copy of `/bin/sleep` cannot stand in.
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
        let this = std::env::current_exe().unwrap();
        std::fs::copy(this, folder.join("app-sleep")).unwrap();
        Self {
            _disk: disk,
            folder,
        }
    }

    fn sleep(&self) -> PathBuf {
        self.folder.join("app-sleep")
    }

    fn named(&self, name: &str) -> PathBuf {
        let program = self.folder.join(name);
        std::fs::copy(self.sleep(), &program).unwrap();
        program
    }

    /// The words that start one stand-in of the given copy.
    fn argv(program: &Path) -> Vec<String> {
        let mut argv = vec![program.display().to_string()];
        argv.extend(STAND_IN_ARGS.map(str::to_owned));
        argv
    }
}

/// Not a check: the body of every stand-in process. Run by the test runner it returns at
/// once. Run by a row from a copy of this program with `STAND_IN` set, it first listens at
/// the socket `LISTEN` names, if any, as a tmux server does, and prints
/// `stand-in listening <pid>`; then it starts each child `CHILDREN` names (one per line: a
/// label, then the program and its words, split by tabs) with `STAND_IN` set, prints one
/// `stand-in <label> <pid>` line each and sleeps.
#[test]
fn stand_in() {
    if std::env::var_os(STAND_IN).is_none() {
        return;
    }
    let listener = std::env::var_os(LISTEN).map(|socket| {
        let listener = crazyagent::spawn_gate::bind_unix_std(&socket).unwrap();
        println!("stand-in listening {}", std::process::id());
        listener
    });
    let children = std::env::var(CHILDREN).unwrap_or_default();
    let mut started = Vec::new();
    for line in children.lines().filter(|line| !line.is_empty()).take(8) {
        let mut words = line.split('\t');
        let label = words.next().unwrap();
        let program = words.next().unwrap();
        let mut command = Command::new(program);
        command
            .args(words)
            .env(STAND_IN, "on")
            .env_remove(CHILDREN)
            .env_remove(LISTEN)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
        println!("stand-in {label} {}", child.id());
        started.push(child);
    }
    std::thread::sleep(Duration::from_secs(600));
    drop(started);
    drop(listener);
}

/// A real process that stands in for an app, an engine or an agent: a copy of this
/// program that sleeps, with the children it was asked to start. Every one is killed and
/// reaped when the row ends, stopped or not.
struct StandIn {
    child: Child,
    helpers: Vec<(String, u32)>,
}

impl StandIn {
    fn command(bin: &Bin, children: &[(&str, Vec<String>)]) -> Command {
        Self::command_of(&bin.sleep(), children)
    }

    fn command_of(program: &Path, children: &[(&str, Vec<String>)]) -> Command {
        let lines: Vec<String> = children
            .iter()
            .map(|(label, argv)| format!("{label}\t{}", argv.join("\t")))
            .collect();
        let mut command = Command::new(program);
        command
            .args(STAND_IN_ARGS)
            .env(STAND_IN, "on")
            .env(CHILDREN, lines.join("\n"))
            .stderr(Stdio::null());
        command
    }

    fn sleep(bin: &Bin) -> Self {
        let mut command = Self::command(bin, &[]);
        command.stdin(Stdio::null()).stdout(Stdio::null());
        Self::start(&mut command)
    }

    fn with_helper(bin: &Bin) -> Self {
        Self::with_children(bin, &[("helper", Bin::argv(&bin.sleep()))], 1)
    }

    fn with_children(bin: &Bin, children: &[(&str, Vec<String>)], lines: usize) -> Self {
        let mut command = Self::command(bin, children);
        Self::reading(&mut command, lines)
    }

    /// A stand-in for a tmux server: a copy of this program under the given name that
    /// listens at the socket, with the children it was asked to start.
    fn listening(bin: &Bin, name: &str, socket: &Path, children: &[(&str, Vec<String>)]) -> Self {
        let mut command = Self::command_of(&bin.named(name), children);
        command.env(LISTEN, socket);
        let started = Self::reading(&mut command, children.len() + 1);
        assert_eq!(started.labelled("listening"), started.pid());
        started
    }

    fn reading(command: &mut Command, lines: usize) -> Self {
        command.stdin(Stdio::null()).stdout(Stdio::piped());
        let mut started = Self::start(command);
        let out = started.child.stdout.take().unwrap();
        let mut reader = BufReader::new(out);
        for _ in 0..LINES_MAX {
            if started.helpers.len() == lines {
                break;
            }
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "the stand-in ended"
            );
            let Some(rest) = line.trim().strip_prefix("stand-in ") else {
                continue;
            };
            let (label, pid) = rest.split_once(' ').unwrap();
            let pid: u32 = pid.parse().unwrap();
            assert!(pid > 1);
            started.helpers.push((label.to_owned(), pid));
        }
        assert_eq!(started.helpers.len(), lines);
        std::thread::spawn(move || std::io::copy(&mut reader, &mut std::io::sink()));
        started
    }

    fn start(command: &mut Command) -> Self {
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
    let notes = listed("Notes", app.pid(), &bin.sleep());
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
        let mut command = StandIn::command(&bin, &[]);
        command.stdin(writer).stdout(Stdio::null());
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
    let server_itself = listed("Server", server.pid(), &bin.sleep());
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

/// A folder outside the row's temp folder, such as an agent's short temp folder, removed
/// with everything in it when the row ends.
struct Made(PathBuf);

impl Made {
    fn new(path: PathBuf) -> Self {
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn link(path: PathBuf, target: &Path) -> Self {
        std::os::unix::fs::symlink(target, &path).unwrap();
        Self(path)
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An engine root with its terminal folder and one agent whose short temp folder exists.
/// Output: the root's disk, the root, the terminal folder and the agent's temp folder.
fn engine_root() -> (tempfile::TempDir, PathBuf, PathBuf, Made) {
    use std::os::unix::fs::PermissionsExt;
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap();
    let terminal = root.join(crazyagent::terminal::FOLDER);
    std::fs::create_dir(&terminal).unwrap();
    std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root
        .join("team")
        .join("agents")
        .join("agent-1")
        .join("home");
    std::fs::create_dir_all(&home).unwrap();
    let tmp = Made::new(crazyagent::sandbox::toolchains::tmp_path(&home));
    (disk, root, terminal, tmp)
}

/// T17b, rv-code T17 r2 F1: the agent roots are the listeners at this engine's own tmux
/// sockets, named by the system from the socket and never by a program name, since the
/// shipped app runs its bundled `crazyagent-tmux`. The engine's server, a lead's own
/// server and a lead's teammates' server all count; a socket nothing listens at does not;
/// an app inside one of them is never frozen.
#[test]
fn every_tmux_server_on_a_socket_of_this_engine_is_an_agent_root_whatever_its_name() {
    let bin = Bin::new();
    let (_disk, root, terminal, tmp) = engine_root();
    let app = ("app", Bin::argv(&bin.sleep()));
    let engine_socket = terminal.join(crazyagent::terminal::tmux::SOCKET);
    let engine_server = StandIn::listening(&bin, "crazyagent-tmux", &engine_socket, &[app]);
    let lead_socket = tmp.0.join(crazyagent::terminal::HARNESS_SOCKET);
    let lead = StandIn::listening(&bin, "tmux", &lead_socket, &[]);
    let mates_socket = tmp.0.join("teammates-0123456789abcdef.sock");
    let mates = StandIn::listening(&bin, "crazyagent-tmux", &mates_socket, &[]);
    drop(
        crazyagent::spawn_gate::bind_unix_std(tmp.0.join("teammates-ffffffffffffffff.sock"))
            .unwrap(),
    );
    let engine = StandIn::sleep(&bin);

    let roots = freeze::servers(&root).unwrap();

    assert_eq!(
        roots,
        BTreeSet::from([engine_server.pid(), lead.pid(), mates.pid()])
    );
    let inside = listed("Notes", engine_server.labelled("app"), &bin.sleep());
    assert!(
        freeze::allowed(&[inside], engine.pid(), &roots)
            .unwrap()
            .is_empty(),
        "an app inside the engine's own server is an agent's"
    );
}

/// T17b: a socket folder of this engine that cannot be read hides which servers run there,
/// so every freeze is refused rather than guessed.
#[test]
fn a_socket_folder_that_cannot_be_read_refuses_every_freeze() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::getuid().is_root() {
        eprintln!("skipped: root reads every folder");
        return;
    }
    let (_disk, root, _terminal, tmp) = engine_root();
    std::fs::set_permissions(&tmp.0, std::fs::Permissions::from_mode(0o000)).unwrap();

    assert!(freeze::servers(&root).is_err());
}

/// T17b code r1 F1, first site: an agent's short temp folder is its TMPDIR, so thousands
/// of its own files can sit beside its server's socket. They never refuse the freeze, and
/// the server is still a root.
#[test]
fn a_full_agent_temp_folder_never_refuses_the_freeze() {
    let bin = Bin::new();
    let (_disk, root, _terminal, tmp) = engine_root();
    for file in 0..5000 {
        std::fs::write(tmp.0.join(format!("left-{file}")), b"").unwrap();
    }
    let lead_socket = tmp.0.join(crazyagent::terminal::HARNESS_SOCKET);
    let lead = StandIn::listening(&bin, "tmux", &lead_socket, &[]);

    let roots = freeze::servers(&root);

    assert!(roots.is_ok(), "{roots:?}");
    assert_eq!(roots.unwrap(), BTreeSet::from([lead.pid()]));
}

/// T17b code r1 F1, second site: the folders of agents long gone stay in the team folder
/// until their project is deleted. Thousands of them never refuse the freeze, and a live
/// lead's server is still a root.
#[test]
fn old_agent_folders_never_refuse_the_freeze() {
    let bin = Bin::new();
    let (_disk, root, _terminal, tmp) = engine_root();
    let agents = root.join("team").join("agents");
    for id in 2..5000 {
        std::fs::create_dir(agents.join(format!("agent-{id}"))).unwrap();
    }
    let lead_socket = tmp.0.join(crazyagent::terminal::HARNESS_SOCKET);
    let lead = StandIn::listening(&bin, "tmux", &lead_socket, &[]);

    let roots = freeze::servers(&root);

    assert!(roots.is_ok(), "{roots:?}");
    assert_eq!(roots.unwrap(), BTreeSet::from([lead.pid()]));
}

/// T17b code r1 F2: an agent's short temp folder sits in the shared temp folder under a
/// name anyone can work out. One that is a link, or another user's, is not this engine's,
/// which never makes its sockets there, so it is skipped and never refuses the freeze.
#[test]
fn an_agent_temp_folder_that_is_a_link_is_skipped_not_a_refusal() {
    use std::os::unix::fs::PermissionsExt;
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap();
    let home = root
        .join("team")
        .join("agents")
        .join("agent-1")
        .join("home");
    std::fs::create_dir_all(&home).unwrap();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _tmp = Made::link(crazyagent::sandbox::toolchains::tmp_path(&home), &elsewhere);

    let roots = freeze::servers(&root);

    assert!(roots.is_ok(), "{roots:?}");
    assert!(roots.unwrap().is_empty());
}

/// rv-code T17 r2 F2: on Linux a journaled pid that another user's process now holds is
/// not the frozen process, since only the owner's are journaled: it reads as gone, and
/// the rest of the app is still thawed.
#[cfg(target_os = "linux")]
#[test]
fn a_journaled_pid_another_user_now_holds_is_gone_and_the_rest_still_thaws() {
    use std::os::unix::fs::MetadataExt;
    let own = rustix::process::getuid().as_raw();
    let theirs = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid > 1)
        .filter(|pid| {
            std::fs::metadata(format!("/proc/{pid}")).is_ok_and(|found| found.uid() != own)
        })
        .find(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|line| !line.is_empty())
        });
    let Some(theirs) = theirs else {
        eprintln!("skipped: no process of another user runs here");
        return;
    };
    let gone = freeze::identity(theirs).unwrap_err();
    assert!(gone.0.starts_with("keep_awake_freeze_gone"), "{gone:?}");
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let app = StandIn::with_helper(&bin);
    let mut journal = freeze::journal(root.path()).unwrap();
    let notes = listed("Notes", app.pid(), &bin.sleep());
    let mut frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();
    assert!(settles(|| stopped(app.pid()) && stopped(app.helper())));
    let helper = frozen
        .procs
        .iter_mut()
        .find(|process| process.pid == app.helper())
        .unwrap();
    helper.pid = theirs;

    freeze::thaw(&frozen.entry()).unwrap();

    assert!(
        settles(|| !stopped(app.pid())),
        "the app's own process is thawed"
    );
    signal("-CONT", app.helper());
}

/// rv-sec3 T17 r2 F1: a process that runs another program by the time its SIGSTOP lands
/// is no longer the app's. Freeze reads who runs once more after stopping and lets that
/// one go on at once, so it never stays stopped under a record that no longer names it.
#[test]
fn a_process_that_runs_another_program_by_its_stop_is_let_go_at_once() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let app = StandIn::with_helper(&bin);
    let helper = app.helper();
    let mut journal = freeze::journal(root.path()).unwrap();
    let notes = listed("Notes", app.pid(), &bin.sleep());
    let reads = std::cell::Cell::new(0_u32);
    let read = |pids: &[u32]| -> Result<BTreeMap<u32, freeze::Proc>, EngineError> {
        reads.set(reads.get() + 1);
        let mut found = BTreeMap::new();
        for pid in pids {
            let process = if reads.get() >= 3 && *pid == helper {
                renamed(*pid)
            } else {
                freeze::identity(*pid)?
            };
            found.insert(*pid, process);
        }
        Ok(found)
    };

    let frozen =
        freeze::freeze_with(&mut journal, &notes, engine.pid(), &no_roots(), read).unwrap();

    assert_eq!(reads.get(), 3, "read to journal, before the stop, after it");
    assert_eq!(frozen.procs.len(), 2);
    assert!(settles(|| stopped(app.pid())));
    assert!(
        settles(|| !stopped(helper)),
        "the process that runs another program goes on"
    );
    drop(journal);
    assert_eq!(reconcile(root.path()), Leftover::Kinds(Vec::new()));
    assert!(settles(|| !stopped(app.pid())));
}

/// rv-sec3 T17 r2 F2: an engine root long enough that its terminal socket is reached
/// through the short address in /tmp still has its server counted as an agent root.
#[test]
fn the_engine_server_on_a_short_address_is_an_agent_root_too() {
    use std::os::unix::fs::PermissionsExt;
    let bin = Bin::new();
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap().join("r".repeat(110));
    let terminal = root.join(crazyagent::terminal::FOLDER);
    std::fs::create_dir_all(&terminal).unwrap();
    std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = terminal.join(crazyagent::terminal::tmux::SOCKET);
    assert!(
        socket.as_os_str().len() > 108,
        "the socket needs the short address"
    );
    let server = StandIn::listening(&bin, "crazyagent-tmux", &socket, &[]);

    let roots = freeze::servers(&root).unwrap();

    assert_eq!(roots, BTreeSet::from([server.pid()]));
}

/// rv-secreview T17 r1 F4, leader 01:57 (2): a descendant that may never be frozen (a
/// system program, CrazyAgent by name) keeps running together with everything below it;
/// the app's own processes pause.
#[test]
fn never_freeze_descendants_keep_running_with_their_whole_subtree() {
    let bin = Bin::new();
    let root = tempfile::tempdir().unwrap();
    let engine = StandIn::sleep(&bin);
    let plain = Bin::argv(&bin.sleep());
    let inner = format!(
        "{STAND_IN}=on {} & echo stand-in inner $!; wait",
        plain.join(" ")
    );
    let system = vec!["/bin/sh".to_owned(), "-c".to_owned(), inner];
    let ours = Bin::argv(&bin.named("crazyagent-tool"));
    let children = [("plain", plain), ("system", system), ("ours", ours)];
    let app = StandIn::with_children(&bin, &children, 4);
    let notes = listed("Notes", app.pid(), &bin.sleep());
    let mut journal = freeze::journal(root.path()).unwrap();

    let frozen = freeze::freeze(&mut journal, &notes, engine.pid(), &no_roots()).unwrap();

    let mut pids: Vec<u32> = frozen.procs.iter().map(|process| process.pid).collect();
    pids.sort_unstable();
    let mut want = vec![app.pid(), app.labelled("plain")];
    want.sort_unstable();
    assert_eq!(pids, want, "the app and its own child only");
    assert!(settles(
        || stopped(app.pid()) && stopped(app.labelled("plain"))
    ));
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

    assert_eq!(
        journal.entries().len(),
        1,
        "the entry stays for a later try"
    );
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
        engine_lock(&root),
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

/// QUESTION-03:02 A: a cover the lane cannot check for agents pauses nothing and names
/// why, once per refusal, for the owner's "Apps not paused" notice.
#[test]
fn a_lane_that_cannot_tell_agents_apart_pauses_nothing_and_says_why() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::getuid().is_root() {
        eprintln!("skipped: root reads every folder");
        return;
    }
    let disk = tempfile::tempdir().unwrap();
    let root = disk.path().canonicalize().unwrap();
    let agents = root.join("team").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o000)).unwrap();
    let program = PathBuf::from(env!("CARGO_BIN_EXE_crazyagent"));
    let mut lane = freeze::Lane::start(
        root.clone(),
        program,
        None,
        std::process::id(),
        engine_lock(&root),
    )
    .unwrap();
    let cover = || freeze::Cover {
        freeze: true,
        mute: false,
        apps: vec![FreezeApp {
            name: "Notes".into(),
            id: "com.example.notes".into(),
        }],
    };

    lane.cover(cover());

    assert!(settles(|| lane.refused().is_some()), "the refusal is named");
    let (first, code) = lane.refused().unwrap();
    assert!(
        code.starts_with("keep_awake_freeze_agents_unknown"),
        "{code}"
    );
    assert_eq!(journaled(&root), Some(0), "nothing was frozen");
    lane.uncover();
    lane.cover(cover());
    assert!(
        settles(|| lane.refused().is_some_and(|(next, _)| next == first + 1)),
        "a second refusal is a new one"
    );
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o700)).unwrap();
    lane.finish();
    assert!(lane.wait(Instant::now() + WAIT));
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
        engine_lock(&root),
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

/// The engine's journal lock for a lane row, taken once in this process as the engine start
/// takes it, and shared with the lane (DEC-167 addendum 36). Input: the engine root.
/// Output: the shared lock.
fn engine_lock(root: &Path) -> std::sync::Arc<JournalLock> {
    std::sync::Arc::new(JournalLock::try_take(root).unwrap().unwrap())
}
