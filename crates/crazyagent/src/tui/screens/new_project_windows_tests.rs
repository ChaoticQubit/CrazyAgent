use super::new_project::*;
use super::project_access::next_level;
use crate::sandbox::policy::AgentAccess;

/// On Windows, where agents run Sandboxed alone in this version, a new project preselects
/// Sandboxed in place of the Own branch default, a level that runs there, and the access
/// choice offers no other level: Own branch and Full access show why they cannot run.
#[test]
fn on_windows_a_new_project_preselects_sandboxed_and_offers_no_wider_level() {
    assert_eq!(AgentAccess::NEW_PROJECT, AgentAccess::OwnBranch);
    let wizard = Wizard::new(Vec::new(), None);
    assert_eq!(wizard.access, AgentAccess::Sandboxed);
    assert_eq!(wizard.access.unavailable(), None);
    for forward in [true, false] {
        assert_eq!(
            next_level(AgentAccess::Sandboxed, forward),
            AgentAccess::Sandboxed
        );
    }
    assert_eq!(
        AgentAccess::OwnBranch.unavailable(),
        Some("On Windows, agents run Sandboxed in this version.")
    );
    assert_eq!(
        AgentAccess::Full.unavailable(),
        Some("Full access isn't available on Windows in this version.")
    );
}
