use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelConnection, resolve_config};
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthQuery, NativeAuthReply, NativeAuthScope,
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRead,
    OwnerRequest, OwnerResponse, OwnerResult, OwnerView,
};
use crazyagent::protocol::*;
use crazyagent::verification::*;
use serde_json::json;
use std::error::Error;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

type FixtureResult<T> = Result<T, Box<dyn Error>>;

fn baseline() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("desktop-project").expect("fixed project id"),
        origin: BaselineOrigin::HumanAuthored,
        requirements: "Inspect <script>untrusted text</script> as literal text".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").expect("fixed check id"),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"desktop behavior".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").expect("fixed check id"),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"test integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

async fn seed(root: &Path) -> FixtureResult<BaselineId> {
    let client = Client::new(root.join("e.sock"), Duration::from_secs(3))?;
    let (ordinary, mut owner) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let created = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("desktop-fixture")?,
            request_id: RequestId::new("desktop-project")?,
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("desktop-project")?,
                spec: ProjectSpec {
                    name: "Desktop fixture".into(),
                    repositories: vec![],
                },
            },
        })
        .await?;
    if !matches!(created.outcome, Outcome::Applied) {
        return Err("real project creation was rejected".into());
    }
    owner
        .read_page(OwnerView::Baselines, None, Some(16))
        .await?;
    let draft = baseline();
    let id = draft.identity()?;
    owner.begin_mutation(OwnerMutation::ProposeBaseline { draft }, 1)?;
    let result = owner.send_pending().await?;
    match result {
        OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied => Ok(id),
        _ => Err("real baseline proposal was not applied".into()),
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

async fn await_stop(id: BaselineId) -> FixtureResult<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 5];
        let result = std::io::stdin().read_exact(&mut bytes);
        let _ = sender.send(result.is_ok() && bytes == *b"stop\n");
    });
    emit(json!({ "ready": true, "baseline_id": id.as_str() }))?;
    match tokio::time::timeout(Duration::from_secs(180), receiver).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err("fixture shutdown request missing or invalid".into()),
    }
}

async fn serve(root: &Path, metadata: bool) -> FixtureResult<()> {
    if !root.is_absolute() || std::fs::read_dir(root)?.next().is_some() {
        return Err("fixture requires a new empty absolute directory".into());
    }
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
    let result = match seed(root).await {
        Ok(id) => await_stop(id).await,
        Err(error) => Err(error),
    };
    let cleanup = service.shutdown().await;
    cleanup?;
    result
}

async fn observe(id: &str) -> FixtureResult<()> {
    let baseline_id = BaselineId::new(id)?;
    let socket =
        std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").ok_or("native fixture socket is required")?;
    let client = Client::new(socket.into(), Duration::from_secs(3))?;
    let (_ordinary, mut owner) =
        OwnerController::attach_configured(client, OwnerClientKind::Tui).await?;
    let state = owner
        .read(OwnerRead::Baseline {
            baseline_id: baseline_id.clone(),
        })
        .await?;
    match state.data {
        OwnerData::Baseline {
            approved,
            revoked,
            current,
            ..
        } => emit(json!({
            "baseline_id": baseline_id.as_str(),
            "approved": approved,
            "revoked": revoked,
            "current": current
        })),
        _ => Err("actual baseline read required".into()),
    }
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
    emit(json!({ "collections": records }))
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

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).take(4).collect();
    let result = match args.as_slice() {
        [mode, root] if mode == "serve" => serve(Path::new(root), false).await,
        [mode, root] if mode == "serve-model-access" => serve(Path::new(root), true).await,
        [mode] if mode == "observe-model-access" => observe_model_access().await,
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
        [mode, id] if mode == "observe" => match id.to_str() {
            Some(id) => observe(id).await,
            None => Err("invalid baseline id".into()),
        },
        _ => Err("one closed fixture mode is required".into()),
    };
    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("native fixture failed");
        std::process::ExitCode::FAILURE
    }
}
