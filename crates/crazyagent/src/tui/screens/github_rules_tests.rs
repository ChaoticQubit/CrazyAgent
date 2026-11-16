use super::intake::{copy, known};
use super::project_github::{
    REPO_TAIL, RULES_NOT_PUBLISHED, RULES_NOT_READY, RULES_UNAVAILABLE, SENDING_TAIL,
};
use super::project_publish::rule_label;
use super::settings_accounts::tests::{ctx, render};
use super::settings_github::{
    DEFAULTS_ABOUT, DEFAULTS_TITLE, DEFAULTS_UNREADABLE, DEFAULTS_UNSAVED, STANDARD_LABEL,
};
use super::{Place, ProjectTab, open};
use crate::github::{GitHubRepoRules, GitHubRule, GitHubRuleOutcome, GitHubRuleState};
use crate::protocol::ProjectId;
use crate::tui::client::OwnerEvent;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::shell::{Shell, targets};
use crate::tui::widgets::checkbox;
use std::collections::BTreeSet;

const REPO: &str = "octo-cat/tool";
const NAME: &str = "Alpha";

fn arrival(query: IntakeQuery, result: Result<IntakeView, String>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Intake(Box::new(IntakeEvent {
        query,
        result,
    }))))
}

fn asked(shell: &mut Shell) -> Vec<IntakeQuery> {
    let mut queries = Vec::new();
    while let Some(request) = shell.ctx.next_screen_request() {
        match request {
            Request::Intake(job) => queries.push(job.query()),
            _ => panic!("the GitHub rules pages send only intake requests"),
        }
    }
    queries
}

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

/// The body read as one text, each row trimmed and joined by a space, so a sentence the
/// page wraps still reads whole. Takes the rendered rows. Returns the text.
fn flat(rows: &[String]) -> String {
    rows.iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn selected(rows: &[String]) -> String {
    rows.iter()
        .find(|row| row.trim_start().starts_with("> ["))
        .cloned()
        .unwrap_or_else(|| panic!("no selected rule:\n{}", rows.join("\n")))
}

fn defaults(width: u16, height: u16, on: BTreeSet<GitHubRule>) -> Shell {
    let mut ctx = ctx(width, height);
    let screen = open(&Place::GitHubDefaults, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    assert_eq!(asked(&mut shell), [IntakeQuery::GitHubRuleDefaults]);
    shell.arrive(arrival(
        IntakeQuery::GitHubRuleDefaults,
        Ok(IntakeView::RuleDefaults(on)),
    ));
    shell
}

fn page(change: impl FnOnce(&mut GitHubRepoRules)) -> GitHubRepoRules {
    let mut page = GitHubRepoRules {
        full_name: REPO.into(),
        private: true,
        checks: vec!["build".into(), "test".into()],
        rules: GitHubRule::ALL
            .into_iter()
            .map(|rule| GitHubRuleOutcome {
                rule,
                on: false,
                state: GitHubRuleState::Applied,
            })
            .collect(),
    };
    change(&mut page);
    page
}

fn set(page: &mut GitHubRepoRules, rule: GitHubRule, on: bool, state: GitHubRuleState) {
    let entry = page
        .rules
        .iter_mut()
        .find(|outcome| outcome.rule == rule)
        .expect("the page lists every rule");
    entry.on = on;
    entry.state = state;
}

/// A page with the two file rules and conversation resolution that cannot change, and
/// the wiki rule GitHub refused.
fn held() -> GitHubRepoRules {
    page(|page| {
        let file = || GitHubRuleState::NotApplicable {
            reason: "a file in the project, written when it is published".into(),
        };
        set(page, GitHubRule::SecurityPolicy, true, file());
        set(page, GitHubRule::DependabotVersionUpdates, true, file());
        set(
            page,
            GitHubRule::ConversationResolution,
            true,
            GitHubRuleState::NotApplicable {
                reason: "pull requests are off".into(),
            },
        );
        set(
            page,
            GitHubRule::WikiOff,
            false,
            GitHubRuleState::Refused {
                status: 403,
                message: "Resource not accessible by integration".into(),
            },
        );
    })
}

fn project(width: u16, height: u16, read: GitHubRepoRules) -> (Shell, ProjectId) {
    let id = ProjectId::new("alpha").unwrap();
    let mut ctx = ctx(width, height);
    let screen = open(
        &Place::ProjectGitHub {
            id: id.clone(),
            name: NAME.into(),
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    assert_eq!(
        asked(&mut shell),
        [IntakeQuery::GitHubRules {
            project: id.clone()
        }]
    );
    shell.arrive(arrival(
        IntakeQuery::GitHubRules {
            project: id.clone(),
        },
        Ok(IntakeView::Rules(Box::new(read))),
    ));
    (shell, id)
}

fn go(shell: &mut Shell, rule: GitHubRule) {
    shell.key(Key::Home);
    let index = GitHubRule::ALL
        .iter()
        .position(|listed| *listed == rule)
        .expect("every rule is listed");
    for _ in 0..index {
        shell.key(Key::Down);
    }
}

/// Settings / GitHub opens with one read and nothing else, lists the 25 rules of DEC-182
/// with signed commits off, and the selected rule stays on screen from the first to the
/// last at 80x24.
#[test]
fn the_defaults_page_lists_every_rule_signed_commits_off_and_follows_the_cursor_at_80_by_24() {
    let mut shell = defaults(80, 24, GitHubRule::defaults());
    assert_eq!(shell.place(), Some(Place::GitHubDefaults));
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Settings  /  GitHub"), "{rows:#?}");
    assert!(shows(&rows, DEFAULTS_TITLE), "{rows:#?}");
    assert!(shows(&rows, &format!("s {STANDARD_LABEL}")), "{rows:#?}");
    for (index, rule) in GitHubRule::ALL.into_iter().enumerate() {
        let rows = render(&mut shell, 80, 24);
        let on = rule != GitHubRule::SignedCommits;
        assert!(
            selected(&rows).contains(&checkbox(on, rule_label(rule))),
            "{index} {rule:?}\n{}",
            rows.join("\n")
        );
        shell.key(Key::Down);
    }
    let rows = render(&mut shell, 120, 40);
    assert!(flat(&rows).contains(DEFAULTS_ABOUT), "{rows:#?}");
    assert!(asked(&mut shell).is_empty(), "moving sends nothing");
}

/// A switch sends the whole set once and waits for the engine; the saved set shows with
/// the changed rule marked, and s sends DEC-182's standard setup.
#[test]
fn a_default_switch_saves_the_whole_set_once_and_the_standard_setup_comes_back() {
    let mut shell = defaults(120, 40, GitHubRule::defaults());
    shell.key(Key::Char(' '));
    let mut without = GitHubRule::defaults();
    assert!(without.remove(&GitHubRule::SquashOnly));
    let sent = IntakeQuery::GitHubRuleDefaultsSet {
        rules: without.clone(),
    };
    assert_eq!(asked(&mut shell), [sent.clone()]);
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    shell.key(Key::Char('s'));
    assert!(asked(&mut shell).is_empty(), "one save at a time");
    shell.arrive(arrival(sent, Ok(IntakeView::RuleDefaults(without))));
    let rows = render(&mut shell, 120, 40);
    let off = checkbox(false, rule_label(GitHubRule::SquashOnly));
    let row = rows
        .iter()
        .find(|row| row.contains(&off))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(row.contains("Changed"), "{row}");
    assert!(!selected(&rows).contains("Changed"), "{rows:#?}");
    shell.key(Key::Char('s'));
    assert_eq!(
        asked(&mut shell),
        [IntakeQuery::GitHubRuleDefaultsSet {
            rules: GitHubRule::defaults()
        }]
    );
}

/// A save that fails says so in the desktop's words and reads the saved set again; saved
/// defaults that can't be read safely say so, and s still replaces them.
#[test]
fn a_failed_defaults_save_says_so_and_reads_the_saved_set_again() {
    let mut shell = defaults(120, 40, GitHubRule::defaults());
    shell.key(Key::Char(' '));
    let [sent] = <[IntakeQuery; 1]>::try_from(asked(&mut shell)).unwrap();
    shell.arrive(arrival(sent, Err("github_state_unavailable".into())));
    assert_eq!(asked(&mut shell), [IntakeQuery::GitHubRuleDefaults]);
    let rows = render(&mut shell, 120, 40);
    assert!(flat(&rows).contains(DEFAULTS_UNSAVED), "{rows:#?}");
    shell.arrive(arrival(
        IntakeQuery::GitHubRuleDefaults,
        Err("github_state_invalid".into()),
    ));
    let rows = render(&mut shell, 120, 40);
    assert!(flat(&rows).contains(DEFAULTS_UNREADABLE), "{rows:#?}");
    assert!(!shows(&rows, "github_state_invalid"), "{rows:#?}");
    assert!(!shows(&rows, "[x] "), "{rows:#?}");
    shell.key(Key::Char('s'));
    assert_eq!(
        asked(&mut shell),
        [IntakeQuery::GitHubRuleDefaultsSet {
            rules: GitHubRule::defaults()
        }]
    );
}

/// Project > Settings > GitHub opens with one read, names the repository, keeps the
/// selected rule on screen at 80x24, and sends a switch for the repository it showed,
/// one at a time.
#[test]
fn the_repository_page_sends_one_switch_at_a_time_for_the_repository_it_showed() {
    let (mut shell, id) = project(80, 24, held());
    assert_eq!(
        shell.place(),
        Some(Place::ProjectGitHub {
            id: id.clone(),
            name: NAME.into()
        })
    );
    let rows = render(&mut shell, 80, 24);
    assert!(shows(&rows, "Alpha  /  GitHub"), "{rows:#?}");
    assert!(
        flat(&rows).contains(&format!("{REPO}, Private{REPO_TAIL}")),
        "{rows:#?}"
    );
    for rule in GitHubRule::ALL {
        go(&mut shell, rule);
        let rows = render(&mut shell, 80, 24);
        assert!(
            selected(&rows).contains(rule_label(rule)),
            "{rule:?}\n{}",
            rows.join("\n")
        );
    }
    go(&mut shell, GitHubRule::WikiOff);
    let rows = render(&mut shell, 80, 24);
    assert!(
        flat(&rows)
            .contains("GitHub refused this (HTTP 403): Resource not accessible by integration"),
        "{rows:#?}"
    );
    shell.key(Key::Char(' '));
    let sent = IntakeQuery::GitHubRuleSet {
        project: id.clone(),
        repo: REPO.into(),
        rule: GitHubRule::WikiOff,
        on: true,
    };
    assert_eq!(asked(&mut shell), [sent.clone()]);
    let rows = render(&mut shell, 80, 24);
    let sending = format!("Sending {}{SENDING_TAIL}", rule_label(GitHubRule::WikiOff));
    assert!(flat(&rows).contains(&sending), "{rows:#?}");
    go(&mut shell, GitHubRule::IssuesOn);
    shell.key(Key::Char(' '));
    assert!(asked(&mut shell).is_empty(), "one switch in flight");
    shell.arrive(arrival(
        sent,
        Ok(IntakeView::Rules(Box::new(page(|page| {
            set(page, GitHubRule::WikiOff, true, GitHubRuleState::Applied);
        })))),
    ));
    let rows = render(&mut shell, 120, 40);
    assert!(!flat(&rows).contains("Sending "), "{rows:#?}");
    assert!(
        shows(&rows, &checkbox(true, rule_label(GitHubRule::WikiOff))),
        "{rows:#?}"
    );
    shell.key(Key::Char(' '));
    assert_eq!(
        asked(&mut shell),
        [IntakeQuery::GitHubRuleSet {
            project: id,
            repo: REPO.into(),
            rule: GitHubRule::IssuesOn,
            on: true,
        }]
    );
}

/// A rule that does not apply, or waits for the first CI run, never sends and says why;
/// required checks name the checks the first CI run reported.
#[test]
fn a_rule_that_cannot_change_never_sends_and_says_why() {
    let (mut shell, _) = project(80, 24, held());
    for rule in [
        GitHubRule::SecurityPolicy,
        GitHubRule::DependabotVersionUpdates,
        GitHubRule::ConversationResolution,
    ] {
        go(&mut shell, rule);
        shell.key(Key::Char(' '));
        assert!(asked(&mut shell).is_empty(), "{rule:?}");
    }
    go(&mut shell, GitHubRule::SecurityPolicy);
    let rows = render(&mut shell, 80, 24);
    assert!(
        flat(&rows).contains("A file in the project, written when it is published."),
        "{rows:#?}"
    );
    go(&mut shell, GitHubRule::ConversationResolution);
    assert!(
        flat(&render(&mut shell, 80, 24)).contains("Pull requests are off."),
        "conversation resolution says why"
    );
    go(&mut shell, GitHubRule::RequiredChecks);
    assert!(
        flat(&render(&mut shell, 80, 24)).contains("Checks the first CI run named: build, test."),
        "required checks name the checks"
    );
    let (mut shell, _) = project(
        80,
        24,
        page(|page| {
            page.checks.clear();
            set(
                page,
                GitHubRule::RequiredChecks,
                true,
                GitHubRuleState::Waiting {
                    reason: "waiting for the first CI run".into(),
                },
            );
        }),
    );
    go(&mut shell, GitHubRule::RequiredChecks);
    shell.key(Key::Char(' '));
    assert!(asked(&mut shell).is_empty(), "a waiting rule never sends");
    assert!(
        flat(&render(&mut shell, 80, 24)).contains("Waiting for the first CI run."),
        "the waiting rule says why"
    );
}

/// A failed switch keeps its problem and reads the page again, since the change may have
/// landed on GitHub; a build without the change says so.
#[test]
fn a_failed_switch_keeps_its_problem_and_reads_the_page_again() {
    let (mut shell, id) = project(120, 40, page(|_| {}));
    let read = IntakeQuery::GitHubRules {
        project: id.clone(),
    };
    for (code, sentence) in [
        ("github_outcome_unknown", copy("github_outcome_unknown")),
        (
            "github_rules_repo_mismatch",
            copy("github_rules_repo_mismatch"),
        ),
        ("not_ready", RULES_NOT_READY.to_owned()),
    ] {
        go(&mut shell, GitHubRule::WikiOff);
        shell.key(Key::Char(' '));
        let [sent] = <[IntakeQuery; 1]>::try_from(asked(&mut shell)).unwrap();
        shell.arrive(arrival(sent, Err(code.into())));
        assert_eq!(asked(&mut shell), [read.clone()], "{code}");
        shell.arrive(arrival(
            read.clone(),
            Ok(IntakeView::Rules(Box::new(page(|_| {})))),
        ));
        let rows = render(&mut shell, 120, 40);
        assert!(flat(&rows).contains(&sentence), "{code}\n{rows:#?}");
        assert!(!shows(&rows, code), "{code}\n{rows:#?}");
        assert!(!flat(&rows).contains("Something went wrong"), "{code}");
    }
}

/// A project CrazyAgent did not publish says where its rules show; a build without the
/// read says so; any other failed read offers r to read again.
#[test]
fn a_project_crazyagent_did_not_publish_says_where_its_rules_show() {
    let id = ProjectId::new("alpha").unwrap();
    let read = IntakeQuery::GitHubRules {
        project: id.clone(),
    };
    for (code, sentence) in [
        ("github_rules_not_published", RULES_NOT_PUBLISHED.to_owned()),
        ("github_rules_unsupported", RULES_UNAVAILABLE.to_owned()),
        ("github_unreachable", copy("github_unreachable")),
    ] {
        let mut ctx = ctx(120, 40);
        let screen = open(
            &Place::ProjectGitHub {
                id: id.clone(),
                name: NAME.into(),
            },
            &mut ctx,
        );
        let mut shell = Shell::with(ctx, screen);
        assert_eq!(asked(&mut shell), [read.clone()]);
        shell.arrive(arrival(read.clone(), Err(code.into())));
        let rows = render(&mut shell, 120, 40);
        assert!(flat(&rows).contains(&sentence), "{code}\n{rows:#?}");
        assert!(!shows(&rows, code), "{code}\n{rows:#?}");
        shell.key(Key::Char(' '));
        assert!(asked(&mut shell).is_empty(), "{code}: nothing to switch");
        shell.key(Key::Char('r'));
        let again = asked(&mut shell);
        if code == "github_unreachable" {
            assert!(shows(&rows, "r Try again"), "{rows:#?}");
            assert_eq!(again, [read.clone()]);
        } else {
            assert!(again.is_empty(), "{code}");
        }
    }
}

/// The TUI says what the desktop says, word for word, for the GitHub rules pages.
#[test]
fn the_tui_github_rules_words_match_the_desktop_word_for_word() {
    let desktop = |path: &str| {
        std::fs::read_to_string(format!(
            "{}/../../apps/desktop/src/{path}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let rules = desktop("screens/project/github-rules.ts");
    for (name, text) in [
        ("RULES_NOT_PUBLISHED", RULES_NOT_PUBLISHED),
        ("DEFAULTS_TITLE", DEFAULTS_TITLE),
        ("DEFAULTS_ABOUT", DEFAULTS_ABOUT),
        ("STANDARD_LABEL", STANDARD_LABEL),
    ] {
        assert!(!text.is_empty(), "{name}");
        assert!(
            rules.contains(&format!("export const {name} = \"{text}\";")),
            "{name}"
        );
    }
    let intake = desktop("api-intake.ts");
    for (name, text) in [
        ("DEFAULTS_UNREADABLE", DEFAULTS_UNREADABLE),
        ("DEFAULTS_UNSAVED", DEFAULTS_UNSAVED),
    ] {
        assert!(!text.is_empty(), "{name}");
        assert!(
            intake.contains(&format!("export const {name} = \"{text}\";")),
            "{name}"
        );
    }
    for code in [
        "github_rules_not_published",
        "github_rule_invalid",
        "github_rules_repo_mismatch",
        "github_repo_unreadable",
    ] {
        let sentence = known(code).unwrap_or_default();
        assert!(!sentence.is_empty(), "{code}");
        assert!(
            intake.contains(&format!("\n  {code}: \"{sentence}\",")),
            "{code}"
        );
    }
    assert_eq!(
        known("github_rules_not_published"),
        Some(RULES_NOT_PUBLISHED)
    );
    let section = desktop("screens/project/github-section.tsx");
    for (name, text) in [
        ("RULES_NOT_READY", RULES_NOT_READY),
        ("SENDING_TAIL", SENDING_TAIL),
        ("REPO_TAIL", REPO_TAIL),
        ("RULES_UNAVAILABLE", RULES_UNAVAILABLE),
    ] {
        assert!(!text.is_empty(), "{name}");
    }
    assert!(section.contains(&format!("const RULES_NOT_READY = \"{RULES_NOT_READY}\";")));
    assert!(section.contains(&format!("<NotReady what=\"{RULES_UNAVAILABLE}\" />")));
    assert!(section.contains(&format!("?? busy}}{SENDING_TAIL}`")));
    assert!(section.contains(&format!("\"Public\"}}{REPO_TAIL}`")));
}

/// Go to lists Settings / GitHub, and G on a project's Settings tab opens that project's
/// GitHub rules.
#[test]
fn go_to_lists_settings_github_and_g_upper_opens_a_project_s_github_rules() {
    let mut ctx = ctx(120, 40);
    assert!(
        targets(&ctx)
            .iter()
            .any(|target| target.label == "Settings / GitHub"
                && target.kind == "Settings"
                && target.place == Place::GitHubDefaults)
    );
    let id = ProjectId::new("alpha").unwrap();
    let screen = open(
        &Place::Project {
            id: id.clone(),
            name: NAME.into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    shell.key(Key::Char('G'));
    assert_eq!(
        shell.place(),
        Some(Place::ProjectGitHub {
            id,
            name: NAME.into()
        })
    );
}
