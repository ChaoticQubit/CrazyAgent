use super::*;
use crate::harness_settings::catalog::{self, Catalog};
use crate::harness_settings::{APP_ONLY, Scope as Layer, Stored, catalog_page, setting};
use crate::tui::client::OwnerEvent;
use crate::tui::glyphs::Status;
use crate::tui::screens::settings_accounts::tests::{ctx, fixture, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use serde_json::json;

const QUALIFIED: Option<&str> = Some("0.51.0");
const SANDBOX: &str = "CrazyAgent runs every agent in its own sandbox.";

fn alpha() -> ProjectId {
    ProjectId::new("alpha").unwrap()
}

/// The fixture Codex catalog document with the sandbox key locked and kept unset by the
/// engine, as the catalog HS-3 reserves engine keys with. Output: the document; a fixture
/// without the sandbox key fails the calling test.
fn codex_json() -> Value {
    let mut document = crate::harness_settings::tests::fixture_json();
    let sandbox = document["keys"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|key| key["key"] == "sandbox_mode")
        .unwrap();
    sandbox["locked"]["engine_value"] = Value::Null;
    document
}

/// The fixture Codex catalog the pages read. Output: the parsed catalog.
fn codex() -> Catalog {
    catalog::parse(&serde_json::to_vec(&codex_json()).unwrap()).unwrap()
}

/// The fixture Codex catalog with its one integer key, the project doc limit the engine
/// locks, turned into an ordinary key the owner edits, so a test can type numbers into a
/// field. Output: the catalog; a fixture whose doc limit is not a locked integer fails the
/// calling test.
fn open_number() -> Catalog {
    let mut document = codex_json();
    let limit = document["keys"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|key| key["key"] == "project_doc_max_bytes")
        .unwrap();
    assert_eq!(limit["type"], "integer");
    assert!(
        limit["locked"].is_object(),
        "the engine locks the doc limit"
    );
    limit["key"] = json!("model_context_window");
    limit["label"] = json!("Context window");
    limit["help"] = json!("Fixture: the most tokens the model reads at once.");
    limit["locked"] = Value::Null;
    catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap()
}

fn stored(values: &[(&str, Value)]) -> Stored {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// Builds what the engine reads for the fixture Codex catalog with the engine's own catalog
/// page and row functions. Inputs: the installed version, the scope, the app values and the
/// project values. Output: the joined read; a row set at this scope has version 1.
fn read(
    installed: Option<&str>,
    layer: &Layer,
    app: &[(&str, Value)],
    project: &[(&str, Value)],
) -> HarnessRead {
    read_with(codex(), installed, layer, app, project)
}

/// Builds what the engine reads for one catalog, as `read` does for the fixture. Inputs: the
/// catalog, the installed version, the scope, the app values and the project values.
/// Output: the joined read.
fn read_with(
    catalog: Catalog,
    installed: Option<&str>,
    layer: &Layer,
    app: &[(&str, Value)],
    project: &[(&str, Value)],
) -> HarnessRead {
    let (app, project) = (stored(app), stored(project));
    let here = match layer {
        Layer::App => &app,
        Layer::Project(_) => &project,
    };
    let settings = catalog
        .keys
        .iter()
        .map(|key| {
            let version = u64::from(here.contains_key(&key.key));
            setting(
                &catalog, installed, layer, &key.key, &app, &project, version,
            )
        })
        .collect();
    let page = catalog_page(&catalog, installed, None).unwrap();
    assert_eq!(page.next, None);
    HarnessRead {
        name: page.name,
        installed: page.installed,
        qualified: page.qualified,
        qualified_versions: page.qualified_versions,
        keys: page.keys,
        settings,
    }
}

fn arrival(query: HarnessSettingsQuery, result: Result<HarnessSettingsReply, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::HarnessSettings(Box::new(HarnessSettingsEvent {
            query,
            result: result.map_err(str::to_owned),
        })),
    ))))
}

fn requests(shell: &mut Shell) -> Vec<HarnessSettingsQuery> {
    let mut sent = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Settings(job) = request
            && let SettingsJob::HarnessSettings(query) = *job
        {
            sent.push(*query);
        }
    }
    sent
}

fn read_query(project: Option<ProjectId>) -> HarnessSettingsQuery {
    HarnessSettingsQuery::Read {
        harness: "codex".into(),
        project,
    }
}

/// Opens the page and answers its first read. Inputs: the scope, the window size and the
/// read. Output: the shell showing the page.
fn opened(scope: Scope, width: u16, height: u16, answer: HarnessRead) -> Shell {
    let project = match &scope {
        Scope::App => None,
        Scope::Project { id, .. } => Some(id.clone()),
    };
    let page = HarnessPage::new("codex".into(), "Codex".into(), scope);
    let mut shell = Shell::with(ctx(width, height), Box::new(page));
    let first = read_query(project);
    assert_eq!(requests(&mut shell), vec![first.clone()]);
    shell.arrive(arrival(first, Ok(HarnessSettingsReply::Read(answer))));
    assert_eq!(requests(&mut shell), vec![]);
    shell
}

fn app_page(installed: Option<&str>) -> Shell {
    let answer = read(
        installed,
        &Layer::App,
        &[("model_reasoning_effort", json!("high"))],
        &[],
    );
    opened(Scope::App, 80, 24, answer)
}

fn project_scope() -> Scope {
    Scope::Project {
        id: alpha(),
        name: "Fieldnotes Mobile".into(),
    }
}

fn project_page(width: u16, height: u16) -> Shell {
    let answer = read(
        QUALIFIED,
        &Layer::Project(alpha()),
        &[("model_reasoning_effort", json!("high"))],
        &[
            ("model_reasoning_effort", json!("low")),
            ("hide_agent_reasoning", json!(true)),
        ],
    );
    opened(project_scope(), width, height, answer)
}

fn screen(shell: &mut Shell) -> String {
    render(shell, 80, 24).join("\n")
}

/// The most rows a test walks to find a setting: the fixture catalog's rows and then some.
const ROWS_WALKED_MAX: usize = 16;

/// Chooses the setting with the given label the way the owner does: Home, then Down until
/// the marked row is it, so a test does not depend on the page's group order. Inputs: the
/// shell and the label. Output: none; panics when no row has the label.
fn to_row(shell: &mut Shell, label: &str) {
    let marked = format!("> {label} ");
    shell.key(Key::Home);
    for _ in 0..ROWS_WALKED_MAX {
        let shown = screen(shell);
        if shown
            .lines()
            .any(|line| line.trim_start().starts_with(&marked))
        {
            return;
        }
        shell.key(Key::Down);
    }
    panic!("no row is {label}:\n{}", screen(shell));
}

/// Asks to quit and reads whether the shell warns about the typed value, then stays.
/// Input: the shell. Output: true when the warning showed.
fn asks_before_quit(shell: &mut Shell) -> bool {
    shell.key(Key::Ctrl('q'));
    let warned = screen(shell).contains("The value you typed will be lost.");
    shell.key(Key::Esc);
    warned
}

fn press(shell: &mut Shell, keys: &[Key]) {
    for key in keys {
        shell.key(key.clone());
    }
}

fn typed(shell: &mut Shell, text: &str) {
    for character in text.chars() {
        shell.key(Key::Char(character));
    }
}

fn written(query: HarnessSettingsQuery, outcome: Written) -> Arrival {
    arrival(query, Ok(HarnessSettingsReply::Written(outcome)))
}

/// The row the engine answers a write with, from the engine's row function. Inputs: the
/// scope, the key, the app and project values after the write and the new version.
fn after(
    layer: &Layer,
    key: &str,
    app: &[(&str, Value)],
    project: &[(&str, Value)],
    version: u64,
) -> Setting {
    let catalog = codex();
    setting(
        &catalog,
        QUALIFIED,
        layer,
        key,
        &stored(app),
        &stored(project),
        version,
    )
}

#[test]
fn the_harness_page_renders_locked_lines_at_80_columns() {
    assert_eq!(Status::of("Locked"), Some(Status::Off));
    assert_eq!(Status::of("Read-only"), Some(Status::Off));
    let mut shell = app_page(QUALIFIED);
    press(&mut shell, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    assert_eq!(render(&mut shell, 80, 24), fixture("harness-page-80", &[]));
}

#[test]
fn the_app_page_keeps_every_codex_setting_in_view_at_80x24() {
    let mut shell = app_page(QUALIFIED);
    let shown = render(&mut shell, 80, 24).join("\n");
    for label in [
        "Hide agent reasoning",
        "Reasoning effort",
        "Status label",
        "Project doc limit",
        "Sandbox",
        "Approvals",
    ] {
        assert!(
            shown.lines().any(|line| line
                .trim_start()
                .trim_start_matches("> ")
                .starts_with(label)),
            "{label} is off the page:\n{shown}"
        );
    }
    assert!(
        shown.contains("Codex 0.51.0 is qualified."),
        "the version stays on the page:\n{shown}"
    );
    for title in ["Settings (3)", "Set by CrazyAgent (3)"] {
        assert!(shown.contains(title), "{title}:\n{shown}");
    }
}

#[test]
fn the_project_page_renders_overrides_and_app_only_rows_at_120_columns() {
    let mut shell = project_page(120, 36);
    assert_eq!(
        render(&mut shell, 120, 36),
        fixture("harness-page-project-120", &[])
    );
}

#[test]
fn set_and_reset_send_the_version_the_owner_read() {
    let mut shell = app_page(QUALIFIED);
    shell.key(Key::Char(' '));
    let flip = HarnessSettingsQuery::Set {
        harness: "codex".into(),
        project: None,
        key: "hide_agent_reasoning".into(),
        value: json!(true),
        version: 0,
    };
    assert_eq!(requests(&mut shell), vec![flip.clone()]);
    shell.key(Key::Down);
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![], "one request at a time");
    let saved = after(
        &Layer::App,
        "hide_agent_reasoning",
        &[
            ("hide_agent_reasoning", json!(true)),
            ("model_reasoning_effort", json!("high")),
        ],
        &[],
        1,
    );
    shell.arrive(written(flip, Written::Saved { setting: saved }));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Saved. Codex gets it the next time it starts."),
        "{shown}"
    );

    press(&mut shell, &[Key::Home]);
    let shown = screen(&mut shell);
    assert!(shown.contains("> Hide agent reasoning      On"), "{shown}");
    assert!(shown.contains("x Reset"), "{shown}");

    press(&mut shell, &[Key::Down, Key::Enter]);
    let shown = screen(&mut shell);
    assert!(
        shown.contains("> Reasoning effort          ← high →"),
        "{shown}"
    );
    assert!(shown.contains("←→ Pick  Enter Save  Esc Cancel"), "{shown}");
    press(&mut shell, &[Key::Right, Key::Right, Key::Left]);
    assert!(screen(&mut shell).contains("← minimal →"));
    shell.key(Key::Enter);
    let pick = HarnessSettingsQuery::Set {
        harness: "codex".into(),
        project: None,
        key: "model_reasoning_effort".into(),
        value: json!("minimal"),
        version: 1,
    };
    assert_eq!(requests(&mut shell), vec![pick.clone()]);
    let saved = after(
        &Layer::App,
        "model_reasoning_effort",
        &[("model_reasoning_effort", json!("minimal"))],
        &[],
        2,
    );
    shell.arrive(written(pick, Written::Saved { setting: saved }));
    assert!(screen(&mut shell).contains("> Reasoning effort          minimal"));

    shell.key(Key::Char('x'));
    let reset = HarnessSettingsQuery::Reset {
        harness: "codex".into(),
        project: None,
        key: "model_reasoning_effort".into(),
        version: 2,
    };
    assert_eq!(requests(&mut shell), vec![reset.clone()]);
    let back = after(&Layer::App, "model_reasoning_effort", &[], &[], 3);
    shell.arrive(written(reset, Written::Saved { setting: back }));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("> Reasoning effort          medium                  Default"),
        "{shown}"
    );
    shell.key(Key::Char('x'));
    assert_eq!(requests(&mut shell), vec![]);
    assert!(
        screen(&mut shell).contains("Nothing to reset. This setting already uses the default.")
    );

    shell.key(Key::Enter);
    shell.key(Key::Esc);
    shell.key(Key::Enter);
    shell.key(Key::Enter);
    assert_eq!(
        requests(&mut shell),
        vec![],
        "a choice left unchanged sends nothing"
    );
}

#[test]
fn the_project_page_writes_and_resets_the_project_override() {
    let mut shell = project_page(80, 24);
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Fieldnotes Mobile  /  Harnesses  /  Codex"),
        "{shown}"
    );
    press(&mut shell, &[Key::Down, Key::Char('x')]);
    let reset = HarnessSettingsQuery::Reset {
        harness: "codex".into(),
        project: Some(alpha()),
        key: "model_reasoning_effort".into(),
        version: 1,
    };
    assert_eq!(requests(&mut shell), vec![reset.clone()]);
    let back = after(
        &Layer::Project(alpha()),
        "model_reasoning_effort",
        &[("model_reasoning_effort", json!("high"))],
        &[("hide_agent_reasoning", json!(true))],
        2,
    );
    shell.arrive(written(reset, Written::Saved { setting: back }));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("> Reasoning effort          high                    App"),
        "{shown}"
    );
    shell.key(Key::Char('x'));
    assert_eq!(requests(&mut shell), vec![]);
    assert!(
        screen(&mut shell).contains("Nothing to reset. This setting already uses the app value.")
    );
}

#[test]
fn locked_and_read_only_rows_send_nothing_and_say_why() {
    let mut shell = app_page(QUALIFIED);
    press(&mut shell, &[Key::End, Key::Up]);
    for key in [Key::Enter, Key::Char(' '), Key::Char('x')] {
        shell.key(key);
        assert_eq!(requests(&mut shell), vec![]);
        let message = shell.ctx.message().unwrap();
        assert_eq!(message.lead, "CrazyAgent sets this one itself.");
        assert_eq!(message.rest, SANDBOX);
    }
    let shown = screen(&mut shell);
    assert!(
        shown.contains(" ↑↓ Move  r Read again  Esc All harnesses"),
        "{shown}"
    );
    assert!(!shown.contains("x Reset"), "{shown}");

    let mut shell = project_page(80, 24);
    to_row(&mut shell, "Status label");
    let shown = screen(&mut shell);
    assert!(
        shown.contains("> Status label              Empty                   ⊘ Read-only"),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("Read-only: {APP_ONLY}.")),
        "{shown}"
    );
    for key in [Key::Enter, Key::Char('x')] {
        shell.key(key);
        assert_eq!(requests(&mut shell), vec![]);
        let message = shell.ctx.message().unwrap();
        assert_eq!(message.lead, "This setting is read-only here.");
        assert_eq!(message.rest, format!("{APP_ONLY}."));
    }
}

#[test]
fn an_unqualified_version_shows_every_setting_read_only() {
    let answer = read(
        Some("0.49.0"),
        &Layer::App,
        &[("model_reasoning_effort", json!("high"))],
        &[],
    );
    let mut shell = opened(Scope::App, 120, 36, answer);
    let shown = render(&mut shell, 120, 36).join("\n");
    assert!(
        shown.contains("Codex 0.49.0 isn't qualified yet, so every setting is read-only"),
        "{shown}"
    );
    assert!(
        shown.contains("Qualified versions: 0.50.0, 0.51.0."),
        "{shown}"
    );
    let read_only = shown.matches("⊘ Read-only").count();
    let locked = shown.matches("⊘ Locked").count();
    assert_eq!((read_only, locked), (3, 3), "{shown}");
    to_row(&mut shell, "Hide agent reasoning");
    shell.key(Key::Char(' '));
    assert_eq!(requests(&mut shell), vec![]);
    assert_eq!(
        shell.ctx.message().unwrap().rest,
        "Not qualified for Codex 0.49.0."
    );

    let mut shell = app_page(None);
    assert!(screen(&mut shell).contains("CrazyAgent couldn't read the installed Codex version"));
    to_row(&mut shell, "Reasoning effort");
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![]);
    assert_eq!(
        shell.ctx.message().unwrap().rest,
        "Not qualified for Codex (version unknown)."
    );
}

#[test]
fn numbers_and_text_are_checked_before_they_are_sent() {
    let answer = read_with(
        open_number(),
        QUALIFIED,
        &Layer::App,
        &[("model_reasoning_effort", json!("high"))],
        &[],
    );
    let mut shell = opened(Scope::App, 80, 24, answer);
    press(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    let shown = screen(&mut shell);
    assert!(shown.contains("New value"), "{shown}");
    assert!(shown.contains("[ 32768█"), "{shown}");
    assert!(shown.contains(" Enter Save  Esc Cancel"), "{shown}");
    assert!(asks_before_quit(&mut shell));
    typed(&mut shell, "r");
    assert_eq!(requests(&mut shell), vec![], "letters type into the field");
    for (text, problem) in [
        ("abc", "Type a whole number."),
        ("-1", "Use a number from 0 to 1048576."),
        ("1048577", "Use a number from 0 to 1048576."),
    ] {
        shell.key(Key::Ctrl('u'));
        typed(&mut shell, text);
        shell.key(Key::Enter);
        assert_eq!(requests(&mut shell), vec![], "{text}");
        assert!(screen(&mut shell).contains(problem), "{text}");
    }
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, " 4096 ");
    shell.key(Key::Enter);
    assert!(!screen(&mut shell).contains("New value"));
    assert_eq!(
        requests(&mut shell),
        vec![HarnessSettingsQuery::Set {
            harness: "codex".into(),
            project: None,
            key: "model_context_window".into(),
            value: json!(4096),
            version: 0,
        }]
    );

    let mut shell = app_page(QUALIFIED);
    press(&mut shell, &[Key::Down, Key::Down, Key::Down, Key::Enter]);
    typed(&mut shell, &"é".repeat(17));
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![]);
    assert!(screen(&mut shell).contains("Use at most 32 bytes."));
    shell.key(Key::Esc);
    assert!(!screen(&mut shell).contains("New value"));
    shell.key(Key::Enter);
    typed(&mut shell, &"é".repeat(16));
    shell.key(Key::Enter);
    assert_eq!(
        requests(&mut shell),
        vec![HarnessSettingsQuery::Set {
            harness: "codex".into(),
            project: None,
            key: "tui.status_label".into(),
            value: json!("é".repeat(16)),
            version: 0,
        }]
    );
}

#[test]
fn a_stale_or_refused_write_shows_the_engine_row() {
    let mut shell = app_page(QUALIFIED);
    shell.key(Key::Char(' '));
    let flip = requests(&mut shell).remove(0);
    let other = HarnessSettingsQuery::Set {
        harness: "codex".into(),
        project: None,
        key: "hide_agent_reasoning".into(),
        value: json!(false),
        version: 0,
    };
    let current = after(
        &Layer::App,
        "hide_agent_reasoning",
        &[("hide_agent_reasoning", json!(true))],
        &[],
        5,
    );
    shell.arrive(written(
        other,
        Written::Saved {
            setting: current.clone(),
        },
    ));
    assert!(
        screen(&mut shell).contains("> Hide agent reasoning      Off"),
        "a reply to another query is ignored"
    );
    shell.key(Key::Char(' '));
    assert_eq!(
        requests(&mut shell),
        vec![],
        "the first write is still pending"
    );
    shell.arrive(written(flip, Written::Stale { setting: current }));
    let shown = screen(&mut shell);
    assert!(shown.contains("> Hide agent reasoning      On"), "{shown}");
    assert!(shown.contains("Changed somewhere else."), "{shown}");
    shell.key(Key::Char(' '));
    let again = requests(&mut shell);
    assert_eq!(
        again,
        vec![HarnessSettingsQuery::Set {
            harness: "codex".into(),
            project: None,
            key: "hide_agent_reasoning".into(),
            value: json!(false),
            version: 5,
        }]
    );
    let refused = after(
        &Layer::App,
        "sandbox_mode",
        &[("hide_agent_reasoning", json!(true))],
        &[],
        0,
    );
    shell.arrive(written(
        again[0].clone(),
        Written::Refused {
            code: "harness_settings_key_locked".into(),
            setting: refused,
        },
    ));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "CrazyAgent sets this one itself.");
    assert_eq!(message.rest, SANDBOX);

    shell.key(Key::Char(' '));
    let failed = requests(&mut shell).remove(0);
    shell.arrive(arrival(failed, Err("harness_settings_value_invalid")));
    let message = shell.ctx.message().unwrap();
    assert_eq!(message.lead, "Could not change the setting.");
    assert_eq!(message.rest, "This value is not allowed for this setting.");
}

#[test]
fn a_failed_read_offers_to_try_again() {
    let page = HarnessPage::new("codex".into(), "Codex".into(), Scope::App);
    let mut shell = Shell::with(ctx(80, 24), Box::new(page));
    let first = requests(&mut shell).remove(0);
    assert!(screen(&mut shell).contains("Reading the Codex settings"));
    shell.arrive(arrival(first, Err("harness_settings_unavailable")));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("Could not read the Codex settings. Harness settings are not available in"),
        "{shown}"
    );
    assert!(shown.contains("Press r to try again."), "{shown}");
    assert!(shown.contains("r Try again  Esc All harnesses"), "{shown}");
    shell.key(Key::Char('r'));
    let again = requests(&mut shell);
    assert_eq!(again, vec![read_query(None)]);
    assert!(screen(&mut shell).contains("Reading the Codex settings"));
    let answer = read(QUALIFIED, &Layer::App, &[], &[]);
    shell.arrive(arrival(
        again[0].clone(),
        Ok(HarnessSettingsReply::Read(answer)),
    ));
    shell.key(Key::Char('r'));
    let reread = requests(&mut shell).remove(0);
    shell.arrive(arrival(reread, Err("controller_pending_intent")));
    let shown = screen(&mut shell);
    assert!(
        shown.contains("> Hide agent reasoning"),
        "a failed reread keeps the rows: {shown}"
    );
    assert_eq!(
        shell.ctx.message().unwrap().lead,
        "Could not read the Codex settings."
    );
}

#[test]
fn values_and_kinds_read_plainly() {
    assert_eq!(shown(&json!(true)), "On");
    assert_eq!(shown(&json!(false)), "Off");
    assert_eq!(shown(&json!(32768)), "32768");
    assert_eq!(shown(&json!("")), "Empty");
    assert_eq!(shown(&Value::Null), "Not set");
    assert_eq!(shown(&json!("tab\there\u{1b}[2J")), "tab?here?[2J");
    assert_eq!(shown(&json!(["a", 1])), "[\"a\",1]");
    let catalog = crate::harness_settings::tests::fixture("codex");
    let sentences: Vec<String> = catalog.keys.iter().take(4).map(kind_sentence).collect();
    assert_eq!(
        sentences,
        [
            "On or off. Default: Off.",
            "One of minimal, low, medium, high. Default: medium.",
            "A whole number from 0 to 1048576. Default: 32768.",
            "One line of text, at most 32 bytes. Default: Empty.",
        ]
    );
    assert_eq!(
        message("harness_settings_key_app_only"),
        "This setting is set once for the whole app."
    );
    assert_eq!(
        message("harness_settings_no_catalog"),
        "CrazyAgent has no settings page for this harness yet."
    );
    assert_eq!(
        message("trusted_connection_unavailable"),
        "The engine connection is not ready yet."
    );
}

#[test]
fn the_page_stands_for_its_place() {
    let page = HarnessPage::new("codex".into(), "Codex".into(), project_scope());
    assert_eq!(
        page.place(),
        Some(Place::HarnessSettings {
            harness: "codex".into(),
            name: "Codex".into(),
            project: Some((alpha(), "Fieldnotes Mobile".into())),
        })
    );
    let mut context = ctx(80, 24);
    let opened = crate::tui::screens::open(
        &Place::HarnessSettings {
            harness: "codex".into(),
            name: "Codex".into(),
            project: None,
        },
        &mut context,
    );
    assert_eq!(opened.crumbs(), ["Settings", "Harnesses", "Codex"]);
    let list = crate::tui::screens::open(
        &Place::ProjectHarnesses {
            id: alpha(),
            name: "Fieldnotes Mobile".into(),
        },
        &mut context,
    );
    assert_eq!(list.crumbs(), ["Fieldnotes Mobile", "Harnesses"]);
}

#[test]
fn a_key_left_to_the_harness_default_reads_harness_default() {
    let mut document = crate::harness_settings::tests::fixture_json();
    document["keys"][1]["default"] = Value::Null;
    let parsed = catalog::parse(&serde_json::to_vec(&document).unwrap());
    assert!(
        parsed.is_ok(),
        "an unlocked key may leave its default to the harness"
    );
    let catalog = parsed.unwrap();
    let key = catalog.key("model_reasoning_effort").unwrap();
    assert_eq!(
        kind_sentence(key),
        "One of minimal, low, medium, high. Default: Harness default."
    );
    let empty = Stored::new();
    let settings = catalog
        .keys
        .iter()
        .map(|key| {
            setting(
                &catalog,
                QUALIFIED,
                &Layer::App,
                &key.key,
                &empty,
                &empty,
                0,
            )
        })
        .collect();
    let page = catalog_page(&catalog, QUALIFIED, None).unwrap();
    let answer = HarnessRead {
        name: page.name,
        installed: page.installed,
        qualified: page.qualified,
        qualified_versions: page.qualified_versions,
        keys: page.keys,
        settings,
    };
    let mut shell = opened(Scope::App, 80, 24, answer);
    let shown = screen(&mut shell);
    let effort = shown
        .lines()
        .find(|line| line.contains("Reasoning effort"))
        .unwrap_or_default();
    assert!(effort.contains("Harness default"), "{shown}");
    assert!(!effort.contains("Not set"), "{shown}");
    assert!(shown.contains("Sandbox"), "{shown}");
}

/// The help bar as an 80x24 window draws it, every row of it. Takes the shell. Returns the
/// bar rows joined by newlines.
fn bar_at_80(shell: &mut Shell) -> String {
    crate::tui::screens::settings_accounts::tests::bar_rows(&render(shell, 80, 24)).join("\n")
}

#[test]
fn every_harness_page_key_stays_in_the_hint_bar_at_80_columns() {
    for mut shell in [app_page(QUALIFIED), project_page(80, 24)] {
        let bar = bar_at_80(&mut shell);
        assert!(
            ["Space Turn on or off", "Enter Choose", "Enter Change"]
                .iter()
                .any(|change| bar.contains(change)),
            "the selected row's change key:\n{bar}"
        );
        for key in ["Move", "r Read again", "Esc All harnesses", "? Help"] {
            assert!(bar.contains(key), "{key}:\n{bar}");
        }
    }
}

#[test]
fn the_harness_page_reads_in_the_desktop_words() {
    let answer = read(QUALIFIED, &Layer::App, &[], &[]);
    let count = |pick: fn(&Access) -> bool| {
        answer
            .settings
            .iter()
            .filter(|setting| pick(&setting.access))
            .count()
    };
    let editable = count(|access| matches!(access, Access::Editable));
    let locked = count(|access| matches!(access, Access::Locked { .. }));
    let read_only = count(|access| matches!(access, Access::ReadOnly { .. }));
    assert!(editable > 0 && locked > 0, "the fixture has both groups");
    let mut shell = opened(Scope::App, 120, 40, answer);
    let shown = render(&mut shell, 120, 40).join("\n");
    for words in [
        "What every master and agent using this harness starts with, in every project.",
        "Codex 0.51.0 is qualified. You can change the settings below.",
        "Changes apply the next time a master or agent starts with this harness. Running ones keep their settings.",
    ] {
        assert!(shown.contains(words), "{words}:\n{shown}");
    }
    for (title, size) in [
        ("Settings", editable),
        ("Set by CrazyAgent", locked),
        ("Read-only", read_only),
    ] {
        let heading = format!("{title} ({size})");
        assert_eq!(shown.contains(&heading), size > 0, "{heading}:\n{shown}");
    }
    assert!(
        !shown.contains("Codex settings"),
        "the title is the harness's name:\n{shown}"
    );

    let mut project = project_page(120, 36);
    let shown = render(&mut project, 120, 36).join("\n");
    assert!(
        shown.contains("wins over the app value for this project only."),
        "{shown}"
    );
}

#[test]
fn the_settings_of_one_group_sit_together_under_its_title() {
    let mut shell = opened(Scope::App, 120, 40, read(QUALIFIED, &Layer::App, &[], &[]));
    let rows = render(&mut shell, 120, 40);
    let at = |title: &str| {
        rows.iter()
            .position(|row| row.trim_start().starts_with(title))
            .unwrap_or_else(|| panic!("{title}:\n{}", rows.join("\n")))
    };
    let (settings, locked) = (at("Settings ("), at("Set by CrazyAgent ("));
    assert!(settings < locked, "{}", rows.join("\n"));
    assert!(
        rows[locked + 1..]
            .iter()
            .take_while(|row| row.contains("⊘ Locked"))
            .count()
            > 0,
        "the locked rows follow their title:\n{}",
        rows.join("\n")
    );
    assert!(
        rows[settings..locked]
            .iter()
            .all(|row| !row.contains("⊘ Locked")),
        "the Settings group holds no locked row:\n{}",
        rows.join("\n")
    );
}
