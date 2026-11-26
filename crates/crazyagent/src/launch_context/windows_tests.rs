use super::*;
use crazyagent_win_sandbox::private;
use kernel_sandbox::policy::HomeCaches;

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
/// lock then holds it until the guard drops. A Codex hold takes the same lock once its skills
/// are removed through the held home, and gives the lock back.
#[test]
fn a_windows_lock_only_holds_a_private_home_until_its_guard_drops() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let held = lock_only(&home, "claude-code").unwrap();
    assert_held(&home);
    drop(held);
    drop(lock_only(&home, "claude-code").unwrap());
    let codex = lock_only(&home, "codex").unwrap();
    assert_held(&home);
    drop(codex);
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

/// What the Windows install cannot write yet, tools for Claude Code or Codex and a team
/// relay, is refused with `launch_context_unavailable` once the home's checks pass, before
/// it takes the lock or changes the home, so the launch folder and a Codex `config.toml`
/// already there keep every byte.
#[test]
fn a_windows_install_it_cannot_write_yet_is_refused_before_it_changes_the_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    std::fs::create_dir(home.join(STATE)).unwrap();
    std::fs::write(home.join(STATE).join("kept"), "state").unwrap();
    std::fs::write(home.join("config.toml"), "model = \"kept\"\n").unwrap();
    let relay = AgentContext {
        relay: Some((base.join("engine.exe"), base.join("mailbox"))),
        ..context()
    };
    let cases = [
        ("tools", context(), "claude-code", true),
        ("codex tools", context(), "codex", true),
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

/// A context that gives a process, as an agent launch with its project does, with the
/// agent's own text and a caches choice. Inputs: the project's data folder, the text and
/// the choice. Output: it.
fn process_context(data: &Path, agent: &str, choice: HomeCaches) -> AgentContext {
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
            agent: agent.to_string(),
            caches: crate::agent_home::Caches {
                choice,
                level: None,
            },
        })),
        ..context()
    }
}

/// The text a context's process gives its agent. Input: the context. Output: the text.
fn given(context: &AgentContext) -> String {
    process_text(context.process.as_ref().unwrap())
        .unwrap()
        .text
}

/// Whether nothing is at `path`, not even a link. Input: the path. Output: the answer.
fn gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
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

/// A Claude Code launch with tools off and a process prepares its home through the held
/// root: the instruction file holds the given text, the launch's instructions end with it,
/// the given and level records are written in the data root, every entry the home does not
/// keep goes, inside a kept folder of only some names too, and every kept one stays.
#[test]
fn a_windows_process_install_prepares_the_home_through_the_held_root() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    std::fs::write(home.join("stray.txt"), "stray").unwrap();
    std::fs::create_dir_all(home.join("junk").join("inner")).unwrap();
    std::fs::write(home.join("junk").join("inner").join("file"), "junk").unwrap();
    std::fs::write(home.join(".claude.json"), "{}").unwrap();
    std::fs::create_dir_all(home.join("projects").join("p1")).unwrap();
    std::fs::write(home.join("projects").join("p1").join("t.jsonl"), "t").unwrap();
    std::fs::create_dir_all(home.join(".claude").join("teams")).unwrap();
    std::fs::create_dir_all(home.join(".claude").join("other")).unwrap();
    let context = process_context(&data, "Check every row.", HomeCaches::Keep);
    let given = given(&context);
    assert!(given.contains("Check every row."));
    let installed = context.install("claude-code", &home, false).unwrap();
    let claude = home.join("CLAUDE.md");
    assert_eq!(std::fs::read_to_string(&claude).unwrap(), given);
    assert!(private::check(&std::fs::File::open(&claude).unwrap()).is_ok());
    assert_eq!(
        std::fs::read_to_string(home.join(STATE).join("instructions.md")).unwrap(),
        format!("{}\n{given}", context.instructions)
    );
    for removed in [
        home.join("stray.txt"),
        home.join("junk"),
        home.join(".claude").join("other"),
    ] {
        assert!(gone(&removed), "{}", removed.display());
    }
    for kept in [
        home.join(".claude.json"),
        home.join("projects").join("p1").join("t.jsonl"),
        home.join(".claude").join("teams"),
    ] {
        assert!(kept.exists(), "{}", kept.display());
    }
    let records = |folder: &str| -> Vec<PathBuf> {
        std::fs::read_dir(data.join(folder))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let recorded = records("agent-instructions-given");
    assert_eq!(recorded.len(), 1);
    assert_eq!(std::fs::read_to_string(&recorded[0]).unwrap(), given);
    assert_eq!(records("agent-home-levels").len(), 1);
    drop(installed);
    assert!(gone(&home.join(STATE)));
    assert_eq!(std::fs::read_to_string(&claude).unwrap(), given);
}

/// Junctions an earlier agent left in its home, at a name the home does not keep, at a kept
/// folder's name and inside a kept folder of only some names, are removed as themselves:
/// the folders they named keep every byte and gain no file.
#[test]
fn a_windows_process_install_removes_planted_junctions_in_the_home_as_themselves() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    let at_name = outside(&base, "at-name");
    let at_kept = outside(&base, "at-kept");
    let in_only = outside(&base, "in-only");
    junction(&home.join("linked"), &at_name);
    junction(&home.join("teams"), &at_kept);
    std::fs::create_dir(home.join(".claude")).unwrap();
    junction(&home.join(".claude").join("linked"), &in_only);
    let context = process_context(&data, "Rows.", HomeCaches::Keep);
    drop(context.install("claude-code", &home, false).unwrap());
    for removed in [
        home.join("linked"),
        home.join("teams"),
        home.join(".claude").join("linked"),
    ] {
        assert!(gone(&removed), "{}", removed.display());
    }
    assert_untouched(&at_name, "at-name");
    assert_untouched(&at_kept, "at-kept");
    assert_untouched(&in_only, "in-only");
}

/// A junction or a folder an earlier agent left at the instruction file's name is removed
/// as itself, and the given text is written there create-only: what the junction named
/// keeps every byte.
#[test]
fn a_windows_process_install_replaces_a_link_or_folder_at_the_instruction_file() {
    let (_temp, base) = base();
    for case in ["junction", "folder"] {
        let home = private_home(&base, case);
        let data = private_home(&base, &format!("{case}-data"));
        let named = format!("{case}-target");
        let target = outside(&base, &named);
        let claude = home.join("CLAUDE.md");
        if case == "junction" {
            junction(&claude, &target);
        } else {
            std::fs::create_dir(&claude).unwrap();
            std::fs::write(claude.join("kept"), "planted").unwrap();
        }
        let context = process_context(&data, "Rows.", HomeCaches::Keep);
        let given = given(&context);
        drop(context.install("claude-code", &home, false).unwrap());
        assert!(
            std::fs::symlink_metadata(&claude).unwrap().is_file(),
            "{case}"
        );
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), given, "{case}");
        assert_untouched(&target, &named);
    }
}

/// With no given text the instruction file goes, a junction at its name as itself, so a
/// stale copy never outlives a cleared page and what the junction named keeps every byte.
#[test]
fn a_windows_process_install_with_no_text_removes_the_instruction_file() {
    let (_temp, base) = base();
    let target = outside(&base, "target");
    for case in ["file", "junction"] {
        let home = private_home(&base, case);
        let data = private_home(&base, &format!("{case}-data"));
        let claude = home.join("CLAUDE.md");
        if case == "file" {
            std::fs::write(&claude, "stale").unwrap();
        } else {
            junction(&claude, &target);
        }
        let context = process_context(&data, "", HomeCaches::Keep);
        assert!(given(&context).is_empty(), "{case}");
        drop(context.install("claude-code", &home, false).unwrap());
        assert!(gone(&claude), "{case}");
    }
    assert_untouched(&target, "target");
}

/// The download caches, `.cache` and npm's content cache, stay under Keep and go under
/// Clear every launch.
#[test]
fn a_windows_process_install_keeps_or_clears_the_caches_by_the_choice() {
    let (_temp, base) = base();
    for (case, choice, stays) in [
        ("keep", HomeCaches::Keep, true),
        ("clear", HomeCaches::ClearEveryLaunch, false),
    ] {
        let home = private_home(&base, case);
        let data = private_home(&base, &format!("{case}-data"));
        for cache in [".cache", ".npm/_cacache"] {
            std::fs::create_dir_all(home.join(cache)).unwrap();
            std::fs::write(home.join(cache).join("blob"), "cached").unwrap();
        }
        let context = process_context(&data, "Rows.", choice);
        drop(context.install("claude-code", &home, false).unwrap());
        for cache in [".cache", ".npm/_cacache"] {
            assert_eq!(
                home.join(cache).join("blob").exists(),
                stays,
                "{case} {cache}"
            );
        }
    }
}

/// In the kept `projects` folder each project's auto-memory goes and a junction directly in
/// it goes as itself, while the transcripts stay and what the junction named keeps every
/// byte.
#[test]
fn a_windows_process_install_clears_project_memory_and_links_but_keeps_transcripts() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    let target = outside(&base, "target");
    let project = home.join("projects").join("p1");
    std::fs::create_dir_all(project.join("memory")).unwrap();
    std::fs::write(project.join("memory").join("m.md"), "memory").unwrap();
    std::fs::write(project.join("t.jsonl"), "t").unwrap();
    junction(&home.join("projects").join("linked"), &target);
    let context = process_context(&data, "Rows.", HomeCaches::Keep);
    drop(context.install("claude-code", &home, false).unwrap());
    assert!(gone(&project.join("memory")));
    assert!(gone(&home.join("projects").join("linked")));
    assert_eq!(
        std::fs::read_to_string(project.join("t.jsonl")).unwrap(),
        "t"
    );
    assert_untouched(&target, "target");
}

/// A kept file the engine cannot open for a reason other than its absence, here one another
/// handle holds without sharing, refuses the launch with `agent_home_unwritable` and stays
/// with its bytes: a failed open never removes a kept file.
#[test]
fn a_windows_process_install_keeps_a_kept_file_it_cannot_open() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    let kept = home.join(".claude.json");
    std::fs::write(&kept, "{\"kept\":true}").unwrap();
    let holder = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&kept)
            .unwrap()
    };
    assert_eq!(
        std::fs::File::open(&kept).unwrap_err().raw_os_error(),
        Some(32)
    );
    let context = process_context(&data, "Rows.", HomeCaches::Keep);
    assert_eq!(
        context.install("claude-code", &home, false).unwrap_err().0,
        "agent_home_unwritable"
    );
    drop(holder);
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "{\"kept\":true}");
}

/// Asserts each file holds exactly the home's inheritable list, passes W1's private check
/// and grants nothing to Everyone, Users or a package. Inputs: the home and the files.
fn assert_home_list(home: &Path, files: &[&Path]) {
    use crazyagent_win_sandbox::dacl;
    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    const USERS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
    let home_list = private::facts(&private::open_dir(home).unwrap())
        .unwrap()
        .list
        .unwrap();
    let home_entries: Vec<_> = dacl::entries(&home_list)
        .unwrap()
        .iter()
        .map(|entry| (entry.allows, entry.mask, entry.sid.to_vec()))
        .collect();
    assert!(!home_entries.is_empty());
    for file in files {
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
}

/// The `config.toml` a Codex launch with tools off writes in a home: its instructions and
/// no hooks, keyed to the home's own config path. Inputs: the home and the instructions.
/// Output: the text.
fn codex_text(home: &Path, instructions: &str) -> String {
    let source = std::fs::canonicalize(home).unwrap().join("config.toml");
    codex_config(instructions, &[], &[], &[], &source.to_string_lossy(), None).unwrap()
}

/// Whether a home's `skills` is a real folder, no link, holding nothing. Input: the home.
/// Output: the answer.
fn skills_empty(home: &Path) -> bool {
    let skills = home.join("skills");
    let found = std::fs::symlink_metadata(&skills).unwrap();
    found.is_dir()
        && !found.file_type().is_symlink()
        && std::fs::read_dir(&skills).unwrap().count() == 0
}

/// Plants an earlier run's `skills` in a home: a file, a nested folder, Codex's `.system`
/// folder and a junction to a folder outside the home. Inputs: the home and the junction's
/// target.
fn plant_skills(home: &Path, target: &Path) {
    let skills = home.join("skills");
    std::fs::create_dir_all(skills.join("nested")).unwrap();
    std::fs::create_dir_all(skills.join(".system")).unwrap();
    std::fs::write(skills.join("a.md"), "a").unwrap();
    std::fs::write(skills.join("nested").join("b.md"), "b").unwrap();
    std::fs::write(
        skills.join(".system").join(".codex-system-skills.marker"),
        "m",
    )
    .unwrap();
    junction(&skills.join("linked"), target);
}

/// A Codex install with tools off writes its `config.toml` with the home's inheritable list
/// and an empty hook key list through the held home, names no argument, leaves an empty
/// `skills` and holds the home's lock; the drop removes the launch folder and the config
/// and leaves `skills` empty.
#[test]
fn a_windows_tools_off_codex_install_writes_its_config_and_holds_the_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let config = home.join("config.toml");
    let installed = context().install("codex", &home, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        codex_text(&home, &context().instructions)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(STATE).join(CODEX_HOOK_KEYS)).unwrap(),
        "[]"
    );
    assert!(installed.arguments().is_empty());
    assert!(skills_empty(&home));
    assert_home_list(&home, &[&config]);
    assert_held(&home);
    drop(installed);
    assert!(gone(&home.join(STATE)));
    assert!(gone(&config));
    assert!(skills_empty(&home));
    drop(lock_only(&home, "claude-code").unwrap());
}

/// The `skills` an earlier run left, a junction inside it included, is removed as itself
/// before a Codex install writes anything, and an empty `skills` is made again: what the
/// junction named keeps every byte.
#[test]
fn a_windows_codex_install_removes_an_earlier_runs_skills_without_following() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let target = outside(&base, "target");
    plant_skills(&home, &target);
    drop(context().install("codex", &home, false).unwrap());
    assert!(skills_empty(&home));
    assert_untouched(&target, "target");
}

/// A junction at `skills` or at `config.toml` is removed as itself, never followed: an
/// empty real `skills` and the config are made in their place, and what each junction
/// named keeps every byte.
#[test]
fn a_windows_codex_install_removes_a_junction_at_skills_or_config_as_itself() {
    let (_temp, base) = base();
    for name in ["skills", "config.toml"] {
        let home = private_home(&base, &format!("{name}-home"));
        let named = format!("{name}-target");
        let target = outside(&base, &named);
        junction(&home.join(name), &target);
        drop(context().install("codex", &home, false).unwrap());
        assert!(skills_empty(&home), "{name}");
        assert_untouched(&target, &named);
    }
    let home = private_home(&base, "config-written");
    let target = outside(&base, "config-target-2");
    junction(&home.join("config.toml"), &target);
    let installed = context().install("codex", &home, false).unwrap();
    let config = home.join("config.toml");
    assert!(std::fs::symlink_metadata(&config).unwrap().is_file());
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        codex_text(&home, &context().instructions)
    );
    drop(installed);
    assert_untouched(&target, "config-target-2");
}

/// A Codex hold at `lock_only` removes an earlier run's `skills` through the held home,
/// a junction inside it as itself, leaves an empty `skills`, holds the lock until it drops
/// and gives it back.
#[test]
fn a_windows_codex_lock_only_removes_an_earlier_runs_skills_without_following() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let target = outside(&base, "target");
    plant_skills(&home, &target);
    let held = lock_only(&home, "codex").unwrap();
    assert_held(&home);
    assert!(skills_empty(&home));
    assert_untouched(&target, "target");
    drop(held);
    drop(lock_only(&home, "claude-code").unwrap());
}

/// A Codex launch with a process prepares the home through the held root: AGENTS.md holds
/// the given text, the config holds the launch's own instructions, and an entry the home
/// does not keep goes.
#[test]
fn a_windows_codex_process_install_prepares_the_home_through_the_held_root() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let data = private_home(&base, "data");
    std::fs::write(home.join("stray.txt"), "stray").unwrap();
    let context = process_context(&data, "Check every row.", HomeCaches::Keep);
    let given = given(&context);
    let installed = context.install("codex", &home, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join("AGENTS.md")).unwrap(),
        given
    );
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml")).unwrap(),
        codex_text(&home, &context.instructions)
    );
    assert!(gone(&home.join("stray.txt")));
    assert!(skills_empty(&home));
    drop(installed);
}

/// A `skills` the engine cannot remove, a file in it held by another handle without delete
/// sharing, refuses a Codex install with `launch_context_skills_kept` before the config or
/// the hook key list is written, and gives the lock back.
#[test]
fn a_windows_codex_install_refuses_skills_it_cannot_remove_before_the_config() {
    use std::os::windows::fs::OpenOptionsExt;
    const SHARING_VIOLATION: i32 = 32;
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    std::fs::create_dir(home.join("skills")).unwrap();
    let busy = home.join("skills").join("busy.md");
    std::fs::write(&busy, "busy").unwrap();
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&busy)
        .unwrap();
    assert_eq!(
        std::fs::remove_file(&busy).unwrap_err().raw_os_error(),
        Some(SHARING_VIOLATION)
    );
    assert_eq!(
        context().install("codex", &home, false).unwrap_err().0,
        SKILLS_KEPT
    );
    assert!(gone(&home.join("config.toml")));
    assert!(gone(&home.join(STATE).join(CODEX_HOOK_KEYS)));
    assert_eq!(std::fs::read_to_string(&busy).unwrap(), "busy");
    drop(lock_only(&home, "claude-code").unwrap());
    drop(holder);
}

/// Makes a W1 private folder `home` inside `folder` with one skill file in its `skills`.
/// Inputs: the folder and the skill's bytes. Output: the home's path.
fn home_with_skill(folder: &Path, bytes: &str) -> PathBuf {
    let home = private_home(folder, "home");
    std::fs::create_dir(home.join("skills")).unwrap();
    std::fs::write(home.join("skills").join("kept.md"), bytes).unwrap();
    home
}

/// A junction above the home pointed at another folder between the first hold and the
/// Codex skills removal makes the second open reach another home: it is refused with
/// `launch_context_unsafe`, both homes' `skills` keep every byte, and no lock stays held.
#[test]
fn a_windows_codex_skills_removal_refuses_a_home_swapped_after_the_hold() {
    let (_temp, base) = base();
    let first = private_home(&base, "first");
    let second = private_home(&base, "second");
    let first_home = home_with_skill(&first, "first");
    let second_home = home_with_skill(&second, "second");
    let link = base.join("link");
    junction(&link, &first);
    let home = link.join("home");
    let held = crate::harnesses::launch::hold_home(&home).unwrap();
    std::fs::remove_dir(&link).unwrap();
    junction(&link, &second);
    assert_eq!(
        windows::codex_copies(&home, &held).unwrap_err().0,
        "launch_context_unsafe"
    );
    for (folder, bytes) in [(&first_home, "first"), (&second_home, "second")] {
        assert_eq!(
            std::fs::read_to_string(folder.join("skills").join("kept.md")).unwrap(),
            bytes
        );
    }
    drop(held);
    drop(lock_only(&second_home, "claude-code").unwrap());
}

/// With no swap, the Codex skills removal after the first hold takes the same home: the
/// earlier run's `skills` goes, a junction in it as itself, and an empty one is made.
#[test]
fn a_windows_codex_skills_removal_after_the_hold_takes_the_held_home() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    let target = outside(&base, "target");
    plant_skills(&home, &target);
    let held = crate::harnesses::launch::hold_home(&home).unwrap();
    windows::codex_copies(&home, &held).unwrap();
    assert!(skills_empty(&home));
    assert_untouched(&target, "target");
    drop(held);
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

/// Holds `first/home` through a junction `link`, after a plain launch of `second/home`
/// recorded its own given text, and repoints the junction at `second`, so the caller's
/// spelling names that other home while the held one stays. Inputs: the temp base and the
/// data root. Output: the held root, the held home and the other home.
fn held_through_a_repointed_link(
    base: &Path,
    data: &Path,
) -> (crate::engine::bootstrap::held::Folder, PathBuf, PathBuf) {
    use crate::engine::bootstrap::held::Folder;
    let first = private_home(base, "first");
    let held_home = private_home(&first, "home");
    let second = private_home(base, "second");
    let other = private_home(&second, "home");
    let seeded = process_context(data, "Other rows.", HomeCaches::Keep);
    drop(seeded.install("claude-code", &other, false).unwrap());
    let link = base.join("link");
    junction(&link, &first);
    let root = Folder::root_judged_by(&link.join("home"), |_| true).unwrap();
    std::fs::remove_dir(&link).unwrap();
    junction(&link, &second);
    (root, held_home, other)
}

/// The texts of every record one record folder of the data root holds, sorted. Inputs: the
/// data root and the folder name. Output: the texts.
fn record_texts(data: &Path, folder: &str) -> Vec<String> {
    let mut texts: Vec<String> = std::fs::read_dir(data.join(folder))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    texts.sort();
    texts
}

/// A process launch whose home was held before a junction above it was repointed keys its
/// given, level and instruction-file records by the held home's place: the other home's
/// records keep their bytes beside them, and only the held home gets the instruction file.
#[test]
fn a_windows_process_keys_its_records_by_the_held_home_after_a_junction_above_is_repointed() {
    let (_temp, base) = base();
    let data = private_home(&base, "data");
    let (root, held_home, other) = held_through_a_repointed_link(&base, &data);
    let context = process_context(&data, "Check every row.", HomeCaches::Keep);
    let other_given = given(&process_context(&data, "Other rows.", HomeCaches::Keep));
    let held_given = windows::prepare_process(&root, context.process.as_ref().unwrap(), false)
        .unwrap()
        .text;
    assert_eq!(held_given, given(&context));
    let mut both = vec![other_given.clone(), held_given.clone()];
    both.sort();
    assert_eq!(record_texts(&data, "agent-instructions-given"), both);
    for folder in ["agent-home-levels", "agent-instruction-files"] {
        assert_eq!(record_texts(&data, folder).len(), 2, "{folder}");
    }
    let claude = |home: &Path| std::fs::read_to_string(home.join("CLAUDE.md")).unwrap();
    assert_eq!(claude(&held_home), held_given);
    assert_eq!(claude(&other), other_given);
}

/// The same launch keys its skills record by the held home's place too, beside the other
/// home's record, never in its place.
#[test]
fn a_windows_process_keys_its_skills_record_by_the_held_home_after_a_junction_above_is_repointed() {
    let (_temp, base) = base();
    let data = private_home(&base, "data");
    let (root, _held_home, _other) = held_through_a_repointed_link(&base, &data);
    assert_eq!(record_texts(&data, "agent-skills").len(), 1);
    let context = process_context(&data, "Check every row.", HomeCaches::Keep);
    windows::prepare_process(&root, context.process.as_ref().unwrap(), false).unwrap();
    assert_eq!(record_texts(&data, "agent-skills").len(), 2);
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

/// A `skills` an earlier run nested 17 folders deep and a `config.toml` left as a folder
/// chain 17 deep are both removed at the next Codex install, which then makes an empty
/// `skills` and writes its config, so no depth blocks a later Codex launch.
#[test]
fn a_windows_codex_install_removes_deep_skills_and_a_deep_config_folder() {
    let (_temp, base) = base();
    let home = private_home(&base, "home");
    for top in ["skills", "config.toml"] {
        let bottom = (0..17).fold(home.join(top), |path, _| path.join("d"));
        std::fs::create_dir_all(&bottom).unwrap();
        std::fs::write(bottom.join("deep.md"), "deep").unwrap();
    }
    drop(context().install("codex", &home, false).unwrap());
    assert!(skills_empty(&home));
    assert!(home.join("config.toml").is_file());
}
