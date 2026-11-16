use super::intake::copy;
use super::new_project_view::{Look, Row};
use super::project_publish::*;
use crate::github::{
    GitHubCommitted, GitHubIgnoreDraft, GitHubMethod, GitHubOwner, GitHubOwners,
    GitHubPromptAnswer, GitHubPublishJob, GitHubPublishPlan, GitHubPublishStep, GitHubRule,
    GitHubRuleOutcome, GitHubRuleState, GitHubSecretFinding, GitHubSecretKind, GitHubSecretScan,
    GitHubStatus, default_description, default_repo_name,
};
use crate::protocol::ProjectId;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery, IntakeView};
use crate::tui::keys::Key;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn job(step: GitHubPublishStep, stopped: Option<&str>) -> GitHubPublishJob {
    GitHubPublishJob {
        project: PathBuf::from("/p"),
        plan: GitHubPublishPlan {
            owner: "me".into(),
            name: "tool".into(),
            description: String::new(),
            private: true,
            docs: false,
        },
        lines: Vec::new(),
        written: Vec::new(),
        step,
        stopped: stopped.map(str::to_owned),
        made: None,
        rules: GitHubRule::defaults(),
    }
}

fn lines_job() -> GitHubPublishJob {
    job(
        GitHubPublishStep::Lines {
            lines: [".env*", "node_modules/", "*.log"]
                .map(String::from)
                .to_vec(),
            draft: GitHubIgnoreDraft::Agent,
        },
        None,
    )
}

fn committed() -> GitHubCommitted {
    GitHubCommitted {
        branch: "main".into(),
        commit: "abc".into(),
        left_out: 0,
    }
}

fn body(pane: &PublishPane) -> String {
    pane.text().body.join("\n")
}

#[test]
fn every_rule_has_its_own_plain_label() {
    let labels: BTreeSet<&str> = GitHubRule::ALL
        .iter()
        .map(|rule| rule_label(*rule))
        .collect();
    for rule in GitHubRule::ALL {
        let label = rule_label(rule);
        assert!(!label.is_empty(), "{rule:?} has no label");
        assert!(!label.contains('_'), "{rule:?} shows a code: {label}");
        assert!(
            !label.ends_with('.'),
            "{rule:?} reads as a sentence: {label}"
        );
    }
    assert_eq!(
        labels.len(),
        GitHubRule::ALL.len(),
        "two rules share a label: {labels:?}"
    );
}

#[test]
fn a_rule_state_reads_as_the_desktop_reads_it() {
    assert_eq!(rule_state_line(&GitHubRuleState::Applied), None);
    assert_eq!(
        rule_state_line(&GitHubRuleState::Refused {
            status: 0,
            message: "no token".into(),
        })
        .as_deref(),
        Some("Not sent: no token")
    );
    assert_eq!(
        rule_state_line(&GitHubRuleState::Refused {
            status: 403,
            message: format!("bad\u{1b}[31m{}", "x".repeat(400)),
        })
        .map(|line| (
            line.starts_with("GitHub refused this (HTTP 403): bad"),
            line.contains('\u{1b}'),
            line.chars().count()
        )),
        Some((
            true,
            false,
            "GitHub refused this (HTTP 403): ".chars().count() + 300
        ))
    );
    assert_eq!(
        rule_state_line(&GitHubRuleState::NotApplicable {
            reason: "private repositories on a free plan can't use it".into(),
        })
        .as_deref(),
        Some("Private repositories on a free plan can't use it.")
    );
    assert_eq!(
        rule_state_line(&GitHubRuleState::Waiting {
            reason: "set after the first CI run.".into(),
        })
        .as_deref(),
        Some("Set after the first CI run.")
    );
}

#[test]
fn the_lines_pane_lets_the_owner_leave_lines_out_and_approves_the_rest() {
    let mut pane = PublishPane::new(lines_job());
    let shown = body(&pane);
    assert!(shown.contains(DRAFT_AGENT), "{shown}");
    assert!(shown.contains("> [x] .env*"), "{shown}");
    assert!(shown.contains("  [x] node_modules/"), "{shown}");
    assert_eq!(pane.key(&Key::Down), PaneKey::Handled);
    assert_eq!(pane.key(&Key::Char(' ')), PaneKey::Handled);
    let shown = body(&pane);
    assert!(shown.contains("> [ ] node_modules/"), "{shown}");
    assert_eq!(
        pane.key(&Key::Enter),
        PaneKey::Ask(PublishAsk::Approve(vec![".env*".into(), "*.log".into()]))
    );
    assert_eq!(pane.key(&Key::Esc), PaneKey::Later);
    let titles: Vec<&str> = pane.text().hints.iter().map(|hint| hint.1).collect();
    assert!(titles.contains(&"Approve and publish"), "{titles:?}");
}

#[test]
fn the_blocked_pane_names_each_file_and_line_never_a_value_and_leaves_a_file_out() {
    let finding = |path: &str, line: u32, kind| GitHubSecretFinding {
        path: path.into(),
        line,
        kind,
    };
    let mut pane = PublishPane::new(job(
        GitHubPublishStep::Blocked {
            scan: GitHubSecretScan {
                findings: vec![
                    finding("keys.txt", 3, GitHubSecretKind::Token),
                    finding("keys.txt", 9, GitHubSecretKind::PrivateKey),
                    finding("a/.env\u{1b}[2J", 1, GitHubSecretKind::NamedValue),
                ],
                truncated: true,
            },
        },
        None,
    ));
    let shown = body(&pane);
    assert!(
        shown.contains("keys.txt, line 3: looks like an access token"),
        "{shown}"
    );
    assert!(
        shown.contains("keys.txt, line 9: looks like a private key"),
        "{shown}"
    );
    assert!(
        shown.contains("line 1: looks like a secret value"),
        "{shown}"
    );
    assert!(!shown.contains('\u{1b}'), "{shown}");
    assert!(
        shown.contains("There are more. Check again after these."),
        "{shown}"
    );
    assert_eq!(pane.key(&Key::Char('r')), PaneKey::Ask(PublishAsk::Recheck));
    assert_eq!(
        pane.key(&Key::Char('x')),
        PaneKey::Ask(PublishAsk::Exclude("keys.txt".into()))
    );
    assert_eq!(pane.key(&Key::Down), PaneKey::Handled);
    assert_eq!(pane.key(&Key::Down), PaneKey::Handled);
    assert_eq!(
        pane.key(&Key::Char('x')),
        PaneKey::Ask(PublishAsk::Exclude("a/.env\u{1b}[2J".into()))
    );
}

#[test]
fn a_stopped_publish_says_why_and_tries_again_and_a_draft_waits() {
    let mut pane = PublishPane::new(job(
        GitHubPublishStep::Creating {
            committed: committed(),
            sent_at: 1,
        },
        Some("github_repo_name_taken"),
    ));
    assert!(
        body(&pane).contains(&copy("github_repo_name_taken")),
        "{}",
        body(&pane)
    );
    assert_eq!(pane.key(&Key::Enter), PaneKey::Ask(PublishAsk::Continue));
    let pane = PublishPane::new(job(
        GitHubPublishStep::Pushed {
            full_name: "me/tool".into(),
            private: true,
            workflows: false,
        },
        None,
    ));
    assert!(body(&pane).contains(PART_WAY), "{}", body(&pane));
    let mut drafting = PublishPane::new(job(GitHubPublishStep::Drafting, None));
    assert!(body(&drafting).contains("Drafting the .gitignore lines..."));
    assert_eq!(drafting.key(&Key::Enter), PaneKey::Handled);
    assert_eq!(drafting.key(&Key::Char('d')), PaneKey::Handled);
    assert!(body(&drafting).contains("Drafting the .gitignore lines..."));
    assert!(opens(drafting.job()));
}

#[test]
fn discard_asks_first_and_is_not_offered_after_the_push() {
    let mut pane = PublishPane::new(lines_job());
    assert_eq!(pane.key(&Key::Char('d')), PaneKey::Handled);
    assert!(
        body(&pane).starts_with("Discard this publish?"),
        "{}",
        body(&pane)
    );
    assert_eq!(pane.key(&Key::Char('n')), PaneKey::Handled);
    assert!(body(&pane).contains(DRAFT_AGENT));
    assert_eq!(pane.key(&Key::Char('d')), PaneKey::Handled);
    assert_eq!(pane.key(&Key::Char('y')), PaneKey::Ask(PublishAsk::Discard));
    let mut pushed = PublishPane::new(job(
        GitHubPublishStep::Pushed {
            full_name: "me/tool".into(),
            private: true,
            workflows: false,
        },
        None,
    ));
    assert_eq!(pushed.key(&Key::Char('d')), PaneKey::Handled);
    assert!(!body(&pushed).starts_with("Discard this publish?"));
}

#[test]
fn a_done_publish_names_its_repository_and_the_rules_it_could_not_set() {
    let outcome = |rule, on, state| GitHubRuleOutcome { rule, on, state };
    let done = job(
        GitHubPublishStep::Done {
            full_name: "me/tool".into(),
            outcomes: vec![
                outcome(GitHubRule::SquashOnly, true, GitHubRuleState::Applied),
                outcome(
                    GitHubRule::SecretScanning,
                    true,
                    GitHubRuleState::Refused {
                        status: 403,
                        message: "plan".into(),
                    },
                ),
                outcome(
                    GitHubRule::SignedCommits,
                    false,
                    GitHubRuleState::NotApplicable {
                        reason: "off".into(),
                    },
                ),
            ],
        },
        None,
    );
    assert!(!opens(&done));
    let mut pane = PublishPane::new(done);
    let shown = body(&pane);
    assert!(
        shown.contains("Published as me/tool. 1 of 2 rules set."),
        "{shown}"
    );
    assert!(
        shown.contains("- Secret scanning: GitHub refused this (HTTP 403): plan"),
        "{shown}"
    );
    assert!(!shown.contains("Signed commits"), "{shown}");
    assert_eq!(pane.key(&Key::Enter), PaneKey::Later);
}

#[test]
fn each_pane_answer_goes_to_the_engine_as_its_own_publish_query() {
    let project = ProjectId::new("alpha").unwrap();
    assert_eq!(
        query(&project, PublishAsk::Approve(vec![".env*".into()])),
        IntakeQuery::GitHubPublishApprove {
            project: project.clone(),
            lines: vec![".env*".into()],
        }
    );
    assert_eq!(
        query(&project, PublishAsk::Exclude("keys.txt".into())),
        IntakeQuery::GitHubPublishExclude {
            project: project.clone(),
            path: "keys.txt".into(),
        }
    );
    assert_eq!(
        query(&project, PublishAsk::Recheck),
        IntakeQuery::GitHubPublishRecheck {
            project: project.clone(),
        }
    );
    assert_eq!(
        query(&project, PublishAsk::Continue),
        IntakeQuery::GitHubPublishContinue {
            project: project.clone(),
        }
    );
    assert_eq!(
        query(&project, PublishAsk::Discard),
        IntakeQuery::GitHubPublishDiscard { project }
    );
}

fn github(signed_in: bool) -> IntakeEvent {
    IntakeEvent {
        query: IntakeQuery::GitHubStatus,
        result: Ok(IntakeView::GitHub(GitHubStatus {
            signed_in,
            login: signed_in.then(|| "me".to_owned()),
            method: if signed_in {
                GitHubMethod::Token
            } else {
                GitHubMethod::None
            },
            waiting: false,
            problem: None,
        })),
    }
}

fn owners(owners: Vec<GitHubOwner>) -> IntakeEvent {
    IntakeEvent {
        query: IntakeQuery::GitHubOwners,
        result: Ok(IntakeView::Owners(GitHubOwners {
            owners,
            problem: None,
        })),
    }
}

fn me() -> GitHubOwner {
    GitHubOwner {
        login: "me".into(),
        personal: true,
    }
}

fn ask_body(text: &AskText) -> String {
    text.body
        .iter()
        .map(|row: &Row| {
            row.0
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_publish_question_keeps_not_now_or_dont_ask_again_and_esc_closes_it() {
    let look = Look::plain();
    let mut pane = AskPane::default();
    let text = pane.text("Notes", "", &look);
    assert_eq!(text.title, PUBLISH_TITLE);
    assert!(ask_body(&text).contains(PROMPT_LINE), "{}", ask_body(&text));
    assert_eq!(
        text.hints,
        [
            ("Enter", "Publish"),
            ("n", "Not now"),
            ("x", "Don't ask again"),
            ("Esc", "Close"),
        ]
    );
    assert_eq!(
        pane.key(&Key::Char('n'), "Notes", ""),
        AskKey::Answer(GitHubPromptAnswer::NotNow)
    );
    assert_eq!(
        pane.key(&Key::Char('x'), "Notes", ""),
        AskKey::Answer(GitHubPromptAnswer::Never)
    );
    assert_eq!(pane.key(&Key::Char('q'), "Notes", ""), AskKey::Handled);
    assert_eq!(pane.key(&Key::Esc, "Notes", ""), AskKey::Close);
}

#[test]
fn publish_reads_the_sign_in_and_its_panel_starts_with_the_plan_the_owner_sees() {
    let look = Look::plain();
    let (name, about) = ("Notes CLI", "Keeps notes.\nAnd more.");
    let mut pane = AskPane::default();
    assert_eq!(
        pane.key(&Key::Enter, name, about),
        AskKey::Read(IntakeQuery::GitHubStatus)
    );
    assert!(pane.takes(&IntakeQuery::GitHubStatus));
    assert!(!pane.takes(&IntakeQuery::GitHubOwners));
    let checking = pane.text(name, about, &look);
    assert!(ask_body(&checking).contains("Checking your GitHub sign-in..."));
    assert_eq!(checking.hints, [("Esc", "Cancel")]);
    assert_eq!(pane.key(&Key::Enter, name, about), AskKey::Handled);
    assert_eq!(pane.arrived(&github(true)), Some(IntakeQuery::GitHubOwners));
    assert!(pane.takes(&IntakeQuery::GitHubOwners));
    assert_eq!(pane.arrived(&owners(vec![me()])), None);
    assert!(!pane.takes(&IntakeQuery::GitHubOwners));
    let text = pane.text(name, about, &look);
    let shown = ask_body(&text);
    assert!(shown.contains("> Visibility"), "{shown}");
    assert!(shown.contains("Account      < me (you) >"), "{shown}");
    assert!(shown.contains("[ Publish ]"), "{shown}");
    assert!(text.hints.contains(&("Esc", "Cancel")), "{:?}", text.hints);
    assert_eq!(pane.key(&Key::Down, name, about), AskKey::Handled);
    assert_eq!(pane.key(&Key::Down, name, about), AskKey::Handled);
    assert_eq!(
        pane.key(&Key::Paste("-2".into()), name, about),
        AskKey::Handled
    );
    for _ in 0..3 {
        assert_eq!(pane.key(&Key::Down, name, about), AskKey::Handled);
    }
    assert_eq!(
        pane.key(&Key::Enter, name, about),
        AskKey::Start(GitHubPublishPlan {
            owner: "me".into(),
            name: format!("{}-2", default_repo_name(name)),
            description: default_description(about),
            private: true,
            docs: false,
        })
    );
}

#[test]
fn esc_in_the_publish_form_starts_nothing_and_publish_moves_to_the_row_that_stops_it() {
    let look = Look::plain();
    let mut pane = AskPane::default();
    pane.key(&Key::Enter, "Notes", "");
    pane.arrived(&github(true));
    pane.arrived(&owners(Vec::new()));
    for _ in 0..5 {
        pane.key(&Key::Down, "Notes", "");
    }
    assert_eq!(pane.key(&Key::Enter, "Notes", ""), AskKey::Handled);
    let shown = ask_body(&pane.text("Notes", "", &look));
    assert!(shown.contains("> Account"), "{shown}");
    assert!(
        shown.contains("Pick the GitHub account the repository goes under."),
        "{shown}"
    );
    assert_eq!(pane.key(&Key::Esc, "Notes", ""), AskKey::Close);
}

#[test]
fn signed_out_publish_opens_the_sign_in_and_a_failed_read_tries_again() {
    let look = Look::plain();
    let mut pane = AskPane::default();
    pane.key(&Key::Enter, "Notes", "");
    assert_eq!(pane.arrived(&github(false)), None);
    let text = pane.text("Notes", "", &look);
    assert!(ask_body(&text).contains("Sign in to GitHub to publish this project."));
    assert_eq!(text.hints, [("Enter", "Sign in"), ("Esc", "Cancel")]);
    assert_eq!(pane.key(&Key::Enter, "Notes", ""), AskKey::SignIn);
    let mut failed = AskPane::default();
    failed.key(&Key::Enter, "Notes", "");
    let refused = IntakeEvent {
        query: IntakeQuery::GitHubStatus,
        result: Err("github_status_failed".into()),
    };
    assert_eq!(failed.arrived(&refused), None);
    let text = failed.text("Notes", "", &look);
    assert!(ask_body(&text).contains("CrazyAgent couldn't read your GitHub sign-in."));
    assert_eq!(text.hints, [("Enter", "Try again"), ("Esc", "Cancel")]);
    assert_eq!(
        failed.key(&Key::Enter, "Notes", ""),
        AskKey::Read(IntakeQuery::GitHubStatus)
    );
    assert!(failed.takes(&IntakeQuery::GitHubStatus));
    assert_eq!(
        failed.key(&Key::Enter, "Notes", ""),
        AskKey::Handled,
        "one read at a time"
    );
}

#[test]
fn a_discarded_publish_says_what_it_left_on_github() {
    use crate::github::GitHubPublishLeft;
    assert_eq!(left_line(None), "");
    assert_eq!(
        left_line(Some(&GitHubPublishLeft::Made {
            full_name: "me/tool".into(),
        })),
        "The empty repository me/tool stays on GitHub. Delete it there if you don't want it."
    );
    assert_eq!(
        left_line(Some(&GitHubPublishLeft::Maybe {
            full_name: "me/tool".into(),
        })),
        "GitHub may have made me/tool. Check GitHub and delete it if you don't want it."
    );
}

/// QUESTION-23:31 R1 (DEC-141): the Overview publish form says the same sign-in words in
/// both apps, and New project keeps its own sign-in wait in both.
#[test]
fn the_overview_publish_form_says_the_desktop_sign_in_words() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/desktop/src/screens/new-project/publish.ts"
    ))
    .unwrap();
    let desktop = |name: &str| -> Option<String> {
        let key = format!("export const {name} = \"");
        let start = source.find(&key)? + key.len();
        let rest = &source[start..];
        Some(rest[..rest.find('"')?].to_owned())
    };
    assert_eq!(desktop("SIGN_IN_CHECKING").as_deref(), Some(CHECKING));
    assert_eq!(desktop("OVERVIEW_SIGNED_OUT").as_deref(), Some(SIGNED_OUT));
    assert_eq!(desktop("SIGN_IN_UNREAD").as_deref(), Some(STATUS_FAILED));
    assert_eq!(
        desktop("SIGN_IN_WAIT").as_deref(),
        Some(super::new_project_publish::SIGN_IN_WAIT)
    );
}
