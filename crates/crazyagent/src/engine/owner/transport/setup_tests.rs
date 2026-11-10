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

#[cfg(unix)]
fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

#[cfg(unix)]
fn assert_applied(reply: &OwnerReply) {
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("not applied: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied, "{reply:?}");
}

/// A Save whose reply was lost has committed with no hook behind it, so the client's
/// Reconcile, answered Committed, is what ends the project's open IDE. Its reply names no
/// project, so every open IDE is compared with its saved policy (v21-ide-level-3 r1 F1).
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconcile_of_a_save_whose_reply_was_lost_stops_the_projects_running_ide() {
    use crate::sandbox::policy::{AgentAccess, AgentPolicy};
    let owner = Usage::start().await;
    let project = ProjectId::new("lost-reply").unwrap();
    let revision = || {
        let query = OwnerRead::Capabilities;
        let request = owner_request("lost-reply-read", 0, OwnerOperation::Read { query });
        let client = &owner.client;
        async move {
            let reply = client.request(request).await.unwrap();
            let OwnerResponse::Read { state } = reply.response else {
                panic!("the owner read failed: {reply:?}");
            };
            state.revision.value()
        }
    };
    let created = owner
        .client
        .request(owner_request(
            "lost-reply-project",
            revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: project.clone(),
                    spec: ProjectSpec {
                        name: "lost reply".into(),
                        repositories: vec![],
                    },
                },
            },
        ))
        .await
        .unwrap();
    assert_applied(&created);
    let checkout = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
    let runtime = owner.runtime();
    let servers = runtime.ide_servers();
    servers
        .open(
            &project,
            &std::fs::canonicalize(checkout.path()).unwrap(),
            &crate::ide::server::tests::cached_build(),
        )
        .await
        .unwrap();
    let group = servers.group(&project).expect("the IDE runs");
    let save = owner_request(
        "lost-reply-save",
        revision().await,
        OwnerOperation::Apply {
            mutation: OwnerMutation::SetProjectAgentPolicy {
                project_id: project.clone(),
                version: 0,
                policy: AgentPolicy {
                    access: AgentAccess::Full,
                    ..AgentPolicy::default()
                },
            },
        },
    );
    let admission = runtime.authority.authenticate(&owner.packet()).unwrap();
    let committed = {
        let _barrier = runtime.engine.owner_egress.write().await;
        runtime
            .engine
            .dispatch_owner(admission, save.clone(), None)
            .await
            .unwrap()
    };
    assert_applied(&committed);
    assert_eq!(
        servers.group(&project),
        Some(group),
        "nothing has followed the commit yet"
    );
    let reconciled = owner
        .client
        .request(owner_request(
            "lost-reply-reconcile",
            revision().await,
            OwnerOperation::Reconcile {
                operation_id: save.request_id.clone(),
                expected_hash: Some(save.mutation_hash().unwrap()),
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Committed { operation } = &reconciled.response else {
        panic!("the Reconcile did not find the Save: {reconciled:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    crate::ide::server::tests::gone(group).await;
    assert_eq!(servers.group(&project), None);
    owner.stop().await;
}

#[cfg(unix)]
fn pack_checkout(setup: &std::path::Path, id: &str) -> std::path::PathBuf {
    let manifest = crate::setup::pack::default_pack().unwrap();
    let entry = manifest
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .unwrap();
    let crate::setup::pack::PackSource::Git { repo, commit, .. } = &entry.source else {
        panic!("{id} is a git entry");
    };
    let checkout = crate::setup::fetch::checkout_folder(setup, repo, commit);
    std::fs::create_dir_all(checkout.join("hooks")).unwrap();
    std::fs::create_dir_all(checkout.join(".claude-plugin")).unwrap();
    std::fs::write(
        checkout.join(".claude-plugin/plugin.json"),
        format!("{{\"name\":\"{id}\"}}"),
    )
    .unwrap();
    std::fs::write(
        checkout.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh \"${CLAUDE_PLUGIN_ROOT}/hooks/start.sh\""}]}]}}"#,
    )
    .unwrap();
    std::fs::write(checkout.join("hooks/start.sh"), "exit 0\n").unwrap();
    checkout
}

#[cfg(unix)]
fn stored_hooks(root: &std::path::Path) -> Vec<String> {
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let mut names: Vec<String> = db
        .prepare("SELECT name FROM plugin_items WHERE kind='hook'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    names.sort();
    names
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_backfill_seeds_hooks_from_cached_checkouts_of_installed_entries_once() {
    let owner = Usage::start().await;
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("caveman");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: caveman\ndescription: The caveman skill.\n---\n",
    )
    .unwrap();
    let sender = owner.runtime().engine.storage.sender.clone();
    let (send, installed) = oneshot::channel();
    sender
        .send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Install {
                id: "caveman".into(),
                source: "https://example.invalid/caveman@1".into(),
                skills: vec![source],
                server: None,
            },
            send,
        ))
        .await
        .unwrap();
    installed.await.unwrap().unwrap();
    let setup = owner.engine_root.join("setup");
    pack_checkout(&setup, "caveman");
    pack_checkout(&setup, "superpowers");

    setup::backfill_hooks(sender.clone(), owner.engine_root.clone()).await;
    let seeded = stored_hooks(&owner.engine_root);
    assert_eq!(seeded.len(), 1, "{seeded:?}");
    assert_eq!(seeded, ["caveman/SessionStart/1"]);
    setup::backfill_hooks(sender, owner.engine_root.clone()).await;
    assert_eq!(
        stored_hooks(&owner.engine_root),
        seeded,
        "a second start adds nothing, and an entry not installed gets no hooks"
    );
    owner.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_backfill_makes_no_setup_folder_before_setup_and_seeds_the_shipped_hook_after() {
    let owner = Usage::start().await;
    let sender = owner.runtime().engine.storage.sender.clone();
    let setup = owner.engine_root.join("setup");
    setup::backfill_hooks(sender.clone(), owner.engine_root.clone()).await;
    assert!(
        std::fs::symlink_metadata(&setup).is_err(),
        "an engine whose owner never ran setup gets no setup folder"
    );
    assert!(stored_hooks(&owner.engine_root).is_empty());

    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("graphify");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: graphify\ndescription: The graphify skill.\n---\n",
    )
    .unwrap();
    let (send, installed) = oneshot::channel();
    sender
        .send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Install {
                id: "graphify".into(),
                source: "https://example.invalid/graphify@1".into(),
                skills: vec![source],
                server: None,
            },
            send,
        ))
        .await
        .unwrap();
    installed.await.unwrap().unwrap();
    crate::setup::bundled::extract(&setup).unwrap();

    setup::backfill_hooks(sender, owner.engine_root.clone()).await;
    assert!(setup.join("bundled-hooks/hooks/graphify").is_dir());
    assert_eq!(
        stored_hooks(&owner.engine_root),
        ["graphify/PreToolUse/1", "graphify/PreToolUse/2"],
        "an installed entry still gets the hook this build ships"
    );
    owner.stop().await;
}
