use super::barrier_tests::{database, revoke_settled, worker_died};
use super::narrowing::hold_drain;
use super::secret_storage_wait_tests::{holders, none_hold};
use super::usage_tests::{IN_FLIGHT, Usage};
use super::*;
use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::engine::owner::{
    AccountSecret, ModelAccessCollection, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerRequest, OwnerResponse, OwnerResult,
};
use crate::plugins::{Scope, ServerSpec};
use crate::storage::narrowing::{Change, IntentId, Intents};

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
    let root = engine_root(owner.root.path());
    assert_eq!(holders(&root, key.as_bytes()), 1);
    assert_eq!(account_rows(&database(owner), &added.id), 1);
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
    assert_eq!(
        holders(&engine_root(owner.root.path()), value.as_bytes()),
        1
    );
    assert_eq!(
        stored_secret_names(&database(owner), &server.id),
        [SECRET_NAME]
    );
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
/// Input: the database path. Output: the connection.
fn stored(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// Counts the stored account rows with this id, which is 0 once storage committed its
/// removal.
/// Inputs: the database path and the id. Output: the count, 0 or 1.
fn account_rows(path: &Path, id: &CredentialRef) -> i64 {
    stored(path)
        .query_row(
            "SELECT COUNT(*) FROM accounts WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// Names the secrets the stored record of one MCP server keeps, which lacks a name once
/// storage committed its detach.
/// Inputs: the database path and the server id. Output: the secret names, in stored order.
fn stored_secret_names(path: &Path, id: &str) -> Vec<String> {
    let body: String = stored(path)
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
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner.client.remove_account(id.clone()).await;
    drop(hold);
    filler.await.unwrap().unwrap();
    assert!(
        landed(|| account_rows(&path, &id) == 0).await,
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
    eprintln!(
        "diag full_queue_removal drain_idle={}",
        owner.runtime().narrowing_drain_idle()
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
        account_rows(&path, &id),
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
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
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
        landed(|| stored_secret_names(&path, &id).is_empty()).await,
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
        stored_secret_names(&path, &id).is_empty(),
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

/// Puts an account in a pool through an owner apply, so agents may use it and its connection
/// joins the model-access catalog, which storage counts as held. Inputs: the engine and the
/// account id. Output: none; the settings files hold the account's connection afterwards.
async fn into_catalog(owner: &Usage, id: &CredentialRef) {
    let applied = catalog_apply(owner, id).await;
    let OwnerResponse::Applied { operation } = applied else {
        panic!("the catalog apply commits: {applied:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(in_settings(owner.root.path(), id));
    assert!(held_in_catalog(owner.root.path(), id));
}

/// Asks the engine, as the owner, to put an account in a new pool, which adds its connection
/// to the model-access catalog. Inputs: the engine and the account id. Output: the answer.
async fn catalog_apply(owner: &Usage, id: &CredentialRef) -> OwnerResponse {
    let read = owner
        .client
        .request(OwnerRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new("rest-catalog-read").unwrap(),
            expected_revision: crate::protocol::Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 16,
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = read.response else {
        panic!("the model-access page reads");
    };
    let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
        panic!("the model-access page names its source hash");
    };
    let applied = owner
        .client
        .request(OwnerRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(format!("rest-hold-{}", id.as_str())).unwrap(),
            expected_revision: state.revision,
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                    edit: crate::config::ModelAccessEdit::CreatePool {
                        pool: crate::config::AccountPool {
                            id: CredentialRef::new("rest-held-pool").unwrap(),
                            name: None,
                            members: vec![id.clone()],
                            projects: vec![],
                            roles: vec![],
                            fallback_allowed: false,
                            provider_quota_policy: Default::default(),
                        },
                    },
                },
            },
        })
        .await
        .unwrap();
    applied.response
}

/// Whether the global settings file names an account, which it does while the account is in
/// the model-access catalog. Inputs: the test root and the account id. Output: the verdict.
fn in_settings(root: &Path, id: &CredentialRef) -> bool {
    std::fs::read_to_string(root.join("global.toml"))
        .unwrap()
        .contains(id.as_str())
}

/// Whether the settings files give an account a connection in the model-access catalog,
/// the one test storage uses to count it as held. A missing project file reads as empty.
/// Inputs: the test root and the account id. Output: the verdict.
fn held_in_catalog(root: &Path, id: &CredentialRef) -> bool {
    let read = |name: &str| match std::fs::read_to_string(root.join(name)) {
        Ok(text) => text,
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(unread) => panic!("the settings file {name} reads: {unread}"),
    };
    crate::config::resolve_config_bytes(&read("global.toml"), &read("project.toml"))
        .unwrap()
        .settings
        .model_access
        .is_some_and(|catalog| catalog.connections.iter().any(|c| c.id == *id))
}

/// The owner admission of the TUI client, as the transport builds it from its packet.
/// Input: the engine. Output: the admission.
fn tui_admission(owner: &Usage) -> crate::engine::owner::Admission {
    owner
        .runtime()
        .authority
        .authenticate(&owner.packet())
        .unwrap()
}

/// The key reference of an account that uses a key, read through storage the way a key
/// replace reads it first. Inputs: the engine and the account id. Output: the reference.
async fn key_reference(owner: &Usage, id: &CredentialRef) -> CredentialRef {
    let admission = tui_admission(owner);
    let Ok(crate::storage::accounts::ResultValue::Account(listed)) = owner
        .runtime()
        .engine
        .native_auth_job(|reply| {
            crate::storage::Job::Accounts(
                admission,
                crate::storage::accounts::Command::Get(id.clone()),
                reply,
            )
        })
        .await
    else {
        panic!("storage lists the added account");
    };
    let crate::config::ModelAuthentication::ApiKey { credential_ref } = listed.connection.auth
    else {
        panic!("the added account keeps an API key");
    };
    credential_ref
}

/// Lands a key replace's storage job in an engine that dies at that job's claim, so storage
/// never hears of the new key. Inputs: the engine, the account, its key reference and the
/// kept intent. Output: none once the worker stopped.
async fn key_replaced_lost(
    owner: &Usage,
    id: &CredentialRef,
    credential: &CredentialRef,
    intent: Option<IntentId>,
) {
    let path = database(owner);
    crate::storage::die_at_claim(&path);
    let lost = super::accounts::key_replaced(
        &owner.runtime().engine,
        &tui_admission(owner),
        id.clone(),
        credential.clone(),
        intent.map(crate::storage::narrowing::NarrowingAsk::Owner),
    )
    .await;
    assert!(
        lost.is_err(),
        "a job the dying worker never ran is not committed"
    );
    assert!(
        worker_died(&path).await,
        "the storage worker dies at its claim"
    );
}

/// Names the intents kept on disk for an engine folder. Input: the folder.
/// Output: the intent names, in the order they were written.
fn kept(root: &Path) -> Vec<IntentId> {
    Intents::open(&engine_root(root))
        .unwrap()
        .expect("unix keeps an intent store")
        .list()
        .unwrap()
        .intents
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Waits until the drainer of an engine landed or answered every queued change.
/// Input: the engine. Output: whether it did within `LANDED_WAIT`.
async fn drained(owner: &Usage) -> bool {
    landed(|| owner.runtime().narrowing_drain_idle()).await
}

/// Counts the settings publications storage journaled and has not finished.
/// Input: the database path. Output: 0 or 1.
fn journaled(path: &Path) -> i64 {
    stored(path)
        .query_row("SELECT COUNT(*) FROM settings_publication", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Makes every settings publication stop after its journal commits and before its file is
/// renamed, the persisted state of an engine that dies there, by failing the journal update
/// the rename step writes first. Input: the database path. Output: none.
fn stop_publications(path: &Path) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER rest_publication_stop BEFORE UPDATE ON settings_publication \
             BEGIN SELECT RAISE(ABORT, 'rest-publication-stop'); END;",
        )
        .unwrap();
}

/// Rewrites a pending owner journal as one the open's replay writes: origin narrowing_replay
/// and the replay's owner id, every other field as the owner's request left it, so only the
/// recover check for a replay's edit stands between it and the settings file.
/// Input: the database path. Output: none.
fn disguise_as_replay(path: &Path) {
    let db = rusqlite::Connection::open(path).unwrap();
    let body: Vec<u8> = db
        .query_row("SELECT body FROM settings_publication", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut journal: serde_json::Value = serde_json::from_slice(&body).unwrap();
    journal["origin"] = serde_json::json!("narrowing_replay");
    journal["admission"]["owner_id"] = serde_json::json!("narrowing_replay");
    db.execute(
        "UPDATE settings_publication SET body=?1",
        [serde_json::to_vec(&journal).unwrap()],
    )
    .unwrap();
}

/// Lets settings publications finish again. Input: the database path. Output: none.
fn resume_publications(path: &Path) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch("DROP TRIGGER rest_publication_stop")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removal_drained_with_a_stale_admission_keeps_its_intent_for_the_next_open() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let (id, key, _listener) = key_account(&owner, "remove-stale").await;
    let path = database(&owner);
    let drain = hold_drain(&owner.runtime().authority.root.path);
    let hold = crate::storage::hold_claims(&path);
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner.client.remove_account(id.clone()).await;
    drop(hold);
    filler.await.unwrap().unwrap();
    revoke_settled(&owner, "rest-revoke-before-drain").await;
    drop(drain);
    assert!(drained(&owner).await, "the drainer answers the removal");
    assert_eq!(
        account_rows(&path, &id),
        1,
        "storage refuses a drained removal whose admission went stale: {answer:?}"
    );
    let restarted = owner.restarted().await;
    let root = engine_root(restarted.root.path());
    assert_eq!(
        account_rows(&path, &id),
        0,
        "the next open applies the removal the drained refusal kept"
    );
    assert!(
        none_hold(&root, key.as_bytes()).await,
        "the removed account's key leaves the disk"
    );
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the applied intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_replaced_after_its_account_was_removed_and_lost_with_the_engine_leaves_no_key() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, first, _listener) = key_account(&owner, "key-race").await;
    let credential = key_reference(&owner, &id).await;
    owner.client.remove_account(id.clone()).await.unwrap();
    let root = engine_root(owner.root.path());
    assert!(none_hold(&root, first.as_bytes()).await);
    let intent = owner
        .runtime()
        .keep_narrowing(Change::KeyReplaced {
            id: id.clone(),
            credential: credential.clone(),
        })
        .await;
    assert!(intent.is_some(), "the key replace keeps its intent first");
    let second = format!("sk-after-race-{}", std::process::id());
    super::accounts::replace_key(
        &owner.runtime().authority.root,
        &credential,
        AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holders(&root, second.as_bytes()),
        1,
        "a replace that read the account before the Remove recreates its key file"
    );
    key_replaced_lost(&owner, &id, &credential, intent).await;
    let restarted = owner.restarted().await;
    assert!(
        none_hold(&root, second.as_bytes()).await,
        "the next open deletes the key a lost key replace left for a removed account"
    );
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the applied intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_replace_lost_after_its_key_rename_leaves_no_orphan_key() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, first, _listener) = key_account(&owner, "key-rename").await;
    let credential = key_reference(&owner, &id).await;
    let second = format!("sk-renamed-{}", std::process::id());
    super::accounts::replace_key(
        &owner.runtime().authority.root,
        &credential,
        AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    key_replaced_lost(&owner, &id, &credential, None).await;
    let path = database(&owner);
    let restarted = owner.restarted().await;
    let root = engine_root(restarted.root.path());
    assert_eq!(account_rows(&path, &id), 1, "the account stays");
    assert!(
        none_hold(&root, first.as_bytes()).await,
        "the rename replaced the old key in place, so no old key is left"
    );
    assert_eq!(
        holders(&root, second.as_bytes()),
        1,
        "the new key stays once"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kept_removal_of_an_account_already_gone_ends_its_intent_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let intent = owner
        .runtime()
        .keep_narrowing(Change::RemoveAccount {
            id: CredentialRef::new("rest-gone-account").unwrap(),
        })
        .await
        .expect("the removal keeps its intent");
    assert_eq!(kept(owner.root.path()), [intent]);
    let restarted = owner.restarted().await;
    assert!(
        kept(restarted.root.path()).is_empty(),
        "a removal that no longer applies ends its intent"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attach_after_a_refused_detach_keeps_the_new_secret_through_the_drain_and_a_reopen() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let (id, _first) = secret_server(&owner, "attach-after").await;
    let path = database(&owner);
    let root = engine_root(owner.root.path());
    let drain = hold_drain(&owner.runtime().authority.root.path);
    let hold = crate::storage::hold_claims(&path);
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner
        .client
        .remove_mcp_secret(id.clone(), SECRET_NAME.into())
        .await
        .map(|_| ());
    drop(hold);
    filler.await.unwrap().unwrap();
    let second = format!("secret-attached-after-{}", std::process::id());
    owner
        .client
        .set_mcp_secret(
            id.clone(),
            SECRET_NAME.into(),
            AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
        )
        .await
        .unwrap();
    drop(drain);
    assert!(drained(&owner).await, "the drainer answers the detach");
    assert_eq!(
        stored_secret_names(&path, &id),
        [SECRET_NAME],
        "the drained detach leaves the secret attached after it: {answer:?}"
    );
    assert_eq!(holders(&root, second.as_bytes()), 1);
    let restarted = owner.restarted().await;
    assert_eq!(
        stored_secret_names(&path, &id),
        [SECRET_NAME],
        "the next open leaves the secret attached after the detach"
    );
    assert_eq!(holders(&root, second.as_bytes()), 1);
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the superseded intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_death_between_an_attach_commit_and_its_intent_unlink_keeps_the_new_secret() {
    let owner = Usage::start_settings(&format!(
        "timeout_ms = {LAPSE_WAIT_MS}\nqueue_capacity = 1\n"
    ))
    .await;
    let (id, _first) = secret_server(&owner, "attach-death").await;
    let path = database(&owner);
    let drain = hold_drain(&owner.runtime().authority.root.path);
    let hold = crate::storage::hold_claims(&path);
    let filler = fill_queue(&owner);
    tokio::time::sleep(IN_FLIGHT).await;
    let answer = owner
        .client
        .remove_mcp_secret(id.clone(), SECRET_NAME.into())
        .await
        .map(|_| ());
    drop(hold);
    filler.await.unwrap().unwrap();
    crate::storage::die_at(&path, crate::storage::Death::AttachCommitted);
    let second = format!("secret-attached-at-death-{}", std::process::id());
    let attached = owner
        .client
        .set_mcp_secret(
            id.clone(),
            SECRET_NAME.into(),
            AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
        )
        .await;
    assert!(
        worker_died(&path).await,
        "the storage worker dies right after the attach commits: {attached:?}"
    );
    drop(drain);
    let restarted = owner.restarted().await;
    let root = engine_root(restarted.root.path());
    assert_eq!(
        stored_secret_names(&path, &id),
        [SECRET_NAME],
        "the next open leaves the secret the attach committed: {answer:?}"
    );
    assert_eq!(holders(&root, second.as_bytes()), 1);
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the superseded intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_account_removal_pending_when_the_engine_dies_leaves_the_catalog_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, key, _listener) = key_account(&owner, "held-die").await;
    into_catalog(&owner, &id).await;
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
    assert!(
        !in_settings(restarted.root.path(), &id),
        "the next open takes the held account out of the catalog: {answer:?}"
    );
    assert_eq!(account_rows(&path, &id), 0);
    assert!(
        none_hold(&root, key.as_bytes()).await,
        "the removed account's key leaves the disk"
    );
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the applied intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_removal_stopped_after_its_settings_journal_finishes_at_the_next_open() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, key, _listener) = key_account(&owner, "held-journal").await;
    into_catalog(&owner, &id).await;
    let path = database(&owner);
    let hold = crate::storage::hold_claims(&path);
    let answer = owner.client.remove_account(id.clone()).await;
    crate::storage::die_at_claim(&path);
    drop(hold);
    assert!(
        worker_died(&path).await,
        "the storage worker dies at its claim"
    );
    stop_publications(&path);
    let stopped = owner.restarted().await;
    assert_eq!(
        journaled(&path),
        1,
        "the replay journals its catalog removal before the rename stops: {answer:?}"
    );
    assert!(
        in_settings(stopped.root.path(), &id),
        "the stopped publication left the settings file as it was"
    );
    resume_publications(&path);
    let restarted = stopped.restarted().await;
    let root = engine_root(restarted.root.path());
    assert_eq!(journaled(&path), 0, "the next open finishes the journal");
    assert!(!in_settings(restarted.root.path(), &id));
    assert_eq!(account_rows(&path, &id), 0);
    assert!(
        none_hold(&root, key.as_bytes()).await,
        "the removed account's key leaves the disk"
    );
    assert!(
        kept(restarted.root.path()).is_empty(),
        "the applied intent ends"
    );
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_replay_journal_with_any_other_edit_is_refused_at_recover() {
    let owner = Usage::start_waiting(LAPSE_WAIT_MS).await;
    let (id, _key, _listener) = key_account(&owner, "replay-widen").await;
    let path = database(&owner);
    stop_publications(&path);
    let stopped = catalog_apply(&owner, &id).await;
    assert_eq!(
        journaled(&path),
        1,
        "the owner apply stops after its journal: {stopped:?}"
    );
    resume_publications(&path);
    disguise_as_replay(&path);
    let (refusal, root) = owner.refused_restart().await;
    assert_eq!(refusal.0, "settings_journal_invalid");
    assert!(
        !in_settings(root.path(), &id),
        "a refused journal widens nothing"
    );
}
