use super::new_project::*;
use crate::config::{EffectiveConfig, resolve_config_bytes};
use crate::project_setup::{Platform, ResearchKind, TeamPlan, catalog};
use crate::protocol::CredentialRef;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeSet;

const POOLS: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-api", "account-codex", "account-claude"]
[[model_access.connections]]
id = "a-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "none" }
[[model_access.connections]]
id = "b-claude"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "model-claude", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
[[model_access.connections]]
id = "c-codex"
provider = "provider-codex"
harness = "codex"
protocol = "native"
account_ref = "account-codex"
endpoint = { kind = "native" }
model = { id = "model-codex", author = "author-codex" }
auth = { method = "native_managed", profile_ref = "profile-codex" }
[[model_access.pools]]
id = "pool-main"
name = "Main"
members = ["a-api", "b-claude", "c-codex"]
projects = []
roles = []
fallback_allowed = false
[[model_access.pools]]
id = "pool-local"
members = ["a-api"]
projects = []
roles = []
fallback_allowed = false
"#;

pub(super) fn config() -> EffectiveConfig {
    resolve_config_bytes(POOLS, "").unwrap()
}

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

pub(super) fn control(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

pub(super) fn type_text(wizard: &mut Wizard, text: &str) {
    for character in text.chars() {
        assert_eq!(wizard.key(key(KeyCode::Char(character))), Reply::Handled);
    }
}

pub(super) fn wizard() -> Wizard {
    Wizard::new(pool_choices(&config()), None)
}

pub(super) fn load_catalog(wizard: &mut Wizard, signed_in: &[&str]) {
    let config = config();
    let signed: BTreeSet<CredentialRef> = signed_in
        .iter()
        .map(|id| CredentialRef::new(*id).unwrap())
        .collect();
    let (pool, research) = wizard.catalog_request().expect("catalog request");
    let served = catalog(&config, pool.as_ref(), &research, &signed).unwrap();
    wizard.apply_catalog(pool, research, Ok(served));
}

#[test]
fn pools_come_from_settings_with_plain_names_and_harness_counts() {
    let pools = pool_choices(&config());
    assert_eq!(
        pools
            .iter()
            .map(|pool| (pool.name.as_str(), pool.accounts))
            .collect::<Vec<_>>(),
        [("Main", 3), ("Unnamed pool", 1)]
    );
    assert_eq!(pools[0].accounts_for("codex"), 1);
    assert_eq!(pools[0].accounts_for("opencode"), 0);
    let wizard = wizard();
    assert_eq!(wizard.pool_name(), Some("Main"));
}

#[test]
fn about_needs_a_name_and_continue_marks_the_step_done() {
    let mut wizard = wizard();
    assert_eq!(wizard.focused(), Control::Name);
    assert!(wizard.typing);
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(
        wizard
            .note
            .as_ref()
            .map(|n| (n.control, n.text.as_str(), n.error)),
        Some((Control::Name, "Give the project a name.", true))
    );
    type_text(&mut wizard, "  Tidepool");
    assert_eq!(wizard.note, None);
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.focused(), Control::About);
    type_text(&mut wizard, "Live buoys.");
    wizard.key(control('j'));
    type_text(&mut wizard, "Alerts?");
    assert_eq!(wizard.about.value(), "Live buoys.\nAlerts?");
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(wizard.step, Step::Stack);
    assert!(wizard.done[Step::About.index()]);
}

#[test]
fn leaving_an_empty_name_shows_the_error_under_it() {
    let mut wizard = wizard();
    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.focused(), Control::About);
    assert_eq!(wizard.note.as_ref().map(|n| n.control), Some(Control::Name));
    wizard.key(key(KeyCode::BackTab));
    wizard.key(key(KeyCode::BackTab));
    assert_eq!(wizard.focused(), Control::Continue);
    assert!(!wizard.typing);
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.step, Step::About);
    assert_eq!(wizard.focused(), Control::Name);
}

#[test]
fn esc_leaves_the_field_first_and_then_keeps_the_draft() {
    let mut wizard = wizard();
    type_text(&mut wizard, "Tidepool?");
    assert_eq!(wizard.name.value(), "Tidepool?");
    assert_eq!(wizard.key(key(KeyCode::Esc)), Reply::Handled);
    assert!(!wizard.typing);
    assert_eq!(wizard.key(key(KeyCode::Char('?'))), Reply::Ignored);
    assert_eq!(wizard.key(key(KeyCode::Char(']'))), Reply::Handled);
    assert_eq!(wizard.step, Step::Stack);
    assert_eq!(wizard.key(key(KeyCode::Esc)), Reply::Leave);
    assert_eq!(wizard.name.value(), "Tidepool?");
}

#[test]
fn shift_arrows_and_brackets_move_between_steps_and_wrap() {
    let mut wizard = wizard();
    type_text(&mut wizard, "[x]");
    assert_eq!(wizard.name.value(), "[x]");
    assert_eq!(wizard.step, Step::About);
    wizard.key(shift(KeyCode::Right));
    assert_eq!(wizard.step, Step::Stack);
    assert!(!wizard.done[Step::About.index()]);
    wizard.key(shift(KeyCode::Left));
    wizard.key(shift(KeyCode::Left));
    assert_eq!(wizard.step, Step::Review);
    assert_eq!(wizard.focused(), Control::Continue);
    wizard.key(key(KeyCode::Char(']')));
    assert_eq!(wizard.step, Step::About);
    wizard.key(key(KeyCode::Char('[')));
    assert_eq!(wizard.step, Step::About);
    assert_eq!(wizard.name.value(), "[x][");
}

#[test]
fn team_defaults_come_from_the_engine_and_owner_edits_survive_a_reload() {
    let mut wizard = wizard();
    wizard.research = vec![
        ResearchKind::CompetitorResearch,
        ResearchKind::MarketResearch,
    ];
    load_catalog(&mut wizard, &["c-codex"]);
    let team = wizard.team.clone().unwrap();
    assert_eq!(team.master, "codex");
    assert_eq!(
        team.sub_harnesses
            .iter()
            .map(|s| (s.harness.as_str(), s.agents, s.tools))
            .collect::<Vec<_>>(),
        [("codex", 2, true)]
    );
    assert_eq!((team.min_agents, team.max_agents), (1, 3));
    assert_eq!((wizard.min.value(), wizard.max.value()), ("1", "3"));
    assert_eq!(wizard.catalog_request(), None);

    wizard.research.push(ResearchKind::DesignResearch);
    load_catalog(&mut wizard, &["c-codex"]);
    assert_eq!(wizard.team.as_ref().unwrap().sub_harnesses[0].agents, 3);
    assert_eq!(wizard.max.value(), "4");

    wizard.go(Step::Team);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Left));
    assert!(wizard.team_edited);
    wizard.research.pop();
    load_catalog(&mut wizard, &[]);
    let kept = wizard.team.clone().unwrap();
    assert_eq!(kept.master, "codex");
    assert_eq!(kept.sub_harnesses[0].agents, 2);
    assert_eq!(kept.max_agents, 4);

    let mut unsigned = Wizard::new(pool_choices(&config()), None);
    load_catalog(&mut unsigned, &[]);
    let fallback = unsigned.team.unwrap();
    assert_eq!(fallback.master, "rcli");
    assert!(fallback.sub_harnesses.is_empty());
    assert_eq!((fallback.min_agents, fallback.max_agents), (1, 1));
}

#[test]
fn a_catalog_failure_keeps_the_draft_and_reports_the_problem() {
    let mut wizard = wizard();
    let (pool, research) = wizard.catalog_request().unwrap();
    assert_eq!(wizard.catalog_request(), None);
    wizard.apply_catalog(pool, research, Err("Not ready yet".into()));
    assert_eq!(wizard.catalog_problem.as_deref(), Some("Not ready yet"));
    assert!(wizard.team.is_none());
    assert!(wizard.catalog_request().is_some());
    assert_eq!(wizard.check(Step::Team), Ok(()));
}

#[test]
fn team_counts_follow_the_engine_rule_including_the_master() {
    let mut wizard = wizard();
    wizard.research = vec![ResearchKind::MarketResearch, ResearchKind::DesignResearch];
    load_catalog(&mut wizard, &["c-codex"]);
    wizard.go(Step::Team);
    assert_eq!(wizard.focused(), Control::Team);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Right));
    assert_eq!(wizard.team.as_ref().unwrap().sub_harnesses[0].agents, 3);
    let note = wizard.note.clone().unwrap();
    assert_eq!(note.control, Control::Max);
    assert_eq!(
        note.text,
        "Sub-agents add up to 3, plus the master makes 4, but Max is 3. Raise Max or lower a count."
    );
    wizard.go(Step::Team);
    wizard.focus_on(Control::Continue);
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.step, Step::Team);
    assert_eq!(wizard.focused(), Control::Max);
    assert!(wizard.typing);
    wizard.key(control('u'));
    type_text(&mut wizard, "x4");
    assert_eq!(wizard.max.value(), "4");
    assert_eq!(wizard.note, None);
    wizard.key(key(KeyCode::BackTab));
    wizard.key(control('u'));
    type_text(&mut wizard, "5");
    assert_eq!(
        wizard.note.as_ref().map(|n| n.text.as_str()),
        Some("Min is 5, but Max is 4. Min can't be more than Max.")
    );
    wizard.key(key(KeyCode::Backspace));
    assert_eq!(
        wizard.note.as_ref().map(|n| n.text.as_str()),
        Some("Type a number from 1 to 64.")
    );
    type_text(&mut wizard, "1");
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.focused(), Control::Max);
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.step, Step::Review);
    assert!(wizard.done[Step::Team.index()]);
}

#[test]
fn team_check_agrees_with_the_engine_validation() {
    let mut wizard = wizard();
    wizard.research = vec![ResearchKind::MarketResearch];
    load_catalog(&mut wizard, &["c-codex"]);
    let base: TeamPlan = wizard.team.clone().unwrap();
    for agents in 1..=4u8 {
        for min in 1..=6u8 {
            for max in 1..=6u8 {
                let mut team = base.clone();
                team.sub_harnesses[0].agents = agents;
                team.min_agents = min;
                team.max_agents = max;
                wizard.team = Some(team.clone());
                wizard.min.set(&min.to_string());
                wizard.max.set(&max.to_string());
                assert_eq!(
                    wizard.check_team().is_ok(),
                    team.validate().is_ok(),
                    "agents {agents} min {min} max {max}"
                );
            }
        }
    }
}

#[test]
fn the_harness_picker_lists_everything_but_picks_only_installed_harnesses() {
    let mut wizard = wizard();
    wizard.research = vec![ResearchKind::MarketResearch];
    load_catalog(&mut wizard, &["c-codex"]);
    wizard.go(Step::Team);
    assert_eq!(
        wizard
            .harness_choices()
            .iter()
            .map(|h| (h.name.as_str(), h.state.clone()))
            .collect::<Vec<_>>(),
        [
            ("rcli", HarnessState::Unknown),
            ("Claude Code", HarnessState::Unknown),
            ("Codex", HarnessState::Unknown)
        ]
    );
    wizard.set_harnesses(vec![
        HarnessChoice {
            id: "codex".into(),
            name: "Codex".into(),
            state: HarnessState::Ready,
        },
        HarnessChoice {
            id: "opencode".into(),
            name: "OpenCode".into(),
            state: HarnessState::NotInstalled,
        },
        HarnessChoice {
            id: "claude".into(),
            name: "Claude Code".into(),
            state: HarnessState::Limited("tools always on".into()),
        },
    ]);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.team_row, 2);
    wizard.key(key(KeyCode::Enter));
    assert_eq!(
        wizard.picker,
        Some(Picker {
            target: PickFor::NewSlot,
            cursor: 0
        })
    );
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert!(
        wizard.picker.is_some(),
        "a missing harness cannot be picked"
    );
    assert_eq!(wizard.key(key(KeyCode::Char('r'))), Reply::CheckHarnesses);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Enter));
    assert_eq!(wizard.picker, None);
    let team = wizard.team.clone().unwrap();
    assert_eq!(team.sub_harnesses[1].harness, "claude");
    assert_eq!(team.sub_harnesses[1].agents, 1);
    assert_eq!(
        team_summary(&team, wizard.harnesses.as_deref()),
        "Codex leads. Codex x1, Claude Code x1. Tools on."
    );
    assert_eq!(
        wizard.note.as_ref().map(|n| n.text.as_str()),
        Some(
            "Sub-agents add up to 2, plus the master makes 3, but Max is 2. Raise Max or lower a count."
        )
    );
    wizard.key(key(KeyCode::Char(' ')));
    wizard.key(key(KeyCode::Char('d')));
    assert_eq!(wizard.team.as_ref().unwrap().sub_harnesses.len(), 1);
    assert_eq!(wizard.note, None);
    wizard.key(key(KeyCode::Up));
    wizard.key(key(KeyCode::Up));
    wizard.key(key(KeyCode::Char(' ')));
    assert!(!wizard.team.as_ref().unwrap().master_tools);
    assert_eq!(
        team_summary(wizard.team.as_ref().unwrap(), None),
        "Codex leads. Codex x1. Tools on for some."
    );
    wizard.key(key(KeyCode::Enter));
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Esc));
    assert_eq!(wizard.picker, None);
    assert_eq!(wizard.team.as_ref().unwrap().master, "codex");
    wizard.team.as_mut().unwrap().master = "opencode".into();
    assert_eq!(
        wizard.check_team().unwrap_err().text,
        "OpenCode is not installed. Pick another harness."
    );
}

#[test]
fn documents_by_path_check_type_size_count_and_duplicates() {
    let home = tempfile::tempdir().unwrap();
    let research = home.path().join("Research");
    std::fs::create_dir(&research).unwrap();
    std::fs::write(research.join("buoy-spec.pdf"), b"%PDF-1.7").unwrap();
    std::fs::write(research.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(research.join("clip.mov"), b"video").unwrap();
    std::fs::write(research.join(".hidden.md"), "x").unwrap();
    let big = std::fs::File::create(home.path().join("huge.pdf")).unwrap();
    big.set_len(51 * 1024 * 1024).unwrap();
    std::os::unix::fs::symlink(research.join("notes.md"), home.path().join("link.md")).unwrap();
    let mut wizard = Wizard::new(Vec::new(), Some(home.path().to_path_buf()));
    wizard.go(Step::Documents);
    assert_eq!(wizard.focused(), Control::Path);

    let add = |wizard: &mut Wizard, text: &str| {
        wizard.key(control('u'));
        type_text(wizard, text);
        wizard.key(key(KeyCode::Enter));
        wizard.note.as_ref().map(|n| n.text.clone())
    };
    assert_eq!(
        add(&mut wizard, "~/nope.pdf").as_deref(),
        Some("No file or folder at ~/nope.pdf.")
    );
    assert_eq!(
        add(&mut wizard, "relative.pdf").as_deref(),
        Some("Type a full path, starting with / or ~.")
    );
    assert_eq!(
        add(&mut wizard, "~/huge.pdf").as_deref(),
        Some("Too big: huge.pdf is 51 MB. The limit is 50 MB per file.")
    );
    assert_eq!(
        add(&mut wizard, "~/Research/clip.mov").as_deref(),
        Some(
            "clip.mov is not a supported type. Add PDF, Markdown, text, CSV, Word or image files."
        )
    );
    assert_eq!(add(&mut wizard, "~/Research"), None);
    assert_eq!(
        wizard
            .documents
            .iter()
            .map(|d| (d.name.as_str(), d.kind(), d.bytes))
            .collect::<Vec<_>>(),
        [("buoy-spec.pdf", "PDF", 8), ("notes.md", "Markdown", 8)]
    );
    assert_eq!(wizard.path.value(), "");
    assert!(
        wizard
            .documents
            .iter()
            .all(|d| std::path::Path::new(&d.path).is_absolute())
    );
    assert_eq!(
        add(&mut wizard, "~/link.md").as_deref(),
        Some("notes.md is already in the list.")
    );
    wizard.key(control('u'));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.focused(), Control::Documents);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Char('d')));
    assert_eq!(wizard.documents[0].name, "buoy-spec.pdf");
    assert_eq!(wizard.documents.len(), 1);
    wizard.key(key(KeyCode::Up));
    assert_eq!(wizard.focused(), Control::Path);
    assert_eq!(add(&mut wizard, "~/link.md"), None);
    let canonical = std::fs::canonicalize(research.join("notes.md")).unwrap();
    assert_eq!(
        wizard.documents.last().unwrap().path,
        canonical.to_str().unwrap()
    );

    let many = home.path().join("many");
    std::fs::create_dir(&many).unwrap();
    for index in 0..15 {
        std::fs::write(many.join(format!("{index:02}.txt")), "x").unwrap();
    }
    let before = wizard.documents.len();
    assert_eq!(
        add(&mut wizard, "~/many").as_deref(),
        Some("A project holds at most 16 documents. Remove one first.")
    );
    assert_eq!(wizard.documents.len(), before);
}

#[test]
fn tab_completes_a_path_like_a_shell() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("Research")).unwrap();
    std::fs::write(home.path().join("Research/report-a.md"), "a").unwrap();
    std::fs::write(home.path().join("Research/report-b.md"), "b").unwrap();
    let mut wizard = Wizard::new(Vec::new(), Some(home.path().to_path_buf()));
    wizard.go(Step::Documents);
    type_text(&mut wizard, "~/Res");
    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.path.value(), "~/Research/");
    type_text(&mut wizard, "r");
    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.path.value(), "~/Research/report-");
    assert_eq!(wizard.focused(), Control::Path);
    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.focused(), Control::Back);
}

#[test]
fn the_stack_grid_toggles_filters_and_adds_custom_entries() {
    let mut wizard = wizard();
    load_catalog(&mut wizard, &[]);
    wizard.go(Step::Stack);
    assert_eq!(wizard.grid_shape(), (3, 4));
    wizard.key(key(KeyCode::Char(' ')));
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Right));
    assert_eq!(wizard.grid, 5);
    wizard.key(key(KeyCode::Char(' ')));
    assert_eq!(wizard.tech, ["TypeScript", "Java"]);
    wizard.key(key(KeyCode::Right));
    wizard.key(key(KeyCode::Right));
    assert_eq!(wizard.grid, 9);
    wizard.key(key(KeyCode::Char('/')));
    assert!(wizard.filtering);
    type_text(&mut wizard, "post");
    assert_eq!(wizard.tech_options(), ["PostgreSQL"]);
    assert_eq!(wizard.grid, 0);
    wizard.key(key(KeyCode::Enter));
    assert!(!wizard.filtering);
    wizard.key(key(KeyCode::Char(' ')));
    assert_eq!(wizard.tech, ["TypeScript", "Java", "PostgreSQL"]);
    wizard.key(key(KeyCode::Char('/')));
    wizard.key(key(KeyCode::Esc));
    assert_eq!(wizard.tech_options().len(), 30);

    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.focused(), Control::Custom);
    type_text(&mut wizard, "TimescaleDB");
    wizard.key(key(KeyCode::Enter));
    assert_eq!(
        wizard.note.as_ref().map(|n| (n.text.as_str(), n.error)),
        Some(("Added", false))
    );
    wizard.key(key(KeyCode::Enter));
    assert_eq!(
        wizard.note.as_ref().map(|n| n.text.as_str()),
        Some("TimescaleDB is already in the list.")
    );
    wizard.key(control('u'));
    type_text(&mut wizard, "react");
    wizard.key(key(KeyCode::Enter));
    assert_eq!(
        wizard.tech,
        ["TypeScript", "Java", "PostgreSQL", "TimescaleDB", "React"]
    );

    wizard.key(key(KeyCode::Tab));
    assert_eq!(wizard.focused(), Control::Platforms);
    wizard.key(key(KeyCode::Char(' ')));
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Char(' ')));
    assert_eq!(wizard.platforms, [Platform::Website, Platform::ApiBackend]);
    wizard.width = 120;
    assert_eq!(wizard.grid_shape(), (4, 5));
}

#[test]
fn work_toggles_follow_the_engine_order_and_the_pool_radio_selects_as_it_moves() {
    let mut wizard = wizard();
    wizard.go(Step::Work);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Char(' ')));
    wizard.key(key(KeyCode::Up));
    wizard.key(key(KeyCode::Enter));
    assert_eq!(
        wizard.research,
        [
            ResearchKind::CompetitorResearch,
            ResearchKind::MarketResearch
        ]
    );
    assert_eq!(
        wizard.research_sorted(),
        [
            ResearchKind::MarketResearch,
            ResearchKind::CompetitorResearch
        ]
    );
    wizard.go(Step::Pool);
    assert_eq!(wizard.pool_name(), Some("Main"));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.pool_name(), Some("Unnamed pool"));
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.pool, None);
    wizard.key(key(KeyCode::Down));
    assert_eq!(wizard.pool, None);
    wizard.key(key(KeyCode::Up));
    assert_eq!(wizard.pool_name(), Some("Unnamed pool"));
    assert_eq!(wizard.key(key(KeyCode::Char('a'))), Reply::CreatePool);
    wizard.set_pools(Vec::new());
    assert_eq!(wizard.pool, None);
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(wizard.step, Step::Team);
}

#[test]
fn create_sends_every_field_and_then_waits_for_the_engine() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("scope.csv"), "a,b\n").unwrap();
    let mut wizard = Wizard::new(pool_choices(&config()), Some(home.path().to_path_buf()));
    type_text(&mut wizard, "Tidepool ");
    wizard.key(key(KeyCode::Tab));
    wizard.paste(" Buoy\r\ndashboard \u{1b}");
    wizard.research = vec![ResearchKind::FrontEndDesign, ResearchKind::MarketResearch];
    wizard.platforms = vec![Platform::Cli, Platform::Website];
    wizard.tech = vec!["Rust".into()];
    load_catalog(&mut wizard, &["c-codex"]);
    wizard.go(Step::Documents);
    type_text(&mut wizard, "~/scope.csv");
    wizard.key(key(KeyCode::Enter));
    wizard.go(Step::Review);
    let Reply::Create(request) = wizard.key(key(KeyCode::Enter)) else {
        panic!("create expected");
    };
    let (spec, setup) = *request;
    assert_eq!(spec.name, "Tidepool");
    assert!(spec.repositories.is_empty());
    assert_eq!(setup.about, "Buoy\ndashboard");
    assert_eq!(setup.platforms, [Platform::Website, Platform::Cli]);
    assert_eq!(
        setup.research,
        [ResearchKind::MarketResearch, ResearchKind::FrontEndDesign]
    );
    assert_eq!(
        setup.pool.as_ref().map(CredentialRef::as_str),
        Some("pool-main")
    );
    assert_eq!(setup.team.as_ref().unwrap().max_agents, 3);
    assert_eq!(setup.documents.len(), 1);
    assert!(setup.documents[0].ends_with("/scope.csv"));
    assert!(setup.validate().is_ok());
    assert_eq!(
        wizard.phase,
        Phase::Creating {
            pool: true,
            created: false
        }
    );
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Ignored);
    assert_eq!(wizard.key(key(KeyCode::Esc)), Reply::Background);
    wizard.created();
    assert_eq!(
        wizard.phase,
        Phase::Creating {
            pool: true,
            created: true
        }
    );
}

#[test]
fn create_jumps_to_the_first_step_that_needs_a_fix() {
    let mut wizard = wizard();
    wizard.go(Step::Review);
    assert_eq!(wizard.key(key(KeyCode::Enter)), Reply::Handled);
    assert_eq!(wizard.step, Step::About);
    assert_eq!(wizard.focused(), Control::Name);
    assert_eq!(wizard.phase, Phase::Editing);
}

#[test]
fn engine_failures_map_to_plain_copy_and_the_step_that_fixes_them() {
    let mut wizard = wizard();
    wizard.go(Step::Review);
    wizard.failed("pool_missing");
    assert_eq!(
        wizard.phase,
        Phase::Failed {
            title: "Could not create the project.".into(),
            detail: "Pool Main no longer exists. Pick another pool.".into(),
            step: Step::Pool
        }
    );
    assert_eq!(wizard.key(key(KeyCode::Char('x'))), Reply::Ignored);
    wizard.key(key(KeyCode::Enter));
    assert_eq!((wizard.step, &wizard.phase), (Step::Pool, &Phase::Editing));
    for (code, step) in [
        ("document_too_large", Step::Documents),
        ("team_agent_range_invalid", Step::Team),
        ("project_folder_exists", Step::Review),
        ("project_about_invalid", Step::About),
        ("some_new_code", Step::Review),
    ] {
        wizard.failed(code);
        let Phase::Failed {
            step: shown,
            detail,
            ..
        } = &wizard.phase
        else {
            panic!("failed phase");
        };
        assert_eq!(*shown, step, "{code}");
        assert!(!detail.contains('_'), "{detail}");
    }
    assert_eq!(wizard.key(key(KeyCode::Esc)), Reply::Leave);
    assert_eq!(wizard.phase, Phase::Editing);
}

#[test]
fn sizes_paths_and_harness_names_read_plainly() {
    assert_eq!(size_label(800), "800 B");
    assert_eq!(size_label(38 * 1024), "38 KB");
    assert_eq!(size_label(2_516_582), "2.4 MB");
    assert_eq!(size_label(50 * 1024 * 1024), "50 MB");
    assert_eq!(size_label(1_288_490_189), "1.2 GB");
    let home = std::path::Path::new("/Users/owner");
    assert_eq!(
        display_path("/Users/owner/CrazyAgent/tidepool", Some(home)),
        "~/CrazyAgent/tidepool"
    );
    assert_eq!(
        display_path("/Users/ownerx/a", Some(home)),
        "/Users/ownerx/a"
    );
    assert_eq!(harness_name("claude", None), "Claude Code");
    assert_eq!(harness_name("mystery", None), "mystery");
}
