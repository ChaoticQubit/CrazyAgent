use super::host::state_of;
use super::{engine_error, owner, project_of};
use crate::engine::EngineError;
use rusqlite::{Connection, Transaction};
use std::path::{Path, PathBuf};
use trackwright_core::export;
use trackwright_core::host::{self as core_host, Paths};
use trackwright_core::model::{Actor, OpKind, ProjectKey, Refusal, codes};

/// The rendered view's name inside a project's `.crazyagent/` folder.
pub const VIEW_FILE: &str = "tickets.md";

/// Where the rendered view goes: only `tickets.md` in the project's own `.crazyagent/`
/// folder, which was checked to be a real folder in the same transaction.
struct View {
    state: PathBuf,
}

impl Paths for View {
    /// Resolves the view's one file. Inputs: the transaction, key and actor (unused: the
    /// engine writes the view itself), the operation, always `export`, and the name, always
    /// `tickets.md`. Output: the path, or `export_path_refused`.
    fn resolve_path(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _actor: &Actor,
        op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal> {
        assert_eq!(op, OpKind::Export);
        assert_eq!(requested, Path::new(VIEW_FILE));
        let path = core_host::confined(&self.state, requested)
            .map_err(|why| Refusal::new(codes::EXPORT_PATH_REFUSED, why))?;
        assert_eq!(path.parent(), Some(self.state.as_path()));
        Ok(path)
    }
}

/// Rewrites a project's `.crazyagent/tickets.md` from the core export after a tracker
/// change. It is best effort: the change is already committed, so a failure is logged and
/// leaves the old view. The core refuses a link at the name, a non-export file there and a
/// linked folder, so the view never follows a link. Inputs: the engine connection, outside
/// a transaction, and the project key. Output: none.
pub(crate) fn refresh(db: &Connection, key: &ProjectKey) {
    assert!(db.is_autocommit());
    if let Err(problem) = rewrite(db, key) {
        eprintln!(
            "crazyagent: the ticket view of {} was not rewritten: {}",
            key.as_str(),
            problem.0
        );
    }
    assert!(db.is_autocommit());
}

/// Reads the export and writes it, in one read transaction that ends unchanged. Inputs:
/// the connection and the key. Output: Ok, or the code that stopped it.
fn rewrite(db: &Connection, key: &ProjectKey) -> Result<(), EngineError> {
    let tx = db
        .unchecked_transaction()
        .map_err(|_| EngineError(super::TRACKER_UNAVAILABLE.into()))?;
    let engine = project_of(&tx, key).map_err(|refusal| EngineError(refusal.code.into()))?;
    let state = state_of(&tx, &engine, codes::EXPORT_PATH_REFUSED)
        .map_err(|refusal| EngineError(refusal.code.into()))?;
    let text = export::project(&tx, key).map_err(engine_error)?;
    assert!(text.starts_with(export::EXPORT_HEADER));
    let view = View { state };
    export::write(&tx, &view, &owner(), key, Path::new(VIEW_FILE), &text).map_err(engine_error)
}
