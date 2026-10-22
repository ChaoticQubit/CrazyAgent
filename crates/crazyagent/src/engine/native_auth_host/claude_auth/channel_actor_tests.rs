use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::native_auth_host::custody::CustodySession;
use crate::engine::owner::native_auth::Command;
use crate::engine::owner::{NativeAuthCancel, NativeAuthMethod, NativeAuthScope, NativeAuthStart};
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn claude_private_input_real_owner_refuses_unobserved_stale_and_cancelled_prompts() {
    let root = tempfile::Builder::new().prefix("ca-claude-input-").tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    std::fs::write(&global, r#"
timeout_ms = 1000
[model_access]
accounts = ["claude-input-account"]
[[model_access.connections]]
id = "claude-input-connection"
provider = "anthropic"
harness = "claude"
protocol = "native"
account_ref = "claude-input-account"
endpoint = { kind = "native" }
model = { id = "claude-input-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-input-profile" }
[[model_access.pools]]
id = "claude-input-pool"
members = ["claude-input-connection"]
projects = ["claude-input-project"]
roles = ["leader"]
fallback_allowed = false
"#).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = crate::config::resolve_config(&global, &project).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(root.path(), "engine.sock", &global, &project, &stopped).await.unwrap();
    let host = service.owner_runtime().unwrap();
    let client = OwnerClient::attach(&host.authority.root.path, OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
    let id = |value: &str| RequestId::new(value).unwrap();
    let response = client.request(OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: id("claude-input-source"),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read { query: OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: CredentialRef::new("claude-input-connection").unwrap(),
        } },
    }).await.unwrap();
    let OwnerResponse::Read { state } = response.response else { panic!("real owner source record required") };
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else { panic!("real source identity required") };
    let scope = NativeAuthScope {
        expected_database_instance: host.identity().db_instance,
        connection: CredentialRef::new("claude-input-connection").unwrap(),
        expected_config_id: config.id,
        expected_source_hash: source_hash,
    };
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(b"CAO1");
    packet[5..].copy_from_slice(&client.token);
    let admission = host.authority.authenticate(&packet).unwrap();
    packet.fill(0);
    let begun = host.engine.native_auth_job(|reply| Job::NativeAuth(admission, Command::Start(NativeAuthStart {
        request_id: id("claude-input-operation"), scope: scope.clone(), method: NativeAuthMethod::ClaudeOfficial,
    }), reply)).await.unwrap();
    assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
    let fact = begun.reply.fact;
    let _launch = host.prepare_native_auth_custody(begun.work.unwrap()).await.unwrap();
    let (_, _, binding) = runner_binding(root.path()).unwrap();
    let mut custody = binding.capture_native_profile_custody().unwrap();
    let guard = custody.acquire_use(&host.identity(), &fact.binding, &fact.operation_id, fact.generation, purpose(&fact)).unwrap();
    let session = CustodySession::new(guard);
    let token = host.authority.native_auth.lock().unwrap().as_ref().unwrap().token;
    let mut channel = NativeAuthCustodyChannel::connect(root.path(), token).await.unwrap();
    let query = NativeAuthQuery { request_id: id("claude-input-query"), scope: scope.clone(), operation_id: fact.operation_id.clone() };
    let request = ClaudeAuthInput { query: query.clone(), generation: fact.generation, prompt_id: 1 };
    let secret = || AccountSecret::new(b"untransmitted-synthetic-code#untransmitted-synthetic-state".to_vec()).unwrap();
    assert!(client.submit_claude_auth_code(request.clone(), secret()).await.is_err());
    let mut stale = request.clone();
    stale.query.scope.expected_source_hash = "f".repeat(64);
    assert!(client.submit_claude_auth_code(stale, secret()).await.is_err());
    let mut wrong_generation = request.clone();
    wrong_generation.generation += 1;
    assert!(client.submit_claude_auth_code(wrong_generation, secret()).await.is_err());
    let read = client.read_native_auth(query).await.unwrap();
    assert_eq!(read.fact.phase, NativeAuthPhase::Starting);
    assert!(read.account.is_none() && read.challenge.is_none() && read.claude_challenge.is_none());
    let cancelled = client.cancel_native_auth(NativeAuthCancel {
        request_id: id("claude-input-cancel"), scope, operation_id: fact.operation_id.clone(),
    }).await.unwrap();
    assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelling);
    assert!(client.submit_claude_auth_code(request, secret()).await.is_err());
    assert!(matches!(session.check(&mut channel).await.unwrap(), NativeAuthCustodyControl::Cancel));
    assert!(session.profile_path().unwrap().is_dir());
    let db = rusqlite::Connection::open_with_flags(root.path().join("engine.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (request, stored): (Vec<u8>, Vec<u8>) = db.query_row("SELECT request,fact FROM native_auth_operations WHERE id=?1", [fact.operation_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    for payload in [request, stored] {
        assert!(!payload.windows(b"untransmitted-synthetic".len()).any(|value| value == b"untransmitted-synthetic"));
    }
    drop(db);
    drop(channel);
    drop(session);
    service.shutdown().await.unwrap();
}
