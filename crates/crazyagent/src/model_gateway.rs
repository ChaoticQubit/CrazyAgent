use crate::config::{EffectiveConfig, ModelConnection, ProviderQuotaPolicy};
use crate::coordination::Role;
use crate::engine::EngineError;
use crate::protocol::{ConfigId, CredentialRef, ProjectId, RequestId, TaskId};
use crate::routing::{ResourceBudget, RouteRequest, validate_pool_scope};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[path = "storage/model_gateway.rs"]
mod persistence;

const OBSERVATION_LIFETIME_MAX_MS: u64 = 86_400_000;
const ATTEMPTS_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ObservationKind {
    Ready {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining: Option<Quota>,
        capabilities: Vec<String>,
    },
    QuotaExhausted {
        retry_at_ms: Option<u64>,
    },
    RateLimited {
        retry_at_ms: Option<u64>,
    },
    InvalidAuthentication,
    TransientFailure {
        retry_at_ms: Option<u64>,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: RequestId,
    pub connection: ModelConnection,
    pub sequence: u64,
    pub observed_at_ms: u64,
    pub valid_until_ms: u64,
    pub kind: ObservationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    Unknown,
    Ready,
    RateLimited,
    InvalidAuthentication,
    TransientFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quota {
    Unknown,
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionState {
    pub health: Health,
    pub quota: Quota,
    pub cooldown_until_ms: Option<u64>,
    pub observed_at_ms: Option<u64>,
    pub valid_until_ms: Option<u64>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    QuotaExhausted,
    RateLimited,
    InvalidAuthentication,
    TransientFailure,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptOutcome {
    Succeeded,
    Failed(FailureKind),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effects {
    None,
    Committed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptCompletion {
    pub outcome: AttemptOutcome,
    pub effects: Effects,
    pub finished_at_ms: u64,
    pub retry_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRequest {
    pub id: RequestId,
    pub task_id: TaskId,
    pub project_id: ProjectId,
    pub role: Role,
    pub pool_ref: CredentialRef,
    pub target_profile_ref: CredentialRef,
    pub route: RouteRequest,
    pub previous_attempt: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub request: AttemptRequest,
    pub config_id: ConfigId,
    pub connection: ModelConnection,
    pub connection_revision: String,
    pub selected_at_ms: u64,
    pub reasons: Vec<String>,
    pub completion: Option<AttemptCompletion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptStart {
    pub attempt: Attempt,
    pub replayed: bool,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    persistence::initialize(connection)
}

fn connection_revision(connection: &ModelConnection) -> Result<String, EngineError> {
    Ok(crate::memory::hash(
        &serde_json::to_vec(connection).map_err(error)?,
    ))
}

fn retry_at(kind: &ObservationKind) -> Option<u64> {
    match kind {
        ObservationKind::QuotaExhausted { retry_at_ms }
        | ObservationKind::RateLimited { retry_at_ms }
        | ObservationKind::TransientFailure { retry_at_ms } => *retry_at_ms,
        _ => None,
    }
}

fn validate_observation(observation: &Observation) -> Result<(), EngineError> {
    if observation.sequence == 0
        || observation.sequence > i64::MAX as u64
        || observation.observed_at_ms >= observation.valid_until_ms
        || observation.valid_until_ms > i64::MAX as u64
        || observation.valid_until_ms - observation.observed_at_ms > OBSERVATION_LIFETIME_MAX_MS
        || retry_at(&observation.kind).is_some_and(|retry| {
            retry <= observation.observed_at_ms || retry > observation.valid_until_ms
        })
    {
        return Err(error("gateway_observation_bounds"));
    }
    if let ObservationKind::Ready { capabilities, .. } = &observation.kind {
        let validation = RouteRequest {
            preferred_target: None,
            required_target: None,
            required_capabilities: capabilities.clone(),
            account_ref: observation.connection.account_ref.clone(),
            budget_ref: observation.connection.id.clone(),
            resources: ResourceBudget::unit(),
        };
        validation.validate().map_err(error)?;
    }
    assert!(observation.sequence > 0);
    assert!(observation.valid_until_ms > observation.observed_at_ms);
    Ok(())
}

pub fn observe(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    observation: &Observation,
) -> Result<(), EngineError> {
    config.validate().map_err(error)?;
    validate_observation(observation)?;
    if !config
        .settings
        .model_access
        .as_ref()
        .is_some_and(|catalog| {
            catalog
                .connections
                .iter()
                .any(|connection| connection == &observation.connection)
        })
    {
        return Err(error("gateway_observation_connection_changed"));
    }
    persistence::insert_observation(tx, observation)
}

pub fn connection_state(
    connection: &Connection,
    model: &ModelConnection,
    now_ms: u64,
) -> Result<ConnectionState, EngineError> {
    let revision = connection_revision(model)?;
    let observation = persistence::latest_observation(connection, &revision)?;
    let mut state = ConnectionState {
        health: Health::Unknown,
        quota: Quota::Unknown,
        cooldown_until_ms: None,
        observed_at_ms: observation.as_ref().map(|value| value.observed_at_ms),
        valid_until_ms: observation.as_ref().map(|value| value.valid_until_ms),
        capabilities: vec![],
    };
    let Some(observation) = observation else {
        return Ok(state);
    };
    validate_observation(&observation)?;
    if now_ms < observation.observed_at_ms || now_ms >= observation.valid_until_ms {
        return Ok(state);
    }
    state.cooldown_until_ms =
        persistence::active_cooldown(connection, &revision, &observation, now_ms)?;
    match observation.kind {
        ObservationKind::Ready {
            remaining,
            capabilities,
        } => {
            state.health = Health::Ready;
            state.quota = remaining.unwrap_or(Quota::Unknown);
            state.capabilities = capabilities;
        }
        ObservationKind::QuotaExhausted { .. } => {
            state.health = Health::Ready;
            state.quota = Quota::Exhausted;
        }
        ObservationKind::RateLimited { .. } => state.health = Health::RateLimited,
        ObservationKind::InvalidAuthentication => state.health = Health::InvalidAuthentication,
        ObservationKind::TransientFailure { .. } => state.health = Health::TransientFailure,
        ObservationKind::Unknown => {}
    }
    assert_eq!(state.observed_at_ms, Some(observation.observed_at_ms));
    assert!(state.cooldown_until_ms.is_none_or(|until| until > now_ms));
    Ok(state)
}

fn validate_authority(
    config: &EffectiveConfig,
    request: &AttemptRequest,
) -> Result<(), EngineError> {
    config.validate().map_err(error)?;
    request.route.validate().map_err(error)?;
    validate_pool_scope(
        config,
        &request.pool_ref,
        &request.project_id,
        &request.role,
        &request.route.account_ref,
    )
    .map_err(error)?;
    if !config
        .settings
        .allowed_destinations
        .contains(&request.target_profile_ref)
        || config
            .settings
            .revoked_destinations
            .contains(&request.target_profile_ref)
        || request
            .route
            .required_target
            .as_ref()
            .is_some_and(|required| required != &request.target_profile_ref)
        || !request
            .route
            .resources
            .fits(&config.settings.routing.task_capacity)
    {
        return Err(error("gateway_route_denied"));
    }
    if !config.settings.routing.fallback_allowed
        && request.route.required_target.is_none()
        && request
            .route
            .preferred_target
            .as_ref()
            .is_some_and(|preferred| preferred != &request.target_profile_ref)
    {
        return Err(error("gateway_fallback_denied"));
    }
    assert!(
        request
            .route
            .resources
            .fits(&config.settings.routing.task_capacity)
    );
    assert!(
        !config
            .settings
            .revoked_destinations
            .contains(&request.target_profile_ref)
    );
    Ok(())
}

fn live_connection(
    current: &EffectiveConfig,
    request: &AttemptRequest,
    connection: &ModelConnection,
) -> Result<(), EngineError> {
    validate_pool_scope(
        current,
        &request.pool_ref,
        &request.project_id,
        &request.role,
        &connection.account_ref,
    )
    .map_err(error)?;
    let catalog = current.settings.model_access.as_ref().unwrap();
    let pool = catalog
        .pools
        .iter()
        .find(|pool| pool.id == request.pool_ref)
        .unwrap();
    if !pool.members.contains(&connection.id)
        || !catalog.connections.iter().any(|live| live == connection)
    {
        return Err(error("gateway_connection_changed_or_denied"));
    }
    assert!(catalog.accounts.contains(&connection.account_ref));
    assert!(pool.members.contains(&connection.id));
    Ok(())
}

fn eligible(
    state: &ConnectionState,
    route: &RouteRequest,
    policy: ProviderQuotaPolicy,
) -> Result<(), EngineError> {
    match state.health {
        Health::Unknown => return Err(error("gateway_health_unknown")),
        Health::InvalidAuthentication => return Err(error("gateway_invalid_authentication")),
        Health::RateLimited => return Err(error("gateway_rate_limited")),
        Health::TransientFailure => return Err(error("gateway_transient_failure")),
        Health::Ready => {}
    }
    match &state.quota {
        Quota::Unknown if policy == ProviderQuotaPolicy::Strict => {
            return Err(error("gateway_quota_unknown"));
        }
        Quota::Exhausted => return Err(error("gateway_quota_exhausted")),
        Quota::Unknown => {}
    }
    if state.cooldown_until_ms.is_some() {
        return Err(error("gateway_cooldown"));
    }
    if !route
        .required_capabilities
        .iter()
        .all(|required| state.capabilities.contains(required))
    {
        return Err(error("gateway_capability_unavailable"));
    }
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(policy, ProviderQuotaPolicy::BoundedAttempt);
    Ok(())
}

fn previous_accounts(
    tx: &Transaction<'_>,
    pinned: &EffectiveConfig,
    request: &AttemptRequest,
    now_ms: u64,
) -> Result<BTreeSet<CredentialRef>, EngineError> {
    let mut accounts = BTreeSet::new();
    let mut previous = request.previous_attempt.clone();
    while let Some(id) = previous {
        if accounts.len() >= ATTEMPTS_MAX {
            return Err(error("gateway_attempt_limit"));
        }
        let prior =
            read_attempt(tx, &id)?.ok_or_else(|| error("gateway_previous_attempt_missing"))?;
        let mut expected = request.clone();
        expected.id = prior.request.id.clone();
        expected.previous_attempt = prior.request.previous_attempt.clone();
        if prior.request != expected || prior.config_id != pinned.id {
            return Err(error("gateway_previous_attempt_mismatch"));
        }
        let completion = prior
            .completion
            .as_ref()
            .ok_or_else(|| error("gateway_attempt_active"))?;
        if completion.outcome != AttemptOutcome::Failed(FailureKind::QuotaExhausted)
            || completion.effects != Effects::None
            || completion.finished_at_ms > now_ms
        {
            return Err(error("gateway_failover_unsafe"));
        }
        if !accounts.insert(prior.connection.account_ref.clone()) {
            return Err(error("gateway_attempt_cycle"));
        }
        previous = prior.request.previous_attempt;
    }
    assert!(accounts.len() <= ATTEMPTS_MAX);
    assert_eq!(accounts.is_empty(), request.previous_attempt.is_none());
    Ok(accounts)
}

pub fn begin_attempt(
    tx: &Transaction<'_>,
    pinned: &EffectiveConfig,
    current: &EffectiveConfig,
    request: &AttemptRequest,
    now_ms: u64,
) -> Result<AttemptStart, EngineError> {
    if now_ms > i64::MAX as u64 {
        return Err(error("gateway_time_bounds"));
    }
    validate_authority(pinned, request)?;
    validate_authority(current, request)?;
    if let Some(attempt) = read_attempt(tx, &request.id)? {
        if attempt.request != *request || attempt.config_id != pinned.id {
            return Err(error("gateway_attempt_conflict"));
        }
        live_connection(current, request, &attempt.connection)?;
        return Ok(AttemptStart {
            attempt,
            replayed: true,
        });
    }
    let previous = previous_accounts(tx, pinned, request, now_ms)?;
    persistence::validate_attempt_chain(tx, request)?;
    let catalog = pinned.settings.model_access.as_ref().unwrap();
    let pool = catalog
        .pools
        .iter()
        .find(|pool| pool.id == request.pool_ref)
        .unwrap();
    let live_pool = current
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .pools
        .iter()
        .find(|pool| pool.id == request.pool_ref)
        .unwrap();
    let fallback = pool.fallback_allowed
        && live_pool.fallback_allowed
        && pinned.settings.routing.fallback_allowed
        && current.settings.routing.fallback_allowed;
    let policy = if pool.provider_quota_policy == ProviderQuotaPolicy::Strict
        || live_pool.provider_quota_policy == ProviderQuotaPolicy::Strict
    {
        ProviderQuotaPolicy::Strict
    } else {
        ProviderQuotaPolicy::BoundedAttempt
    };
    if !previous.is_empty() && !fallback {
        return Err(error("gateway_fallback_denied"));
    }
    let mut candidates: Vec<_> = pool
        .members
        .iter()
        .filter_map(|id| {
            catalog
                .connections
                .iter()
                .find(|connection| &connection.id == id)
        })
        .collect();
    candidates.sort_by_key(|connection| connection.account_ref != request.route.account_ref);
    let preferred = candidates
        .iter()
        .find(|connection| connection.account_ref == request.route.account_ref)
        .ok_or_else(|| error("gateway_preferred_account_missing"))?;
    let mut quota_fallback = !previous.is_empty();
    let mut unavailable = error("gateway_no_eligible_account");
    for connection in candidates.iter().copied() {
        if previous.contains(&connection.account_ref) {
            continue;
        }
        let same_account = connection.account_ref == request.route.account_ref;
        if !same_account && (!fallback || !quota_fallback) {
            continue;
        }
        if connection.model != preferred.model
            || connection.harness != preferred.harness
            || connection.protocol != preferred.protocol
        {
            unavailable = error("gateway_incompatible_connection");
            continue;
        }
        if let Err(failure) = live_connection(current, request, connection) {
            unavailable = failure;
            continue;
        }
        let state = connection_state(tx, connection, now_ms)?;
        if let Err(failure) = eligible(&state, &request.route, policy) {
            if same_account && state.quota == Quota::Exhausted {
                quota_fallback = true;
            }
            unavailable = failure;
            continue;
        }
        let reason = if same_account {
            "preferred_account"
        } else {
            "quota_failover"
        };
        let attempt = Attempt {
            request: request.clone(),
            config_id: pinned.id.clone(),
            connection: connection.clone(),
            connection_revision: connection_revision(connection)?,
            selected_at_ms: now_ms,
            reasons: vec![reason.into(), "bounded_attempt_quota_unknown".into()],
            completion: None,
        };
        persistence::insert_attempt(tx, &attempt)?;
        assert_eq!(attempt.config_id, pinned.id);
        assert!(pool.members.contains(&attempt.connection.id));
        return Ok(AttemptStart {
            attempt,
            replayed: false,
        });
    }
    Err(unavailable)
}

pub fn finish_attempt(
    tx: &Transaction<'_>,
    id: &RequestId,
    completion: &AttemptCompletion,
) -> Result<(), EngineError> {
    let attempt = read_attempt(tx, id)?.ok_or_else(|| error("gateway_attempt_missing"))?;
    if let Some(previous) = &attempt.completion {
        return if previous == completion {
            Ok(())
        } else {
            Err(error("gateway_completion_conflict"))
        };
    }
    if completion.finished_at_ms < attempt.selected_at_ms
        || completion.finished_at_ms > i64::MAX as u64 - OBSERVATION_LIFETIME_MAX_MS
        || completion.retry_at_ms.is_some_and(|retry| {
            retry <= completion.finished_at_ms
                || retry - completion.finished_at_ms > OBSERVATION_LIFETIME_MAX_MS
        })
        || matches!(
            completion.outcome,
            AttemptOutcome::Succeeded
                | AttemptOutcome::Failed(FailureKind::InvalidAuthentication | FailureKind::Unknown)
        ) && completion.retry_at_ms.is_some()
    {
        return Err(error("gateway_completion_bounds"));
    }
    if let AttemptOutcome::Failed(kind) = &completion.outcome {
        let kind = match kind {
            FailureKind::QuotaExhausted => ObservationKind::QuotaExhausted {
                retry_at_ms: completion.retry_at_ms,
            },
            FailureKind::RateLimited => ObservationKind::RateLimited {
                retry_at_ms: completion.retry_at_ms,
            },
            FailureKind::InvalidAuthentication => ObservationKind::InvalidAuthentication,
            FailureKind::TransientFailure => ObservationKind::TransientFailure {
                retry_at_ms: completion.retry_at_ms,
            },
            FailureKind::Unknown => ObservationKind::Unknown,
        };
        let latest = persistence::latest_observation(tx, &attempt.connection_revision)?;
        let sequence = latest
            .map_or(Some(1), |value| value.sequence.checked_add(1))
            .ok_or_else(|| error("gateway_observation_bounds"))?;
        let observation = Observation {
            id: RequestId::new(format!(
                "failure:{}",
                crate::memory::hash(id.as_str().as_bytes())
            ))
            .map_err(error)?,
            connection: attempt.connection.clone(),
            sequence,
            observed_at_ms: completion.finished_at_ms,
            valid_until_ms: completion.finished_at_ms + OBSERVATION_LIFETIME_MAX_MS,
            kind,
        };
        validate_observation(&observation)?;
        persistence::insert_observation(tx, &observation)?;
    }
    persistence::complete_attempt(tx, id, completion)?;
    let stored = read_attempt(tx, id)?.ok_or_else(|| error("gateway_attempt_missing"))?;
    assert_eq!(stored.connection, attempt.connection);
    assert_eq!(stored.completion.as_ref(), Some(completion));
    Ok(())
}

pub fn read_attempt(
    connection: &Connection,
    id: &RequestId,
) -> Result<Option<Attempt>, EngineError> {
    persistence::read_attempt(connection, id)
}
