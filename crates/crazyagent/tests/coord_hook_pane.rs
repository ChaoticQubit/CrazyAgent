#![cfg(unix)]

use crazyagent::team::caller::{Callers, socket_for_mailbox};
use crazyagent::team::tool::MAILBOX_FOLDER;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LEAD_SESSION: &str = "lead-session";
const MATE_SESSION: &str = "alpha-session";

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Server {
    fn run(&self, arguments: &[&str]) -> String {
        let output = crazyagent::spawn_gate::output(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .args(arguments)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn panes(&self) -> Vec<(String, u32)> {
        self.run(&["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])
            .lines()
            .filter_map(|line| {
                let (pane, pid) = line.split_once(' ')?;
                Some((pane.to_owned(), pid.parse().ok()?))
            })
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::output(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

struct Placed(PathBuf);

impl Drop for Placed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        if let Some(folder) = self.0.parent() {
            let _ = std::fs::remove_dir(folder);
        }
    }
}

/// A native lead's team as its hooks see it: a real tmux server with the lead's pane and
/// two teammate panes, Claude Code's team config naming the teammates by those panes, the
/// lead's mailbox in the engine's layout, and the lead's caller socket, which the test
/// serves as the engine does.
struct Team {
    _folder: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    config: PathBuf,
    mailbox: PathBuf,
    server: Server,
    callers: Callers,
    _placed: Placed,
    alpha: String,
    beta: String,
    runs: u32,
}

impl Team {
    fn new() -> Self {
        let folder = tempfile::Builder::new()
            .prefix("ca-hook-pane-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = folder.path().to_path_buf();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = Server {
            tmux: crazyagent::terminal::tmux::installed(&root)
                .expect("tmux must be installed on this machine for native teammates"),
            socket: root.join("t.sock"),
        };
        let lead = server.run(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-x",
            "120",
            "-y",
            "40",
            "sleep 600",
        ]);
        let split = || {
            server.run(&[
                "split-window",
                "-t",
                &lead,
                "-P",
                "-F",
                "#{pane_id}",
                "sleep 600",
            ])
        };
        let alpha = split();
        let beta = split();
        assert!(lead != alpha && alpha != beta && beta != lead);
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let config = root.join("home");
        let team = config.join("teams").join("pane-team");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::write(
            team.join("config.json"),
            json!({
                "leadSessionId": LEAD_SESSION,
                "members": [
                    {"name": "team-lead", "tmuxPaneId": "leader"},
                    {"name": "alpha", "tmuxPaneId": alpha, "cwd": project},
                    {"name": "beta", "tmuxPaneId": beta, "cwd": project},
                ],
            })
            .to_string(),
        )
        .unwrap();
        let mailbox = root.join("agent-1").join("tmp").join(MAILBOX_FOLDER);
        std::fs::create_dir_all(&mailbox).unwrap();
        std::fs::set_permissions(&mailbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = socket_for_mailbox(&mailbox).unwrap();
        let short = socket.parent().unwrap();
        assert!(!short.exists(), "{}", short.display());
        std::fs::create_dir(short).unwrap();
        std::fs::set_permissions(short, std::fs::Permissions::from_mode(0o700)).unwrap();
        let placed = Placed(socket.clone());
        let callers = Callers::bind(&socket, None).unwrap();
        Self {
            _folder: folder,
            root,
            project,
            config,
            mailbox,
            server,
            callers,
            _placed: placed,
            alpha,
            beta,
            runs: 0,
        }
    }

    /// Runs one relay hook the way a teammate's harness does, in `pane` of the tmux server,
    /// with `TMUX_PANE` set to `claimed`; detached, it runs a second after its parent shell
    /// left, under pid 1. The test serves the caller socket meanwhile. Takes the pane, the
    /// claimed pane, the event, the hook input and whether it is detached. Returns what the
    /// hook printed.
    fn hook(
        &mut self,
        pane: &str,
        claimed: &str,
        event: &str,
        input: &Value,
        detached: bool,
    ) -> String {
        self.runs += 1;
        let run_id = self.runs;
        let input_path = self.root.join(format!("in-{run_id}.json"));
        let output = self.root.join(format!("out-{run_id}.txt"));
        let done = self.root.join(format!("done-{run_id}"));
        std::fs::write(&input_path, input.to_string()).unwrap();
        let run = format!(
            "/usr/bin/env CLAUDE_CONFIG_DIR='{}' TMUX_PANE='{claimed}' '{}' coord-hook {event} '{}' < '{}' > '{}'; touch '{}'",
            self.config.display(),
            env!("CARGO_BIN_EXE_crazyagent"),
            self.mailbox.display(),
            input_path.display(),
            output.display(),
            done.display(),
        );
        let command = if detached {
            format!("( (sleep 1; {run}) & ); sleep 600")
        } else {
            format!("{run}; sleep 600")
        };
        self.server
            .run(&["respawn-pane", "-k", "-t", pane, &command]);
        let server = &self.server;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done.exists() && Instant::now() < deadline {
            self.callers.serve(&mut || server.panes());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(done.exists(), "the hook in {pane} never finished");
        std::fs::read_to_string(&output).unwrap()
    }

    fn tools(&self, pane: &str) -> Option<String> {
        let number: u128 = pane[1..].parse().unwrap();
        std::fs::read_to_string(self.mailbox.join(format!("{:032x}.tools", number + 1))).ok()
    }
}

fn read_tool(session: &str) -> Value {
    json!({"session_id": session, "tool_name": "Read", "tool_input": {"file_path": "/etc/hosts"}})
}

fn edit(session: &str, path: &Path) -> Value {
    json!({
        "session_id": session,
        "tool_name": "Edit",
        "tool_input": {"file_path": path, "old_string": "a", "new_string": "b"},
    })
}

fn denial(output: &str) -> Option<String> {
    let answer: Value = serde_json::from_str(output.trim()).ok()?;
    let decision = &answer["hookSpecificOutput"];
    (decision["permissionDecision"] == "deny").then(|| {
        decision["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .to_owned()
    })
}

/// A teammate's hook runs in the teammate's own tmux pane with `TMUX_PANE` claiming another
/// teammate's pane, as any process in the lead's shared sandbox may set it. The engine names
/// the pane the hook's process runs in, so the tool counts for the real caller and its edits
/// stay confined to its own worktree, never the claimed teammate's (DEC-111 Q18).
#[test]
fn a_teammate_hook_claiming_another_pane_acts_as_its_own_pane() {
    let mut team = Team::new();
    let (alpha, beta) = (team.alpha.clone(), team.beta.clone());

    let printed = team.hook(&alpha, &beta, "post-tool", &read_tool(MATE_SESSION), false);
    assert_eq!(printed, "");
    assert_eq!(team.tools(&beta), None, "the claimed pane got nothing");
    assert_eq!(
        team.tools(&alpha).as_deref(),
        Some("Read\n"),
        "the tool counts for the pane the hook runs in"
    );

    let theirs = team
        .project
        .join(".worktrees")
        .join("beta")
        .join("notes.md");
    let printed = team.hook(&alpha, &beta, "edit", &edit(MATE_SESSION, &theirs), false);
    let reason = denial(&printed);
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains("you are teammate alpha")),
        "{printed}"
    );
}

/// A hook whose process left the pane tree, its parent now pid 1, runs in no teammate's
/// pane: whatever `TMUX_PANE` says, it counts for nobody and its edit is held, never let
/// through as the claimed teammate's or the lead's (DEC-111 Q18).
#[test]
fn a_hook_outside_every_pane_counts_for_nobody_and_its_edits_are_held() {
    let mut team = Team::new();
    let alpha = team.alpha.clone();

    let printed = team.hook(&alpha, &alpha, "post-tool", &read_tool(MATE_SESSION), true);
    assert_eq!(printed, "");
    assert_eq!(team.tools(&alpha), None, "nobody's tool is counted");

    let own = team
        .project
        .join(".worktrees")
        .join("alpha")
        .join("notes.md");
    let printed = team.hook(&alpha, &alpha, "edit", &edit(MATE_SESSION, &own), true);
    let reason = denial(&printed);
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not tell which agent")),
        "{printed}"
    );
}
