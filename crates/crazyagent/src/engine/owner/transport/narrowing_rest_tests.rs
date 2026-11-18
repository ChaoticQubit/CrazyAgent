use super::barrier_tests::{database, worker_died};
use super::secret_storage_wait_tests::{holders, none_hold};
use super::usage_tests::{IN_FLIGHT, Usage};
use super::*;
use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::engine::owner::AccountSecret;
use crate::plugins::{Scope, ServerSpec};

const LAPSE_WAIT_MS: u64 = 250;
const PATIENT_WAIT_MS: u64 = 30_000;
const LANDED_WAIT: Duration = Duration::from_secs(10);
const SECRET_NAME: &str = "API_TOKEN";

/// Adds one API-key account on a local URL through the owner client, the way an owner adds
/// one, outside the model-access catalog. Inputs: the engine and a name for its key.
/// Output: the account id, its key text, and the listener its URL names.
async fn key_account(
    owner: &Usage,
    name: &str,
) -> (CredentialRef, String, tokio::net::TcpListener) {
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let key = format!("sk-{name}-{}", std::process::id());
    let added = owner
        .client
        .add_account(
            AccountDraft {
                provider: Provider::OpenaiCompatible,
                method: SignInMethod::Url,
                name: "Lab server".into(),
                url: Some(url),
                model: Some("lab-model".into()),
            },
            Some(AccountSecret::new(key.as_bytes().to_vec()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(holders(&engine_root(owner.root.path()), key.as_bytes()), 1);
    (added.id, key, listener)
}

/// Adds one global MCP server with one secret through the owner client. Inputs: the engine
/// and a name for the server and its secret. Output: the server id and the secret text.
async fn secret_server(owner: &Usage, name: &str) -> (String, String) {
    let server = owner
        .client
        .add_mcp_server(
            Scope::Global,
            name.into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let value = format!("secret-{name}-{}", std::process::id());
    owner
        .client
        .set_mcp_secret(
            server.id.clone(),
            SECRET_NAME.into(),
            AccountSecret::new(value.as_bytes().to_vec()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(holders(&engine_root(owner.root.path()), value.as_bytes()), 1);
    (server.id, value)
}

/// The engine folder of a test root, as the engine canonicalizes it, so file scans and
/// database reads look where the engine wrote.
/// Input: the root. Output: the folder that holds engine.db and the private stores.
fn engine_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap()
}

/// Opens engine.db read-only through a connection of its own, so a row reads what storage
/// committed without going through the engine.
/// Input: the engine folder. Output: the connection.
fn stored(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        root.join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

/// Counts the stored account rows with this id, which is 0 once storage committed its
/// removal.
/// Inputs: the engine folder and the id. Output: the count, 0 or 1.
fn account_rows(root: &Path, id: &CredentialRef) -> i64 {
    stored(root)
        .query_row(
            "SELECT COUNT(*) FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// Names the secrets the stored record of one MCP server keeps, which lacks a name once
/// storage committed its detach.
/// Inputs: the engine folder and the server id. Output: the secret names, in stored order.
fn stored_secret_names(root: &Path, id: &str) -> Vec<String> {
    let body: String = stored(root)
        .query_row(
            "SELECT body FROM plugin_items WHERE id=?1 AND kind='server'",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let record: serde_json::Value = serde_json::from_str(&body).unwrap();
    record["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|secret| secret["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Waits until a stored condition holds, polling every 20 ms, for a change the drainer or
/// a late worker lands while the engine runs.
/// Input: the condition. Output: whether it held within `LANDED_WAIT`.
async fn landed(mut done: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + LANDED_WAIT;
    while !done() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// Takes the one queue slot of an engine started with queue_capacity 1 with a sweep the
/// held worker cannot claim. Input: the engine. Output: the sweep's submit, which ends once
/// the worker takes it.
fn fill_queue(owner: &Usage) -> tokio::task::JoinHandle<Result<(), EngineError>> {
    let engine = owner.runtime().engine.clone();
    tokio::spawn(async move {
        let (send, _answer) = oneshot::channel();
        engine
            .storage
            .submit(
                crate::storage::Job::Setup(crate::storage::setup::Command::Sweep, send),
                tokio::time::Instant::now() + Duration::from_millis(PATIENT_WAIT_MS),
                "owner_queue_not_submitted",
            )
            .await
            .map(|_| ())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removal_the_full_queue_refuses_is_pending_and_lands_while_the_engine_runs() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let (id, key, _listener) = key_account(&owner, "remove-queue").await;
    let root = engine_root(owner.root.path());
    let hold = crate::storage::hold_claims(&database(&owner));
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner.client.remove_account(id.clone()).await;
    drop(hold);
    filler.await.unwrap().unwrap();
    assert!(
        landed(|| account_rows(&root, &id) == 0).await,
        "a removal the full queue refused lands while the engine runs: {answer:?}"
    );
    assert!(
        none_hold(&root, key.as_bytes()).await,
        "the removed account's key leaves the disk"
    );
    assert_eq!(
        answer,
        Err(failure(crate::storage::ACCESS_CHANGE_PENDING)),
        "a kept removal is pending, never 'nothing changed'"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removal_pending_when_the_engine_dies_lands_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, key, _listener) = key_account(&owner, "remove-die").await;
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    let answer = owner.client.remove_account(id.clone()).await;
    crate::storage::die_at_claim(&path);
    drop(hold);
    assert!(
        worker_died(&path).await,
        "the storage worker dies at its claim"
    );
    let restarted = owner.restarted().await;
    let root = engine_root(restarted.root.path());
    assert_eq!(
        account_rows(&root, &id),
        0,
        "the next open removes the account the dead engine never removed: {answer:?}"
    );
    assert!(
        none_hold(&root, key.as_bytes()).await,
        "the removed account's key leaves the disk"
    );
    assert_eq!(
        answer,
        Err(failure(crate::storage::ACCESS_CHANGE_PENDING)),
        "the removal outlasts the owner wait and is pending"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_detach_the_full_queue_refuses_is_pending_and_drains_and_deletes_the_file() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let (id, value) = secret_server(&owner, "detach-queue").await;
    let root = engine_root(owner.root.path());
    let hold = crate::storage::hold_claims(&database(&owner));
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner
        .client
        .remove_mcp_secret(id.clone(), SECRET_NAME.into())
        .await
        .map(|_| ());
    drop(hold);
    filler.await.unwrap().unwrap();
    assert!(
        landed(|| stored_secret_names(&root, &id).is_empty()).await,
        "a detach the full queue refused lands while the engine runs: {answer:?}"
    );
    assert!(
        none_hold(&root, value.as_bytes()).await,
        "the detached secret's file leaves the disk"
    );
    assert_eq!(
        answer,
        Err(failure(crate::storage::ACCESS_CHANGE_PENDING)),
        "a kept detach is pending, never 'nothing changed'"
    );
    owner.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_detach_pending_when_the_engine_dies_lands_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, value) = secret_server(&owner, "detach-die").await;
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    let answer = owner
        .client
        .remove_mcp_secret(id.clone(), SECRET_NAME.into())
        .await
        .map(|_| ());
    crate::storage::die_at_claim(&path);
    drop(hold);
    assert!(
        worker_died(&path).await,
        "the storage worker dies at its claim"
    );
    let restarted = owner.restarted().await;
    let root = engine_root(restarted.root.path());
    assert!(
        stored_secret_names(&root, &id).is_empty(),
        "the next open detaches the secret the dead engine never detached: {answer:?}"
    );
    assert!(
        none_hold(&root, value.as_bytes()).await,
        "the detached secret's file leaves the disk"
    );
    assert_eq!(
        answer,
        Err(failure(crate::storage::ACCESS_CHANGE_PENDING)),
        "the detach outlasts the owner wait and is pending"
    );
    restarted.stop().await;
}
