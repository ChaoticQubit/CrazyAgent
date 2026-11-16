#[cfg(target_os = "macos")]
mod macos {
    use crazyagent::keep_awake::{Held, HoldError, Want, platform_hold};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(2);
    const STEP: Duration = Duration::from_millis(50);
    const SYSTEM: &str = "PreventUserIdleSystemSleep";
    const DISPLAY: &str = "PreventUserIdleDisplaySleep";

    /// A real process that stands in for the engine a hold is taken for: `/bin/sleep`,
    /// killed and reaped when the row ends.
    struct StandIn(Child);

    impl StandIn {
        fn start() -> Self {
            let child = crazyagent::spawn_gate::spawn(
                Command::new("/bin/sleep")
                    .arg("120")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            assert!(child.id() > 1);
            Self(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn kill(&mut self) {
            let _ = self.0.kill();
            self.0.wait().unwrap();
        }
    }

    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Reads one `pid <n>(caffeinate): [...] <kind> named: ...` line of `pmset -g assertions`.
    /// Input: the line after `pid `. Output: the caffeinate pid and the assertion kind, or
    /// none for a line of another program.
    fn owner(rest: &str) -> Option<(u32, String)> {
        let (pid, after) = rest.split_once('(')?;
        if !after.starts_with("caffeinate)") {
            return None;
        }
        let kind = after
            .split_whitespace()
            .find(|word| word.starts_with("Prevent"))?;
        Some((pid.parse().ok()?, kind.to_owned()))
    }

    /// Lists the assertions the system holds on behalf of one engine pid. Input: the pid.
    /// Output: the caffeinate pid and kind of each, sorted by kind.
    fn assertions(engine: u32) -> Vec<(u32, String)> {
        let output = crazyagent::spawn_gate::output(
            Command::new("/usr/bin/pmset").args(["-g", "assertions"]),
        )
        .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let behalf = format!("on behalf of Process ID {engine}");
        let mut found = Vec::new();
        let mut last = None;
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("pid ") {
                last = owner(rest);
            } else if line.starts_with("Details:") && line.ends_with(&behalf) {
                found.extend(last.take());
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        found
    }

    fn kinds(engine: u32) -> Vec<String> {
        assertions(engine)
            .into_iter()
            .map(|(_, kind)| kind)
            .collect()
    }

    /// Waits a bounded time for a condition. Output: whether it came true within `WAIT`.
    fn until(mut condition: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + WAIT;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(STEP);
        }
    }

    const IDLE: Want = Want::Idle { display: false };

    #[test]
    fn an_idle_hold_shows_in_pmset_assertions_and_is_gone_after_its_engine_is_killed() {
        let mut engine = StandIn::start();
        let pid = engine.pid();
        let mut hold = platform_hold(pid);
        let held = hold.apply(&IDLE).unwrap();
        assert_eq!(
            held,
            Held {
                idle: true,
                lid: false,
                display: false
            }
        );
        assert!(until(|| kinds(pid) == [SYSTEM]));

        engine.kill();
        assert!(until(|| assertions(pid).is_empty()));
        hold.release();
    }

    #[test]
    fn display_awake_adds_the_display_assertion_and_drops_it_on_release() {
        let engine = StandIn::start();
        let pid = engine.pid();
        let mut hold = platform_hold(pid);

        let held = hold.apply(&Want::Idle { display: true }).unwrap();
        assert!(held.idle);
        assert!(held.display);
        assert!(until(|| kinds(pid) == [DISPLAY, SYSTEM]));

        let held = hold.apply(&IDLE).unwrap();
        assert!(held.idle);
        assert!(!held.display);
        assert!(until(|| kinds(pid) == [SYSTEM]));

        hold.apply(&Want::Idle { display: true }).unwrap();
        assert!(until(|| kinds(pid) == [DISPLAY, SYSTEM]));
        hold.release();
        assert!(until(|| assertions(pid).is_empty()));
    }

    #[test]
    fn a_hold_for_nothing_takes_no_assertion_and_releases_a_running_one() {
        let engine = StandIn::start();
        let pid = engine.pid();
        let mut hold = platform_hold(pid);
        assert_eq!(hold.apply(&Want::Nothing).unwrap(), Held::NONE);
        assert!(assertions(pid).is_empty());

        hold.apply(&IDLE).unwrap();
        assert!(until(|| kinds(pid) == [SYSTEM]));
        assert_eq!(hold.apply(&Want::Nothing).unwrap(), Held::NONE);
        assert!(until(|| assertions(pid).is_empty()));
    }

    #[test]
    fn a_lost_hold_is_reported_once_and_taken_again() {
        let engine = StandIn::start();
        let pid = engine.pid();
        let mut hold = platform_hold(pid);
        hold.apply(&IDLE).unwrap();
        assert!(until(|| kinds(pid) == [SYSTEM]));
        let first = assertions(pid)[0].0;

        let killed = crazyagent::spawn_gate::status(
            Command::new("/bin/kill").args(["-9", &first.to_string()]),
        )
        .unwrap();
        assert!(killed.success());
        assert!(until(|| assertions(pid).is_empty()));

        assert_eq!(hold.apply(&IDLE), Err(HoldError::Lost));
        let held = hold.apply(&IDLE).unwrap();
        assert!(held.idle);
        assert!(until(|| kinds(pid) == [SYSTEM]));
        assert_ne!(assertions(pid)[0].0, first);
        assert!(hold.apply(&IDLE).is_ok());
        hold.release();
        assert!(until(|| assertions(pid).is_empty()));
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use crazyagent::keep_awake::{REASON, Want, platform_hold};
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsFd;
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const HOLDER: &str = "CRAZYAGENT_KEEP_AWAKE_ROW_HOLDER";
    const WAIT: Duration = Duration::from_secs(5);
    const STEP: Duration = Duration::from_millis(100);
    const HOLDER_SECONDS_MAX: u64 = 60;
    const DISPLAY_HOLDER: &str = "CRAZYAGENT_KEEP_AWAKE_ROW_DISPLAY_HOLDER";
    const DBUS_DAEMON: &str = "/usr/bin/dbus-daemon";
    const SESSION: &str = "DBUS_SESSION_BUS_ADDRESS";
    const COOKIE: u32 = 7;
    const ASKS_MAX: usize = 16;
    const LINES_MAX: usize = 64;
    const FDS_MAX: usize = 4096;
    const REFUSED: &str = "org.freedesktop.DBus.Error.NotSupported";

    type Inhibitor = (String, String, String, String, u32, u32);

    /// Lists the inhibitor locks logind holds, over D-Bus. Output: what, who, why, mode, uid
    /// and pid of each.
    fn inhibitors() -> Vec<Inhibitor> {
        let bus = zbus::blocking::Connection::system().unwrap();
        let reply = bus
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "ListInhibitors",
                &(),
            )
            .unwrap();
        reply.body().deserialize().unwrap()
    }

    fn ours(pid: u32) -> bool {
        inhibitors().iter().any(|(what, who, _, mode, _, holder)| {
            *holder == pid
                && who == "CrazyAgent"
                && mode == "block"
                && what.contains("idle")
                && what.contains("sleep")
        })
    }

    fn until(mut condition: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + WAIT;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(STEP);
        }
    }

    /// Runs only as the child of the row below: takes the hold in a process of its own and
    /// keeps it until that process is killed, at most a minute.
    #[test]
    fn holder_process() {
        if std::env::var_os(HOLDER).is_none() {
            return;
        }
        let mut hold = platform_hold(std::process::id());
        hold.apply(&Want::Idle { display: false }).unwrap();
        std::thread::sleep(Duration::from_secs(HOLDER_SECONDS_MAX));
    }

    #[test]
    fn the_logind_lock_is_listed_while_held_and_gone_when_the_holder_dies() {
        let mut holder = crazyagent::spawn_gate::spawn(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "linux::holder_process", "--nocapture"])
                .env(HOLDER, "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let pid = holder.id();
        let listed = until(|| ours(pid));
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert!(listed);
        assert!(until(|| !ours(pid)));
    }

    type Call = (String, String, String, u32);

    /// A stand-in screensaver service on the private session bus: it records each call with
    /// its sender and answers `Inhibit` with `COOKIE`.
    #[derive(Clone, Default)]
    struct ScreenSaver {
        calls: Arc<Mutex<Vec<Call>>>,
    }

    #[zbus::interface(name = "org.freedesktop.ScreenSaver")]
    impl ScreenSaver {
        fn inhibit(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            application_name: String,
            reason_for_inhibit: String,
        ) -> u32 {
            let sender = header
                .sender()
                .map(|name| name.to_string())
                .unwrap_or_default();
            let words = format!("{application_name}: {reason_for_inhibit}");
            self.calls
                .lock()
                .unwrap()
                .push(("Inhibit".into(), sender, words, COOKIE));
            COOKIE
        }

        fn un_inhibit(&self, #[zbus(header)] header: zbus::message::Header<'_>, cookie: u32) {
            let sender = header
                .sender()
                .map(|name| name.to_string())
                .unwrap_or_default();
            self.calls
                .lock()
                .unwrap()
                .push(("UnInhibit".into(), sender, String::new(), cookie));
        }
    }

    /// A private session bus: a real `dbus-daemon` on a socket in a temporary folder, so no
    /// row touches the owner's own session. It ends with the value.
    struct SessionBus {
        daemon: Child,
        address: String,
        _folder: tempfile::TempDir,
    }

    impl SessionBus {
        fn start() -> Self {
            let folder = tempfile::tempdir().unwrap();
            let socket = folder.path().join("bus");
            let address = format!("unix:path={}", socket.display());
            let daemon = crazyagent::spawn_gate::spawn(
                Command::new(DBUS_DAEMON)
                    .args(["--session", "--nofork", "--nopidfile"])
                    .arg(format!("--address={address}"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            assert!(until(|| socket.exists()), "the private bus listens");
            Self {
                daemon,
                address,
                _folder: folder,
            }
        }

        /// Serves the stand-in screensaver on this bus. Output: its connection and its calls.
        fn screensaver(&self) -> (zbus::blocking::Connection, Arc<Mutex<Vec<Call>>>) {
            let service = ScreenSaver::default();
            let calls = service.calls.clone();
            let connection = zbus::blocking::connection::Builder::address(self.address.as_str())
                .unwrap()
                .name("org.freedesktop.ScreenSaver")
                .unwrap()
                .serve_at("/org/freedesktop/ScreenSaver", service)
                .unwrap()
                .build()
                .unwrap();
            (connection, calls)
        }
    }

    impl Drop for SessionBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    /// Runs only as the child of the display rows: for each "on" or "off" line on stdin it
    /// applies the hold with or without the display and answers "held true" or "held false";
    /// it ends when stdin closes.
    #[test]
    fn display_holder_process() {
        if std::env::var_os(DISPLAY_HOLDER).is_none() {
            return;
        }
        let mut hold = platform_hold(std::process::id());
        for line in std::io::stdin().lines().take(ASKS_MAX) {
            let display = line.unwrap() == "on";
            let held = hold.apply(&Want::Idle { display }).unwrap();
            println!("held {}", held.display);
        }
        hold.release();
    }

    /// The display holder child on a given session bus, driven through its stdin.
    struct Holder {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }

    impl Holder {
        fn start(address: &str) -> Self {
            let mut child = crazyagent::spawn_gate::spawn(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "linux::display_holder_process", "--nocapture"])
                    .env(DISPLAY_HOLDER, "1")
                    .env(SESSION, address)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            let input = child.stdin.take().unwrap();
            let output = BufReader::new(child.stdout.take().unwrap());
            Self {
                child,
                input,
                output,
            }
        }

        /// Asks for the display on or off. Input: "on" or "off". Output: whether the hold
        /// reports the display held.
        fn ask(&mut self, wanted: &str) -> bool {
            writeln!(self.input, "{wanted}").unwrap();
            self.input.flush().unwrap();
            for _ in 0..LINES_MAX {
                let mut line = String::new();
                assert!(
                    self.output.read_line(&mut line).unwrap() > 0,
                    "the holder ended"
                );
                if let Some(at) = line.find("held ") {
                    return line[at + "held ".len()..].trim() == "true";
                }
            }
            panic!("the holder never answered");
        }

        fn end(mut self) {
            drop(self.input);
            let _ = self.child.kill();
            self.child.wait().unwrap();
        }
    }

    #[test]
    fn display_awake_takes_a_session_inhibit_and_gives_it_back() {
        let bus = SessionBus::start();
        let (_service, calls) = bus.screensaver();
        let mut holder = Holder::start(&bus.address);

        assert!(holder.ask("on"), "the display is held");
        let taken = calls.lock().unwrap().clone();
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(taken[0].0, "Inhibit");
        assert_eq!(taken[0].2, format!("CrazyAgent: {REASON}"));
        let sender = taken[0].1.clone();
        assert!(sender.starts_with(':'), "{sender}");

        assert!(!holder.ask("off"), "the display is no longer held");
        let given = calls.lock().unwrap().clone();
        assert_eq!(given.len(), 2, "{given:?}");
        assert_eq!(
            given[1],
            ("UnInhibit".into(), sender, String::new(), COOKIE)
        );
        holder.end();
    }

    #[test]
    fn the_engines_session_connection_takes_no_descriptor() {
        let bus = SessionBus::start();
        let (service, calls) = bus.screensaver();
        let mut holder = Holder::start(&bus.address);
        assert!(holder.ask("on"));
        let taken = calls.lock().unwrap().clone();
        assert_eq!(taken.len(), 1, "{taken:?}");
        let sender = taken[0].1.clone();

        let probe = tempfile::NamedTempFile::new().unwrap();
        let file = std::fs::File::open(probe.path()).unwrap();
        let sent = service.call_method(
            Some(sender.as_str()),
            "/",
            Some("dev.crazyagent.Probe"),
            "Take",
            &(zbus::zvariant::Fd::from(file.as_fd()),),
        );
        let refused = match &sent {
            Err(zbus::Error::MethodError(name, _, _)) => name.as_str() == REFUSED,
            _ => false,
        };
        assert!(
            refused,
            "the bus refuses a descriptor for the engine: {sent:?}"
        );
        let pid = holder.child.id();
        let open = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .take(FDS_MAX)
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .any(|target| target == probe.path());
        assert!(!open, "the holder never holds the probe file");
        holder.end();
    }
}

#[cfg(windows)]
mod windows {
    use crazyagent::keep_awake::{REASON, Want, platform_hold};
    use std::process::Command;

    const LINES_MAX: usize = 4096;

    /// Tells whether `powercfg /requests` lists our reason. It needs an elevated prompt, so
    /// this row runs on the Windows probe machine as an administrator.
    fn listed() -> bool {
        let output =
            crazyagent::spawn_gate::output(Command::new("powercfg").arg("/requests")).unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).contains(REASON)
    }

    /// The request kinds `powercfg /requests` lists our reason under, in its order (DISPLAY
    /// before SYSTEM). Needs an elevated prompt, as `listed` does.
    fn kinds_listing_us() -> Vec<String> {
        let output =
            crazyagent::spawn_gate::output(Command::new("powercfg").arg("/requests")).unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        let mut kind = String::new();
        let mut kinds: Vec<String> = Vec::new();
        for line in text.lines().take(LINES_MAX).map(str::trim) {
            let heading = line.len() > 1
                && line.ends_with(':')
                && line[..line.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_uppercase());
            if heading {
                kind = line[..line.len() - 1].to_owned();
            } else if line.contains(REASON) && !kinds.contains(&kind) {
                kinds.push(kind.clone());
            }
        }
        kinds
    }

    #[test]
    fn powercfg_requests_lists_our_display_request_while_display_awake_is_on() {
        let mut hold = platform_hold(std::process::id());
        let held = hold.apply(&Want::Idle { display: true }).unwrap();
        assert!(held.display);
        assert_eq!(kinds_listing_us(), ["DISPLAY", "SYSTEM"]);
        let held = hold.apply(&Want::Idle { display: false }).unwrap();
        assert!(!held.display);
        assert_eq!(kinds_listing_us(), ["SYSTEM"]);
        hold.release();
        assert!(kinds_listing_us().is_empty());
    }

    #[test]
    fn powercfg_requests_lists_our_system_request_while_held() {
        let mut hold = platform_hold(std::process::id());
        assert!(!listed());
        let held = hold.apply(&Want::Idle { display: false }).unwrap();
        assert!(held.idle);
        assert!(!held.display);
        assert!(listed());
        hold.release();
        assert!(!listed());
    }
}

#[cfg(unix)]
mod journal {
    use crazyagent::engine::EngineError;
    use crazyagent::keep_awake::journal::{Entry, Journal};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    /// The stand-in for a program that changes a persistent setting: a real shell that
    /// writes one value into the stand-in setting file.
    fn change(setting: &Path, value: &str) {
        let status = crazyagent::spawn_gate::status(
            Command::new("/bin/sh")
                .args(["-c", "printf %s \"$1\" > \"$2\"", "sh", value])
                .arg(setting),
        )
        .unwrap();
        assert!(status.success());
    }

    fn entry() -> Entry {
        Entry {
            kind: "stand_in".into(),
            restore: "before".into(),
        }
    }

    #[test]
    fn the_journal_is_on_disk_before_the_change_and_reconcile_undoes_it() {
        let root = tempfile::tempdir().unwrap();
        let setting = root.path().join("setting");
        change(&setting, "before");
        let file = root.path().join("keep-awake").join("journal.json");

        let mut journal = Journal::open(root.path()).unwrap();
        assert!(journal.entries().is_empty());
        journal.record(entry()).unwrap();
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("stand_in"));
        assert!(written.contains("before"));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        change(&setting, "after");
        drop(journal);

        let mut journal = Journal::open(root.path()).unwrap();
        assert_eq!(journal.entries(), [entry()]);
        let failed = journal.reconcile(|_| Err(EngineError("stand_in_undo_failed".into())));
        assert_eq!(failed, Err(EngineError("stand_in_undo_failed".into())));
        assert_eq!(std::fs::read_to_string(&setting).unwrap(), "after");
        assert_eq!(Journal::open(root.path()).unwrap().entries(), [entry()]);

        journal
            .reconcile(|entry| {
                assert_eq!(entry.kind, "stand_in");
                change(&setting, &entry.restore);
                Ok(())
            })
            .unwrap();
        assert_eq!(std::fs::read_to_string(&setting).unwrap(), "before");
        assert!(journal.entries().is_empty());
        assert!(Journal::open(root.path()).unwrap().entries().is_empty());
    }

    /// DEC-167 addendum 32: settle undoes the entries of one kind, newest first, keeps the
    /// ones whose undo failed and every entry of another kind, and keeps the file in step.
    #[test]
    fn settle_undoes_one_kind_newest_first_and_keeps_every_other_kind() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(root.path()).unwrap();
        let lid = |restore: &str| Entry {
            kind: "lid".into(),
            restore: restore.into(),
        };
        for recorded in [lid("first"), entry(), lid("second"), lid("third")] {
            journal.record(recorded).unwrap();
        }
        let mut seen = Vec::new();
        let failed = journal.settle("lid", |undone| {
            assert_eq!(undone.kind, "lid");
            seen.push(undone.restore.clone());
            match undone.restore.as_str() {
                "second" => Err(EngineError("stand_in_undo_failed".into())),
                _ => Ok(()),
            }
        });
        assert_eq!(failed, Err(EngineError("stand_in_undo_failed".into())));
        assert_eq!(seen, ["third", "second", "first"]);
        assert_eq!(journal.entries(), [entry(), lid("second")]);
        assert_eq!(
            Journal::open(root.path()).unwrap().entries(),
            [entry(), lid("second")]
        );

        let none = journal.settle("other", |_| panic!("no entry of this kind"));
        assert_eq!(none, Ok(()));
        journal.settle("lid", |_| Ok(())).unwrap();
        assert_eq!(journal.entries(), [entry()]);
        assert_eq!(Journal::open(root.path()).unwrap().entries(), [entry()]);
    }

    #[test]
    fn an_unreadable_journal_is_an_error_and_stays_in_place() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(root.path()).unwrap();
        journal.record(entry()).unwrap();
        drop(journal);
        let file = root.path().join("keep-awake").join("journal.json");
        std::fs::write(&file, "not a journal").unwrap();

        let opened = Journal::open(root.path());
        assert!(opened.is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not a journal");
    }

    #[test]
    fn the_journal_refuses_more_entries_than_its_bound_and_oversized_ones() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(root.path()).unwrap();
        let long = Entry {
            kind: "k".repeat(65),
            restore: String::new(),
        };
        assert!(journal.record(long).is_err());
        for _ in 0..64 {
            journal.record(entry()).unwrap();
        }
        assert!(journal.record(entry()).is_err());
        assert_eq!(journal.entries().len(), 64);
        assert_eq!(Journal::open(root.path()).unwrap().entries().len(), 64);
    }
}
