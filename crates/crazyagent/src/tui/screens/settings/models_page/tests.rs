use super::*;
use crate::engine::owner::MODEL_CHOICE_PROTOCOL_CODES;
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings::harness_page::HarnessPage;
use crate::tui::screens::settings_accounts::tests::{ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;
use serde_json::json;

const HARNESS: &str = "claude-code";

fn alpha() -> ProjectId {
    ProjectId::new("alpha").unwrap()
}

fn project_scope() -> Scope {
    Scope::Project {
        id: alpha(),
        name: "Fieldnotes Mobile".into(),
    }
}

fn row(slot: Slot, here: Option<Value>, effective: Value, source: Source) -> Row {
    Row {
        slot,
        here,
        effective,
        source,
        version: 4,
        access: Access::Editable,
    }
}

fn unset(slot: Slot) -> Row {
    let effective = match slot {
        Slot::Allowed => json!([]),
        Slot::Native => json!("follow"),
        _ => Value::Null,
    };
    row(slot, None, effective, Source::Default)
}

/// The Claude Code section with the shipped list and the owner ids given, its Native
/// sub-agent model unset. Inputs: the three model rows and the owner ids. Output: the
/// section.
fn models(rows: [Row; 3], owned: &[(&str, Origin)]) -> Models {
    with_native(rows, unset(Slot::Native), owned)
}

/// The Claude Code section with the shipped list, the owner ids and the Native sub-agent
/// model row given. Inputs: the three model rows, the native row and the owner ids.
/// Output: the section.
fn with_native(rows: [Row; 3], native: Row, owned: &[(&str, Origin)]) -> Models {
    assert_eq!(native.slot, Slot::Native);
    let mut candidates: Vec<Candidate> = crate::model_choice::catalog(HARNESS)
        .into_iter()
        .map(|id| Candidate {
            id,
            origin: Origin::Catalog,
        })
        .collect();
    candidates.extend(owned.iter().map(|(id, origin)| Candidate {
        id: (*id).into(),
        origin: *origin,
    }));
    Models {
        harness: HARNESS.into(),
        rows: rows.into_iter().chain([native]).collect(),
        candidates,
        qualified: true,
        reason: None,
    }
}

fn plain_models() -> Models {
    models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &[],
    )
}

fn rules(pick: PickRule, fallback: FallbackRule) -> Rules {
    Rules {
        pick,
        pick_version: 2,
        fallback,
        fallback_version: 5,
    }
}

fn arrival(query: ModelChoiceQuery, result: Result<ModelChoiceReply, &str>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::ModelChoice(Box::new(ModelChoiceEvent {
            query,
            result: result.map_err(str::to_owned),
        })),
    ))))
}

fn requests(shell: &mut Shell) -> Vec<ModelChoiceQuery> {
    let mut sent = Vec::new();
    while let Some(request) = shell.ctx.next_request() {
        if let Request::Settings(job) = request
            && let SettingsJob::ModelChoice(query) = *job
        {
            sent.push(*query);
        }
    }
    sent
}

fn read_query(project: Option<ProjectId>) -> ModelChoiceQuery {
    ModelChoiceQuery::Read {
        harness: HARNESS.into(),
        project,
    }
}

/// Opens the page and answers its first read. Inputs: the scope and the section; the app
/// page also gets the rules. Output: the shell showing the page.
fn opened(scope: Scope, section: Models) -> Shell {
    let project = match &scope {
        Scope::App => None,
        Scope::Project { id, .. } => Some(id.clone()),
    };
    let app = project.is_none();
    let page = ModelsPage::new(HARNESS.into(), "Claude Code".into(), scope);
    let mut shell = Shell::with(ctx(80, 24), Box::new(page));
    let first = read_query(project);
    assert_eq!(requests(&mut shell), vec![first.clone()]);
    let reply = ModelChoiceReply::Read {
        models: section,
        rules: app.then(|| rules(PickRule::Strict, FallbackRule::Notice)),
    };
    shell.arrive(arrival(first, Ok(reply)));
    assert_eq!(requests(&mut shell), vec![]);
    shell
}

fn app_page(section: Models) -> Shell {
    opened(Scope::App, section)
}

fn screen(shell: &mut Shell) -> String {
    render(shell, 80, 24).join("\n")
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

fn lead(shell: &Shell) -> String {
    shell
        .ctx
        .message()
        .map(|message| message.lead.clone())
        .unwrap_or_default()
}

#[test]
fn the_app_page_reads_the_section_and_shows_every_slot_rule_and_the_list_it_was_checked_against() {
    let mut shell = app_page(plain_models());
    let text = screen(&mut shell);
    for words in [
        "Claude Code models",
        "Checked against 2.1.309",
        "Master model",
        "Agent default",
        "Allowed models",
        "Pick rule",
        "Fallback rule",
        "Add a model id",
        "Account's own model",
        "Only the allowed list",
        "Tell me",
    ] {
        assert!(text.contains(words), "{words}\n{text}");
    }
}

#[test]
fn enter_and_the_arrows_choose_a_master_model_and_enter_sends_it_with_the_version_read() {
    let mut shell = app_page(plain_models());
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Set {
            harness: HARNESS.into(),
            project: None,
            slot: Slot::Master,
            value: json!(crate::model_choice::catalog(HARNESS)[0]),
            version: 4,
        }]
    );
}

#[test]
fn choosing_the_account_s_own_model_sends_a_reset_never_a_null() {
    let section = models(
        [
            row(
                Slot::Master,
                Some(json!("opus")),
                json!("opus"),
                Source::App,
            ),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &[],
    );
    let mut shell = app_page(section);
    press(&mut shell, &[Key::Enter]);
    assert!(
        screen(&mut shell).contains("opus"),
        "the choice starts on the stored id"
    );
    let count = crate::model_choice::catalog(HARNESS).len();
    let at = crate::model_choice::catalog(HARNESS)
        .iter()
        .position(|id| id == "opus")
        .unwrap();
    for _ in 0..(at + 1) {
        shell.key(Key::Left);
    }
    assert!(count > at);
    shell.key(Key::Enter);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Reset {
            harness: HARNESS.into(),
            project: None,
            slot: Slot::Master,
            version: 4,
        }]
    );
    press(&mut shell, &[Key::Down, Key::Enter, Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![],
        "an unchanged choice sends nothing"
    );
}

#[test]
fn x_resets_a_stored_slot_and_says_there_is_nothing_to_reset_otherwise() {
    let section = models(
        [
            unset(Slot::Master),
            row(
                Slot::Agent,
                Some(json!("sonnet")),
                json!("sonnet"),
                Source::App,
            ),
            unset(Slot::Allowed),
        ],
        &[],
    );
    let mut shell = app_page(section);
    shell.key(Key::Char('x'));
    assert_eq!(requests(&mut shell), vec![]);
    assert_eq!(lead(&shell), "Nothing to reset.");
    press(&mut shell, &[Key::Down, Key::Char('x')]);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Reset {
            harness: HARNESS.into(),
            project: None,
            slot: Slot::Agent,
            version: 4,
        }]
    );
}

#[test]
fn space_ticks_allowed_models_and_enter_sends_them_in_page_order() {
    let mut shell = app_page(plain_models());
    press(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    assert!(
        screen(&mut shell).contains("[ ] opus"),
        "{}",
        screen(&mut shell)
    );
    let ids = crate::model_choice::catalog(HARNESS);
    press(
        &mut shell,
        &[
            Key::Down,
            Key::Char(' '),
            Key::Up,
            Key::Char(' '),
            Key::Enter,
        ],
    );
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Set {
            harness: HARNESS.into(),
            project: None,
            slot: Slot::Allowed,
            value: json!([ids[0], ids[1]]),
            version: 4,
        }]
    );
}

#[test]
fn a_pending_or_failed_id_cannot_be_ticked_and_a_seventeenth_does_not_fit() {
    let owned = [("mine-a", Origin::Pending), ("mine-c", Origin::Failed)];
    let ids = crate::model_choice::catalog(HARNESS);
    let section = models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &owned,
    );
    let mut shell = app_page(section);
    press(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    for _ in 0..ids.len() {
        shell.key(Key::Down);
    }
    let text = screen(&mut shell);
    assert!(
        text.contains("mine-a") && text.contains("Waiting for its test launch"),
        "{text}"
    );
    shell.key(Key::Char(' '));
    assert_eq!(lead(&shell), "mine-a can't be ticked yet.");
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    assert_eq!(lead(&shell), "mine-c can't be ticked.");
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![], "nothing ticked, nothing sent");

    let many: Vec<(String, Origin)> = (0..ALLOWED_MAX + 1)
        .map(|n| (format!("own-{n:02}"), Origin::Verified))
        .collect();
    let owned: Vec<(&str, Origin)> = many
        .iter()
        .map(|(id, origin)| (id.as_str(), *origin))
        .collect();
    let mut shell = app_page(models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &owned,
    ));
    press(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    for _ in 0..ALLOWED_MAX {
        press(&mut shell, &[Key::Char(' '), Key::Down]);
    }
    shell.key(Key::Char(' '));
    assert_eq!(lead(&shell), "At most 16 models can be ticked.");
}

#[test]
fn unticking_every_model_on_a_project_sends_an_own_empty_list() {
    let section = models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            row(Slot::Allowed, None, json!(["opus"]), Source::App),
        ],
        &[],
    );
    let mut shell = opened(project_scope(), section);
    let text = screen(&mut shell);
    assert!(
        !text.contains("Pick rule") && !text.contains("Add a model id"),
        "{text}"
    );
    press(&mut shell, &[Key::Down, Key::Down, Key::Enter]);
    let at = crate::model_choice::catalog(HARNESS)
        .iter()
        .position(|id| id == "opus")
        .unwrap();
    for _ in 0..at {
        shell.key(Key::Down);
    }
    press(&mut shell, &[Key::Char(' '), Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Set {
            harness: HARNESS.into(),
            project: Some(alpha()),
            slot: Slot::Allowed,
            value: json!([]),
            version: 4,
        }]
    );
}

#[test]
fn the_project_page_offers_the_app_value_and_says_where_each_value_comes_from() {
    let section = models(
        [
            row(Slot::Master, None, json!("opus"), Source::App),
            row(
                Slot::Agent,
                Some(json!("sonnet")),
                json!("sonnet"),
                Source::Project,
            ),
            unset(Slot::Allowed),
        ],
        &[],
    );
    let mut shell = opened(project_scope(), section);
    let text = screen(&mut shell);
    assert!(
        text.contains("Claude Code models for Fieldnotes Mobile"),
        "{text}"
    );
    assert!(text.contains("App") && text.contains("Project"), "{text}");
    press(&mut shell, &[Key::Enter]);
    assert!(
        screen(&mut shell).contains(APP_VALUE),
        "the first choice keeps the app value"
    );
    press(&mut shell, &[Key::Esc, Key::Down, Key::Char('x')]);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Reset {
            harness: HARNESS.into(),
            project: Some(alpha()),
            slot: Slot::Agent,
            version: 4,
        }]
    );
}

#[test]
fn typing_an_id_adds_it_and_an_id_the_rule_refuses_never_leaves() {
    let mut shell = app_page(plain_models());
    for _ in 0..6 {
        shell.key(Key::Down);
    }
    shell.key(Key::Enter);
    assert!(shell.typing(), "the add field takes typing");
    typed(&mut shell, "bad id!");
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![]);
    assert!(
        screen(&mut shell).contains("Use letters, digits and - _ . : / @ +"),
        "{}",
        screen(&mut shell)
    );
    for _ in 0.."bad id!".len() {
        shell.key(Key::Backspace);
    }
    typed(&mut shell, "my-model-1");
    shell.key(Key::Enter);
    let add = ModelChoiceQuery::Add {
        harness: HARNESS.into(),
        model: "my-model-1".into(),
    };
    assert_eq!(requests(&mut shell), vec![add.clone()]);
    let after = models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &[("my-model-1", Origin::Pending)],
    );
    shell.arrive(arrival(
        add,
        Ok(ModelChoiceReply::Added(ModelAdded::Pending, after)),
    ));
    assert_eq!(lead(&shell), "Added my-model-1.");
    assert!(screen(&mut shell).contains("Waiting for its test launch"));
}

#[test]
fn x_on_an_own_id_removes_it() {
    let section = models(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        &[("my-model-1", Origin::Pending)],
    );
    let mut shell = app_page(section);
    for _ in 0..7 {
        shell.key(Key::Down);
    }
    shell.key(Key::Char('x'));
    let remove = ModelChoiceQuery::Remove {
        harness: HARNESS.into(),
        model: "my-model-1".into(),
    };
    assert_eq!(requests(&mut shell), vec![remove.clone()]);
    shell.arrive(arrival(
        remove,
        Ok(ModelChoiceReply::Removed(true, plain_models())),
    ));
    assert_eq!(lead(&shell), "Removed my-model-1.");
    assert!(
        !screen(&mut shell).contains("Yours"),
        "no owner id row is left"
    );
}

#[test]
fn each_pick_and_fallback_option_sends_its_rule_with_the_version_read() {
    let mut shell = app_page(plain_models());
    let mut current = rules(PickRule::Strict, FallbackRule::Notice);
    press(&mut shell, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    let steps = [
        (Rule::Pick(PickRule::Hint), "Any offered model"),
        (Rule::Pick(PickRule::Default), "Always the default"),
        (Rule::Pick(PickRule::Strict), "Only the allowed list"),
        (Rule::Fallback(FallbackRule::Stop), "Stop and say why"),
        (Rule::Fallback(FallbackRule::Silent), "Quietly"),
        (Rule::Fallback(FallbackRule::Notice), "Tell me"),
    ];
    for (index, (rule, words)) in steps.into_iter().enumerate() {
        if index == 3 {
            shell.key(Key::Down);
        }
        press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
        let version = match rule {
            Rule::Pick(_) => current.pick_version,
            Rule::Fallback(_) => current.fallback_version,
        };
        let sent = requests(&mut shell);
        assert_eq!(
            sent,
            vec![ModelChoiceQuery::SetRule { rule, version }],
            "{words}"
        );
        press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
        assert_eq!(requests(&mut shell), vec![], "one request at a time");
        match rule {
            Rule::Pick(pick) => {
                current.pick = pick;
                current.pick_version += 1;
            }
            Rule::Fallback(fallback) => {
                current.fallback = fallback;
                current.fallback_version += 1;
            }
        }
        shell.arrive(arrival(
            sent[0].clone(),
            Ok(ModelChoiceReply::RuleWritten(RuleWritten::Saved {
                rules: current.clone(),
            })),
        ));
        assert_eq!(lead(&shell), "Saved.");
        assert!(screen(&mut shell).contains(words), "{words}");
    }
}

#[test]
fn a_harness_whose_model_is_not_chosen_shows_the_reason_and_sends_no_set() {
    let read_only = |slot| Row {
        access: Access::ReadOnly {
            reason: NOT_SUPPORTED.into(),
        },
        source: Source::NotApplied,
        ..unset(slot)
    };
    let section = Models {
        harness: "opencode".into(),
        rows: vec![
            read_only(Slot::Master),
            read_only(Slot::Agent),
            read_only(Slot::Allowed),
            read_only(Slot::Native),
        ],
        candidates: vec![],
        qualified: true,
        reason: None,
    };
    let page = ModelsPage::new("opencode".into(), "OpenCode".into(), Scope::App);
    let mut shell = Shell::with(ctx(80, 24), Box::new(page));
    let first = ModelChoiceQuery::Read {
        harness: "opencode".into(),
        project: None,
    };
    assert_eq!(requests(&mut shell), vec![first.clone()]);
    shell.arrive(arrival(
        first,
        Ok(ModelChoiceReply::Read {
            models: section,
            rules: Some(rules(PickRule::Strict, FallbackRule::Notice)),
        }),
    ));
    let text = screen(&mut shell);
    assert!(text.contains("No model list"), "{text}");
    assert!(text.contains(NOT_SUPPORTED), "{text}");
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![]);
    assert_eq!(lead(&shell), "Not supported by this harness.");
}

#[test]
fn a_harness_version_the_model_list_was_not_checked_on_shows_the_reason_and_still_sends() {
    let reason = "listed for Claude Code 2.1.309, 2.1.322; installed 2.1.330 not yet checked";
    let mut shell = app_page(Models {
        qualified: false,
        reason: Some(reason.into()),
        ..plain_models()
    });
    let text = screen(&mut shell);
    assert!(
        text.contains("Version not checked: listed for Claude Code"),
        "{text}"
    );
    assert!(text.contains("Add a model id"), "{text}");
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    assert_eq!(requests(&mut shell).len(), 1, "the ids still save");
    let mut shell = app_page(Models {
        qualified: false,
        reason: Some("x".repeat(crate::model_choice::UNQUALIFIED_REASON_BYTES_MAX + 1)),
        ..plain_models()
    });
    let text = screen(&mut shell);
    assert!(
        text.contains("Could not read the Claude Code models."),
        "{text}"
    );
}

#[test]
fn a_stale_or_refused_write_replaces_the_row_and_says_why() {
    let mut shell = app_page(plain_models());
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    let sent = requests(&mut shell);
    let newer = row(
        Slot::Master,
        Some(json!("sonnet")),
        json!("sonnet"),
        Source::App,
    );
    shell.arrive(arrival(
        sent[0].clone(),
        Ok(ModelChoiceReply::Written(Written::Stale { row: newer })),
    ));
    assert_eq!(lead(&shell), "Changed somewhere else.");
    assert!(screen(&mut shell).contains("sonnet"));
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    let sent = requests(&mut shell);
    shell.arrive(arrival(sent[0].clone(), Err("model_choice_value_invalid")));
    assert_eq!(lead(&shell), "Could not change the model choice.");
}

#[test]
fn every_model_choice_code_has_its_own_words() {
    for code in MODEL_CHOICE_PROTOCOL_CODES {
        let words = message(code);
        assert_ne!(words, "The engine could not do this.", "{code}");
        assert!(!words.contains('_'), "{code}: {words}");
        assert!(!words.contains('\u{2014}'), "{code}");
    }
}

#[test]
fn the_page_stands_for_its_place_and_m_on_the_harness_page_opens_it() {
    let page = ModelsPage::new(HARNESS.into(), "Claude Code".into(), project_scope());
    let place = Place::Models {
        harness: HARNESS.into(),
        name: "Claude Code".into(),
        project: Some((alpha(), "Fieldnotes Mobile".into())),
    };
    assert_eq!(page.place(), Some(place.clone()));
    let mut context = ctx(80, 24);
    let mut harness = HarnessPage::new(HARNESS.into(), "Claude Code".into(), project_scope());
    match harness.key(&Key::Char('m'), &mut context) {
        Flow::Open(opened) => assert_eq!(opened, place),
        other => panic!("expected the Models page, got {other:?}"),
    }
    let opened = crate::tui::screens::open(&place, &mut context);
    assert_eq!(opened.place(), Some(place));
}

fn native_query(project: Option<ProjectId>, value: &str) -> ModelChoiceQuery {
    ModelChoiceQuery::Set {
        harness: HARNESS.into(),
        project,
        slot: Slot::Native,
        value: json!(value),
        version: 4,
    }
}

fn to_native(shell: &mut Shell) {
    press(shell, &[Key::Down, Key::Down, Key::Down]);
}

/// M-11 (DEC-150): the app page shows the Native sub-agent model on Follow the agent's
/// model with nothing set, with its help and where the value comes from in the desktop
/// app's words.
#[test]
fn the_app_page_shows_the_native_sub_agent_model_on_follow_the_agent_s_model() {
    let mut shell = app_page(plain_models());
    to_native(&mut shell);
    let text = screen(&mut shell);
    for words in [
        "Native sub-agent model",
        "Follow the agent's model",
        "What the sub-agents a native lead starts itself run on.",
        "Nothing set: native sub-agents follow the agent's model.",
    ] {
        assert!(text.contains(words), "{words}\n{text}");
    }
}

/// M-11 (DEC-150), Let the harness choose: Enter, Right and Enter send it with the version
/// read.
#[test]
fn enter_and_the_arrows_choose_let_the_harness_choose_and_enter_sends_it() {
    let mut shell = app_page(plain_models());
    to_native(&mut shell);
    press(&mut shell, &[Key::Enter, Key::Right]);
    let text = screen(&mut shell);
    assert!(text.contains("Let the harness choose"), "{text}");
    assert!(
        text.contains("The harness picks each sub-agent's model"),
        "the option's help shows:\n{text}"
    );
    shell.key(Key::Enter);
    assert_eq!(requests(&mut shell), vec![native_query(None, "harness")]);
}

/// M-11 (DEC-150), Follow the agent's model: from a saved Let the harness choose, the
/// choice starts on it and one Right sends Follow the agent's model as its own value.
#[test]
fn choosing_follow_the_agent_s_model_after_let_the_harness_choose_sends_follow() {
    let section = with_native(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        row(
            Slot::Native,
            Some(json!("harness")),
            json!("harness"),
            Source::App,
        ),
        &[],
    );
    let mut shell = app_page(section);
    to_native(&mut shell);
    assert!(screen(&mut shell).contains("Let the harness choose"));
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    assert_eq!(requests(&mut shell), vec![native_query(None, "follow")]);
    press(&mut shell, &[Key::Enter, Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![],
        "an unchanged choice sends nothing"
    );
}

/// M-11 (DEC-150): a project page offers Use the app value first; a project's own choice
/// starts on itself and choosing the app value sends a reset, never a null.
#[test]
fn a_project_page_offers_the_app_value_for_the_native_sub_agent_model_and_sends_a_reset() {
    let inherit = with_native(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        row(Slot::Native, None, json!("harness"), Source::App),
        &[],
    );
    let mut shell = opened(project_scope(), inherit);
    to_native(&mut shell);
    let text = screen(&mut shell);
    assert!(
        text.contains("Uses the app value: Let the harness choose."),
        "{text}"
    );
    press(&mut shell, &[Key::Enter, Key::Right, Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![native_query(Some(alpha()), "follow")]
    );

    let own = with_native(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        row(
            Slot::Native,
            Some(json!("follow")),
            json!("follow"),
            Source::Project,
        ),
        &[],
    );
    let mut shell = opened(project_scope(), own);
    to_native(&mut shell);
    press(&mut shell, &[Key::Enter, Key::Left, Key::Enter]);
    assert_eq!(
        requests(&mut shell),
        vec![ModelChoiceQuery::Reset {
            harness: HARNESS.into(),
            project: Some(alpha()),
            slot: Slot::Native,
            version: 4,
        }]
    );
}

/// M-11 (DEC-150): a harness without native sub-agents shows Not supported by this harness
/// on the Native sub-agent model row, never the stored word, and Enter changes nothing.
#[test]
fn a_harness_without_native_sub_agents_shows_the_native_row_not_supported() {
    let read_only = Row {
        access: Access::ReadOnly {
            reason: NOT_SUPPORTED.into(),
        },
        source: Source::NotApplied,
        ..unset(Slot::Native)
    };
    let section = with_native(
        [
            unset(Slot::Master),
            unset(Slot::Agent),
            unset(Slot::Allowed),
        ],
        read_only,
        &[],
    );
    let mut shell = app_page(section);
    to_native(&mut shell);
    let text = screen(&mut shell);
    assert!(text.contains(NOT_SUPPORTED), "{text}");
    assert!(!text.contains("Follow the agent's model"), "{text}");
    shell.key(Key::Enter);
    assert_eq!(lead(&shell), "Not supported by this harness.");
    assert_eq!(requests(&mut shell), vec![]);
}
