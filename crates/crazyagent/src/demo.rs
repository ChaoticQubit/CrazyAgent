use crate::protocol::{CredentialRef, ProjectId};
use serde::{Deserialize, Serialize};

/// The one id a demo project has (DEC-153): clients send it with CreateDemo and the engine
/// refuses any other, so at most one demo exists and a client never invents its id.
pub const PROJECT_ID: &str = "crazyagent-demo";

/// The demo project's name, which also names its folder in the projects folder.
pub const NAME: &str = "CrazyAgent demo";

/// The goal the guide gives the demo's master, word for word on desktop and in the TUI: add
/// a dark mode switch and a test to the seed web page, with nothing installed or downloaded,
/// and use the owner's answer to the planted question for the starting theme.
pub const GOAL: &str = "This is the CrazyAgent demo: a small web page for a bakery (index.html, style.css, app.js, test/app.test.js). Add a dark mode switch to the page: a button in the header that switches between light and dark colors and remembers the choice. Add a test for it beside test/app.test.js and keep every test passing with node --test. Use plain HTML, CSS and JavaScript only: add no dependencies, no package files and no downloads. Hand the work to one team agent. The page starts in the theme I answer to the demo question on the Questions tab; until I answer, it starts light.";

/// The question the engine plants in the demo when it is made, so the guide's question step
/// never waits on the model to ask one.
pub const QUESTION: &str = "Which theme should the bakery page start in?";

/// The answers the planted question offers, the recommended one first.
pub const QUESTION_OPTIONS: [&str; 3] = ["Follow the system setting", "Light", "Dark"];

/// Every refusal CreateDemo answers with besides the New Project codes it passes through
/// (projects_home_unset, home_new_project_failed, home_new_project_interrupted).
pub const CODES: [&str; 8] = [
    "demo_exists",
    "demo_name_taken",
    "demo_project_id_invalid",
    "demo_pool_missing",
    "demo_pool_not_ready",
    "demo_harness_unavailable",
    "demo_platform_unqualified",
    "demo_seed_failed",
];

/// Where the demo stands, read from live rows only (counts, never content): each step is
/// true once its row exists in the demo project. All false while there is no demo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoSteps {
    pub created: bool,
    pub goal_sent: bool,
    pub agent_started: bool,
    pub question_answered: bool,
    pub change_set_accepted: bool,
}

/// One saved pool as the demo dialog offers it: none when a demo can run on it, else the
/// code that says why not (`demo_pool_not_ready`, `demo_harness_unavailable`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoPool {
    pub pool: CredentialRef,
    pub unavailable: Option<String>,
}

/// The ProjectDemo read: the demo project when one exists (its id, name and when it was
/// made), its steps, the goal words and every saved pool with whether a demo can run on it.
/// Without a demo the id and time are none and every step is false.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoView {
    pub project_id: Option<ProjectId>,
    pub name: String,
    pub created_ms: Option<u64>,
    pub steps: DemoSteps,
    pub goal: String,
    pub pools: Vec<DemoPool>,
}
