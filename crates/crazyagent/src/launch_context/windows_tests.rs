use super::*;
use crazyagent_win_sandbox::private;

/// A temporary folder in the plain spelling H1's check takes. Output: the folder's guard and
/// its plain path.
fn base() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = kernel_sandbox::paths::canonical(temp.path()).unwrap();
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
pub(crate) fn context() -> AgentContext {
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

/// What the Windows install cannot write yet, a process's project files, tools, Codex and a
/// team relay, is refused with `launch_context_unavailable` once the home's checks pass,
/// before it takes the lock or changes the home, so the launch folder and a Codex
/// `config.toml` already there keep every byte.
#[test]
fn a_windows_install_it_cannot_write_yet_is_refused_before_it_changes_the_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    std::fs::create_dir(home.join(STATE)).unwrap();
    std::fs::write(home.join(STATE).join("kept"), "state").unwrap();
    std::fs::write(home.join("config.toml"), "model = \"kept\"\n").unwrap();
    let relay = AgentContext {
        relay: Some((base.join("engine.exe"), base.join("mailbox"))),
        ..context()
    };
    let cases = [
        ("process", context_with_process(&data), "claude-code", false),
        ("tools", context(), "claude-code", true),
        ("codex", context(), "codex", false),
        ("relay", relay, "claude-code", false),
    ];
    for (case, context, harness, tools) in cases {
        assert_eq!(
            context.install(harness, &home, tools).unwrap_err().0,
            "launch_context_unavailable",
            "{case}"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(STATE).join("kept")).unwrap(),
            "state",
            "{case}"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model = \"kept\"\n",
            "{case}"
        );
        assert!(!home.join(LOCK).exists(), "{case}");
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

/// Makes a junction at `link` that names `target`. Inputs: both absolute paths.
pub(crate) fn junction(link: &Path, target: &Path) {
    run(
        "cmd.exe",
        &[
            "/d",
            "/c",
            "mklink",
            "/J",
            link.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
}

/// A private folder outside the home holding one file, the place a planted junction names.
/// Inputs: the base and a name. Output: the folder's path.
fn outside(base: &Path, name: &str) -> PathBuf {
    let folder = private_home(base, name);
    std::fs::write(folder.join("kept"), name).unwrap();
    folder
}

/// Asserts a folder a junction named still holds only its one file, with its bytes. Inputs:
/// the folder and the name it was made with.
fn assert_untouched(folder: &Path, name: &str) {
    assert_eq!(std::fs::read_to_string(folder.join("kept")).unwrap(), name);
    assert_eq!(std::fs::read_dir(folder).unwrap().count(), 1);
}

/// A context that gives a process, as an agent launch with its project does. Input: the
/// project's data folder. Output: it.
fn context_with_process(data: &Path) -> AgentContext {
    AgentContext {
        process: Some(Box::new(Process {
            project: crate::agent_home::AgentProject {
                data: data.to_path_buf(),
                id: ProjectId::new("process").unwrap(),
                folder: None,
                mode: crate::instruction_files::Mode::Both,
                repeated: crate::agent_home::Repeated::LeaveOut,
            },
            hooks: Vec::new(),
            agent: String::new(),
            caches: crate::agent_home::Caches::default(),
        })),
        ..context()
    }
}

/// A Claude Code install with tools off writes its MCP list and its instructions into the
/// launch folder of a private home, names both in its arguments, and gives both the home's
/// inheritable list rather than a private one; it holds the home's lock until it drops, and
/// the drop removes the launch folder.
#[test]
fn a_windows_tools_off_claude_install_writes_its_two_files_and_holds_the_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let state = home.join(STATE);
    let installed = context().install("claude-code", &home, false).unwrap();
    let mcp = state.join("mcp.json");
    let instructions = state.join("instructions.md");
    assert_eq!(
        std::fs::read_to_string(&mcp).unwrap(),
        claude_config(&[]).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&instructions).unwrap(),
        context().instructions
    );
    assert_eq!(
        installed.arguments(),
        [
            OsString::from("--mcp-config"),
            mcp.clone().into_os_string(),
            OsString::from("--append-system-prompt-file"),
            instructions.clone().into_os_string(),
        ]
    );
    use crazyagent_win_sandbox::dacl;
    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    const USERS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
    let home_list = private::facts(&private::open_dir(&home).unwrap())
        .unwrap()
        .list
        .unwrap();
    let home_entries: Vec<_> = dacl::entries(&home_list)
        .unwrap()
        .iter()
        .map(|entry| (entry.allows, entry.mask, entry.sid.to_vec()))
        .collect();
    assert!(!home_entries.is_empty());
    for file in [&mcp, &instructions] {
        let opened = std::fs::File::open(file).unwrap();
        assert!(private::check(&opened).is_ok(), "{}", file.display());
        let list = private::facts(&opened).unwrap().list.unwrap();
        let entries = dacl::entries(&list).unwrap();
        let held: Vec<_> = entries
            .iter()
            .map(|entry| (entry.allows, entry.mask, entry.sid.to_vec()))
            .collect();
        assert_eq!(held, home_entries, "{}: {entries:?}", file.display());
        assert!(
            entries.iter().all(|entry| entry.sid != EVERYONE
                && entry.sid != USERS
                && !dacl::package(entry.sid)),
            "{}: {entries:?}",
            file.display()
        );
    }
    assert_held(&home);
    drop(installed);
    assert!(!state.exists());
    drop(lock_only(&home, "claude-code").unwrap());
}

/// A junction an earlier agent left at the launch folder's name, or inside the launch
/// folder, is removed as itself before the install writes: the folders it named keep every
/// byte and gain no file.
#[test]
fn a_windows_install_removes_a_planted_junction_as_itself() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let state = home.join(STATE);
    let at_name = outside(&base, "at-name");
    junction(&state, &at_name);
    drop(context().install("claude-code", &home, false).unwrap());
    assert_untouched(&at_name, "at-name");
    std::fs::create_dir(&state).unwrap();
    let inside = outside(&base, "inside");
    junction(&state.join("plugin"), &inside);
    let installed = context().install("claude-code", &home, false).unwrap();
    assert_untouched(&inside, "inside");
    assert!(!state.join("plugin").exists());
    assert!(state.join("mcp.json").is_file());
    drop(installed);
}

/// The drop removes the launch folder through the home it held, so a junction planted in
/// the launch folder after the install is removed as itself, and a junction above the home
/// pointed at another folder after the install never sends the drop there.
#[test]
fn a_windows_install_drop_never_follows_a_junction_planted_after_it() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let installed = context().install("claude-code", &home, false).unwrap();
    let planted = outside(&base, "planted");
    junction(&home.join(STATE).join("link"), &planted);
    drop(installed);
    assert_untouched(&planted, "planted");
    assert!(!home.join(STATE).exists());
    let first = private_home(&base, "first");
    private_home(&first, "home");
    let second = private_home(&base, "second");
    let elsewhere = private_home(&second, "home");
    std::fs::create_dir(elsewhere.join(STATE)).unwrap();
    std::fs::write(elsewhere.join(STATE).join("kept"), "second").unwrap();
    let alias = base.join("alias");
    junction(&alias, &first);
    let installed = context()
        .install("claude-code", &alias.join("home"), false)
        .unwrap();
    assert!(first.join("home").join(STATE).join("mcp.json").is_file());
    run("cmd.exe", &["/d", "/c", "rmdir", alias.to_str().unwrap()]);
    junction(&alias, &second);
    drop(installed);
    assert!(!first.join("home").join(STATE).exists());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join(STATE).join("kept")).unwrap(),
        "second"
    );
}

/// The Windows install's own judge runs on the home it holds, after every earlier check: a
/// home whose list was opened to the Users group is refused with `launch_context_unsafe`
/// before the lock is made or the launch folder is touched.
#[test]
fn a_windows_install_refuses_a_home_its_held_judge_refuses_before_a_lock() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    std::fs::create_dir(home.join(STATE)).unwrap();
    std::fs::write(home.join(STATE).join("kept"), "state").unwrap();
    run(
        "icacls.exe",
        &[
            home.to_str().unwrap(),
            "/grant",
            "*S-1-5-32-545:(OI)(CI)(RX)",
        ],
    );
    assert_eq!(
        windows::install(&context(), &home, false, false)
            .unwrap_err()
            .0,
        "launch_context_unsafe"
    );
    assert!(!home.join(LOCK).exists());
    assert_eq!(
        std::fs::read_to_string(home.join(STATE).join("kept")).unwrap(),
        "state"
    );
}

/// The held judge refuses a home whose label for what is made in it, an inherit-only entry,
/// is below medium or lacks no-write-up, as it refuses such a label on the home itself; an
/// inherit-only medium no-write-up label passes.
#[test]
fn the_held_home_judge_refuses_a_low_or_open_inherited_label() {
    const INHERITED: u8 = 0x01 | 0x02 | 0x08;
    const MEDIUM: u32 = 0x2000;
    const LOW: u32 = 0x1000;
    const NO_WRITE_UP: u32 = 0x1;
    let (_temp, base) = base();
    let held = private::dir(&base.join("home")).unwrap();
    let facts = private::facts(&held).unwrap();
    let user = private::user_sid().unwrap();
    assert!(windows::judged_home(&facts, &user));
    let labelled = |level, policy| private::Facts {
        label: Some(crazyagent_win_sandbox::dacl::labelled(
            level, policy, INHERITED,
        )),
        ..facts.clone()
    };
    assert!(windows::judged_home(&labelled(MEDIUM, NO_WRITE_UP), &user));
    assert!(!windows::judged_home(&labelled(LOW, NO_WRITE_UP), &user));
    assert!(!windows::judged_home(&labelled(MEDIUM, 0), &user));
}

/// A junction above the home repointed after a Claude Code install leaves the launch's
/// arguments naming the files in the home it held, by its final path, never the folder the
/// junction names now, whose own launch folder keeps every byte.
#[test]
fn a_windows_install_names_its_files_in_the_held_home_after_a_junction_above_is_repointed() {
    let (_temp, base) = base();
    let first = private_home(&base, "first");
    let held_home = private_home(&first, "home");
    let second = private_home(&base, "second");
    let other = private_home(&second, "home");
    std::fs::create_dir(other.join(STATE)).unwrap();
    std::fs::write(other.join(STATE).join("mcp.json"), "other").unwrap();
    let link = base.join("link");
    junction(&link, &first);
    let installed = context()
        .install("claude-code", &link.join("home"), false)
        .unwrap();
    std::fs::remove_dir(&link).unwrap();
    junction(&link, &second);
    let state = held_home.join(STATE);
    assert_eq!(
        installed.arguments(),
        [
            OsString::from("--mcp-config"),
            state.join("mcp.json").into_os_string(),
            OsString::from("--append-system-prompt-file"),
            state.join("instructions.md").into_os_string(),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(state.join("mcp.json")).unwrap(),
        claude_config(&[]).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(other.join(STATE).join("mcp.json")).unwrap(),
        "other"
    );
    drop(installed);
}

/// A folder above the home's parent cannot be moved while its launch holds the home, so no
/// junction can take its place to name another home in the held one's spelling; once the
/// launch drops, it moves.
#[test]
fn a_windows_folder_above_the_homes_parent_cannot_be_moved_while_its_launch_holds_it() {
    let (_temp, base) = base();
    let grand = private_home(&base, "grand");
    let parent = private_home(&grand, "parent");
    let home = private_home(&parent, "home");
    let installed = context().install("claude-code", &home, false).unwrap();
    assert!(std::fs::rename(&grand, base.join("moved")).is_err());
    drop(installed);
    std::fs::rename(&grand, base.join("moved")).unwrap();
}

/// A pane record a Windows install leaves names the home it held, by its final path: after
/// a junction above the home is repointed, an adopt holds and locks that home, so a second
/// launch through the junction pointed back is busy and the pane's launch folder stays.
#[test]
fn a_windows_pane_record_names_the_held_home_so_adopt_locks_it_after_a_junction_is_repointed() {
    let (_temp, base) = base();
    let first = private_home(&base, "first");
    let held_home = private_home(&first, "home");
    let second = private_home(&base, "second");
    let other = private_home(&second, "home");
    let link = base.join("link");
    junction(&link, &first);
    let installed = context()
        .install("claude-code", &link.join("home"), false)
        .unwrap();
    let files = installed.files().unwrap().clone();
    installed.keep();
    assert_eq!(files.home, held_home);
    std::fs::remove_dir(&link).unwrap();
    junction(&link, &second);
    let adopted = Installed::adopt(&files).unwrap();
    assert_held(&held_home);
    drop(lock_only(&other, "claude-code").unwrap());
    std::fs::remove_dir(&link).unwrap();
    junction(&link, &first);
    assert_eq!(
        context()
            .install("claude-code", &link.join("home"), false)
            .unwrap_err()
            .0,
        "launch_home_busy"
    );
    let mcp = held_home.join(STATE).join("mcp.json");
    assert_eq!(
        std::fs::read_to_string(&mcp).unwrap(),
        claude_config(&[]).unwrap()
    );
    drop(adopted);
    drop(lock_only(&held_home, "claude-code").unwrap());
}
