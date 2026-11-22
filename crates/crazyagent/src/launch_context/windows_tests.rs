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
    let ran = kernel_processes::spawn_gate::collected(&mut command).unwrap();
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
        handover: None,
        tool_home: None,
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
        rendered: Vec::new(),
        listed: false,
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
            rendered: Vec::new(),
            listed: false,
        })
    });
}

/// CodeRabbit CR-1140 (1): a W1 private home passes H1's check at `install`, which is then
/// refused with `launch_context_unavailable` before it takes the lock or changes the home,
/// so the launch state folder and a Codex `config.toml` already there keep every byte. It
/// used to delete the state folder before its first file write was refused.
#[test]
fn a_windows_install_is_refused_before_it_changes_the_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    std::fs::create_dir(home.join(STATE)).unwrap();
    std::fs::write(home.join(STATE).join("kept"), "state").unwrap();
    std::fs::write(home.join("config.toml"), "model = \"kept\"\n").unwrap();
    for harness in ["claude-code", "codex"] {
        assert_eq!(
            context().install(harness, &home, false).unwrap_err().0,
            "launch_context_unavailable",
            "{harness}"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(STATE).join("kept")).unwrap(),
            "state"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model = \"kept\"\n"
        );
        assert!(!home.join(LOCK).exists(), "{harness}");
    }
}

/// CodeRabbit CR-1140 (2): from before H1's check until its lock is taken, a Windows launch
/// holds its home open without delete sharing, so neither the home nor a folder above it
/// can be renamed, and nothing can take the home's name between the check and the lock's
/// by-path open. Once the hold ends, both renames work.
#[test]
fn a_windows_home_and_its_parent_cannot_be_moved_while_its_launch_holds_it() {
    let (_temp, base) = base();
    let parent = private_home(&base, "parent");
    let home = private_home(&parent, "home");
    let held = crate::harnesses::launch::hold_home(&home).unwrap();
    assert!(std::fs::rename(&home, parent.join("moved")).is_err());
    assert!(std::fs::rename(&parent, base.join("moved")).is_err());
    drop(held);
    std::fs::rename(&home, parent.join("moved")).unwrap();
    std::fs::rename(&parent, base.join("moved")).unwrap();
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

/// V244-WIN-MASTER S9a r3 (CodeRabbit CR-1252 1): a home that is not an absolute path on a
/// drive, a device name, a network share or a relative path, is refused with H1's
/// `launch_account_home_unsafe` before the hold opens anything, so no share is contacted.
#[test]
fn a_windows_launch_refuses_a_home_off_a_drive_before_it_holds_it() {
    for home in [
        r"\\.\NUL",
        r"\\crazyagent.invalid\share\home",
        "relative-home",
    ] {
        assert_eq!(
            crate::harnesses::launch::hold_home(Path::new(home))
                .unwrap_err()
                .0,
            "launch_account_home_unsafe",
            "{home}"
        );
    }
}

/// V244-WIN-MASTER S9a r3 (CodeRabbit CR-1252 2): a hold blocks renaming the home and the
/// folders above it, not pointing a junction above it elsewhere. A home spelled through a
/// junction is held, the junction is then pointed at another folder, and the lock its path
/// now reaches is refused with `launch_context_unsafe`; the lock in the held home passes.
#[test]
fn a_windows_launch_lock_its_path_reached_outside_the_held_home_is_refused() {
    let (_temp, base) = base();
    let first = private_home(&base, "first");
    let held_home = private_home(&first, "home");
    let second = private_home(&base, "second");
    private_home(&second, "home");
    let alias = base.join("alias");
    let junction = |target: &Path| {
        run(
            "cmd.exe",
            &[
                "/d",
                "/c",
                "mklink",
                "/J",
                alias.to_str().unwrap(),
                target.to_str().unwrap(),
            ],
        )
    };
    junction(&first);
    let home = alias.join("home");
    let held = crate::harnesses::launch::hold_home(&home).unwrap();
    run("cmd.exe", &["/d", "/c", "rmdir", alias.to_str().unwrap()]);
    junction(&second);
    let elsewhere = lock(&home).unwrap();
    assert!(second.join("home").join(LOCK).exists());
    assert_eq!(
        locked_in(&elsewhere, &held).unwrap_err().0,
        "launch_context_unsafe"
    );
    drop(elsewhere);
    let own = lock(&held_home).unwrap();
    locked_in(&own, &held).unwrap();
}
