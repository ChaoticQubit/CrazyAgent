use super::*;
use crate::harness_settings::tests::{fixture, fixture_text};
use crate::harness_settings::{Key, Kind, Scope, Stored, Written, resolve};
use crate::harnesses::interactive::{self, Run};
use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest};
use crate::launch_context::AgentContext;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const PROJECT: &str = "alpha";
const SESSION: &str = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
const CODEX: Option<&str> = Some("0.50.0");
const CLAUDE: Option<&str> = Some("2.1.309");
const RCLI: Option<&str> = Some("0.9.0");

/// One engine database with a set-up project, an agent home and a sandbox profile, all
/// in one private temporary folder.
struct Case {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    db: Connection,
    project: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-harness-apply-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for folder in ["engine", "project", "home", "bin"] {
            std::fs::create_dir(base.join(folder)).unwrap();
            std::fs::set_permissions(base.join(folder), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let setup = Connection::open(base.join("engine").join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&setup).unwrap();
        crate::ide::workspace::tests::add_project(&setup, PROJECT, &base.join("project"));
        drop(setup);
        std::fs::write(base.join("project.sb"), "(version 1)\n(allow default)\n").unwrap();
        let db = crate::storage::versioned::open(&base.join("engine")).unwrap();
        Self {
            _tmp: tmp,
            base,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
        }
    }

    fn here(&self) -> Scope {
        Scope::Project(self.project.clone())
    }

    /// Saves one value through the store, as the owner protocol does.
    fn set(
        &self,
        catalog: &Catalog,
        installed: Option<&str>,
        scope: &Scope,
        key: &str,
        value: Value,
    ) {
        let written = store::set(&self.db, catalog, installed, scope, key, &value, 0).unwrap();
        assert!(
            matches!(written, Written::Saved { .. }),
            "{key}: {written:?}"
        );
    }

    /// Writes one row past the store's checks, as a damaged or hostile database would hold.
    fn forge(&self, scope: &Scope, harness: &str, key: &str, value: &Value) {
        let (scope, project) = match scope {
            Scope::App => ("app", ""),
            Scope::Project(project) => ("project", project.as_str()),
        };
        self.db
            .execute(
                "INSERT OR REPLACE INTO harness_settings(scope, project, harness, key, value_json, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                rusqlite::params![scope, project, harness, key, value.to_string()],
            )
            .unwrap();
    }

    fn request(&self, harness: &str, tools: bool) -> LaunchRequest {
        let executable = self.base.join("bin").join(harness);
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        LaunchRequest {
            harness: harness.into(),
            executable,
            working_dir: self.base.join("project"),
            account: AccountBinding::Subscription {
                home: self.base.join("home"),
            },
            model: None,
            tools,
            sandbox_profile: Some(self.base.join("project.sb")),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        }
    }

    /// Builds what a terminal pane runs for one agent with these settings, after the
    /// project's real install into the agent home. Output: the arguments after the
    /// harness executable.
    fn pane(&self, request: &LaunchRequest, settings: &Applied) -> Vec<String> {
        let context = AgentContext::empty().with_harness_settings(settings.clone());
        let installed = context
            .install(&request.harness, request.account.home(), request.tools)
            .unwrap();
        let run = Run {
            session_id: SESSION,
            key_file: None,
            resume: false,
        };
        let argv = interactive::command(request, Some(&installed), &run)
            .unwrap()
            .argv;
        after_executable(&argv, request)
    }

    /// Builds the headless command for one agent with these settings through
    /// `command_with_context`. Output: the arguments after the harness executable.
    fn headless(&self, request: &LaunchRequest, settings: &Applied) -> Vec<String> {
        let context = AgentContext::empty().with_harness_settings(settings.clone());
        let (command, _installed) =
            crate::harnesses::launch::command_with_context(request, &context).unwrap();
        let argv: Vec<String> = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        after_executable(&argv, request)
    }
}

fn after_executable(argv: &[String], request: &LaunchRequest) -> Vec<String> {
    let executable = request.executable.to_str().unwrap();
    let index = argv
        .iter()
        .position(|argument| argument == executable)
        .expect("the harness executable is in the command");
    argv[index + 1..].to_vec()
}

fn settings_json(arguments: &[String]) -> Value {
    let index = arguments
        .iter()
        .position(|argument| argument == "--settings")
        .expect("a --settings argument");
    serde_json::from_str(&arguments[index + 1]).unwrap()
}

fn configs(arguments: &[String]) -> Vec<&str> {
    arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| *argument == "-c")
        .map(|(index, _)| arguments[index + 1].as_str())
        .collect()
}

fn installed(harness: &str) -> Option<&'static str> {
    match harness {
        "codex" => CODEX,
        "claude-code" => CLAUDE,
        _ => RCLI,
    }
}

/// The fixture catalog of one harness with every lock taken off, as a catalog with a
/// missing lock would ship.
fn unlocked(harness: &str) -> Catalog {
    let mut document: Value = serde_json::from_str(fixture_text(harness)).unwrap();
    for key in document["keys"].as_array_mut().unwrap() {
        key["locked"] = Value::Null;
    }
    catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap()
}

/// A value of the key's own type that is not the engine's value.
fn hostile(key: &Key) -> Value {
    let engine = key.locked.as_ref().map(|locked| &locked.engine_value);
    let value = match key.kind {
        Kind::Bool => json!(engine.and_then(Value::as_bool) != Some(true)),
        Kind::Integer => json!(key.max.unwrap()),
        Kind::Enum => json!(
            key.options
                .iter()
                .rev()
                .find(|option| engine.and_then(Value::as_str) != Some(option.as_str()))
                .unwrap()
        ),
        Kind::String => json!("/bin/sh -c 'echo forged'"),
    };
    assert_ne!(Some(&value), engine);
    value
}

#[test]
fn no_owner_or_project_value_can_override_a_locked_key() {
    let case = Case::new();
    for harness in ["claude-code", "codex"] {
        let catalog = fixture(harness);
        let open = catalog
            .keys
            .iter()
            .find(|key| key.locked.is_none())
            .unwrap();
        case.set(
            &catalog,
            installed(harness),
            &Scope::App,
            &open.key,
            hostile(open),
        );
        for key in catalog.keys.iter().filter(|key| key.locked.is_some()) {
            for scope in [Scope::App, case.here()] {
                case.forge(&scope, harness, &key.key, &hostile(key));
            }
        }
        let shipped =
            resolved(&case.db, &catalog, installed(harness), Some(&case.project)).unwrap();
        let names: Vec<&str> = shipped
            .values()
            .iter()
            .map(|value| value.key.as_str())
            .collect();
        assert_eq!(names, [open.key.as_str()], "{harness}");
        let missing_locks = unlocked(harness);
        let forged = resolved(
            &case.db,
            &missing_locks,
            installed(harness),
            Some(&case.project),
        )
        .unwrap();
        let locked = catalog
            .keys
            .iter()
            .filter(|key| key.locked.is_some())
            .count();
        assert_eq!(forged.values().len(), 1 + locked, "{harness}");
        assert!(
            forged
                .values()
                .iter()
                .all(|value| value.source == Source::Project || value.key == open.key)
        );
        for settings in [&shipped, &forged] {
            let request = case.request(harness, false);
            for arguments in [
                case.pane(&request, settings),
                case.headless(&request, settings),
            ] {
                if harness == "claude-code" {
                    let json = settings_json(&arguments);
                    assert_eq!(json["disableAllHooks"], json!(true));
                    assert_eq!(json["includeCoAuthoredBy"], json!(false));
                    assert_eq!(json["attribution"], json!({"commit": "", "pr": ""}));
                    assert!(json.get("apiKeyHelper").is_none(), "{json}");
                    continue;
                }
                let lines = configs(&arguments);
                for (key, engine) in [
                    ("sandbox_mode", "sandbox_mode=\"read-only\""),
                    ("approval_policy", "approval_policy=\"never\""),
                ] {
                    let set: Vec<&str> = lines
                        .iter()
                        .copied()
                        .filter(|line| line.starts_with(&format!("{key}=")))
                        .collect();
                    assert_eq!(set, [engine], "{arguments:?}");
                }
            }
        }
    }
}

#[test]
fn an_owner_claude_setting_reaches_the_agent_settings_json() {
    let case = Case::new();
    let catalog = fixture("claude-code");
    case.set(
        &catalog,
        CLAUDE,
        &Scope::App,
        "cleanupPeriodDays",
        json!(45),
    );
    case.set(
        &catalog,
        CLAUDE,
        &Scope::App,
        "spinnerTipsEnabled",
        json!(false),
    );
    case.set(
        &catalog,
        CLAUDE,
        &Scope::App,
        "outputStyle",
        json!("Explanatory"),
    );
    case.set(
        &catalog,
        CLAUDE,
        &case.here(),
        "cleanupPeriodDays",
        json!(60),
    );
    case.set(
        &catalog,
        CLAUDE,
        &case.here(),
        "feedback.surveys",
        json!(false),
    );
    let project = resolved(&case.db, &catalog, CLAUDE, Some(&case.project)).unwrap();
    let app = resolved(&case.db, &catalog, CLAUDE, None).unwrap();
    let request = case.request("claude-code", false);
    let expected = |days: i64, surveys: Option<bool>| {
        let mut owner = json!({
            "cleanupPeriodDays": days,
            "spinnerTipsEnabled": false,
            "outputStyle": "Explanatory",
        });
        if let Some(surveys) = surveys {
            owner["feedback"] = json!({"surveys": surveys});
        }
        owner
    };
    let with = |mut engine: Value, owner: Value| {
        for (key, value) in owner.as_object().unwrap() {
            engine[key] = value.clone();
        }
        engine
    };
    let pane_engine = json!({
        "attribution": {"commit": "", "pr": ""},
        "disableAllHooks": true,
        "includeCoAuthoredBy": false,
        "skipDangerousModePermissionPrompt": true,
    });
    let headless_engine = json!({
        "attribution": {"commit": "", "pr": ""},
        "disableAllHooks": true,
        "includeCoAuthoredBy": false,
    });
    for (settings, owner) in [
        (&project, expected(60, Some(false))),
        (&app, expected(45, None)),
    ] {
        let pane = case.pane(&request, settings);
        assert_eq!(
            settings_json(&pane),
            with(pane_engine.clone(), owner.clone())
        );
        let plain = case.pane(&request, &Applied::default());
        assert_eq!(pane.len(), plain.len());
        for (index, (one, other)) in pane.iter().zip(&plain).enumerate() {
            assert!(
                one == other || plain[index - 1] == "--settings",
                "{one} {other}"
            );
        }
        let headless = case.headless(&request, settings);
        assert_eq!(
            settings_json(&headless),
            with(headless_engine.clone(), owner)
        );
    }
}

#[test]
fn an_owner_codex_setting_becomes_one_c_line() {
    let case = Case::new();
    let catalog = fixture("codex");
    case.set(
        &catalog,
        CODEX,
        &Scope::App,
        "model_reasoning_effort",
        json!("high"),
    );
    case.set(
        &catalog,
        CODEX,
        &case.here(),
        "hide_agent_reasoning",
        json!(true),
    );
    case.set(
        &catalog,
        CODEX,
        &Scope::App,
        "tui.status_label",
        json!("agent \"one\""),
    );
    let settings = resolved(&case.db, &catalog, CODEX, Some(&case.project)).unwrap();
    let owner = [
        "-c",
        "hide_agent_reasoning=true",
        "-c",
        "model_reasoning_effort=\"high\"",
        "-c",
        "tui.status_label=\"agent \\\"one\\\"\"",
    ];
    for tools in [false, true] {
        let request = case.request("codex", tools);
        let pane = case.pane(&request, &settings);
        let plain = case.pane(&request, &Applied::default());
        assert_eq!(pane[..owner.len()], owner);
        assert_eq!(pane[owner.len()..], plain[..]);
        let headless = case.headless(&request, &settings);
        let plain = case.headless(&request, &Applied::default());
        assert_eq!(headless[..owner.len()], owner);
        assert_eq!(headless[owner.len()..], plain[..]);
    }
}

#[test]
fn an_rcli_setting_merges_into_the_agent_config() {
    let case = Case::new();
    let catalog = fixture("rcli");
    case.set(&catalog, RCLI, &case.here(), "theme", json!("light"));
    case.forge(&Scope::App, "rcli", "model", &json!("forged-model"));
    let engine = json!({"model": "account-model", "telemetry": {"enabled": false}});
    let settings = resolved(&case.db, &catalog, RCLI, Some(&case.project)).unwrap();
    assert_eq!(
        json_settings(&engine, &settings, Apply::RcliConfig, &[]),
        json!({"model": "account-model", "telemetry": {"enabled": false}, "theme": "light"})
    );
    let forged = resolved(&case.db, &unlocked("rcli"), RCLI, Some(&case.project)).unwrap();
    assert_eq!(forged.values().len(), 2);
    assert_eq!(
        json_settings(&engine, &forged, Apply::RcliConfig, &[]),
        json!({"model": "account-model", "telemetry": {"enabled": false}, "theme": "light"})
    );
    assert_eq!(
        json_settings(&json!({}), &forged, Apply::RcliConfig, &["model"]),
        json!({"theme": "light"})
    );
}

#[test]
fn an_unqualified_harness_version_launches_with_engine_defaults_only() {
    let case = Case::new();
    let codex = fixture("codex");
    let claude = fixture("claude-code");
    case.set(
        &codex,
        CODEX,
        &Scope::App,
        "hide_agent_reasoning",
        json!(true),
    );
    case.set(
        &codex,
        CODEX,
        &case.here(),
        "model_reasoning_effort",
        json!("low"),
    );
    case.set(&claude, CLAUDE, &Scope::App, "cleanupPeriodDays", json!(45));
    for (catalog, versions) in [
        (&codex, ["0.49.0", "0.50.0-beta"]),
        (&claude, ["2.1.0", "2.1.3090"]),
    ] {
        let request = case.request(&catalog.harness, false);
        let plain_pane = case.pane(&request, &Applied::default());
        let plain_headless = case.headless(&request, &Applied::default());
        for version in versions.map(Some).into_iter().chain([None]) {
            let settings = resolved(&case.db, catalog, version, Some(&case.project)).unwrap();
            assert!(settings.values().is_empty(), "{version:?}");
            assert_eq!(settings.harness(), Some(catalog.harness.as_str()));
            assert_eq!(case.pane(&request, &settings), plain_pane);
            assert_eq!(case.headless(&request, &settings), plain_headless);
        }
        let qualified = resolved(
            &case.db,
            catalog,
            installed(&catalog.harness),
            Some(&case.project),
        )
        .unwrap();
        assert!(!qualified.values().is_empty());
        assert_ne!(case.pane(&request, &qualified), plain_pane);
    }
    let silent = case.request("codex", false).executable;
    for harness in ["codex", "claude", "not-a-harness"] {
        let settings = for_launch(&case.db, harness, &silent, Some(&case.project)).unwrap();
        assert!(settings.values().is_empty(), "{harness}");
    }
}

#[test]
fn an_owner_key_beside_an_engine_key_is_kept_and_one_on_or_around_it_is_dropped() {
    let keys: Vec<Value> = [
        "features.shell_tool",
        "web_search.mode",
        "tools",
        "features_extra",
        "mcp_servers.extra",
        "hide_agent_reasoning",
    ]
    .iter()
    .map(|key| {
        json!({"key": key, "label": "Fixture", "help": "Fixture key.", "type": "bool",
            "default": false, "apply": "codex_config", "scope": "app_and_project",
            "locked": null})
    })
    .collect();
    let document = json!({
        "harness": "codex",
        "qualified_versions": ["0.50.0"],
        "sources": ["https://example.invalid/fixture/codex-config"],
        "keys": keys,
    });
    let catalog = catalog::parse(document.to_string().as_bytes()).unwrap();
    let app: Stored = catalog
        .keys
        .iter()
        .map(|key| (key.key.clone(), json!(true)))
        .collect();
    let settings = Applied::new(&catalog, resolve(&catalog, CODEX, &app, None));
    assert_eq!(settings.values().len(), 6);
    let engine = [
        "features.shell_tool=false",
        "web_search=\"disabled\"",
        "tools.view_image=false",
    ]
    .map(str::to_owned);
    assert_eq!(
        codex_lines(&engine, &["mcp_servers"], &settings),
        ["features_extra=true", "hide_agent_reasoning=true"]
    );
}

#[test]
#[should_panic]
fn a_locked_key_never_enters_the_applied_settings() {
    let catalog = fixture("codex");
    let forged = Resolved {
        key: "sandbox_mode".into(),
        value: json!("danger-full-access"),
        source: Source::App,
        apply: Apply::CodexConfig,
    };
    let _ = Applied::new(&catalog, vec![forged]);
}

#[test]
#[should_panic]
fn a_codex_value_that_is_no_toml_literal_is_never_rendered() {
    let catalog = fixture("codex");
    let fraction = Resolved {
        key: "project_doc_max_bytes".into(),
        value: json!(1.5),
        source: Source::App,
        apply: Apply::CodexConfig,
    };
    let _ = codex_lines(&[], &[], &Applied::new(&catalog, vec![fraction]));
}

#[test]
#[should_panic]
fn codex_values_never_merge_into_a_settings_object() {
    let settings = resolved(&Case::new().db, &fixture("codex"), CODEX, None).unwrap();
    let _ = json_settings(&json!({}), &settings, Apply::CodexConfig, &[]);
}

#[test]
#[should_panic]
fn owner_settings_merge_only_into_an_object() {
    let _ = json_settings(&json!([]), &Applied::default(), Apply::ClaudeSettings, &[]);
}

#[test]
#[should_panic]
fn a_key_inside_another_owner_key_is_never_merged() {
    let mut catalog = fixture("claude-code");
    let mut outer = catalog.key("feedback.surveys").unwrap().clone();
    outer.key = "feedback".into();
    catalog.keys.push(outer);
    let app: Stored = [
        ("feedback".to_owned(), json!(false)),
        ("feedback.surveys".to_owned(), json!(false)),
    ]
    .into_iter()
    .collect();
    let settings = Applied::new(&catalog, resolve(&catalog, CLAUDE, &app, None));
    let _ = json_settings(&json!({}), &settings, Apply::ClaudeSettings, &[]);
}
