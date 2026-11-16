use super::intake::copy;
use super::project_publish::*;
use crate::github::{
    GitHubCommitted, GitHubIgnoreDraft, GitHubPublishJob, GitHubPublishPlan, GitHubPublishStep,
    GitHubRule, GitHubRuleOutcome, GitHubRuleState, GitHubSecretFinding, GitHubSecretKind,
    GitHubSecretScan,
};
use crate::protocol::ProjectId;
use crate::tui::client::intake::IntakeQuery;
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
