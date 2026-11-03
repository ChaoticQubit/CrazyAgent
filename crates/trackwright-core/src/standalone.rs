use crate::criteria;
use crate::host::{
    Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links, TicketView,
};
use crate::model::{Actor, EvidenceResult, OpKind, ProjectKey, Refusal, codes};
use crate::policy;
use crate::store::StoreError;
use crate::text::{self, TextField};
use rusqlite::Transaction;

/// The state every standalone link shows: the tracker keeps the reference as given and
/// cannot look at its target.
pub const LINK_STATE_UNKNOWN: &str = "unknown";

/// The host a tracker runs with on its own: links are any bounded reference with state
/// `unknown`, a check follows the latest evidence a user or CI recorded for it, and
/// authority is the project's policy applied to the self-declared actor, so it binds honest
/// agents only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Standalone;

impl Links for Standalone {
    fn kinds(&self) -> &[LinkKind] {
        &[]
    }

    fn resolve(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        text::check(TextField::LinkKind, kind)?;
        text::check(TextField::LinkTarget, target)?;
        Ok(LinkState {
            live: true,
            label: target.to_owned(),
            state: LINK_STATE_UNKNOWN.to_owned(),
        })
    }
}

impl Evidence for Standalone {
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        assert_eq!(
            check.ticket.key(),
            project,
            "a check belongs to its project"
        );
        let latest = criteria::latest_evidence(tx, check).map_err(refusal_of)?;
        let state = match latest {
            None => EvidenceState {
                satisfied: false,
                detail: "no evidence recorded".to_owned(),
            },
            Some(recorded) => EvidenceState {
                satisfied: recorded.result == EvidenceResult::Pass,
                detail: format!(
                    "{} recorded by {}: {}",
                    recorded.result, recorded.recorded_by, recorded.reference
                ),
            },
        };
        assert!(!state.detail.is_empty());
        Ok(state)
    }
}

impl Authority for Standalone {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        assert!(ticket.is_none_or(|view| view.id.key() == project));
        let policy = policy::load(tx, project).map_err(refusal_of)?;
        policy::permits(&policy, actor, op, ticket)
    }

    fn actors_verified(&self) -> bool {
        false
    }
}

fn refusal_of(error: StoreError) -> Refusal {
    match error {
        StoreError::Refused(refusal) => refusal,
        other => Refusal::new(codes::STORE_FAILED, other.to_string()),
    }
}
