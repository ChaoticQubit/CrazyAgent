use super::*;
use crate::coordination::{PlanAuthority, Reservation, WorkItem, WorkPlan};
use crate::protocol::{BaselineId, ConfigId, CredentialRef, SessionId, SnapshotId, TaskId};
use crate::routing::{ResourceBudget, RouteRequest, SchedulingPolicy};
use crate::worker_dispatch::{DeniedReason, DeniedReceipt};

fn selection() -> Selection {
    Selection {
        database: "a".repeat(64),
        epoch: "b".repeat(64),
        task: ExecutionContext {
            project_id: crate::protocol::ProjectId::new("unit-project").unwrap(),
            task_id: TaskId::new("unit-parent").unwrap(),
            config_id: ConfigId::new("unit-config").unwrap(),
            snapshot_id: SnapshotId::new("unit-snapshot").unwrap(),
            baseline_id: BaselineId::new("unit-baseline").unwrap(),
            allowed_destinations: vec![],
            budget_ref: CredentialRef::new("unit-budget").unwrap(),
        },
        plan_id: PlanId::new("unit-plan").unwrap(),
    }
}

fn attempt(index: usize) -> AttemptRef {
    AttemptRef {
        plan_id: selection().plan_id,
        item_id: TaskId::new(format!("unit-item-{index}")).unwrap(),
        reservation_cursor: Cursor::new(10 + index as u64),
    }
}

fn state(count: usize) -> State {
    let mut state = State::default();
    state
        .select(
            Some(selection()),
            (0..count).map(attempt).collect(),
            true,
            Cursor::new(30),
        )
        .unwrap();
    state
}

fn view(query: &Query, status: DispatchState) -> DispatchView {
    DispatchView {
        dispatch_id: query.dispatch_id.clone(),
        attempt: query.attempt.clone(),
        state: status,
        delivery_generation: 1,
        binding_digest: "c".repeat(64),
        lease_digest: "d".repeat(64),
        offer_digest: None,
        cancellation: DispatchCancellation::None,
        receipt: None,
    }
}

fn denied(query: &Query) -> DispatchView {
    let mut value = view(query, DispatchState::DeliveredDenied);
    value.offer_digest = Some("e".repeat(64));
    value.receipt = Some(DeniedReceipt {
        dispatch_id: query.dispatch_id.clone(),
        attempt: query.attempt.clone(),
        delivery_generation: value.delivery_generation,
        database_instance: query.selection.database.clone(),
        offer_engine_epoch: query.selection.epoch.clone(),
        registration_id: RequestId::new("unit-registration").unwrap(),
        runner_boot_id: RequestId::new("unit-runner-boot").unwrap(),
        controller_generation: 1,
        offer_digest: value.offer_digest.clone().unwrap(),
        journal_sequence: 1,
        reason: DeniedReason::NativeRuntimeAdmissionUnavailable,
        native_identity: None,
    });
    value
}

#[test]
fn actual_plan_reservation_selects_item_instead_of_parent_task() {
    let selected = selection();
    let item = WorkItem {
        id: attempt(0).item_id,
        session_id: SessionId::new("unit-worker-session").unwrap(),
        role: Role::Worker,
        native_descendant: None,
        pool_ref: None,
        route: RouteRequest {
            preferred_target: None,
            required_target: None,
            required_capabilities: vec![],
            account_ref: CredentialRef::new("unit-account").unwrap(),
            budget_ref: selected.task.budget_ref.clone(),
            resources: ResourceBudget::unit(),
        },
    };
    let reservation = Reservation {
        resources: ResourceBudget::unit(),
        config_id: selected.task.config_id.clone(),
        shared_config_id: selected.task.config_id.clone(),
        shared_capacity: ResourceBudget::unit(),
        scheduling: SchedulingPolicy::FairShare,
        priority_weight: 1,
        budget_ref: selected.task.budget_ref.clone(),
        cursor: attempt(0).reservation_cursor,
        provenance: "engine_planning_hold_not_native_dispatch".into(),
        binding: None,
    };
    let mut plan = PlanState {
        plan: WorkPlan {
            id: selected.plan_id.clone(),
            scope: selected.task.clone(),
            tasks: vec![item.clone()],
            dependencies: vec![],
            authority: PlanAuthority::Human,
            config_id: selected.task.config_id.clone(),
        },
        approved: true,
        revoked: false,
        approval_provenance: Some("unit-owner-input".into()),
        queued_revision: 2,
        reservations: [(item.id, reservation)].into(),
        messages: vec![],
    };
    assert_eq!(
        reserved_attempts(&selected.task, &plan).unwrap(),
        vec![attempt(0)]
    );
    assert_ne!(attempt(0).item_id, selected.task.task_id);
    plan.plan.scope.task_id = TaskId::new("another-parent").unwrap();
    assert!(reserved_attempts(&selected.task, &plan).is_err());
}

#[test]
fn dispatch_poll_is_single_pending_and_round_robins_all_sixteen_attempts() {
    let mut state = state(16);
    for index in 0..17 {
        let query = state.request().unwrap();
        assert_eq!(query.attempt, attempt(index % 16));
        assert_eq!(
            query.dispatch_id,
            dispatch_id(&selection().database, &query.attempt).unwrap()
        );
        assert!(state.request().is_none());
        state.complete(&query, Cursor::new(30), None).unwrap();
    }
    assert_eq!(state.rows.len(), 16);
    assert!(
        State::default()
            .select(
                Some(selection()),
                (0..17).map(attempt).collect(),
                true,
                Cursor::new(30)
            )
            .is_err()
    );
}

#[test]
fn missing_dispatch_is_only_reported_after_a_current_protected_read() {
    let mut state = state(1);
    assert!(state.lines().contains("no current protected read"));
    assert!(!state.lines().contains("Not found"));
    let query = state.request().unwrap();
    state.complete(&query, Cursor::new(30), None).unwrap();
    assert!(state.lines().contains("Not found: no recorded dispatch"));
    assert!(state.lines().contains("owner revision 30"));
    assert!(state.lines().contains(attempt(0).item_id.as_str()));
    assert!(!state.lines().contains("Delivered"));
}

#[test]
fn every_dispatch_state_and_cancellation_is_rendered_without_claiming_execution() {
    let cases = [
        (DispatchState::Queued, "Queued: committed outbox"),
        (DispatchState::RunnerUnavailable, "Unavailable: runner"),
        (DispatchState::Offered, "Uncertain: offer sent"),
        (
            DispatchState::OutcomeUnknown,
            "Uncertain: delivery outcome unknown",
        ),
        (
            DispatchState::ReconciliationRequired,
            "Uncertain: reconciliation required",
        ),
        (DispatchState::NotConsumed, "Not consumed"),
        (
            DispatchState::BlockedRecovery,
            "Unavailable: recovery blocked",
        ),
        (
            DispatchState::CancelledBeforeOffer,
            "Cancelled before offer",
        ),
        (DispatchState::DeliveredDenied, "Delivered, denied"),
    ];
    for (status, expected) in cases {
        let mut state = state(1);
        let query = state.request().unwrap();
        let mut value = if status == DispatchState::DeliveredDenied {
            denied(&query)
        } else {
            view(&query, status)
        };
        value.cancellation = DispatchCancellation::Requested;
        state
            .complete(&query, Cursor::new(30), Some(value))
            .unwrap();
        let rendered = state.lines();
        assert!(rendered.contains(expected), "{rendered}");
        assert!(rendered.contains("Cancellation requested; completion is not confirmed"));
        assert!(rendered.contains("No provider or native execution is confirmed"));
        assert!(!rendered.contains("running successfully"));
    }
}

#[test]
fn delivered_denial_requires_matching_retained_receipt_and_no_native_identity() {
    for corruption in 0..6 {
        let mut state = state(1);
        let query = state.request().unwrap();
        let mut value = denied(&query);
        match corruption {
            0 => value.receipt = None,
            1 => value.receipt.as_mut().unwrap().database_instance = "f".repeat(64),
            2 => {
                value.receipt.as_mut().unwrap().native_identity =
                    Some(RequestId::new("unit-native").unwrap())
            }
            3 => value.receipt.as_mut().unwrap().attempt = attempt(1),
            4 => value.receipt.as_mut().unwrap().delivery_generation += 1,
            5 => value.receipt.as_mut().unwrap().offer_digest = "f".repeat(64),
            _ => unreachable!(),
        }
        assert!(
            state
                .complete(&query, Cursor::new(30), Some(value))
                .is_err()
        );
        assert!(!state.lines().contains("Delivered, denied"));
        assert!(state.lines().contains("Uncertain"));
    }
}

#[test]
fn selection_attempt_and_epoch_replacement_ignore_old_completion() {
    for replacement in 0..4 {
        let mut state = state(1);
        let query = state.request().unwrap();
        let mut next = selection();
        let mut attempts = vec![attempt(0)];
        match replacement {
            0 => next.task.task_id = TaskId::new("another-parent").unwrap(),
            1 => next.database = "f".repeat(64),
            2 => next.epoch = "f".repeat(64),
            3 => attempts[0].reservation_cursor = Cursor::new(31),
            _ => unreachable!(),
        }
        state
            .select(Some(next), attempts, true, Cursor::new(31))
            .unwrap();
        state
            .complete(&query, Cursor::new(32), Some(denied(&query)))
            .unwrap();
        assert!(!state.lines().contains("Delivered, denied"));
        assert!(state.request().is_some());
    }
}

#[test]
fn limited_mode_retains_stale_readback_and_stops_owner_polling() {
    let mut state = state(1);
    let query = state.request().unwrap();
    state
        .complete(
            &query,
            Cursor::new(30),
            Some(view(&query, DispatchState::Queued)),
        )
        .unwrap();
    state
        .select(Some(selection()), vec![attempt(0)], false, Cursor::new(30))
        .unwrap();
    assert!(state.request().is_none());
    let rendered = state.lines();
    assert!(rendered.contains("limited mode"));
    assert!(rendered.contains("STALE"));
    assert!(rendered.contains("Queued: committed outbox"));
    state
        .select(Some(selection()), vec![attempt(0)], true, Cursor::new(31))
        .unwrap();
    let query = state.request().unwrap();
    state.complete(&query, Cursor::new(30), None).unwrap();
    assert!(state.lines().contains("STALE"));
    assert!(!state.lines().contains("Not found"));
}

#[test]
fn plan_and_supervision_use_the_actual_dispatch_renderer() {
    let mut state = state(1);
    let query = state.request().unwrap();
    state
        .complete(&query, Cursor::new(30), Some(denied(&query)))
        .unwrap();
    let mut app = crate::tui::app::App::new();
    for _ in 0..3 {
        app.reduce(crate::tui::app::UiEvent::FocusNext).unwrap();
    }
    for expected_focus in [
        crate::tui::app::Focus::Plan,
        crate::tui::app::Focus::Supervision,
    ] {
        app.dispatch = state.clone();
        assert_eq!(app.focus(), expected_focus);
        let rendered = crate::tui::views::body(&app);
        assert!(rendered.contains("Delivered, denied"), "{rendered}");
        assert!(rendered.contains(query.dispatch_id.as_str()));
        assert!(rendered.contains("native_runtime_admission_unavailable"));
        if expected_focus == crate::tui::app::Focus::Plan {
            app.reduce(crate::tui::app::UiEvent::FocusNext).unwrap();
        }
    }
}

#[test]
fn corrupt_attempt_bound_trips_internal_assertion() {
    let mut state = state(16);
    let extra = state.rows[0].clone();
    state.rows.push(extra);
    assert!(std::panic::catch_unwind(|| state.assert_consistent()).is_err());
}
