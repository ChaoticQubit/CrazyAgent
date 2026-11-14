use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelConnection, resolve_config};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthQuery, NativeAuthReply, NativeAuthScope,
    OwnerClient, OwnerClientKind, OwnerData, OwnerOperation, OwnerRead, OwnerRequest,
    OwnerResponse, OwnerResult,
};
use crazyagent::protocol::*;
use crazyagent::usage::service::{UsageService, start_for_engine};
use serde_json::json;
use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

type FixtureResult<T> = Result<T, Box<dyn Error>>;

/// How long a served fixture engine waits for its stop request when the runner names no life.
const LIFE_DEFAULT_SECONDS: u64 = 360;
/// The shortest life the runner may name, in seconds.
const LIFE_MIN_SECONDS: u64 = 60;
/// The longest life the runner may name, in seconds, so a lost runner never leaves an engine
/// running for more than an hour.
const LIFE_MAX_SECONDS: u64 = 3600;
const _: () = assert!(LIFE_MIN_SECONDS <= LIFE_DEFAULT_SECONDS);
const _: () = assert!(LIFE_DEFAULT_SECONDS <= LIFE_MAX_SECONDS);

/// The life of a served fixture engine: the native runner's own bound for it, from
/// `CRAZYAGENT_DESKTOP_TEST_FIXTURE_SECONDS`, so a long suite never loses its engine before the
/// runner's bound, else `LIFE_DEFAULT_SECONDS`. Output: the life, or why the value is refused.
fn fixture_life() -> FixtureResult<Duration> {
    let Some(value) = std::env::var_os("CRAZYAGENT_DESKTOP_TEST_FIXTURE_SECONDS") else {
        return Ok(Duration::from_secs(LIFE_DEFAULT_SECONDS));
    };
    let seconds: u64 = value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or("the fixture life is not a whole number of seconds")?;
    if !(LIFE_MIN_SECONDS..=LIFE_MAX_SECONDS).contains(&seconds) {
        return Err("the fixture life is out of bounds".into());
    }
    Ok(Duration::from_secs(seconds))
}

async fn attach(root: &Path, projects: &Path) -> FixtureResult<()> {
    let client = Client::new(root.join("e.sock"), Duration::from_secs(3))?;
    let (_, mut owner) = OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let path = projects
        .to_str()
        .ok_or("projects folder path is not text")?;
    match owner.set_projects_home(path.to_owned(), 1).await? {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
            if operation.result == OwnerResult::Applied =>
        {
            Ok(())
        }
        _ => Err("the projects folder was not set".into()),
    }
}

fn emit(value: serde_json::Value) -> FixtureResult<()> {
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() > 4096 {
        return Err("fixture output bound".into());
    }
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

async fn await_stop(life: Duration) -> FixtureResult<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 5];
        let result = std::io::stdin().read_exact(&mut bytes);
        let _ = sender.send(result.is_ok() && bytes == *b"stop\n");
    });
    emit(json!({ "ready": true }))?;
    match tokio::time::timeout(life, receiver).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err("fixture shutdown request missing or invalid".into()),
    }
}

/// Starts the engine's usage upkeep beside the fixture engine when the native run sets
/// `CRAZYAGENT_DESKTOP_TEST_USAGE_UPKEEP=1`, as the engine binary does for a team. It gets no
/// harness executable and no lookup, so it never spawns a tool or calls a provider, and an
/// account that is not signed in reads nothing. Input: the fixture root. Output: the upkeep,
/// none when not asked for, or why it could not start.
fn usage_upkeep(root: &Path) -> FixtureResult<Option<UsageService>> {
    match std::env::var_os("CRAZYAGENT_DESKTOP_TEST_USAGE_UPKEEP") {
        None => Ok(None),
        Some(value) if value == "1" => start_for_engine(
            root,
            &root.join("g"),
            &root.join("p"),
            BTreeMap::new(),
            None,
        )
        .map(Some)
        .map_err(|problem| format!("usage upkeep did not start: {problem}").into()),
        Some(_) => Err("the usage upkeep switch takes only 1".into()),
    }
}

async fn serve(root: &Path, projects: &Path, metadata: bool) -> FixtureResult<()> {
    if !root.is_absolute() || std::fs::read_dir(root)?.next().is_some() {
        return Err("fixture requires a new empty absolute directory".into());
    }
    let life = fixture_life()?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = if metadata {
        EngineService::start_with_settings_sources(
            root,
            "e.sock",
            &root.join("g"),
            &root.join("p"),
            &stopped,
        )
        .await?
    } else {
        let config = resolve_config(&root.join("g"), &root.join("p"))?;
        EngineService::start(root, "e.sock", &config, &stopped).await?
    };
    let upkeep = match usage_upkeep(root) {
        Ok(upkeep) => upkeep,
        Err(error) => {
            service.shutdown().await?;
            return Err(error);
        }
    };
    let result = match attach(root, projects).await {
        Ok(()) => await_stop(life).await,
        Err(error) => Err(error),
    };
    if let Some(upkeep) = upkeep {
        let _ = tokio::task::spawn_blocking(move || upkeep.stop()).await;
    }
    let cleanup = service.shutdown().await;
    cleanup?;
    result
}

async fn observe_model_access() -> FixtureResult<()> {
    let socket =
        std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").ok_or("native fixture socket is required")?;
    let client = Client::new(socket.into(), Duration::from_secs(3))?;
    let (_, mut owner) = OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let mut records = Vec::new();
    for collection in [
        ModelAccessCollection::Accounts,
        ModelAccessCollection::Connections,
        ModelAccessCollection::Pools,
    ] {
        let state = owner
            .read(OwnerRead::ModelAccessPage {
                collection,
                cursor: None,
                limit: 16,
            })
            .await?;
        let OwnerData::ModelAccessPage {
            items,
            next,
            writable,
            ..
        } = state.data
        else {
            return Err("actual metadata page required".into());
        };
        if next.is_some() {
            return Err("native fixture observation exceeds one page".into());
        }
        records.push(json!({ "collection": collection, "items": items, "writable": writable, "revision": state.revision.value().to_string() }));
    }
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|record| record["items"].is_array()));
    let registry = owner.list_accounts(None, 16).await?;
    if registry.next.is_some() {
        return Err("native fixture account registry exceeds one page".into());
    }
    emit(json!({ "collections": records, "registry": registry.accounts }))
}

async fn auth_connection(
    owner: &OwnerClient,
    id: CredentialRef,
) -> FixtureResult<(ConfigId, String, bool, String, ModelConnection)> {
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("native-auth-observer-record")?,
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read {
            query: OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: id.clone(),
            },
        },
    };
    let OwnerResponse::Read { state } = owner.request(request).await?.response else {
        return Err("Desktop observer connection read was not admitted".into());
    };
    let OwnerData::ModelAccessRecord {
        source_hash,
        writable,
        record: ModelAccessRecord::Connection { connection },
    } = state.data
    else {
        return Err("actual native connection record required".into());
    };
    if connection.id != id {
        return Err("observer connection identity mismatch".into());
    }
    Ok((
        state.config_id,
        source_hash,
        writable,
        state.revision.value().to_string(),
        connection,
    ))
}

fn auth_observation(
    reply: NativeAuthReply,
    scope: &NativeAuthScope,
    connection: &ModelConnection,
    operation_id: &RequestId,
    engine_epoch: &str,
) -> FixtureResult<serde_json::Value> {
    let binding = &reply.fact.binding;
    if &reply.fact.operation_id != operation_id
        || reply.request_id.as_str() != "native-auth-observer-read"
        || reply.database_instance != scope.expected_database_instance
        || reply.engine_epoch != engine_epoch
        || binding.database_instance != reply.database_instance
        || binding.engine_epoch != engine_epoch
        || binding.config_id != scope.expected_config_id
        || binding.source_hash != scope.expected_source_hash
        || &binding.connection != connection
    {
        return Err("actual owner auth fact did not join the inspected connection".into());
    }
    Ok(json!({
        "request_id": reply.request_id.as_str(),
        "operation_id": reply.fact.operation_id.as_str(),
        "database_instance": reply.database_instance,
        "engine_epoch": reply.engine_epoch,
        "config_id": binding.config_id.as_str(),
        "source_hash": binding.source_hash,
        "connection": binding.connection.id.as_str(),
        "connection_match": true,
        "generation": reply.fact.generation.to_string(),
        "action": reply.fact.action,
        "phase": reply.fact.phase,
        "code": reply.fact.code,
        "challenge_present": reply.challenge.is_some(),
        "account_present": reply.account.is_some()
    }))
}

async fn observe_native_auth(id: &str, operation: Option<&str>) -> FixtureResult<()> {
    let id = CredentialRef::new(id)?;
    let operation = operation.map(RequestId::new).transpose()?;
    let socket =
        std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").ok_or("native fixture socket is required")?;
    let client = Client::new(socket.into(), Duration::from_secs(3))?;
    let (_, owner, identity) = client.with_owner(OwnerClientKind::Desktop).await?;
    let (config_id, source_hash, writable, revision, connection) =
        auth_connection(&owner, id.clone()).await?;
    let scope = NativeAuthScope {
        expected_database_instance: identity.database_instance().into(),
        connection: id,
        expected_config_id: config_id,
        expected_source_hash: source_hash,
    };
    let observation = if let Some(operation_id) = operation {
        let reply = owner
            .read_native_auth(NativeAuthQuery {
                request_id: RequestId::new("native-auth-observer-read")?,
                scope: scope.clone(),
                operation_id: operation_id.clone(),
            })
            .await?;
        Some(auth_observation(
            reply,
            &scope,
            &connection,
            &operation_id,
            identity.engine_epoch(),
        )?)
    } else {
        None
    };
    emit(json!({
        "observer_kind": "desktop",
        "connection": connection.id.as_str(),
        "config_id": scope.expected_config_id.as_str(),
        "source_hash": scope.expected_source_hash,
        "database_instance": identity.database_instance(),
        "engine_epoch": identity.engine_epoch(),
        "revision": revision,
        "writable": writable,
        "operation": observation
    }))
}

/// Reads whether the owner skipped the Learn CrazyAgent guide through the engine's own
/// learning read, independently of the window. Input: the engine socket from the
/// environment. Output: one JSON line with the skipped flag only; no counts or step words.
async fn observe_learning() -> FixtureResult<()> {
    let socket =
        std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").ok_or("native fixture socket is required")?;
    let client = Client::new(socket.into(), Duration::from_secs(3))?;
    let (_, mut owner) = OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let progress = owner.learning_progress().await?;
    emit(json!({ "skipped": progress.skipped }))
}

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).take(4).collect();
    let result = match args.as_slice() {
        [mode, root, projects] if mode == "serve" => {
            serve(Path::new(root), Path::new(projects), false).await
        }
        [mode, root, projects] if mode == "serve-model-access" => {
            serve(Path::new(root), Path::new(projects), true).await
        }
        [mode] if mode == "observe-model-access" => observe_model_access().await,
        [mode] if mode == "observe-learning" => observe_learning().await,
        [mode, id] if mode == "observe-native-auth" => match id.to_str() {
            Some(id) => observe_native_auth(id, None).await,
            None => Err("invalid connection id".into()),
        },
        [mode, id, operation] if mode == "observe-native-auth" => {
            match (id.to_str(), operation.to_str()) {
                (Some(id), Some(operation)) => observe_native_auth(id, Some(operation)).await,
                _ => Err("invalid connection or operation id".into()),
            }
        }
        _ => Err("one closed fixture mode is required".into()),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("native fixture failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
