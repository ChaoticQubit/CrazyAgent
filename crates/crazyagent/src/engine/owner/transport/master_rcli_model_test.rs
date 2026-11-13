use super::custody::RcliCustody;
use super::{EngineError, OwnerRuntimeHandle, TURN_DEADLINE, failure};
use crate::config::{EffectiveConfig, ModelConnection};
use crate::engine::bootstrap::{NativeProfileCustody, generated_home_needs_key};
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
use crate::engine::owner::{is_hex_id, random_id};
use crate::model_choice::test_launch::{
    BUSY, NotStarted, POOL_UNKNOWN, PROMPT, RcliEnded, Slot, Tested, WALL_TIME, rcli_not_started,
    rcli_outcome, rcli_pick,
};
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use crate::rcli_host::{
    RcliMasterBinding, RcliMasterLaunch, RcliMasterStatus, checked_configured_profile,
};
use std::cell::RefCell;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const UNAVAILABLE: &str = "model_choice_unavailable";
const PROJECT: &str = "model-test";
const TURN: &str = "model-test-turn";
const WORK: &str = "work";
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One RCLI test launch's settled inputs: the runtime handle, the engine database, the
/// settings and their files, the picked account, the RCLI executable, the owner's id and
/// the provider it is tested on.
struct Test<'a> {
    host: &'a OwnerRuntimeHandle,
    db: &'a rusqlite::Connection,
    config: &'a EffectiveConfig,
    settings: (&'a Path, &'a Path),
    connection: &'a ModelConnection,
    executable: &'a RegisteredNativeAuthHost,
    model: &'a str,
    provider: &'a str,
}

/// Runs one RCLI test launch (DEC-150 addendum 9e, leader 19:01): the fixed prompt as one
/// turn of RCLI's closed host, with no tools and no project, on the pool's first set-up
/// keyed account of the id's provider that no limit paused, with the owner's id as its
/// model, under that account's configured profile in the narrowest master sandbox. Only
/// the decision is kept. Inputs: the runtime handle, the engine folder, the settings files,
/// the id, its provider and the pool. Output: the decision and its pending reason, or
/// `BUSY`, `POOL_UNKNOWN`, `model_choice_provider_unknown` or unavailable.
pub(in crate::engine::owner::transport) fn run(
    host: &OwnerRuntimeHandle,
    root: &Path,
    settings: (&Path, &Path),
    model: &str,
    provider: &str,
    pool: &CredentialRef,
) -> Result<(Tested, Option<String>), &'static str> {
    if crate::model_choice::rcli_provider_name(provider).is_none() {
        return Err("model_choice_provider_unknown");
    }
    let root = root.canonicalize().map_err(|_| UNAVAILABLE)?;
    let _slot = Slot::take(&root, "rcli").ok_or(BUSY)?;
    let pending = |why| Ok((Tested::Pending, Some(rcli_not_started(provider, why))));
    let executable = match host.registered_rcli_master_executable() {
        Ok(executable) => executable,
        Err(error) if error.0 == "native_auth_host_unavailable" => {
            return pending(NotStarted::NotInstalled);
        }
        Err(_) => return Err(UNAVAILABLE),
    };
    let (global, project) = settings;
    let config = crate::config::resolve_config(global, project).map_err(|_| UNAVAILABLE)?;
    let Some(access) = config.settings.model_access.as_ref() else {
        return Err(POOL_UNKNOWN);
    };
    let Some(chosen) = access.pools.iter().find(|candidate| &candidate.id == pool) else {
        return Err(POOL_UNKNOWN);
    };
    let members: Vec<&ModelConnection> = chosen
        .members
        .iter()
        .filter_map(|id| access.connections.iter().find(|item| &item.id == id))
        .collect();
    assert!(members.len() <= crate::config::POOL_MEMBERS_MAX);
    let db = crate::storage::versioned::open(&root).map_err(|_| UNAVAILABLE)?;
    let database = root.join("engine.db");
    let opened = std::fs::metadata(&database).map_err(|_| UNAVAILABLE)?;
    let custody = NativeProfileCustody::capture(&database, &opened).map_err(|_| UNAVAILABLE)?;
    let custody = RefCell::new(custody);
    let stamp = host.identity();
    let set_up = |connection: &ModelConnection| {
        let Some(profile) = crate::config::configured_profile_ref(connection) else {
            return false;
        };
        matches!(generated_home_needs_key(&root, &profile), Ok(false))
            && custody
                .borrow_mut()
                .configured_rcli_profile(&stamp, connection, &profile)
                .is_ok()
    };
    let paused = |connection: &ModelConnection| {
        !matches!(crate::team::held_until(&db, "", &connection.id), Ok(None))
    };
    let connection = match rcli_pick(&members, provider, set_up, paused) {
        Ok(connection) => connection,
        Err(why) => return pending(why),
    };
    let test = Test {
        host,
        db: &db,
        config: &config,
        settings,
        connection,
        executable: &executable,
        model,
        provider,
    };
    match launched(&test, custody.into_inner()) {
        Ok(ended) => Ok(rcli_outcome(provider, ended)),
        Err(error) => {
            eprintln!("model_choice_test_failed harness=rcli cause={}", error.0);
            pending(NotStarted::CouldNotStart)
        }
    }
}

/// Starts the test launch's closed RCLI host on the picked account and waits for its one
/// turn, the way a master turn holds and checks that account's profile, then shuts it down.
/// The turn folder outlives the host. Inputs: the settled inputs and custody. Output: how
/// the run ended, or the refusal that kept it from starting or ending cleanly.
fn launched(test: &Test<'_>, mut custody: NativeProfileCustody) -> Result<RcliEnded, EngineError> {
    let host = test.host;
    let connection = test.connection;
    let binding_failure = || failure("master_runtime_binding");
    let stamp = host.identity();
    let profile_ref =
        crate::config::configured_profile_ref(connection).ok_or_else(binding_failure)?;
    let metadata = custody.configured_rcli_profile(&stamp, connection, &profile_ref)?;
    if metadata.profile_ref != profile_ref
        || metadata.profile_generation == 0
        || !is_hex_id(&metadata.custody_id)
        || !is_hex_id(&metadata.profile_config_sha256)
    {
        return Err(binding_failure());
    }
    let binding = crate::storage::account_profile::rcli_test_binding(
        test.db,
        test.config,
        test.settings,
        &stamp,
        connection,
        &metadata.custody_id,
    )?;
    let operation =
        RequestId::new(format!("model-test-{}", random_id()?)).map_err(|_| binding_failure())?;
    let profile = custody.acquire_configured_rcli_use(
        &stamp,
        &binding,
        &profile_ref,
        &operation,
        metadata.profile_generation,
    )?;
    profile.revalidate_retained()?;
    let home_dir = profile.profile_path()?;
    let profile_config_sha256 =
        checked_configured_profile(&home_dir, connection).map_err(|error| EngineError(error.0))?;
    profile.revalidate_retained()?;
    if profile_config_sha256 != metadata.profile_config_sha256 {
        return Err(failure("rcli_master_profile_changed"));
    }
    let executable = test.executable.verified_path()?.to_path_buf();
    let artifact_sha256 = test.executable.verified_sha256()?;
    if !is_hex_id(&artifact_sha256) {
        return Err(binding_failure());
    }
    if !host.authority.usable.load(Ordering::SeqCst) {
        return Err(failure("owner_runtime_stale"));
    }
    let root = &host.authority.root;
    root.verify()?;
    let (runtime_ref, _, capture) =
        super::super::master_host_launch::allocate_master_capture(root)?;
    drop(capture);
    let _folder = super::super::master_host_launch::MasterRuntimeFolder::new(root, &runtime_ref);
    let work = root
        .path
        .join(format!("master-runtime-{}", runtime_ref.as_str()))
        .join(WORK);
    let private = |path: &Path| {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(crate::engine::io_error)
    };
    private(&work)?;
    private(&work.join(".git"))?;
    let session = SessionId::new(format!("model-test-{}", runtime_ref.as_str()))
        .map_err(|_| binding_failure())?;
    let launch = RcliMasterLaunch {
        binding: RcliMasterBinding {
            project_id: ProjectId::new(PROJECT).map_err(|_| binding_failure())?,
            session_id: session,
            instance_id: RequestId::new(random_id()?).map_err(|_| binding_failure())?,
            connection: connection.clone(),
            target_profile_ref: profile_ref,
        },
        executable,
        artifact_sha256,
        home_dir,
        cwd: work.clone(),
        profile_config_sha256,
        instructions: None,
        model: Some(test.model.to_owned()),
        model_provider: Some(test.provider.to_owned()),
    };
    launch.command().map_err(|error| EngineError(error.0))?;
    let sandbox = super::super::master_host_launch::model_test_sandbox(
        root,
        &runtime_ref,
        &work,
        &launch.home_dir,
        &launch.executable,
        &connection.endpoint,
    )?;
    let deadline = Instant::now() + WALL_TIME;
    let mut custody = RcliCustody::launch(launch, sandbox, profile, Some(WALL_TIME))?;
    let turn = RequestId::new(TURN).map_err(|_| binding_failure())?;
    let ended = wait(&mut custody, &turn, deadline);
    let settled = matches!(ended, Ok(RcliEnded::Answered | RcliEnded::Failed(_)));
    let retired = custody.retire((!settled).then_some(&turn));
    let ended = ended?;
    retired?;
    Ok(ended)
}

/// Prompts the host's one turn once it is ready and reads how it ended, never keeping the
/// reply's text. Inputs: the host in custody, the turn id and the run's deadline. Output:
/// the ending, `TimedOut` at the deadline, or the host's refusal.
fn wait(
    custody: &mut RcliCustody,
    turn: &RequestId,
    deadline: Instant,
) -> Result<RcliEnded, EngineError> {
    let mut prompted = false;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(RcliEnded::TimedOut);
        }
        let observed = match custody.poll() {
            Ok(Some(observed)) => observed,
            Ok(None) => {
                std::thread::sleep(POLL_INTERVAL.min(deadline - now));
                continue;
            }
            Err(error) if error.0 == TURN_DEADLINE => return Ok(RcliEnded::TimedOut),
            Err(error) => return Err(error),
        };
        if observed.is_ready() {
            if prompted {
                return Err(failure("rcli_master_message_order"));
            }
            custody.prompt(turn.clone(), PROMPT)?;
            prompted = true;
        }
        let Some(completion) = observed.completion else {
            continue;
        };
        if !prompted || completion.turn_id != *turn {
            return Err(failure("rcli_master_turn_scope"));
        }
        let answered = completion
            .reply
            .as_deref()
            .is_some_and(|reply| !reply.trim().is_empty());
        return Ok(match completion.status {
            RcliMasterStatus::Succeeded if answered => RcliEnded::Answered,
            RcliMasterStatus::Succeeded => RcliEnded::Failed(None),
            RcliMasterStatus::Cancelled => RcliEnded::Stopped,
            RcliMasterStatus::Failed => RcliEnded::Failed(completion.failure),
        });
    }
}
