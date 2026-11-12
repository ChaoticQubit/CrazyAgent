use super::settings::problem;
use super::*;
use crate::engine::EngineError;
use crate::harness_settings::{KEYS_MAX, Key, Setting, Written};
use serde_json::Value;

const PAGES_MAX: usize = KEYS_MAX;

/// One harness settings request a harness page sends: read the whole catalog and every
/// value at one scope, or set or reset one key naming the version the owner read. No
/// project means the app-wide values. Runs on the owner worker as a Settings job.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) enum HarnessSettingsQuery {
    Read {
        harness: String,
        project: Option<ProjectId>,
    },
    Set {
        harness: String,
        project: Option<ProjectId>,
        key: String,
        value: Value,
        version: u64,
    },
    Reset {
        harness: String,
        project: Option<ProjectId>,
        key: String,
        version: u64,
    },
}

/// Every catalog key and every value of one harness at one scope, joined from all pages:
/// the harness's name, the installed version, whether the catalog is qualified for it and
/// the versions it is qualified for.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) struct HarnessRead {
    pub name: String,
    pub installed: Option<String>,
    pub qualified: bool,
    pub qualified_versions: Vec<String>,
    pub keys: Vec<Key>,
    pub settings: Vec<Setting>,
}

impl HarnessRead {
    /// Finds the catalog key of one row. Input: the setting key. Output: the catalog
    /// key, or none for a stored key the catalog no longer lists.
    pub(in crate::tui) fn key(&self, name: &str) -> Option<&Key> {
        self.keys.iter().find(|key| key.key == name)
    }
}

/// What the engine answered a harness settings request with: the joined read, or what a
/// set or reset did.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) enum HarnessSettingsReply {
    Read(HarnessRead),
    Written(Written),
}

/// One harness settings reply for the screens: the query it answers and the engine's
/// answer, or a bounded plain code.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::tui) struct HarnessSettingsEvent {
    pub query: HarnessSettingsQuery,
    pub result: Result<HarnessSettingsReply, String>,
}

impl HarnessSettingsEvent {
    /// The reply for a request that reached no trusted connection. Takes the query.
    /// Returns the event for that query.
    pub(in crate::tui) fn unavailable(query: HarnessSettingsQuery) -> Self {
        Self {
            query,
            result: Err("trusted_connection_unavailable".into()),
        }
    }
}

impl NativeOwner {
    /// Runs one harness settings request through the trusted owner controller. Inputs:
    /// the query and the lost-connection flag. Output: the event; a transport failure
    /// sets the flag so the shell reconnects.
    pub(super) async fn harness_settings(
        &mut self,
        query: HarnessSettingsQuery,
        lost: &mut bool,
    ) -> HarnessSettingsEvent {
        let result = match self.settings_controller() {
            Ok(controller) => run(controller, &query)
                .await
                .map_err(|error| problem(&error, lost)),
            Err(code) => Err(code),
        };
        HarnessSettingsEvent { query, result }
    }
}

async fn run(
    controller: &OwnerController,
    query: &HarnessSettingsQuery,
) -> Result<HarnessSettingsReply, EngineError> {
    match query {
        HarnessSettingsQuery::Read { harness, project } => {
            read(controller, harness, project.as_ref())
                .await
                .map(HarnessSettingsReply::Read)
        }
        HarnessSettingsQuery::Set {
            harness,
            project,
            key,
            value,
            version,
        } => controller
            .harness_settings_set(harness, project.clone(), key, value.clone(), *version)
            .await
            .map(HarnessSettingsReply::Written),
        HarnessSettingsQuery::Reset {
            harness,
            project,
            key,
            version,
        } => controller
            .harness_settings_reset(harness, project.clone(), key, *version)
            .await
            .map(HarnessSettingsReply::Written),
    }
}

/// The next page start after one page, refusing a `next` that does not move forward so a
/// wrong reply cannot loop. Inputs: the start just read and the page's `next`. Output:
/// the next start, none after the last page, or `harness_settings_page_invalid`.
fn advance(after: Option<u32>, next: Option<u32>) -> Result<Option<u32>, EngineError> {
    match next {
        Some(next) if after.is_none_or(|start| next > start) => Ok(Some(next)),
        Some(_) => Err(EngineError("harness_settings_page_invalid".into())),
        None => Ok(None),
    }
}

/// Reads every catalog page, then every values page at the scope, at most `PAGES_MAX`
/// of each. Inputs: the controller, the harness and the project, none for app-wide.
/// Output: the joined read, or the first refusal.
async fn read(
    controller: &OwnerController,
    harness: &str,
    project: Option<&ProjectId>,
) -> Result<HarnessRead, EngineError> {
    let mut joined = HarnessRead {
        name: String::new(),
        installed: None,
        qualified: false,
        qualified_versions: Vec::new(),
        keys: Vec::new(),
        settings: Vec::new(),
    };
    let mut after = None;
    let mut pages = 0;
    loop {
        if pages == PAGES_MAX {
            return Err(EngineError("harness_settings_pages_unbounded".into()));
        }
        let page = controller.harness_settings_catalog(harness, after).await?;
        pages += 1;
        joined.name = page.name;
        joined.installed = page.installed;
        joined.qualified = page.qualified;
        joined.qualified_versions = page.qualified_versions;
        joined.keys.extend(page.keys);
        match advance(after, page.next)? {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let mut after = None;
    let mut pages = 0;
    loop {
        if pages == PAGES_MAX {
            return Err(EngineError("harness_settings_pages_unbounded".into()));
        }
        let page = controller
            .harness_settings_values(harness, project.cloned(), after)
            .await?;
        pages += 1;
        joined.settings.extend(page.settings);
        match advance(after, page.next)? {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_start_that_does_not_move_forward_is_refused() {
        assert_eq!(advance(None, None).unwrap(), None);
        assert_eq!(advance(None, Some(0)).unwrap(), Some(0));
        assert_eq!(advance(None, Some(4)).unwrap(), Some(4));
        assert_eq!(advance(Some(4), Some(9)).unwrap(), Some(9));
        assert_eq!(advance(Some(9), None).unwrap(), None);
        for (after, next) in [(Some(4), Some(4)), (Some(9), Some(3)), (Some(0), Some(0))] {
            assert_eq!(
                advance(after, next).unwrap_err().0,
                "harness_settings_page_invalid",
                "{after:?} {next:?}"
            );
        }
    }
}
