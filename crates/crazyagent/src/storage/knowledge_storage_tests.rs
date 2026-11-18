use super::*;
use crate::memory::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn bounded_sqlite_full_rolls_back_knowledge_and_operation_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("db");
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    crate::state_log::fence::host_started(&started.host);
    let mut revision = 0;
    let mut sequence = 0;
    let project_id = ProjectId::new("p").unwrap();
    let task_id = TaskId::new("t").unwrap();
    let mut call = |db: &mut Connection, command| {
        sequence += 1;
        let reply = apply(
            db,
            &config,
            &path,
            WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(format!("op-{sequence}")).unwrap(),
                expected_revision: Cursor::new(revision),
                command,
            },
        );
        if let Ok(reply) = &reply {
            revision = reply.revision.value();
        }
        reply
    };
    assert_eq!(
        call(
            &mut db,
            Command::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: "p".into(),
                    repositories: vec![]
                }
            }
        )
        .unwrap()
        .outcome,
        Outcome::Applied
    );
    assert_eq!(
        call(
            &mut db,
            Command::PinTask {
                context: ExecutionContext {
                    project_id: project_id.clone(),
                    task_id: task_id.clone(),
                    config_id: config.id.clone(),
                    snapshot_id: SnapshotId::new("s").unwrap(),
                    baseline_id: BaselineId::new("b").unwrap(),
                    allowed_destinations: vec![],
                    budget_ref: CredentialRef::new("budget").unwrap(),
                }
            }
        )
        .unwrap()
        .outcome,
        Outcome::Applied
    );
    assert_eq!(
        call(
            &mut db,
            Command::Knowledge {
                action: KnowledgeAction::Admit {
                    principal: Principal {
                        id: RequestId::new("writer").unwrap(),
                        project_id,
                        task_id,
                        session_id: SessionId::new("session").unwrap(),
                        role: KnowledgeRole::Contributor,
                    }
                }
            }
        )
        .unwrap()
        .outcome,
        Outcome::Applied
    );
    let pages: i64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    let bounded: i64 = db
        .query_row(&format!("PRAGMA max_page_count={pages}"), [], |r| r.get(0))
        .unwrap();
    assert_eq!(pages, bounded);
    let before: (i64, i64) = db
        .query_row(
            "SELECT revision,(SELECT COUNT(*) FROM operations) FROM state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let result = call(
        &mut db,
        Command::Knowledge {
            action: KnowledgeAction::Capture {
                principal_id: RequestId::new("writer").unwrap(),
                activity: ActivityInput {
                    id: RequestId::new("full").unwrap(),
                    source_id: RequestId::new("source").unwrap(),
                    category: CaptureCategory::ToolOutput,
                    text: "x".repeat(config.settings.knowledge.excerpt_bytes_max),
                    gaps: vec![],
                },
            },
        },
    );
    assert!(result.unwrap_err().0.contains("database or disk is full"));
    let after: (i64, i64) = db
        .query_row(
            "SELECT revision,(SELECT COUNT(*) FROM operations) FROM state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM knowledge_activity", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM knowledge_sources", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(db.is_autocommit());
}
