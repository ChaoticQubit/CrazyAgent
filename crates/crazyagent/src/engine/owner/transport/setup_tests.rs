use super::usage_tests::Usage;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pack_removal_that_does_not_commit_puts_its_skill_folders_back() {
    let owner = Usage::start().await;
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("alpha");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: alpha\ndescription: The alpha skill.\n---\n",
    )
    .unwrap();
    let (send, installed) = oneshot::channel();
    owner
        .runtime()
        .engine
        .storage
        .sender
        .send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Install {
                id: "superpowers".into(),
                source: "https://example.invalid/pack@1".into(),
                skills: vec![source],
                server: None,
            },
            send,
        ))
        .await
        .unwrap();
    installed.await.unwrap().unwrap();
    let store = owner
        .engine_root
        .join(crate::storage::plugins::STORE)
        .join("skills")
        .join("global");
    let stored: Vec<_> = std::fs::read_dir(&store)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].join("SKILL.md").is_file());
    let exhausted = rusqlite::Connection::open(owner.engine_root.join("engine.db")).unwrap();
    exhausted
        .execute("UPDATE state SET revision=?1 WHERE id=1", [i64::MAX])
        .unwrap();
    drop(exhausted);

    let reply = owner
        .client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("remove-superpowers").unwrap(),
            expected_revision: Cursor::new(i64::MAX as u64),
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::RemovePackEntry {
                    id: "superpowers".into(),
                    version: 0,
                },
            },
        })
        .await;

    assert!(!matches!(
        reply,
        Ok(OwnerReply {
            response: OwnerResponse::Applied { .. },
            ..
        })
    ));
    let left: Vec<_> = std::fs::read_dir(&store)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(left, stored);
    assert!(stored[0].join("SKILL.md").is_file());
    owner.stop().await;
}
