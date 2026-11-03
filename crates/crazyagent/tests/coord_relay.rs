#![cfg(unix)]

mod pane_support;

use crazyagent::coord::adoption::Flag;
use crazyagent::coord::relay::Layout;
use crazyagent::coord::settings::{CoordSettings, Nudges, Overlaps};
use crazyagent::protocol::ProjectId;
use crazyagent::usage::capture::{Attribution, Format, RunKind};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pane_support::{LEAD_PANE, PaneCallers, PaneRun};

const LEAD_SESSION: &str = "lead-session";
const MATE_SESSION: &str = "mate-session";
const MATE_PANE: &str = "%7";
const HELPER_AGENT: &str = "helper@relay-team";
const LEAD_KEY: &str = "00000000000000000000000000000000";
const MATE_KEY: &str = "00000000000000000000000000000008";

struct Team {
    _root: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    mailbox: PathBuf,
    callers: PaneCallers,
}

impl Team {
    fn new() -> Self {
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().to_path_buf();
        let config = path.join("home");
        let team = config.join("teams").join("relay-team");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::write(
            team.join("config.json"),
            json!({
                "leadSessionId": LEAD_SESSION,
                "members": [
                    {"name": "team-lead", "tmuxPaneId": "leader"},
                    {"name": "greet", "tmuxPaneId": MATE_PANE, "cwd": path},
                    {"name": "helper", "agentId": HELPER_AGENT, "tmuxPaneId": "in-process", "cwd": path},
                ],
            })
            .to_string(),
        )
        .unwrap();
        let (mailbox, callers) = PaneCallers::new(&path, &[(MATE_PANE, "greet")]);
        Self {
            _root: root,
            root: path,
            config,
            mailbox,
            callers,
        }
    }

    fn command(&self, args: &[&str], pane: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_crazyagent"));
        command
            .args(args)
            .arg(&self.mailbox)
            .env("CLAUDE_CONFIG_DIR", &self.config)
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("TMUX_PANE");
        if let Some(pane) = pane {
            command
                .env("TMUX_PANE", pane)
                .env("CLAUDE_PROJECT_DIR", &self.root);
        }
        command
    }

    fn start_hook(&self, event: &str, pane: Option<&str>, input: Value) -> PaneRun {
        let mut child = self.callers.run(
            pane.unwrap_or(LEAD_PANE),
            &self.command(&["coord-hook", event], pane),
        );
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child
    }

    fn hook(&self, event: &str, pane: Option<&str>, input: Value) -> String {
        finished(self.start_hook(event, pane, input))
    }

    fn letter(&self, id: i64, to: &str, body: &str) {
        std::fs::write(
            self.entry(&format!("{id:032x}.inbox")),
            json!({
                "id": id,
                "to": to,
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

    fn entry(&self, name: &str) -> PathBuf {
        self.mailbox.join(name)
    }

    fn lead_entry(&self, name: &str) -> PathBuf {
        self.mailbox
            .join(crazyagent::coord::relay::LEAD_PART)
            .join(name)
    }

    fn text(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.entry(name)).ok()
    }
}

fn finished(child: PaneRun) -> String {
    let done = child.wait_with_output().unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).unwrap()
}

fn tool(session: &str, agent: Option<&str>, name: &str, input: Value) -> Value {
    let mut value = json!({"session_id": session, "tool_name": name, "tool_input": input});
    if let Some(agent) = agent {
        value["agent_id"] = json!(agent);
    }
    value
}

fn context(output: &str) -> String {
    let answer: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(
        answer["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "{output}"
    );
    answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn denial(output: &str) -> String {
    let answer: Value = serde_json::from_str(output.trim()).unwrap();
    let decision = &answer["hookSpecificOutput"];
    assert_eq!(decision["hookEventName"], "PreToolUse", "{output}");
    assert_eq!(decision["permissionDecision"], "deny", "{output}");
    decision["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn edit(session: &str, path: &Path) -> Value {
    tool(
        session,
        None,
        "Edit",
        json!({"file_path": path, "old_string": "a", "new_string": "b"}),
    )
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

#[test]
fn post_tool_hands_each_session_only_its_own_letters_and_logs_the_tool_for_its_tag() {
    let team = Team::new();
    team.letter(5, "leader", "For the lead.");
    team.letter(6, MATE_PANE, "For greet.");

    let subagent = team.hook(
        "post-tool",
        None,
        tool(
            LEAD_SESSION,
            Some("sub-1"),
            "Read",
            json!({"file_path": "/x"}),
        ),
    );
    assert_eq!(subagent, "", "a subagent of the lead takes no letter");
    let helper = team.hook(
        "post-tool",
        None,
        tool(MATE_SESSION, Some(HELPER_AGENT), "Read", json!({})),
    );
    assert_eq!(helper, "", "an in-process teammate takes no letter");
    assert!(team.entry(&format!("{:032x}.inbox", 5)).is_file());

    let lead = context(&team.hook(
        "post-tool",
        None,
        tool(
            LEAD_SESSION,
            None,
            "Bash",
            json!({"command": "graphify query \"who calls serve\""}),
        ),
    ));
    assert!(
        lead.starts_with("CrazyAgent team messages for you (1). Answer a question or work for you with update_work kind reply"),
        "{lead}"
    );
    assert!(
        lead.ends_with(
            "[crazyagent msg 5] from Ada (@lead-a) (note, it takes no reply):\nFor the lead."
        ),
        "{lead}"
    );
    assert!(!lead.contains("For greet."));
    assert!(team.entry(&format!("{:032x}.shown", 5)).is_file());
    assert!(!team.entry(&format!("{:032x}.inbox", 5)).exists());
    assert_eq!(
        team.text(&format!("{LEAD_KEY}.tools")).as_deref(),
        Some("Read\nBash\ngraphify\n"),
        "the subagent's call counts for the lead, the in-process teammate's for nobody"
    );

    let mate = context(&team.hook(
        "post-tool",
        Some(MATE_PANE),
        tool(
            MATE_SESSION,
            None,
            "Edit",
            json!({"file_path": "/x", "old_string": "a", "new_string": "b"}),
        ),
    ));
    assert!(mate.ends_with(":\nFor greet."), "{mate}");
    assert!(!mate.contains("For the lead."));
    assert_eq!(
        team.text(&format!("{MATE_KEY}.tools")).as_deref(),
        Some("Edit\n")
    );

    let again = team.hook(
        "post-tool",
        None,
        tool(LEAD_SESSION, None, "Read", json!({})),
    );
    assert_eq!(again, "", "a taken letter is never handed over twice");
}

#[test]
fn the_leads_stop_blocks_only_while_a_letter_waits_for_it() {
    let team = Team::new();
    let stop = json!({"session_id": LEAD_SESSION, "stop_hook_active": false});
    assert_eq!(team.hook("stop", None, stop.clone()), "");
    assert!(team.entry(&format!("{LEAD_KEY}.touch")).is_file());

    team.letter(9, "leader", "Merge greet's branch.");
    let blocked = team.hook("stop", None, stop.clone());
    let answer: Value = serde_json::from_str(blocked.trim()).unwrap();
    assert_eq!(answer["decision"], "block", "{blocked}");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .ends_with("[crazyagent msg 9] from Ada (@lead-a) (note, it takes no reply):\nMerge greet's branch."),
        "{blocked}"
    );
    assert_eq!(team.hook("stop", None, stop), "");
}

#[test]
fn a_question_or_permission_prompt_sets_a_wait_that_the_next_tool_call_or_prompt_clears() {
    let team = Team::new();
    let waiting = || team.text(&format!("{LEAD_KEY}.waiting"));
    let asked = || team.entry(&format!("{LEAD_KEY}.asked")).is_file();

    team.hook(
        "asked",
        None,
        tool(LEAD_SESSION, None, "AskUserQuestion", json!({})),
    );
    assert_eq!(waiting().as_deref(), Some("question"));
    assert!(asked());
    team.hook(
        "post-tool",
        None,
        tool(LEAD_SESSION, None, "Read", json!({})),
    );
    assert_eq!(waiting().as_deref(), Some("none"));
    assert!(!asked());

    std::fs::remove_file(team.entry(&format!("{LEAD_KEY}.waiting"))).unwrap();
    team.hook(
        "notification",
        None,
        json!({"session_id": LEAD_SESSION, "notification_type": "idle_prompt"}),
    );
    assert_eq!(waiting(), None, "an idle notice is no wait");
    team.hook(
        "notification",
        None,
        json!({"session_id": LEAD_SESSION, "notification_type": "permission_prompt"}),
    );
    assert_eq!(waiting().as_deref(), Some("permission"));
    team.hook(
        "prompt",
        None,
        json!({"session_id": LEAD_SESSION, "prompt": "go", "source": "loop_wakeup"}),
    );
    assert_eq!(
        waiting().as_deref(),
        Some("permission"),
        "a prompt the harness typed itself is not the user answering"
    );
    team.hook(
        "prompt",
        None,
        json!({"session_id": LEAD_SESSION, "prompt": "go", "source": "user"}),
    );
    assert_eq!(waiting().as_deref(), Some("none"));
}

#[test]
fn a_teammate_edit_outside_its_own_worktree_is_denied_whatever_the_path_says() {
    let team = Team::new();
    let own = team.root.join(".worktrees").join("greet");
    std::fs::create_dir_all(own.join("src")).unwrap();
    let elsewhere = team.root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, own.join("out")).unwrap();
    let mate = |path: PathBuf| team.hook("edit", Some(MATE_PANE), edit(MATE_SESSION, &path));

    assert_eq!(mate(own.join("src").join("a.rs")), "");
    assert_eq!(
        mate(own.join("new").join("b.rs")),
        "",
        "a new folder is fine"
    );
    for outside in [
        team.root.join("src").join("a.rs"),
        own.join("..").join("other").join("a.rs"),
        own.join("out").join("a.rs"),
    ] {
        let reason = denial(&mate(outside.clone()));
        assert!(
            reason.starts_with(
                "CrazyAgent: you are teammate greet. Edit files only inside your own worktree"
            ),
            "{}: {reason}",
            outside.display()
        );
    }
    let notebook = team.hook(
        "edit",
        Some(MATE_PANE),
        tool(
            MATE_SESSION,
            None,
            "NotebookEdit",
            json!({"notebook_path": team.root.join("n.ipynb"), "new_source": "x"}),
        ),
    );
    assert!(denial(&notebook).contains("your own worktree"));
    assert_eq!(
        team.hook(
            "edit",
            None,
            edit(LEAD_SESSION, &team.root.join("src").join("a.rs"))
        ),
        "",
        "the lead is not bound to a teammate worktree"
    );
}

#[test]
fn under_overlaps_block_an_edit_on_another_agents_claim_is_denied_once_the_engine_answers() {
    let team = Team::new();
    let repo = team.root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let db = Connection::open(team.root.join("engine.db")).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
        .unwrap();
    crazyagent::team::initialize(&db).unwrap();
    crazyagent::project_setup::initialize(&db).unwrap();
    crazyagent::coord::ensure(&db).unwrap();
    let project = ProjectId::new("alpha").unwrap();
    let agent = || {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES ('alpha','sub_agent','claude-code',1,'working',0,0)",
            [],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let lead = agent();
    let other = agent();
    db.execute(
        "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
         VALUES (?1,'alpha',?2,?2,?3)",
        params![
            lead,
            repo.to_str().unwrap(),
            repo.join(".git").to_str().unwrap()
        ],
    )
    .unwrap();
    crazyagent::coord::settings::write(
        &db,
        &project,
        &CoordSettings {
            overlaps: Overlaps::Block,
            ..CoordSettings::default()
        },
    )
    .unwrap();
    crazyagent::coord::start_work(
        &db,
        &project,
        other,
        "Parser".to_owned(),
        vec!["src/a.rs".to_owned()],
        now_ms(),
    )
    .unwrap();
    let home = team.root.join("agent-1").join("home");
    std::fs::create_dir_all(&home).unwrap();
    let serve = || {
        crazyagent::coord::relay::serve(
            &db,
            &project,
            lead,
            &home,
            &team.mailbox,
            Layout::Single,
            false,
            now_ms(),
        )
        .unwrap()
    };
    serve();
    let answered = |path: PathBuf| {
        let mut child = team.start_hook("edit", None, edit(LEAD_SESSION, &path));
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the engine answered in time"
            );
            serve();
            std::thread::sleep(Duration::from_millis(50));
        }
        finished(child)
    };

    let reason = denial(&answered(repo.join("src").join("a.rs")));
    let holder = crazyagent::team::events::label(&db, other).unwrap();
    assert_eq!(
        reason,
        format!(
            "CrazyAgent: src/a.rs is claimed by {holder} (src/a.rs). Overlapping edits are set to Block in this project: message the holder with update_work, or start_work on other files."
        )
    );
    assert_eq!(answered(repo.join("src").join("b.rs")), "");
    let left: Vec<String> = std::fs::read_dir(&team.mailbox)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".ask") || name.ends_with(".answer"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A native teammate writes the relay mailbox itself, never the lead's part, where the
/// engine keeps the lead's letters. A claim it plants in the mailbox for the lead's letter,
/// as one `touch` from its shell does, is not the lead's session taking it: the engine still
/// holds the message for the lead and the lead's next tool call gets it, never Delivered
/// unseen (DEC-111 relay mailbox forgery, C1).
#[test]
fn a_teammate_claiming_the_leads_letter_never_marks_it_delivered_unseen() {
    let team = Team::new();
    let (db, project) = engine(&team, Nudges::Off);
    let lead = tools_agent(&db, "claude-code", false);
    let other = tools_agent(&db, "claude-code", false);
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent,pane)
         VALUES ('alpha','teammate','claude-code',1,'working',0,0,?1,?2)",
        params![lead, MATE_PANE],
    )
    .unwrap();
    let home = team.root.join("agent").join("home");
    std::fs::create_dir_all(&home).unwrap();
    let serve = || {
        crazyagent::coord::relay::serve(
            &db,
            &project,
            lead,
            &home,
            &team.mailbox,
            Layout::Split,
            false,
            now_ms(),
        )
        .unwrap()
    };
    let to_lead = crazyagent::coord::send(
        &db,
        crazyagent::coord::NewMessage {
            project: project.clone(),
            from: crazyagent::coord::Party::Agent(other),
            to: crazyagent::coord::Party::Agent(lead),
            kind: crazyagent::coord::MessageKind::Note,
            question: None,
            work: None,
            reply_to: None,
            body: "Stop the release, the owner found a bug.".to_owned(),
            idempotency_key: None,
        },
        now_ms(),
    )
    .unwrap();
    serve();

    let forged = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/touch")
            .arg(team.entry(&format!("{to_lead:032x}.shown")))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        forged.status.success(),
        "{}",
        String::from_utf8_lossy(&forged.stderr)
    );
    serve();
    assert_eq!(
        crazyagent::coord::message(&db, to_lead).unwrap().receipt,
        crazyagent::coord::Receipt::Sent,
        "a claim the lead's session never made leaves the message waiting for the lead"
    );
    serve();
    let seen = context(&team.hook(
        "post-tool",
        None,
        tool(LEAD_SESSION, None, "Read", json!({})),
    ));
    assert!(
        seen.ends_with(":\nStop the release, the owner found a bug."),
        "{seen}"
    );
}

/// Under the split layout the reports left in the mailbox itself count only for the
/// teammates' tags: tool calls, activity and a wait planted there under the lead's key
/// never reach the lead's row, while the same report in the lead's part does (DEC-111
/// relay mailbox forgery, C1).
#[test]
fn reports_planted_under_the_leads_key_outside_its_part_count_for_nobody() {
    let team = Team::new();
    let (db, project) = engine(&team, Nudges::Off);
    let lead = tools_agent(&db, "claude-code", false);
    let home = team.root.join("agent").join("home");
    std::fs::create_dir_all(&home).unwrap();
    let serve = || {
        crazyagent::coord::relay::serve(
            &db,
            &project,
            lead,
            &home,
            &team.mailbox,
            Layout::Split,
            false,
            now_ms(),
        )
        .unwrap()
    };
    let counted = |table: &str| -> i64 {
        db.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE agent=?1"),
            [lead],
            |row| row.get(0),
        )
        .unwrap()
    };
    for (suffix, text) in [
        (".tools", "Forged\n"),
        (".touch", ""),
        (".waiting", "permission"),
    ] {
        std::fs::write(team.entry(&format!("{LEAD_KEY}{suffix}")), text).unwrap();
    }
    serve();
    assert_eq!(
        (counted("coord_tool_use"), counted("coord_activity")),
        (0, 0),
        "nothing a teammate plants under the lead's key counts for the lead"
    );

    std::fs::write(team.lead_entry(&format!("{LEAD_KEY}.tools")), "Read\n").unwrap();
    serve();
    let tools: Vec<String> = db
        .prepare("SELECT tool FROM coord_tool_use WHERE agent=?1")
        .unwrap()
        .query_map([lead], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tools, ["Read"], "the lead's own report in its part counts");
}

/// What one nudges option does through the real chain: the engine's relay writes the
/// project's settings for the real hook binary, the lead's session edits a file twice with
/// its tool call logged between, the relay takes that log into the engine, and the
/// adoption tick runs. Inputs: the team and the option. Output: the hook's answers to the
/// two edits and the engine notes the lead's agent got.
fn nudged(team: &Team, nudges: Nudges) -> ([String; 2], Vec<String>) {
    let (db, project) = engine(team, nudges);
    let lead = tools_agent(&db, "claude-code", false);
    let home = team.root.join("agent-1").join("home");
    std::fs::create_dir_all(&home).unwrap();
    let serve = || {
        crazyagent::coord::relay::serve(
            &db,
            &project,
            lead,
            &home,
            &team.mailbox,
            Layout::Single,
            false,
            now_ms(),
        )
        .unwrap()
    };
    serve();
    let file = team.root.join("src").join("a.rs");
    let first = team.hook("edit", None, edit(LEAD_SESSION, &file));
    team.hook("post-tool", None, edit(LEAD_SESSION, &file));
    let second = team.hook("edit", None, edit(LEAD_SESSION, &file));
    serve();
    crazyagent::coord::adoption::tick(&db, &project, now_ms()).unwrap();
    ([first, second], engine_notes(&db, lead))
}

/// Opens the team's real engine database with the project `alpha`, the team, setup,
/// coordination and usage tables, and the chosen nudges stored. Inputs: the team and the
/// option. Output: the connection and the project.
fn engine(team: &Team, nudges: Nudges) -> (Connection, ProjectId) {
    let db = Connection::open(team.root.join("engine.db")).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); INSERT INTO projects VALUES ('alpha','{}');")
        .unwrap();
    crazyagent::team::initialize(&db).unwrap();
    crazyagent::project_setup::initialize(&db).unwrap();
    crazyagent::coord::ensure(&db).unwrap();
    crazyagent::usage::initialize(&db).unwrap();
    let project = ProjectId::new("alpha").unwrap();
    crazyagent::coord::settings::write(
        &db,
        &project,
        &CoordSettings {
            nudges,
            ..CoordSettings::default()
        },
    )
    .unwrap();
    (db, project)
}

/// Adds one working tools-on sub-agent of a harness to project `alpha`. Inputs: the
/// database, the harness id and whether its launch fires no hooks. Output: the agent's id.
fn tools_agent(db: &Connection, harness: &str, hookless: bool) -> i64 {
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,hookless)
         VALUES ('alpha','sub_agent',?1,1,'working',0,0,?2)",
        params![harness, hookless],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// The engine notes one agent got, oldest first. Inputs: the database and the agent.
/// Output: the note bodies.
fn engine_notes(db: &Connection, agent: i64) -> Vec<String> {
    let mut statement = db
        .prepare(
            "SELECT body FROM coord_messages WHERE from_kind='engine' AND to_kind='agent'
             AND to_agent=?1 ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([agent], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap()
}

#[test]
fn with_nudges_off_no_reminder_is_sent() {
    let team = Team::new();
    let (edits, notes) = nudged(&team, Nudges::Off);
    assert_eq!(edits, ["", ""], "no hook reminder on an edit");
    assert!(notes.is_empty(), "no engine reminder: {notes:?}");
}

#[test]
fn with_ten_minutes_the_adoption_reminder_is_sent() {
    let team = Team::new();
    let (edits, notes) = nudged(&team, Nudges::TenMinutes);
    assert_eq!(
        edits,
        ["", ""],
        "the 10-minute option never reminds on an edit"
    );
    assert_eq!(
        notes,
        [
            Flag::EditsWithoutWork.reminder(),
            Flag::Ungrounded.reminder()
        ],
        "the logged edit without start_work and without a graph query earns one reminder per flag"
    );
}

#[test]
fn with_first_edit_the_hook_returns_the_reminder_once() {
    let team = Team::new();
    let (edits, notes) = nudged(&team, Nudges::FirstEdit);
    let answer: Value = serde_json::from_str(edits[0].trim()).unwrap();
    assert_eq!(
        answer,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped.",
        }}),
        "a reminder with no permission decision, so the edit stays allowed"
    );
    assert_eq!(edits[1], "", "once per session");
    assert!(notes.is_empty(), "no 10-minute reminder: {notes:?}");

    let own = team.root.join(".worktrees").join("greet").join("a.rs");
    team.hook(
        "post-tool",
        Some(MATE_PANE),
        tool(
            MATE_SESSION,
            None,
            "Bash",
            json!({"command": "graphify query \"parser\""}),
        ),
    );
    assert_eq!(
        team.hook("edit", Some(MATE_PANE), edit(MATE_SESSION, &own)),
        "",
        "a session that queried the graph first gets no reminder"
    );
}

/// Under the split layout the engine keeps `relay.json` in the lead's part alone, and a
/// native teammate's hook, which never opens that part, still reads the project's nudges
/// from it (DEC-111 relay mailbox forgery, C1).
#[test]
fn a_native_teammate_reads_the_roster_the_engine_keeps_in_the_leads_part() {
    let team = Team::new();
    nudged(&team, Nudges::FirstEdit);
    assert!(
        team.lead_entry("relay.json").is_file() && !team.entry("relay.json").exists(),
        "the roster sits in the lead's part, where no teammate writes"
    );
    let own = team.root.join(".worktrees").join("greet").join("a.rs");
    let answer: Value = serde_json::from_str(
        team.hook("edit", Some(MATE_PANE), edit(MATE_SESSION, &own))
            .trim(),
    )
    .unwrap();
    assert_eq!(
        answer["hookSpecificOutput"]["additionalContext"],
        "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped.",
        "the teammate's hook read the FirstEdit nudges from the lead's part"
    );
}

/// Runs the FirstEdit engine fallback (DEC-111 R4) for two agents of one harness: one whose
/// real session record edits a file before any graphify call and one whose record queried
/// the graph first. The engine's usage scan of each real home and the adoption tick run
/// twice. Inputs: the team, the harness id, whether its launch fires no hooks, the record's
/// path inside a home, and the edit and graph-query lines. Output: the engine notes of the
/// editing agent and of the grounded one.
fn engine_fallback(
    team: &Team,
    harness: &str,
    hookless: bool,
    record: &str,
    edit: &Value,
    graph: &Value,
) -> [Vec<String>; 2] {
    let (db, project) = engine(team, Nudges::FirstEdit);
    let format = Format::of(harness).unwrap();
    let agents = [vec![edit], vec![graph, edit]].map(|lines| {
        let agent = tools_agent(&db, harness, hookless);
        let home = team
            .root
            .join("agents")
            .join(format!("agent-{agent}"))
            .join("home");
        let path = home.join(record);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&path, text).unwrap();
        (agent, home)
    });
    for _ in 0..2 {
        for (agent, home) in &agents {
            let who = Attribution {
                kind: RunKind::Agent,
                project: Some(project.clone()),
                pool: None,
                account: None,
                agent: format!("team-{agent}"),
                harness: harness.to_owned(),
            };
            crazyagent::usage::capture::scan_agent_home(&db, home, format, &who, *agent, now_ms())
                .unwrap();
        }
        crazyagent::coord::adoption::tick(&db, &project, now_ms()).unwrap();
    }
    agents.map(|(agent, _)| engine_notes(&db, agent))
}

/// Under FirstEdit an agent without hooks gets the same reminder from the engine (DEC-111
/// R4): a Codex agent's real rollout file records a patch before any graphify call, the
/// engine's usage scan of its home counts the calls, and the adoption tick sends one note,
/// however often both run. A Codex agent whose rollout ran graphify first gets none.
#[test]
fn with_first_edit_a_codex_agent_without_hooks_gets_the_engine_reminder_once() {
    let patch = json!({"type": "response_item", "payload": {
        "type": "custom_tool_call", "name": "apply_patch", "input": "*** Begin Patch",
    }});
    let graphify = json!({"type": "response_item", "payload": {
        "type": "function_call", "name": "exec_command",
        "arguments": json!({"cmd": "graphify query \"parser\""}).to_string(),
    }});
    let [edited, grounded] = engine_fallback(
        &Team::new(),
        "codex",
        false,
        "sessions/2026/11/01/rollout-2026-11-01T14-00-00-t.jsonl",
        &patch,
        &graphify,
    );
    assert_eq!(
        edited,
        [
            "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped."
        ],
        "one engine reminder, the hook's own words"
    );
    assert_eq!(
        grounded,
        Vec::<String>::new(),
        "a Codex agent that queried the graph first"
    );
}

/// Under FirstEdit a Claude Code agent launched with --bare, whose hooks never run (DEC-104
/// bare-hooks), gets the engine's reminder too (DEC-111 R4): its real transcript records an
/// Edit before any graphify call, the usage scan of its home counts the calls, and the
/// adoption tick sends one note, however often both run. One whose transcript ran graphify
/// first gets none, and a Claude Code agent with hooks is left to its hook.
#[test]
fn with_first_edit_a_bare_claude_agent_gets_the_engine_reminder_once() {
    let call = |id: &str, name: &str, input: Value| {
        json!({"type": "assistant", "message": {
            "id": id, "role": "assistant",
            "content": [{"type": "tool_use", "id": format!("toolu_{id}"), "name": name, "input": input}],
        }})
    };
    let edit = call(
        "msg_edit",
        "Edit",
        json!({"file_path": "/repo/src/a.rs", "old_string": "a", "new_string": "b"}),
    );
    let graphify = call(
        "msg_graph",
        "Bash",
        json!({"command": "graphify query \"parser\""}),
    );
    let record = "projects/-repo/0b6f1c52-5a8e-4f43-9a51-1f2d7c3e9a10.jsonl";
    let [edited, grounded] =
        engine_fallback(&Team::new(), "claude-code", true, record, &edit, &graphify);
    assert_eq!(
        edited,
        [
            "You are editing before any code-graph query. Run graphify query first if this touches code you have not mapped."
        ],
        "one engine reminder, the hook's own words"
    );
    assert_eq!(
        grounded,
        Vec::<String>::new(),
        "a bare Claude agent that queried the graph first"
    );
    let [hooked, _] = engine_fallback(&Team::new(), "claude-code", false, record, &edit, &graphify);
    assert_eq!(
        hooked,
        Vec::<String>::new(),
        "a Claude agent with hooks gets its reminder from the hook, not the engine"
    );
}

#[test]
fn the_monitor_prints_each_new_letter_for_its_session_once_and_exits_with_its_parent() {
    let team = Team::new();
    let mut parent = team.callers.spawn(
        LEAD_PANE,
        Command::new("/bin/sh")
            .arg("-c")
            .arg("\"$0\" coord-watch \"$1\" & exec sleep 60")
            .arg(env!("CARGO_BIN_EXE_crazyagent"))
            .arg(&team.mailbox)
            .env("CLAUDE_CONFIG_DIR", &team.config)
            .env_remove("TMUX_PANE")
            .env_remove("CLAUDE_PROJECT_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    let mut output = BufReader::new(parent.stdout.take().unwrap());
    let (lines, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        while output.read_line(&mut line).is_ok_and(|read| read > 0) {
            lines.send(std::mem::take(&mut line)).unwrap();
        }
        let mut rest = String::new();
        output.read_to_string(&mut rest).unwrap();
        rest
    });

    team.letter(5, "leader", "For the lead.\nSecond line.");
    team.letter(6, MATE_PANE, "For greet.");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(10)).unwrap(),
        "[crazyagent msg 5] from Ada (@lead-a): For the lead. Second line.\n"
    );
    team.letter(8, "leader", &"x".repeat(300));
    assert_eq!(
        received.recv_timeout(Duration::from_secs(10)).unwrap(),
        format!(
            "[crazyagent msg 8] from Ada (@lead-a): {}\n",
            "x".repeat(200)
        ),
        "each letter once, only the session's own, its body clipped to 200 bytes"
    );
    assert!(
        received.recv_timeout(Duration::from_secs(3)).is_err(),
        "nothing is announced twice"
    );

    parent.kill().unwrap();
    parent.wait().unwrap();
    let started = Instant::now();
    loop {
        match received.recv_timeout(Duration::from_millis(200)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(line) => panic!("unexpected line {line}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "the monitor exits once its parent is gone"
                );
            }
        }
    }
    assert_eq!(reader.join().unwrap(), "");
}
