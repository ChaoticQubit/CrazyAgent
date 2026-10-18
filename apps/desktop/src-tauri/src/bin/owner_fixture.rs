use crazyagent::client_controller::OwnerController;
use crazyagent::config::resolve_config;
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse, OwnerResult, OwnerView,
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

async fn serve(root: &Path) -> FixtureResult<()> {
    if !root.is_absolute() || std::fs::read_dir(root)?.next().is_some() {
        return Err("fixture requires a new empty absolute directory".into());
    }
    let config = resolve_config(&root.join("g"), &root.join("p"))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root, "e.sock", &config, &stopped).await?;
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

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).take(3).collect();
    let result = match args.as_slice() {
        [mode, root] if mode == "serve" => serve(Path::new(root)).await,
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
