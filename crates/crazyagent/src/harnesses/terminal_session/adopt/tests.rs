use super::*;
use crate::harnesses::interactive::InteractiveCommand;
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const TURN_OPEN: &str = r#"{"type":"user","message":{"content":"Fix the build"}}"#;
const TURN_DONE: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done"}],"stop_reason":"end_turn"}}"#;
const ESCAPE_HARNESS: &str = r#"stty raw -echo; while :; do c=$(dd bs=1 count=1 2>/dev/null); if [ "$c" = "$(printf '\033')" ]; then printf '%s\n' '{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}' >> "$1"; fi; done"#;

struct Fixture {
    root: tempfile::TempDir,
    tmux: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-adopt-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = installed(root.path()).expect("tmux must be installed on this machine");
        Self { root, tmux }
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(
            Some(self.tmux.clone()),
            &self.root.path().join(FOLDER),
        ))
    }

    fn transcript(&self, lines: &[&str]) -> PathBuf {
        let file = self.root.path().join("session.jsonl");
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(&file, text).unwrap();
        file
    }

    fn spawn(
        &self,
        host: &Arc<dyn Host>,
        agent: &AgentId,
        script: &str,
        file: &Path,
    ) -> PaneRecord {
        let command = InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                script.into(),
                "harness".into(),
                file.display().to_string(),
            ],
            cwd: self.root.path().canonicalize().unwrap(),
            env: Vec::new(),
            transcript: Transcript::ClaudeSession {
                file: file.to_path_buf(),
            },
        };
        let since = SystemTime::now();
        let pane = host.spawn(agent, &command).unwrap();
        PaneRecord::new(&command.transcript, &command.cwd, since, pane.pid, None).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.root.path().join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

fn lines(file: &Path) -> usize {
    std::fs::read_to_string(file).unwrap().lines().count()
}

#[test]
fn an_idle_pane_is_adopted_as_it_is() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = AgentId::team(1).unwrap();
    let file = fixture.transcript(&[TURN_OPEN, TURN_DONE]);
    let record = fixture.spawn(&host, &agent, "exec cat", &file);

    let session = TerminalSession::adopt(
        host.clone(),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .unwrap();
    assert_eq!(session.record(), Some(&record));
    assert_eq!(session.process_group(), record.pid);
    assert!(
        session.ready && !session.turn,
        "the adopted agent waits for its next prompt"
    );
    assert_eq!(lines(&file), 2, "nothing was typed into an idle agent");
    session.leave();
    assert_eq!(
        host.alive().unwrap(),
        std::slice::from_ref(&agent),
        "leaving keeps the pane running"
    );

    let session = TerminalSession::adopt(
        host.clone(),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .unwrap();
    drop(session);
    assert!(
        host.alive().unwrap().is_empty(),
        "dropping an adopted session ends its pane"
    );
}

#[test]
fn a_turn_still_running_is_stopped_before_the_pane_is_adopted() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = AgentId::team(2).unwrap();
    let file = fixture.transcript(&[TURN_OPEN, TURN_DONE, TURN_OPEN]);
    let record = fixture.spawn(&host, &agent, ESCAPE_HARNESS, &file);
    std::thread::sleep(Duration::from_millis(500));

    let session = TerminalSession::adopt(
        host.clone(),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .unwrap();
    assert!(!session.turn);
    assert_eq!(lines(&file), 4, "the harness got Escape and ended its turn");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("[Request interrupted by user]")
    );
    drop(session);
}

#[test]
fn a_turn_that_does_not_stop_leaves_no_pane_behind() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = AgentId::team(3).unwrap();
    let file = fixture.transcript(&[TURN_OPEN]);
    let record = fixture.spawn(&host, &agent, "exec cat", &file);

    let refused = TerminalSession::adopt(
        host.clone(),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .err()
    .map(|problem| problem.0);
    assert_eq!(refused.as_deref(), Some("launch_adopt_unsettled"));
    assert!(host.alive().unwrap().is_empty());
}

#[test]
fn a_pane_that_ended_cannot_be_adopted() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = AgentId::team(4).unwrap();
    let file = fixture.transcript(&[]);
    let record = fixture.spawn(&host, &agent, "exec cat", &file);
    host.kill(&agent).unwrap();
    let refused = TerminalSession::adopt(host, agent, &record, LaunchLimits::default())
        .err()
        .map(|problem| problem.0);
    assert_eq!(refused.as_deref(), Some("launch_closed"));
}

#[test]
fn pane_records_round_trip_and_refuse_unsafe_values() {
    let record = PaneRecord {
        source: RecordSource::CodexRollouts {
            sessions: PathBuf::from("/private/tmp/home/sessions"),
        },
        cwd: PathBuf::from("/private/tmp/project/worktrees/agent-3"),
        since_ms: 1_793_000_000_000,
        pid: 4242,
        key: Some(PathBuf::from("/private/tmp/home/.key")),
        resumed: None,
    };
    let text = record.encode().unwrap();
    assert_eq!(PaneRecord::decode(&text), Some(record.clone()));
    let mut relative = record.clone();
    relative.cwd = PathBuf::from("worktrees/agent-3");
    assert_eq!(PaneRecord::decode(&relative.encode().unwrap()), None);
    let mut relative_key = record.clone();
    relative_key.key = Some(PathBuf::from("home/.key"));
    assert_eq!(PaneRecord::decode(&relative_key.encode().unwrap()), None);
    let mut older = record.clone();
    older.key = None;
    let without_key = older.encode().unwrap().replace(",\"key\":null", "");
    assert!(!without_key.contains("key"));
    assert_eq!(PaneRecord::decode(&without_key), Some(older));
    assert!(
        !text.contains("resumed"),
        "a fresh pane's record stays as before"
    );
    let mut resumed = record.clone();
    resumed.resumed = Some((PathBuf::from("/private/tmp/home/sessions/r.jsonl"), 512));
    assert_eq!(
        PaneRecord::decode(&resumed.encode().unwrap()),
        Some(resumed.clone())
    );
    resumed.resumed = Some((PathBuf::from("sessions/r.jsonl"), 512));
    assert_eq!(PaneRecord::decode(&resumed.encode().unwrap()), None);
    let mut no_process = record.clone();
    no_process.pid = 0;
    assert_eq!(PaneRecord::decode(&no_process.encode().unwrap()), None);
    assert_eq!(PaneRecord::decode("{\"source\":1}"), None);
    let mut long = record;
    long.cwd = PathBuf::from(format!("/{}", "a".repeat(RECORD_BYTES_MAX)));
    assert_eq!(long.encode(), None);
    assert_eq!(
        PaneRecord::new(
            &Transcript::Unavailable,
            Path::new("/tmp"),
            SystemTime::now(),
            1,
            None
        ),
        None
    );
}

#[test]
fn an_adopted_pane_removes_its_key_file_when_it_stops_but_not_when_left() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let agent = AgentId::team(5).unwrap();
    let file = fixture.transcript(&[TURN_OPEN, TURN_DONE]);
    let key = fixture.root.path().join("key");
    std::fs::write(&key, "synthetic-test-key").unwrap();
    let mut record = fixture.spawn(&host, &agent, "exec cat", &file);
    record.key = Some(key.clone());

    let session = TerminalSession::adopt(
        host.clone(),
        agent.clone(),
        &record,
        LaunchLimits::default(),
    )
    .unwrap();
    session.leave();
    assert!(key.is_file(), "a pane left running keeps the key it reads");

    let session =
        TerminalSession::adopt(host.clone(), agent, &record, LaunchLimits::default()).unwrap();
    assert_eq!(session.stop(), None);
    assert!(
        !key.exists(),
        "stopping the adopted pane removes its key file"
    );
    assert!(host.alive().unwrap().is_empty());
}

/// Makes a real FIFO at `path` with the system `mkfifo`. Input: the path. Output: none.
fn fifo(path: &std::path::Path) {
    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(path)
            .env_clear()
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
}

/// Opens the FIFO at `path` for writing without waiting and closes it, so a reader a
/// failing test left blocked on it ends. Input: the path. Output: none.
fn release_fifo(path: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path);
}

/// A private folder under the canonical `/tmp` for one key file test.
fn key_folder() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::Builder::new()
        .prefix("ca-key-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

/// The longest key the engine writes: a relay token.
fn longest_key() -> String {
    format!(
        "{}{}",
        crate::model_gateway::relay::TOKEN_PREFIX,
        "a".repeat(64)
    )
}

/// A pane record with only its key path set to `key`.
fn keyed(key: &std::path::Path) -> PaneRecord {
    PaneRecord {
        source: RecordSource::CodexRollouts {
            sessions: PathBuf::from("/private/tmp/home/sessions"),
        },
        cwd: PathBuf::from("/private/tmp/project"),
        since_ms: 1_793_000_000_000,
        pid: 4242,
        key: Some(key.to_path_buf()),
        resumed: None,
    }
}

#[test]
fn an_adopted_key_file_longer_than_the_longest_key_is_not_taken() {
    let folder = key_folder();
    let key = folder.path().join(crate::harnesses::interactive::KEY_FILE);
    std::fs::write(&key, longest_key()).unwrap();
    assert!(
        keyed(&key)
            .key_file()
            .is_some_and(|taken| taken.path == key),
        "the longest key the engine writes was refused"
    );
    std::fs::File::create(&key)
        .unwrap()
        .set_len(16 << 20)
        .unwrap();
    let taken = keyed(&key).key_file();
    assert!(
        taken.is_none(),
        "adopt read a 16 MiB file at the key path whole"
    );
}

#[test]
fn an_adopted_key_file_that_is_a_fifo_is_not_taken_and_never_blocks_adopt() {
    let folder = key_folder();
    let key = folder.path().join(crate::harnesses::interactive::KEY_FILE);
    fifo(&key);
    let record = keyed(&key);
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let taken = record.key_file();
        let _ = sent.send(taken.is_some());
    });
    let taken = got.recv_timeout(Duration::from_secs(5));
    release_fifo(&key);
    assert_eq!(
        taken,
        Ok(false),
        "adopt blocked on or took a FIFO at the key path"
    );
}
