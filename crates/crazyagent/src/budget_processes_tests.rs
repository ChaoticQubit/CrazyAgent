use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::tmux::installed;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use kernel_common::Platform;
use kernel_processes::service::Service;
use kernel_processes::spawn_gate;
use kernel_processes::terminal::buffer::Chunk;
use kernel_processes::terminal::tmux::{SOCKET, Tmux};
use kernel_processes::terminal::{AgentId, Host, TerminalStream, type_into};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The budget row's terminal folder: a fixed name in the user's private temp root, so the
/// next run finds the tmux server a hard-stopped run left on its socket.
const TERMINAL_FOLDER: &str = "ca-budget-row";

/// The folder the stale-server row plants its server in, apart from the budget row's.
const STALE_FOLDER: &str = "ca-budget-stale";

/// How long a reaped server and its panes may take to exit.
const REAP_WAIT: Duration = Duration::from_secs(10);

const PTY_LINE: &[u8] = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r";
const ARCHIVE_BYTES: usize = 1 << 20;

type Call<'a> = (&'static str, usize, Box<dyn FnMut() + 'a>);
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Names the path a filesystem step failed on. Inputs: the path and the step's result.
/// Output: the value, or an error naming the path.
fn at<T>(path: &Path, result: std::io::Result<T>) -> TestResult<T> {
    result.map_err(|error| format!("{path:?}: {error}").into())
}

/// Times each process call on this machine and prints its p50 and p95 in microseconds.
/// Inputs: none (a fresh temp folder, a private tmux socket, a key pair made in setup).
/// Output: one `budget` line per call on stdout; it asserts every sample was taken.
#[test]
#[ignore = "budget row: run with --ignored --nocapture on a quiet and a loaded Mac"]
fn processes_budget_rows() -> TestResult {
    let home = tempfile::tempdir()?;
    let fixtures = Fixtures::new(home.path())?;
    let calls = fixtures.calls();
    assert!(calls.len() >= 6);
    for (call, runs, mut run) in calls {
        let mut micros: Vec<u128> = (0..runs).map(|_| time(&mut run)).collect();
        assert_eq!(micros.len(), runs);
        micros.sort_unstable();
        println!(
            "budget {call} n={runs} p50_us={} p95_us={}",
            micros[runs / 2],
            micros[runs * 95 / 100]
        );
    }
    Ok(())
}

fn time(run: &mut dyn FnMut()) -> u128 {
    let started = Instant::now();
    run();
    started.elapsed().as_micros()
}

struct Fixtures {
    home: PathBuf,
    host: Tmux,
    echo: AgentId,
    #[cfg(target_os = "macos")]
    shown: AgentId,
    #[cfg(target_os = "macos")]
    typed: std::cell::Cell<u64>,
    stream: std::cell::RefCell<TerminalStream>,
    archive: PathBuf,
    sha256: String,
    signature: String,
    key: String,
    service: Service,
    _terminal: TerminalFolder,
}

impl Fixtures {
    fn new(home: &Path) -> TestResult<Self> {
        let tmux = tmux_for(home);
        let folder = crate::harnesses::finding::private_root().join(TERMINAL_FOLDER);
        let terminal = TerminalFolder::new(&tmux, folder)?;
        let host = Tmux::open(
            Some(tmux),
            &terminal.folder,
            kernel_sandbox::confinement(),
            crate::terminal::tmux::tmux_hooks(),
        );
        let cat = |agent: &AgentId| {
            let command = InteractiveCommand {
                argv: vec!["/bin/cat".into()],
                cwd: home.to_path_buf(),
                env: Vec::new(),
                transcript: Transcript::Unavailable,
            };
            host.spawn(
                agent,
                &kernel_processes::terminal::PaneCommand::from(&command),
            )
        };
        let echo = AgentId::team(1).unwrap();
        #[cfg(target_os = "macos")]
        let shown = AgentId::team(2).unwrap();
        cat(&echo)?;
        #[cfg(target_os = "macos")]
        cat(&shown)?;
        let stream = std::cell::RefCell::new(host.attach(&echo)?);
        let (archive, sha256, signature, key) = signed_archive(home)?;
        let service = Service {
            platform: Platform::CURRENT,
            name: format!("dev.crazyagent.engine.budget-{}", std::process::id()),
            executable: home.join("crazyagent"),
            root: home.join("engine"),
            path: "/usr/bin:/bin".into(),
            home: Some(home.to_path_buf()),
            definition: home.join("budget.plist"),
            user: None,
        };
        let fixtures = Self {
            home: home.to_path_buf(),
            host,
            echo,
            #[cfg(target_os = "macos")]
            shown,
            #[cfg(target_os = "macos")]
            typed: std::cell::Cell::new(0),
            stream,
            archive,
            sha256,
            signature,
            key,
            service,
            _terminal: terminal,
        };
        fixtures.pty_echo();
        Ok(fixtures)
    }

    fn calls(&self) -> Vec<Call<'_>> {
        let mut calls: Vec<Call<'_>> = vec![
            ("spawn_true", 500, Box::new(spawn_true)),
            ("output_echo", 500, Box::new(output_echo)),
            (
                "unix_bind_connect",
                2_000,
                Box::new(|| self.unix_bind_connect()),
            ),
        ];
        #[cfg(target_os = "macos")]
        calls.push(("tmux_send_round_trip", 200, Box::new(|| self.tmux_send())));
        calls.push(("pty_echo", 500, Box::new(|| self.pty_echo())));
        calls.push(("service_status", 200, Box::new(|| self.service_status())));
        calls.push(("update_verify", 200, Box::new(|| self.update_verify())));
        calls
    }

    fn unix_bind_connect(&self) {
        let socket = self.home.join("budget.sock");
        let listener = spawn_gate::bind_unix_std(&socket).unwrap();
        let stream = spawn_gate::connect_unix_std(&socket).unwrap();
        drop(stream);
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn tmux_send(&self) {
        self.typed.set(self.typed.get() + 1);
        let marker = format!("budget-{}", self.typed.get());
        let line = format!("{marker}\r");
        self.host.input(&self.shown, line.as_bytes()).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let screen = self.host.screen(&self.shown).unwrap();
            if String::from_utf8_lossy(&screen).contains(&marker) {
                return;
            }
            assert!(Instant::now() < deadline, "tmux never showed {marker}");
        }
    }

    fn pty_echo(&self) {
        type_into(&self.host, None, &self.echo, PTY_LINE).unwrap();
        let wanted = 2 * (PTY_LINE.len() - 1);
        let mut seen = 0;
        let mut stream = self.stream.borrow_mut();
        while seen < wanted {
            match stream.next_blocking().expect("the pane's stream ended") {
                Chunk::Output(bytes) => seen += bytes.iter().filter(|byte| **byte == b'x').count(),
                Chunk::Behind => panic!("the budget pane's stream fell behind"),
            }
        }
        assert_eq!(seen, wanted);
    }

    fn service_status(&self) {
        let status = self.service.status().unwrap();
        assert!(!status.installed && !status.running);
    }

    fn update_verify(&self) {
        kernel_processes::update::verify_file(
            &self.archive,
            &self.sha256,
            &self.signature,
            &self.key,
            "0.2.0",
        )
        .unwrap();
    }
}

/// A row's terminal folder at a fixed name. Making it ends the server an earlier run left
/// there; dropping it ends this run's server and removes the folder.
struct TerminalFolder {
    tmux: PathBuf,
    folder: PathBuf,
}

impl TerminalFolder {
    /// Sets the folder up fresh ([`fresh_terminal`]). Inputs: the tmux program and the
    /// folder. Output: the guard, or the folder step that failed.
    fn new(tmux: &Path, folder: PathBuf) -> TestResult<Self> {
        fresh_terminal(tmux, &folder)?;
        Ok(Self {
            tmux: tmux.to_path_buf(),
            folder,
        })
    }
}

impl Drop for TerminalFolder {
    fn drop(&mut self) {
        end_server(&self.tmux, &self.folder.join(SOCKET));
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

/// Ends the tmux server on a socket, by that socket only, when one answers there.
/// Inputs: the tmux program and the socket. Output: none.
fn end_server(tmux: &Path, socket: &Path) {
    let _ = spawn_gate::status(
        Command::new(tmux)
            .arg("-S")
            .arg(socket)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

/// The tmux the rows time and plant with. Input: the engine root the resolver reads.
/// Output: the tmux program.
fn tmux_for(home: &Path) -> PathBuf {
    installed(home).expect("tmux is required for the budget row")
}

/// Makes a row's terminal folder again, private and empty: when an earlier run left it, the
/// tmux server on its socket is ended first, its holder and panes with it, and the folder is
/// removed, which also ends a holder whose server did not answer. Inputs: the tmux program
/// and the folder. Output: none, or the folder step that failed.
fn fresh_terminal(tmux: &Path, folder: &Path) -> TestResult {
    assert!(tmux.is_absolute());
    if std::fs::symlink_metadata(folder).is_ok() {
        end_server(tmux, &folder.join(SOCKET));
        at(folder, std::fs::remove_dir_all(folder))?;
    }
    at(folder, std::fs::create_dir(folder))?;
    at(
        folder,
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)),
    )
}

/// The pids of the tmux server on a socket and of every pane it runs, the holder's
/// included. Inputs: the tmux program and the socket. Output: the pids, none when no
/// server answers there.
fn server_pids(tmux: &Path, socket: &Path) -> TestResult<Vec<i32>> {
    let mut pids = Vec::new();
    for asked in [
        ["display-message", "-p", "#{pid}"].as_slice(),
        ["list-panes", "-a", "-F", "#{pane_pid}"].as_slice(),
    ] {
        let output = spawn_gate::collected(
            Command::new(tmux)
                .arg("-S")
                .arg(socket)
                .args(asked)
                .env_clear(),
        )
        .map_err(|error| format!("{} at {}: {error}", tmux.display(), socket.display()))?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        pids.extend(
            text.lines()
                .filter_map(|line| line.trim().parse::<i32>().ok()),
        );
    }
    Ok(pids)
}

/// Whether a process with this pid still runs. Input: the pid. Output: the answer.
fn alive(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

/// A tmux server a hard-stopped run left on the row's socket, with its holder and a `cat`
/// pane, is gone once the next run sets its terminal folder up, and the folder is made again
/// empty and private. It plants in its own folder, so it never ends the budget row's server.
#[test]
#[ignore = "budget row: run with --ignored --nocapture on a quiet and a loaded Mac"]
fn a_server_an_earlier_run_left_is_gone_once_the_next_run_sets_up() -> TestResult {
    let home = tempfile::tempdir()?;
    let tmux = tmux_for(home.path());
    let terminal = TerminalFolder::new(
        &tmux,
        crate::harnesses::finding::private_root().join(STALE_FOLDER),
    )?;
    let folder = terminal.folder.clone();
    let planted = Tmux::open(
        Some(tmux.clone()),
        &folder,
        kernel_sandbox::confinement(),
        crate::terminal::tmux::tmux_hooks(),
    );
    let command = InteractiveCommand {
        argv: vec!["/bin/cat".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: Transcript::Unavailable,
    };
    let agent = AgentId::team(1).unwrap();
    planted
        .spawn(
            &agent,
            &kernel_processes::terminal::PaneCommand::from(&command),
        )
        .map_err(|error| {
            format!(
                "tmux at {} spawns {agent:?}: {error}",
                planted.socket().display()
            )
        })?;
    let socket = planted.socket();
    drop(planted);
    let pids = server_pids(&tmux, &socket)?;
    assert!(pids.len() >= 3, "server, holder and cat: {pids:?}");
    assert!(pids.iter().all(|pid| alive(*pid)), "{pids:?}");

    fresh_terminal(&tmux, &folder)?;
    let gone = crate::poll_support::poll_until(Instant::now() + REAP_WAIT, || {
        pids.iter().all(|pid| !alive(*pid))
    });
    let left: Vec<i32> = pids.iter().copied().filter(|pid| alive(*pid)).collect();
    assert!(gone, "still running after setup: {left:?}");
    let entries = at(&folder, std::fs::read_dir(&folder))?.count();
    assert_eq!(entries, 0, "{}", folder.display());
    let mode = at(&folder, std::fs::symlink_metadata(&folder))?
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    Ok(())
}

fn spawn_true() {
    let mut child = spawn_gate::spawn(
        Command::new("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
}

fn output_echo() {
    let output = spawn_gate::output(
        Command::new("/bin/echo")
            .arg("budget")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(output.status.success());
}

fn signed_archive(home: &Path) -> TestResult<(PathBuf, String, String, String)> {
    let bytes: Vec<u8> = (0..ARCHIVE_BYTES).map(|at| (at % 251) as u8).collect();
    let archive = home.join("crazyagent.tar.gz");
    at(&archive, std::fs::write(&archive, &bytes))?;
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        std::io::Cursor::new(&bytes),
        Some("timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:0.2.0"),
        None,
    )
    .unwrap();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    Ok((
        archive,
        sha256,
        STANDARD.encode(signature.into_string()),
        key,
    ))
}
