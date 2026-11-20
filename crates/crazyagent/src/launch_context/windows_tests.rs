use super::*;
use crazyagent_win_sandbox::private;

/// A temporary folder in the plain spelling H1's check takes. Output: the folder's guard and
/// its plain path.
fn base() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    (temp, base)
}

/// Makes a W1 private folder. Inputs: the base and a name. Output: its path.
fn private_home(base: &Path, name: &str) -> PathBuf {
    let home = base.join(name);
    drop(private::dir(&home).unwrap());
    home
}

/// Runs one System32 tool and asserts it succeeds. Inputs: its file name and arguments.
fn run(tool: &str, args: &[&str]) {
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let mut command = std::process::Command::new(system.join(tool));
    command.args(args);
    let ran = crate::spawn_gate::collected(&mut command).unwrap();
    assert!(ran.status.success(), "{tool} {args:?} failed");
}

/// The two homes H1's check refuses as `launch_account_home_unsafe`: a private folder the
/// Users group can read, and a junction to a private home. Input: the base. Output: both
/// homes and the junction's target.
fn refused_homes(base: &Path) -> ([PathBuf; 2], PathBuf) {
    let open = private_home(base, "open");
    run(
        "icacls.exe",
        &[
            open.to_str().unwrap(),
            "/grant",
            "*S-1-5-32-545:(OI)(CI)(RX)",
        ],
    );
    let target = private_home(base, "target");
    let linked = base.join("linked");
    run(
        "cmd.exe",
        &[
            "/d",
            "/c",
            "mklink",
            "/J",
            linked.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
    ([open, linked], target)
}

/// Asserts each home is refused as `launch_account_home_unsafe` before any lock is made:
/// no lock name in the home or in the junction's target. Inputs: the homes, the target and
/// the call under test.
fn assert_refused_before_a_lock(
    homes: &[PathBuf],
    target: &Path,
    take: impl Fn(&Path) -> Result<Installed, LaunchError>,
) {
    for home in homes {
        assert_eq!(
            take(home).unwrap_err().0,
            "launch_account_home_unsafe",
            "{}",
            home.display()
        );
        assert!(!home.join(LOCK).exists(), "{}", home.display());
    }
    assert!(!target.join(LOCK).exists());
}

/// Asserts the home's launch lock is a W1 private file and is held: a second take is
/// `launch_home_busy`. Input: the home.
fn assert_held(home: &Path) {
    drop(private::file(&home.join(LOCK), false).unwrap());
    assert_eq!(
        lock_only(home, "claude-code").unwrap_err().0,
        "launch_home_busy"
    );
}

/// A launch context with nothing to install, as the Unix rows build one. Output: it.
fn context() -> AgentContext {
    AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: false,
        off_tools: Vec::new(),
        native_team: None,
        native_threads: None,
        relay: None,
        model_proxy: None,
        freeze_record: None,
        terminal_display: TerminalDisplay::Classic,
        catch_up_main: true,
    }
}

/// V244-WIN-MASTER S9a: a W1 private home passes H1's check at `lock_only`, and the Windows
/// lock then holds it until the guard drops. A Codex hold stays refused until its skills can
/// be removed handle-relative (S9b), and gives the lock back.
#[test]
fn a_windows_lock_only_holds_a_private_home_until_its_guard_drops() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let held = lock_only(&home, "claude-code").unwrap();
    assert_held(&home);
    drop(held);
    drop(lock_only(&home, "claude-code").unwrap());
    assert_eq!(
        lock_only(&home, "codex").unwrap_err().0,
        "launch_context_unavailable"
    );
    drop(lock_only(&home, "claude-code").unwrap());
}

/// A Windows `lock_only` refuses a home open to the Users group and a junction home with
/// H1's code before it makes a lock.
#[test]
fn a_windows_lock_only_refuses_an_open_or_linked_home_before_a_lock() {
    let (_temp, base) = base();
    let (homes, target) = refused_homes(&base);
    assert_refused_before_a_lock(&homes, &target, |home| lock_only(home, "claude-code"));
}

/// A W1 private home passes H1's check at `adopt`, and the Windows lock then holds it until
/// the adopted launch drops.
#[test]
fn a_windows_adopted_launch_holds_a_private_home_until_it_drops() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let files = InstalledFiles {
        home: home.clone(),
        codex: false,
    };
    let adopted = Installed::adopt(&files).unwrap();
    assert_held(&home);
    drop(adopted);
    drop(lock_only(&home, "claude-code").unwrap());
}

/// A Windows `adopt` refuses a home open to the Users group and a junction home with H1's
/// code before it makes a lock.
#[test]
fn a_windows_adopt_refuses_an_open_or_linked_home_before_a_lock() {
    let (_temp, base) = base();
    let (homes, target) = refused_homes(&base);
    assert_refused_before_a_lock(&homes, &target, |home| {
        Installed::adopt(&InstalledFiles {
            home: home.to_path_buf(),
            codex: false,
        })
    });
}

/// A W1 private home passes H1's check at `install`, which takes the Windows lock as a
/// private file, then stops at the first file it cannot yet write handle-relative (S9b)
/// with `launch_context_unavailable`, its lock given back.
#[test]
fn a_windows_install_takes_the_lock_then_stops_before_its_files() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    assert_eq!(
        context()
            .install("claude-code", &home, false)
            .unwrap_err()
            .0,
        "launch_context_unavailable"
    );
    drop(private::file(&home.join(LOCK), false).unwrap());
    drop(lock_only(&home, "claude-code").unwrap());
}

/// A Windows `install` refuses a home open to the Users group and a junction home with H1's
/// code before it makes a lock.
#[test]
fn a_windows_install_refuses_an_open_or_linked_home_before_a_lock() {
    let (_temp, base) = base();
    let (homes, target) = refused_homes(&base);
    let context = context();
    assert_refused_before_a_lock(&homes, &target, |home| {
        context.install("claude-code", home, false)
    });
}
