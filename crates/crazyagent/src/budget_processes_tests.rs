use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::paths::Platform;
use crate::service::Service;
use crate::terminal::buffer::Chunk;
use crate::terminal::tmux::{Tmux, installed};
use crate::terminal::{AgentId, Host, TerminalStream, type_into};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use kernel_processes::spawn_gate;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const PTY_LINE: &[u8] = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r";
const ARCHIVE_BYTES: usize = 1 << 20;

type Call<'a> = (&'static str, usize, Box<dyn FnMut() + 'a>);

/// Times each process call on this machine and prints its p50 and p95 in microseconds.
/// Inputs: none (a fresh temp folder, a private tmux socket, a key pair made in setup).
/// Output: one `budget` line per call on stdout; it asserts every sample was taken.
#[test]
#[ignore = "budget row: run with --ignored --nocapture on a quiet and a loaded Mac"]
fn processes_budget_rows() {
    let home = tempfile::tempdir().unwrap();
    let fixtures = Fixtures::new(home.path());
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
}

fn time(run: &mut dyn FnMut()) -> u128 {
    let started = Instant::now();
    run();
    started.elapsed().as_micros()
}

struct Fixtures {
    home: PathBuf,
    tmux: PathBuf,
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
}

impl Fixtures {
    fn new(home: &Path) -> Self {
        let folder = home.join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = installed(home).expect("tmux is required for the budget row");
        let host = Tmux::open(Some(tmux.clone()), &folder);
        let cat = |agent: &AgentId| {
            let command = InteractiveCommand {
                argv: vec!["/bin/cat".into()],
                cwd: home.to_path_buf(),
                env: Vec::new(),
                transcript: Transcript::Unavailable,
            };
            host.spawn(agent, &crate::terminal::PaneCommand::from(&command))
                .unwrap();
        };
        let echo = AgentId::team(1).unwrap();
        #[cfg(target_os = "macos")]
        let shown = AgentId::team(2).unwrap();
        cat(&echo);
        #[cfg(target_os = "macos")]
        cat(&shown);
        let stream = std::cell::RefCell::new(host.attach(&echo).unwrap());
        let (archive, sha256, signature, key) = signed_archive(home);
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
            tmux,
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
        };
        fixtures.pty_echo();
        fixtures
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

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.host.socket())
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
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

fn signed_archive(home: &Path) -> (PathBuf, String, String, String) {
    let bytes: Vec<u8> = (0..ARCHIVE_BYTES).map(|at| (at % 251) as u8).collect();
    let archive = home.join("crazyagent.tar.gz");
    std::fs::write(&archive, &bytes).unwrap();
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
    (
        archive,
        sha256,
        STANDARD.encode(signature.into_string()),
        key,
    )
}
