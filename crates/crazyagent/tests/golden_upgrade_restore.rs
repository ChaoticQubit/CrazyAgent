#![cfg(target_os = "macos")]

mod e2e_support;
mod golden_support;
mod keychain_support;

use crazyagent::paths::Layout;
use crazyagent::restore::{ENGINE_RESTORED, RestoreState};
use crazyagent::service::{LAUNCHD_LABEL, answering};
use crazyagent::terminal::tmux::installed;
use e2e_support::{TMUX, scratch, stop_tmux};
use golden_support::{Record, wait_until};
use keychain_support::TemporaryKeychain;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const FLOW: &str = "upgrade-restore";
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v242-engine.db");
const NO_FIXTURE: &str =
    "no upgrade fixture yet; write it with SNAPSHOT_V242_WRITE=1 on the 2.4.2 release commit";
const BIN: &str = env!("CARGO_BIN_EXE_crazyagent");
const BARE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const LAUNCHCTL: &str = "/bin/launchctl";
const SESSION: &str = "golden";
const COLUMNS: &str = "120";
const ROWS: &str = "40";
const SHIFT_LEFT: [&str; 6] = ["1b", "5b", "31", "3b", "32", "44"];
const UPGRADE_TIME: Duration = Duration::from_secs(240);
const RESTORE_TIME: Duration = Duration::from_secs(180);
const SCREEN_TIME: Duration = Duration::from_secs(30);
const TABLES: &str =
    "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'";
const PINNED_VERSION: i64 = 11;
const FIRST_RUN_STEPS_MAX: usize = 12;
const STEPS: [&str; 5] = [
    "seed the 2.4.2 data",
    "the first start upgrades it",
    "open Restore 2.4",
    "bring back the 2.4 data",
    "find the 2.4 data back",
];

/// The scratch places a run uses in place of the owner's: a HOME whose default keychain is a
/// temporary one, and an engine root in a scratch folder that is not that HOME's default
/// root, so the login service the TUI derives from it carries the root's hash.
struct Places {
    home: PathBuf,
    root: PathBuf,
}

impl Places {
    /// Gives a command only the scratch environment: HOME, the macOS fixed user home and the
    /// engine root, the bare PATH and a locale, nothing of the runner's own.
    /// Input: the command. Output: none.
    fn apply(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("CFFIXED_USER_HOME", &self.home)
            .env("CRAZYAGENT_HOME", &self.root)
            .env("PATH", BARE_PATH)
            .env("LANG", "en_US.UTF-8");
    }

    /// Where the login service definition of `label` would sit under the scratch HOME.
    /// Input: the label.
    /// Output: the path.
    fn definition(&self, label: &str) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    }
}

/// What a run leaves behind when it stops early: the scratch root's detached engine and the
/// scratch label's login service and definition. Dropped, it stops that engine and removes
/// that label, never another.
struct Leftovers {
    root: PathBuf,
    label: String,
    definition: PathBuf,
}

impl Drop for Leftovers {
    /// Stops the scratch root's engine processes, boots the scratch label out when launchd
    /// holds it and removes its definition.
    /// Input: none. Output: none.
    fn drop(&mut self) {
        for pid in engine_pids(&self.root) {
            if let Some(pid) = rustix::process::Pid::from_raw(pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
        if launchd_holds(&self.label) {
            let target = format!("gui/{}/{}", uid(), self.label);
            let _ = run(Command::new(LAUNCHCTL).args(["bootout", &target]));
        }
        let _ = fs::remove_file(&self.definition);
    }
}

/// The real `crazyagent tui` in a pane of its own tmux server, started with only the scratch
/// environment and no socket, so it finds its default engine and shows Restore 2.4.
/// Fields: tmux, the server's socket, the scratch HOME and the folder holding the socket.
struct Tui {
    tmux: PathBuf,
    socket: PathBuf,
    home: PathBuf,
    _folder: tempfile::TempDir,
}

impl Tui {
    /// Starts the TUI on the scratch places.
    /// Inputs: tmux and the places.
    /// Output: the TUI, which starts the scratch root's engine as its first act.
    fn start(tmux: PathBuf, places: &Places) -> Self {
        let parts = [BIN, &text(&places.home), &text(&places.root)];
        assert!(parts.iter().all(|part| !part.contains('\'')));
        let folder = scratch("ca-golden-pty-");
        let tui = Self {
            tmux,
            socket: folder.path().join("pty.sock"),
            home: places.home.clone(),
            _folder: folder,
        };
        let command = format!(
            "/usr/bin/env -i \"TERM=$TERM\" 'HOME={home}' 'CFFIXED_USER_HOME={home}' \
             'CRAZYAGENT_HOME={root}' 'PATH={BARE_PATH}' 'LANG=en_US.UTF-8' '{BIN}' tui",
            home = parts[1],
            root = parts[2],
        );
        tui.tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            COLUMNS,
            "-y",
            ROWS,
            &command,
        ]);
        tui
    }

    /// Runs one tmux command on this TUI's server with the scratch HOME and the bare PATH.
    /// Input: the arguments.
    /// Output: what tmux printed; a failing tmux fails the flow.
    fn tmux(&self, args: &[&str]) -> Output {
        let mut command = Command::new(&self.tmux);
        command
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", BARE_PATH);
        let output = run(&mut command).expect(TMUX);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// Presses keys in the TUI's pane by their tmux names.
    /// Input: the key names.
    /// Output: none.
    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION];
        args.extend_from_slice(keys);
        self.tmux(&args);
    }

    /// Sends raw bytes to the TUI's pane, such as a key tmux has no name for.
    /// Input: the bytes, in hex.
    /// Output: none.
    fn bytes(&self, hex: &[&str]) {
        let mut args = vec!["send-keys", "-t", SESSION, "-H"];
        args.extend_from_slice(hex);
        self.tmux(&args);
    }

    /// The text the TUI's pane shows now.
    /// Input: none.
    /// Output: the screen.
    fn screen(&self) -> String {
        let shown = self.tmux(&["capture-pane", "-p", "-t", SESSION]);
        String::from_utf8_lossy(&shown.stdout).into_owned()
    }

    /// Brings the TUI to Home as a first run does: skips each first-run step with `c` until
    /// Home shows, then waits for Home's side panel to settle on its tools check, after which
    /// only keys move the focus.
    /// Input: none. Output: none; a TUI that never reaches Home fails the flow.
    async fn home(&self) {
        for _ in 0..FIRST_RUN_STEPS_MAX {
            let screen = self
                .until("Home or a first-run step", |now| {
                    now.contains("New project") || now.contains("c Skip for now")
                })
                .await;
            if screen.contains("New project") {
                self.until("Home's side panel to settle on its tools check", settled)
                    .await;
                return;
            }
            assert!(
                !screen.contains("Welcome to CrazyAgent"),
                "the fixture's saved profile skips Welcome:\n{screen}"
            );
            self.keys(&["c"]);
            self.until("the first-run step to move on", |now| now != screen)
                .await;
        }
        panic!("the TUI never reached Home:\n{}", self.screen());
    }

    /// Waits until the screen passes a check, reading it as `wait_until` does.
    /// Inputs: what is awaited and the check.
    /// Output: the screen that passed.
    async fn until(&self, what: &str, check: impl Fn(&str) -> bool) -> String {
        wait_until(what, SCREEN_TIME, async || {
            let screen = self.screen();
            check(&screen).then_some(screen)
        })
        .await
    }
}

impl Drop for Tui {
    /// Ends the TUI's tmux server and the TUI in it, so a failed run leaves no TUI.
    /// Input: none.
    /// Output: none.
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.socket);
    }
}

/// Whether Home's side panel settled on its tools check, as the TUI e2e rows read it: "Get
/// set up" shows the check's words, "Learn CrazyAgent" offers Add an account.
/// Input: the screen. Output: the answer.
fn settled(screen: &str) -> bool {
    let checked = screen.contains("[x] Install tools") || screen.contains("Missing: ");
    (screen.contains("Get set up") && checked)
        || (screen.contains("Learn CrazyAgent") && screen.contains("Add an account"))
}

/// Runs a command with no input and its output kept.
/// Input: the command.
/// Output: its output.
fn run(command: &mut Command) -> std::io::Result<Output> {
    crazyagent::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
}

/// A path as text, as tmux and the service status print it.
/// Input: the path.
/// Output: its text; a path that is not text fails the flow.
fn text(path: &Path) -> String {
    path.to_str().expect("a scratch path is text").to_owned()
}

/// This user's id, as launchd's GUI domain names it.
/// Input: none.
/// Output: the id.
fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Whether launchd holds a job with this label in this user's GUI domain, read with
/// `launchctl print` only.
/// Input: the label. Output: the answer.
fn launchd_holds(label: &str) -> bool {
    let target = format!("gui/{}/{label}", uid());
    run(Command::new(LAUNCHCTL).args(["print", &target])).is_ok_and(|out| out.status.success())
}

/// The owner's own login service as launchd reports it, read only through `launchctl print`
/// and never through `crazyagent`.
/// Input: none. Output: whether it is loaded and its pid.
fn owner_engine() -> (bool, Option<String>) {
    let target = format!("gui/{}/{LAUNCHD_LABEL}", uid());
    let Ok(printed) = run(Command::new(LAUNCHCTL).args(["print", &target])) else {
        return (false, None);
    };
    if !printed.status.success() {
        return (false, None);
    }
    let shown = String::from_utf8_lossy(&printed.stdout).into_owned();
    let pid = shown
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = "))
        .map(str::to_owned);
    (true, pid)
}

/// The pids of engine processes running on the scratch root, found by their arguments.
/// Input: the root.
/// Output: the pids.
fn engine_pids(root: &Path) -> Vec<i32> {
    let pattern = format!("engine --home {}", text(root));
    let Ok(found) = run(Command::new("/usr/bin/pgrep").args(["-f", &pattern])) else {
        return Vec::new();
    };
    let listed = String::from_utf8_lossy(&found.stdout).into_owned();
    listed
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Reads the login service `crazyagent service status` derives for the scratch places and
/// refuses the run unless it is a scratch one: the owner's label with the root's hash, its
/// definition under the scratch HOME and not installed, nothing running, the scratch root as
/// its data folder. Input: the places. Output: the label.
fn scratch_label(places: &Places) -> String {
    let mut command = Command::new(BIN);
    places.apply(&mut command);
    let output = run(command.args(["service", "status"])).unwrap();
    let shown = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "service status: {shown}");
    let line = |name: &str| {
        let found = shown.lines().find_map(|line| line.strip_prefix(name));
        found.unwrap_or_default().to_owned()
    };
    let label = line("service: ");
    let hash = label
        .strip_prefix(&format!("{LAUNCHD_LABEL}."))
        .unwrap_or_default();
    let hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    assert!(
        hash.len() == 12 && hash.bytes().all(hex),
        "the run refuses a login service that is not a scratch one: {label:?}"
    );
    let definition = format!("{} (not installed)", places.definition(&label).display());
    assert_eq!(line("definition: "), definition, "{shown}");
    assert_eq!(line("running: "), "no", "{shown}");
    assert_eq!(line("data folder: "), text(&places.root), "{shown}");
    label
}

/// Opens a database to read only.
/// Input: its path.
/// Output: the connection.
fn read_only(db: &Path) -> Connection {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(db, flags).unwrap()
}

/// Each table's row count, by name.
/// Input: the database.
/// Output: the counts.
fn counts(db: &Path) -> BTreeMap<String, i64> {
    let connection = read_only(db);
    let names: Vec<String> = connection
        .prepare(TABLES)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!names.is_empty(), "{}", db.display());
    names
        .into_iter()
        .map(|name| {
            let quoted = name.replace('"', "\"\"");
            let query = format!("SELECT COUNT(*) FROM \"{quoted}\"");
            let count = connection.query_row(&query, [], |row| row.get(0)).unwrap();
            (name, count)
        })
        .collect()
}

/// The database's version of record.
/// Input: the database.
/// Output: `PRAGMA user_version`.
fn version(db: &Path) -> i64 {
    read_only(db)
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

/// The bytes of the regular files under a folder, links not followed.
/// Input: the folder.
/// Output: the sum.
fn bytes(folder: &Path) -> u64 {
    let mut total = 0;
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            total += bytes(&path);
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_brings_back_the_2_4_data_from_the_tui_after_the_first_2_5_start() {
    let fixture = Path::new(FIXTURE);
    assert!(fixture.is_file(), "{NO_FIXTURE}");
    let mut record = Record::new(FLOW, "tui");
    let keychain = TemporaryKeychain::create();
    let place = scratch("ca-golden-up-");
    let root = place.path().join("engine");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let places = Places {
        home: keychain.home().to_path_buf(),
        root: root.clone(),
    };
    assert!(
        place.path().starts_with("/private/tmp") && root.starts_with(place.path()),
        "the run refuses an engine root outside its scratch folder: {}",
        root.display()
    );
    let label = scratch_label(&places);
    let owner = owner_engine();
    let _leftovers = Leftovers {
        root: root.clone(),
        label: label.clone(),
        definition: places.definition(&label),
    };
    let fixture_copy = place.path().join("fixture-copy.db");
    fs::copy(fixture, &fixture_copy).unwrap();
    let fixture_counts = counts(&fixture_copy);
    let db = root.join("engine.db");
    fs::copy(fixture, &db).unwrap();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
    let seeded = bytes(&root);
    record.step(STEPS[0]);

    let socket = Layout::new(root.clone()).unwrap().socket();
    let tmux = installed(&root).expect(TMUX);
    let started = Instant::now();
    let tui = Tui::start(tmux, &places);
    wait_until(
        "the first start to upgrade the 2.4.2 data",
        UPGRADE_TIME,
        async || {
            let pinned = crazyagent::restore::restorable_until(&root).is_some();
            (answering(&socket) && pinned).then_some(())
        },
    )
    .await;
    let upgrade_ms = u64::try_from(started.elapsed().as_millis()).unwrap();
    let grown = bytes(&root).saturating_sub(seeded);
    record.measure("upgrade_start_ms", upgrade_ms);
    record.measure("disk_growth_bytes", grown);
    tui.home().await;
    record.step(STEPS[1]);

    tui.keys(&["s"]);
    tui.until("Settings on Accounts", |screen| {
        screen.contains("Settings  /  Accounts")
    })
    .await;
    tui.bytes(&SHIFT_LEFT);
    tui.until("Restore 2.4 with its button", |screen| {
        screen.contains("You can bring back your 2.4 data until")
            && screen.contains("Bring back 2.4 data")
    })
    .await;
    record.step(STEPS[2]);

    tui.keys(&["Enter"]);
    tui.until("the restore question", |screen| {
        screen.contains("Bring back your 2.4 data?")
    })
    .await;
    tui.keys(&["Right"]);
    tui.keys(&["Enter"]);
    let copy =
        wait_until(
            "the restore to end",
            RESTORE_TIME,
            async || match crazyagent::restore::state(&root) {
                Some(RestoreState::Restored { copy }) => Some(copy),
                Some(RestoreState::Failed { code }) => panic!("the restore failed: {code}"),
                _ => None,
            },
        )
        .await;
    wait_until("the engine to stay stopped", SCREEN_TIME, async || {
        (!answering(&socket) && engine_pids(&root).is_empty()).then_some(())
    })
    .await;
    record.step(STEPS[3]);

    assert!(copy.starts_with("replaced-"), "{copy}");
    let kept = root.join("backups").join(&copy);
    let mode = fs::symlink_metadata(&kept).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "the dated copy of the 2.5 data is private");
    let kept_version = version(&kept.join("engine.db"));
    assert!(
        kept_version > PINNED_VERSION,
        "the dated copy holds the 2.5 data: {kept_version}"
    );
    assert_eq!(
        version(&db),
        PINNED_VERSION,
        "engine.db is the 2.4 data again"
    );
    assert_eq!(counts(&db), fixture_counts, "every 2.4.2 row is back");
    assert_eq!(
        crazyagent::restore::blocks_start(&root, false),
        Some(ENGINE_RESTORED),
        "no client starts 2.5 on the restored data by itself"
    );
    assert_eq!(owner_engine(), owner, "the owner's engine is untouched");
    record.step(STEPS[4]);

    drop(tui);
    assert!(
        !launchd_holds(&label),
        "the run leaves no scratch login service"
    );
    assert!(!places.definition(&label).exists());
    let finished = record.finish();
    let steps: Option<Vec<&str>> = finished
        .as_ref()
        .and_then(|record| record["steps"].as_array())
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| step["step"].as_str())
                .collect()
        });
    let outcome = finished
        .as_ref()
        .and_then(|record| record["outcome"].as_str());
    assert_eq!(
        (steps, outcome),
        (Some(STEPS.to_vec()), Some("passed")),
        "the run record names each step in order"
    );
    let measure = |name: &str| {
        let measures = finished.as_ref().map(|record| &record["measures"]);
        measures.and_then(|measures| measures[name].as_u64())
    };
    assert!(
        measure("upgrade_start_ms").is_some_and(|ms| ms > 0)
            && measure("disk_growth_bytes").is_some_and(|grown| grown > 0),
        "the run record holds the upgrade start's time and disk growth: {finished:?}"
    );
}
