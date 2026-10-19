use crate::config::{EffectiveConfig, ModelConnection};
use crate::engine::EngineError;
use crate::protocol::*;
use crate::routing::{
    ApprovalMode, QueueEntry, ResourceBudget, RouteRequest, SchedulingPolicy, order_queue,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    WorkspaceLeader,
    ProjectLeader,
    Worker,
    NativeDescendant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanAuthority {
    Human,
    ConfiguredAutonomy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub id: TaskId,
    pub session_id: SessionId,
    pub role: Role,
    pub native_descendant: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_ref: Option<CredentialRef>,
    pub route: RouteRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub before: TaskId,
    pub after: TaskId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlan {
    pub id: PlanId,
    pub scope: ExecutionContext,
    pub tasks: Vec<WorkItem>,
    pub dependencies: Vec<Dependency>,
    pub authority: PlanAuthority,
    pub config_id: ConfigId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBinding {
    pub plan_id: PlanId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub pool_ref: CredentialRef,
    pub target_profile_ref: CredentialRef,
    pub connection: ModelConnection,
    pub config_id: ConfigId,
    pub budget_ref: CredentialRef,
    pub resources: ResourceBudget,
    pub attempt_cursor: Cursor,
}

impl WorkerBinding {
    fn assert_consistent(&self, plan: &WorkPlan, item: &WorkItem, reservation: &Reservation) {
        assert_eq!(item.role, Role::Worker);
        assert_eq!(self.plan_id, plan.id);
        assert_eq!(self.task_id, item.id);
        assert_eq!(self.session_id, item.session_id);
        assert_eq!(Some(&self.pool_ref), item.pool_ref.as_ref());
        assert_eq!(
            Some(&self.target_profile_ref),
            item.route.required_target.as_ref()
        );
        assert_eq!(self.connection.account_ref, item.route.account_ref);
        assert_eq!(self.config_id, reservation.config_id);
        assert_eq!(self.budget_ref, reservation.budget_ref);
        assert_eq!(self.resources, reservation.resources);
        assert_eq!(self.attempt_cursor, reservation.cursor);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub resources: ResourceBudget,
    pub config_id: ConfigId,
    pub shared_config_id: ConfigId,
    pub shared_capacity: ResourceBudget,
    pub scheduling: SchedulingPolicy,
    pub priority_weight: u16,
    pub budget_ref: CredentialRef,
    pub cursor: Cursor,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<WorkerBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedMessage {
    pub id: RequestId,
    pub source_plan: PlanId,
    pub target_plan: PlanId,
    pub sender: SessionId,
    pub recipient: SessionId,
    pub config_id: ConfigId,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveredMessage {
    pub message: ScopedMessage,
    pub cursor: Cursor,
    pub target_config_id: ConfigId,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinationLink {
    pub id: RequestId,
    pub source_plan: PlanId,
    pub target_plan: PlanId,
    pub source_config: ConfigId,
    pub target_config: ConfigId,
    pub allow_cross_project: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanState {
    pub plan: WorkPlan,
    pub approved: bool,
    pub revoked: bool,
    pub approval_provenance: Option<String>,
    pub queued_revision: u64,
    pub reservations: BTreeMap<TaskId, Reservation>,
    pub messages: Vec<DeliveredMessage>,
}

impl PlanState {
    fn assert_consistent(&self) {
        assert!(validate_plan(&self.plan).is_ok());
        assert_eq!(self.plan.config_id, self.plan.scope.config_id);
        assert!(self.queued_revision > 0);
        assert!(self.queued_revision <= i64::MAX as u64);
        assert!(self.reservations.len() <= self.plan.tasks.len());
        assert_eq!(self.approved, self.approval_provenance.is_some());
        assert!(self.reservations.is_empty() || self.approved);
        assert!(self.messages.is_empty() || self.approved);
        for (item, reservation) in &self.reservations {
            let task = self
                .plan
                .tasks
                .iter()
                .find(|task| &task.id == item)
                .unwrap();
            assert_eq!(task.route.resources, reservation.resources);
            assert_eq!(self.plan.config_id, reservation.config_id);
            assert_eq!(task.route.budget_ref, reservation.budget_ref);
            assert!(reservation.resources.fits(&reservation.shared_capacity));
            assert!((1..=100).contains(&reservation.priority_weight));
            assert!(reservation.cursor.value() > self.queued_revision);
            assert_eq!(
                reservation.provenance,
                "engine_planning_hold_not_native_dispatch"
            );
            if let Some(binding) = &reservation.binding {
                binding.assert_consistent(&self.plan, task, reservation);
            }
        }
        assert!(
            self.messages
                .windows(2)
                .all(|w| w[0].cursor.value() < w[1].cursor.value())
        );
        assert!(self.messages.len() <= 64);
        assert_eq!(
            self.messages
                .iter()
                .map(|m| &m.message.id)
                .collect::<BTreeSet<_>>()
                .len(),
            self.messages.len()
        );
        for delivered in &self.messages {
            assert_eq!(delivered.message.target_plan, self.plan.id);
            assert_eq!(delivered.target_config_id, self.plan.config_id);
            assert!(member(&self.plan, &delivered.message.recipient));
            assert!(!delivered.message.body.is_empty());
            assert!(delivered.message.body.len() <= 512);
            assert!(delivered.cursor.value() > self.queued_revision);
            assert_eq!(delivered.provenance, "protected_owner_relay");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum OwnerAction {
    ApprovePlan { plan_id: PlanId },
    RevokePlan { plan_id: PlanId },
    Reserve { plan_id: PlanId, item_id: TaskId },
    SendMessage { message: ScopedMessage },
    Link { link: CoordinationLink },
    RemoveLink { link_id: RequestId },
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS coordination_plans
            (id TEXT PRIMARY KEY, task TEXT NOT NULL UNIQUE, body TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS coordination_links (id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )
        .map_err(db_error)
}

fn db_error(error: impl std::fmt::Display) -> EngineError {
    EngineError(error.to_string())
}

fn rejected(code: impl Into<String>) -> Result<Outcome, EngineError> {
    Ok(Outcome::Rejected { code: code.into() })
}

pub(crate) fn is_mutation(command: &Command) -> bool {
    matches!(
        command,
        Command::ProposePlan { .. }
            | Command::OwnerCoordination { .. }
            | Command::RouteWorkItem { .. }
    )
}

pub(crate) fn execute(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    command: &Command,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match command {
        Command::ProposePlan { plan } => propose(tx, config, plan, revision),
        Command::CoordinationState { plan_id } => match load(tx, plan_id)? {
            Some(state) => Ok(Outcome::Coordination {
                state: Box::new(state),
            }),
            None => rejected("plan_missing"),
        },
        Command::TaskPlan { task_id } => task_plan(tx, task_id),
        Command::OwnerCoordination { action } => owner_action(tx, config, action, revision),
        Command::RouteWorkItem { plan_id, item_id } => route_item(tx, config, plan_id, item_id),
        Command::Mailbox {
            plan_id,
            recipient,
            after,
        } => mailbox(tx, config, plan_id, recipient, after),
        _ => Err(db_error("unsupported_coordination_command")),
    }
}

fn owner_action(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &OwnerAction,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match action {
        OwnerAction::ApprovePlan { plan_id } => approve_plan(tx, config, plan_id),
        OwnerAction::RevokePlan { plan_id } => revoke_plan(tx, plan_id),
        OwnerAction::Reserve { plan_id, item_id } => {
            reserve(tx, config, plan_id, item_id, revision)
        }
        OwnerAction::SendMessage { message } => send_message(tx, config, message, revision),
        OwnerAction::Link { link } => create_link(tx, config, link),
        OwnerAction::RemoveLink { link_id } => {
            if tx
                .execute(
                    "DELETE FROM coordination_links WHERE id=?1",
                    [link_id.as_str()],
                )
                .map_err(db_error)?
                == 0
            {
                return rejected("link_missing");
            }
            Ok(Outcome::Applied)
        }
    }
}

fn task_plan(tx: &Transaction<'_>, task_id: &TaskId) -> Result<Outcome, EngineError> {
    let id: Option<String> = tx
        .query_row(
            "SELECT id FROM coordination_plans WHERE task=?1",
            [task_id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let Some(id) = id else {
        return rejected("plan_missing");
    };
    let id = PlanId::new(id).map_err(db_error)?;
    let state = load(tx, &id)?.ok_or_else(|| db_error("plan_index_inconsistent"))?;
    assert_eq!(&state.plan.scope.task_id, task_id);
    Ok(Outcome::Coordination {
        state: Box::new(state),
    })
}

fn load(tx: &Transaction<'_>, id: &PlanId) -> Result<Option<PlanState>, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM coordination_plans WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    body.map(|body| {
        let state: PlanState = serde_json::from_str(&body).map_err(db_error)?;
        state.assert_consistent();
        assert_eq!(&state.plan.id, id);
        Ok(state)
    })
    .transpose()
}

fn all_plans(tx: &Transaction<'_>) -> Result<Vec<PlanState>, EngineError> {
    let mut statement = tx
        .prepare("SELECT body FROM coordination_plans ORDER BY id LIMIT 129")
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_error)?;
    let states: Vec<PlanState> = rows
        .map(|row| serde_json::from_str(&row.map_err(db_error)?).map_err(db_error))
        .collect::<Result<_, _>>()?;
    assert!(states.len() <= 128);
    for state in &states {
        state.assert_consistent();
    }
    Ok(states)
}

fn save(tx: &Transaction<'_>, state: &PlanState) -> Result<Outcome, EngineError> {
    state.assert_consistent();
    let body = serde_json::to_string(state).map_err(db_error)?;
    if body.len() > FRAME_BYTES_MAX - 2048 {
        return rejected("coordination_state_capacity");
    }
    tx.execute(
        "INSERT INTO coordination_plans VALUES (?1,?2,?3)
        ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        params![
            state.plan.id.as_str(),
            state.plan.scope.task_id.as_str(),
            body
        ],
    )
    .map_err(db_error)?;
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn pinned_config(tx: &Transaction<'_>, id: &ConfigId) -> Result<EffectiveConfig, EngineError> {
    let body: String = tx
        .query_row("SELECT body FROM configs WHERE id=?1", [id.as_str()], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    let config: EffectiveConfig = serde_json::from_str(&body).map_err(db_error)?;
    config.validate().map_err(db_error)?;
    assert_eq!(&config.id, id);
    Ok(config)
}

fn validate_plan(plan: &WorkPlan) -> Result<(), String> {
    if plan.tasks.is_empty()
        || plan.tasks.len() > 16
        || plan.dependencies.len() > 64
        || plan.config_id != plan.scope.config_id
    {
        return Err("invalid_plan_shape".into());
    }
    let mut items = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for item in &plan.tasks {
        if !items.insert(&item.id) || !sessions.insert(&item.session_id) {
            return Err("duplicate_plan_member".into());
        }
        item.route.validate()?;
        if let Some(child) = &item.native_descendant
            && (!sessions.insert(child)
                || item.route.resources.slots < 2
                || item.route.resources.processes < 2)
        {
            return Err("descendant_budget_missing".into());
        }
        let request = &item.route;
        if request.budget_ref != plan.scope.budget_ref
            || [&request.preferred_target, &request.required_target]
                .into_iter()
                .flatten()
                .any(|target| !plan.scope.allowed_destinations.contains(target))
        {
            return Err("route_scope_denied".into());
        }
    }
    validate_dependencies(plan, &items)
}

fn validate_dependencies(plan: &WorkPlan, items: &BTreeSet<&TaskId>) -> Result<(), String> {
    let mut edges = BTreeSet::new();
    for edge in &plan.dependencies {
        if !items.contains(&edge.before)
            || !items.contains(&edge.after)
            || !edges.insert((&edge.before, &edge.after))
        {
            return Err("invalid_dependency".into());
        }
    }
    let mut visited = BTreeSet::new();
    for _ in 0..plan.tasks.len() {
        for item in &plan.tasks {
            if plan
                .dependencies
                .iter()
                .filter(|d| d.after == item.id)
                .all(|d| visited.contains(&d.before))
            {
                visited.insert(item.id.clone());
            }
        }
    }
    if visited.len() != plan.tasks.len() {
        return Err("dependency_cycle".into());
    }
    assert_eq!(visited.len(), items.len());
    assert!(edges.len() <= 64);
    Ok(())
}

fn check_context(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    plan: &WorkPlan,
) -> Result<Option<String>, EngineError> {
    if crate::storage::task_context(tx, &plan.scope.task_id)?.as_ref() != Some(&plan.scope) {
        return Ok(Some("plan_context_mismatch".into()));
    }
    if plan
        .scope
        .allowed_destinations
        .iter()
        .any(|d| config.settings.revoked_destinations.contains(d))
    {
        return Ok(Some("plan_authority_revoked".into()));
    }
    Ok(None)
}

fn propose(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    plan: &WorkPlan,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if let Err(code) = validate_plan(plan) {
        return rejected(code);
    }
    if let Some(code) = check_context(tx, config, plan)? {
        return rejected(code);
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM coordination_plans WHERE id=?1 OR task=?2)",
            params![plan.id.as_str(), plan.scope.task_id.as_str()],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if exists {
        return rejected("plan_exists");
    }
    if all_plans(tx)?.len() >= 128 {
        return rejected("plan_capacity");
    }
    let pinned = pinned_config(tx, &plan.config_id)?;
    if plan.tasks.iter().any(|item| {
        !pinned
            .settings
            .routing
            .approved_accounts
            .contains(&item.route.account_ref)
    }) {
        return rejected("route_account_denied");
    }
    for item in &plan.tasks {
        if let Some(pool_ref) = &item.pool_ref
            && let Err(error) = crate::routing::validate_pool_scope(
                &pinned,
                pool_ref,
                &plan.scope.project_id,
                &item.role,
                &item.route.account_ref,
            )
        {
            return rejected(error.0);
        }
    }
    let automatic = plan.authority == PlanAuthority::ConfiguredAutonomy;
    if automatic && pinned.settings.routing.approval_mode != ApprovalMode::BoundedAutomatic {
        return rejected("human_approval_required");
    }
    save(
        tx,
        &PlanState {
            plan: plan.clone(),
            approved: automatic,
            revoked: false,
            approval_provenance: automatic.then(|| "pinned_configured_autonomy".into()),
            queued_revision: revision + 1,
            reservations: BTreeMap::new(),
            messages: vec![],
        },
    )
}

fn active(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    state: &PlanState,
) -> Result<Option<String>, EngineError> {
    if state.revoked {
        return Ok(Some("plan_revoked".into()));
    }
    if !state.approved {
        return Ok(Some("plan_not_approved".into()));
    }
    check_context(tx, config, &state.plan)
}

fn approve_plan(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &PlanId,
) -> Result<Outcome, EngineError> {
    let Some(mut state) = load(tx, id)? else {
        return rejected("plan_missing");
    };
    if state.revoked {
        return rejected("plan_revoked");
    }
    if state.approved {
        return rejected("plan_already_approved");
    }
    if let Err(code) = validate_plan(&state.plan) {
        return rejected(code);
    }
    if let Some(code) = check_context(tx, config, &state.plan)? {
        return rejected(code);
    }
    state.approved = true;
    state.approval_provenance = Some("protected_owner_in_process".into());
    save(tx, &state)
}

fn revoke_plan(tx: &Transaction<'_>, id: &PlanId) -> Result<Outcome, EngineError> {
    let Some(mut state) = load(tx, id)? else {
        return rejected("plan_missing");
    };
    if state.revoked {
        return rejected("plan_revoked");
    }
    state.revoked = true;
    save(tx, &state)
}

fn totals(states: &[PlanState], plan: &WorkPlan) -> Result<[ResourceBudget; 3], EngineError> {
    let mut result = [
        ResourceBudget::zero(),
        ResourceBudget::zero(),
        ResourceBudget::zero(),
    ];
    for state in states {
        for reservation in state.reservations.values() {
            result[0] = result[0]
                .checked_add(&reservation.resources)
                .map_err(db_error)?;
            if state.plan.scope.project_id == plan.scope.project_id {
                result[1] = result[1]
                    .checked_add(&reservation.resources)
                    .map_err(db_error)?;
            }
            if state.plan.scope.task_id == plan.scope.task_id {
                result[2] = result[2]
                    .checked_add(&reservation.resources)
                    .map_err(db_error)?;
            }
        }
    }
    Ok(result)
}

fn fits_capacity(
    totals: &[ResourceBudget; 3],
    item: &WorkItem,
    config: &EffectiveConfig,
    pinned: &EffectiveConfig,
) -> Result<bool, EngineError> {
    let limits = [
        &config.settings.routing.global_capacity,
        &pinned.settings.routing.project_capacity,
        &pinned.settings.routing.task_capacity,
    ];
    for (used, limit) in totals.iter().zip(limits) {
        if !used
            .checked_add(&item.route.resources)
            .map_err(db_error)?
            .fits(limit)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn next_allocation(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    states: &[PlanState],
) -> Result<Option<(PlanId, TaskId)>, EngineError> {
    let mut queue = vec![];
    for state in states {
        if active(tx, config, state)?.is_some() {
            continue;
        }
        let pinned = pinned_config(tx, &state.plan.config_id)?;
        let total = totals(states, &state.plan)?;
        for item in &state.plan.tasks {
            if state.reservations.contains_key(&item.id)
                || state.plan.dependencies.iter().any(|d| d.after == item.id)
                || !fits_capacity(&total, item, config, &pinned)?
            {
                continue;
            }
            if item.role == Role::Worker
                && item.route.required_target.is_some()
                && let Some(pool_ref) = &item.pool_ref
                && crate::routing::select_pool_connection(
                    &pinned,
                    pool_ref,
                    &state.plan.scope.project_id,
                    &item.role,
                    &item.route.account_ref,
                )
                .is_err()
            {
                continue;
            }
            queue.push((
                QueueEntry {
                    project_id: state.plan.scope.project_id.clone(),
                    priority_weight: pinned.settings.routing.priority_weight,
                    allocated_slots: total[1].slots,
                    queued_revision: state.queued_revision,
                },
                state.plan.id.clone(),
                item.id.clone(),
            ));
        }
    }
    let mut ranks: Vec<_> = queue.iter().map(|(entry, _, _)| entry.clone()).collect();
    order_queue(&mut ranks, &config.settings.routing.scheduling);
    Ok(ranks
        .first()
        .and_then(|first| queue.iter().find(|(entry, _, _)| entry == first))
        .map(|(_, plan, item)| (plan.clone(), item.clone())))
}

fn worker_binding(
    plan: &WorkPlan,
    item: &WorkItem,
    pinned: &EffectiveConfig,
    cursor: &Cursor,
) -> Result<Option<WorkerBinding>, crate::routing::RouteError> {
    if item.role != Role::Worker {
        return Ok(None);
    }
    let (Some(pool_ref), Some(target_profile_ref)) = (&item.pool_ref, &item.route.required_target)
    else {
        return Ok(None);
    };
    let connection = crate::routing::select_pool_connection(
        pinned,
        pool_ref,
        &plan.scope.project_id,
        &item.role,
        &item.route.account_ref,
    )?;
    Ok(Some(WorkerBinding {
        plan_id: plan.id.clone(),
        task_id: item.id.clone(),
        session_id: item.session_id.clone(),
        pool_ref: pool_ref.clone(),
        target_profile_ref: target_profile_ref.clone(),
        connection: connection.clone(),
        config_id: plan.config_id.clone(),
        budget_ref: item.route.budget_ref.clone(),
        resources: item.route.resources.clone(),
        attempt_cursor: cursor.clone(),
    }))
}

fn reserve(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &PlanId,
    item_id: &TaskId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let Some(mut state) = load(tx, id)? else {
        return rejected("plan_missing");
    };
    if let Some(code) = active(tx, config, &state)? {
        return rejected(code);
    }
    let Some(item) = state.plan.tasks.iter().find(|i| &i.id == item_id) else {
        return rejected("work_item_missing");
    };
    if state.reservations.contains_key(item_id) {
        return rejected("already_reserved");
    }
    if state.plan.dependencies.iter().any(|d| &d.after == item_id) {
        return rejected("dependencies_unfinished");
    }
    let pinned = pinned_config(tx, &state.plan.config_id)?;
    let cursor = Cursor::new(revision + 1);
    let binding = match worker_binding(&state.plan, item, &pinned, &cursor) {
        Ok(binding) => binding,
        Err(error) => return rejected(error.0),
    };
    let states = all_plans(tx)?;
    if !fits_capacity(&totals(&states, &state.plan)?, item, config, &pinned)? {
        return rejected("resource_budget_exhausted");
    }
    if next_allocation(tx, config, &states)? != Some((id.clone(), item_id.clone())) {
        return rejected("allocation_queued_by_policy");
    }
    state.reservations.insert(
        item_id.clone(),
        Reservation {
            resources: item.route.resources.clone(),
            config_id: state.plan.config_id.clone(),
            shared_config_id: config.id.clone(),
            shared_capacity: config.settings.routing.global_capacity.clone(),
            scheduling: config.settings.routing.scheduling.clone(),
            priority_weight: pinned.settings.routing.priority_weight,
            budget_ref: state.plan.scope.budget_ref.clone(),
            cursor,
            provenance: "engine_planning_hold_not_native_dispatch".into(),
            binding,
        },
    );
    let outcome = save(tx, &state)?;
    if outcome == Outcome::Applied {
        crate::storage::store_config(tx, config)?;
    }
    Ok(outcome)
}

fn route_item(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &PlanId,
    item_id: &TaskId,
) -> Result<Outcome, EngineError> {
    let Some(state) = load(tx, id)? else {
        return rejected("plan_missing");
    };
    if let Some(code) = active(tx, config, &state)? {
        return rejected(code);
    }
    let Some(item) = state.plan.tasks.iter().find(|i| &i.id == item_id) else {
        return rejected("work_item_missing");
    };
    if !state.reservations.contains_key(item_id) {
        return rejected("reservation_required");
    }
    let mut pinned = pinned_config(tx, &state.plan.config_id)?;
    pinned.settings.allowed_destinations = state.plan.scope.allowed_destinations.clone();
    pinned.settings.revoked_destinations = config.settings.revoked_destinations.clone();
    if let Some(pool_ref) = &item.pool_ref
        && let Err(error) = crate::routing::validate_pool_scope(
            &pinned,
            pool_ref,
            &state.plan.scope.project_id,
            &item.role,
            &item.route.account_ref,
        )
    {
        return rejected(error.0);
    }
    match crate::routing::route(&item.route, &pinned) {
        Ok(decision) => Ok(Outcome::Route { decision }),
        Err(error) => rejected(error.0),
    }
}

fn member(plan: &WorkPlan, session: &SessionId) -> bool {
    plan.tasks
        .iter()
        .any(|item| &item.session_id == session || item.native_descendant.as_ref() == Some(session))
}

fn linked(tx: &Transaction<'_>, source: &WorkPlan, target: &WorkPlan) -> Result<bool, EngineError> {
    if source.id == target.id {
        return Ok(true);
    }
    let mut statement = tx
        .prepare("SELECT body FROM coordination_links ORDER BY id LIMIT 129")
        .map_err(db_error)?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_error)?;
    for row in rows {
        let link: CoordinationLink =
            serde_json::from_str(&row.map_err(db_error)?).map_err(db_error)?;
        if link.source_plan == source.id
            && link.target_plan == target.id
            && link.source_config == source.config_id
            && link.target_config == target.config_id
            && (source.scope.project_id == target.scope.project_id || link.allow_cross_project)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn create_link(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    link: &CoordinationLink,
) -> Result<Outcome, EngineError> {
    let Some(source) = load(tx, &link.source_plan)? else {
        return rejected("plan_missing");
    };
    let Some(target) = load(tx, &link.target_plan)? else {
        return rejected("plan_missing");
    };
    for state in [&source, &target] {
        if let Some(code) = active(tx, config, state)? {
            return rejected(code);
        }
    }
    if link.source_plan == link.target_plan
        || link.source_config != source.plan.config_id
        || link.target_config != target.plan.config_id
    {
        return rejected("link_scope_mismatch");
    }
    if source.plan.scope.project_id != target.plan.scope.project_id && !link.allow_cross_project {
        return rejected("cross_project_authority_required");
    }
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM coordination_links", [], |r| r.get(0))
        .map_err(db_error)?;
    if count >= 128 {
        return rejected("link_capacity");
    }
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO coordination_links VALUES (?1,?2)",
            params![
                link.id.as_str(),
                serde_json::to_string(link).map_err(db_error)?
            ],
        )
        .map_err(db_error)?;
    if inserted == 0 {
        return rejected("link_exists");
    }
    Ok(Outcome::Applied)
}

fn send_message(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    message: &ScopedMessage,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if message.body.is_empty() || message.body.len() > 512 || message.body.contains('\0') {
        return rejected("invalid_message_body");
    }
    let Some(source) = load(tx, &message.source_plan)? else {
        return rejected("plan_missing");
    };
    let Some(mut target) = load(tx, &message.target_plan)? else {
        return rejected("plan_missing");
    };
    for state in [&source, &target] {
        if let Some(code) = active(tx, config, state)? {
            return rejected(code);
        }
    }
    if source.plan.config_id != message.config_id
        || !member(&source.plan, &message.sender)
        || !member(&target.plan, &message.recipient)
    {
        return rejected("message_scope_denied");
    }
    if !linked(tx, &source.plan, &target.plan)? {
        return rejected("coordination_link_required");
    }
    if all_plans(tx)?
        .iter()
        .any(|state| state.messages.iter().any(|m| m.message.id == message.id))
    {
        return rejected("message_identity_conflict");
    }
    let pinned = pinned_config(tx, &target.plan.config_id)?;
    if target.messages.len() >= pinned.settings.routing.mailbox_capacity {
        return rejected("mailbox_full");
    }
    target.messages.push(DeliveredMessage {
        message: message.clone(),
        cursor: Cursor::new(revision + 1),
        target_config_id: target.plan.config_id.clone(),
        provenance: "protected_owner_relay".into(),
    });
    save(tx, &target)
}

fn mailbox(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &PlanId,
    recipient: &SessionId,
    after: &Cursor,
) -> Result<Outcome, EngineError> {
    let Some(state) = load(tx, id)? else {
        return rejected("plan_missing");
    };
    if let Some(code) = active(tx, config, &state)? {
        return rejected(code);
    }
    if !member(&state.plan, recipient) {
        return rejected("message_scope_denied");
    }
    let mut messages = vec![];
    for delivered in &state.messages {
        if &delivered.message.recipient != recipient || delivered.cursor.value() <= after.value() {
            continue;
        }
        let Some(source) = load(tx, &delivered.message.source_plan)? else {
            continue;
        };
        if active(tx, config, &source)?.is_none() && linked(tx, &source.plan, &state.plan)? {
            messages.push(delivered.clone());
        }
    }
    assert!(messages.len() <= 64);
    assert!(
        messages
            .windows(2)
            .all(|w| w[0].cursor.value() < w[1].cursor.value())
    );
    Ok(Outcome::Messages { messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_bound_state() -> PlanState {
        let mut state = state();
        let item = &mut state.plan.tasks[0];
        let pool = CredentialRef::new("pool").unwrap();
        let target = CredentialRef::new("profile").unwrap();
        item.pool_ref = Some(pool.clone());
        item.route.required_target = Some(target.clone());
        state.plan.scope.allowed_destinations.push(target.clone());
        let binding = WorkerBinding {
            plan_id: state.plan.id.clone(),
            task_id: item.id.clone(),
            session_id: item.session_id.clone(),
            pool_ref: pool,
            target_profile_ref: target,
            connection: serde_json::from_value(serde_json::json!({
                "id": "connection", "provider": "service", "harness": "rcli",
                "protocol": "openai-compatible", "account_ref": "account",
                "endpoint": { "kind": "http", "url": "http://127.0.0.1:11434/v1" },
                "model": { "id": "model", "author": "author" },
                "auth": { "method": "none" }
            }))
            .unwrap(),
            config_id: state.plan.config_id.clone(),
            budget_ref: item.route.budget_ref.clone(),
            resources: item.route.resources.clone(),
            attempt_cursor: Cursor::new(2),
        };
        state.reservations.insert(
            item.id.clone(),
            Reservation {
                resources: item.route.resources.clone(),
                config_id: state.plan.config_id.clone(),
                shared_config_id: state.plan.config_id.clone(),
                shared_capacity: ResourceBudget::unit(),
                scheduling: SchedulingPolicy::FairShare,
                priority_weight: 1,
                budget_ref: item.route.budget_ref.clone(),
                cursor: Cursor::new(2),
                provenance: "engine_planning_hold_not_native_dispatch".into(),
                binding: Some(binding),
            },
        );
        state.assert_consistent();
        assert_eq!(state.reservations.len(), 1);
        state
    }

    #[test]
    fn persisted_worker_binding_invariants_detect_identity_drift() {
        for variant in 0..13 {
            let mut state = worker_bound_state();
            let item_id = state.plan.tasks[0].id.clone();
            let binding = state
                .reservations
                .get_mut(&item_id)
                .unwrap()
                .binding
                .as_mut()
                .unwrap();
            match variant {
                0 => binding.plan_id = PlanId::new("other-plan").unwrap(),
                1 => binding.task_id = TaskId::new("other-item").unwrap(),
                2 => binding.session_id = SessionId::new("other-session").unwrap(),
                3 => binding.pool_ref = CredentialRef::new("other-pool").unwrap(),
                4 => binding.target_profile_ref = CredentialRef::new("other-target").unwrap(),
                5 => binding.connection.account_ref = CredentialRef::new("other-account").unwrap(),
                6 => binding.config_id = ConfigId::new("other-config").unwrap(),
                7 => binding.budget_ref = CredentialRef::new("other-budget").unwrap(),
                8 => binding.resources = ResourceBudget::zero(),
                9 => binding.attempt_cursor = Cursor::new(3),
                10 => state.plan.tasks[0].role = Role::ProjectLeader,
                11 => state.plan.tasks[0].pool_ref = None,
                12 => state.plan.tasks[0].route.required_target = None,
                _ => unreachable!(),
            }
            assert!(
                std::panic::catch_unwind(|| state.assert_consistent()).is_err(),
                "worker binding invariant must reject variant {variant}"
            );
        }
    }

    fn state() -> PlanState {
        let config_id = ConfigId::new("config").unwrap();
        let scope = ExecutionContext {
            project_id: ProjectId::new("project").unwrap(),
            task_id: TaskId::new("task").unwrap(),
            config_id: config_id.clone(),
            snapshot_id: SnapshotId::new("snapshot").unwrap(),
            baseline_id: BaselineId::new("baseline").unwrap(),
            allowed_destinations: vec![],
            budget_ref: CredentialRef::new("budget").unwrap(),
        };
        let task = WorkItem {
            id: TaskId::new("item").unwrap(),
            session_id: SessionId::new("worker").unwrap(),
            role: Role::Worker,
            native_descendant: None,
            pool_ref: None,
            route: RouteRequest {
                preferred_target: None,
                required_target: None,
                required_capabilities: vec![],
                account_ref: CredentialRef::new("account").unwrap(),
                budget_ref: scope.budget_ref.clone(),
                resources: ResourceBudget::unit(),
            },
        };
        PlanState {
            plan: WorkPlan {
                id: PlanId::new("plan").unwrap(),
                scope,
                tasks: vec![task],
                dependencies: vec![],
                authority: PlanAuthority::Human,
                config_id,
            },
            approved: true,
            revoked: false,
            approval_provenance: Some("protected_owner_in_process".into()),
            queued_revision: 1,
            reservations: BTreeMap::new(),
            messages: vec![],
        }
    }

    #[test]
    #[should_panic]
    fn persisted_approval_invariant_detects_missing_owner_provenance() {
        let mut state = state();
        state.approval_provenance = None;
        state.assert_consistent();
    }

    #[test]
    #[should_panic]
    fn persisted_reservation_invariant_detects_changed_resource_accounting() {
        let mut state = state();
        let item = &state.plan.tasks[0];
        state.reservations.insert(
            item.id.clone(),
            Reservation {
                resources: ResourceBudget::zero(),
                config_id: state.plan.config_id.clone(),
                shared_config_id: state.plan.config_id.clone(),
                shared_capacity: ResourceBudget::unit(),
                scheduling: SchedulingPolicy::FairShare,
                priority_weight: 1,
                budget_ref: item.route.budget_ref.clone(),
                cursor: Cursor::new(2),
                provenance: "engine_planning_hold_not_native_dispatch".into(),
                binding: None,
            },
        );
        state.assert_consistent();
    }

    #[test]
    #[should_panic]
    fn persisted_mailbox_invariant_detects_misattributed_recipient() {
        let mut state = state();
        state.messages.push(DeliveredMessage {
            message: ScopedMessage {
                id: RequestId::new("message").unwrap(),
                source_plan: state.plan.id.clone(),
                target_plan: state.plan.id.clone(),
                sender: state.plan.tasks[0].session_id.clone(),
                recipient: SessionId::new("intruder").unwrap(),
                config_id: state.plan.config_id.clone(),
                body: "data".into(),
            },
            cursor: Cursor::new(2),
            target_config_id: state.plan.config_id.clone(),
            provenance: "protected_owner_relay".into(),
        });
        state.assert_consistent();
    }
}
