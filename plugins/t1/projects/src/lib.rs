#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use sdk::Refusal;
use sdk::generated::project_rows::{self, RowsPage};

/// The page sizes the list takes, the engine's owner page bound.
const LIMITS: core::ops::RangeInclusive<u16> = 1..=32;

/// Lists one page of projects in id order through the SDK. Inputs: the host the kernel
/// bound to this plugin, the id the page starts after (none for the first page) and the
/// page size, 1 to 32. Output: the page, `owner_page_limit` for another size, or the bus
/// refusal.
pub fn list(host: &dyn sdk::Host, after: Option<String>, limit: u16) -> Result<RowsPage, Refusal> {
    if !LIMITS.contains(&limit) {
        return Err(Refusal::Rejected {
            code: "owner_page_limit".into(),
        });
    }
    let page = project_rows::page(host, after, limit)?;
    assert!(page.rows.len() <= usize::from(limit));
    Ok(page)
}
