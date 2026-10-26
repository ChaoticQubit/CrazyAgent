use super::*;
use crate::engine::owner::{
    AccountProfileOutcome, AccountProfileReceipt, AccountProfileReply, AccountProfileRequest,
    ProducerCapability,
};
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::net::TcpListener;

const SYNTHETIC_SECRET: &str = "synthetic-ui-key-only-1035";
const RETAINED_DRAFT: &str = "Retained conversation draft 日本";

async fn selected(case: &Case, index: usize) -> Ui {
    let mut ui = Ui::new(case).await;
    ui.app.reduce(UiEvent::FocusPrevious).unwrap();
    ui.app.reduce(UiEvent::FocusPrevious).unwrap();
    for character in RETAINED_DRAFT.chars() {
        ui.app.reduce(UiEvent::Character(character)).unwrap();
    }
    ui.app.reduce(UiEvent::FocusNext).unwrap();
    ui.app.reduce(UiEvent::FocusNext).unwrap();
    ui.connections(index).await;
    ui.key(KeyCode::Enter);
    ui.drain().await;
    assert_eq!(ui.app.draft(), RETAINED_DRAFT);
    ui
}

fn private_panel(ui: &mut Ui, connection: &ModelConnection) {
    ui.key(KeyCode::Char('k'));
    let body = views::body(&ui.app);
    assert!(body.contains("Private profile setup"));
    assert!(body.contains(connection.provider.as_str()));
    assert!(body.contains(&connection.model.id));
    let crate::config::ModelEndpoint::Http { url } = &connection.endpoint else {
        panic!("actual HTTP connection required");
    };
    assert!(body.contains(url));
    for field in [
        "Account reference",
        "Connection ID",
        "Credential/profile reference",
    ] {
        assert!(!body.contains(field));
    }
}

fn assert_private(ui: &Ui) {
    assert_eq!(ui.app.draft(), RETAINED_DRAFT);
    assert!(!views::body(&ui.app).contains(SYNTHETIC_SECRET));
    assert!(!ui.app.model_access.notice.contains(SYNTHETIC_SECRET));
    for (width, height) in [(120, 40), (40, 10), (12, 6)] {
        assert!(!ui.render(width, height).contains(SYNTHETIC_SECRET));
    }
}

fn assert_no_auth(case: &Case) {
    let count: i64 = case
        .observer()
        .query_row("SELECT count(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

fn assert_no_secret_persistence(case: &Case) {
    let database = case.observer();
    let mut statement = database
        .prepare("SELECT request,receipt FROM account_profile_operations")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        })
        .unwrap();
    for row in rows {
        let (request, receipt) = row.unwrap();
        for bytes in [Some(request), receipt].into_iter().flatten() {
            assert!(
                !bytes
                    .windows(SYNTHETIC_SECRET.len())
                    .any(|window| window == SYNTHETIC_SECRET.as_bytes())
            );
        }
    }
    assert!(
        !std::fs::read(&case.global)
            .unwrap()
            .windows(SYNTHETIC_SECRET.len())
            .any(|window| window == SYNTHETIC_SECRET.as_bytes())
    );
    for name in ["engine.db", "engine.db-wal"] {
        let path = case.root.path().join(name);
        if path.exists() {
            assert!(std::fs::metadata(&path).unwrap().len() <= 8 * 1024 * 1024);
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(SYNTHETIC_SECRET.len())
                    .any(|window| window == SYNTHETIC_SECRET.as_bytes())
            );
        }
    }
}

async fn capabilities(
    case: &Case,
    submission: &model_access::ProfileSubmission,
) -> ProducerCapability {
    let mut owner = case.controller().await;
    let actual = owner
        .read_account_auth_capabilities(submission.request.clone())
        .await
        .unwrap();
    assert_eq!(actual.request_id, submission.request.request_id);
    assert_eq!(
        actual.database_instance,
        submission.basis.identity.database_instance()
    );
    assert_eq!(
        actual.engine_epoch,
        submission.basis.identity.engine_epoch()
    );
    assert_eq!(actual.config_id, submission.basis.config_id);
    assert_eq!(actual.source_hash, submission.basis.source_hash);
    assert_eq!(actual.connection, submission.connection);
    actual.configured
}

fn configure(
    ui: &mut Ui,
    secret_expected: bool,
) -> (model_access::Job, model_access::ProfileSubmission) {
    ui.key(KeyCode::Enter);
    let job = ui.app.model_access_request().unwrap().unwrap();
    let model_access::Job::AccountProfileConfigure { submission, secret } = &job else {
        panic!("actual App must move one private profile configure job");
    };
    assert_eq!(secret.is_some(), secret_expected);
    if let Some(secret) = secret {
        assert_eq!(format!("{secret:?}"), "AccountSecret([REDACTED])");
    }
    let submission = submission.clone();
    assert!(submission.generation > 0);
    assert_eq!(
        submission.request.scope.connection,
        submission.connection.id
    );
    assert_eq!(
        submission.request.scope.expected_config_id,
        submission.basis.config_id
    );
    assert_eq!(
        submission.request.scope.expected_source_hash,
        submission.basis.source_hash
    );
    assert_eq!(
        submission.request.scope.expected_database_instance,
        submission.basis.identity.database_instance()
    );
    ui.key(KeyCode::Enter);
    assert!(ui.app.model_access_request().unwrap().is_none());
    assert!(ui.app.model_access.has_pending());
    assert_private(ui);
    (job, submission)
}

async fn exchange(ui: &mut Ui, job: model_access::Job) -> OwnerEvent {
    assert!(!ui.connection.busy());
    ui.connection.model_access(job).unwrap();
    assert!(ui.connection.busy());
    ui.owner_event().await
}

fn reply(
    event: &OwnerEvent,
) -> (
    &model_access::ProfileSubmission,
    &Result<Box<AccountProfileReply>, String>,
    bool,
) {
    let OwnerEvent::ModelAccess(event) = event else {
        panic!("actual capacity-one owner completion required");
    };
    let model_access::Event::AccountProfile {
        submission,
        result,
        retained,
    } = &**event
    else {
        panic!("actual profile completion required");
    };
    (submission, result, *retained)
}

fn operation(
    case: &Case,
    request: &AccountProfileRequest,
) -> (String, Option<AccountProfileReceipt>) {
    let (bytes, phase, receipt): (Vec<u8>, String, Option<Vec<u8>>) = case
        .observer()
        .query_row(
            "SELECT request,phase,receipt FROM account_profile_operations WHERE request_id=?1",
            [request.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<AccountProfileRequest>(&bytes).unwrap(),
        *request
    );
    (
        phase,
        receipt.map(|bytes| serde_json::from_slice(&bytes).unwrap()),
    )
}

fn assert_profile_file(case: &Case, receipt: &AccountProfileReceipt) {
    let key = format!(
        "{:x}",
        Sha256::digest(receipt.profile_ref.as_str().as_bytes())
    );
    let home = case.root.path().join("configured-sdk-homes").join(key);
    let path = home.join(".backboard/config.json");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        receipt.profile_config_sha256
    );
    let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        actual["model"]["provider"],
        receipt.connection.provider.as_str()
    );
    assert_eq!(actual["model"]["model"], receipt.connection.model.id);
    assert_eq!(
        actual["providers"][0]["auth"],
        serde_json::json!({ "type": "none" })
    );
    assert_eq!(
        actual["providers"][0]["protocol"],
        receipt.connection.protocol.as_str()
    );
    let crate::config::ModelEndpoint::Http { url } = &receipt.connection.endpoint else {
        panic!("actual HTTP connection required");
    };
    assert_eq!(actual["providers"][0]["baseUrl"], url.as_str());
    assert!(receipt.profile_generation > 0);
}

#[tokio::test]
async fn api_key_uses_masked_transient_input_and_actual_unqualified_writer_rejection() {
    run(|case| async move {
        let mut ui = selected(&case, 2).await;
        let (_, connection) = case.selected("c-api").await;
        let original = std::fs::read(&case.global).unwrap();
        let before = case.counts();
        private_panel(&mut ui, &connection);
        assert!(views::body(&ui.app).contains("API key (hidden)"));
        for character in SYNTHETIC_SECRET.chars() {
            ui.key(KeyCode::Char(character));
        }
        ui.key(KeyCode::Backspace);
        ui.key(KeyCode::Char('5'));
        assert!(views::body(&ui.app).contains("Key buffer: hidden"));
        assert_private(&ui);
        ui.key(KeyCode::Esc);
        ui.drain().await;
        assert_eq!(case.counts(), before);
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        private_panel(&mut ui, &connection);
        assert!(views::body(&ui.app).contains("Key buffer: empty"));
        for character in SYNTHETIC_SECRET.chars() {
            ui.key(KeyCode::Char(character));
        }
        let (job, submission) = configure(&mut ui, true);
        assert_eq!(submission.connection, connection);
        assert_eq!(
            capabilities(&case, &submission).await,
            ProducerCapability::Unavailable {
                code: "rcli_private_key_writer_unavailable".into()
            }
        );
        let event = exchange(&mut ui, job).await;
        let (actual, result, retained) = reply(&event);
        assert_eq!(actual, &submission);
        assert_eq!(
            result.as_ref().unwrap_err(),
            "rcli_private_key_writer_unavailable"
        );
        assert!(!retained);
        ui.app.apply_owner(event).unwrap();
        ui.drain().await;
        let body = views::body(&ui.app);
        assert!(body.contains("rcli_private_key_writer_unavailable"));
        assert!(!body.contains("ConfiguredOnly"));
        assert!(!body.contains("AuthenticatedObserved"));
        assert!(!body.contains("Ready"));
        assert!(!ui.app.model_access.has_pending());
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert_eq!(
            operation(&case, &submission.request),
            ("not_applied".into(), None)
        );
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        assert_no_secret_persistence(&case);
        assert_private(&ui);
        assert_no_auth(&case);
    })
    .await;
}

async fn local(case: &Case, listener: &TcpListener) -> (Ui, ModelConnection) {
    let mut settings: toml::Value = toml::from_str(CATALOG).unwrap();
    settings["model_access"]["connections"][3]["protocol"] =
        toml::Value::String("openai-chat".into());
    settings["model_access"]["connections"][3]["endpoint"]["url"] =
        toml::Value::String(format!("http://{}/v1", listener.local_addr().unwrap()));
    let document = toml_edit::ser::to_document(&settings).unwrap();
    std::fs::write(&case.global, document.to_string()).unwrap();
    case.restart().await;
    let ui = selected(case, 3).await;
    let (_, connection) = case.selected("d-local").await;
    (ui, connection)
}

fn local_receipt(
    event: &OwnerEvent,
    expected: &model_access::ProfileSubmission,
    historical: bool,
) -> AccountProfileReceipt {
    let (submission, result, retained) = reply(event);
    assert_eq!(submission, expected);
    assert!(!retained);
    let actual = result.as_ref().unwrap();
    assert_eq!(actual.request_id, expected.request.request_id);
    assert_eq!(
        actual.database_instance,
        expected.basis.identity.database_instance()
    );
    assert_eq!(actual.engine_epoch, expected.basis.identity.engine_epoch());
    let AccountProfileOutcome::ConfiguredOnly {
        receipt,
        historical_replay,
    } = &actual.outcome
    else {
        panic!("actual configured-only local receipt required");
    };
    assert_eq!(*historical_replay, historical);
    assert_eq!(receipt.connection, expected.connection);
    assert_eq!(receipt.config_id, expected.basis.config_id);
    assert_eq!(receipt.source_hash, expected.basis.source_hash);
    (**receipt).clone()
}

#[tokio::test]
async fn full_profile_history_prunes_finished_rows_and_keeps_unfinished_ones() {
    run(|case| async move {
        let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut ui, connection) = local(&case, &listener).await;
        private_panel(&mut ui, &connection);
        let (job, submission) = configure(&mut ui, false);
        let (owner, epoch): (String, i64) = case
            .observer()
            .query_row(
                "SELECT owner_id,owner_epoch FROM owner_identities WHERE client_kind='tui'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let engine_epoch = submission.basis.identity.engine_epoch().to_string();
        {
            let mut db = rusqlite::Connection::open(case.root.path().join("engine.db")).unwrap();
            db.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
            let db = db.transaction().unwrap();
            db.execute_batch("CREATE TABLE IF NOT EXISTS account_profile_operations (owner_id TEXT NOT NULL, request_id TEXT NOT NULL, owner_epoch INTEGER NOT NULL, engine_epoch TEXT NOT NULL, request BLOB NOT NULL, phase TEXT NOT NULL CHECK(phase IN ('reserved','running','committed','not_applied','uncertain')), receipt BLOB, PRIMARY KEY(owner_id,request_id))").unwrap();
            let insert = |id: String, phase: &str, owner_epoch: i64, engine: &str| {
                db.execute(
                    "INSERT INTO account_profile_operations(owner_id,request_id,owner_epoch,engine_epoch,request,phase) VALUES(?1,?2,?3,?4,x'7b7d',?5)",
                    rusqlite::params![owner, id, owner_epoch, engine, phase],
                )
                .unwrap();
            };
            insert("unfinished".into(), "reserved", epoch, &engine_epoch);
            for n in 0..1023 {
                let (phase, owner_epoch, engine) = match n % 3 {
                    0 => ("not_applied", epoch, engine_epoch.as_str()),
                    1 => ("uncertain", epoch, engine_epoch.as_str()),
                    _ => ("reserved", epoch, "previous-engine"),
                };
                insert(format!("finished-{n:04}"), phase, owner_epoch, engine);
            }
            db.commit().unwrap();
        }
        let completion = exchange(&mut ui, job).await;
        let receipt = local_receipt(&completion, &submission, false);
        assert_eq!(
            operation(&case, &submission.request),
            ("committed".into(), Some(receipt))
        );
        let rows = |sql: &str| -> i64 { case.observer().query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(rows("SELECT count(*) FROM account_profile_operations"), 1024);
        assert_eq!(
            rows("SELECT count(*) FROM account_profile_operations WHERE request_id='unfinished' AND phase='reserved'"),
            1
        );
        assert_eq!(
            rows("SELECT count(*) FROM account_profile_operations WHERE request_id='finished-0000'"),
            0
        );
        assert_no_secret_persistence(&case);
    })
    .await;
}

#[tokio::test]
async fn local_none_commits_private_profile_and_recovers_original_receipt_without_endpoint_io() {
    run(|case| async move {
        let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut ui, connection) = local(&case, &listener).await;
        let original = std::fs::read(&case.global).unwrap();
        private_panel(&mut ui, &connection);
        let body = views::body(&ui.app);
        assert!(body.contains("No API key"));
        assert!(!body.contains("API key (hidden)"));
        let (job, submission) = configure(&mut ui, false);
        assert_eq!(
            capabilities(&case, &submission).await,
            ProducerCapability::Available
        );
        let completion = exchange(&mut ui, job).await;
        let receipt = local_receipt(&completion, &submission, false);
        assert_eq!(
            operation(&case, &submission.request),
            ("committed".into(), Some(receipt.clone()))
        );
        assert_profile_file(&case, &receipt);
        drop(completion);
        assert!(ui.app.model_access.has_pending());
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert!(ui.app.model_access.retry());
        let read = ui.app.model_access_request().unwrap().unwrap();
        let model_access::Job::AccountProfileRead {
            submission: original_submission,
        } = &read
        else {
            panic!("lost completion recovery must read metadata, never configure again");
        };
        assert_eq!(original_submission, &submission);
        let reconciled = exchange(&mut ui, read).await;
        assert_eq!(local_receipt(&reconciled, &submission, true), receipt);
        ui.app.apply_owner(reconciled).unwrap();
        ui.drain().await;
        let body = views::body(&ui.app);
        assert!(body.contains("ConfiguredOnly"));
        assert!(!body.contains("AuthenticatedObserved"));
        assert!(!body.contains("Ready"));
        assert!(!ui.app.model_access.has_pending());
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        let count: i64 = case
            .observer()
            .query_row(
                "SELECT count(*) FROM account_profile_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            crate::spawn_gate::accept_tcp_std(&listener)
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        assert_no_secret_persistence(&case);
        assert_private(&ui);
        assert_no_auth(&case);
        let focus = ui.app.focus();
        ui.key(KeyCode::BackTab);
        assert_ne!(
            ui.app.focus(),
            focus,
            "a finished setup panel must not trap focus"
        );
        assert!(!views::body(&ui.app).contains("Private profile setup"));
    })
    .await;
}
