#![cfg(unix)]

use crazyagent::{
    config::{EffectiveConfig, resolve_config},
    coordination::*,
    engine::{Client, Engine, Server},
    protocol::*,
    routing::{ResourceBudget, RouteRequest},
};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

struct Harness {
    root: tempfile::TempDir,
    config: EffectiveConfig,
    engine: Arc<Engine>,
    revision: u64,
    sequence: u64,
}

impl Harness {
    async fn open() -> Self {
        Self::with_routing("approved_accounts = ['account']").await
    }

    async fn with_routing(routing: &str) -> Self {
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            root.path().join("g"),
            format!("allowed_destinations = ['profile-a', 'profile-b']\n[routing]\n{routing}"),
        )
        .unwrap();
        let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("db"), config.clone())
                .await
                .unwrap(),
        );
        Self {
            root,
            config,
            engine,
            revision: 0,
            sequence: 0,
        }
    }

    fn request(&mut self, command: Command) -> WireRequest {
        self.sequence += 1;
        WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("owner").unwrap(),
            request_id: RequestId::new(format!("op-{}", self.sequence)).unwrap(),
            expected_revision: Cursor::new(self.revision),
            command,
        }
    }

    async fn call(&mut self, command: Command) -> Outcome {
        let request = self.request(command);
        let reply = self.engine.apply(request).await.unwrap();
        self.revision = reply.revision.value();
        reply.outcome
    }

    async fn applied(&mut self, command: Command) {
        assert_eq!(self.call(command).await, Outcome::Applied);
    }

    async fn task(&mut self, project: &str, task: &str) -> ExecutionContext {
        self.applied(Command::CreateProject {
            project_id: ProjectId::new(project).unwrap(),
            spec: ProjectSpec {
                name: project.into(),
                repositories: vec![],
            },
        })
        .await;
        let context = ExecutionContext {
            project_id: ProjectId::new(project).unwrap(),
            task_id: TaskId::new(task).unwrap(),
            config_id: self.config.id.clone(),
            snapshot_id: SnapshotId::new("snapshot").unwrap(),
            baseline_id: BaselineId::new("baseline").unwrap(),
            allowed_destinations: self.config.settings.allowed_destinations.clone(),
            budget_ref: CredentialRef::new("budget").unwrap(),
        };
        self.applied(Command::PinTask {
            context: context.clone(),
        })
        .await;
        context
    }

    async fn state(&mut self, id: &PlanId) -> PlanState {
        let Outcome::Coordination { state } = self
            .call(Command::CoordinationState {
                plan_id: id.clone(),
            })
            .await
        else {
            panic!("durable plan state required")
        };
        *state
    }
}

#[tokio::test]
async fn account_authority_must_come_from_pinned_configuration() {
    let mut h = Harness::with_routing("").await;
    let context = h.task("p", "t").await;
    let plan = plan(context, "plan");
    rejected(
        h.call(Command::ProposePlan { plan }).await,
        "route_account_denied",
    );
    assert_eq!(h.revision, 2);
    h.engine.close().await.unwrap();
}

fn plan(context: ExecutionContext, id: &str) -> WorkPlan {
    WorkPlan {
        id: PlanId::new(id).unwrap(),
        config_id: context.config_id.clone(),
        scope: context,
        authority: PlanAuthority::Human,
        dependencies: vec![],
        tasks: ["a", "b"]
            .into_iter()
            .map(|id| WorkItem {
                id: TaskId::new(id).unwrap(),
                session_id: SessionId::new(id).unwrap(),
                role: Role::Worker,
                native_descendant: None,
                pool_ref: None,
                route: RouteRequest {
                    resources: ResourceBudget::unit(),
                    preferred_target: Some(CredentialRef::new("profile-a").unwrap()),
                    required_target: None,
                    required_capabilities: vec!["coding".into()],
                    account_ref: CredentialRef::new("account").unwrap(),
                    budget_ref: CredentialRef::new("budget").unwrap(),
                },
            })
            .collect(),
    }
}

fn owner(action: OwnerAction) -> Command {
    Command::OwnerCoordination { action }
}

fn rejected(outcome: Outcome, expected: &str) {
    assert_eq!(
        outcome,
        Outcome::Rejected {
            code: expected.into()
        }
    );
}

async fn approved(h: &mut Harness, plan: &WorkPlan) {
    h.applied(Command::ProposePlan { plan: plan.clone() }).await;
    h.applied(owner(OwnerAction::ApprovePlan {
        plan_id: plan.id.clone(),
    }))
    .await;
}

async fn hold(h: &mut Harness, plan: &WorkPlan, item: &str) -> Outcome {
    h.call(owner(OwnerAction::Reserve {
        plan_id: plan.id.clone(),
        item_id: TaskId::new(item).unwrap(),
    }))
    .await
}

fn message(source: &WorkPlan, target: &WorkPlan, id: &str) -> ScopedMessage {
    ScopedMessage {
        id: RequestId::new(id).unwrap(),
        source_plan: source.id.clone(),
        target_plan: target.id.clone(),
        sender: source.tasks[0].session_id.clone(),
        recipient: target.tasks[1].session_id.clone(),
        config_id: source.config_id.clone(),
        body: "peer finding".into(),
    }
}

#[tokio::test]
async fn resource_budget_applies_to_all_roles_and_projects_without_releasing_revoked_holds() {
    let mut h = Harness::open().await;
    let context = h.task("p1", "t1").await;
    let mut first = plan(context, "first");
    first.tasks[0].role = Role::WorkspaceLeader;
    first.tasks[1].role = Role::NativeDescendant;
    first.tasks[0].route.resources.slots = 8;
    first.tasks[1].route.resources.slots = 8;
    approved(&mut h, &first).await;
    assert_eq!(hold(&mut h, &first, "a").await, Outcome::Applied);
    assert_eq!(hold(&mut h, &first, "b").await, Outcome::Applied);
    let context = h.task("p2", "t2").await;
    let second = plan(context, "second");
    approved(&mut h, &second).await;
    let before = h.state(&second.id).await;
    rejected(
        hold(&mut h, &second, "a").await,
        "resource_budget_exhausted",
    );
    assert_eq!(h.state(&second.id).await, before);
    h.applied(owner(OwnerAction::RevokePlan {
        plan_id: first.id.clone(),
    }))
    .await;
    rejected(
        hold(&mut h, &second, "a").await,
        "resource_budget_exhausted",
    );
    assert_eq!(h.state(&first.id).await.reservations.len(), 2);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn fair_sharing_reserves_for_other_projects_before_more_work_in_the_first() {
    let mut h = Harness::open().await;
    let context = h.task("p1", "t1").await;
    let first = plan(context, "first");
    approved(&mut h, &first).await;
    assert_eq!(hold(&mut h, &first, "a").await, Outcome::Applied);
    let context = h.task("p2", "t2").await;
    let second = plan(context, "second");
    approved(&mut h, &second).await;
    rejected(
        hold(&mut h, &first, "b").await,
        "allocation_queued_by_policy",
    );
    assert_eq!(hold(&mut h, &second, "a").await, Outcome::Applied);
    assert_eq!(hold(&mut h, &first, "b").await, Outcome::Applied);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn links_scope_cross_task_and_cross_project_delivery_and_removal() {
    let mut h = Harness::open().await;
    let context = h.task("p1", "t1").await;
    let first = plan(context, "first");
    approved(&mut h, &first).await;
    let mut context = first.scope.clone();
    context.task_id = TaskId::new("t2").unwrap();
    h.applied(Command::PinTask {
        context: context.clone(),
    })
    .await;
    let second = plan(context, "second");
    approved(&mut h, &second).await;
    let same_project = message(&first, &second, "same-project");
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: same_project.clone(),
        }))
        .await,
        "coordination_link_required",
    );
    let link = CoordinationLink {
        id: RequestId::new("link").unwrap(),
        source_plan: first.id.clone(),
        target_plan: second.id.clone(),
        source_config: first.config_id.clone(),
        target_config: second.config_id.clone(),
        allow_cross_project: false,
    };
    h.applied(owner(OwnerAction::Link { link: link.clone() }))
        .await;
    h.applied(owner(OwnerAction::SendMessage {
        message: same_project,
    }))
    .await;
    assert_eq!(h.state(&second.id).await.messages.len(), 1);
    h.applied(owner(OwnerAction::RemoveLink {
        link_id: link.id.clone(),
    }))
    .await;
    let Outcome::Messages { messages } = h
        .call(Command::Mailbox {
            plan_id: second.id.clone(),
            recipient: second.tasks[1].session_id.clone(),
            after: Cursor::new(0),
        })
        .await
    else {
        panic!("mailbox required")
    };
    assert!(messages.is_empty());
    let context = h.task("p3", "t3").await;
    let third = plan(context, "third");
    approved(&mut h, &third).await;
    let mut cross = link;
    cross.id = RequestId::new("cross").unwrap();
    cross.target_plan = third.id.clone();
    cross.target_config = third.config_id.clone();
    rejected(
        h.call(owner(OwnerAction::Link {
            link: cross.clone(),
        }))
        .await,
        "cross_project_authority_required",
    );
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: message(&first, &third, "denied"),
        }))
        .await,
        "coordination_link_required",
    );
    assert!(h.state(&third.id).await.messages.is_empty());
    cross.allow_cross_project = true;
    h.applied(owner(OwnerAction::Link { link: cross })).await;
    h.applied(owner(OwnerAction::SendMessage {
        message: message(&first, &third, "allowed"),
    }))
    .await;
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn message_identity_is_global_and_revocation_does_not_erase_evidence() {
    let mut h = Harness::open().await;
    let context = h.task("p1", "t1").await;
    let first = plan(context, "first");
    approved(&mut h, &first).await;
    h.applied(owner(OwnerAction::SendMessage {
        message: message(&first, &first, "unique"),
    }))
    .await;
    let context = h.task("p2", "t2").await;
    let second = plan(context, "second");
    approved(&mut h, &second).await;
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: message(&second, &second, "unique"),
        }))
        .await,
        "message_identity_conflict",
    );
    assert!(h.state(&second.id).await.messages.is_empty());
    h.applied(owner(OwnerAction::RevokePlan {
        plan_id: first.id.clone(),
    }))
    .await;
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: message(&first, &first, "later"),
        }))
        .await,
        "plan_revoked",
    );
    assert_eq!(h.state(&first.id).await.messages.len(), 1);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn dependency_holds_cannot_claim_completion_and_descendants_need_capacity() {
    let mut h = Harness::open().await;
    let context = h.task("p", "t").await;
    let mut plan = plan(context, "plan");
    plan.tasks[0].native_descendant = Some(SessionId::new("child").unwrap());
    rejected(
        h.call(Command::ProposePlan { plan: plan.clone() }).await,
        "descendant_budget_missing",
    );
    plan.tasks[0].route.resources.slots = 2;
    plan.dependencies.push(Dependency {
        before: TaskId::new("a").unwrap(),
        after: TaskId::new("b").unwrap(),
    });
    approved(&mut h, &plan).await;
    assert_eq!(hold(&mut h, &plan, "a").await, Outcome::Applied);
    rejected(hold(&mut h, &plan, "b").await, "dependencies_unfinished");
    assert_eq!(h.state(&plan.id).await.reservations.len(), 1);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn autonomy_is_explicit_and_pinned_instead_of_a_proposal_approval_flag() {
    let mut h = Harness::open().await;
    let context = h.task("p", "t").await;
    let mut automatic = plan(context, "automatic");
    automatic.authority = PlanAuthority::ConfiguredAutonomy;
    rejected(
        h.call(Command::ProposePlan {
            plan: automatic.clone(),
        })
        .await,
        "human_approval_required",
    );
    std::fs::write(
        h.root.path().join("p"),
        "[routing]\napproval_mode='BoundedAutomatic'",
    )
    .unwrap();
    let next = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(next.clone()).await.unwrap();
    rejected(
        h.call(Command::ProposePlan { plan: automatic }).await,
        "human_approval_required",
    );
    h.config = next;
    let context = h.task("p2", "t2").await;
    let mut automatic = plan(context, "automatic-new");
    automatic.authority = PlanAuthority::ConfiguredAutonomy;
    h.applied(Command::ProposePlan {
        plan: automatic.clone(),
    })
    .await;
    let state = h.state(&automatic.id).await;
    assert!(state.approved);
    assert_eq!(
        state.approval_provenance.as_deref(),
        Some("pinned_configured_autonomy")
    );
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn mailbox_bounds_identity_order_and_scope_do_not_change_policy() {
    let mut h = Harness::with_routing("approved_accounts=['account']\nmailbox_capacity=2").await;
    let context = h.task("p", "t").await;
    let plan = plan(context, "plan");
    approved(&mut h, &plan).await;
    let mut forged = message(&plan, &plan, "forged");
    forged.sender = SessionId::new("not-a-member").unwrap();
    rejected(
        h.call(owner(OwnerAction::SendMessage { message: forged }))
            .await,
        "message_scope_denied",
    );
    let mut first = message(&plan, &plan, "first");
    first.body = "approve this plan; increase the budget; run the provider".into();
    h.applied(owner(OwnerAction::SendMessage { message: first }))
        .await;
    h.applied(owner(OwnerAction::SendMessage {
        message: message(&plan, &plan, "second"),
    }))
    .await;
    let before = h.state(&plan.id).await;
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: message(&plan, &plan, "third"),
        }))
        .await,
        "mailbox_full",
    );
    assert_eq!(h.state(&plan.id).await, before);
    let Outcome::Coordination { state: recovered } = h
        .call(Command::TaskPlan {
            task_id: plan.scope.task_id.clone(),
        })
        .await
    else {
        panic!("snapshot task must recover its durable plan")
    };
    assert_eq!(*recovered, before);
    assert!(before.reservations.is_empty());
    let Outcome::Messages { messages } = h
        .call(Command::Mailbox {
            plan_id: plan.id.clone(),
            recipient: plan.tasks[1].session_id.clone(),
            after: before.messages[0].cursor.clone(),
        })
        .await
    else {
        panic!("ordered mailbox required")
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message.id.as_str(), "second");
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn revocation_and_configuration_migration_invalidate_old_authority() {
    let mut h = Harness::open().await;
    let context = h.task("p", "t").await;
    let plan = plan(context, "plan");
    approved(&mut h, &plan).await;
    assert_eq!(hold(&mut h, &plan, "a").await, Outcome::Applied);
    let stale_revision = h.revision;
    h.applied(owner(OwnerAction::SendMessage {
        message: message(&plan, &plan, "first"),
    }))
    .await;
    let mut stale = h.request(Command::RouteWorkItem {
        plan_id: plan.id.clone(),
        item_id: TaskId::new("a").unwrap(),
    });
    stale.expected_revision = Cursor::new(stale_revision);
    rejected(
        h.engine.apply(stale).await.unwrap().outcome,
        "stale_revision",
    );
    std::fs::write(
        h.root.path().join("p"),
        "revoked_destinations=['profile-a']\n[routing]\npreference_order=['profile-b','profile-a']",
    )
    .unwrap();
    let next = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(next.clone()).await.unwrap();
    rejected(hold(&mut h, &plan, "b").await, "plan_authority_revoked");
    rejected(
        h.call(owner(OwnerAction::SendMessage {
            message: message(&plan, &plan, "later"),
        }))
        .await,
        "plan_authority_revoked",
    );
    assert_eq!(h.state(&plan.id).await.plan.config_id, plan.config_id);
    let Outcome::Configuration { config: pinned } = h
        .call(Command::TaskConfig {
            task_id: plan.scope.task_id.clone(),
        })
        .await
    else {
        panic!("pinned config required")
    };
    assert!(pinned.settings.routing.preference_order.is_empty());
    h.applied(Command::MigrateTask {
        task_id: plan.scope.task_id.clone(),
        config_id: next.id,
    })
    .await;
    rejected(hold(&mut h, &plan, "b").await, "plan_context_mismatch");
    assert_eq!(h.state(&plan.id).await.reservations.len(), 1);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn strict_priority_affects_future_allocations_without_changing_existing_holds() {
    let mut h = Harness::open().await;
    let context = h.task("p1", "t1").await;
    let first = plan(context, "first");
    approved(&mut h, &first).await;
    assert_eq!(hold(&mut h, &first, "a").await, Outcome::Applied);
    std::fs::write(h.root.path().join("g"),
        "allowed_destinations=['profile-a','profile-b']\n[routing]\napproved_accounts=['account']\nscheduling='StrictPriority'\npriority_weight=4").unwrap();
    h.config = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(h.config.clone()).await.unwrap();
    let context = h.task("p2", "t2").await;
    let second = plan(context, "second");
    approved(&mut h, &second).await;
    rejected(
        hold(&mut h, &first, "b").await,
        "allocation_queued_by_policy",
    );
    assert_eq!(hold(&mut h, &second, "a").await, Outcome::Applied);
    assert_eq!(hold(&mut h, &second, "b").await, Outcome::Applied);
    assert_eq!(h.state(&first.id).await.reservations.len(), 1);
    let first_hold = h.state(&first.id).await.reservations[&TaskId::new("a").unwrap()].clone();
    let second_hold = h.state(&second.id).await.reservations[&TaskId::new("a").unwrap()].clone();
    assert_eq!(
        first_hold.scheduling,
        crazyagent::routing::SchedulingPolicy::FairShare
    );
    assert_eq!(
        second_hold.scheduling,
        crazyagent::routing::SchedulingPolicy::StrictPriority
    );
    assert_ne!(first_hold.shared_config_id, second_hold.shared_config_id);
    assert_eq!(hold(&mut h, &first, "b").await, Outcome::Applied);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn additive_schema_keeps_legacy_task_pins_across_new_engine_configuration() {
    let mut h = Harness::open().await;
    let mut legacy = resolve_config(
        &h.root.path().join("absent-g"),
        &h.root.path().join("absent-p"),
    )
    .unwrap();
    legacy
        .descriptors
        .retain(|name, _| !name.starts_with("routing."));
    legacy.id =
        ConfigId::new("472693a858862960f058c909a411a19aa8f8d03e94240e0aa4ccb8c1ad4f3e0c").unwrap();
    legacy.validate().unwrap();
    h.engine.close().await.unwrap();
    let old_database = h.root.path().join("legacy-db");
    let connection = rusqlite::Connection::open(&old_database).unwrap();
    std::fs::set_permissions(&old_database, std::fs::Permissions::from_mode(0o600)).unwrap();
    connection
        .execute_batch("CREATE TABLE configs (id TEXT PRIMARY KEY, body TEXT NOT NULL);")
        .unwrap();
    connection
        .execute(
            "INSERT INTO configs VALUES (?1,?2)",
            rusqlite::params![legacy.id.as_str(), serde_json::to_string(&legacy).unwrap()],
        )
        .unwrap();
    drop(connection);
    h.engine = Arc::new(Engine::open(&old_database, legacy.clone()).await.unwrap());
    let modern = h.config.clone();
    h.config = legacy.clone();
    let context = h.task("legacy-project", "legacy-task").await;
    h.engine.close().await.unwrap();
    h.engine = Arc::new(Engine::open(&old_database, modern).await.unwrap());
    let Outcome::Configuration { config } = h
        .call(Command::TaskConfig {
            task_id: context.task_id,
        })
        .await
    else {
        panic!("legacy pin required")
    };
    assert_eq!(*config, legacy);
    config.validate().unwrap();
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn approval_reservations_and_mailbox_survive_restart_without_native_dispatch() {
    let mut h = Harness::open().await;
    let context = h.task("project", "task").await;
    let plan = plan(context, "plan");
    h.applied(Command::ProposePlan { plan: plan.clone() }).await;
    assert!(!h.state(&plan.id).await.approved);
    rejected(
        h.call(owner(OwnerAction::Reserve {
            plan_id: plan.id.clone(),
            item_id: TaskId::new("a").unwrap(),
        }))
        .await,
        "plan_not_approved",
    );
    let approve = h.request(owner(OwnerAction::ApprovePlan {
        plan_id: plan.id.clone(),
    }));
    assert!(serde_json::from_slice::<WireRequest>(&serde_json::to_vec(&approve).unwrap()).is_err());
    let reply = h.engine.apply(approve.clone()).await.unwrap();
    assert_eq!(reply.outcome, Outcome::Applied);
    h.revision = reply.revision.value();
    for item in ["a", "b"] {
        h.applied(owner(OwnerAction::Reserve {
            plan_id: plan.id.clone(),
            item_id: TaskId::new(item).unwrap(),
        }))
        .await;
    }
    rejected(
        h.call(Command::RouteWorkItem {
            plan_id: plan.id.clone(),
            item_id: TaskId::new("a").unwrap(),
        })
        .await,
        "native_admission_unavailable",
    );
    let message = ScopedMessage {
        id: RequestId::new("message-1").unwrap(),
        source_plan: plan.id.clone(),
        target_plan: plan.id.clone(),
        sender: SessionId::new("a").unwrap(),
        recipient: SessionId::new("b").unwrap(),
        config_id: plan.config_id.clone(),
        body: "finding: inspect the real files".into(),
    };
    let send = h.request(owner(OwnerAction::SendMessage {
        message: message.clone(),
    }));
    let sent = h.engine.apply(send.clone()).await.unwrap();
    assert_eq!(sent.outcome, Outcome::Applied);
    h.revision = sent.revision.value();
    let before = h.state(&plan.id).await;
    assert_eq!(before.reservations.len(), 2);
    h.engine.close().await.unwrap();
    h.engine = Arc::new(
        Engine::open(&h.root.path().join("db"), h.config.clone())
            .await
            .unwrap(),
    );
    assert_eq!(h.engine.apply(approve).await.unwrap(), reply);
    assert_eq!(h.engine.apply(send).await.unwrap(), sent);
    assert_eq!(h.state(&plan.id).await, before);
    let Outcome::Messages { messages } = h
        .call(Command::Mailbox {
            plan_id: plan.id.clone(),
            recipient: SessionId::new("b").unwrap(),
            after: Cursor::new(0),
        })
        .await
    else {
        panic!("mailbox required")
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message, message);
    assert_eq!(messages[0].cursor, sent.revision);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn proposals_reject_cycles_scope_forgery_and_stale_revisions_atomically() {
    let mut h = Harness::open().await;
    let context = h.task("project", "task").await;
    let valid = plan(context, "plan");
    let mut cyclic = valid.clone();
    cyclic.dependencies = vec![
        Dependency {
            before: TaskId::new("a").unwrap(),
            after: TaskId::new("b").unwrap(),
        },
        Dependency {
            before: TaskId::new("b").unwrap(),
            after: TaskId::new("a").unwrap(),
        },
    ];
    rejected(
        h.call(Command::ProposePlan { plan: cyclic }).await,
        "dependency_cycle",
    );
    let mut forged = valid.clone();
    forged.scope.project_id = ProjectId::new("other-project").unwrap();
    rejected(
        h.call(Command::ProposePlan { plan: forged }).await,
        "plan_context_mismatch",
    );
    let mut forged = valid.clone();
    forged.tasks[0].route.budget_ref = CredentialRef::new("other-budget").unwrap();
    rejected(
        h.call(Command::ProposePlan { plan: forged }).await,
        "route_scope_denied",
    );
    let mut stale = h.request(Command::ProposePlan {
        plan: valid.clone(),
    });
    stale.expected_revision = Cursor::new(0);
    rejected(
        h.engine.apply(stale).await.unwrap().outcome,
        "stale_revision",
    );
    assert_eq!(h.revision, 2);
    h.applied(Command::ProposePlan {
        plan: valid.clone(),
    })
    .await;
    assert!(h.state(&valid.id).await.reservations.is_empty());
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn socket_proposal_cannot_carry_human_approval() {
    let mut h = Harness::open().await;
    let context = h.task("project", "task").await;
    let plan = plan(context, "plan");
    let path = h.root.path().join("engine.sock");
    let server = Server::bind(&path, h.engine.clone(), &h.config).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client = Client::new(path, Duration::from_secs(2)).unwrap();
    let request = h.request(Command::ProposePlan { plan: plan.clone() });
    let mut forged = serde_json::to_value(&request).unwrap();
    forged["command"]["plan"]["approved"] = true.into();
    assert!(serde_json::from_value::<WireRequest>(forged).is_err());
    assert_eq!(
        client.request(request).await.unwrap().outcome,
        Outcome::Applied
    );
    assert!(!h.state(&plan.id).await.approved);
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    h.engine.close().await.unwrap();
}
