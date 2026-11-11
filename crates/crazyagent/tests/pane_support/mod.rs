#![allow(dead_code)]

use crazyagent::protocol::ProjectId;
use crazyagent::team::caller::{Callers, Whom, socket_for_mailbox};
use crazyagent::team::tool::MAILBOX_FOLDER;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The pane a lead and its in-process teammates run in, which names no teammate.
pub const LEAD_PANE: &str = "%1";
const RUN_WAIT: Duration = Duration::from_secs(60);

/// The engine's side of a native lead's caller socket for relay tests: the real `Callers`
/// bound where the lead's mailbox finds it and served on a thread, naming each process by
/// the pane process it runs under, as the engine names it by the panes tmux lists, and
/// answering `who` with the teammate the test registered for that pane, or the lead for
/// any other pane, as the engine answers from its rows. Each pane's process is one shell
/// that lives as long as the test, and every hook in that pane runs as its child, as
/// Claude Code's hooks run under the one Claude Code process of their pane, so the
/// engine's first-seen pane pins hold. A teammate's relay request goes to the engine's real
/// handler on the test's engine database, `engine.db` below the root, for the lead whose
/// live teammate row holds that pane, as the runtime serves that lead's socket; with no
/// database or row it is refused. The real tmux chain and the engine's own rows are proven
/// in `coord_hook_pane` and the runtime tests.
pub struct PaneCallers {
    panes: Arc<Mutex<Vec<(String, u32)>>>,
    shells: Mutex<Vec<(String, Child, ChildStdin)>>,
    runs: AtomicU32,
    folder: PathBuf,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
    socket: PathBuf,
}

/// One hook run under a pane's shell: its input, which the run waits for until it is
/// closed, and the folder its output, error output and exit status land in.
pub struct PaneRun {
    pub stdin: Option<PaneInput>,
    folder: PathBuf,
}

/// The input of one hook run. Bytes written go to the run's input file; dropping it lets
/// the run start.
pub struct PaneInput {
    file: File,
    ready: PathBuf,
}

impl Write for PaneInput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Drop for PaneInput {
    fn drop(&mut self) {
        self.file.flush().expect("the run's input");
        std::fs::write(&self.ready, b"").expect("the run's ready marker");
    }
}

impl PaneRun {
    /// Tells whether the run ended. Takes the run. Returns its exit status once its shell
    /// wrote it.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        Ok(std::fs::read_to_string(self.folder.join("rc"))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .map(|code| ExitStatus::from_raw(code << 8)))
    }

    /// Closes the input and waits up to `RUN_WAIT` for the run to end. Takes the run.
    /// Returns its status and its output, panicking when it never ends.
    pub fn wait_with_output(mut self) -> std::io::Result<Output> {
        drop(self.stdin.take());
        let deadline = Instant::now() + RUN_WAIT;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Output {
                    status,
                    stdout: std::fs::read(self.folder.join("out"))?,
                    stderr: std::fs::read(self.folder.join("err"))?,
                });
            }
            assert!(
                Instant::now() < deadline,
                "the run in {} never ended",
                self.folder.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Quotes one word for `/bin/sh`. Takes the word. Returns it in single quotes.
fn quoted(word: &std::ffi::OsStr) -> String {
    format!("'{}'", word.to_str().unwrap().replace('\'', "'\\''"))
}

impl PaneCallers {
    /// Makes a lead's mailbox in the engine's layout below `root`, private, and binds and
    /// serves its caller socket. Takes the root and the teammates as (pane, name). Returns
    /// the mailbox and the served socket.
    pub fn new(root: &Path, teammates: &[(&str, &str)]) -> (PathBuf, Self) {
        let mailbox = root.join("agent-1").join("tmp").join(MAILBOX_FOLDER);
        std::fs::create_dir_all(&mailbox).unwrap();
        std::fs::set_permissions(&mailbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        let folder = root.join("pane-runs");
        std::fs::create_dir(&folder).unwrap();
        let socket = socket_for_mailbox(&mailbox).unwrap();
        let short = socket.parent().unwrap();
        assert!(!short.exists(), "{}", short.display());
        std::fs::create_dir(short).unwrap();
        std::fs::set_permissions(short, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut callers = Callers::bind(&socket, None).unwrap();
        let panes: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let names: Vec<(String, String)> = teammates
            .iter()
            .map(|(pane, name)| ((*pane).to_owned(), (*name).to_owned()))
            .collect();
        let engine = root.join("engine.db");
        let serving = {
            let panes = Arc::clone(&panes);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    callers.serve(
                        &mut || panes.lock().unwrap().clone(),
                        &mut |pane, _| {
                            Some(
                                names
                                    .iter()
                                    .find(|(listed, _)| listed == pane)
                                    .map_or(Whom::Lead, |(_, name)| Whom::Teammate(name.clone())),
                            )
                        },
                        &mut |pane, _, request, out| relay(&engine, pane, request, out),
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        (
            mailbox,
            Self {
                panes,
                shells: Mutex::new(Vec::new()),
                runs: AtomicU32::new(0),
                folder,
                stop,
                serving: Some(serving),
                socket,
            },
        )
    }

    /// Starts a process as `pane`'s own process: it is listed there before the engine can
    /// answer it, as tmux lists a pane's process. Takes the pane and the command. Returns
    /// the child.
    pub fn spawn(&self, pane: &str, command: &mut Command) -> Child {
        let mut panes = self.panes.lock().unwrap();
        let child = crazyagent::spawn_gate::spawn(command).unwrap();
        panes.push((pane.to_owned(), child.id()));
        child
    }

    /// Runs a command under `pane`'s shell, which it starts as the pane's process on the
    /// pane's first run, with the command's environment changes, folder and arguments; its
    /// input, output and exit status go through files in its own run folder. Takes the pane
    /// and the command. Returns the run.
    pub fn run(&self, pane: &str, command: &Command) -> PaneRun {
        let number = self.runs.fetch_add(1, Ordering::Relaxed);
        let folder = self.folder.join(format!("run-{number}"));
        std::fs::create_dir(&folder).unwrap();
        let file = File::create(folder.join("in")).unwrap();
        let path = |name: &str| quoted(folder.join(name).as_os_str());
        let mut words = vec!["/usr/bin/env".to_owned()];
        for (key, value) in command.get_envs() {
            if value.is_none() {
                words.extend(["-u".to_owned(), quoted(key)]);
            }
        }
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                let mut pair = key.to_os_string();
                pair.push("=");
                pair.push(value);
                words.push(quoted(&pair));
            }
        }
        words.push(quoted(command.get_program()));
        words.extend(command.get_args().map(quoted));
        let enter = command
            .get_current_dir()
            .map(|dir| format!("cd {} && ", quoted(dir.as_os_str())))
            .unwrap_or_default();
        let line = format!(
            "(while [ ! -e {ready} ]; do sleep 0.01; done; {enter}{command} <{input} >{out} 2>{err}; echo $? >{rc}) &\n",
            ready = path("ready"),
            command = words.join(" "),
            input = path("in"),
            out = path("out"),
            err = path("err"),
            rc = path("rc"),
        );
        let mut shells = self.shells.lock().unwrap();
        if !shells.iter().any(|(held, ..)| held == pane) {
            let mut shell = self.spawn(
                pane,
                Command::new("/bin/sh")
                    .arg("-c")
                    .arg("while IFS= read -r line; do eval \"$line\"; done")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
            let input = shell.stdin.take().unwrap();
            shells.push((pane.to_owned(), shell, input));
        }
        let (.., input) = shells.iter_mut().find(|(held, ..)| held == pane).unwrap();
        input.write_all(line.as_bytes()).unwrap();
        input.flush().unwrap();
        PaneRun {
            stdin: Some(PaneInput {
                file,
                ready: folder.join("ready"),
            }),
            folder,
        }
    }
}

/// Answers one relay request with the engine's real handler on the test's engine database,
/// for the lead whose newest live teammate row holds the pane, or no when the database or
/// such a row is missing. Takes the database path, the pane, the request and the connection.
fn relay(engine: &Path, pane: &str, request: &[u8], out: &mut dyn Write) {
    let lead = Connection::open_with_flags(engine, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .ok()
        .and_then(|db| {
            let found: Option<(String, i64)> = db
                .query_row(
                    "SELECT project,parent FROM team_agents WHERE role='teammate' AND pane=?1
                     AND status IN ('starting','idle','working') ORDER BY id DESC LIMIT 1",
                    [pane],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .ok()?;
            Some((db, found?))
        });
    let Some((db, (project, lead))) = lead else {
        out.write_all(b"no\n").unwrap();
        return;
    };
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    crazyagent::coord::relay::answer(
        &db,
        crazyagent::coord::relay::ForLead {
            project: &ProjectId::new(&project).unwrap(),
            id: lead,
            starts: &mut crazyagent::coord::relay::TaskStarts::default(),
        },
        pane,
        request,
        out,
        now,
    )
    .unwrap();
}

impl Drop for PaneCallers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
        for (_, mut shell, input) in self.shells.lock().unwrap().drain(..) {
            drop(input);
            let _ = shell.kill();
            let _ = shell.wait();
        }
        let _ = std::fs::remove_file(&self.socket);
        if let Some(folder) = self.socket.parent() {
            let _ = std::fs::remove_dir(folder);
        }
    }
}
