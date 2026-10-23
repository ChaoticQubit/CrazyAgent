use crate::config::{EffectiveConfig, ModelConnection};
use crate::coordination::Role;
use crate::protocol::{ConfigId, CredentialRef, ProjectId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBudget {
    pub slots: u64,
    pub cpu_millis: u64,
    pub memory_mib: u64,
    pub processes: u64,
    pub output_bytes: u64,
    pub wall_time_ms: u64,
    pub spend_microunits: u64,
}

impl ResourceBudget {
    pub fn unit() -> Self {
        Self {
            slots: 1,
            cpu_millis: 1000,
            memory_mib: 512,
            processes: 4,
            output_bytes: 1048576,
            wall_time_ms: 60000,
            spend_microunits: 0,
        }
    }

    pub fn zero() -> Self {
        Self {
            slots: 0,
            cpu_millis: 0,
            memory_mib: 0,
            processes: 0,
            output_bytes: 0,
            wall_time_ms: 0,
            spend_microunits: 0,
        }
    }

    fn dimensions(&self) -> [u64; 7] {
        [
            self.slots,
            self.cpu_millis,
            self.memory_mib,
            self.processes,
            self.output_bytes,
            self.wall_time_ms,
            self.spend_microunits,
        ]
    }

    pub fn validate(&self) -> Result<(), String> {
        let values = self.dimensions();
        if values[..6].contains(&0) || values.iter().any(|v| *v > 1_000_000_000_000) {
            return Err("invalid_resource_budget".into());
        }
        Ok(())
    }

    pub fn fits(&self, limit: &Self) -> bool {
        self.dimensions()
            .iter()
            .zip(limit.dimensions())
            .all(|(used, cap)| *used <= cap)
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, String> {
        let mut values = [0; 7];
        for (index, (a, b)) in self.dimensions().iter().zip(other.dimensions()).enumerate() {
            values[index] = a.checked_add(b).ok_or("resource_overflow")?;
        }
        Ok(Self {
            slots: values[0],
            cpu_millis: values[1],
            memory_mib: values[2],
            processes: values[3],
            output_bytes: values[4],
            wall_time_ms: values[5],
            spend_microunits: values[6],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SchedulingPolicy {
    #[default]
    FairShare,
    StrictPriority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ApprovalMode {
    #[default]
    Human,
    BoundedAutomatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicy {
    pub preference_order: Vec<CredentialRef>,
    pub approved_accounts: Vec<CredentialRef>,
    pub fallback_allowed: bool,
    pub approval_mode: ApprovalMode,
    pub scheduling: SchedulingPolicy,
    pub priority_weight: u16,
    pub global_capacity: ResourceBudget,
    pub project_capacity: ResourceBudget,
    pub task_capacity: ResourceBudget,
    pub mailbox_capacity: usize,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        let capacity = ResourceBudget {
            slots: 16,
            cpu_millis: 16000,
            memory_mib: 8192,
            processes: 128,
            output_bytes: 67108864,
            wall_time_ms: 86400000,
            spend_microunits: 0,
        };
        Self {
            preference_order: vec![],
            approved_accounts: vec![],
            fallback_allowed: true,
            approval_mode: ApprovalMode::Human,
            scheduling: SchedulingPolicy::FairShare,
            priority_weight: 1,
            global_capacity: capacity.clone(),
            project_capacity: capacity.clone(),
            task_capacity: capacity,
            mailbox_capacity: 32,
        }
    }
}

impl RoutingPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        self.global_capacity.validate()?;
        self.project_capacity.validate()?;
        self.task_capacity.validate()?;
        if !self.project_capacity.fits(&self.global_capacity)
            || !self.task_capacity.fits(&self.project_capacity)
            || !(1..=100).contains(&self.priority_weight)
            || !(1..=64).contains(&self.mailbox_capacity)
            || self.preference_order.len() > 64
            || self.approved_accounts.len() > 64
            || self.approved_accounts.iter().collect::<BTreeSet<_>>().len()
                != self.approved_accounts.len()
            || self.preference_order.iter().collect::<BTreeSet<_>>().len()
                != self.preference_order.len()
        {
            return Err("invalid_routing_policy".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRequest {
    pub preferred_target: Option<CredentialRef>,
    pub required_target: Option<CredentialRef>,
    pub required_capabilities: Vec<String>,
    pub account_ref: CredentialRef,
    pub budget_ref: CredentialRef,
    pub resources: ResourceBudget,
}

impl RouteRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.resources.validate()?;
        if self.required_capabilities.len() > 16
            || self.required_capabilities.iter().any(|c| {
                c.is_empty()
                    || c.len() > 64
                    || !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err("invalid_capabilities".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Usage {
    Known {
        resources: ResourceBudget,
        provenance: String,
    },
    Estimated {
        resources: ResourceBudget,
        provenance: String,
    },
    Unavailable {
        provenance: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteDecision {
    pub target_profile_ref: CredentialRef,
    pub reasons: Vec<String>,
    pub budget_ref: CredentialRef,
    pub config_id: ConfigId,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError(pub String);

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RouteError {}

pub(crate) fn validate_pool_scope(
    config: &EffectiveConfig,
    pool_ref: &CredentialRef,
    project: &ProjectId,
    role: &Role,
    account: &CredentialRef,
) -> Result<(), RouteError> {
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| RouteError("route_pool_missing".into()))?;
    let pool = catalog
        .pools
        .iter()
        .find(|pool| &pool.id == pool_ref)
        .ok_or_else(|| RouteError("route_pool_missing".into()))?;
    let role = match role {
        Role::WorkspaceLeader => "workspace_leader",
        Role::ProjectLeader => "project_leader",
        Role::Worker => "worker",
        Role::NativeDescendant => "native_descendant",
    };
    if !pool
        .projects
        .iter()
        .any(|id| id.as_str() == project.as_str())
        || !pool.roles.iter().any(|id| id.as_str() == role)
    {
        return Err(RouteError("route_pool_scope_denied".into()));
    }
    if !config.settings.routing.approved_accounts.contains(account) {
        return Err(RouteError("route_account_denied".into()));
    }
    if !catalog.connections.iter().any(|connection| {
        &connection.account_ref == account && pool.members.contains(&connection.id)
    }) {
        return Err(RouteError("route_pool_account_denied".into()));
    }
    Ok(())
}

pub(crate) fn select_pool_connection<'a>(
    config: &'a EffectiveConfig,
    pool_ref: &CredentialRef,
    project: &ProjectId,
    role: &Role,
    account: &CredentialRef,
) -> Result<&'a ModelConnection, RouteError> {
    validate_pool_scope(config, pool_ref, project, role, account)?;
    let catalog = config.settings.model_access.as_ref().unwrap();
    let pool = catalog
        .pools
        .iter()
        .find(|pool| &pool.id == pool_ref)
        .unwrap();
    let mut matching = catalog.connections.iter().filter(|connection| {
        &connection.account_ref == account && pool.members.contains(&connection.id)
    });
    let connection = matching
        .next()
        .ok_or_else(|| RouteError("route_pool_account_denied".into()))?;
    if matching.next().is_some() {
        return Err(RouteError("route_pool_connection_ambiguous".into()));
    }
    Ok(connection)
}

struct AdmittedTarget {
    profile: CredentialRef,
    account: CredentialRef,
    capabilities: BTreeSet<String>,
    remaining: Option<ResourceBudget>,
    provenance: String,
}

pub fn route(
    request: &RouteRequest,
    config: &EffectiveConfig,
) -> Result<RouteDecision, RouteError> {
    select(request, config, &[])
}

fn select(
    request: &RouteRequest,
    config: &EffectiveConfig,
    targets: &[AdmittedTarget],
) -> Result<RouteDecision, RouteError> {
    request.validate().map_err(RouteError)?;
    config.settings.routing.validate().map_err(RouteError)?;
    if !config
        .settings
        .routing
        .approved_accounts
        .contains(&request.account_ref)
    {
        return Err(RouteError("route_account_denied".into()));
    }
    assert!(targets.len() <= 64);
    assert_eq!(
        targets
            .iter()
            .map(|t| &t.profile)
            .collect::<BTreeSet<_>>()
            .len(),
        targets.len()
    );
    if !request
        .resources
        .fits(&config.settings.routing.task_capacity)
    {
        return Err(RouteError("resource_budget_exhausted".into()));
    }
    if [&request.preferred_target, &request.required_target]
        .into_iter()
        .flatten()
        .any(|target| !authorized_target(config, target))
    {
        return Err(RouteError("route_scope_denied".into()));
    }
    if targets.is_empty() {
        return Err(RouteError("native_admission_unavailable".into()));
    }
    let candidates = candidate_order(request, config);
    for (index, profile) in candidates.iter().enumerate() {
        let Some(target) = targets.iter().find(|t| &t.profile == *profile) else {
            continue;
        };
        if !eligible(request, config, target) {
            continue;
        }
        assert!(!target.provenance.is_empty());
        let reason = if request.required_target.is_some() {
            "required_target"
        } else if index == 0 {
            "approved_preference"
        } else {
            "approved_fallback"
        };
        return Ok(decision(request, config, target, reason));
    }
    Err(RouteError(
        if request.required_target.is_some() {
            "required_target_unavailable"
        } else {
            "no_eligible_target"
        }
        .into(),
    ))
}

fn authorized_target(config: &EffectiveConfig, target: &CredentialRef) -> bool {
    config.settings.allowed_destinations.contains(target)
        && !config.settings.revoked_destinations.contains(target)
}

fn eligible(request: &RouteRequest, config: &EffectiveConfig, target: &AdmittedTarget) -> bool {
    authorized_target(config, &target.profile)
        && target.account == request.account_ref
        && request
            .required_capabilities
            .iter()
            .all(|c| target.capabilities.contains(c))
        && target
            .remaining
            .as_ref()
            .is_some_and(|remaining| request.resources.fits(remaining))
}

fn decision(
    request: &RouteRequest,
    config: &EffectiveConfig,
    target: &AdmittedTarget,
    reason: &str,
) -> RouteDecision {
    RouteDecision {
        target_profile_ref: target.profile.clone(),
        reasons: vec![
            reason.into(),
            target.provenance.clone(),
            "known_capacity_checked".into(),
        ],
        budget_ref: request.budget_ref.clone(),
        config_id: config.id.clone(),
        usage: Usage::Unavailable {
            provenance: "native_usage_not_observed".into(),
        },
    }
}

fn candidate_order<'a>(
    request: &'a RouteRequest,
    config: &'a EffectiveConfig,
) -> Vec<&'a CredentialRef> {
    if let Some(required) = &request.required_target {
        return vec![required];
    }
    let mut candidates: Vec<_> = request.preferred_target.iter().collect();
    for profile in config
        .settings
        .routing
        .preference_order
        .iter()
        .chain(config.settings.allowed_destinations.iter())
    {
        if !candidates.contains(&profile) {
            candidates.push(profile);
        }
    }
    if !config.settings.routing.fallback_allowed {
        candidates.truncate(1);
    }
    assert!(candidates.len() <= 129);
    candidates
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub project_id: ProjectId,
    pub priority_weight: u16,
    pub allocated_slots: u64,
    pub queued_revision: u64,
}

pub fn order_queue(entries: &mut [QueueEntry], policy: &SchedulingPolicy) {
    entries.sort_by(|a, b| {
        let rank = match policy {
            SchedulingPolicy::FairShare => (u128::from(a.allocated_slots)
                * u128::from(b.priority_weight))
            .cmp(&(u128::from(b.allocated_slots) * u128::from(a.priority_weight))),
            SchedulingPolicy::StrictPriority => b.priority_weight.cmp(&a.priority_weight),
        };
        rank.then(a.queued_revision.cmp(&b.queued_revision))
            .then(a.project_id.cmp(&b.project_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> (
        tempfile::TempDir,
        EffectiveConfig,
        RouteRequest,
        Vec<AdmittedTarget>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("g");
        std::fs::write(
            &global,
            "allowed_destinations=['alpha','beta']\n[routing]\npreference_order=['alpha','beta']\napproved_accounts=['account']",
        )
        .unwrap();
        let config = crate::config::resolve_config(&global, &root.path().join("p")).unwrap();
        let request = RouteRequest {
            preferred_target: Some(CredentialRef::new("alpha").unwrap()),
            required_target: None,
            required_capabilities: vec!["coding".into()],
            account_ref: CredentialRef::new("account").unwrap(),
            budget_ref: CredentialRef::new("budget").unwrap(),
            resources: ResourceBudget::unit(),
        };
        let targets = ["alpha", "beta"]
            .into_iter()
            .map(|p| AdmittedTarget {
                profile: CredentialRef::new(p).unwrap(),
                account: request.account_ref.clone(),
                capabilities: BTreeSet::from(["coding".into()]),
                remaining: Some(config.settings.routing.global_capacity.clone()),
                provenance: "controlled-pure-policy-input".into(),
            })
            .collect();
        (root, config, request, targets)
    }

    #[test]
    fn required_target_never_silently_falls_back() {
        let (_root, config, mut request, mut targets) = inputs();
        targets.remove(0);
        request.required_target = request.preferred_target.clone();
        assert!(select(&request, &config, &targets).is_err());
        request.required_target = None;
        assert_eq!(
            select(&request, &config, &targets)
                .unwrap()
                .target_profile_ref
                .as_str(),
            "beta"
        );
        assert_eq!(
            route(&request, &config).unwrap_err().0,
            "native_admission_unavailable"
        );
    }

    #[test]
    fn fallback_obeys_account_capability_budget_and_known_availability() {
        let (_root, config, request, mut targets) = inputs();
        targets[0].remaining = None;
        targets[1].account = CredentialRef::new("other-account").unwrap();
        assert!(select(&request, &config, &targets).is_err());
        targets[1].account = request.account_ref.clone();
        targets[1].capabilities.clear();
        assert!(select(&request, &config, &targets).is_err());
        targets[1].capabilities.insert("coding".into());
        targets[1].remaining = Some(ResourceBudget::zero());
        assert!(select(&request, &config, &targets).is_err());
        targets[1].remaining = Some(config.settings.routing.global_capacity.clone());
        let decision = select(&request, &config, &targets).unwrap();
        assert!(decision.reasons.iter().any(|r| r == "approved_fallback"));
        assert!(matches!(decision.usage, Usage::Unavailable { .. }));
    }

    #[test]
    fn revocations_required_targets_and_disabled_fallback_dominate_rankings() {
        let (root, config, request, targets) = inputs();
        std::fs::write(
            root.path().join("p"),
            "revoked_destinations=['alpha']\n[routing]\npreference_order=['beta','alpha']",
        )
        .unwrap();
        let revoked =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        assert_eq!(
            select(&request, &revoked, &targets).unwrap_err().0,
            "route_scope_denied"
        );
        std::fs::write(root.path().join("p"), "[routing]\nfallback_allowed=false").unwrap();
        let binding =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        assert!(select(&request, &binding, &targets[1..]).is_err());
        let mut required = request;
        required.required_target = Some(targets[1].profile.clone());
        assert_eq!(
            select(&required, &config, &targets)
                .unwrap()
                .target_profile_ref,
            targets[1].profile
        );
    }

    #[test]
    fn reservations_bound_every_dimension_and_reject_arithmetic_overflow() {
        let limit = ResourceBudget::unit();
        for dimension in 0..7 {
            let mut extra = ResourceBudget::zero();
            match dimension {
                0 => extra.slots = 1,
                1 => extra.cpu_millis = 1,
                2 => extra.memory_mib = 1,
                3 => extra.processes = 1,
                4 => extra.output_bytes = 1,
                5 => extra.wall_time_ms = 1,
                _ => extra.spend_microunits = 1,
            }
            assert!(!limit.checked_add(&extra).unwrap().fits(&limit));
        }
        let mut invalid = limit.clone();
        invalid.slots = 0;
        assert!(invalid.validate().is_err());
        let mut overflow = limit.clone();
        overflow.memory_mib = u64::MAX;
        assert!(overflow.checked_add(&limit).is_err());
    }

    #[test]
    fn fairness_weights_do_not_give_strict_priority_or_change_running_usage() {
        let entries = vec![
            QueueEntry {
                project_id: ProjectId::new("high").unwrap(),
                priority_weight: 4,
                allocated_slots: 4,
                queued_revision: 1,
            },
            QueueEntry {
                project_id: ProjectId::new("low").unwrap(),
                priority_weight: 1,
                allocated_slots: 0,
                queued_revision: 2,
            },
        ];
        let mut fair = entries.clone();
        order_queue(&mut fair, &SchedulingPolicy::FairShare);
        assert_eq!(fair[0].project_id.as_str(), "low");
        let mut strict = entries;
        order_queue(&mut strict, &SchedulingPolicy::StrictPriority);
        assert_eq!(strict[0].project_id.as_str(), "high");
        assert_eq!(strict[0].allocated_slots, 4);
    }

    #[test]
    #[should_panic]
    fn trusted_admission_invariant_detects_duplicate_profiles() {
        let (_root, config, request, mut targets) = inputs();
        targets[1].profile = targets[0].profile.clone();
        let _ = select(&request, &config, &targets);
    }
}
