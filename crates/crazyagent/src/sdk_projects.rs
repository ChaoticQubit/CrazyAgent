use crate::engine::EngineError;
use crate::engine::owner::OwnerSummary;
use crate::protocol::{FRAME_BYTES_MAX, RequestId, StoredProject};
use rusqlite::{Transaction, params};
use sdk::generated::project_rows::{ProjectRow, RowsPage};

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

#[cfg(test)]
mod tests;
