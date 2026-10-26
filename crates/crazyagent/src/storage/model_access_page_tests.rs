use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

async fn send_read(
    storage: &Storage,
    started: &Started,
    query: OwnerRead,
) -> Result<OwnerReadReply, EngineError> {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(
            started.owners[0].admission.clone(),
            query,
            send,
        ))
        .unwrap_or_else(|_| panic!("actual owner read queue unavailable"));
    receive.await.unwrap()
}

async fn page(
    storage: &Storage,
    started: &Started,
    collection: ModelAccessCollection,
    cursor: Option<ModelAccessCursor>,
) -> OwnerData {
    send_read(
        storage,
        started,
        OwnerRead::ModelAccessPage {
            collection,
            cursor,
            limit: 16,
        },
    )
    .await
    .unwrap()
    .data
}

async fn read_all(
    storage: &Storage,
    started: &Started,
    collection: ModelAccessCollection,
) -> (Vec<String>, usize) {
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let OwnerData::ModelAccessPage { items, next, .. } =
            page(storage, started, collection, cursor).await
        else {
            panic!("model-access page required");
        };
        pages += 1;
        assert!(!items.is_empty());
        seen.extend(items.iter().map(|item| item.id().as_str().to_owned()));
        let Some(after) = next else {
            return (seen, pages);
        };
        cursor = Some(after);
    }
}

fn catalog() -> String {
    let count = crate::config::ACCOUNTS_MAX;
    let accounts: Vec<String> = (0..count).map(|i| format!("\"account-{i:04}\"")).collect();
    let mut text = format!(
        "version = 1\n[model_access]\naccounts = [{}]\n",
        accounts.join(",")
    );
    for i in 0..count {
        text.push_str(&format!(
            "[[model_access.connections]]\nid = \"connection-{i:04}\"\nprovider = \"local-provider\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"account-{i:04}\"\nendpoint = {{ kind=\"http\", url=\"http://127.0.0.1:11434/v1\" }}\nmodel = {{ id=\"local-model-{i:04}\", author=\"local-author\" }}\nauth = {{ method=\"none\" }}\n"
        ));
    }
    text
}

#[tokio::test]
async fn a_model_access_collection_read_takes_one_settings_snapshot() {
    let root = tempfile::Builder::new()
        .prefix("ca-model-access-pages-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let folder = std::fs::canonicalize(root.path()).unwrap();
    let global = root.path().join("global.toml");
    let text = catalog();
    private_file(&global, text.as_bytes());
    let (storage, _config) = Storage::open_with_settings_sources(
        &root.path().join("engine.db"),
        &global,
        &root.path().join("project.toml"),
    )
    .await
    .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("actual owner start queue unavailable"));
    let started = receive.await.unwrap().unwrap();
    let mut measured = Vec::new();
    for collection in [
        ModelAccessCollection::Accounts,
        ModelAccessCollection::Connections,
    ] {
        let before = settings::source_reads(&folder);
        let clock = Instant::now();
        let (seen, pages) = read_all(&storage, &started, collection).await;
        let elapsed = clock.elapsed();
        let reads = settings::source_reads(&folder) - before;
        eprintln!(
            "model-access {collection:?}: {} entries, {pages} pages, {reads} settings file reads, {} ms, settings file {} bytes",
            seen.len(),
            elapsed.as_millis(),
            text.len()
        );
        assert_eq!(seen.len(), crate::config::ACCOUNTS_MAX);
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(seen, sorted);
        measured.push((collection, pages, reads, elapsed));
    }
    for (collection, pages, reads, elapsed) in &measured {
        assert!(
            *reads <= 3,
            "{collection:?}: {pages} pages read the settings files {reads} times; one collection read takes one snapshot of the lease, global and project files"
        );
        assert!(*elapsed < Duration::from_secs(60));
    }

    let OwnerData::ModelAccessPage {
        next: Some(cursor), ..
    } = page(&storage, &started, ModelAccessCollection::Accounts, None).await
    else {
        panic!("a full first page offers the next page");
    };
    let basis = send_read(&storage, &started, OwnerRead::Capabilities)
        .await
        .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(
            started.owners[0].admission.clone(),
            OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("create-paged-project").unwrap(),
                expected_revision: basis.revision,
                operation: OwnerOperation::Apply {
                    mutation: OwnerMutation::CreateProject {
                        project_id: ProjectId::new("paged-project").unwrap(),
                        spec: ProjectSpec {
                            name: "Paged project".into(),
                            repositories: vec![],
                        },
                    },
                },
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
    assert_eq!(receive.await.unwrap().unwrap().result, OwnerResult::Applied);
    assert!(matches!(
        page(
            &storage,
            &started,
            ModelAccessCollection::Accounts,
            Some(cursor)
        )
        .await,
        OwnerData::ResyncRequired
    ));

    private_file(&global, format!("timeout_ms = 1500\n{text}").as_bytes());
    let changed = send_read(
        &storage,
        &started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        },
    )
    .await;
    assert_eq!(
        changed.unwrap_err(),
        error("settings_source_conflict"),
        "a new collection read takes a new snapshot"
    );

    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStop(started.host.clone(), send))
        .unwrap_or_else(|_| panic!("actual owner stop queue unavailable"));
    receive.await.unwrap().unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("actual actor close queue unavailable"));
    receive.await.unwrap().unwrap();
}
