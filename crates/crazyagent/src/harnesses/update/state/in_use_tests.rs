use super::*;

/// Adds the team runtime tables and, when asked, a bare agent runs table to a database,
/// with one row per harness name and status. Inputs: the database, the team agents and
/// the agent runs. Output: none; a failed write panics.
fn fill(db: &Connection, agents: &[(&str, &str)], runs: Option<&[(&str, &str)]>) {
    crate::team::initialize(db).unwrap();
    for (harness, status) in agents {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES('p-1','sub_agent',?1,1,?2,1,1)",
            rusqlite::params![harness, status],
        )
        .unwrap();
    }
    let Some(runs) = runs else { return };
    db.execute_batch("CREATE TABLE agent_runs(harness TEXT NOT NULL, state TEXT NOT NULL)")
        .unwrap();
    for (harness, state) in runs {
        db.execute(
            "INSERT INTO agent_runs(harness,state) VALUES(?1,?2)",
            rusqlite::params![harness, state],
        )
        .unwrap();
    }
}

/// An in-memory database filled as [`fill`] does. Inputs: the team agents and the agent
/// runs. Output: the database.
fn live_db(agents: &[(&str, &str)], runs: Option<&[(&str, &str)]>) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    fill(&db, agents, runs);
    db
}

/// A team agent of a harness that is starting, idle or working runs it, whether it is
/// stored under the catalog id or an alias.
#[test]
fn a_live_team_agent_of_a_harness_by_its_id_or_an_alias_is_in_use() {
    for status in ["starting", "idle", "working"] {
        assert_eq!(
            in_use(&live_db(&[("codex", status)], None), "codex"),
            Ok(true)
        );
    }
    let alias = live_db(&[("claude", "working")], None);
    assert_eq!(in_use(&alias, "claude-code"), Ok(true));
}

/// A team agent that ended, or one of another harness, runs nothing the update replaces.
#[test]
fn an_ended_team_agent_or_another_harness_is_not_in_use() {
    let ended = live_db(&[("codex", "stopped"), ("codex", "failed")], None);
    assert_eq!(in_use(&ended, "codex"), Ok(false));
    assert_eq!(
        in_use(&live_db(&[("claude", "working")], None), "codex"),
        Ok(false)
    );
}

/// An agent run still running uses its harness; one that closed does not.
#[test]
fn a_running_agent_run_is_in_use_and_a_closed_one_is_not() {
    assert_eq!(
        in_use(&live_db(&[], Some(&[("codex", "running")])), "codex"),
        Ok(true)
    );
    assert_eq!(
        in_use(&live_db(&[], Some(&[("codex", "closed")])), "codex"),
        Ok(false)
    );
}

/// A database that never made the session tables holds no session, so it refuses
/// nothing; this is the Windows engine, which keeps no agent runs.
#[test]
fn a_database_without_the_session_tables_has_nothing_in_use() {
    assert_eq!(
        in_use(&Connection::open_in_memory().unwrap(), "codex"),
        Ok(false)
    );
}

/// A job whose turn came reads the sessions again from the database file: a live one
/// refuses with the in-use code, none lets it go on, and a database with no file to open
/// again refuses as unavailable.
#[test]
fn a_job_reads_the_sessions_again_from_the_database_file() -> Result<(), String> {
    let folder =
        tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    let file = folder.path().join("engine.db");
    let path = utf8(&file)?;
    let db = Connection::open(&file).map_err(|error| format!("open {path}: {error}"))?;
    fill(&db, &[("codex", "working")], None);
    assert_eq!(
        reopened(path).and_then(|db| unused(&db, "codex")),
        Err(IN_USE)
    );
    assert_eq!(
        reopened(path).and_then(|db| unused(&db, "claude-code")),
        Ok(())
    );
    assert!(matches!(reopened(""), Err(UNAVAILABLE)));
    let missing = folder.path().join("missing.db");
    assert!(matches!(reopened(utf8(&missing)?), Err(UNAVAILABLE)));
    Ok(())
}

/// A path as the `&str` the reopen takes. Input: the path. Output: its text, or an error
/// naming a path that is not UTF-8.
fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// A database with the update settings, the team runtime tables and Check and update on,
/// holding one team agent of a harness in a status. Inputs: the stored harness name and
/// the status. Output: the database.
fn sessions_db(harness: &str, status: &str) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let settings = UpdateSettings {
        mode: UpdateMode::CheckAndUpdate,
        ..CLOSED
    };
    set_settings(&db, settings).unwrap();
    fill(&db, &[(harness, status)], None);
    db
}

/// The catalog entry of a harness, found at a file under a fresh folder, and roots with
/// no tool. Input: the catalog id. Output: the folder, which holds the file, the entry and
/// the roots.
fn found_entry(id: &str) -> (tempfile::TempDir, HarnessEntry, Roots) {
    let folder = tempfile::tempdir().unwrap();
    let mut entry = harnesses::catalog(&harnesses::Search::new(Vec::new(), None, Vec::new()))
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap();
    entry.executable = Some(folder.path().join(format!("{id}.exe")));
    let roots = Roots {
        home: folder.path().to_path_buf(),
        brew_prefixes: Vec::new(),
        casks: false,
        npm: None,
        places: crate::tools::tools_new(Vec::new(), None),
        #[cfg(windows)]
        elevated: false,
    };
    (folder, entry, roots)
}

/// On Windows an update of a harness that a live session runs, named by its catalog id
/// or by an alias, is refused before anything starts, since a running harness's image
/// cannot be replaced; a session that ended refuses nothing.
#[cfg(windows)]
#[test]
fn a_windows_update_of_a_harness_a_live_session_runs_is_refused() {
    for (id, stored, status) in [
        ("codex", "codex", "working"),
        ("codex", "codex", "starting"),
        ("claude-code", "claude", "idle"),
    ] {
        let (_folder, entry, roots) = found_entry(id);
        let refused = Updates::new().start(&sessions_db(stored, status), &entry, &roots, |_| {});
        assert_eq!(refused, Err(IN_USE), "{id} {stored} {status}");
    }
    let (_folder, entry, roots) = found_entry("codex");
    let ended = Updates::new().start(&sessions_db("codex", "stopped"), &entry, &roots, |_| {});
    assert_ne!(ended, Err(IN_USE));
    let other = Updates::new().start(&sessions_db("claude", "working"), &entry, &roots, |_| {});
    assert_ne!(other, Err(IN_USE));
}

/// On macOS and Linux a live session refuses no update, as before: the system replaces
/// the file under a running harness, and refusing there is the owner's call.
#[cfg(not(windows))]
#[test]
fn an_update_outside_windows_is_not_refused_for_a_live_session() {
    let (_folder, entry, roots) = found_entry("codex");
    let started = Updates::new().start(&sessions_db("codex", "working"), &entry, &roots, |_| {});
    assert_ne!(started, Err(IN_USE));
}

/// A program running from the folders an update changes refuses it as in use, the reader
/// being asked about exactly those folders; none running lets it go on, and processes that
/// cannot be read refuse it as unavailable.
#[test]
fn a_program_running_from_the_update_folders_refuses_and_a_failed_read_is_unavailable() {
    let held = [PathBuf::from("prefix")];
    let asked: Images = |folders| Ok(folders == [PathBuf::from("prefix")]);
    assert_eq!(not_running(&held, asked), Err(IN_USE));
    assert_eq!(not_running(&held, |_| Ok(false)), Ok(()));
    assert_eq!(not_running(&held, |_| Err(5)), Err(UNAVAILABLE));
}

/// An update watches the folders its command changes, and a self-updater, whose command
/// changes none, the folder of the harness's real file.
#[test]
fn an_update_watches_its_folders_or_the_folder_of_a_self_updater() {
    let real = Path::new("tools").join("fx").join("fx.exe");
    let held = vec![PathBuf::from("prefix")];
    assert_eq!(watched(&held, &real), held);
    assert_eq!(watched(&[], &real), vec![Path::new("tools").join("fx")]);
}

/// While an update or install job holds a harness, a Windows launch of it is refused with
/// the updating code and a launch elsewhere goes on; once the job ends, also by a panic,
/// the launch goes on again.
#[test]
fn a_launch_during_a_running_job_refuses_on_windows_until_the_job_ends() {
    let updates = Updates::new();
    let running = updates.begin("fx").unwrap();
    assert_eq!(launchable("fx", true).err(), Some(LAUNCH_UPDATING));
    assert!(launchable("fx", false).is_ok());
    drop(running);
    assert!(launchable("fx", true).is_ok());
    let held = Arc::clone(&updates);
    let ended = std::thread::spawn(move || {
        let _running = held.begin("fx").unwrap();
        assert_eq!(launchable("fx", true).err(), Some(LAUNCH_UPDATING));
        panic!("the job failed");
    })
    .join();
    assert!(ended.is_err());
    assert!(launchable("fx", true).is_ok());
}

/// A Windows launch holds its harness from its update check until its process started: a
/// job of the harness begun meanwhile is refused as in use, two holds both count, a launch
/// refused while the job runs holds nothing, and once every hold drops the job begins.
#[test]
fn a_held_launch_refuses_a_job_of_its_harness_until_every_hold_drops() {
    let updates = Updates::new();
    let first = launchable("fx-held", true).unwrap();
    let second = launchable("fx-held", true).unwrap();
    assert_eq!(updates.begin("fx-held").err(), Some(IN_USE));
    drop(first);
    assert_eq!(updates.begin("fx-held").err(), Some(IN_USE));
    drop(second);
    let running = updates.begin("fx-held").unwrap();
    assert_eq!(launchable("fx-held", true).err(), Some(LAUNCH_UPDATING));
    drop(running);
    assert!(updates.begin("fx-held").is_ok());
    let elsewhere = launchable("fx-held", false).unwrap();
    assert!(updates.begin("fx-held").is_ok());
    drop(elsewhere);
}

/// A launch that holds a harness before its update begins makes the update refuse as in
/// use before anything starts; once the hold drops, the update is not refused so.
#[test]
fn a_launch_held_before_an_update_begins_makes_the_update_refuse() {
    let (_folder, entry, roots) = found_entry("opencode");
    let db = sessions_db("codex", "stopped");
    let held = launchable("opencode", true).unwrap();
    let refused = Updates::new().start(&db, &entry, &roots, |_| {});
    assert_eq!(refused, Err(IN_USE));
    drop(held);
    let started = Updates::new().start(&db, &entry, &roots, |_| {});
    assert_ne!(started, Err(IN_USE));
}

/// A started child that is killed and waited for once it goes out of scope, so a failed
/// assert leaves no suspended process behind; errors there are ignored, since a child that
/// already ended is no failure.
#[cfg(windows)]
struct Held(std::process::Child);

#[cfg(windows)]
impl Held {
    /// Ends the child now and waits for it, for a row that checks what follows its end.
    /// Output: Ok, or the error naming the step and the process.
    fn end(&mut self) -> Result<(), String> {
        let id = self.0.id();
        self.0
            .kill()
            .map_err(|error| format!("kill {id}: {error}"))?;
        self.0
            .wait()
            .map(drop)
            .map_err(|error| format!("wait {id}: {error}"))
    }
}

#[cfg(windows)]
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The Windows System32 folder and a new temporary prefix in its canonical spelling, for a
/// row that starts a program held suspended. Output: the folder, the temporary folder that
/// owns the prefix, and the prefix, or the error naming the step.
#[cfg(windows)]
fn windows_prefix() -> Result<(PathBuf, tempfile::TempDir, PathBuf), String> {
    let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
    let made = tempfile::tempdir().map_err(|error| format!("make a temporary folder: {error}"))?;
    let prefix = kernel_sandbox::paths::canonical(made.path())
        .map_err(|error| format!("canonical {}: {error}", made.path().display()))?;
    Ok((PathBuf::from(root).join("System32"), made, prefix))
}

/// Starts a program held suspended with no window, through the spawn gate. Inputs: the
/// program and its arguments. Output: the held child, or the error naming the program.
#[cfg(windows)]
fn suspended(program: &Path, arguments: &[&std::ffi::OsStr]) -> Result<Held, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    kernel_processes::spawn_gate::spawn(
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0000_0004 | 0x0800_0000),
    )
    .map(Held)
    .map_err(|error| format!("spawn {}: {error}", program.display()))
}

/// On Windows a program started from an update's folder outside any runtime, held
/// suspended, refuses the update as in use through the process snapshot until it ends.
#[cfg(windows)]
#[test]
fn a_windows_program_started_outside_the_runtime_under_the_prefix_refuses_the_update()
-> Result<(), String> {
    let (system, _made, prefix) = windows_prefix()?;
    let ping = system.join("PING.EXE");
    let copy = prefix.join("fx.exe");
    std::fs::copy(&ping, &copy)
        .map_err(|error| format!("copy {} to {}: {error}", ping.display(), copy.display()))?;
    let mut child = suspended(
        &copy,
        &[
            std::ffi::OsStr::new("-n"),
            std::ffi::OsStr::new("60"),
            std::ffi::OsStr::new("127.0.0.1"),
        ],
    )?;
    let held = [prefix];
    assert_eq!(not_running(&held, IMAGES), Err(IN_USE));
    child.end()?;
    assert_eq!(not_running(&held, IMAGES), Ok(()));
    Ok(())
}

/// On Windows a runtime started from outside an update's folder whose command line names a
/// script under it, as `node.exe` runs an npm harness, refuses the update as in use until it
/// ends, so a session the database does not know is still seen.
#[cfg(windows)]
#[test]
fn a_windows_runtime_whose_command_line_names_a_script_under_the_prefix_refuses_the_update()
-> Result<(), String> {
    let (system, _made, prefix) = windows_prefix()?;
    let script = prefix.join("node_modules").join("fx").join("cli.js");
    let mut child = suspended(
        &system.join("PING.EXE"),
        &[
            std::ffi::OsStr::new("-n"),
            std::ffi::OsStr::new("60"),
            script.as_os_str(),
        ],
    )?;
    let held = [prefix];
    assert_eq!(not_running(&held, IMAGES), Err(IN_USE));
    child.end()?;
    assert_eq!(not_running(&held, IMAGES), Ok(()));
    Ok(())
}
