use crate::coverage;
use crate::criteria;
use crate::host::{
    self, Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links, Paths,
    TicketView,
};
use crate::model::{Actor, CheckKind, EvidenceResult, OpKind, ProjectKey, Refusal, codes};
use crate::policy::{self, Role};
use crate::store::StoreError;
use crate::text::{self, TextField};
use rusqlite::Transaction;
use std::path::{Path, PathBuf};

/// The state every standalone link shows: the tracker keeps the reference as given and
/// cannot look at its target.
pub const LINK_STATE_UNKNOWN: &str = "unknown";

/// The host a tracker runs with on its own: links are any bounded reference with state
/// `unknown`, a `test` check naming a case follows that case's runs, any other check follows
/// the latest evidence a user or CI recorded for it, manual and ci evidence come from the
/// owner or an engine only, export and import stay inside the folder that holds the tracker
/// database, and authority is the project's policy applied to the self-declared actor, so it
/// binds honest agents only.
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
        if check.kind == CheckKind::Test
            && let Some(state) = coverage::test_check(tx, self, project, check)?
        {
            return Ok(state);
        }
        let latest = criteria::latest_evidence(tx, check).map_err(refusal_of)?;
        let state = match latest {
            None => EvidenceState {
                satisfied: false,
                detail: "no evidence recorded".to_owned(),
            },
            Some(recorded) => EvidenceState {
                satisfied: recorded.result == EvidenceResult::Pass,
                detail: if recorded.reference.is_empty() {
                    format!("{} recorded by {}", recorded.result, recorded.recorded_by)
                } else {
                    format!(
                        "{} recorded by {}: {}",
                        recorded.result, recorded.recorded_by, recorded.reference
                    )
                },
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

    fn evidence_may(
        &self,
        _tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        check: &CheckView,
    ) -> Result<(), Refusal> {
        assert_eq!(
            check.ticket.key(),
            project,
            "a check belongs to its project"
        );
        let witnessed = matches!(check.kind, CheckKind::Manual | CheckKind::Ci);
        if witnessed && policy::role(actor) == Role::Agent {
            return Err(Refusal::new(
                codes::EVIDENCE_NOT_FOR_AGENTS,
                format!(
                    "only the owner or an engine records {} evidence, not {actor}",
                    check.kind
                ),
            ));
        }
        Ok(())
    }
}

impl Paths for Standalone {
    fn resolve_path(
        &self,
        tx: &Transaction<'_>,
        _project: &ProjectKey,
        _actor: &Actor,
        op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal> {
        let code = match op {
            OpKind::Export => codes::EXPORT_PATH_REFUSED,
            OpKind::ImportPreview | OpKind::ImportApply => codes::IMPORT_PATH_REFUSED,
            other => panic!("{other} opens no file"),
        };
        let root = tx
            .path()
            .filter(|database| !database.is_empty())
            .and_then(|database| Path::new(database).parent())
            .ok_or_else(|| Refusal::new(code, "this tracker has no folder to hold files"))?;
        let path = host::confined(root, requested).map_err(|why| {
            Refusal::new(
                code,
                format!("{why}; name a .md file inside the tracker folder"),
            )
        })?;
        assert!(path.starts_with(root));
        Ok(path)
    }
}

/// The refusal a host gives for a store error: the core's own refusal as it is, any other
/// failure as `store_failed`. Input: the error. Output: the refusal.
pub(crate) fn refusal_of(error: StoreError) -> Refusal {
    match error {
        StoreError::Refused(refusal) => refusal,
        other => Refusal::new(codes::STORE_FAILED, other.to_string()),
    }
}
