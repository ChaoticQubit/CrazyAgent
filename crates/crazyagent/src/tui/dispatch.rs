use crate::coordination::{PlanState, Role};
use crate::engine::owner::{OwnerData, OwnerReadReply};
use crate::protocol::{Cursor, ExecutionContext, PlanId, RequestId};
use crate::worker_dispatch::{
    AttemptRef, DispatchCancellation, DispatchState, DispatchView, dispatch_id,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub database: String,
    pub epoch: String,
    pub task: ExecutionContext,
    pub plan_id: PlanId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Query {
    pub selection: Selection,
    pub generation: u64,
    pub attempt: AttemptRef,
    pub dispatch_id: RequestId,
    pub revision: Cursor,
}

pub(super) struct Event {
    pub query: Query,
    pub result: Result<OwnerReadReply, String>,
}

#[derive(Clone)]
struct Row {
    attempt: AttemptRef,
    id: RequestId,
    view: Option<DispatchView>,
    revision: Option<Cursor>,
    verified: Option<u64>,
    problem: Option<String>,
}

#[derive(Clone)]
pub(super) struct State {
    selection: Option<Selection>,
    rows: Vec<Row>,
    generation: u64,
    pending: Option<Query>,
    next: usize,
    online: bool,
    revision: Cursor,
}

impl Default for State {
    fn default() -> Self {
        Self {
            selection: None,
            rows: Vec::new(),
            generation: 0,
            pending: None,
            next: 0,
            online: false,
            revision: Cursor::new(0),
        }
    }
}

pub(super) fn reserved_attempts(
    task: &ExecutionContext,
    state: &PlanState,
) -> Result<Vec<AttemptRef>, String> {
    if &state.plan.scope != task || state.plan.tasks.len() > 16 || state.reservations.len() > 16 {
        return Err("Dispatch unavailable: current plan does not match the selected task".into());
    }
    let mut attempts = Vec::new();
    for item in state
        .plan
        .tasks
        .iter()
        .filter(|item| item.role == Role::Worker)
    {
        if let Some(reservation) = state.reservations.get(&item.id) {
            if reservation.cursor.value() == 0 {
                return Err("Dispatch unavailable: invalid reservation cursor".into());
            }
            attempts.push(AttemptRef {
                plan_id: state.plan.id.clone(),
                item_id: item.id.clone(),
                reservation_cursor: reservation.cursor.clone(),
            });
        }
    }
    assert!(attempts.len() <= 16);
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.plan_id == state.plan.id)
    );
    Ok(attempts)
}

impl State {
    pub fn select(
        &mut self,
        selection: Option<Selection>,
        attempts: Vec<AttemptRef>,
        online: bool,
        revision: Cursor,
    ) -> Result<(), String> {
        if attempts.len() > 16
            || (selection.is_none() && !attempts.is_empty())
            || attempts.iter().any(|attempt| {
                selection
                    .as_ref()
                    .is_none_or(|selected| selected.plan_id != attempt.plan_id)
                    || attempt.reservation_cursor.value() == 0
            })
        {
            return Err("Dispatch selection exceeds the current plan bounds".into());
        }
        let changed = self.selection != selection
            || !self.rows.iter().map(|row| &row.attempt).eq(attempts.iter());
        if changed || self.online != online {
            let generation = self
                .generation
                .checked_add(1)
                .ok_or("Dispatch generation exhausted")?;
            if changed {
                let mut rows = Vec::new();
                for attempt in attempts {
                    let selected = selection
                        .as_ref()
                        .expect("validated attempts require selection");
                    let id = dispatch_id(&selected.database, &attempt).map_err(|error| error.0)?;
                    if rows.iter().any(|row: &Row| row.id == id) {
                        return Err("Duplicate dispatch attempt in the selected plan".into());
                    }
                    rows.push(Row {
                        attempt,
                        id,
                        view: None,
                        revision: None,
                        verified: None,
                        problem: None,
                    });
                }
                self.rows = rows;
                self.selection = selection;
                self.next = 0;
            }
            self.generation = generation;
            self.pending = None;
        }
        self.online = online;
        self.revision = revision;
        self.assert_consistent();
        Ok(())
    }

    pub fn request(&mut self) -> Option<Query> {
        self.assert_consistent();
        if !self.online || self.pending.is_some() || self.rows.is_empty() {
            return None;
        }
        let selected = self
            .selection
            .as_ref()
            .expect("dispatch rows require selection");
        let row = &self.rows[self.next];
        let query = Query {
            selection: selected.clone(),
            generation: self.generation,
            attempt: row.attempt.clone(),
            dispatch_id: row.id.clone(),
            revision: self.revision.clone(),
        };
        self.next = (self.next + 1) % self.rows.len();
        self.pending = Some(query.clone());
        assert!(self.next < self.rows.len());
        assert!(self.matches(&query));
        Some(query)
    }

    fn matches(&self, query: &Query) -> bool {
        self.online
            && self.generation == query.generation
            && self.selection.as_ref() == Some(&query.selection)
            && self.pending.as_ref() == Some(query)
    }

    fn fail(&mut self, query: &Query, problem: &str) -> Option<String> {
        if !self.matches(query) {
            return None;
        }
        self.pending = None;
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.id == query.dispatch_id)
            .expect("pending dispatch belongs to one retained row");
        let problem = super::views::safe_text(problem, 512);
        row.verified = None;
        row.problem = Some(problem.clone());
        assert!(self.pending.is_none());
        assert!(row.verified.is_none());
        Some(problem)
    }

    fn complete(
        &mut self,
        query: &Query,
        revision: Cursor,
        view: Option<DispatchView>,
    ) -> Result<(), String> {
        if !self.matches(query) {
            return Ok(());
        }
        self.pending = None;
        let row = self
            .rows
            .iter_mut()
            .find(|row| row.id == query.dispatch_id)
            .expect("pending dispatch belongs to its selected attempt");
        row.verified = None;
        if revision.value() < self.revision.value() || revision.value() < query.revision.value() {
            row.problem = Some("Stale protected read; refresh required".into());
            return Ok(());
        }
        if let Some(view) = &view
            && let Err(problem) = validate_readback(query, view)
        {
            row.problem = Some(problem.clone());
            return Err(problem);
        }
        row.view = view;
        row.revision = Some(revision);
        row.verified = Some(self.generation);
        row.problem = None;
        assert!(self.pending.is_none());
        assert_eq!(row.verified, Some(self.generation));
        Ok(())
    }

    pub fn apply(&mut self, event: Event) -> Option<String> {
        if !self.matches(&event.query) {
            return None;
        }
        match event.result {
            Ok(reply) => match reply.data {
                OwnerData::WorkerDispatch { dispatch } => self
                    .complete(&event.query, reply.revision, dispatch.map(|view| *view))
                    .err(),
                OwnerData::ResyncRequired => {
                    self.fail(
                        &event.query,
                        "Protected dispatch read requires resynchronization",
                    );
                    None
                }
                _ => self.fail(&event.query, "Unexpected protected dispatch readback"),
            },
            Err(problem) => self.fail(&event.query, &problem),
        }
    }

    pub fn lines(&self) -> String {
        let mut lines = vec!["Worker dispatch readback".to_owned()];
        if !self.online {
            lines.push(
                "Dispatch unavailable: owner disconnected; limited mode. Ctrl-R reconnects.".into(),
            );
        }
        let Some(selected) = &self.selection else {
            lines.push("No current selected task plan; no dispatch status is inferred.".into());
            return lines.join("\n");
        };
        lines.push(format!(
            "Task: {} | Plan: {}",
            selected.task.task_id.as_str(),
            selected.plan_id.as_str()
        ));
        if self.rows.is_empty() {
            lines.push("No reserved worker attempt in the selected task plan.".into());
        }
        for row in &self.rows {
            let current = self.online
                && row.verified == Some(self.generation)
                && row
                    .revision
                    .as_ref()
                    .is_some_and(|revision| revision.value() >= self.revision.value());
            lines.push(format!(
                "Item: {} | reservation {} | {}",
                row.attempt.item_id.as_str(),
                row.attempt.reservation_cursor.value(),
                if current { "Current" } else { "STALE" }
            ));
            let status = if let Some(view) = &row.view {
                status_label(&view.state)
            } else if current {
                "Not found: no recorded dispatch at this owner revision"
            } else {
                "Uncertain: no current protected read"
            };
            lines.push(status.into());
            lines.push(format!("Dispatch: {}", row.id.as_str()));
            if let Some(revision) = &row.revision {
                lines.push(format!(
                    "Last protected read: owner revision {}",
                    revision.value()
                ));
            }
            if let Some(view) = &row.view {
                lines.push(format!("Delivery generation: {}", view.delivery_generation));
                if view.cancellation == DispatchCancellation::Requested {
                    lines.push("Cancellation requested; completion is not confirmed".into());
                }
                if let Some(receipt) = &view.receipt {
                    lines.push(format!(
                        "Retained denial: native_runtime_admission_unavailable | journal {}",
                        receipt.journal_sequence
                    ));
                }
            }
            if let Some(problem) = &row.problem {
                lines.push(format!("Uncertain readback: {problem}"));
            }
        }
        lines.push("No provider or native execution is confirmed by this readback.".into());
        lines.join("\n")
    }

    fn assert_consistent(&self) {
        assert!(self.rows.len() <= 16);
        assert!(self.selection.is_some() || self.rows.is_empty());
        assert!(self.rows.is_empty() || self.next < self.rows.len());
        assert!(self.pending.as_ref().is_none_or(|query| {
            query.generation == self.generation
                && self
                    .rows
                    .iter()
                    .any(|row| row.id == query.dispatch_id && row.attempt == query.attempt)
        }));
    }
}

fn validate_readback(query: &Query, view: &DispatchView) -> Result<(), String> {
    if view.dispatch_id != query.dispatch_id || view.attempt != query.attempt {
        return Err("Dispatch readback does not match the selected attempt".into());
    }
    if view.state == DispatchState::DeliveredDenied {
        let valid = view.receipt.as_ref().is_some_and(|receipt| {
            receipt.dispatch_id == view.dispatch_id
                && receipt.attempt == view.attempt
                && receipt.database_instance == query.selection.database
                && receipt.delivery_generation == view.delivery_generation
                && Some(&receipt.offer_digest) == view.offer_digest.as_ref()
                && receipt.controller_generation > 0
                && receipt.journal_sequence > 0
                && receipt.native_identity.is_none()
        });
        if !valid {
            return Err(
                "Delivered denial lacks a matching retained receipt; outcome uncertain".into(),
            );
        }
    }
    assert_eq!(view.dispatch_id, query.dispatch_id);
    assert_eq!(view.attempt, query.attempt);
    Ok(())
}

fn status_label(state: &DispatchState) -> &'static str {
    match state {
        DispatchState::Queued => "Queued: committed outbox; delivery is not confirmed",
        DispatchState::RunnerUnavailable => {
            "Unavailable: runner is not ready; delivery is not confirmed"
        }
        DispatchState::Offered => "Uncertain: offer sent; retained consumption receipt pending",
        DispatchState::OutcomeUnknown => "Uncertain: delivery outcome unknown",
        DispatchState::ReconciliationRequired => "Uncertain: reconciliation required",
        DispatchState::NotConsumed => {
            "Not consumed: reconciliation recorded; no execution confirmed"
        }
        DispatchState::BlockedRecovery => {
            "Unavailable: recovery blocked; resources may remain held"
        }
        DispatchState::CancelledBeforeOffer => "Cancelled before offer; no delivery confirmed",
        DispatchState::DeliveredDenied => "Delivered, denied: native runtime admission unavailable",
    }
}

#[cfg(test)]
mod tests;
