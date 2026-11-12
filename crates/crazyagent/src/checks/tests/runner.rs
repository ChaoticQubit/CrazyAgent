use crate::checks::runner::{self, CheckExecution, ReportFile, RunLimits};
use crate::checks::snapshot::{self, RunKey, Snapshot};
use crate::checks::{
    AdmittedCheck, CheckKind, CheckSpec, NetworkMode, ProfileId, ReportFormat, Trigger,
};
use crate::landing::{self, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace};
use crate::protocol::ProjectId;
use crate::sandbox::SandboxRemote;
use crate::sandbox::toolchains::Toolchain;
use crate::spawn_gate;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// One owner repository with one agent worktree added from it and recorded as the
/// engine records it, an isolated home for the test's own git, and an engine runtime
/// holding the change set store and the checks folder.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    engine: PathBuf,
    db: Connection,
    workspace: Workspace,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the home, the folder and the arguments. Output: the trimmed standard output.
fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn private(folder: &Path) {
    fs::create_dir_all(folder).unwrap();
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

fn owner_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

fn marker_script(path: &Path, marker: &Path) {
    write(path, &format!("#!/bin/sh\ntouch {}\ncat\n", quoted(marker)));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every file and link below a folder with its bytes or link target, so a test can
/// prove a folder byte-identical before and after a run. Input: the folder. Output: the
/// entries by path.
fn contents(folder: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                found.insert(path, target.into_os_string().into_encoded_bytes());
            } else if metadata.is_dir() {
                found.insert(path.clone(), Vec::new());
                pending.push(path);
            } else {
                found.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    found
}

impl Case {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("owner"), root.join("engine"));
        for folder in [&repo, &home, &engine, &engine.join("checks")] {
            private(folder);
        }
        git(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        write(&repo.join("README"), "start\n");
        git(&home, &repo, &["add", "README"]);
        git(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-1");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let db = landing::open(&engine).unwrap();
        let workspace = Workspace {
            project: ProjectId::new("project-1").unwrap(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "agent-1".into(),
                label: "Agent 1".into(),
            },
            repository: repo.clone(),
            worktree,
            git_dir: repo.join(".git/worktrees/agent-1"),
        };
        landing::record_worktree_start(
            &db,
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            repo,
            home,
            engine,
            db,
            workspace,
        }
    }

    /// Writes files into the agent worktree and records the turn end the engine records,
    /// which commits them into the worktree's open change set. Input: the files. Output:
    /// the change set.
    fn change_set(&self, files: &[(&str, &str)]) -> ChangeSetId {
        for (path, text) in files {
            write(&self.workspace.worktree.join(path), text);
        }
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Add the check fixture".into(),
        };
        landing::record_turn_end(&self.db, &self.workspace, &turn)
            .unwrap()
            .unwrap()
    }

    fn checks(&self) -> PathBuf {
        self.engine.join("checks")
    }

    fn snapshot(&self, id: &ChangeSetId, run: &str) -> Snapshot {
        snapshot::prepare(&self.db, &self.checks(), id, &RunKey::new(run).unwrap()).unwrap()
    }

    /// Moves the agent into its own clone of the owner repository (DEC-102 Own branch),
    /// made and recorded as the engine makes it, commits files there as the agent would and
    /// records the turn end, which fetches the agent branch. Input: the files. Output: the
    /// change set.
    fn own_branch_change_set(&mut self, files: &[(&str, &str)]) -> ChangeSetId {
        let worktree = self.root.join("agent-own");
        landing::add_agent_clone(&self.repo, &worktree).unwrap();
        self.workspace.git_dir = crate::agent_git::git_dir(&worktree);
        self.workspace.worktree = worktree;
        landing::record_worktree_start(
            &self.db,
            &self.workspace.repository,
            &self.workspace.worktree,
            &self.workspace.git_dir,
        )
        .unwrap();
        for (path, text) in files {
            write(&self.workspace.worktree.join(path), text);
        }
        git(&self.home, &self.workspace.worktree, &["add", "-A"]);
        git(
            &self.home,
            &self.workspace.worktree,
            &[
                "-c",
                "user.name=Agent One",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-q",
                "-m",
                "Agent work",
            ],
        );
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Add the agent file".into(),
        };
        landing::record_turn_end(&self.db, &self.workspace, &turn)
            .unwrap()
            .unwrap()
    }

    fn head(&self, id: &ChangeSetId) -> String {
        landing::change_set(&self.db, id).unwrap().unwrap().1.head
    }
}

fn admitted(
    argv: &[&str],
    network: NetworkMode,
    toolchain: Option<Toolchain>,
    files: &BTreeMap<String, Vec<u8>>,
) -> AdmittedCheck {
    let holdout = (!files.is_empty()).then(|| {
        files
            .iter()
            .map(|(path, bytes)| (path.clone(), format!("{:x}", Sha256::digest(bytes))))
            .collect()
    });
    AdmittedCheck {
        profile: ProfileId::new("a".repeat(64)).unwrap(),
        approved_ms: 1_800_000_000_000,
        spec: CheckSpec {
            id: "unit".into(),
            kind: CheckKind::Unit,
            toolchain,
            argv: argv.iter().map(|item| (*item).to_owned()).collect(),
            cwd: ".".into(),
            format: ReportFormat::ExitCode,
            report: None,
            timeout_s: 600,
            network,
            when: vec![Trigger::OnRequest],
            required: true,
            protected: false,
        },
        holdout,
    }
}

fn check(argv: &[&str]) -> AdmittedCheck {
    admitted(argv, NetworkMode::Off, None, &BTreeMap::new())
}

fn limits() -> RunLimits {
    RunLimits {
        timeout: Duration::from_secs(120),
        output_cap: runner::OUTPUT_CAP,
        report_cap: runner::REPORT_CAP,
        network_ceiling: NetworkMode::Remotes,
        remotes: Vec::new(),
    }
}

fn run(snapshot: &Snapshot, check: &AdmittedCheck, limits: &RunLimits) -> CheckExecution {
    runner::run(
        snapshot,
        check,
        &BTreeMap::new(),
        limits,
        &AtomicBool::new(false),
    )
}

fn stdout(execution: &CheckExecution) -> String {
    String::from_utf8_lossy(&execution.stdout_tail).into_owned()
}

fn gone(pid: i32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if rustix::process::test_kill_process(pid).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_check_cannot_write_the_agent_worktree_or_read_the_owner_home() {
    let case = Case::new();
    let escape = case.workspace.worktree.join("escape.txt");
    let owner = case.repo.join("escape.txt");
    let probe = format!(
        "if echo pwned > {w} 2>/dev/null; then echo wrote_worktree; else echo worktree_denied; fi\n\
         if echo pwned > {o} 2>/dev/null; then echo wrote_owner; else echo owner_denied; fi\n\
         if ls {h} > /dev/null 2>&1; then echo listed_home; else echo home_denied; fi\n\
         echo inside > made.txt && echo snapshot_written\n",
        w = quoted(&escape),
        o = quoted(&owner),
        h = quoted(&owner_home()),
    );
    let id = case.change_set(&[("probe.sh", &probe)]);
    let worktree = contents(&case.workspace.worktree);
    let checkout = contents(&case.repo);
    let snapshot = case.snapshot(&id, "run-1");
    assert_eq!(snapshot.commit, case.head(&id));
    let execution = run(&snapshot, &check(&["/bin/sh", "probe.sh"]), &limits());
    assert_eq!(
        execution.exit,
        Some(0),
        "{}",
        String::from_utf8_lossy(&execution.stderr_tail)
    );
    assert_eq!(
        stdout(&execution),
        "worktree_denied\nowner_denied\nhome_denied\nsnapshot_written\n"
    );
    assert!(!execution.timed_out && !execution.cancelled);
    assert_eq!(execution.unavailable, None);
    assert!(!execution.holdout);
    assert_eq!(
        fs::read_to_string(snapshot.root.join("made.txt")).unwrap(),
        "inside\n"
    );
    assert!(!escape.exists() && !owner.exists());
    assert_eq!(contents(&case.workspace.worktree), worktree);
    assert_eq!(contents(&case.repo), checkout);
    assert!(snapshot.root.starts_with(case.checks()));
    snapshot.remove().unwrap();
    assert_eq!(
        fs::read_dir(
            case.checks()
                .join(snapshot::project_folder(&case.workspace.project))
        )
        .unwrap()
        .count(),
        0
    );
}

#[test]
fn a_check_with_network_off_cannot_open_a_socket() {
    let case = Case::new();
    let listener = spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe = format!(
        "exec /usr/bin/perl -MIO::Socket::INET -e 'IO::Socket::INET->new(PeerAddr => \"127.0.0.1:{port}\", Timeout => 5) or exit 3; print \"connected\\n\"; exit 0'\n"
    );
    let id = case.change_set(&[("connect.sh", &probe)]);
    let snapshot = case.snapshot(&id, "run-1");
    let off = run(&snapshot, &check(&["/bin/sh", "connect.sh"]), &limits());
    assert_eq!(
        off.exit,
        Some(3),
        "{}",
        String::from_utf8_lossy(&off.stderr_tail)
    );
    assert_eq!(off.network, NetworkMode::Off);
    assert!(
        spawn_gate::accept_tcp_std(&listener)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let mut loopback = check(&["/bin/sh", "connect.sh"]);
    loopback.spec.network = NetworkMode::Localhost;
    let mut ceiling = limits();
    ceiling.network_ceiling = NetworkMode::Off;
    let lowered = run(&snapshot, &loopback, &ceiling);
    assert_eq!(lowered.exit, Some(3));
    assert_eq!(lowered.network, NetworkMode::Off);
    assert!(
        spawn_gate::accept_tcp_std(&listener)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let local = run(&snapshot, &loopback, &limits());
    assert_eq!(
        local.exit,
        Some(0),
        "{}",
        String::from_utf8_lossy(&local.stderr_tail)
    );
    assert_eq!(stdout(&local), "connected\n");
    assert_eq!(local.network, NetworkMode::Localhost);
    assert!(spawn_gate::accept_tcp_std(&listener).is_ok());
    assert_ne!(off.sandbox_digest, local.sandbox_digest);
}

#[test]
fn a_timed_out_check_kills_its_process_group_and_says_timed_out() {
    let case = Case::new();
    let id = case.change_set(&[("slow.sh", "/bin/sleep 600 &\necho $! > sleeper.pid\nwait\n")]);
    let snapshot = case.snapshot(&id, "run-1");
    let mut short = limits();
    short.timeout = Duration::from_secs(3);
    let started = Instant::now();
    let execution = run(&snapshot, &check(&["/bin/sh", "slow.sh"]), &short);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(execution.timed_out);
    assert!(!execution.cancelled);
    assert_eq!(execution.exit, None);
    assert!(execution.duration_ms >= 3000);
    let sleeper: i32 = fs::read_to_string(snapshot.root.join("sleeper.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(gone(sleeper), "the sleeping child outlived the timeout");
}

#[test]
fn a_cancelled_check_kills_its_process_group_and_says_cancelled() {
    let case = Case::new();
    let id = case.change_set(&[("slow.sh", "/bin/sleep 600 &\necho $! > sleeper.pid\nwait\n")]);
    let snapshot = case.snapshot(&id, "run-1");
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let execution = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_secs(2));
            cancel.store(true, Ordering::SeqCst);
        });
        runner::run(
            &snapshot,
            &check(&["/bin/sh", "slow.sh"]),
            &BTreeMap::new(),
            &limits(),
            &cancel,
        )
    });
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(execution.cancelled);
    assert!(!execution.timed_out);
    assert_eq!(execution.exit, None);
    let sleeper: i32 = fs::read_to_string(snapshot.root.join("sleeper.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(gone(sleeper), "the sleeping child outlived the cancel");
}

#[test]
fn output_past_the_cap_keeps_the_tail_and_marks_it() {
    let case = Case::new();
    let id = case.change_set(&[(
        "loud.sh",
        "exec /usr/bin/perl -e 'print \"x\" x 1000000; print \"\\nTHE-END\\n\"; print STDERR \"e\" x 300; print STDERR \"\\nERR-END\\n\"'\n",
    )]);
    let snapshot = case.snapshot(&id, "run-1");
    let mut small = limits();
    small.output_cap = 64 * 1024;
    let execution = run(&snapshot, &check(&["/bin/sh", "loud.sh"]), &small);
    assert_eq!(execution.exit, Some(0));
    assert_eq!(execution.stdout_tail.len(), 64 * 1024);
    assert!(execution.stdout_tail.ends_with(b"x\nTHE-END\n"));
    assert_eq!(execution.stdout_elided, 1_000_009 - 64 * 1024);
    assert_eq!(execution.stderr_elided, 0);
    assert_eq!(execution.stderr_tail.len(), 309);
    assert!(execution.stderr_tail.ends_with(b"e\nERR-END\n"));
}

#[test]
fn a_hostile_gitattributes_filter_in_the_change_set_never_runs_during_checkout() {
    let case = Case::new();
    let markers = case.root.join("markers");
    private(&markers);
    let hooks = case.root.join("hooks");
    private(&hooks);
    for hook in [
        "post-checkout",
        "post-index-change",
        "reference-transaction",
    ] {
        marker_script(
            &case.repo.join(".git/hooks").join(hook),
            &markers.join(hook),
        );
        marker_script(&hooks.join(hook), &markers.join(format!("path-{hook}")));
    }
    let helper = |name: &str| {
        let script = case.home.join(format!("{name}.sh"));
        marker_script(&script, &markers.join(name));
        script.display().to_string()
    };
    for (key, value) in [
        ("filter.evil.clean", helper("clean")),
        ("filter.evil.smudge", helper("smudge")),
        ("filter.evil.process", helper("process")),
        ("core.fsmonitor", helper("fsmonitor")),
        ("core.hooksPath", hooks.display().to_string()),
    ] {
        git(&case.home, &case.repo, &["config", key, &value]);
    }
    let id = case.change_set(&[
        (".gitattributes", "* filter=evil\n"),
        ("data.txt", "payload\n"),
    ]);
    let head = case.head(&id);
    let snapshot = case.snapshot(&id, "run-1");
    assert_eq!(snapshot.commit, head);
    assert_eq!(
        snapshot.tree,
        git(
            &case.home,
            &case.repo,
            &["rev-parse", &format!("{head}^{{tree}}")]
        )
    );
    assert_eq!(
        fs::read_to_string(snapshot.root.join("data.txt")).unwrap(),
        "payload\n"
    );
    assert_eq!(
        fs::read_to_string(snapshot.root.join(".gitattributes")).unwrap(),
        "* filter=evil\n"
    );
    assert!(!snapshot.root.join(".git/objects/info/alternates").exists());
    assert_eq!(
        git(&case.home, &snapshot.root, &["rev-parse", "HEAD"]),
        head
    );
    let ran: Vec<_> = fs::read_dir(&markers)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(ran.is_empty(), "configured commands ran: {ran:?}");
}

#[test]
fn a_symlink_in_the_change_set_tree_is_refused_and_leaves_no_snapshot() {
    let case = Case::new();
    std::os::unix::fs::symlink("README", case.workspace.worktree.join("inner-link")).unwrap();
    let id = case.change_set(&[("plain.txt", "plain\n")]);
    let refused = snapshot::prepare(
        &case.db,
        &case.checks(),
        &id,
        &RunKey::new("run-1").unwrap(),
    )
    .unwrap_err();
    assert_eq!(refused.0, "checks_snapshot_link");
    let project = case
        .checks()
        .join(snapshot::project_folder(&case.workspace.project));
    assert!(!project.join("run-1").exists());
}

#[test]
fn holdout_files_exist_only_inside_the_snapshot_during_the_run() {
    let case = Case::new();
    let id = case.change_set(&[("visible.txt", "visible\n")]);
    let snapshot = case.snapshot(&id, "run-1");
    let files = BTreeMap::from([(
        "holdout/hidden_test.sh".to_owned(),
        b"cat visible.txt\necho holdout-ran\n".to_vec(),
    )]);
    let holdout = admitted(
        &["/bin/sh", "holdout/hidden_test.sh"],
        NetworkMode::Off,
        None,
        &files,
    );
    let mut forged = files.clone();
    forged.insert(
        "holdout/hidden_test.sh".to_owned(),
        b"echo forged\n".to_vec(),
    );
    let refused = runner::run(
        &snapshot,
        &holdout,
        &forged,
        &limits(),
        &AtomicBool::new(false),
    );
    assert_eq!(
        refused.unavailable.as_deref(),
        Some("checks_holdout_missing")
    );
    assert_eq!(refused.exit, None);
    assert!(!snapshot.root.join("holdout").exists());
    let execution = runner::run(
        &snapshot,
        &holdout,
        &files,
        &limits(),
        &AtomicBool::new(false),
    );
    assert_eq!(
        execution.exit,
        Some(0),
        "{}",
        String::from_utf8_lossy(&execution.stderr_tail)
    );
    assert_eq!(stdout(&execution), "visible\nholdout-ran\n");
    assert!(execution.holdout);
    for folder in [&snapshot.root, &case.workspace.worktree, &case.repo] {
        assert!(!folder.join("holdout/hidden_test.sh").exists());
        assert!(!folder.join("holdout").exists());
    }
    let plain = run(
        &snapshot,
        &check(&["/bin/sh", "-c", "ls holdout"]),
        &limits(),
    );
    assert_eq!(plain.unavailable.as_deref(), Some("checks_snapshot_spent"));
    assert_eq!(plain.exit, None);
}

#[test]
fn a_snapshot_that_ran_a_holdout_check_refuses_every_later_check() {
    let case = Case::new();
    let listener = spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let id = case.change_set(&[("visible.txt", "visible\n")]);
    let snapshot = case.snapshot(&id, "run-1");
    let files = BTreeMap::from([(
        "holdout/leak.sh".to_owned(),
        format!(
            "cp holdout/leak.sh copied.txt && echo copied\n\
             exec /usr/bin/perl -MIO::Socket::INET -e 'IO::Socket::INET->new(PeerAddr => \"127.0.0.1:{port}\", Timeout => 5) or exit 3; exit 0'\n"
        )
        .into_bytes(),
    )]);
    let holdout = admitted(
        &["/bin/sh", "holdout/leak.sh"],
        NetworkMode::Remotes,
        None,
        &files,
    );
    let mut wide = limits();
    wide.remotes = vec![SandboxRemote::Loopback(port)];
    let execution = runner::run(&snapshot, &holdout, &files, &wide, &AtomicBool::new(false));
    assert_eq!(execution.network, NetworkMode::Off);
    assert_eq!(
        execution.exit,
        Some(3),
        "{}",
        String::from_utf8_lossy(&execution.stderr_tail)
    );
    assert_eq!(stdout(&execution), "copied\n");
    assert!(execution.holdout);
    assert!(spawn_gate::accept_tcp_std(&listener).is_err());
    assert!(snapshot.root.join("copied.txt").exists());
    assert!(!snapshot.root.join("holdout").exists());
    let later = run(&snapshot, &check(&["/bin/cat", "copied.txt"]), &limits());
    assert_eq!(later.unavailable.as_deref(), Some("checks_snapshot_spent"));
    assert_eq!(later.exit, None);
    assert!(later.stdout_tail.is_empty());
    let again = runner::run(&snapshot, &holdout, &files, &wide, &AtomicBool::new(false));
    assert_eq!(again.unavailable.as_deref(), Some("checks_snapshot_spent"));
    assert!(!snapshot.root.join("holdout").exists());
}

#[test]
fn an_agent_store_that_borrows_a_third_repository_is_refused() {
    let mut case = Case::new();
    let id = case.own_branch_change_set(&[("agent.txt", "agent\n")]);
    let honest = case.snapshot(&id, "run-1");
    assert_eq!(honest.commit, case.head(&id));
    assert_eq!(
        fs::read_to_string(honest.root.join("agent.txt")).unwrap(),
        "agent\n"
    );
    let third = case.root.join("third");
    private(&third);
    git(
        &case.home,
        &third,
        &["init", "-q", "--template=", "-b", "main"],
    );
    let alternates = case.workspace.git_dir.join("objects/info/alternates");
    let mut lines = fs::read_to_string(&alternates).unwrap();
    lines.push_str(&format!(
        "{}\n",
        third.join(".git/objects").to_str().unwrap()
    ));
    fs::set_permissions(&alternates, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&alternates, lines).unwrap();
    let refused = snapshot::prepare(
        &case.db,
        &case.checks(),
        &id,
        &RunKey::new("run-2").unwrap(),
    )
    .unwrap_err();
    assert_eq!(refused.0, "checks_snapshot_store_invalid");
    let project = case
        .checks()
        .join(snapshot::project_folder(&case.workspace.project));
    assert!(!project.join("run-2").exists());
}

#[test]
fn a_report_is_not_read_when_the_run_cleanup_is_uncertain() {
    let case = Case::new();
    let id = case.change_set(&[
        ("report.sh", "echo '<testsuite/>' > report.xml\n"),
        (
            "unwarded.sh",
            "for warden in $(/usr/bin/pgrep -P $$); do kill -9 \"$warden\"; done\n\
             echo '<testsuite/>' > report.xml\n",
        ),
    ]);
    let snapshot = case.snapshot(&id, "run-1");
    let junit = |script: &str| {
        let mut junit = check(&["/bin/sh", script]);
        junit.spec.format = ReportFormat::Junit;
        junit.spec.report = Some("report.xml".into());
        junit
    };
    let confirmed = run(&snapshot, &junit("report.sh"), &limits());
    assert_eq!(confirmed.exit, Some(0));
    assert!(!confirmed.cleanup_uncertain);
    assert_eq!(confirmed.report_problem, None);
    assert_eq!(
        confirmed.report,
        vec![ReportFile {
            name: "report.xml".into(),
            bytes: b"<testsuite/>\n".to_vec(),
        }]
    );
    let uncertain = run(&snapshot, &junit("unwarded.sh"), &limits());
    assert_eq!(
        uncertain.exit,
        Some(0),
        "{}",
        String::from_utf8_lossy(&uncertain.stderr_tail)
    );
    assert!(uncertain.cleanup_uncertain);
    assert!(uncertain.report.is_empty());
    assert_eq!(uncertain.report_problem, Some("report_unreadable"));
}

#[test]
fn the_toolchain_version_is_read_inside_the_sandbox() {
    let case = Case::new();
    let id = case.change_set(&[("rust-toolchain.toml", "[toolchain]\nchannel = \"1.98.1\"\n")]);
    let snapshot = case.snapshot(&id, "run-1");
    let rust = admitted(
        &["rustc", "--version"],
        NetworkMode::Off,
        Some(Toolchain::Rust),
        &BTreeMap::new(),
    );
    let execution = run(&snapshot, &rust, &limits());
    assert_eq!(
        execution.exit,
        Some(0),
        "{}",
        String::from_utf8_lossy(&execution.stderr_tail)
    );
    assert!(
        execution.toolchain_version.starts_with("rustc 1.98.1"),
        "{}",
        execution.toolchain_version
    );
    assert_eq!(stdout(&execution).trim(), execution.toolchain_version);
    let none = run(&snapshot, &check(&["/usr/bin/true"]), &limits());
    assert_eq!(none.toolchain_version, "none");
    assert_eq!(none.exit, Some(0));
}
