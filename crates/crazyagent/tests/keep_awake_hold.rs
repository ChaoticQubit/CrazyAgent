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
    use crazyagent::keep_awake::{Want, platform_hold};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const HOLDER: &str = "CRAZYAGENT_KEEP_AWAKE_ROW_HOLDER";
    const WAIT: Duration = Duration::from_secs(5);
    const STEP: Duration = Duration::from_millis(100);
    const HOLDER_SECONDS_MAX: u64 = 60;

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
}

#[cfg(windows)]
mod windows {
    use crazyagent::keep_awake::{REASON, Want, platform_hold};
    use std::process::Command;

    /// Tells whether `powercfg /requests` lists our reason. It needs an elevated prompt, so
    /// this row runs on the Windows probe machine as an administrator.
    fn listed() -> bool {
        let output =
            crazyagent::spawn_gate::output(Command::new("powercfg").arg("/requests")).unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).contains(REASON)
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
