use super::{PROJECTS_PLUGIN, RowsProvider, rows};
use crate::generated_served::served::served;
use crate::protocol::{ProjectId, ProjectSpec, StoredProject};
use crate::sdk_slice::EngineRefusals;
use bus_gateway::{AllOn, Gateway, Grants, Switches};
use kernel_plugin_host::generated::permissions::{BUNDLED_GRANTS, PERMISSIONS};
use kernel_plugin_host::permission::{BundledGrant, Tier};
use rusqlite::{Connection, Transaction};
use sdk::Refusal;
use sdk::generated::project_rows;
use sdk::generated::projects::{self, ProjectPage, ProjectView};
use sdk_host::{Bound, Providers};

/// The test plugin that holds `projects.read`.
const READER: &str = "ctx";

/// The test plugin that holds no permission.
const BARE: &str = "bare";

/// The grants the rows read under: the bundled projects plugin as it ships, a reader
/// holding `projects.read` and a plugin holding nothing.
const TABLE: [BundledGrant; 3] = [
    BundledGrant {
        plugin: BARE,
        tier: Tier::T1,
        permissions: &[],
        essential: false,
        capabilities: &[],
    },
    BundledGrant {
        plugin: READER,
        tier: Tier::T1,
        permissions: &["projects.read"],
        essential: false,
        capabilities: &[],
    },
    BundledGrant {
        plugin: PROJECTS_PLUGIN,
        tier: Tier::T1,
        permissions: &["project_rows.read"],
        essential: true,
        capabilities: &["projects"],
    },
];

/// An in-memory store with the engine's schema and the given projects, each with two
/// repositories. Input: the project ids. Output: the connection.
fn store(ids: &[&str]) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::state_log::fence::seed(&db);
    for id in ids {
        let body = serde_json::to_string(&StoredProject {
            project_id: ProjectId::new(*id).unwrap(),
            spec: ProjectSpec {
                name: format!("Project {id}"),
                repositories: vec!["/r/0".to_owned(), "/r/1".to_owned()],
            },
        })
        .unwrap();
        db.execute(
            "INSERT INTO projects(id, body) VALUES (?1, ?2)",
            rusqlite::params![id, body],
        )
        .unwrap();
    }
    db
}

/// Switches with only the projects plugin turned off.
struct ProjectsOff;

impl Switches for ProjectsOff {
    fn is_on(&self, plugin: &str) -> bool {
        plugin != PROJECTS_PLUGIN
    }
}

/// Calls `projects.list` as one test plugin through the real gateway, the generated
/// serving host and the engine's `served()`. Inputs: the transaction, the `project_rows`
/// provider the call site binds, the switches, the calling plugin, the id to read past
/// and the page size. Output: the served page or the refusal.
fn list(
    tx: &Transaction<'_>,
    rows: &dyn project_rows::Provide,
    switches: &dyn Switches,
    caller: &'static str,
    after: Option<String>,
    limit: u16,
) -> Result<ProjectPage, Refusal> {
    let grants = Grants::from_table(&TABLE, &PERMISSIONS).expect("the test grants build");
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(&grants, &refusals, switches);
    let editor = crate::sdk_editor::EditorCaps::unregistered(tx);
    let providers = Providers {
        editor: &editor,
        net: &crate::security_caps::Absent,
        process: &crate::sdk_install::InstallCaps::UNREGISTERED,
        project_rows: rows,
        sandbox: &crate::security_caps::Absent,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let served = served();
    let host = Bound::serving(gateway.bind(caller), &providers, &served);
    projects::list(&host, after, limit)
}

/// A plugin holding `projects.read` gets, through the projects plugin, the same page the
/// direct read gives, field by field, and the projects plugin reads it once.
#[test]
fn a_plugin_holding_projects_read_gets_the_owner_page() {
    let mut db = store(&["alpha", "beta", "gamma"]);
    let tx = db.transaction().unwrap();
    let direct = rows(&tx, "", 2).unwrap().page;
    let provider = RowsProvider::new(&tx, "", 2);
    let page = list(&tx, &provider, &AllOn, READER, None, 2).unwrap();
    let expected: Vec<ProjectView> = direct
        .rows
        .iter()
        .map(|row| ProjectView {
            id: row.id.clone(),
            name: row.name.clone(),
            repository_count: row.repository_count,
        })
        .collect();
    assert_eq!(expected.len(), 2);
    assert_eq!(expected[0].repository_count, 2);
    assert_eq!(
        page,
        ProjectPage {
            items: expected,
            more: direct.more,
        }
    );
    assert!(page.more);
    assert_eq!(provider.calls.get(), 1);
}

/// A plugin holding no `projects.read` is refused at the gateway and reaches no provider.
#[test]
fn a_plugin_without_projects_read_is_not_granted() {
    let mut db = store(&["alpha"]);
    let tx = db.transaction().unwrap();
    let provider = RowsProvider::new(&tx, "", 2);
    let refused = list(&tx, &provider, &AllOn, BARE, None, 2);
    assert_eq!(
        refused,
        Err(Refusal::NotGranted {
            capability: "projects.list",
            needs: "projects.read",
        })
    );
    assert_eq!(provider.calls.get(), 0);
}

/// With the projects plugin turned off, the served list is refused before the plugin runs.
#[test]
fn a_turned_off_projects_plugin_refuses_the_served_list() {
    let mut db = store(&["alpha"]);
    let tx = db.transaction().unwrap();
    let provider = RowsProvider::new(&tx, "", 2);
    let refused = list(&tx, &provider, &ProjectsOff, READER, None, 2);
    assert_eq!(
        refused,
        Err(Refusal::Failed {
            code: "capability_provider_off".to_owned(),
        })
    );
    assert_eq!(provider.calls.get(), 0);
}

/// The served list refuses a page size outside 1 to 32 with the 2.4.4 code, before any read.
#[test]
fn the_served_list_refuses_a_bad_limit_like_2_4_4() {
    let mut db = store(&["alpha"]);
    let tx = db.transaction().unwrap();
    for limit in [0, 33] {
        let provider = RowsProvider::new(&tx, "", limit);
        let refused = list(&tx, &provider, &AllOn, READER, None, limit);
        assert_eq!(
            refused,
            Err(Refusal::Rejected {
                code: "owner_page_limit".to_owned(),
            })
        );
        assert_eq!(provider.calls.get(), 0);
    }
}

/// The projects plugin alone serves the `projects` stem, and no bundled plugin holds
/// `projects.read` yet.
#[test]
fn only_the_projects_plugin_serves_projects() {
    let servers: Vec<&str> = BUNDLED_GRANTS
        .iter()
        .filter(|grant| grant.capabilities.contains(&"projects"))
        .map(|grant| grant.plugin)
        .collect();
    assert_eq!(servers, [PROJECTS_PLUGIN]);
    let readers: Vec<&str> = BUNDLED_GRANTS
        .iter()
        .filter(|grant| grant.permissions.contains(&"projects.read"))
        .map(|grant| grant.plugin)
        .collect();
    assert_eq!(readers, Vec::<&str>::new());
}

/// A call site that binds no rows provider gives the served list `cap_provider_absent` as
/// a value: never a panic and never an empty page.
#[test]
fn a_served_list_with_no_bound_rows_provider_is_cap_provider_absent() {
    let mut db = store(&["alpha", "beta"]);
    let tx = db.transaction().unwrap();
    let refused = list(&tx, &crate::security_caps::Absent, &AllOn, READER, None, 2);
    assert_eq!(
        refused,
        Err(Refusal::Failed {
            code: "cap_provider_absent".to_owned(),
        })
    );
}

/// The engine's served wiring names the projects plugin's own static.
#[test]
fn served_wires_the_projects_static() {
    let wired = served().projects as *const dyn projects::Serve as *const ();
    let built = &crazyagent_plugin_projects::PROJECTS as *const _ as *const ();
    assert!(core::ptr::eq(wired, built));
}
