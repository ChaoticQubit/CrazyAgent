use super::*;
use crate::engine::bootstrap::EngineService;
use crate::questions::{self, QuestionStatus};
use crate::tracker::context_import::{self, Imported};
use crate::tracker::host::{
    ACTOR_NOT_ENGINE, ACTOR_OTHER_PROJECT, EVIDENCE_ENGINE_ONLY, EVIDENCE_OWNER_ONLY, HOST_FAILED,
    LINK_BROKEN, LINK_CROSS_PROJECT, POLICY_IN_SETTINGS, PROJECT_NOT_ATTACHED,
};
use rusqlite::TransactionBehavior;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use trackwright_core::catalog::{self, FieldType, OpSpec, Shape};
use trackwright_core::export::{self, EXPORT_HEADER};
use trackwright_core::model::{ClosePolicy, codes};

const PROJECT: &str = "alpha";
const KEY: &str = "CA";
const OWNER_WAIT_MS: u64 = 5000;
const PLANNED: &str = "# Roadmap\n## V1: first release\nScope:\n- Sign in with a magic link\n\n\
    ## Risks\n- The board may be slow on phones.\n\n# Release process\n\
    ## Before the tag\n- [ ] Run the full suite\n\n# Open questions\n\
    - Which payment provider should V1 use?\n";

fn key() -> ProjectKey {
    ProjectKey::new(KEY).unwrap()
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

/// Makes a project folder with its `.crazyagent/context/` folder under `base` and sets it
/// up as engine project `alpha` in the engine database at `root`. Inputs: the engine root
/// and the base folder. Output: the project folder.
fn project_in(root: &Path, base: &Path) -> PathBuf {
    let folder = base.join("project");
    std::fs::create_dir_all(folder.join(".crazyagent").join("context")).unwrap();
    let db = questions::open(root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::team::initialize(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    folder
}

/// An engine root whose database holds every authority schema and project `alpha`. Output:
/// the temporary folder, the engine root and the project folder.
fn engine() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("ca-owner-tracker-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    std::fs::create_dir(&root).unwrap();
    let folder = project_in(&root, &base);
    (tmp, root, folder)
}

/// Sends one request through this protocol's selector, parsing and handler, as an owner
/// client's frame body. Inputs: the engine root and the body. Output: the reply, or the
/// refusal.
fn send(root: &Path, body: serde_json::Value) -> Result<Reply, EngineError> {
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Tracker));
    run(root, parse(&bytes)?)
}

fn attached(root: &Path) -> TrackedProject {
    let body = json!({"tracker": {"kind": "attach", "project": PROJECT, "key": KEY,
        "name": "Alpha"}});
    match send(root, body).unwrap() {
        Reply::Attached { project } => project,
        other => panic!("expected the attached project, got {other:?}"),
    }
}

fn op(root: &Path, op: &str, args: serde_json::Value) -> serde_json::Value {
    let body = json!({"tracker": {"kind": "op", "op": op, "args": args}});
    match send(root, body).unwrap() {
        Reply::Envelope { envelope } => envelope,
        other => panic!("expected an envelope, got {other:?}"),
    }
}

fn exported(root: &Path) -> String {
    let db = questions::open(root).unwrap();
    let tx = rusqlite::Transaction::new_unchecked(&db, TransactionBehavior::Deferred).unwrap();
    export::project(&tx, &key()).unwrap()
}

/// Builds the smallest arguments of one catalog operation: every required field with a
/// value of its catalog type, naming project `CA` and its first ticket. Input: the
/// operation's spec. Output: the arguments object.
fn sample(spec: &OpSpec) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    for field in spec.fields.iter().filter(|field| field.required) {
        let value = match field.ty {
            FieldType::Project => json!(KEY),
            FieldType::Ticket => json!("CA-1"),
            FieldType::Test(object) => json!(format!("CA-{}1", object.letter())),
            FieldType::Line { .. } => json!("sample"),
            FieldType::Shape(shape) => sample_shape(shape),
            FieldType::Actor => json!("owner"),
            FieldType::Text(_) => json!("Sample text"),
            FieldType::Version => json!(1),
            FieldType::Integer { min, .. } => json!(min),
            FieldType::Choice(named) => json!(named.names()[0]),
            FieldType::Day => json!("2026-12-01"),
            FieldType::Flag => json!(false),
            FieldType::Word => json!("word"),
            FieldType::Path => json!("sample.md"),
            FieldType::Keys => json!(["sample"]),
            FieldType::Cursor | FieldType::RequestId => {
                panic!("{}: {} is never required", spec.kind, field.name)
            }
        };
        args.insert(field.name.to_owned(), value);
    }
    serde_json::Value::Object(args)
}

/// Builds the smallest valid value of one structured catalog field, as the core's own op
/// tests build it. Input: the shape. Output: the value.
fn sample_shape(shape: Shape) -> serde_json::Value {
    match shape {
        Shape::Steps => json!(["Sample step"]),
        Shape::Results => json!([{"name": "sample", "outcome": "passed"}]),
        Shape::Source => json!({"kind": "declared", "ref": "local"}),
        Shape::Policy => json!({
            "profile": "d".repeat(64),
            "profile_approved_ms": 0,
            "check": "unit",
            "kind": "unit",
            "argv": [],
            "toolchain": {"family": "none", "version": ""},
            "sandbox": "e".repeat(64),
            "network": "off",
            "timeout_s": 0,
            "environment": "f".repeat(64),
        }),
        Shape::Integrity => json!({
            "executed": 0,
            "inventory": {"baseline_run": null, "removed": [], "removed_total": 0},
            "new_skips": [],
            "protected_touched": [],
            "holdouts_ran": false,
            "build_ok": true,
        }),
    }
}

struct Served {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Served {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-own-tk-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("version = 1\ntimeout_ms = {OWNER_WAIT_MS}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service,
            _stop: stop,
        }
    }

    async fn client(&self) -> OwnerClient {
        OwnerClient::discover(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(20),
        )
        .await
        .unwrap()
        .0
    }

    async fn stop(mut self) {
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_tracker_requests_round_trip_every_catalog_op() {
    let served = Served::start().await;
    let base = tempfile::Builder::new()
        .prefix("ca-tk-project-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let folder = project_in(served.root.path(), &base.path().canonicalize().unwrap());
    let client = served.client().await;

    let tracked = client
        .tracker_attach(project(), key(), "Alpha".into())
        .await
        .unwrap();
    assert_eq!(
        tracked,
        TrackedProject {
            project: project(),
            key: key(),
            name: "Alpha".into(),
        }
    );
    assert_eq!(client.tracker_projects().await.unwrap(), vec![tracked]);
    let again = client
        .tracker_attach(project(), key(), "Alpha".into())
        .await;
    assert_eq!(again.unwrap_err().0, "project_exists");
    let ghost = ProjectId::new("ghost").unwrap();
    let missing = client
        .tracker_attach(
            ghost.clone(),
            ProjectKey::new("GH").unwrap(),
            "Ghost".into(),
        )
        .await;
    assert_eq!(missing.unwrap_err().0, "tracker_project_missing");

    let created = client
        .tracker_op(OpKind::Create, json!({"project": KEY, "title": "First"}))
        .await
        .unwrap();
    assert_eq!(created["api"], 1);
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["data"]["id"], "CA-1");
    let view = std::fs::read_to_string(folder.join(".crazyagent/tickets.md")).unwrap();
    assert!(view.starts_with(EXPORT_HEADER));
    assert!(view.contains("CA-1: First"), "{view}");

    for spec in catalog::catalog() {
        let envelope = client.tracker_op(spec.kind, sample(spec)).await.unwrap();
        assert_eq!(envelope["api"], 1, "{}", spec.kind);
        match envelope["ok"].as_bool() {
            Some(true) => assert!(envelope["data"].is_object(), "{}: {envelope}", spec.kind),
            Some(false) => {
                let code = envelope["code"].as_str().unwrap();
                assert!(TRACKER_CODES.contains(&code), "{}: {code}", spec.kind);
                assert_ne!(code, codes::OP_UNAVAILABLE, "{} runs", spec.kind);
            }
            None => panic!("{}: no ok in {envelope}", spec.kind),
        }
    }

    let shown = client
        .tracker_op(OpKind::Show, json!({"id": "CA-1"}))
        .await
        .unwrap();
    assert_eq!(shown["data"]["ticket"]["id"], "CA-1", "{shown}");
    let listed = client
        .tracker_op(OpKind::List, json!({"project": KEY}))
        .await
        .unwrap();
    assert_eq!(listed["data"]["tickets"][0]["id"], "CA-1", "{listed}");
    let schema = client.tracker_op(OpKind::Schema, json!({})).await.unwrap();
    assert_eq!(schema["ok"], true, "{schema}");
    let unknown = client
        .tracker_op(OpKind::Show, json!({"id": "CA-99"}))
        .await
        .unwrap();
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["code"], "ticket_unknown");
    let policy = client
        .tracker_op(
            OpKind::PolicySet,
            json!({"project": KEY, "key": "close", "value": "owner"}),
        )
        .await
        .unwrap();
    assert_eq!(policy["code"], POLICY_IN_SETTINGS);
    let file = client
        .tracker_op(OpKind::Export, json!({"project": KEY, "path": "open.md"}))
        .await
        .unwrap();
    assert_eq!(file["ok"], true, "{file}");
    let written = std::fs::read_to_string(folder.join(".crazyagent/exports/open.md")).unwrap();
    assert!(written.starts_with(EXPORT_HEADER));
    let escape = client
        .tracker_op(
            OpKind::Export,
            json!({"project": KEY, "path": "../tickets.md"}),
        )
        .await
        .unwrap();
    assert_eq!(escape["code"], "export_path_refused");
    std::fs::write(
        folder.join(".crazyagent/context/roadmap.md"),
        "# Roadmap\n\n## V1: first release\nScope:\n- Sign in with a magic link\n",
    )
    .unwrap();
    let preview = client
        .tracker_op(
            OpKind::ImportPreview,
            json!({"project": KEY, "path": "roadmap.md"}),
        )
        .await
        .unwrap();
    assert_eq!(
        preview["data"]["items"].as_array().unwrap().len(),
        2,
        "{preview}"
    );
    let large = client
        .tracker_op(
            OpKind::Create,
            json!({"project": KEY, "title": "x".repeat(REQUEST_BYTES_MAX)}),
        )
        .await;
    assert_eq!(large.unwrap_err().0, "tracker_request_bounds");

    let app = client.tracker_settings(None).await.unwrap();
    assert_eq!(app.project, None);
    assert_eq!(app.version, 0);
    assert_eq!(app.values, TicketOverride::default());
    assert_eq!(app.below, TicketSettings::default());
    assert_eq!(app.effective, TicketSettings::default());
    let keys: Vec<&str> = app
        .choices
        .iter()
        .map(|choice| choice.key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "close",
            "create",
            "agent_edit",
            "cycle_days",
            "rollover",
            "import",
            "test_evidence",
            "skipped_counts",
            "case_protected_visibility",
            "run_writers",
            "run_retention",
            "manual_evidence",
            "ci_evidence"
        ]
    );
    let close = &app.choices[0];
    assert_eq!(
        close.options,
        [json!("agents"), json!("evidence"), json!("owner")]
    );
    assert_eq!(close.proposed, json!("evidence"));
    assert_eq!(close.custom, None);
    let cycle = &app.choices[3];
    assert_eq!(cycle.options, [json!(7), json!(14), json!(21), json!(28)]);
    assert_eq!(cycle.proposed, json!(14));
    assert_eq!(cycle.custom, Some(CustomBounds { min: 1, max: 90 }));
    assert_eq!(
        app.choices[12].options,
        [json!("engine"), json!("owner"), json!("agents")]
    );

    let values = TicketOverride {
        close: Some(ClosePolicy::Owner),
        ..TicketOverride::default()
    };
    let saved = client
        .tracker_save_settings(Some(project()), values, 0)
        .await
        .unwrap();
    assert_eq!(saved, SettingsSaved::Saved { version: 1 });
    let stale = client
        .tracker_save_settings(Some(project()), TicketOverride::default(), 0)
        .await
        .unwrap();
    assert_eq!(stale, SettingsSaved::Stale { current: 1 });
    let mine = client.tracker_settings(Some(project())).await.unwrap();
    assert_eq!(mine.project, Some(project()));
    assert_eq!(mine.values, values);
    assert_eq!(mine.version, 1);
    assert_eq!(mine.below, app.effective);
    assert_eq!(mine.effective.policy.close, ClosePolicy::Owner);
    let core = client
        .tracker_op(OpKind::PolicyGet, json!({"project": KEY}))
        .await
        .unwrap();
    assert_eq!(core["data"]["settings"][0]["key"], "close", "{core}");
    assert_eq!(core["data"]["settings"][0]["value"], "owner", "{core}");
    let unattached = client.tracker_settings(Some(ghost)).await;
    assert_eq!(unattached.unwrap_err().0, PROJECT_NOT_ATTACHED);
    served.stop().await;
}

#[test]
fn tracker_codes_cover_every_core_code() {
    assert!(
        TRACKER_CODES.windows(2).all(|pair| pair[0] < pair[1]),
        "sorted, each once"
    );
    for code in codes::ALL {
        assert!(TRACKER_CODES.contains(code), "core {code}");
    }
    let host = [
        LINK_CROSS_PROJECT,
        LINK_BROKEN,
        HOST_FAILED,
        PROJECT_NOT_ATTACHED,
        ACTOR_NOT_ENGINE,
        ACTOR_OTHER_PROJECT,
        EVIDENCE_OWNER_ONLY,
        EVIDENCE_ENGINE_ONLY,
        POLICY_IN_SETTINGS,
        crate::tracker::TRACKER_UNAVAILABLE,
    ];
    let connection: Vec<&str> = crate::engine::owner::QUESTIONS_CODES
        .iter()
        .copied()
        .filter(|code| !code.starts_with("questions_"))
        .collect();
    assert!(connection.contains(&"owner_grant_stale_or_revoked"));
    for code in host.iter().chain(&connection) {
        assert!(TRACKER_CODES.contains(code), "{code}");
    }
    for code in TRACKER_CODES {
        assert!(
            codes::ALL.contains(code)
                || host.contains(code)
                || connection.contains(code)
                || code.starts_with("tracker_"),
            "{code} has no source"
        );
    }
}

#[test]
fn an_unparsed_roadmap_section_becomes_one_question() {
    let (_tmp, root, folder) = engine();
    attached(&root);
    let state = folder.join(".crazyagent");
    let plan = crate::planning::publish(&state, PLANNED, "codex").unwrap();
    let database = root.join("engine.db");
    assert_eq!(
        crate::planning::raise(&database, PROJECT, &plan)
            .unwrap()
            .len(),
        1
    );

    let imported = context_import::after_planning(&database, PROJECT).unwrap();
    assert_eq!(
        imported,
        Imported::Applied {
            items: 3,
            questions: 1
        }
    );
    let db = questions::open(&root).unwrap();
    let page = questions::page(&db, &project(), None).unwrap();
    assert!(
        page.items
            .iter()
            .all(|q| q.status == QuestionStatus::Pending)
    );
    assert_eq!(page.items.len(), 2);
    let ours: Vec<_> = page
        .items
        .iter()
        .filter(|q| q.question.contains("Risks"))
        .collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].asker, "engine:planning");
    let context = ours[0].context.as_deref().unwrap();
    assert!(context.contains("context/roadmap.md"), "{context}");
    assert!(
        context.contains("The board may be slow on phones."),
        "{context}"
    );
    assert!(
        !page
            .items
            .iter()
            .any(|q| q.question.contains("Open questions"))
    );
    let titles = |root: &Path| {
        let listed = op(root, "list", json!({"project": KEY}));
        listed["data"]["tickets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["title"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(&root),
        ["Sign in with a magic link", "Run the full suite"]
    );
    let shown = op(&root, "show", json!({"id": "CA-1"}));
    assert_eq!(shown["data"]["ticket"]["created_by"], "engine:import");
    let view = std::fs::read_to_string(state.join("tickets.md")).unwrap();
    assert!(view.contains("Sign in with a magic link"), "{view}");

    let again = context_import::after_planning(&database, PROJECT).unwrap();
    assert_eq!(again, Imported::Skipped);
    assert_eq!(
        questions::page(&db, &project(), None).unwrap().items.len(),
        2
    );
    assert_eq!(titles(&root).len(), 2);
}

#[test]
fn tickets_md_is_rewritten_escaped_and_follows_no_link() {
    let (tmp, root, folder) = engine();
    attached(&root);
    let state = folder.join(".crazyagent");
    let view = state.join("tickets.md");
    assert_eq!(std::fs::read_to_string(&view).unwrap(), exported(&root));

    let title = "Ship <b>it</b> | [now](x) #1";
    assert_eq!(
        op(&root, "create", json!({"project": KEY, "title": title}))["ok"],
        true
    );
    let first = std::fs::read_to_string(&view).unwrap();
    assert_eq!(first, exported(&root));
    assert!(!first.contains("<b>"), "{first}");
    assert!(
        first.contains(r"Ship \<b\>it\</b\> \| \[now\](x) \#1"),
        "{first}"
    );
    assert_eq!(
        op(&root, "create", json!({"project": KEY, "title": "Second"}))["ok"],
        true
    );
    let second = std::fs::read_to_string(&view).unwrap();
    assert_ne!(second, first);
    assert!(second.contains("CA-2: Second"), "{second}");

    let base = tmp.path().canonicalize().unwrap();
    let outside = base.join("outside.md");
    std::fs::write(&outside, "owner's notes\n").unwrap();
    std::fs::remove_file(&view).unwrap();
    std::os::unix::fs::symlink(&outside, &view).unwrap();
    assert_eq!(
        op(&root, "create", json!({"project": KEY, "title": "Third"}))["ok"],
        true
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "owner's notes\n"
    );
    assert!(
        std::fs::symlink_metadata(&view)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let elsewhere = base.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::rename(&state, base.join("moved")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &state).unwrap();
    assert_eq!(
        op(&root, "create", json!({"project": KEY, "title": "Fourth"}))["ok"],
        true
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// Saves one project's ticket settings over the owner protocol. Inputs: the engine root,
/// the values and the version read. Output: the saved version.
fn saved(root: &Path, values: serde_json::Value, version: u64) -> u64 {
    let body = json!({"tracker": {"kind": "save_settings", "project": PROJECT,
        "values": values, "version": version}});
    match send(root, body).unwrap() {
        Reply::Saved {
            saved: SettingsSaved::Saved { version },
        } => version,
        other => panic!("expected a saved version, got {other:?}"),
    }
}

/// Counts what an import leaves behind: the project's tickets, its import marks and its
/// pending owner questions. Input: the engine root. Output: the three counts.
fn left_behind(root: &Path) -> (usize, i64, usize) {
    let listed = op(root, "list", json!({"project": KEY}));
    let tickets = listed["data"]["tickets"].as_array().unwrap().len();
    let db = questions::open(root).unwrap();
    let marks: i64 = db
        .query_row("SELECT count(*) FROM tracker_context_imports", [], |row| {
            row.get(0)
        })
        .unwrap();
    let asked = questions::page(&db, &project(), None).unwrap().items.len();
    (tickets, marks, asked)
}

/// Reads the ticket settings view of the app (None) or project `alpha` over the owner
/// protocol. Inputs: the engine root and the project. Output: the view.
fn viewed(root: &Path, project: Option<&str>) -> TicketSettingsView {
    let body = json!({"tracker": {"kind": "settings", "project": project}});
    match send(root, body).unwrap() {
        Reply::Settings { settings } => settings,
        other => panic!("expected the settings view, got {other:?}"),
    }
}

#[test]
fn the_test_settings_save_at_app_and_project_scope_and_run_writers_stays_locked() {
    let (_tmp, root, _folder) = engine();
    attached(&root);
    let app = json!({"tracker": {"kind": "save_settings", "project": null,
        "values": {"run_retention": "prune_age", "skipped_counts": "neutral"}, "version": 0}});
    match send(&root, app).unwrap() {
        Reply::Saved {
            saved: SettingsSaved::Saved { version },
        } => assert_eq!(version, 1),
        other => panic!("expected a saved version, got {other:?}"),
    }
    let values = json!({
        "test_evidence": "plan",
        "skipped_counts": "fail",
        "case_protected_visibility": "full"
    });
    assert_eq!(saved(&root, values, 0), 1);

    let mine = viewed(&root, Some(PROJECT));
    assert_eq!(mine.values.test_evidence, Some(TestEvidence::Plan));
    assert_eq!(mine.values.run_retention, None);
    let policy = mine.effective.policy;
    assert_eq!(policy.test_evidence, TestEvidence::Plan);
    assert_eq!(policy.skipped_counts, SkippedCounts::Fail);
    assert_eq!(
        policy.case_protected_visibility,
        CaseProtectedVisibility::Full
    );
    assert_eq!(policy.run_retention, RunRetention::PruneAge, "from the app");
    assert_eq!(policy.run_writers, RunWriters::EngineOnly);
    let core = op(&root, "policy_get", json!({"project": KEY}));
    let held = |key: &str| {
        let entries = core["data"]["settings"].as_array().unwrap();
        let entry = entries.iter().find(|entry| entry["key"] == key);
        entry.unwrap_or_else(|| panic!("{key} in {core}"))["value"].clone()
    };
    assert_eq!(held("test_evidence"), "plan");
    assert_eq!(held("skipped_counts"), "fail");
    assert_eq!(held("case_protected_visibility"), "full");
    assert_eq!(held("run_retention"), "prune_age");
    assert_eq!(held("run_writers"), "engine_only");

    let writers = mine
        .choices
        .iter()
        .find(|choice| choice.key == "run_writers")
        .unwrap();
    assert_eq!(writers.options, [json!("engine_only")]);
    assert_eq!(writers.proposed, json!("engine_only"));
    let locked = json!({"tracker": {"kind": "save_settings", "project": PROJECT,
        "values": {"run_writers": "any"}, "version": 1}});
    assert_eq!(
        send(&root, locked).unwrap_err().0,
        "tracker_request_invalid"
    );
    assert_eq!(viewed(&root, Some(PROJECT)).version, 1);
}

#[test]
fn an_on_request_project_imports_nothing_after_planning() {
    let (_tmp, root, folder) = engine();
    attached(&root);
    assert_eq!(saved(&root, json!({"import": "on_request"}), 0), 1);
    crate::planning::publish(&folder.join(".crazyagent"), PLANNED, "codex").unwrap();
    let database = root.join("engine.db");

    let skipped = context_import::after_planning(&database, PROJECT).unwrap();
    assert_eq!(skipped, Imported::Skipped);
    assert_eq!(left_behind(&root), (0, 0, 0));

    assert_eq!(saved(&root, json!({"import": "auto_once"}), 1), 2);
    let imported = context_import::after_planning(&database, PROJECT).unwrap();
    assert_eq!(
        imported,
        Imported::Applied {
            items: 3,
            questions: 1
        }
    );
    assert_eq!(left_behind(&root), (2, 1, 1));
}

#[test]
fn a_context_file_the_tracker_refuses_becomes_one_question() {
    let (_tmp, root, folder) = engine();
    attached(&root);
    let state = folder.join(".crazyagent");
    crate::planning::publish(&state, PLANNED, "codex").unwrap();
    std::fs::remove_file(state.join("context/release-process.md")).unwrap();

    let imported = context_import::after_planning(&root.join("engine.db"), PROJECT).unwrap();
    assert_eq!(
        imported,
        Imported::Applied {
            items: 2,
            questions: 2
        }
    );
    assert_eq!(left_behind(&root), (1, 1, 2));
    let db = questions::open(&root).unwrap();
    let page = questions::page(&db, &project(), None).unwrap();
    let refused: Vec<_> = page
        .items
        .iter()
        .filter(|q| q.question.contains("did not apply release-process.md"))
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", page.items);
    assert!(
        refused[0].question.contains(codes::IMPORT_PATH_REFUSED),
        "{}",
        refused[0].question
    );
    assert_eq!(refused[0].asker, "engine:planning");
    let context = refused[0].context.as_deref().unwrap();
    assert!(context.contains("context/release-process.md"), "{context}");
}
