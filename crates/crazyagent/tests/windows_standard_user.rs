#![cfg(windows)]

use crazyagent::harnesses::update::{self, Roots, Updates};
use crazyagent::harnesses::{self, HarnessEntry, Search};
use crazyagent_win_sandbox::private;
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const OLDER: &str = "0.170.0";
const PACKAGE: &str = "@openai/codex";
const JOB_TIME: Duration = Duration::from_secs(900);
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Makes a throwaway engine root in the running user's local application data folder, where
/// the engine's own root lives, never in a temp folder the harness search sets aside, with the
/// engine database and its harness update table in place. Output: the folder, kept until it
/// drops, its path in plain drive spelling and the open database.
fn engine() -> (tempfile::TempDir, PathBuf, rusqlite::Connection) {
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA for this user");
    let disk = tempfile::Builder::new()
        .prefix("ca-standard-user-")
        .tempdir_in(local)
        .unwrap();
    let root = kernel_sandbox::paths::canonical(disk.path()).unwrap();
    let db = crazyagent::landing::open(&root).unwrap();
    update::initialize(&db).unwrap();
    (disk, root, db)
}

/// The Codex row of the catalog the engine lists for one engine root, with the engine's own
/// npm prefix as the last folder looked in. Input: the engine root. Output: the row.
fn codex(root: &Path) -> HarnessEntry {
    let search = Search::from_environment().with_engine(root);
    let found = harnesses::catalog(&search)
        .into_iter()
        .find(|entry| entry.id == "codex");
    found.expect("the catalog always lists codex")
}

/// Tells a waiting row that the job holding it let it go, on every path a job ends by: called,
/// refused before its turn or failed.
struct Ended(mpsc::Sender<()>);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Waits for a job's end signal and then for the update state to record it and free the
/// harness, for at most `JOB_TIME`. Inputs: the update state and the signal. Output: whether
/// the job ended in time.
fn ended(updates: &Updates, told: &mpsc::Receiver<()>) -> bool {
    let deadline = Instant::now() + JOB_TIME;
    if told.recv_timeout(JOB_TIME).is_err() {
        return false;
    }
    while !updates.idle() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

/// Checks that a folder is owned by the running user's SID, read where it sits without
/// following a link, and, when asked, that it also passes the engine's private-folder test.
/// Inputs: the folder and whether it must be private. Output: Ok, or what failed.
fn owned(folder: &Path, private_too: bool) -> Result<(), String> {
    let user = private::user_sid().map_err(|error| format!("user SID: {error}"))?;
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
        .open(folder)
        .map_err(|error| format!("open {}: {error}", folder.display()))?;
    let facts =
        private::facts(&opened).map_err(|error| format!("facts {}: {error}", folder.display()))?;
    if facts.owner != user {
        return Err(format!("{} is owned by another SID", folder.display()));
    }
    if private_too {
        private::open_dir(folder)
            .map_err(|error| format!("{} is not private: {error}", folder.display()))?;
    }
    Ok(())
}

#[test]
#[ignore = "standard-user gate: run by the windows-2022 farm job as a created standard user"]
fn a_standard_user_is_not_elevated_and_owns_what_it_makes() {
    assert!(
        matches!(crazyagent_win_sandbox::elevated(), Ok(false)),
        "this row runs only as a standard user: {:?}",
        crazyagent_win_sandbox::elevated()
    );
    let (_disk, root, _db) = engine();
    assert!(!Roots::engine(&root).elevated);
    owned(&root, false).unwrap();
}

#[test]
#[ignore = "standard-user gate: run by the windows-2022 farm job as a created standard user"]
fn a_standard_user_installs_checks_and_updates_an_npm_harness_into_the_engine_prefix() {
    let (_disk, root, db) = engine();
    let roots = Roots::engine(&root);
    assert!(!roots.elevated, "the engine token reads as elevated");
    let npm = roots.npm.clone().expect("a usable Node on this machine");
    let before = codex(&root);
    assert_eq!(before.executable, None, "codex is already installed here");

    let updates = Updates::new();
    let search_root = root.clone();
    let (tell, told) = mpsc::channel();
    let signal = Ended(tell);
    updates
        .install(&db, &before, &roots, move || {
            let _held = &signal;
            codex(&search_root).executable
        })
        .unwrap();
    assert!(ended(&updates, &told), "the install did not end in time");
    let installed = codex(&root);
    let entry = installed
        .executable
        .clone()
        .expect("codex after its install");
    assert!(entry.starts_with(&npm.prefix), "{}", entry.display());
    let last = updates.versions(&db, &installed, &roots, false).last;
    let last = last.expect("the install's record");
    assert!(last.ok, "{last:?}");
    let latest = last.to.clone().expect("the installed version");
    let package = npm.prefix.join("node_modules").join(PACKAGE);
    for (folder, private_too) in [
        (npm.prefix.clone(), true),
        (npm.prefix.join("node_modules"), true),
        (package.clone(), false),
    ] {
        owned(&folder, private_too).unwrap();
    }

    let checked = updates.versions(&db, &installed, &roots, true);
    assert_eq!(
        checked.installed.as_deref(),
        Some(latest.as_str()),
        "{checked:?}"
    );
    assert_eq!(checked.refusal, None, "{checked:?}");
    assert!(checked.command.is_some(), "{checked:?}");

    let mut pin = Command::new(&npm.program);
    pin.args(["install", "--global", "--prefix"])
        .arg(&npm.prefix)
        .arg(format!("{PACKAGE}@{OLDER}"));
    let search = npm.search.iter().cloned();
    let path = std::env::var_os("PATH").unwrap_or_default();
    pin.env(
        "PATH",
        std::env::join_paths(search.chain(std::env::split_paths(&path))).unwrap(),
    );
    let pinned = kernel_processes::spawn_gate::collected(&mut pin).unwrap();
    assert!(
        pinned.status.success(),
        "{}",
        String::from_utf8_lossy(&pinned.stderr)
    );
    let older = codex(&root);
    let read = updates.versions(&db, &older, &roots, false);
    assert_eq!(read.installed.as_deref(), Some(OLDER), "{read:?}");
    assert!(
        harnesses::run_min("codex").is_none_or(|minimum| !update::below(OLDER, minimum)),
        "{OLDER} is below the oldest codex CrazyAgent drives"
    );

    let (tell, told) = mpsc::channel();
    let signal = Ended(tell);
    let (said, heard) = mpsc::channel();
    updates
        .start(&db, &older, &roots, move |updated| {
            let _held = &signal;
            let _ = said.send(updated);
        })
        .unwrap();
    assert!(ended(&updates, &told), "the update did not end in time");
    assert_eq!(heard.try_recv(), Ok(true), "the update did not end well");
    let after = codex(&root);
    let last = updates.versions(&db, &after, &roots, false).last;
    let last = last.expect("the update's record");
    assert!(last.ok, "{last:?}");
    assert_eq!(last.from.as_deref(), Some(OLDER), "{last:?}");
    assert!(last.to.as_deref().is_some_and(|to| to != OLDER), "{last:?}");
    for folder in [npm.prefix.clone(), npm.prefix.join("node_modules")] {
        owned(&folder, true).unwrap();
    }
    owned(&package, false).unwrap();
}
