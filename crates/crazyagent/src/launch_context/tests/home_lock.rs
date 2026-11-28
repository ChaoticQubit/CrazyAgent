use super::*;

/// A launch locks a home whose lock name an agent took for something that is no plain file
/// (RV-DS241-137 lock, rv-code2 13:10 note): a folder with a file in it, a link to a file
/// outside the home and a FIFO each go without being followed, a plain lock is made in their
/// place and held, and the file the link named stays as it was. Before, each one refused
/// every later launch of the home, since the open that follows no link never got a file.
#[cfg(unix)]
#[test]
fn a_launch_replaces_a_lock_name_that_is_no_plain_file_and_locks_the_home() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let outside = root.join("owner.txt");
    fs::write(&outside, "the owner's\n").unwrap();
    for planted in ["folder", "link", "fifo"] {
        let home = root.join(planted);
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let lock = home.join(LOCK);
        match planted {
            "folder" => {
                fs::create_dir(&lock).unwrap();
                fs::write(lock.join("inside"), "x").unwrap();
            }
            "link" => std::os::unix::fs::symlink(&outside, &lock).unwrap(),
            _ => {
                let made = kernel_processes::spawn_gate::collected(
                    std::process::Command::new("/usr/bin/mkfifo").arg(&lock),
                )
                .unwrap();
                assert!(made.status.success(), "{made:?}");
            }
        }

        let held = lock_only(&home, "claude-code").map_err(|error| error.0);
        let second = lock_only(&home, "claude-code").err().map(|error| error.0);

        assert_eq!(held.as_ref().err(), None, "{planted}");
        assert_eq!(second.as_deref(), Some("launch_home_busy"), "{planted}");
        let found = fs::symlink_metadata(&lock).unwrap();
        assert!(found.file_type().is_file(), "{planted}");
        assert_eq!(found.nlink(), 1, "{planted}");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "the owner's\n");
    }
}

/// A launch locks a home whose lock an agent left with no owner read or write, which one
/// `chmod 000` of it does on Linux, where an agent writes its whole home: the lock is this
/// user's plain file, so it gets its owner-only mode back and is held, and a second launch
/// is refused as busy. Before, the open failed and every later launch of the home was
/// refused.
#[cfg(unix)]
#[test]
fn a_launch_locks_a_home_whose_lock_was_left_with_no_owner_read_or_write() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("claude");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let first = lock_only(&home, "claude-code")
        .map(|_| ())
        .map_err(|error| error.0);
    assert_eq!(first, Ok(()));
    fs::set_permissions(home.join(LOCK), fs::Permissions::from_mode(0o000)).unwrap();

    let held = lock_only(&home, "claude-code").map_err(|error| error.0);
    let second = lock_only(&home, "claude-code").err().map(|error| error.0);

    assert_eq!(held.as_ref().err(), None);
    assert_eq!(second.as_deref(), Some("launch_home_busy"));
    let found = fs::symlink_metadata(home.join(LOCK)).unwrap();
    assert!(found.file_type().is_file());
    assert_eq!(found.mode() & 0o777, 0o600);
}

/// Two launches that find a folder at the lock name of one home never both hold the home:
/// the one that clears the name holds the home's folder meanwhile, so the other neither
/// removes the lock the first made nor makes a second one. The step run while the first
/// clears the name stands in for the second launch.
#[cfg(unix)]
#[test]
fn a_second_launch_never_removes_the_lock_a_first_made_in_place_of_a_folder() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("claude");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    fs::create_dir(home.join(LOCK)).unwrap();
    let racing = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen = racing.clone();
    let other = home.clone();
    WHILE_LOCK_NAME_CLEARS.with(|step| {
        *step.borrow_mut() = Some(Box::new(move || {
            let second = lock_only(&other, "claude-code").err().map(|error| error.0);
            *seen.borrow_mut() = Some(second);
        }));
    });

    let held = lock_only(&home, "claude-code").map_err(|error| error.0);

    assert_eq!(held.as_ref().err(), None);
    assert_eq!(
        racing.borrow().clone(),
        Some(Some("launch_home_busy".to_owned()))
    );
    assert!(
        fs::symlink_metadata(home.join(LOCK))
            .unwrap()
            .file_type()
            .is_file()
    );
}

/// A launch locks a home an earlier run left with an ACL that denies listing and searching
/// it (RV-DS241-137 code r2 F3): the retry clears the home's ACL by its path, which needs
/// neither, where clearing it on the folder itself opens it and fails.
#[cfg(target_os = "macos")]
#[test]
fn a_launch_locks_a_home_whose_acl_denies_listing_and_searching_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("claude");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let added = kernel_processes::spawn_gate::collected(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny list,search"])
            .arg(&home),
    )
    .unwrap();
    assert!(added.status.success(), "{added:?}");

    let locked = lock_only(&home, "claude-code")
        .map(|_| ())
        .map_err(|error| error.0);

    let _ = kernel_processes::spawn_gate::collected(
        std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&home),
    );
    assert_eq!(locked, Ok(()));
}

/// A context-less launch's hold takes the same home lock as an install and leaves the home's
/// files as they were (aabe420f): an install or a second hold on the held home is refused, the
/// home keeps its own file and gains only the lock file, and the home is free once the hold
/// ends.
#[cfg(unix)]
#[test]
fn lock_only_holds_the_home_and_leaves_no_files() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap().join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    fs::write(home.join("config.toml"), "model = \"kept\"\n").unwrap();
    let context = AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
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
    };

    let held = lock_only(&home, "codex").unwrap();
    assert_eq!(
        context
            .install("codex", &home, false)
            .err()
            .map(|error| error.0),
        Some("launch_home_busy".to_owned()),
        "an install waits for the held home"
    );
    assert_eq!(
        lock_only(&home, "codex").err().map(|error| error.0),
        Some("launch_home_busy".to_owned()),
        "a second hold waits for the held home"
    );
    drop(held);

    let mut names: Vec<String> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["config.toml", LOCK]);
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        "model = \"kept\"\n"
    );
    assert!(
        lock_only(&home, "codex").is_ok(),
        "the ended hold still kept its home busy"
    );
}

/// RV-DS-74: a second keyed launch on an account home whose agent is already running is
/// refused at the home lock before it writes the key file, so the running agent's relay key
/// file survives. The lock is taken by `command_with_context` ahead of `write_key_file`; the
/// refusal must leave the earlier key file exactly as it was.
#[cfg(unix)]
#[test]
fn a_refused_keyed_launch_on_a_busy_home_keeps_the_running_agents_key_file() {
    use crate::harnesses::launch::{
        AccountBinding, LaunchBoundary, LaunchLimits, LaunchRequest, Secret, Session,
    };
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let profile = base.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
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
    };
    let held = context().install("codex", &home, false).unwrap();
    let key_path = home.join(".crazyagent-key");
    fs::write(&key_path, b"running-agent-relay-token").unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: std::path::PathBuf::from("/usr/bin/true"),
        working_dir: work,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: Some(Secret::new("crazyagent-local".to_owned()).unwrap()),
        },
        model: None,
        tools: false,
        boundary: LaunchBoundary::Profile(profile),
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let refused = Session::start_with_context(request, context());
    assert_eq!(
        refused.err().map(|error| error.0).as_deref(),
        Some("launch_home_busy")
    );
    assert_eq!(fs::read(&key_path).unwrap(), b"running-agent-relay-token");
    drop(held);
}

#[cfg(unix)]
#[test]
fn a_refused_context_less_durable_launch_on_a_busy_home_keeps_the_running_agents_key_file() {
    use crate::harnesses::launch::{
        AccountBinding, LaunchBoundary, LaunchLimits, LaunchRequest, Secret, Session,
    };
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let home = base.join("codex");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let state = base.join("state");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let profile = base.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let context = || AgentContext {
        plugins: LaunchPlugins::default(),
        instructions: "# Project context from CrazyAgent\n".into(),
        process: None,
        transcripts: true,
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
    };
    let held = context().install("codex", &home, false).unwrap();
    let key_path = home.join(".crazyagent-key");
    fs::write(&key_path, b"running-agent-relay-token").unwrap();
    let request = LaunchRequest {
        harness: "codex".into(),
        executable: std::path::PathBuf::from("/usr/bin/true"),
        working_dir: work,
        account: AccountBinding::Url {
            home: home.clone(),
            base_url: "http://127.0.0.1:11434".into(),
            key: Some(Secret::new("crazyagent-local".to_owned()).unwrap()),
        },
        model: None,
        tools: false,
        boundary: LaunchBoundary::Profile(profile),
        git_identity: None,
        limits: LaunchLimits::default(),
    };
    let refused = Session::start_durable(request, None, &state);
    assert_eq!(
        refused.err().map(|error| error.0).as_deref(),
        Some("launch_home_busy")
    );
    assert_eq!(fs::read(&key_path).unwrap(), b"running-agent-relay-token");
    drop(held);
}
