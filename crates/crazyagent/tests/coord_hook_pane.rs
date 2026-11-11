#![cfg(unix)]

use crazyagent::protocol::ProjectId;
use crazyagent::team::caller::{Callers, Whom, socket_for_mailbox};
use crazyagent::team::tool::MAILBOX_FOLDER;
use rusqlite::{Connection, params};
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
/// lead's mailbox in the engine's layout, the engine's database with the lead and both
/// teammates' rows by those panes, and the lead's caller socket, which the test serves as
/// the engine does, answering `who` for the lead's pane and each teammate's pane from the
/// names it registered, as the engine answers from its rows, and each teammate's relay
/// request with the engine's real handler.
struct Team {
    _folder: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    config: PathBuf,
    mailbox: PathBuf,
    server: Server,
    callers: Option<Callers>,
    db: Connection,
    project_id: ProjectId,
    lead_id: i64,
    _placed: Placed,
    lead: String,
    alpha: String,
    beta: String,
    runs: u32,
    starts: crazyagent::coord::relay::TaskStarts,
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
                "/bin/sh",
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
        let part = mailbox.join(crazyagent::coord::relay::LEAD_PART);
        std::fs::create_dir(&part).unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = socket_for_mailbox(&mailbox).unwrap();
        let short = socket.parent().unwrap();
        assert!(!short.exists(), "{}", short.display());
        std::fs::create_dir(short).unwrap();
        std::fs::set_permissions(short, std::fs::Permissions::from_mode(0o700)).unwrap();
        let placed = Placed(socket.clone());
        let callers = Callers::bind(&socket, None).unwrap();
        let db = Connection::open(root.join("engine.db")).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
            .unwrap();
        crazyagent::team::initialize(&db).unwrap();
        crazyagent::project_setup::initialize(&db).unwrap();
        crazyagent::coord::ensure(&db).unwrap();
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES ('alpha','sub_agent','claude-code',1,'working',0,0)",
            [],
        )
        .unwrap();
        let lead_id = db.last_insert_rowid();
        for pane in [&alpha, &beta] {
            db.execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent,pane)
                 VALUES ('alpha','teammate','claude-code',1,'working',0,0,?1,?2)",
                params![lead_id, pane],
            )
            .unwrap();
        }
        Self {
            _folder: folder,
            root,
            project,
            config,
            mailbox,
            server,
            callers: Some(callers),
            db,
            project_id: ProjectId::new("alpha").unwrap(),
            lead_id,
            _placed: placed,
            lead,
            alpha,
            beta,
            runs: 0,
            starts: crazyagent::coord::relay::TaskStarts::default(),
        }
    }

    /// Runs one relay hook the way a teammate's harness does, as a child of the one shell
    /// that is `pane`'s process for the whole test, as Claude Code's hooks run under its
    /// pane's one process, with `TMUX_PANE` set to `claimed`; detached, it runs a second
    /// after the subshell that started it left, under pid 1. The test serves the caller
    /// socket meanwhile, each relay request with the engine's real handler. Takes the pane, the
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
            "/usr/bin/env CLAUDE_CONFIG_DIR='{}' CLAUDE_PROJECT_DIR='{}' TMUX_PANE='{claimed}' '{}' coord-hook {event} '{}' < '{}' > '{}'; touch '{}'",
            self.config.display(),
            self.project.display(),
            env!("CARGO_BIN_EXE_crazyagent"),
            self.mailbox.display(),
            input_path.display(),
            output.display(),
            done.display(),
        );
        let command = if detached {
            format!("( (sleep 1; {run}) & )")
        } else {
            run
        };
        self.server.run(&["send-keys", "-t", pane, "-l", &command]);
        self.server.run(&["send-keys", "-t", pane, "Enter"]);
        let server = &self.server;
        let rows = [
            (self.lead.as_str(), Whom::Lead),
            (self.alpha.as_str(), Whom::Teammate("alpha".to_owned())),
            (self.beta.as_str(), Whom::Teammate("beta".to_owned())),
        ];
        let (db, project, lead) = (&self.db, &self.project_id, self.lead_id);
        let starts = &mut self.starts;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done.exists() && Instant::now() < deadline {
            if let Some(callers) = self.callers.as_mut() {
                callers.serve(
                    &mut || server.panes(),
                    &mut |pane, _| {
                        rows.iter()
                            .find(|(row, _)| *row == pane)
                            .map(|(_, whom)| whom.clone())
                    },
                    &mut |pane, _, request, out| {
                        crazyagent::coord::relay::answer(
                            db,
                            crazyagent::coord::relay::ForLead {
                                project,
                                id: lead,
                                starts: &mut *starts,
                            },
                            pane,
                            request,
                            out,
                            now_ms(),
                        )
                        .unwrap();
                    },
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(done.exists(), "the hook in {pane} never finished");
        std::fs::read_to_string(&output).unwrap()
    }

    /// The tools the engine counted for the teammate in a pane, by name.
    fn tools(&self, pane: &str) -> Vec<String> {
        self.db
            .prepare(
                "SELECT tool FROM coord_tool_use WHERE agent=(SELECT id FROM team_agents
                 WHERE role='teammate' AND pane=?1) ORDER BY tool",
            )
            .unwrap()
            .query_map([pane], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn lead_tools(&self) -> Option<String> {
        std::fs::read_to_string(self.lead_part().join(format!("{:032x}.tools", 0))).ok()
    }

    fn lead_part(&self) -> PathBuf {
        self.mailbox.join(crazyagent::coord::relay::LEAD_PART)
    }

    /// Overwrites the team config the way any agent of the team can. Takes the new config.
    fn forge(&self, config: &Value) {
        std::fs::write(
            self.config
                .join("teams")
                .join("pane-team")
                .join("config.json"),
            config.to_string(),
        )
        .unwrap();
    }

    /// Leaves one message in the lead's part of the mailbox, as the engine does for a native
    /// lead's own sessions. Takes the message id and the body.
    fn lead_letter(&self, id: i64, body: &str) {
        std::fs::write(
            self.lead_part().join(format!("{id:032x}.inbox")),
            json!({
                "id": id,
                "to": "leader",
                "from": "Ada (@lead-a)",
                "kind": "note",
                "question": null,
                "work": null,
                "reply_to": null,
                "body": body,
            })
            .to_string(),
        )
        .unwrap();
    }

    /// Sends one note from the lead to the teammate in a pane, which the engine hands that
    /// teammate's session on its own request. Takes the pane and the body.
    fn note(&self, pane: &str, body: &str) {
        let to: i64 = self
            .db
            .query_row(
                "SELECT id FROM team_agents WHERE role='teammate' AND pane=?1",
                [pane],
                |row| row.get(0),
            )
            .unwrap();
        crazyagent::coord::send(
            &self.db,
            crazyagent::coord::NewMessage {
                project: self.project_id.clone(),
                from: crazyagent::coord::Party::Agent(self.lead_id),
                to: crazyagent::coord::Party::Agent(to),
                kind: crazyagent::coord::MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: body.to_owned(),
                idempotency_key: None,
            },
            now_ms(),
        )
        .unwrap();
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
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
    assert!(team.tools(&beta).is_empty(), "the claimed pane got nothing");
    assert_eq!(
        team.tools(&alpha),
        ["Read"],
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

/// T3-15: a native teammate's prompt hook, run in its own pane over the lead's caller
/// socket, adds the lead's Task-start reminder to its first prompt of a task as the prompt's
/// added context, adds nothing to a prompt mid-task, and adds it again to its first prompt
/// after it rested.
#[test]
fn a_teammate_prompt_hook_adds_the_task_start_reminder_to_the_first_prompt_of_each_task() {
    const TEXT: &str = "Task start reminder: before you work on this task, query the project graph with `graphify query \"<question>\"` before you read or search source, and check these skills and call the Skill tool for each one that applies: `crazyagent:tdd`.";
    let mut team = Team::new();
    team.starts.text = Some(TEXT.into());
    let alpha = team.alpha.clone();
    let prompt = json!({"session_id": MATE_SESSION, "prompt": "Write notes.md"});
    let context = json!({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit",
        "additionalContext": TEXT,
    }});
    let added = |printed: String| serde_json::from_str::<Value>(printed.trim()).ok();

    assert_eq!(
        added(team.hook(&alpha, &alpha, "prompt", &prompt, false)),
        Some(context.clone()),
        "the first prompt of a task"
    );
    assert_eq!(
        team.hook(&alpha, &alpha, "prompt", &prompt, false),
        "",
        "a prompt mid-task"
    );
    assert_eq!(
        team.hook(
            &alpha,
            &alpha,
            "stop",
            &json!({"session_id": MATE_SESSION}),
            false
        ),
        ""
    );
    assert_eq!(
        added(team.hook(&alpha, &alpha, "prompt", &prompt, false)),
        Some(context),
        "the first prompt after it rested"
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
    assert!(team.tools(&alpha).is_empty(), "nobody's tool is counted");

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

/// A teammate that rewrites its lead's team config, which every agent of the team can
/// write, still acts only as itself, since the engine names the pane and the member behind
/// it from its own rows (DEC-111 Q18, RH-2): naming its own session as the lead's, giving
/// its pane another member's name or removing every pane entry still hands it only its own
/// letters, counts its tools for its own pane and keeps its edits in its own worktree.
#[test]
fn a_teammate_rewriting_its_team_config_still_acts_as_itself() {
    let mut team = Team::new();
    let (alpha, beta) = (team.alpha.clone(), team.beta.clone());
    let forgeries = [
        json!({
            "leadSessionId": MATE_SESSION,
            "members": [
                {"name": "team-lead", "tmuxPaneId": "leader"},
                {"name": "alpha", "tmuxPaneId": alpha, "cwd": team.project},
                {"name": "beta", "tmuxPaneId": beta, "cwd": team.project},
            ],
        }),
        json!({
            "leadSessionId": LEAD_SESSION,
            "members": [
                {"name": "team-lead", "tmuxPaneId": "leader"},
                {"name": "beta", "tmuxPaneId": alpha, "cwd": team.project},
                {"name": "alpha", "tmuxPaneId": beta, "cwd": team.project},
            ],
        }),
        json!({
            "leadSessionId": LEAD_SESSION,
            "members": [{"name": "team-lead", "tmuxPaneId": "leader"}],
        }),
    ];
    let theirs = team
        .project
        .join(".worktrees")
        .join("beta")
        .join("notes.md");
    for (round, forged) in forgeries.iter().enumerate() {
        team.forge(forged);
        let id = 10 + 2 * i64::try_from(round).unwrap();
        team.lead_letter(id, &format!("For the lead {round}."));
        team.note(&alpha, &format!("For alpha {round}."));
        let printed = team.hook(&alpha, &alpha, "post-tool", &read_tool(MATE_SESSION), false);
        assert!(
            printed.contains(&format!("For alpha {round}.")),
            "round {round}: {printed}"
        );
        assert!(
            !printed.contains("For the lead"),
            "round {round}: {printed}"
        );
        assert_eq!(
            team.lead_tools(),
            None,
            "round {round}: nothing counts for the lead"
        );
        assert_eq!(
            team.tools(&alpha),
            ["Read"],
            "round {round}: the tool counts for alpha's pane"
        );
        let printed = team.hook(&alpha, &alpha, "edit", &edit(MATE_SESSION, &theirs), false);
        assert!(
            denial(&printed).is_some_and(|reason| reason.contains("you are teammate alpha")),
            "round {round}: {printed}"
        );
    }
}

/// While the engine is away, its caller socket bound by no one, a teammate's hook is told
/// nothing and so is nobody: its tool counts for no one and its edit is held, never taken
/// for the lead's as when the agent leads no native team (DEC-111 Q18, RH-2).
#[test]
fn a_teammate_hook_while_the_engine_is_away_is_held_not_taken_for_the_lead() {
    let mut team = Team::new();
    let alpha = team.alpha.clone();
    team.callers = None;

    let printed = team.hook(&alpha, &alpha, "post-tool", &read_tool(MATE_SESSION), false);
    assert_eq!(printed, "");
    assert!(
        team.tools(&alpha).is_empty(),
        "nothing counts for the teammate"
    );
    assert_eq!(team.lead_tools(), None, "nothing counts for the lead");

    let own = team
        .project
        .join(".worktrees")
        .join("alpha")
        .join("notes.md");
    let printed = team.hook(&alpha, &alpha, "edit", &edit(MATE_SESSION, &own), false);
    assert!(
        denial(&printed).is_some_and(|reason| reason.contains("could not tell which agent")),
        "{printed}"
    );
}
