use super::*;
use crate::ide::bridge::changes::tests::{Case, NOW};
use crate::project_setup::{StoredSetup, TeamPlan};

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

#[test]
fn ide_comment_reaches_the_agent_as_one_message_with_file_and_line() {
    let case = Case::new();
    with_team(&case);
    let set = case.only(NOW);
    comment(
        &case.db,
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
        "On Ada (@builder)'s changes \"Say one in Italian\", README line 1:\n\nSay uno once.\nKeep the rest."
    );
    for path in ["Cargo.toml", "../repo/README", ".git/config", "OLD"] {
        assert_eq!(
            comment(&case.db, &case.project, &set.id, path, 1, "Here")
                .unwrap_err()
                .0,
            "changes_not_found",
            "{path}"
        );
    }
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        comment(&case.db, &other, &set.id, "README", 1, "Here")
            .unwrap_err()
            .0,
        "changes_not_found"
    );
    assert_eq!(
        comment(&case.db, &case.project, &set.id, "README", 1, "Ring \u{7}")
            .unwrap_err()
            .0,
        "team_message_invalid"
    );
    assert_eq!(messages(&case).len(), 1);
}

#[test]
fn comment_on_a_closed_change_set_is_refused() {
    let accepted = Case::new();
    with_team(&accepted);
    let set = accepted.only(NOW);
    landing::accept(
        &accepted.db,
        &accepted.home,
        &set.id,
        &set.head,
        Some("Land it"),
    )
    .unwrap();
    assert_eq!(
        comment(
            &accepted.db,
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
    let set = discarded.only(NOW);
    landing::discard(&discarded.db, &set.id, &set.head, NOW).unwrap();
    assert_eq!(
        comment(
            &discarded.db,
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
fn a_comment_on_a_project_without_a_team_conversation_is_refused_and_sends_nothing() {
    let case = Case::new();
    crate::project_setup::initialize(&case.db).unwrap();
    crate::team::initialize(&case.db).unwrap();
    let set = case.only(NOW);
    assert_eq!(
        comment(&case.db, &case.project, &set.id, "README", 1, "Here")
            .unwrap_err()
            .0,
        "project_setup_missing"
    );
    assert!(messages(&case).is_empty());
}
