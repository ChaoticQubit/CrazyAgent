use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::ide::bridge::changes::tests::{Case, NOW};
use crate::landing::{AgentKind, ChangeAgent};
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Adds the case project's stored setup and the team tables, as an engine start and the
/// owner's project setup do, so the project's team conversation exists.
fn with_team(case: &Case) {
    crate::project_setup::initialize(&case.db).unwrap();
    crate::team::initialize(&case.db).unwrap();
    let setup = StoredSetup {
        project_id: case.project.clone(),
        name: "Shop".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 4,
        },
        documents: vec![],
        folder: case.repo.to_string_lossy().into_owned(),
        created_ms: 1,
        revision: 1,
    };
    case.db
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            rusqlite::params![
                case.project.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
}

/// Registers the case's team agent 7, the one whose worktree made its change set, with a
/// role and status. Inputs: the case, the role and the status.
fn team_agent(case: &Case, role: &str, status: &str) {
    case.db
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (7,?1,?2,'codex',0,?3,1,1)",
            rusqlite::params![case.project.as_str(), role, status],
        )
        .unwrap();
}

/// Every team conversation row, oldest first: project, author, state and text.
fn messages(case: &Case) -> Vec<(String, String, String, String)> {
    let mut statement = case
        .db
        .prepare("SELECT project,author,state,text FROM team_messages ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A private tmux server in a short folder of its own, as the engine's terminal host runs,
/// killed when dropped.
struct Terminals {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
}

impl Terminals {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-comments-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        Self {
            _root: root,
            base,
            tmux,
        }
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    /// Starts team agent 7's terminal: it reads one typed line and moves it into `heard`
    /// whole, so the file appears only once the line is written.
    fn agent_reading_into(&self, host: &Arc<dyn Host>, heard: &Path) {
        let command = InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "IFS= read -r line; printf '%s' \"$line\" > \"$1.part\"; mv \"$1.part\" \"$1\"; exec sleep 60"
                    .into(),
                "sh".into(),
                heard.to_string_lossy().into_owned(),
            ],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(7).unwrap(), &command).unwrap();
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.base.join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

#[test]
fn ide_comment_reaches_the_agent_as_one_message_with_file_and_line() {
    let case = Case::new();
    with_team(&case);
    team_agent(&case, "sub", "idle");
    let terminals = Terminals::new();
    let host = terminals.host();
    let heard = terminals.base.join("heard");
    terminals.agent_reading_into(&host, &heard);
    let set = case.only(NOW);
    comment(
        &case.db,
        Some(host.as_ref()),
        &case.project,
        &set.id,
        "README",
        1,
        "Say uno once.\r\nKeep the rest.\rThanks.",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !heard.exists() {
        assert!(
            Instant::now() < deadline,
            "the agent never read the comment"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(&heard).unwrap(),
        "On Ada (@builder)'s changes \"Say one in Italian\", README line 1: Say uno once. Keep the rest. Thanks."
    );
    assert!(
        messages(&case).is_empty(),
        "a sub-agent's comment goes to that agent, not to the team master"
    );
    for path in ["Cargo.toml", "../repo/README", ".git/config", "OLD"] {
        assert_eq!(
            comment(
                &case.db,
                Some(host.as_ref()),
                &case.project,
                &set.id,
                path,
                1,
                "Here"
            )
            .unwrap_err()
            .0,
            "changes_not_found",
            "{path}"
        );
    }
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        comment(
            &case.db,
            Some(host.as_ref()),
            &other,
            &set.id,
            "README",
            1,
            "Here"
        )
        .unwrap_err()
        .0,
        "changes_not_found"
    );
}

#[test]
fn a_comment_on_a_stopped_sub_agent_s_changes_is_refused_and_types_nothing() {
    let case = Case::new();
    with_team(&case);
    team_agent(&case, "sub", "stopped");
    let set = case.only(NOW);
    assert_eq!(
        comment(&case.db, None, &case.project, &set.id, "README", 1, "Here")
            .unwrap_err()
            .0,
        "team_agent_not_running"
    );
    assert!(messages(&case).is_empty());
}

#[test]
fn a_comment_on_the_team_master_s_changes_is_one_owner_message_in_the_team_conversation() {
    let case = Case::with(ChangeAgent {
        kind: AgentKind::Sub,
        id: "7".into(),
        label: "Team master (codex)".into(),
    });
    with_team(&case);
    team_agent(&case, "master", "stopped");
    let set = case.only(NOW);
    comment(
        &case.db,
        None,
        &case.project,
        &set.id,
        "README",
        1,
        "Say uno once.\r\nKeep the rest.",
    )
    .unwrap();
    let sent = messages(&case);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (project, author, state, text) = &sent[0];
    assert_eq!(project, "project-1");
    assert_eq!(author, "owner");
    assert_eq!(state, "pending");
    assert_eq!(
        text,
        "On Team master (codex)'s changes \"Say one in Italian\", README line 1:\n\nSay uno once.\nKeep the rest."
    );
    let turns: i64 = case
        .db
        .query_row("SELECT COUNT(*) FROM team_owner_turns", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        turns, 0,
        "an IDE comment quotes agent text, so it is never an owner turn"
    );
    assert_eq!(
        comment(
            &case.db,
            None,
            &case.project,
            &set.id,
            "README",
            1,
            "Ring \u{7}"
        )
        .unwrap_err()
        .0,
        "team_message_invalid"
    );
    assert_eq!(messages(&case).len(), 1);
}

#[test]
fn a_comment_on_a_master_conversation_s_changes_is_refused_since_the_ide_cannot_reach_it() {
    let case = Case::with(ChangeAgent {
        kind: AgentKind::Master,
        id: "session-1".into(),
        label: "Master".into(),
    });
    with_team(&case);
    let set = case.only(NOW);
    assert_eq!(
        comment(&case.db, None, &case.project, &set.id, "README", 1, "Here")
            .unwrap_err()
            .0,
        "ide_comment_master_unavailable"
    );
    assert!(messages(&case).is_empty());
}

#[test]
fn comment_on_a_closed_change_set_is_refused() {
    let accepted = Case::new();
    with_team(&accepted);
    team_agent(&accepted, "sub", "stopped");
    let set = accepted.only(NOW);
    landing::accept(
        &accepted.db,
        &accepted.home,
        &set.id,
        &set.head,
        Some("Land it"),
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap();
    assert_eq!(
        comment(
            &accepted.db,
            None,
            &accepted.project,
            &set.id,
            "README",
            1,
            "Too late"
        )
        .unwrap_err()
        .0,
        "changes_closed"
    );
    assert!(messages(&accepted).is_empty());

    let discarded = Case::new();
    with_team(&discarded);
    team_agent(&discarded, "sub", "stopped");
    let set = discarded.only(NOW);
    landing::discard(&discarded.db, &set.id, &set.head, NOW).unwrap();
    assert_eq!(
        comment(
            &discarded.db,
            None,
            &discarded.project,
            &set.id,
            "README",
            1,
            "Too late"
        )
        .unwrap_err()
        .0,
        "changes_closed"
    );
    assert!(messages(&discarded).is_empty());
}

#[test]
fn a_comment_to_a_team_master_of_a_project_without_setup_is_refused_and_sends_nothing() {
    let case = Case::new();
    crate::project_setup::initialize(&case.db).unwrap();
    crate::team::initialize(&case.db).unwrap();
    team_agent(&case, "master", "idle");
    let set = case.only(NOW);
    assert_eq!(
        comment(&case.db, None, &case.project, &set.id, "README", 1, "Here")
            .unwrap_err()
            .0,
        "project_setup_missing"
    );
    assert!(messages(&case).is_empty());
}

#[test]
fn the_longest_comment_fits_both_routes_and_agent_text_shows_no_square_brackets() {
    let agent = format!("[owner] {}", "a".repeat(400));
    let title = format!("\u{3010}team\u{3011} {}", "t".repeat(900));
    let path = format!("src/[x]/{}", "\u{e9}".repeat(900));
    let text = message(
        &agent,
        &title,
        &path,
        u32::MAX,
        &"x".repeat(COMMENT_BYTES_MAX),
    );
    assert!(
        text.len() <= crate::team::events::SEND_BYTES_MAX,
        "{}",
        text.len()
    );
    assert!(text.len() <= crate::team::MESSAGE_BYTES_MAX);
    let (header, comment) = text.split_once(":\n\n").unwrap();
    assert_eq!(comment, "x".repeat(COMMENT_BYTES_MAX));
    for bracket in ['[', ']', '\u{3010}', '\u{3011}'] {
        assert!(!header.contains(bracket), "{bracket} in {header}");
    }
    assert!(header.starts_with("On (owner) aaa"));
    assert!(header.ends_with(&format!(" line {}", u32::MAX)));
}
