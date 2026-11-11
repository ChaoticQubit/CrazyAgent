use super::*;
use crate::sandbox::warden::fixture::{detached_survivors, processes_naming};
use std::os::unix::fs::PermissionsExt;

const READ_ONLY_ENGINE_BASE: &str = "CRAZYAGENT_TEST_READ_ONLY_ENGINE_BASE";

/// A harness standing in for Codex that leaves what a shell tool can leave: a process in
/// its own session that writes `detached` in the agent's temp folder once its session has
/// started and then sleeps `seconds`. The harness waits for that file, then sleeps the same
/// time when it `stays`, or exits. Its sleeps run under [`sleep_marker`], the harness's own
/// path, so only this run's processes name it. Takes the seconds and whether it stays.
/// Returns the script.
fn detaching_harness(seconds: u32, stays: bool) -> String {
    let sleep = format!("exec {{ q(/bin/sleep) }} qq($ARGV[0]-sleep), q({seconds})");
    let then = if stays {
        format!(r#"exec /usr/bin/perl -e '{sleep}' "$0""#)
    } else {
        "exit 0".to_owned()
    };
    format!(
        r#"#!/bin/sh
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "$ENV{{TMPDIR}}/detached") or exit 1; close($file); {sleep}' "$0" < /dev/null > /dev/null 2>&1 &
while [ ! -e "$TMPDIR/detached" ]; do /bin/sleep 0.05; done
{then}
"#
    )
}

/// The command text both sleeps of the [`detaching_harness`] in `base` run under: the
/// harness's own path, unique to the run. Takes the base. Returns the text.
fn sleep_marker(base: &Path) -> String {
    format!("{}-sleep", read_only_place(base).3.to_str().unwrap())
}

/// The engine database, research state folder, read folder and harness of the read-only run
/// whose engine works in `base`. Takes the base. Returns them, none created.
fn read_only_place(base: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let engine = base.join("engine");
    (
        engine.join("engine.db"),
        engine.join("research"),
        base.join("repository"),
        base.join("bin").join("codex"),
    )
}

/// Makes a private folder under `/tmp` holding an engine database, its research state, a
/// folder to read and a harness running `script`, such as a [`detaching_harness`]. Takes
/// the script. Returns the folder, removed when dropped.
fn read_only_base(script: &str) -> tempfile::TempDir {
    let temp = tempfile::Builder::new()
        .prefix("ca-read-only-warden-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let (database, state, folder, harness) = read_only_place(temp.path());
    for private in [
        database.parent().unwrap(),
        &state,
        &folder,
        harness.parent().unwrap(),
    ] {
        fs::create_dir(private).unwrap();
        fs::set_permissions(private, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sqlite = rusqlite::Connection::open(&database).unwrap();
    sqlite.execute_batch("PRAGMA user_version=1").unwrap();
    sqlite.close().map_err(|(_, error)| error).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&harness, script).unwrap();
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

/// Runs the read-only run of the engine in `base` on a loopback endpoint the harness never
/// calls, as onboarding's mapping step does. Takes the base and the stop poll. Returns how
/// it ended, or the runner failure.
fn read_only_run(base: &Path, stop: &dyn Fn() -> bool) -> Result<ReadOnlyEnding, RunError> {
    let (database, state, folder, harness) = read_only_place(base);
    let runner =
        ResearchRunner::new(&database, &state, &harness, ResearchLimits::default()).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    runner.run_read_only(
        &ReadOnlyRun {
            name: "onboarding-warden",
            agent: "onboarding",
            project: None,
            folder: &folder,
            prompt: "Describe this repository.",
            account: &account,
            wall_time: Duration::from_secs(60),
            output_tokens: None,
        },
        stop,
    )
}

/// Waits up to 30 s for at least `count` processes of this user to name `needle`. Takes the
/// text and the count. Returns them.
fn running_naming(needle: &str, count: usize) -> Vec<(i32, String)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = processes_naming(needle);
        if found.len() >= count {
            return found;
        }
        assert!(Instant::now() < deadline, "{needle}: only {found:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_stopped_read_only_run_leaves_nothing_its_harness_detached_into_its_own_session() {
    let mut bystander = crate::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("3311")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let base = read_only_base(&detaching_harness(3311, true));
    let marker = sleep_marker(base.path());
    let stopping = AtomicBool::new(false);
    let ending = std::thread::scope(|scope| {
        let run = scope
            .spawn(|| read_only_run(base.path(), &|| stopping.load(Ordering::Relaxed)).unwrap());
        running_naming(&marker, 2);
        stopping.store(true, Ordering::Relaxed);
        run.join().unwrap()
    });
    let survivors = detached_survivors(Instant::now() + Duration::from_secs(2), &[&marker]);
    let spared = bystander.try_wait().unwrap();
    let _ = bystander.kill();
    bystander.wait().unwrap();
    assert_eq!(ending, ReadOnlyEnding::Stopped);
    assert_eq!(survivors, []);
    assert_eq!(
        spared, None,
        "a process of this user that only shares the sleep length was not started by the run"
    );
}

#[test]
fn a_read_only_run_whose_harness_exits_leaves_nothing_it_detached() {
    let base = read_only_base(&detaching_harness(3312, false));
    let marker = sleep_marker(base.path());
    let ending = read_only_run(base.path(), &|| false).unwrap();
    assert!(
        !matches!(ending, ReadOnlyEnding::TimedOut | ReadOnlyEnding::Stopped),
        "{ending:?}"
    );
    assert_eq!(
        detached_survivors(Instant::now() + Duration::from_secs(2), &[&marker]),
        []
    );
}

/// Runs the read-only run in a second engine process, which this test runs again as itself
/// with [`READ_ONLY_ENGINE_BASE`] naming the folder, and kills that engine with SIGKILL
/// once its harness and the detached process both run.
#[test]
fn a_read_only_run_whose_engine_dies_leaves_nothing_its_harness_detached() {
    if let Some(base) = std::env::var_os(READ_ONLY_ENGINE_BASE) {
        let ending = read_only_run(Path::new(&base), &|| false);
        panic!("the engine under test was not killed: {ending:?}");
    }
    let base = read_only_base(&detaching_harness(3313, true));
    let marker = sleep_marker(base.path());
    let module = module_path!().split_once("::").unwrap().1;
    let test =
        format!("{module}::a_read_only_run_whose_engine_dies_leaves_nothing_its_harness_detached");
    let mut engine = crate::spawn_gate::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(READ_ONLY_ENGINE_BASE, base.path())
            .stdin(Stdio::null()),
    )
    .unwrap();
    running_naming(&marker, 2);
    let exited = engine.try_wait().unwrap();
    engine.kill().unwrap();
    engine.wait().unwrap();
    assert_eq!(exited, None);
    assert_eq!(
        detached_survivors(Instant::now() + Duration::from_secs(5), &[&marker]),
        []
    );
}

#[test]
fn a_read_only_run_whose_warden_was_killed_fails_with_its_cleanup_uncertain_and_leaves_nothing() {
    let base = read_only_base(&detaching_harness(3314, true));
    let marker = sleep_marker(base.path());
    let (_, _, _, harness) = read_only_place(base.path());
    let needle = harness.to_str().unwrap().to_owned();
    let stopping = AtomicBool::new(false);
    let ending = std::thread::scope(|scope| {
        let run = scope.spawn(|| read_only_run(base.path(), &|| stopping.load(Ordering::Relaxed)));
        running_naming(&marker, 2);
        let killed = crate::sandbox::warden::fixture::kill_wardens_of(&needle);
        assert_eq!(killed.len(), 1, "one warden per run");
        stopping.store(true, Ordering::Relaxed);
        run.join().unwrap()
    });
    let left = detached_survivors(Instant::now() + Duration::from_secs(2), &[&marker]);
    assert!(
        matches!(ending, Err(RunError::Uncertain("warden_unconfirmed"))),
        "{ending:?}"
    );
    assert_eq!(
        left.len(),
        1,
        "the detached process outlived the group: {left:?}"
    );
}
