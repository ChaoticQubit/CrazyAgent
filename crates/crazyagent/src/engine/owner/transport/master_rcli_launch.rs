use super::{EngineError, OwnerRuntimeHandle, failure};
use crate::config::{ModelAuthentication, ModelEndpoint};
use crate::coordination::Role;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::io_error;
use crate::engine::owner::{is_hex_id, random_id};
use crate::master_session::{MasterBackend, MasterSessionPhase};
use crate::protocol::RequestId;
use crate::rcli_host::{RcliMasterBinding, RcliMasterLaunch, checked_configured_profile};
use crate::storage::PreparedMasterTurn;
use crate::worktrees::LeaseState;
use std::path::Path;
use std::sync::atomic::Ordering;

impl OwnerRuntimeHandle {
    pub(super) fn prepare_rcli_master_host(
        &self,
        prepared: &PreparedMasterTurn,
        profile: &NativeProfileUse,
    ) -> Result<RcliMasterLaunch, EngineError> {
        validate_prepared(self, prepared)?;
        profile.revalidate_retained()?;
        let home_dir = profile.profile_path()?;
        let profile_config_sha256 =
            checked_configured_profile(&home_dir, &prepared.attempt.connection)
                .map_err(|error| EngineError(error.0))?;
        profile.revalidate_retained()?;
        let executable = self.registered_rcli_master_executable()?;
        let executable_path = executable.verified_path()?.to_path_buf();
        let artifact_sha256 = executable.verified_sha256()?;
        if !is_hex_id(&artifact_sha256) || !is_hex_id(&profile_config_sha256) {
            return Err(failure("master_runtime_binding"));
        }
        let instance_id = RequestId::new(random_id()?).map_err(io_error)?;
        validate_prepared(self, prepared)?;
        if profile.profile_path()? != home_dir {
            return Err(failure("master_runtime_binding"));
        }
        Ok(RcliMasterLaunch {
            binding: RcliMasterBinding {
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                instance_id,
                connection: prepared.attempt.connection.clone(),
                target_profile_ref: prepared.attempt.request.target_profile_ref.clone(),
            },
            executable: executable_path,
            artifact_sha256,
            home_dir,
            cwd: prepared.cwd.clone(),
            profile_config_sha256,
            instructions: None,
        })
    }
}

fn validate_prepared(
    host: &OwnerRuntimeHandle,
    prepared: &PreparedMasterTurn,
) -> Result<(), EngineError> {
    if !host.authority.usable.load(Ordering::SeqCst) {
        return Err(failure("owner_runtime_stale"));
    }
    host.authority.root.verify()?;
    let binding = prepared.profile.binding();
    let identity = host.identity();
    if binding.database_instance != identity.db_instance
        || binding.engine_epoch != identity.engine_epoch
    {
        return Err(failure("owner_runtime_stale"));
    }
    let session = &prepared.session;
    let request = &prepared.attempt.request;
    let connection = &prepared.attempt.connection;
    let lease = &prepared.lease;
    if session.selection.backend != MasterBackend::Rcli
        || connection.harness.as_str() != "rcli"
        || !matches!(
            connection.protocol.as_str(),
            "openai-chat" | "openai-responses" | "anthropic-messages"
        )
        || !matches!(connection.endpoint, ModelEndpoint::Http { .. })
        || !matches!(
            connection.auth,
            ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}
        )
    {
        return Err(failure("master_runtime_backend_unsupported"));
    }
    if session.phase != MasterSessionPhase::Configured
        || session.current_turn.is_none()
        || session.owner.origin != "native_owner"
        || session.engine_epoch() != identity.engine_epoch
        || prepared.attempt.completion.is_some()
        || request.project_id != session.project_id
        || !prepared.attempt_on_current_turn()
        || request.role != Role::ProjectLeader
        || request.pool_ref != session.selection.pool_ref
        || (request.target_profile_ref.clone(), request.route.clone())
            != crate::storage::member_request(session, connection)
        || request.route.account_ref != connection.account_ref
        || &prepared.attempt.config_id != session.binding().0
        || (&binding.config_id, binding.source_hash.as_str()) != session.binding()
        || binding.connection != *connection
        || lease.state != LeaseState::Ready
        || lease.project_id != session.project_id
        || lease.task_id != session.bootstrap_task_id
        || lease.owner_session != session.session_id
        || lease.native_descendant.is_some()
        || Path::new(&lease.path) != prepared.cwd.as_path()
        || Path::new(&lease.repository) != prepared.repository.as_path()
    {
        return Err(failure("master_runtime_binding"));
    }
    let admitted = {
        let slots = host
            .authority
            .slots
            .lock()
            .map_err(|_| failure("owner_runtime_stale"))?;
        slots.iter().any(|slot| {
            let admission = &slot.registration.admission;
            slot.registration.active
                && admission.db_instance == identity.db_instance
                && admission.engine_epoch == identity.engine_epoch
                && admission.owner_id == session.owner.owner_id
                && admission.owner_epoch == session.owner.resulting_epoch
        })
    };
    if !admitted {
        return Err(failure("master_runtime_scope_revoked"));
    }
    for path in [&prepared.cwd, &prepared.repository] {
        let metadata = std::fs::symlink_metadata(path).map_err(io_error)?;
        if !path.is_absolute()
            || !metadata.is_dir()
            || !crate::platform::owned_by_effective_user(&metadata)
            || std::fs::canonicalize(path).map_err(io_error)? != *path
        {
            return Err(failure("master_runtime_binding"));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{OwnerClient, OwnerRuntimeHandle};
    use crate::engine::bootstrap::{EngineService, NativeProfileUse, NativeProfileUsePurpose};
    use crate::engine::owner::{
        ModelAccessCollection, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
        OwnerRead, OwnerReadReply, OwnerRequest, OwnerResponse, OwnerResult, PreparedNativeProfile,
    };
    use crate::master_session::{
        MasterBackend, MasterOwnerAction, MasterSelection, MasterTurnPhase,
    };
    use crate::protocol::{
        CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
    };
    use crate::routing::{ResourceBudget, RouteRequest};
    use crate::storage::{Job, MasterRuntimeCommand, MasterRuntimeResult, PreparedMasterTurn};
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["local-profile"]
[routing]
approved_accounts = ["local-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["local-account"]
[[model_access.connections]]
id = "local-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "local-pool"
members = ["local-connection"]
projects = ["rcli-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

    struct Fixture {
        root: tempfile::TempDir,
        service: EngineService,
        host: OwnerRuntimeHandle,
        client: OwnerClient,
        prepared: PreparedMasterTurn,
        profile: NativeProfileUse,
        home: PathBuf,
    }

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn private_directory(path: &Path) {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn private_file(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn git(repository: &Path, home: &Path, arguments: &[&str]) {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args([
                    "-c",
                    "user.name=Master launch fixture",
                    "-c",
                    "user.email=master-launch@example.invalid",
                ])
                .arg("-C")
                .arg(repository)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "actual Git fixture failed: {output:?}"
        );
    }

    async fn read(client: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("launch-helper-read").unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read { query },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual authenticated owner read required");
        };
        state
    }

    /// Applies one owner change as a client must: an outcome left unknown because the job
    /// outlasted the owner timeout is settled by sending the same request again, which the
    /// engine answers from its record. Input: the host, client, request id and change.
    async fn apply(
        host: &OwnerRuntimeHandle,
        client: &OwnerClient,
        id: &str,
        mutation: OwnerMutation,
    ) {
        let basis = read(client, OwnerRead::Capabilities).await;
        let admission = {
            let slots = host.authority.slots.lock().unwrap();
            let slot = slots
                .iter()
                .find(|slot| {
                    slot.registration.kind == OwnerClientKind::Tui && slot.registration.active
                })
                .unwrap();
            slot.registration.admission.clone()
        };
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: basis.revision,
            operation: OwnerOperation::Apply { mutation },
        };
        request.validate().unwrap();
        let (admission, request) = (&admission, &request);
        super::super::master_dispatch::drive_until_settled(
            move || async move {
                let operation = host
                    .engine
                    .owner_job(|reply| Job::OwnerApply(admission.clone(), request.clone(), reply))
                    .await?;
                assert_eq!(operation.result, OwnerResult::Applied);
                Ok(())
            },
            super::super::master_dispatch::DRIVE_PAUSE,
        )
        .await
        .unwrap();
    }

    impl Fixture {
        async fn start() -> Self {
            Self::start_with_timeout(2000).await
        }

        async fn start_with_timeout(timeout_ms: u64) -> Self {
            let config = CONFIG.replace("timeout_ms = 2000", &format!("timeout_ms = {timeout_ms}"));
            assert!(config.contains(&format!("timeout_ms = {timeout_ms}\n")));
            let home_config = serde_json::json!({
                "model": {"provider": "local-provider", "model": "local-model"},
                "providers": [{"id": "local-provider", "name": "Local provider",
                    "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                    "auth": {"type": "none"}}]
            });
            Self::build(&config, &home_config).await
        }

        /// A master whose pool's RCLI key is keyed_connection: a keyed OpenAI account saved
        /// before DEC-150 addendum 9a, registered on an owner home naming its hashed id.
        async fn keyed() -> Self {
            let provider = "id = \"local-connection\"\nprovider = \"openai\"";
            let auth =
                r#"auth = { method = "api_key", credential_ref = "synthetic-key-ref-legacy" }"#;
            let config = CONFIG
                .replace(
                    "id = \"local-connection\"\nprovider = \"local-provider\"",
                    provider,
                )
                .replace(r#"auth = { method = "none" }"#, auth);
            assert!(config.contains(provider) && config.contains(auth));
            let hashed = crate::rcli_host::configured_sdk_connection(&keyed_connection())
                .unwrap()
                .provider;
            let home_config = serde_json::json!({
                "model": {"provider": hashed.as_str(), "model": "local-model"},
                "providers": [{"id": hashed.as_str(), "name": "Local provider",
                    "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                    "auth": {"type": "apiKey"}}]
            });
            Self::build(&config, &home_config).await
        }

        async fn build(config: &str, home_config: &serde_json::Value) -> Self {
            let root = tempfile::Builder::new()
                .prefix("ca-mrl-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let state = root.path().join("state");
            private_directory(&state);
            let global = root.path().join("global.toml");
            private_file(&global, config.as_bytes());
            let home = root.path().join("configured-home");
            private_directory(&home);
            private_directory(&home.join(".backboard"));
            private_file(
                &home.join(".backboard/config.json"),
                &serde_json::to_vec(home_config).unwrap(),
            );
            let repository = root.path().join("repository");
            private_directory(&repository);
            git(
                &repository,
                root.path(),
                &["init", "--initial-branch=trunk"],
            );
            private_file(
                &repository.join("README"),
                b"Actual master launch workspace.\n",
            );
            git(&repository, root.path(), &["add", "README"]);
            git(
                &repository,
                root.path(),
                &["commit", "-m", "Initialize master launch fixture"],
            );
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            let service = EngineService::start_with_settings_sources(
                &state,
                "e.sock",
                &global,
                &root.path().join("project.toml"),
                &stopped,
            )
            .await
            .unwrap();
            let handle = service.owner_runtime().unwrap();
            let host = OwnerRuntimeHandle {
                engine: handle.engine.clone(),
                authority: handle.authority.clone(),
            };
            let client = OwnerClient::attach(
                &host.authority.root.path,
                OwnerClientKind::Tui,
                Duration::from_secs(2),
            )
            .unwrap();
            let project_id = ProjectId::new("rcli-project").unwrap();
            apply(
                &host,
                &client,
                "create-launch-project",
                OwnerMutation::CreateProject {
                    project_id: project_id.clone(),
                    spec: ProjectSpec {
                        name: "Configured launch helper".into(),
                        repositories: vec![repository.to_str().unwrap().into()],
                    },
                },
            )
            .await;
            let basis = read(
                &client,
                OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 1,
                },
            )
            .await;
            let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
                panic!("actual model source basis missing");
            };
            apply(
                &host,
                &client,
                "open-launch-master",
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project_id.clone(),
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(MasterSelection {
                            backend: MasterBackend::Rcli,
                            pool_ref: reference("local-pool"),
                            target_profile_ref: reference("local-profile"),
                            route: RouteRequest {
                                preferred_target: Some(reference("local-profile")),
                                required_target: Some(reference("local-profile")),
                                required_capabilities: vec![],
                                account_ref: reference("local-account"),
                                budget_ref: reference("conversation-budget"),
                                resources: ResourceBudget::unit(),
                            },
                        }),
                    },
                },
            )
            .await;
            let state = read(
                &client,
                OwnerRead::ProjectMaster {
                    project_id: project_id.clone(),
                },
            )
            .await;
            let OwnerData::ProjectMaster {
                session: Some(session),
            } = state.data
            else {
                panic!("actual configured master session missing");
            };
            let session = *session;
            let registered = host
                .engine
                .owner_job(|reply| {
                    Job::MasterRuntime(
                        Box::new(MasterRuntimeCommand::RegisterRcliProfile {
                            host: host.identity(),
                            project_id: project_id.clone(),
                            session_id: session.session_id.clone(),
                            configured_home: home.clone(),
                        }),
                        reply,
                    )
                })
                .await
                .unwrap();
            assert!(matches!(
                registered,
                MasterRuntimeResult::ConfiguredProfile(_)
            ));
            apply(
                &host,
                &client,
                "submit-launch-turn",
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Submit {
                        project_id: project_id.clone(),
                        session_id: session.session_id.clone(),
                        text: "Describe the project without using tools.".into(),
                    },
                },
            )
            .await;
            let state = read(
                &client,
                OwnerRead::ProjectMasterTurns {
                    project_id: project_id.clone(),
                    session_id: session.session_id.clone(),
                    cursor: None,
                    limit: 1,
                },
            )
            .await;
            let OwnerData::ProjectMasterTurns { page } = state.data else {
                panic!("actual queued master turn missing");
            };
            assert_eq!(page.turns.len(), 1);
            let turn = &page.turns[0];
            assert_eq!(turn.phase, MasterTurnPhase::Queued);
            let result = host
                .prepare_master_turn(&project_id, &session.session_id, &turn.id)
                .await
                .unwrap();
            let MasterRuntimeResult::Prepared(prepared) = result else {
                panic!("genuine Core prepared master turn required");
            };
            let (profile, _) = host
                .acquire_master_profile(&prepared, &turn.id)
                .await
                .unwrap();
            profile
                .revalidate(
                    &host.identity(),
                    prepared.profile.binding(),
                    &turn.id,
                    turn.ordinal,
                    NativeProfileUsePurpose::Execution,
                )
                .unwrap();
            assert_eq!(profile.profile_path().unwrap(), home);
            assert!(!home.join(".backboard/keys.json").exists());
            Self {
                root,
                service,
                host,
                client,
                prepared: *prepared,
                profile,
                home,
            }
        }

        fn names(&self) -> BTreeSet<std::ffi::OsString> {
            std::fs::read_dir(&self.host.authority.root.path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect()
        }

        async fn close(mut self) {
            drop(self.profile);
            self.service.shutdown().await.unwrap();
            assert!(self.root.path().is_dir());
        }
    }

    /// The pool's RCLI key of Fixture::keyed, as its settings hold it. The key reference is
    /// synthetic. Output: the connection.
    fn keyed_connection() -> crate::config::ModelConnection {
        crate::config::ModelConnection {
            id: reference("local-connection"),
            provider: reference("openai"),
            harness: reference("rcli"),
            protocol: reference("openai-chat"),
            account_ref: reference("local-account"),
            endpoint: crate::config::ModelEndpoint::Http {
                url: "http://127.0.0.1:11434/v1".into(),
            },
            model: crate::config::ModelReference {
                id: "local-model".into(),
                author: reference("local-author"),
            },
            auth: crate::config::ModelAuthentication::ApiKey {
                credential_ref: reference("synthetic-key-ref-legacy"),
            },
        }
    }

    /// Writes the generated SDK home of a keyed account in the engine root, the fixture's
    /// state folder, with the fixture's config and no key file, as a keyed account saved
    /// before DEC-150 addendum 9a has it. Inputs: the fixture and the account's connection.
    /// Output: the home.
    fn generated_home(case: &Fixture, connection: &crate::config::ModelConnection) -> PathBuf {
        use sha2::{Digest, Sha256};
        let profile = crate::config::configured_profile_ref(connection).unwrap();
        let homes = case.root.path().join("state/configured-sdk-homes");
        if !homes.exists() {
            private_directory(&homes);
        }
        let home = homes.join(format!("{:x}", Sha256::digest(profile.as_str().as_bytes())));
        private_directory(&home);
        private_directory(&home.join(".backboard"));
        private_file(
            &home.join(".backboard/config.json"),
            &std::fs::read(case.home.join(".backboard/config.json")).unwrap(),
        );
        home
    }

    /// Counts the account setups the engine reserved or started and never settled, read
    /// from the engine's own database in the fixture's state folder. Input: the fixture.
    /// Output: the count, zero before any setup made the table.
    fn unsettled_setups(case: &Fixture) -> i64 {
        let database = case.root.path().join("state/engine.db");
        assert!(database.is_file());
        let db = rusqlite::Connection::open(&database).unwrap();
        let table: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='account_profile_operations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if table == 0 {
            return 0;
        }
        db.query_row(
            "SELECT count(*) FROM account_profile_operations WHERE phase IN ('reserved','running')",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// KY-M1: a pooled keyed account saved before DEC-150 addendum 9a has a generated home
    /// holding its config but no key file. When the engine cannot write the saved key there,
    /// the launch stops with the key setup cause and leaves no setup unsettled.
    #[tokio::test]
    async fn a_keyed_master_whose_generated_home_lacks_its_key_stops_with_the_key_setup_cause() {
        let case = Fixture::keyed().await;
        assert_eq!(case.prepared.attempt.connection, keyed_connection());
        let generated = generated_home(&case, &keyed_connection());
        let refused = case
            .host
            .set_up_master_key(&case.prepared)
            .await
            .unwrap_err();
        assert_eq!(refused.0, "rcli_master_key_setup_required");
        assert!(!generated.join(".backboard/keys.json").exists());
        assert_eq!(unsettled_setups(&case), 0);
        case.close().await;
    }

    /// KY-M1: a keyless master, and a keyed one whose generated home already holds its key
    /// file, launch with no key setup.
    #[tokio::test]
    async fn a_master_with_no_key_missing_launches_with_no_key_setup() {
        let case = Fixture::start().await;
        case.host.set_up_master_key(&case.prepared).await.unwrap();
        assert_eq!(unsettled_setups(&case), 0);
        case.close().await;
        let case = Fixture::keyed().await;
        let generated = generated_home(&case, &keyed_connection());
        private_file(&generated.join(".backboard/keys.json"), b"{}");
        case.host.set_up_master_key(&case.prepared).await.unwrap();
        assert_eq!(unsettled_setups(&case), 0);
        case.close().await;
    }

    /// The synthetic API key Fixture::keyed's account has saved, as save_key stores it.
    const SAVED_KEY: &[u8] = b"sk-synthetic-ky-m1-saved-key";

    /// A stand-in for the RCLI host's private key writer: it saves the key it reads after
    /// the length prefix as the home's keys.json and answers with the configured_only ack
    /// the engine's writer protocol expects, echoing the request's id, generation and
    /// provider.
    const KEY_WRITER: &str = r#"#!/bin/sh
[ "$1" = --private-key-write ] || exit 2
umask 077
dd bs=1 skip=4 2>/dev/null >"$HOME/.backboard/keys.json" || exit 3
id=$(printf '%s' "$2" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
generation=$(printf '%s' "$2" | sed 's/.*"generation":\([0-9][0-9]*\).*/\1/')
provider=$(printf '%s' "$2" | sed 's/.*"provider":"\([^"]*\)".*/\1/')
ack="{\"protocol_version\":1,\"request_id\":\"$id\",\"generation\":$generation,\"provider\":\"$provider\",\"status\":\"configured_only\"}"
size=${#ack}
printf "\\000\\000\\$(printf %03o $((size / 256)))\\$(printf %03o $((size % 256)))"
printf '%s' "$ack"
"#;

    /// Saves the keyed account's synthetic key the way the add-account screen does: an
    /// owner-only `account-keys` folder in the engine root holding an owner-only file named
    /// by the key reference. Input: the fixture. Output: none.
    fn save_key(case: &Fixture) {
        let folder = case
            .root
            .path()
            .join("state")
            .join(crate::storage::accounts::ACCOUNT_KEYS);
        if !folder.exists() {
            private_directory(&folder);
        }
        private_file(&folder.join("synthetic-key-ref-legacy"), SAVED_KEY);
    }

    /// Writes KEY_WRITER as an owner-only executable in the fixture's private folder.
    /// Input: the fixture. Output: its path, for EngineService::register_rcli_executable.
    fn key_writer(case: &Fixture) -> PathBuf {
        let path = case.root.path().join("rcli-key-writer");
        private_file(&path, KEY_WRITER.as_bytes());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    /// KY-M1: a pooled pre-fix keyed account whose key is saved in the engine's key store
    /// gets that key written into its generated home in the engine root before the master
    /// launch, through the Configure flow and the registered key writer (a stand-in here),
    /// and leaves no setup unsettled.
    #[tokio::test]
    async fn a_keyed_master_whose_generated_home_lacks_its_key_gets_its_saved_key_written() {
        let mut case = Fixture::keyed().await;
        let generated = generated_home(&case, &keyed_connection());
        save_key(&case);
        let writer = key_writer(&case);
        case.service.register_rcli_executable(&writer).unwrap();
        case.host.set_up_master_key(&case.prepared).await.unwrap();
        assert_eq!(
            std::fs::read(generated.join(".backboard/keys.json")).unwrap(),
            SAVED_KEY
        );
        assert_eq!(unsettled_setups(&case), 0);
        case.close().await;
    }

    fn copy_prepared(prepared: &PreparedMasterTurn) -> PreparedMasterTurn {
        PreparedMasterTurn {
            text: prepared.text.clone(),
            message: prepared.message.clone(),
            owner_turn: prepared.owner_turn,
            instructions: prepared.instructions.clone(),
            tools: prepared.tools,
            session: prepared.session.clone(),
            attempt: prepared.attempt.clone(),
            profile: PreparedNativeProfile::from_binding(prepared.profile.binding().clone()),
            repository: prepared.repository.clone(),
            cwd: prepared.cwd.clone(),
            worktree_lease: prepared.worktree_lease.clone(),
            lease: prepared.lease.clone(),
            agent_project: prepared.agent_project.clone(),
            agent_policy: prepared.agent_policy.clone(),
            hidden_paths: prepared.hidden_paths.clone(),
            tools_off: prepared.tools_off.clone(),
            steps: prepared.steps,
            time: prepared.time,
            frames: prepared.frames,
        }
    }

    #[tokio::test]
    async fn rcli_launch_prepares_when_worktree_provisioning_outlasts_the_owner_timeout() {
        let case = Fixture::start_with_timeout(100).await;
        assert_eq!(case.prepared.cwd, PathBuf::from(&case.prepared.lease.path));
        assert!(case.prepared.cwd.join(".git").is_file());
        assert_eq!(case.profile.profile_path().unwrap(), case.home);
        case.close().await;
    }

    /// Holds the engine database's write lock from a second real connection for `hold`, as
    /// a slow disk does to the engine's own commits. Input: the database and the hold.
    /// Output: the holding thread, returned once the lock is held.
    fn hold_writes(database: &Path, hold: Duration) -> std::thread::JoinHandle<()> {
        let database = database.to_path_buf();
        let (held, wait) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            held.send(()).unwrap();
            std::thread::sleep(hold);
            connection.execute_batch("COMMIT").unwrap();
        });
        wait.recv().unwrap();
        holder
    }

    #[tokio::test]
    async fn a_launch_setup_change_slower_than_the_owner_timeout_settles_on_the_same_request() {
        let case = Fixture::start_with_timeout(100).await;
        let database = case.root.path().join("state/engine.db");
        assert!(database.is_file());
        let before = read(&case.client, OwnerRead::Capabilities).await.revision;
        let holder = hold_writes(&database, Duration::from_millis(200));
        let started = std::time::Instant::now();
        apply(
            &case.host,
            &case.client,
            "create-while-the-disk-is-slow",
            OwnerMutation::CreateProject {
                project_id: ProjectId::new("second-project").unwrap(),
                spec: ProjectSpec {
                    name: "Second project".into(),
                    repositories: vec![
                        case.root.path().join("repository").to_str().unwrap().into(),
                    ],
                },
            },
        )
        .await;
        assert!(started.elapsed() >= Duration::from_millis(100));
        holder.join().unwrap();
        let after = read(&case.client, OwnerRead::Capabilities).await.revision;
        assert_eq!(
            after.value(),
            before.value() + 1,
            "the repeated request applied once"
        );
        case.close().await;
    }

    #[tokio::test]
    async fn rcli_launch_rejects_changed_retained_owner_authority() {
        let case = Fixture::start().await;
        let before = case.names();
        std::fs::set_permissions(
            &case.host.authority.root.path,
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        let result = case
            .host
            .prepare_rcli_master_host(&case.prepared, &case.profile);
        std::fs::set_permissions(
            &case.host.authority.root.path,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(result.unwrap_err().0, "owner_root_replaced_or_unsafe");
        for database in [false, true] {
            let mut prepared = copy_prepared(&case.prepared);
            let mut binding = prepared.profile.binding().clone();
            if database {
                binding.database_instance = "foreign-database".into();
            } else {
                binding.engine_epoch = "foreign-epoch".into();
            }
            prepared.profile = PreparedNativeProfile::from_binding(binding);
            assert_eq!(
                case.host
                    .prepare_rcli_master_host(&prepared, &case.profile)
                    .unwrap_err()
                    .0,
                "owner_runtime_stale"
            );
        }
        assert_eq!(case.names(), before);
        case.close().await;
    }

    #[tokio::test]
    async fn rcli_launch_rejects_prepared_provenance_drift() {
        let case = Fixture::start().await;
        let before = case.names();
        for field in 0..10 {
            let mut prepared = copy_prepared(&case.prepared);
            match field {
                0 => prepared.session.current_turn = None,
                1 => {
                    prepared.attempt.request.project_id = ProjectId::new("foreign-project").unwrap()
                }
                2 => {
                    prepared.lease.owner_session =
                        crate::protocol::SessionId::new("foreign-session").unwrap()
                }
                3 => prepared.lease.path = case.root.path().to_str().unwrap().into(),
                4 => prepared.lease.repository = case.home.to_str().unwrap().into(),
                5 => prepared.lease.state = crate::worktrees::LeaseState::Reserved,
                6 => prepared.session.source_hash = "changed-source".into(),
                7 => prepared.attempt.connection.account_ref = reference("foreign-account"),
                8 => prepared.session.selection.target_profile_ref = reference("foreign-profile"),
                9 => prepared.session.selection.pool_ref = reference("foreign-pool"),
                _ => unreachable!(),
            }
            assert_eq!(
                case.host
                    .prepare_rcli_master_host(&prepared, &case.profile)
                    .unwrap_err()
                    .0,
                "master_runtime_binding",
                "prepared provenance field {field} was not rejected"
            );
        }
        assert_eq!(case.names(), before);
        case.close().await;
    }

    #[tokio::test]
    async fn rcli_launch_rejects_replaced_retained_profile_directory() {
        let case = Fixture::start().await;
        let before = case.names();
        let sdk = case.home.join(".backboard");
        let retained = case.home.join("retained-backboard");
        let bytes = std::fs::read(sdk.join("config.json")).unwrap();
        std::fs::rename(&sdk, &retained).unwrap();
        private_directory(&sdk);
        private_file(&sdk.join("config.json"), &bytes);
        let result = case
            .host
            .prepare_rcli_master_host(&case.prepared, &case.profile);
        std::fs::remove_file(sdk.join("config.json")).unwrap();
        std::fs::remove_dir(&sdk).unwrap();
        std::fs::rename(&retained, &sdk).unwrap();
        assert_eq!(result.unwrap_err().0, "native_profile_custody_unsafe");
        case.profile.revalidate_retained().unwrap();
        assert_eq!(case.names(), before);
        case.close().await;
    }

    #[tokio::test]
    async fn rcli_launch_requires_registered_artifact_without_capture_allocation() {
        let case = Fixture::start().await;
        let before = case.names();
        assert_eq!(
            case.host
                .registered_rcli_master_executable()
                .err()
                .unwrap()
                .0,
            "native_auth_host_unavailable"
        );
        assert_eq!(
            case.host
                .prepare_rcli_master_host(&case.prepared, &case.profile)
                .unwrap_err()
                .0,
            "native_auth_host_unavailable"
        );
        assert_eq!(case.names(), before);
        let current = case
            .host
            .inspect_master_turn(
                &case.prepared.session.project_id,
                &case.prepared.session.session_id,
                case.prepared.session.current_turn.as_ref().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(current.phase, MasterTurnPhase::Delivering);
        assert_eq!(current.attempt.as_ref(), Some(&case.prepared.attempt));
        assert_eq!(current.runtime, None);
        assert_eq!(current.reply, None);
        case.profile.revalidate_retained().unwrap();
        case.close().await;
    }

    #[tokio::test]
    async fn rcli_launch_rejects_changed_retained_config_binding() {
        let case = Fixture::start().await;
        let before = case.names();
        let config = case.home.join(".backboard/config.json");
        let original = std::fs::read(&config).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        changed["model"]["model"] = "changed-model".into();
        private_file(&config, &serde_json::to_vec(&changed).unwrap());
        let result = case
            .host
            .prepare_rcli_master_host(&case.prepared, &case.profile);
        assert_eq!(result.unwrap_err().0, "rcli_profile_config_changed");
        assert_eq!(
            case.profile.revalidate_retained().unwrap_err().0,
            "rcli_profile_config_changed"
        );
        assert_eq!(case.names(), before);
        case.close().await;
    }
}
