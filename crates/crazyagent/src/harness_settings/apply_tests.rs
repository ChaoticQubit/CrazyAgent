use super::*;
use crate::harness_settings::tests::{fixture, fixture_text};
use crate::harness_settings::{Access, Key, Kind, Scope, Stored, Written, resolve};
use crate::harnesses::interactive::{self, Run};
use crate::harnesses::launch::{
    AccountBinding, CLAUDE_ENGINE_KEYS, CODEX_ENGINE_KEYS, LaunchLimits, LaunchRequest, Secret,
};
use crate::launch_context::AgentContext;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
            trust: crate::trust::Trust::Reviewed,
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
        self.pane_run(request, settings, None)
    }

    /// Builds what a terminal pane runs, as `pane` does, for an account whose relay key the
    /// harness reads from `key_file`, none for a signed-in account.
    fn pane_run(
        &self,
        request: &LaunchRequest,
        settings: &Applied,
        key_file: Option<&Path>,
    ) -> Vec<String> {
        let context = AgentContext::empty().with_harness_settings(settings.clone());
        let installed = context
            .install(&request.harness, request.account.home(), request.tools)
            .unwrap();
        let run = Run {
            session_id: SESSION,
            key_file,
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

/// Reads the value one launch gives a dotted key: Claude Code's from the `--settings`
/// JSON, Codex's from its last `-c` line for that key, read as TOML. Inputs: the harness,
/// the arguments after the executable and the key. Output: the value, none when unset.
fn launched(harness: &str, arguments: &[String], key: &str) -> Option<Value> {
    let document = if harness == "claude-code" {
        settings_json(arguments)
    } else {
        let line = configs(arguments)
            .into_iter()
            .rev()
            .chain(crate::launch_context::CODEX_CONFIG_FIXED)
            .find(|line| {
                line.split_once('=')
                    .is_some_and(|(name, _)| name.trim() == key)
            })?;
        serde_json::to_value(toml::from_str::<toml::Table>(line).unwrap()).unwrap()
    };
    at(&document, key).cloned()
}

/// Lists the settings one launch passes its harness, as dotted key and value: Claude
/// Code's `--settings` object, or Codex's `-c` lines with the fixed lines every installed
/// agent's `config.toml` holds. Inputs: the harness and the arguments after the executable.
fn engine_settings(harness: &str, arguments: &[String]) -> Vec<(String, Value)> {
    if harness == "claude-code" {
        crate::harness_settings::tests::json_leaves(&settings_json(arguments))
    } else {
        crate::harness_settings::tests::codex_settings(
            configs(arguments)
                .into_iter()
                .chain(crate::launch_context::CODEX_CONFIG_FIXED),
        )
    }
}

/// One catalog built from a fixture with more keys. Inputs: the harness and the keys as
/// (name, type, extra fields). Output: the catalog.
fn with_keys(harness: &str, keys: &[(&str, &str, Value)]) -> Catalog {
    let mut document: Value = serde_json::from_str(fixture_text(harness)).unwrap();
    let apply = if harness == "codex" {
        "codex_config"
    } else {
        "claude_settings"
    };
    for (name, kind, extra) in keys {
        let mut key = json!({"key": name, "label": "Fixture", "help": "Fixture key.",
            "type": kind, "apply": apply, "scope": "app_and_project", "locked": null});
        for (field, value) in extra.as_object().unwrap() {
            key[field] = value.clone();
        }
        document["keys"].as_array_mut().unwrap().push(key);
    }
    catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap()
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
            for tools in [false, true] {
                let request = case.request(harness, tools);
                let built = [
                    case.pane(&request, settings),
                    case.headless(&request, settings),
                ];
                let plain = [
                    case.pane(&request, &Applied::default()),
                    case.headless(&request, &Applied::default()),
                ];
                for (arguments, plain) in built.iter().zip(&plain) {
                    for key in catalog.keys.iter().filter(|key| key.locked.is_some()) {
                        let value = launched(harness, arguments, &key.key);
                        assert_eq!(value, launched(harness, plain, &key.key), "{}", key.key);
                        let engine = &key.locked.as_ref().unwrap().engine_value;
                        if !engine.is_null() {
                            assert_eq!(value.as_ref(), Some(engine), "{}", key.key);
                        }
                    }
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
        let lead = if tools { "" } else { "--no-daemon" };
        assert_eq!(pane[0], lead);
        assert_eq!(plain[0], lead);
        assert_eq!(pane[1..=owner.len()], owner);
        assert_eq!(pane[owner.len() + 1..], plain[1..]);
        let headless = case.headless(&request, &settings);
        let plain = case.headless(&request, &Applied::default());
        assert_eq!(headless[..owner.len()], owner);
        assert_eq!(headless[owner.len()..], plain[..]);
    }
}

#[test]
fn an_rcli_or_arg_setting_shows_not_applied_and_is_never_resolved() {
    let case = Case::new();
    case.set(
        &fixture("codex"),
        CODEX,
        &Scope::App,
        "hide_agent_reasoning",
        json!(true),
    );
    let rcli = fixture("rcli");
    let codex = with_keys(
        "codex",
        &[(
            "profile_note",
            "string",
            json!({"default": "", "max_bytes": 16, "apply": "arg"}),
        )],
    );
    for (catalog, key, value, version) in [
        (&rcli, "theme", json!("light"), RCLI),
        (&codex, "profile_note", json!("x"), CODEX),
    ] {
        for scope in [Scope::App, case.here()] {
            let written = store::set(&case.db, catalog, version, &scope, key, &value, 0).unwrap();
            let (code, setting) = match written {
                Written::Refused { code, setting } => (code, setting),
                other => panic!("{key}: {other:?}"),
            };
            assert_eq!(code, "harness_settings_key_not_qualified");
            assert_eq!(
                setting.access,
                Access::ReadOnly {
                    reason: crate::harness_settings::NOT_APPLIED.into()
                }
            );
            assert_eq!(
                (setting.effective, setting.source),
                (Value::Null, Source::NotApplied)
            );
            case.forge(&scope, &catalog.harness, key, &value);
        }
        let page = store::values(&case.db, catalog, version, &case.here(), None).unwrap();
        let row = page.settings.iter().find(|row| row.key == key).unwrap();
        assert_eq!(row.here, Some(value.clone()));
        assert_eq!(
            (&row.effective, row.source),
            (&Value::Null, Source::NotApplied)
        );
        assert!(matches!(row.access, Access::ReadOnly { .. }));
        let settings = resolved(&case.db, catalog, version, Some(&case.project)).unwrap();
        assert!(settings.values().iter().all(|value| value.key != key));
    }
    let model = rcli.key("model").unwrap();
    assert!(model.locked.as_ref().unwrap().engine_value.is_null());
    let mut document: Value = serde_json::from_str(fixture_text("rcli")).unwrap();
    document["keys"][1]["locked"]["engine_value"] = json!("account-model");
    assert_eq!(
        catalog::parse(&serde_json::to_vec(&document).unwrap())
            .unwrap_err()
            .0,
        "harness_settings_catalog_invalid"
    );
}

#[test]
fn a_key_left_to_the_harness_default_reaches_no_launch_until_the_owner_sets_it() {
    let case = Case::new();
    for (harness, name, apply) in [
        ("codex", "fixture_unset", "codex_config"),
        ("claude-code", "fixtureUnset", "claude_settings"),
    ] {
        let installed = installed(harness);
        let mut document: Value = serde_json::from_str(fixture_text(harness)).unwrap();
        document["keys"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key": name, "label": "Fixture",
            "help": "Fixture key the harness decides.", "type": "enum",
            "options": ["low", "high"], "default": null, "apply": apply,
            "scope": "app_and_project", "locked": null}));
        let parsed = catalog::parse(&serde_json::to_vec(&document).unwrap());
        assert!(parsed.is_ok(), "{harness}: an unset default parses");
        let catalog = parsed.unwrap();
        let launches = |settings: &Applied| {
            let mut seen = Vec::new();
            for tools in [false, true] {
                let request = case.request(harness, tools);
                for arguments in [
                    case.pane(&request, settings),
                    case.headless(&request, settings),
                ] {
                    seen.push(launched(harness, &arguments, name));
                }
            }
            assert_eq!(seen.len(), 4);
            seen
        };
        let current = || resolved(&case.db, &catalog, installed, Some(&case.project)).unwrap();
        let unset = current();
        assert!(unset.values().iter().all(|value| value.key != name));
        assert_eq!(launches(&unset), [None, None, None, None], "{harness}");
        let null = store::set(
            &case.db,
            &catalog,
            installed,
            &Scope::App,
            name,
            &Value::Null,
            0,
        );
        assert_eq!(null.unwrap_err().0, "harness_settings_value_invalid");
        let mut versions = Vec::new();
        for (scope, value) in [(Scope::App, json!("high")), (case.here(), json!("low"))] {
            let written =
                store::set(&case.db, &catalog, installed, &scope, name, &value, 0).unwrap();
            let setting = match written {
                Written::Saved { setting } => setting,
                other => panic!("{harness} {name}: {other:?}"),
            };
            assert_eq!(
                (&setting.effective, &setting.here),
                (&value, &Some(value.clone()))
            );
            assert_eq!(
                launches(&current()),
                vec![Some(value.clone()); 4],
                "{harness}"
            );
            versions.push((scope, setting.version));
        }
        for (scope, version) in versions.into_iter().rev() {
            let written = store::reset(&case.db, &catalog, installed, &scope, name, version);
            assert!(matches!(written, Ok(Written::Saved { .. })), "{harness}");
        }
        let reset = current();
        assert!(reset.values().iter().all(|value| value.key != name));
        assert_eq!(launches(&reset), [None, None, None, None], "{harness}");
    }
}

#[test]
fn every_key_the_engine_writes_into_a_codex_config_is_reserved() {
    let case = Case::new();
    for tools in [false, true] {
        let request = case.request("codex", tools);
        let installed = AgentContext::empty()
            .install("codex", request.account.home(), tools)
            .unwrap();
        let text = std::fs::read_to_string(request.account.home().join("config.toml")).unwrap();
        let config: toml::Table = toml::from_str(&text).unwrap();
        for written in ["developer_instructions", "project_doc_max_bytes"] {
            assert!(config.contains_key(written), "{text}");
        }
        for fixed in crate::launch_context::CODEX_CONFIG_FIXED {
            let line: toml::Table = toml::from_str(fixed).unwrap();
            for (key, value) in &line {
                assert_eq!(config.get(key), Some(value), "{fixed}");
            }
        }
        for key in config.keys() {
            assert!(CODEX_ENGINE_KEYS.contains(&key.as_str()), "{key}");
        }
        drop(installed);
    }
}

#[test]
fn an_owner_value_never_reaches_a_key_the_engine_sets_or_delivers_instructions_with() {
    let case = Case::new();
    let text = |max: i64| json!({"default": "", "max_bytes": max});
    let codex = with_keys(
        "codex",
        &[
            ("developer_instructions", "string", text(64)),
            ("experimental_instructions_file", "string", text(64)),
            ("model_instructions_file", "string", text(64)),
            ("project_doc_fallback_filenames", "string", text(64)),
            ("mcp_servers.extra.command", "string", text(64)),
            ("model", "string", text(64)),
        ],
    );
    let claude = with_keys(
        "claude-code",
        &[
            ("env.ANTHROPIC_BASE_URL", "string", text(64)),
            ("permissions.defaultMode", "string", text(64)),
            ("model", "string", text(64)),
            ("hooks.Stop", "string", text(64)),
            (
                "enableAllProjectMcpServers",
                "bool",
                json!({"default": false}),
            ),
            (
                "skipDangerousModePermissionPrompt",
                "bool",
                json!({"default": false}),
            ),
        ],
    );
    let cases: [(&Catalog, &[&str], usize); 2] = [
        (&codex, &CODEX_ENGINE_KEYS, 6),
        (&claude, &CLAUDE_ENGINE_KEYS, 6),
    ];
    for (catalog, reserved, owner) in cases {
        let version = installed(&catalog.harness);
        for key in &catalog.keys {
            let top = key.key.split('.').next().unwrap();
            if key.locked.is_none() && reserved.contains(&top) {
                for scope in [Scope::App, case.here()] {
                    case.set(catalog, version, &scope, &key.key, hostile(key));
                }
            }
        }
        let settings = resolved(&case.db, catalog, version, Some(&case.project)).unwrap();
        assert_eq!(settings.values().len(), owner, "{:?}", settings.values());
        for tools in [false, true] {
            let request = case.request(&catalog.harness, tools);
            assert_eq!(
                case.pane(&request, &settings),
                case.pane(&request, &Applied::default())
            );
            assert_eq!(
                case.headless(&request, &settings),
                case.headless(&request, &Applied::default())
            );
        }
    }
}

#[test]
#[should_panic]
fn a_codex_lock_no_launch_uses_stops_the_launch() {
    let case = Case::new();
    let mut document: Value = serde_json::from_str(fixture_text("codex")).unwrap();
    document["keys"][4]["locked"]["engine_value"] = json!("workspace-write");
    let catalog = catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    let settings = resolved(&case.db, &catalog, CODEX, None).unwrap();
    let _ = case.pane(&case.request("codex", false), &settings);
}

#[test]
#[should_panic]
fn a_claude_lock_no_launch_sets_stops_the_launch() {
    let case = Case::new();
    let catalog = with_keys(
        "claude-code",
        &[(
            "permissions.defaultMode",
            "enum",
            json!({"options": ["default", "plan"], "default": "default",
                "locked": {"reason": "Fixture.", "engine_value": "plan"}}),
        )],
    );
    let settings = resolved(&case.db, &catalog, CLAUDE, None).unwrap();
    let _ = case.headless(&case.request("claude-code", false), &settings);
}

#[test]
fn a_harness_is_asked_for_its_version_once_per_executable_file() {
    let case = Case::new();
    let catalog = fixture("codex");
    case.set(
        &catalog,
        CODEX,
        &Scope::App,
        "hide_agent_reasoning",
        json!(true),
    );
    let executable = case.base.join("bin").join("codex-versioned");
    std::fs::write(
        &executable,
        "#!/bin/sh\necho asked >> \"$0.count\"\necho \"codex-cli 0.50.0\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    for _ in 0..3 {
        let settings = for_catalog(&case.db, &catalog, &executable, Some(&case.project)).unwrap();
        let names: Vec<&str> = settings
            .values()
            .iter()
            .map(|value| value.key.as_str())
            .collect();
        assert_eq!(names, ["hide_agent_reasoning"]);
    }
    let count = case.base.join("bin").join("codex-versioned.count");
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 1);
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
fn every_key_the_engine_sets_at_launch_is_locked_with_a_reason() {
    use crate::harness_settings::tests::{assert_locked_in_shipped, codex_file_settings};
    let case = Case::new();
    let home = case.base.join("home");
    let key_file = home.join("relay-key");
    for harness in ["claude-code", "codex"] {
        let shipped = catalog::builtin(harness).unwrap().unwrap();
        let version = shipped.qualified_versions.first().map(String::as_str);
        let settings = resolved(&case.db, &shipped, version, Some(&case.project)).unwrap();
        let chosen = shipped
            .keys
            .iter()
            .filter(|key| key.harness_default.is_some())
            .count();
        assert_eq!(settings.values().len(), chosen);
        assert!(settings.values().iter().all(|value| {
            value.source == Source::Default
                && shipped
                    .key(&value.key)
                    .is_some_and(|key| key.harness_default.is_some() && key.default == value.value)
        }));
        assert_eq!(settings.harness(), Some(harness));
        let mut launched: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
        let mut launches = 0;
        for tools in [false, true] {
            for keyed in [false, true] {
                let mut request = case.request(harness, tools);
                if keyed {
                    request.account = AccountBinding::ApiKey {
                        home: home.clone(),
                        key: Secret::new(crate::model_gateway::relay::LOCAL_PLACEHOLDER.into())
                            .unwrap(),
                        base_url: None,
                    };
                }
                let pane = case.pane_run(&request, &settings, keyed.then_some(key_file.as_path()));
                let headless = case.headless(&request, &settings);
                for arguments in [pane, headless] {
                    let set = engine_settings(harness, &arguments);
                    assert_locked_in_shipped(harness, &set);
                    launches += 1;
                    for (key, value) in set {
                        launched.entry(key).or_default().push(value);
                    }
                }
            }
            if harness == "codex" {
                let context = AgentContext::empty().with_harness_settings(settings.clone());
                let installed = context.install(harness, &home, tools).unwrap();
                let text = std::fs::read_to_string(home.join("config.toml")).unwrap();
                drop(installed);
                let written = codex_file_settings(&text);
                assert!(
                    written
                        .iter()
                        .any(|(key, _)| key == "developer_instructions")
                );
                assert_locked_in_shipped(harness, &written);
            }
        }
        assert_eq!(launches, 8);
        for key in &shipped.keys {
            let Some(engine) = key
                .locked
                .as_ref()
                .map(|lock| &lock.engine_value)
                .filter(|value| !value.is_null())
            else {
                continue;
            };
            let values = launched
                .get(&key.key)
                .unwrap_or_else(|| panic!("{harness} locks {} to a value no launch sets", key.key));
            assert_eq!(values.len(), launches, "{harness} {}", key.key);
            assert!(
                values.iter().all(|value| value == engine),
                "{harness} {}",
                key.key
            );
        }
    }
}

#[test]
fn auto_memory_starts_off_for_claude_agents_unless_the_owner_turns_it_on() {
    let case = Case::new();
    let shipped = catalog::builtin("claude-code").unwrap().unwrap();
    let key = shipped.key("autoMemoryEnabled").unwrap();
    assert!(key.locked.is_none());
    assert_eq!(key.default, json!(false));
    assert_eq!(key.harness_default, Some(json!(true)));
    let chosen: Vec<String> = catalog::BUILTIN
        .iter()
        .flat_map(|(harness, _)| catalog::builtin(harness).unwrap().unwrap().keys)
        .filter(|key| key.harness_default.is_some())
        .map(|key| key.key)
        .collect();
    assert_eq!(chosen, ["autoMemoryEnabled"]);
    let version = shipped.qualified_versions.first().map(String::as_str);
    let request = case.request("claude-code", false);
    let launched = |settings: &Applied| {
        [
            case.pane(&request, settings),
            case.headless(&request, settings),
        ]
        .map(|arguments| settings_json(&arguments)["autoMemoryEnabled"].clone())
    };
    let off = resolved(&case.db, &shipped, version, Some(&case.project)).unwrap();
    let value = off
        .values()
        .iter()
        .find(|value| value.key == "autoMemoryEnabled")
        .unwrap();
    assert_eq!(
        (&value.value, value.source),
        (&json!(false), Source::Default)
    );
    assert_eq!(launched(&off), [json!(false), json!(false)]);
    let empty = Stored::new();
    let row = crate::harness_settings::setting(
        &shipped,
        version,
        &case.here(),
        "autoMemoryEnabled",
        &empty,
        &empty,
        0,
    );
    assert_eq!(
        (row.effective, row.source, row.access),
        (json!(false), Source::Default, Access::Editable)
    );
    case.set(
        &shipped,
        version,
        &Scope::App,
        "autoMemoryEnabled",
        json!(true),
    );
    let on = resolved(&case.db, &shipped, version, Some(&case.project)).unwrap();
    assert_eq!(launched(&on), [json!(true), json!(true)]);
    case.set(
        &shipped,
        version,
        &case.here(),
        "autoMemoryEnabled",
        json!(false),
    );
    let project = resolved(&case.db, &shipped, version, Some(&case.project)).unwrap();
    assert_eq!(launched(&project), [json!(false), json!(false)]);
    let unqualified = resolved(&case.db, &shipped, Some("2.1.0"), Some(&case.project)).unwrap();
    assert!(unqualified.values().is_empty());
    assert_eq!(launched(&unqualified), [Value::Null, Value::Null]);
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
        key: "tui.status_label".into(),
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
fn rcli_values_never_merge_into_a_settings_object() {
    let _ = json_settings(&json!({}), &Applied::default(), Apply::RcliConfig, &[]);
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

/// The fixed `--settings` object of a Claude Code master turn, as `claude_host` builds it.
fn master_engine() -> Value {
    json!({
        "attribution": {"commit": "", "pr": ""},
        "disableAllHooks": true,
        "includeCoAuthoredBy": false,
    })
}

#[test]
fn a_value_an_older_catalog_allowed_never_blocks_a_master_command() {
    let case = Case::new();
    let catalog = fixture("claude-code");
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
        &Scope::App,
        "cleanupPeriodDays",
        json!(45),
    );
    let mut document: Value = serde_json::from_str(fixture_text("claude-code")).unwrap();
    let style = document["keys"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|key| key["key"] == "outputStyle")
        .unwrap();
    style["max_bytes"] = json!(4);
    let later = catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    for (catalog, keys) in [
        (&catalog, vec!["cleanupPeriodDays", "outputStyle"]),
        (&later, vec!["cleanupPeriodDays"]),
    ] {
        let settings = resolved(&case.db, catalog, CLAUDE, Some(&case.project)).unwrap();
        let names: Vec<&str> = settings
            .values()
            .iter()
            .map(|value| value.key.as_str())
            .collect();
        assert_eq!(names, keys);
        let text = json_settings(
            &master_engine(),
            &settings,
            Apply::ClaudeSettings,
            &CLAUDE_ENGINE_KEYS,
        )
        .to_string();
        let read = read_back(
            Some(catalog),
            &master_engine(),
            Apply::ClaudeSettings,
            &CLAUDE_ENGINE_KEYS,
            &text,
        )
        .expect("the verifier accepts the command the engine built");
        assert_eq!(read, settings);
    }
}

#[test]
fn a_master_verifier_reads_back_only_settings_the_engine_would_build() {
    let catalog = fixture("claude-code");
    let engine = master_engine();
    let app: Stored = [
        ("cleanupPeriodDays".to_owned(), json!(60)),
        ("feedback.surveys".to_owned(), json!(false)),
    ]
    .into_iter()
    .collect();
    let settings = Applied::new(&catalog, resolve(&catalog, CLAUDE, &app, None));
    let reserved = ["apiKeyHelper"];
    let text = json_settings(&engine, &settings, Apply::ClaudeSettings, &reserved).to_string();
    let read = read_back(
        Some(&catalog),
        &engine,
        Apply::ClaudeSettings,
        &reserved,
        &text,
    )
    .expect("the engine's own text reads back");
    assert_eq!(
        read.values()
            .iter()
            .map(|value| (value.key.as_str(), &value.value))
            .collect::<Vec<_>>(),
        [
            ("cleanupPeriodDays", &json!(60)),
            ("feedback.surveys", &json!(false))
        ]
    );
    assert_eq!(
        json_settings(&engine, &read, Apply::ClaudeSettings, &reserved).to_string(),
        text
    );
    let with = |key: &str, value: Value| {
        let mut forged = engine.clone();
        forged[key] = value;
        forged
    };
    for forged in [
        with("disableAllHooks", json!(false)),
        with("hooks", json!({})),
        with("cleanupPeriodDays", json!(0)),
        with("cleanupPeriodDays", json!("60")),
        with("apiKeyHelper", json!("/bin/sh -c id")),
        with("includeCoAuthoredBy", json!(true)),
        with(
            "attribution",
            json!({"commit": "Co-Authored-By: x", "pr": ""}),
        ),
        with("feedback", json!({"surveys": false, "extra": 1})),
        json!({"disableAllHooks": true}),
        json!([]),
    ] {
        assert!(
            read_back(
                Some(&catalog),
                &engine,
                Apply::ClaudeSettings,
                &reserved,
                &forged.to_string()
            )
            .is_none(),
            "{forged}"
        );
    }
    assert!(
        read_back(
            Some(&catalog),
            &engine,
            Apply::ClaudeSettings,
            &reserved,
            "{"
        )
        .is_none()
    );
    let bare = read_back(
        None,
        &engine,
        Apply::ClaudeSettings,
        &reserved,
        &engine.to_string(),
    );
    assert_eq!(bare, Some(Applied::default()));
    assert!(read_back(None, &engine, Apply::ClaudeSettings, &reserved, &text).is_none());
}

#[test]
fn owner_codex_settings_for_a_config_file_are_one_line_each_or_none() {
    let catalog = fixture("codex");
    let app: Stored = [
        ("hide_agent_reasoning".to_owned(), json!(true)),
        ("project_doc_max_bytes".to_owned(), json!(65536)),
        ("tui.status_label".to_owned(), json!("one")),
    ]
    .into_iter()
    .collect();
    let settings = Applied::new(&catalog, resolve(&catalog, CODEX, &app, None));
    let engine = [
        "approval_policy=\"never\"".to_owned(),
        "project_doc_max_bytes=0".to_owned(),
    ];
    assert_eq!(
        codex_config(&engine, &["tui"], &settings).as_deref(),
        Some("hide_agent_reasoning=true\n")
    );
    assert_eq!(
        codex_config(&engine, &[], &settings).as_deref(),
        Some("hide_agent_reasoning=true\ntui.status_label=\"one\"\n")
    );
    assert_eq!(codex_config(&engine, &[], &Applied::default()), None);
}

#[test]
#[should_panic]
fn a_lock_no_launch_writes_never_names_an_engine_value() {
    let mut catalog = fixture("rcli");
    let model = catalog
        .keys
        .iter_mut()
        .find(|key| key.key == "model")
        .unwrap();
    model.locked.as_mut().unwrap().engine_value = json!("account-model");
    let _ = Applied::new(&catalog, resolve(&catalog, RCLI, &Stored::new(), None));
}

#[test]
#[should_panic]
fn an_owner_value_no_launch_writes_never_enters_the_applied_settings() {
    let catalog = fixture("rcli");
    let theme = Resolved {
        key: "theme".into(),
        value: json!("light"),
        source: Source::App,
        apply: Apply::RcliConfig,
    };
    let _ = Applied::new(&catalog, vec![theme]);
}
