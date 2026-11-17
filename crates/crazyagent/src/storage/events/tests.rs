use super::*;
use crate::protocol::{Command, ProjectId, ProjectSpec, TaskId};

/// An in-memory store holding the `state` row at `revision` and one event per cursor in
/// `logged`, each with its topic, as storage.rs creates and writes them. Inputs: the
/// revision and the events. Output: the connection.
fn store(revision: u64, logged: &[(u64, Topic)]) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);",
    )
    .unwrap();
    db.execute("INSERT INTO state VALUES (1,?1)", [revision as i64])
        .unwrap();
    for (cursor, topic) in logged {
        let event = Event {
            cursor: Cursor::new(*cursor),
            request_id: RequestId::new(format!("r{cursor}")).unwrap(),
            client_id: ClientId::new("desktop").unwrap(),
            topic: *topic,
        };
        db.execute(
            "INSERT INTO events VALUES (?1,?2)",
            params![*cursor as i64, serde_json::to_string(&event).unwrap()],
        )
        .unwrap();
    }
    db
}

/// A project creation. Input: the project id. Output: the command.
fn create(id: &str) -> Command {
    Command::CreateProject {
        project_id: ProjectId::new(id).unwrap(),
        spec: ProjectSpec {
            name: "Inventory".into(),
            repositories: vec!["/tmp/inventory".into()],
        },
    }
}

#[test]
fn each_command_names_the_topic_of_what_it_changes_and_a_read_names_none() {
    let task = TaskId::new("task-1").unwrap();
    for (command, topic) in [
        (create("p"), Topic::Projects),
        (
            Command::WorktreeLeases {
                task_id: task.clone(),
            },
            Topic::Worktrees,
        ),
        (Command::TaskContext { task_id: task }, Topic::Team),
        (Command::Snapshot, Topic::Unknown),
        (
            Command::Events {
                after: Cursor::new(0),
            },
            Topic::Unknown,
        ),
    ] {
        assert_eq!(command.topic(), topic, "{command:?}");
    }
    let stored: Event =
        serde_json::from_str(r#"{"cursor":"1","request_id":"r1","client_id":"desktop"}"#).unwrap();
    assert_eq!(
        stored.topic,
        Topic::Unknown,
        "an event stored before topics"
    );
}

#[test]
fn changes_names_the_sorted_topics_after_a_cursor_and_moves_it_to_the_revision() {
    let mut db = store(
        3,
        &[(1, Topic::Team), (2, Topic::Projects), (3, Topic::Team)],
    );
    assert_eq!(
        changes(&mut db, &Cursor::new(0)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(3),
            topics: vec![Topic::Projects, Topic::Team],
        }
    );
    assert_eq!(
        changes(&mut db, &Cursor::new(2)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(3),
            topics: vec![Topic::Team],
        }
    );
    assert_eq!(
        changes(&mut db, &Cursor::new(3)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(3),
            topics: Vec::new(),
        }
    );
}

#[test]
fn a_cursor_pruned_below_the_retained_events_or_ahead_of_the_store_is_refreshed() {
    let mut db = store(5, &[(4, Topic::Team), (5, Topic::Memory)]);
    for after in [0, 1, 2, 6] {
        assert_eq!(
            changes(&mut db, &Cursor::new(after)).unwrap(),
            Changes::Refresh {
                cursor: Cursor::new(5),
            },
            "after {after}"
        );
    }
    assert_eq!(
        changes(&mut db, &Cursor::new(3)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(5),
            topics: vec![Topic::Memory, Topic::Team],
        }
    );
}

#[test]
fn past_the_catch_up_bound_the_cursor_stops_at_the_last_event_read() {
    let logged: Vec<(u64, Topic)> = (1..=130).map(|cursor| (cursor, Topic::Ide)).collect();
    let mut db = store(130, &logged);
    assert_eq!(
        changes(&mut db, &Cursor::new(0)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(128),
            topics: vec![Topic::Ide],
        }
    );
    assert_eq!(
        changes(&mut db, &Cursor::new(128)).unwrap(),
        Changes::Logged {
            cursor: Cursor::new(130),
            topics: vec![Topic::Ide],
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_applied_request_logs_the_topic_its_command_names() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = crate::engine::Engine::open(&root.path().join("engine.db"), config)
        .await
        .unwrap();
    let request = |id: &str, revision: u64, command: Command| crate::protocol::WireRequest {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        client_id: ClientId::new("desktop").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    };
    engine
        .apply(request("create-1", 0, create("project-1")))
        .await
        .unwrap();
    let reply = engine
        .apply(request(
            "events-1",
            1,
            Command::Events {
                after: Cursor::new(0),
            },
        ))
        .await
        .unwrap();
    let Outcome::Events { events } = reply.outcome else {
        panic!("events required")
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].topic, Topic::Projects);
    engine.close().await.unwrap();
}
