#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use sdk::Refusal;
use sdk::generated::project_rows::{self, RowsPage};
use sdk::generated::projects::{ProjectPage, ProjectView, Serve};

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

/// The projects plugin's server for `projects.list`: it holds no state; every read goes
/// through the host the gateway bound to the projects plugin.
pub struct Projects;

/// The served provider the engine's generated `served()` names for the `projects` stem.
pub static PROJECTS: Projects = Projects;

impl Serve for Projects {
    /// Serves one page of projects to a calling plugin, read through `project_rows.page`
    /// with the host the gateway bound to the projects plugin. Inputs: that host, the id
    /// the page starts after and the page size, 1 to 32. Output: the page, or the refusal.
    fn list(
        &self,
        host: &dyn sdk::Host,
        after: Option<String>,
        limit: u16,
    ) -> Result<ProjectPage, Refusal> {
        let page = list(host, after, limit)?;
        let items: alloc::vec::Vec<ProjectView> = page
            .rows
            .into_iter()
            .map(|row| ProjectView {
                id: row.id,
                name: row.name,
                repository_count: row.repository_count,
            })
            .collect();
        assert!(items.len() <= usize::from(limit));
        Ok(ProjectPage {
            items,
            more: page.more,
        })
    }
}
