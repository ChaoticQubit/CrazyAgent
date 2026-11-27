use std::cell::{Cell, RefCell};

use crate::engine::EngineError;
use crate::engine::owner::OwnerSummary;
use crate::protocol::{FRAME_BYTES_MAX, RequestId, StoredProject};
use crate::sdk_slice::{CallRecord, EngineRefusals};
use bus_gateway::{Gateway, Grants, Switches};
use crazyagent_plugin_projects as projects;
use rusqlite::{Transaction, params};
use sdk::Refusal;
use sdk::generated::project_rows::{self, ProjectRow, RowsPage};
use sdk_host::{Bound, Providers};

/// The bundled plugin that serves the projects list, the one caller `project_rows.page`
/// answers.
pub(crate) const PROJECTS_PLUGIN: &str = "projects";

/// The refusal a `project_rows.page` call from any plugin but the projects plugin gets.
const CALLER_REFUSED: &str = "project_rows_caller";

/// The projects page read: one page of rows past an id, each id and body bounded in SQL so
/// an oversized row reads as NULL.
const SQL: &str = "SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(CAST(body AS BLOB))<=65536 THEN body END FROM projects WHERE id>?1 ORDER BY id LIMIT ?2";

/// The most rows one page holds.
const LIMIT_MAX: u16 = 32;

/// The longest project id: `validate_identity` refuses longer ones and the SQL bounds it.
const ID_BYTES_MAX: usize = 128;

/// The longest project name a stored row may carry.
const NAME_BYTES_MAX: usize = 256;

/// The most repositories a stored row may carry.
const REPOSITORIES_MAX: usize = 64;

/// The most bytes JSON writes for one name byte: a control escaped as `\u00XX`.
const ESCAPED_BYTE_MAX: usize = 6;

/// The digits of the largest repository count.
const COUNT_DIGITS_MAX: usize = 2;

/// The bytes of one project item's JSON other than its id, name and count.
const ITEM_JSON_FIXED: usize =
    r#"{"kind":"Project","project_id":"","name":"","repository_count":}"#.len();

/// The longest JSON one projects page serializes to: 32 worst items, their commas and the
/// brackets. An id holds only `[A-Za-z0-9-_.:]`, so it is never escaped.
pub(crate) const PAGE_JSON_MAX: usize = 2
    + LIMIT_MAX as usize
        * (ITEM_JSON_FIXED + ID_BYTES_MAX + NAME_BYTES_MAX * ESCAPED_BYTE_MAX + COUNT_DIGITS_MAX)
    + (LIMIT_MAX as usize - 1);

const _: () = assert!(PAGE_JSON_MAX <= FRAME_BYTES_MAX - 4096);
const _: () = assert!(REPOSITORIES_MAX < 10_usize.pow(COUNT_DIGITS_MAX as u32));

/// One projects page as the engine keeps it and as SDK data for a plugin, built from the
/// same parsed rows.
#[derive(Debug)]
pub(crate) struct Kept {
    pub(crate) items: Vec<OwnerSummary>,
    pub(crate) page: RowsPage,
}

/// Maps a failure to the engine error the storage page read returns.
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Reads one projects page in the caller's transaction: rows with an id past `after`, at
/// most `limit` (1 to 32), each bounded, checked and parsed. Input: the transaction, the
/// id to read past ("" for the first page) and the limit. Output: the kept page, or the
/// error the engine's page read gives. A row past the limit is probed only, never read, so
/// `more` holds whether one exists.
pub(crate) fn rows(tx: &Transaction<'_>, after: &str, limit: u16) -> Result<Kept, EngineError> {
    if !(1..=LIMIT_MAX).contains(&limit) {
        return Err(error("owner_page_limit"));
    }
    let mut stmt = tx.prepare(SQL).map_err(error)?;
    let mut found = stmt
        .query(params![after, i64::from(limit) + 1])
        .map_err(error)?;
    let mut items = Vec::new();
    let mut rows = Vec::new();
    let mut more = false;
    while let Some(row) = found.next().map_err(error)? {
        if items.len() == usize::from(limit) {
            more = true;
            break;
        }
        let id: String = row
            .get::<_, Option<String>>(0)
            .map_err(error)?
            .ok_or_else(|| error("owner_row_bounds"))?;
        let body: String = row
            .get::<_, Option<String>>(1)
            .map_err(error)?
            .ok_or_else(|| error("owner_row_bounds"))?;
        RequestId::new(id.clone()).map_err(error)?;
        let p: StoredProject = serde_json::from_str(&body).map_err(error)?;
        if p.project_id.as_str() != id
            || p.spec.name.len() > NAME_BYTES_MAX
            || p.spec.repositories.len() > REPOSITORIES_MAX
        {
            return Err(error("owner_row_invalid"));
        }
        let repository_count = p.spec.repositories.len();
        rows.push(ProjectRow {
            id,
            name: p.spec.name.clone(),
            repository_count: u16::try_from(repository_count)
                .expect("a count of at most 64 fits a u16"),
        });
        items.push(OwnerSummary::Project {
            project_id: p.project_id,
            name: p.spec.name,
            repository_count,
        });
    }
    assert!(items.len() <= usize::from(limit) && rows.len() == items.len());
    Ok(Kept {
        items,
        page: RowsPage { rows, more },
    })
}

/// A bundled projects plugin's list entry: what the plugin crate exports for the engine to
/// call with the host bound to it. Inputs: the host, the id the page starts after and the
/// page size. Output: the plugin's page, which the engine reads as input, never as an
/// invariant.
pub(crate) type ListEntry = fn(&dyn sdk::Host, Option<String>, u16) -> Result<RowsPage, Refusal>;

/// Reads one projects page through the bundled projects plugin, the generated SDK and the
/// bus gateway's switch and grant checks, in the caller's transaction. A refusal is queued
/// in the engine sink but never logged here: reads never drain it, the next owner request
/// does. Inputs: the read transaction, the id to read past ("" for the first page) and the
/// limit. Output: the same page or error as a direct read, or `plugin_call_refused`.
pub(crate) fn page(tx: &Transaction<'_>, after: &str, limit: u16) -> Result<Kept, EngineError> {
    page_with(
        projects::list,
        crate::sdk_slice::bundled_grants(),
        crate::app_plugins::switches(),
        crate::sdk_slice::engine_refusals(),
        tx,
        after,
        limit,
    )
}

/// Reads one projects page through a plugin entry with a given grant table, plugin switches
/// and refusal sink. The plugin's page is input: a refusal the gateway recorded in this call
/// is `plugin_call_refused`; one admitted call whose page equals the rows the provider kept
/// is those rows; a failed call whose code is the kept error is that error; every other
/// shape is `plugin_call_invalid`. Inputs: the entry, the grants, the switches, the sink,
/// the transaction, the id to read past and the limit.
pub(crate) fn page_with(
    entry: ListEntry,
    grants: &Grants,
    switches: &dyn Switches,
    refusals: &EngineRefusals,
    tx: &Transaction<'_>,
    after: &str,
    limit: u16,
) -> Result<Kept, EngineError> {
    let provider = RowsProvider::new(tx);
    let record = CallRecord {
        sink: refusals,
        refused: Cell::new(false),
    };
    let gateway = Gateway::new(grants, &record, switches);
    let editor = crate::sdk_editor::EditorCaps::unregistered(tx);
    let providers = Providers {
        editor: &editor,
        net: &crate::security_caps::Absent,
        process: &crate::sdk_install::InstallCaps::UNREGISTERED,
        project_rows: &provider,
        sandbox: &crate::security_caps::Absent,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let host = Bound::new(gateway.bind(PROJECTS_PLUGIN), &providers);
    let result = entry(&host, (!after.is_empty()).then(|| after.to_owned()), limit);
    if record.refused.get() {
        return Err(EngineError("plugin_call_refused".to_owned()));
    }
    let invalid = || Err(EngineError("plugin_call_invalid".to_owned()));
    if provider.calls.get() != 1 {
        return invalid();
    }
    match (result, provider.kept.take(), provider.error.take()) {
        (Ok(page), Some((items, more)), None) if same(&items, more, &page) => {
            Ok(Kept { items, page })
        }
        (Err(Refusal::Failed { code }), None, Some(error)) if code == error.0 => Err(error),
        _ => invalid(),
    }
}

/// Whether a plugin's page holds exactly the rows the provider kept, field by field and in
/// order. Inputs: the kept items, the kept `more` and the plugin's page. Output: the answer.
fn same(items: &[OwnerSummary], more: bool, page: &RowsPage) -> bool {
    page.more == more
        && items.len() == page.rows.len()
        && items.iter().zip(&page.rows).all(|(item, row)| {
            matches!(item, OwnerSummary::Project { project_id, name, repository_count }
                if project_id.as_str() == row.id
                    && *name == row.name
                    && *repository_count == usize::from(row.repository_count))
        })
}

/// The `project_rows.page` provider for one read: it answers only the projects plugin, runs
/// the engine's page read in the read transaction, counts every call, and keeps the engine
/// items and `more`, or the error, for the caller; the SDK rows move out to the plugin.
struct RowsProvider<'t, 'c> {
    tx: &'t Transaction<'c>,
    calls: Cell<u32>,
    kept: RefCell<Option<(Vec<OwnerSummary>, bool)>>,
    error: RefCell<Option<EngineError>>,
}

impl<'t, 'c> RowsProvider<'t, 'c> {
    /// A provider that has served no call. Input: the read transaction. Output: it.
    fn new(tx: &'t Transaction<'c>) -> Self {
        Self {
            tx,
            calls: Cell::new(0),
            kept: RefCell::new(None),
            error: RefCell::new(None),
        }
    }
}

impl project_rows::Provide for RowsProvider<'_, '_> {
    /// Serves one page to the projects plugin; `None` reads from the first id, as `""`
    /// does. Inputs: the caller, the id to read past and the limit. Output: the SDK rows,
    /// `project_rows_caller` for another caller, or the engine error's code as a failure.
    fn page(
        &self,
        caller: &'static str,
        after: Option<String>,
        limit: u16,
    ) -> Result<RowsPage, Refusal> {
        if caller != PROJECTS_PLUGIN {
            return Err(Refusal::Rejected {
                code: CALLER_REFUSED.to_owned(),
            });
        }
        self.calls.set(self.calls.get().saturating_add(1));
        match rows(self.tx, after.as_deref().unwrap_or(""), limit) {
            Ok(Kept { items, page }) => {
                *self.kept.borrow_mut() = Some((items, page.more));
                Ok(page)
            }
            Err(error) => {
                let code = error.0.clone();
                *self.error.borrow_mut() = Some(error);
                Err(Refusal::Failed { code })
            }
        }
    }
}

#[cfg(test)]
mod tests;
