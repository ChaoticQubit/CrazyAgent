use super::*;
use crate::github::http::{Request, Verb};
use crate::github::test_server::{Answer, Seen, Server};
use serde_json::{Value, json};

const REPO: &str = "/repos/octo-cat/tool";
const PLAN: &str = "Upgrade to GitHub Pro or make this repository public to enable this feature.";

fn token() -> SecretString {
    SecretString::new(b"gho_rulesstandin0123456789".to_vec()).unwrap()
}

struct Stand {
    rulesets: Value,
    refuse: Vec<(&'static str, String)>,
    keep_wiki: bool,
    security_reply: Option<Value>,
}

fn stand(rulesets: Value) -> Stand {
    Stand {
        rulesets,
        refuse: Vec::new(),
        keep_wiki: false,
        security_reply: None,
    }
}

fn github(stand: Stand) -> Server {
    Server::start(move |seen, index| {
        if stand
            .refuse
            .iter()
            .any(|(method, path)| *method == seen.method && *path == seen.path)
        {
            return Answer::json(403, json!({ "message": PLAN }));
        }
        let body: Value = serde_json::from_slice(&seen.body).unwrap_or_default();
        let rulesets = format!("{REPO}/rulesets");
        match seen.method.as_str() {
            "GET" if seen.path == rulesets => Answer::json(200, stand.rulesets.clone()),
            "POST" if seen.path == rulesets => Answer::json(201, json!({ "id": 100 + index })),
            "PUT" if seen.path.starts_with(&format!("{rulesets}/")) => {
                Answer::json(200, json!({ "id": 7 }))
            }
            "PATCH" if seen.path == REPO && body.get("security_and_analysis").is_some() => {
                Answer::json(200, stand.security_reply.clone().unwrap_or(body))
            }
            "PATCH" if seen.path == REPO => {
                let mut repository = body;
                if stand.keep_wiki && repository.get("has_wiki").is_some() {
                    repository["has_wiki"] = json!(true);
                }
                Answer::json(200, repository)
            }
            "PATCH" => Answer::json(202, json!({ "run_id": 7 })),
            "PUT" | "DELETE" => Answer::bytes(204, Vec::new()),
            _ => Answer::json(404, json!({ "message": "Not Found" })),
        }
    })
}

fn calls(server: &Server) -> Vec<String> {
    server
        .seen()
        .iter()
        .map(|seen| format!("{} {}", seen.method, seen.path))
        .collect()
}

fn body(server: &Server, method: &str, path: &str, nth: usize) -> Value {
    let seen: Vec<Seen> = server
        .seen()
        .into_iter()
        .filter(|seen| seen.method == method && seen.path == path)
        .collect();
    serde_json::from_slice(&seen[nth].body).unwrap()
}

fn private() -> GitHubRepoFacts {
    GitHubRepoFacts {
        private: true,
        ci: GitHubCi::None,
    }
}

fn applied(
    server: &Server,
    on: &BTreeSet<GitHubRule>,
    facts: &GitHubRepoFacts,
) -> Vec<GitHubRuleOutcome> {
    applied_on(server, on, facts, &mut RepoRecord::default())
}

fn applied_on(
    server: &Server,
    on: &BTreeSet<GitHubRule>,
    facts: &GitHubRepoFacts,
    made: &mut RepoRecord,
) -> Vec<GitHubRuleOutcome> {
    apply(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        on,
        facts,
        made,
    )
    .unwrap()
}

fn state_of(outcomes: &[GitHubRuleOutcome], rule: GitHubRule) -> GitHubRuleState {
    outcomes
        .iter()
        .find(|outcome| outcome.rule == rule)
        .unwrap()
        .state
        .clone()
}

fn recorded(rules: &[(GitHubRule, u64)]) -> RepoRecord {
    RepoRecord {
        rulesets: rules.iter().copied().collect(),
        on: None,
    }
}

#[test]
fn a_change_request_sends_its_verb_and_a_bodyless_put_says_its_length_is_zero() {
    let server = Server::start(|_, _| Answer::bytes(204, Vec::new()));
    let request = Request {
        url: url::Url::parse(&format!("http://127.0.0.1:{}{REPO}/x", server.port)).unwrap(),
        accept: "application/json",
        token: None,
        form: None,
        headers: Vec::new(),
        allow_http: true,
    };
    for (verb, body) in [
        (Verb::Put, None),
        (Verb::Delete, None),
        (Verb::Patch, Some(r#"{"state":"configured"}"#)),
        (Verb::Post, Some("{}")),
    ] {
        assert_eq!(request.send_body(verb, body).unwrap().status, 204);
    }
    let seen = server.seen();
    let methods: Vec<&str> = seen.iter().map(|seen| seen.method.as_str()).collect();
    assert_eq!(methods, ["PUT", "DELETE", "PATCH", "POST"]);
    assert_eq!(seen[0].header("content-length"), Some("0"));
    assert!(seen[0].body.is_empty() && seen[1].body.is_empty());
    assert_eq!(seen[2].header("content-type"), Some("application/json"));
    assert_eq!(seen[2].body, br#"{"state":"configured"}"#);
}

#[test]
fn the_standard_rules_on_a_new_private_repository_make_each_call_once_in_order() {
    let server = github(stand(json!([])));
    let outcomes = applied(&server, &GitHubRule::defaults(), &private());
    let r = REPO;
    let rulesets = format!("{r}/rulesets");
    let mut expected: Vec<String> = vec![format!("PATCH {r}"); 6];
    expected.push(format!("GET {rulesets}"));
    expected.extend(vec![format!("POST {rulesets}"); 5]);
    expected.extend([
        format!("PUT {r}/vulnerability-alerts"),
        format!("PUT {r}/automated-security-fixes"),
        format!("PATCH {r}"),
        format!("PATCH {r}"),
        format!("PUT {r}/actions/permissions"),
        format!("PUT {r}/actions/permissions/selected-actions"),
        format!("PUT {r}/actions/permissions/workflow"),
        format!("PATCH {r}/code-scanning/default-setup"),
    ]);
    assert_eq!(calls(&server), expected);
    let listed = server
        .seen()
        .into_iter()
        .find(|seen| seen.method == "GET")
        .unwrap();
    let query = listed.query_pairs();
    assert_eq!(
        query.get("includes_parents").map(String::as_str),
        Some("false")
    );
    assert_eq!(query.get("per_page").map(String::as_str), Some("100"));
    let patches: Vec<Value> = (0..6).map(|nth| body(&server, "PATCH", r, nth)).collect();
    assert_eq!(
        patches,
        [
            json!({ "allow_squash_merge": true, "allow_merge_commit": false, "allow_rebase_merge": false }),
            json!({ "delete_branch_on_merge": true }),
            json!({ "has_issues": true }),
            json!({ "has_wiki": false }),
            json!({ "has_projects": false }),
            json!({ "allow_auto_merge": false }),
        ]
    );
    assert_eq!(
        body(&server, "POST", &rulesets, 0),
        json!({
            "name": "CrazyAgent pull requests",
            "target": "branch",
            "enforcement": "active",
            "bypass_actors": [],
            "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
            "rules": [{
                "type": "pull_request",
                "parameters": {
                    "required_approving_review_count": 0,
                    "dismiss_stale_reviews_on_push": false,
                    "require_code_owner_review": false,
                    "require_last_push_approval": false,
                    "required_review_thread_resolution": true
                }
            }]
        })
    );
    let names: Vec<Value> = (0..5)
        .map(|nth| body(&server, "POST", &rulesets, nth)["name"].clone())
        .collect();
    assert_eq!(
        names,
        [
            "CrazyAgent pull requests",
            "CrazyAgent no force push",
            "CrazyAgent no branch deletion",
            "CrazyAgent linear history",
            "CrazyAgent release tags"
        ]
    );
    assert_eq!(
        body(&server, "POST", &rulesets, 1)["rules"],
        json!([{ "type": "non_fast_forward" }])
    );
    assert_eq!(
        body(&server, "POST", &rulesets, 2)["rules"],
        json!([{ "type": "deletion" }])
    );
    assert_eq!(
        body(&server, "POST", &rulesets, 3)["rules"],
        json!([{ "type": "required_linear_history" }])
    );
    assert_eq!(
        body(&server, "POST", &rulesets, 4),
        json!({
            "name": "CrazyAgent release tags",
            "target": "tag",
            "enforcement": "active",
            "bypass_actors": [],
            "conditions": { "ref_name": { "include": ["~ALL"], "exclude": [] } },
            "rules": [
                { "type": "deletion" },
                { "type": "non_fast_forward" },
                { "type": "update", "parameters": { "update_allows_fetch_and_merge": false } }
            ]
        })
    );
    assert_eq!(
        body(&server, "PATCH", r, 6),
        json!({ "security_and_analysis": { "secret_scanning": { "status": "enabled" } } })
    );
    assert_eq!(
        body(&server, "PATCH", r, 7),
        json!({ "security_and_analysis": { "secret_scanning_push_protection": { "status": "enabled" } } })
    );
    assert_eq!(
        body(&server, "PUT", &format!("{r}/actions/permissions"), 0),
        json!({ "enabled": true, "allowed_actions": "selected" })
    );
    assert_eq!(
        body(
            &server,
            "PUT",
            &format!("{r}/actions/permissions/selected-actions"),
            0
        ),
        json!({ "github_owned_allowed": true, "verified_allowed": true, "patterns_allowed": [] })
    );
    assert_eq!(
        body(
            &server,
            "PUT",
            &format!("{r}/actions/permissions/workflow"),
            0
        ),
        json!({ "default_workflow_permissions": "read", "can_approve_pull_request_reviews": false })
    );
    assert_eq!(
        body(
            &server,
            "PATCH",
            &format!("{r}/code-scanning/default-setup"),
            0
        ),
        json!({ "state": "configured" })
    );
    let alerts = server
        .seen()
        .into_iter()
        .find(|seen| seen.path == format!("{r}/vulnerability-alerts"))
        .unwrap();
    assert_eq!(alerts.header("content-length"), Some("0"));
    assert!(alerts.body.is_empty());
    let expected: Vec<GitHubRuleOutcome> = GitHubRule::ALL
        .into_iter()
        .filter(|rule| {
            !matches!(
                rule,
                GitHubRule::DependabotVersionUpdates | GitHubRule::SecurityPolicy
            )
        })
        .map(|rule| GitHubRuleOutcome {
            rule,
            on: rule != GitHubRule::SignedCommits,
            state: match rule {
                GitHubRule::RequiredChecks => GitHubRuleState::NotApplicable {
                    reason: "no CI workflows in this repository".into(),
                },
                GitHubRule::PrivateVulnerabilityReporting => GitHubRuleState::NotApplicable {
                    reason: "public repositories only".into(),
                },
                GitHubRule::ActionsOutsidePrApproval => GitHubRuleState::NotApplicable {
                    reason: "a private repository runs no outside pull request workflows".into(),
                },
                _ => GitHubRuleState::Applied,
            },
        })
        .collect();
    assert_eq!(outcomes, expected);
}

#[test]
fn a_refused_rule_is_listed_with_githubs_reason_and_the_others_still_apply() {
    let r = REPO;
    let server = github(Stand {
        rulesets: json!([]),
        refuse: vec![
            ("POST", format!("{r}/rulesets")),
            ("PUT", format!("{r}/vulnerability-alerts")),
            ("PATCH", format!("{r}/code-scanning/default-setup")),
        ],
        keep_wiki: true,
    });
    let outcomes = applied(&server, &GitHubRule::defaults(), &private());
    assert_eq!(outcomes.len(), 23);
    let state = |rule: GitHubRule| {
        outcomes
            .iter()
            .find(|outcome| outcome.rule == rule)
            .unwrap()
            .state
            .clone()
    };
    let plan = GitHubRuleState::Refused {
        status: 403,
        message: PLAN.into(),
    };
    for rule in [
        GitHubRule::PullRequest,
        GitHubRule::ConversationResolution,
        GitHubRule::NoForcePush,
        GitHubRule::NoDeletion,
        GitHubRule::LinearHistory,
        GitHubRule::TagProtection,
        GitHubRule::DependabotAlerts,
        GitHubRule::CodeqlDefaultSetup,
    ] {
        assert_eq!(state(rule), plan, "{rule:?}");
    }
    assert_eq!(
        state(GitHubRule::WikiOff),
        GitHubRuleState::Refused {
            status: 200,
            message: "GitHub kept has_wiki from becoming false.".into(),
        }
    );
    for rule in [
        GitHubRule::SquashOnly,
        GitHubRule::ProjectsOff,
        GitHubRule::DependabotSecurityUpdates,
        GitHubRule::SecretScanning,
        GitHubRule::PushProtection,
        GitHubRule::ActionsVerifiedOnly,
        GitHubRule::ActionsReadOnlyToken,
    ] {
        assert_eq!(state(rule), GitHubRuleState::Applied, "{rule:?}");
    }
    assert!(calls(&server).contains(&format!("PUT {r}/actions/permissions/workflow")));
}

#[test]
fn a_second_apply_updates_the_rulesets_crazyagent_recorded_and_posts_none_twice() {
    let r = REPO;
    let server = github(stand(json!([
        { "id": 7, "name": "CrazyAgent pull requests", "source_type": "Repository" },
        { "id": 8, "name": "Team linear history", "source_type": "Repository" },
        { "id": 9, "name": "CrazyAgent signed commits", "source_type": "Repository" },
        { "id": 10, "name": "CrazyAgent no branch deletion", "source_type": "Organization" },
    ])));
    let mut made = recorded(&[
        (GitHubRule::PullRequest, 7),
        (GitHubRule::NoForcePush, 12),
        (GitHubRule::LinearHistory, 8),
        (GitHubRule::SignedCommits, 9),
    ]);
    let outcomes = applied_on(&server, &GitHubRule::defaults(), &private(), &mut made);
    let made_calls: Vec<String> = calls(&server)
        .into_iter()
        .filter(|call| call.contains("/rulesets"))
        .collect();
    assert_eq!(
        made_calls,
        [
            format!("GET {r}/rulesets"),
            format!("PUT {r}/rulesets/7"),
            format!("POST {r}/rulesets"),
            format!("POST {r}/rulesets"),
            format!("PUT {r}/rulesets/8"),
            format!("DELETE {r}/rulesets/9"),
            format!("POST {r}/rulesets"),
        ]
    );
    assert_eq!(
        body(&server, "PUT", &format!("{r}/rulesets/7"), 0)["name"],
        "CrazyAgent pull requests"
    );
    assert_eq!(
        body(&server, "PUT", &format!("{r}/rulesets/8"), 0)["name"],
        "CrazyAgent linear history"
    );
    let signed = outcomes
        .iter()
        .find(|outcome| outcome.rule == GitHubRule::SignedCommits)
        .unwrap();
    assert!(!signed.on);
    assert_eq!(signed.state, GitHubRuleState::Applied);
    let posted: Vec<u64> = server
        .seen()
        .iter()
        .enumerate()
        .filter(|(_, seen)| seen.method == "POST")
        .map(|(index, _)| 100 + index as u64)
        .collect();
    assert_eq!(posted.len(), 3);
    assert_eq!(
        made,
        recorded(&[
            (GitHubRule::PullRequest, 7),
            (GitHubRule::NoForcePush, posted[0]),
            (GitHubRule::NoDeletion, posted[1]),
            (GitHubRule::LinearHistory, 8),
            (GitHubRule::TagProtection, posted[2]),
        ])
    );
}

#[test]
fn a_rule_set_later_sends_only_that_rule_and_checks_what_it_needs_first() {
    let r = REPO;
    let server = github(stand(json!([
        { "id": 7, "name": "CrazyAgent pull requests", "source_type": "Repository" },
    ])));
    let endpoints = Endpoints::loopback(server.port);
    let facts = private();
    let mut made = recorded(&[(GitHubRule::PullRequest, 7)]);
    let mut on = GitHubRule::defaults();
    on.remove(&GitHubRule::ConversationResolution);
    let outcome = set(
        &endpoints,
        &token(),
        "octo-cat/tool",
        GitHubRule::ConversationResolution,
        &on,
        &facts,
        &mut made,
    )
    .unwrap();
    assert_eq!(
        outcome,
        GitHubRuleOutcome {
            rule: GitHubRule::ConversationResolution,
            on: false,
            state: GitHubRuleState::Applied,
        }
    );
    assert_eq!(
        calls(&server),
        [format!("GET {r}/rulesets"), format!("PUT {r}/rulesets/7")]
    );
    assert_eq!(
        body(&server, "PUT", &format!("{r}/rulesets/7"), 0)["rules"][0]["parameters"]["required_review_thread_resolution"],
        json!(false)
    );
    on.remove(&GitHubRule::DependabotAlerts);
    let alerts = set(
        &endpoints,
        &token(),
        "octo-cat/tool",
        GitHubRule::DependabotAlerts,
        &on,
        &facts,
        &mut made,
    )
    .unwrap();
    assert_eq!(alerts.state, GitHubRuleState::Applied);
    assert_eq!(
        calls(&server).last().unwrap(),
        &format!("DELETE {r}/vulnerability-alerts")
    );
    let before = server.seen().len();
    on.remove(&GitHubRule::PullRequest);
    on.insert(GitHubRule::ConversationResolution);
    on.remove(&GitHubRule::SecretScanning);
    for (rule, reason) in [
        (GitHubRule::ConversationResolution, "needs pull requests"),
        (
            GitHubRule::DependabotSecurityUpdates,
            "needs Dependabot alerts",
        ),
        (GitHubRule::PushProtection, "needs secret scanning"),
        (
            GitHubRule::DependabotVersionUpdates,
            "a file in the project, written when it is published",
        ),
    ] {
        let outcome = set(
            &endpoints,
            &token(),
            "octo-cat/tool",
            rule,
            &on,
            &facts,
            &mut made,
        )
        .unwrap();
        assert_eq!(
            outcome.state,
            GitHubRuleState::NotApplicable {
                reason: reason.into()
            },
            "{rule:?}"
        );
    }
    assert_eq!(server.seen().len(), before);
    assert_eq!(
        set(
            &endpoints,
            &token(),
            "octo-cat/../tool",
            GitHubRule::PullRequest,
            &on,
            &facts,
            &mut made,
        )
        .unwrap_err()
        .0,
        "github_repo_name_invalid"
    );
}

#[test]
fn required_checks_wait_for_the_first_ci_run_and_a_public_repository_gets_its_own_rules() {
    let r = REPO;
    let server = github(stand(json!([])));
    let endpoints = Endpoints::loopback(server.port);
    let on = GitHubRule::defaults();
    let waiting = set(
        &endpoints,
        &token(),
        "octo-cat/tool",
        GitHubRule::RequiredChecks,
        &on,
        &GitHubRepoFacts {
            private: false,
            ci: GitHubCi::Waiting,
        },
        &mut RepoRecord::default(),
    )
    .unwrap();
    assert_eq!(
        waiting.state,
        GitHubRuleState::Waiting {
            reason: "waiting for the first CI run to name its checks".into()
        }
    );
    let unnamed = set(
        &endpoints,
        &token(),
        "octo-cat/tool",
        GitHubRule::RequiredChecks,
        &on,
        &GitHubRepoFacts {
            private: false,
            ci: GitHubCi::Checks(Vec::new()),
        },
        &mut RepoRecord::default(),
    )
    .unwrap();
    assert_eq!(unnamed.state, waiting.state);
    assert!(server.seen().is_empty());
    let facts = GitHubRepoFacts {
        private: false,
        ci: GitHubCi::Checks(vec!["build".into(), "test".into()]),
    };
    let outcomes = applied(&server, &on, &facts);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.state == GitHubRuleState::Applied),
        "{outcomes:?}"
    );
    let rulesets = format!("{r}/rulesets");
    let checks = body(&server, "POST", &rulesets, 4);
    assert_eq!(checks["name"], "CrazyAgent required checks");
    assert_eq!(
        checks["rules"],
        json!([{
            "type": "required_status_checks",
            "parameters": {
                "required_status_checks": [{ "context": "build" }, { "context": "test" }],
                "strict_required_status_checks_policy": false
            }
        }])
    );
    assert_eq!(
        body(
            &server,
            "PUT",
            &format!("{r}/actions/permissions/fork-pr-contributor-approval"),
            0
        ),
        json!({ "approval_policy": "all_external_contributors" })
    );
    assert!(calls(&server).contains(&format!("PUT {r}/private-vulnerability-reporting")));
}

#[test]
fn a_sign_in_github_no_longer_accepts_stops_the_rules_with_its_refusal() {
    let server = Server::start(|_, _| Answer::json(401, json!({ "message": "Bad credentials" })));
    let refused = apply(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        &GitHubRule::defaults(),
        &private(),
        &mut RepoRecord::default(),
    )
    .unwrap_err();
    assert_eq!(refused.0, "github_token_refused");
    assert_eq!(server.seen().len(), 1);
}

#[test]
fn secret_scanning_github_does_not_confirm_is_refused_never_applied() {
    let shapes = [
        json!({ "name": "tool" }),
        json!({ "security_and_analysis": {} }),
        json!({ "security_and_analysis": {
            "secret_scanning": {},
            "secret_scanning_push_protection": {}
        } }),
        json!({ "security_and_analysis": {
            "secret_scanning": { "status": null },
            "secret_scanning_push_protection": { "status": null }
        } }),
        json!({ "security_and_analysis": {
            "secret_scanning": { "status": 1 },
            "secret_scanning_push_protection": { "status": 1 }
        } }),
    ];
    for shape in shapes {
        let server = github(Stand {
            security_reply: Some(shape.clone()),
            ..stand(json!([]))
        });
        let endpoints = Endpoints::loopback(server.port);
        for (rule, feature) in [
            (GitHubRule::SecretScanning, "secret_scanning"),
            (
                GitHubRule::PushProtection,
                "secret_scanning_push_protection",
            ),
        ] {
            let outcome = set(
                &endpoints,
                &token(),
                "octo-cat/tool",
                rule,
                &GitHubRule::defaults(),
                &private(),
                &mut RepoRecord::default(),
            )
            .unwrap();
            assert_eq!(
                outcome.state,
                GitHubRuleState::Refused {
                    status: 200,
                    message: format!("GitHub did not say whether {feature} is now enabled."),
                },
                "{shape}"
            );
        }
        let outcomes = applied(&server, &GitHubRule::defaults(), &private());
        assert!(matches!(
            state_of(&outcomes, GitHubRule::SecretScanning),
            GitHubRuleState::Refused { .. }
        ));
        assert_eq!(
            state_of(&outcomes, GitHubRule::CodeqlDefaultSetup),
            GitHubRuleState::Applied
        );
    }
    let server = github(Stand {
        security_reply: Some(
            json!({ "security_and_analysis": { "secret_scanning": { "status": "disabled" } } }),
        ),
        ..stand(json!([]))
    });
    let kept = set(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        GitHubRule::SecretScanning,
        &GitHubRule::defaults(),
        &private(),
        &mut RepoRecord::default(),
    )
    .unwrap();
    assert_eq!(
        kept.state,
        GitHubRuleState::Refused {
            status: 200,
            message: "GitHub kept secret_scanning disabled.".into(),
        }
    );
}

#[test]
fn a_ruleset_crazyagent_did_not_make_is_left_alone_and_reported() {
    let r = REPO;
    let server = github(stand(json!([
        { "id": 7, "name": "CrazyAgent pull requests", "source_type": "Repository" },
        { "id": 9, "name": "CrazyAgent signed commits", "source_type": "Repository" },
    ])));
    let mut made = RepoRecord::default();
    let outcomes = applied_on(&server, &GitHubRule::defaults(), &private(), &mut made);
    let foreign = GitHubRuleState::Refused {
        status: 0,
        message: "a ruleset with this name exists that CrazyAgent did not make".into(),
    };
    for rule in [
        GitHubRule::PullRequest,
        GitHubRule::ConversationResolution,
        GitHubRule::SignedCommits,
    ] {
        assert_eq!(state_of(&outcomes, rule), foreign, "{rule:?}");
    }
    let changed: Vec<String> = calls(&server)
        .into_iter()
        .filter(|call| call.contains("/rulesets/"))
        .collect();
    assert!(changed.is_empty(), "{changed:?}");
    let posted: Vec<u64> = server
        .seen()
        .iter()
        .enumerate()
        .filter(|(_, seen)| seen.method == "POST" && seen.path == format!("{r}/rulesets"))
        .map(|(index, _)| 100 + index as u64)
        .collect();
    let names: Vec<Value> = (0..posted.len())
        .map(|nth| body(&server, "POST", &format!("{r}/rulesets"), nth)["name"].clone())
        .collect();
    assert_eq!(
        names,
        [
            "CrazyAgent no force push",
            "CrazyAgent no branch deletion",
            "CrazyAgent linear history",
            "CrazyAgent release tags"
        ]
    );
    assert_eq!(
        made,
        recorded(&[
            (GitHubRule::NoForcePush, posted[0]),
            (GitHubRule::NoDeletion, posted[1]),
            (GitHubRule::LinearHistory, posted[2]),
            (GitHubRule::TagProtection, posted[3]),
        ])
    );
}

#[test]
fn check_names_that_cannot_be_sent_refuse_only_required_checks() {
    let r = REPO;
    let unsendable = GitHubRuleState::Refused {
        status: 0,
        message: "the check names from the CI run cannot be sent: each needs 1 to 255 bytes with no control character, and there can be at most 100".into(),
    };
    let bad: [Vec<String>; 6] = [
        vec!["build".into(), "te\u{7f}st".into()],
        vec!["line\nbreak".into()],
        vec!["c1\u{85}control".into()],
        vec![String::new()],
        vec!["x".repeat(256)],
        (0..101).map(|n| format!("check-{n}")).collect(),
    ];
    for checks in bad {
        let server = github(stand(json!([])));
        let facts = GitHubRepoFacts {
            private: true,
            ci: GitHubCi::Checks(checks.clone()),
        };
        let outcomes = applied(&server, &GitHubRule::defaults(), &facts);
        assert_eq!(
            state_of(&outcomes, GitHubRule::RequiredChecks),
            unsendable,
            "{checks:?}"
        );
        for rule in [
            GitHubRule::TagProtection,
            GitHubRule::SecretScanning,
            GitHubRule::CodeqlDefaultSetup,
        ] {
            assert_eq!(
                state_of(&outcomes, rule),
                GitHubRuleState::Applied,
                "{rule:?}"
            );
        }
        let posts = calls(&server)
            .iter()
            .filter(|call| **call == format!("POST {r}/rulesets"))
            .count();
        let names: Vec<Value> = (0..posts)
            .map(|nth| body(&server, "POST", &format!("{r}/rulesets"), nth)["name"].clone())
            .collect();
        assert!(!names.contains(&json!("CrazyAgent required checks")));
        let set_alone = set(
            &Endpoints::loopback(server.port),
            &token(),
            "octo-cat/tool",
            GitHubRule::RequiredChecks,
            &GitHubRule::defaults(),
            &facts,
            &mut RepoRecord::default(),
        )
        .unwrap();
        assert_eq!(set_alone.state, unsendable);
    }
    let server = github(stand(json!([])));
    let mut widest: Vec<String> = (0..99).map(|n| format!("check-{n}")).collect();
    widest.push("x".repeat(255));
    let outcome = set(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        GitHubRule::RequiredChecks,
        &GitHubRule::defaults(),
        &GitHubRepoFacts {
            private: true,
            ci: GitHubCi::Checks(widest),
        },
        &mut RepoRecord::default(),
    )
    .unwrap();
    assert_eq!(outcome.state, GitHubRuleState::Applied);
    let sent = body(&server, "POST", &format!("{r}/rulesets"), 0);
    assert_eq!(
        sent["rules"][0]["parameters"]["required_status_checks"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
}

#[test]
fn required_checks_go_when_ci_is_removed_and_stay_while_waiting_for_new_names() {
    let r = REPO;
    let on = GitHubRule::defaults();
    let required = |server: &Server, ci: GitHubCi, made: &mut RepoRecord| {
        set(
            &Endpoints::loopback(server.port),
            &token(),
            "octo-cat/tool",
            GitHubRule::RequiredChecks,
            &on,
            &GitHubRepoFacts { private: true, ci },
            made,
        )
        .unwrap()
    };
    let mut made = RepoRecord::default();
    let published = github(stand(json!([])));
    let with_ci = required(
        &published,
        GitHubCi::Checks(vec!["build".into()]),
        &mut made,
    );
    assert_eq!(with_ci.state, GitHubRuleState::Applied);
    assert_eq!(
        calls(&published),
        [format!("GET {r}/rulesets"), format!("POST {r}/rulesets")]
    );
    let id = made.rulesets[&GitHubRule::RequiredChecks];
    assert_eq!(id, 101);
    let listing =
        json!([{ "id": id, "name": "CrazyAgent required checks", "source_type": "Repository" }]);
    let renamed = github(stand(listing.clone()));
    let waiting = required(&renamed, GitHubCi::Waiting, &mut made);
    assert_eq!(
        waiting.state,
        GitHubRuleState::Waiting {
            reason: "waiting for the first CI run to name its checks; the checks named before stay required".into(),
        }
    );
    assert_eq!(calls(&renamed), [format!("GET {r}/rulesets")]);
    assert_eq!(made.rulesets.get(&GitHubRule::RequiredChecks), Some(&id));
    let refusing = github(Stand {
        refuse: vec![("DELETE", format!("{r}/rulesets/{id}"))],
        ..stand(listing.clone())
    });
    let kept = required(&refusing, GitHubCi::None, &mut made);
    assert_eq!(
        kept.state,
        GitHubRuleState::Refused {
            status: 403,
            message: PLAN.into(),
        }
    );
    assert_eq!(made.rulesets.get(&GitHubRule::RequiredChecks), Some(&id));
    let removed = github(stand(listing));
    let gone = required(&removed, GitHubCi::None, &mut made);
    assert_eq!(
        gone.state,
        GitHubRuleState::NotApplicable {
            reason: "no CI workflows in this repository".into(),
        }
    );
    assert_eq!(
        calls(&removed),
        [
            format!("GET {r}/rulesets"),
            format!("DELETE {r}/rulesets/{id}")
        ]
    );
    assert!(made.rulesets.is_empty());
    let after = github(stand(json!([])));
    let none = required(&after, GitHubCi::None, &mut made);
    assert_eq!(none.state, gone.state);
    assert!(after.seen().is_empty());
}

#[test]
fn a_pass_remembers_the_rules_the_owner_wants_on() {
    let server = github(stand(json!([])));
    let mut made = RepoRecord::default();
    let mut on = GitHubRule::defaults();
    on.remove(&GitHubRule::WikiOff);
    applied_on(&server, &on, &private(), &mut made);
    assert_eq!(made.on.as_ref(), Some(&on));
    on.insert(GitHubRule::SignedCommits);
    set(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        GitHubRule::SignedCommits,
        &on,
        &private(),
        &mut made,
    )
    .unwrap();
    assert_eq!(made.on.as_ref(), Some(&on));
    let older: RepoRecord = serde_json::from_str(r#"{"rulesets":{}}"#).unwrap();
    assert_eq!(older.on, None);
    assert_eq!(
        serde_json::to_string(&RepoRecord::default()).unwrap(),
        r#"{"rulesets":{}}"#
    );
}

const FILE: &str = "a file in the project, written when it is published";
const OURS: [(GitHubRule, u64, &str); 7] = [
    (GitHubRule::PullRequest, 11, "CrazyAgent pull requests"),
    (GitHubRule::NoForcePush, 12, "CrazyAgent no force push"),
    (GitHubRule::NoDeletion, 13, "CrazyAgent no branch deletion"),
    (GitHubRule::LinearHistory, 14, "CrazyAgent linear history"),
    (GitHubRule::RequiredChecks, 15, "CrazyAgent required checks"),
    (GitHubRule::SignedCommits, 16, "CrazyAgent signed commits"),
    (GitHubRule::TagProtection, 17, "CrazyAgent release tags"),
];

type Reads = Vec<(String, u16, Option<Value>)>;
type Shown = Vec<(GitHubRule, bool, GitHubRuleState)>;

fn reads(answers: Reads) -> Server {
    Server::start(move |seen, _| {
        if seen.method != "GET" {
            return Answer::json(500, json!({ "message": "a page read sent a write" }));
        }
        match answers.iter().find(|(path, _, _)| *path == seen.path) {
            Some((_, status, Some(body))) => Answer::json(*status, body.clone()),
            Some((_, status, None)) => Answer::bytes(*status, Vec::new()),
            None => Answer::json(404, json!({ "message": "Not Found" })),
        }
    })
}

fn listed(enforcement: &[(u64, &str)]) -> Value {
    Value::Array(
        OURS.iter()
            .map(|(_, id, name)| {
                let state = enforcement
                    .iter()
                    .find(|(listed, _)| listed == id)
                    .map_or("active", |(_, state)| *state);
                json!({
                    "id": id,
                    "name": name,
                    "source_type": "Repository",
                    "enforcement": state,
                })
            })
            .collect(),
    )
}

fn ours() -> RepoRecord {
    recorded(&OURS.map(|(rule, id, _)| (rule, id)))
}

fn secure(private: bool) -> Reads {
    let r = REPO;
    vec![
        (
            r.to_owned(),
            200,
            Some(json!({
                "full_name": "octo-cat/tool",
                "private": private,
                "default_branch": "main",
                "allow_squash_merge": true,
                "allow_merge_commit": false,
                "allow_rebase_merge": false,
                "delete_branch_on_merge": true,
                "has_issues": true,
                "has_wiki": false,
                "has_projects": false,
                "allow_auto_merge": false,
                "security_and_analysis": {
                    "secret_scanning": { "status": "enabled" },
                    "secret_scanning_push_protection": { "status": "enabled" },
                },
            })),
        ),
        (format!("{r}/rulesets"), 200, Some(listed(&[]))),
        (
            format!("{r}/rulesets/11"),
            200,
            Some(json!({
                "id": 11,
                "name": "CrazyAgent pull requests",
                "enforcement": "active",
                "rules": [{
                    "type": "pull_request",
                    "parameters": {
                        "required_approving_review_count": 0,
                        "required_review_thread_resolution": true,
                    },
                }],
            })),
        ),
        (format!("{r}/vulnerability-alerts"), 204, None),
        (
            format!("{r}/automated-security-fixes"),
            200,
            Some(json!({ "enabled": true, "paused": false })),
        ),
        (
            format!("{r}/private-vulnerability-reporting"),
            200,
            Some(json!({ "enabled": true })),
        ),
        (
            format!("{r}/actions/permissions"),
            200,
            Some(json!({ "enabled": true, "allowed_actions": "selected" })),
        ),
        (
            format!("{r}/actions/permissions/selected-actions"),
            200,
            Some(json!({
                "github_owned_allowed": true,
                "verified_allowed": true,
                "patterns_allowed": [],
            })),
        ),
        (
            format!("{r}/actions/permissions/workflow"),
            200,
            Some(json!({
                "default_workflow_permissions": "read",
                "can_approve_pull_request_reviews": false,
            })),
        ),
        (
            format!("{r}/actions/permissions/fork-pr-contributor-approval"),
            200,
            Some(json!({ "approval_policy": "all_external_contributors" })),
        ),
        (
            format!("{r}/code-scanning/default-setup"),
            200,
            Some(json!({ "state": "configured", "languages": ["rust"] })),
        ),
        (
            format!("{r}/actions/workflows"),
            200,
            Some(json!({ "total_count": 1, "workflows": [] })),
        ),
        (
            format!("{r}/commits/main/check-runs"),
            200,
            Some(json!({
                "total_count": 3,
                "check_runs": [{ "name": "test" }, { "name": "build" }, { "name": "test" }],
            })),
        ),
    ]
}

fn answer(answers: &mut Reads, tail: &str, status: u16, body: Option<Value>) {
    let path = if tail.is_empty() {
        REPO.to_owned()
    } else {
        format!("{REPO}/{tail}")
    };
    let at = answers
        .iter_mut()
        .find(|(listed, _, _)| *listed == path)
        .expect("a read the stand answers");
    at.1 = status;
    at.2 = body;
}

fn page_of(server: &Server, made: &RepoRecord) -> (GitHubRepoRules, GitHubRepoFacts) {
    read(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        made,
    )
    .unwrap()
}

fn shown(page: &GitHubRepoRules) -> Shown {
    page.rules
        .iter()
        .map(|outcome| (outcome.rule, outcome.on, outcome.state.clone()))
        .collect()
}

fn each(state: impl Fn(GitHubRule) -> (bool, GitHubRuleState)) -> Shown {
    GitHubRule::ALL
        .into_iter()
        .map(|rule| {
            let (on, state) = state(rule);
            (rule, on, state)
        })
        .collect()
}

fn sorted(server: &Server) -> Vec<String> {
    let mut seen = calls(server);
    seen.sort();
    seen
}

fn every_get(tails: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = tails
        .iter()
        .map(|tail| {
            if tail.is_empty() {
                format!("GET {REPO}")
            } else {
                format!("GET {REPO}/{tail}")
            }
        })
        .collect();
    expected.sort();
    expected
}

#[test]
fn the_settings_page_reads_each_rule_from_its_own_get_and_sends_no_write() {
    let server = reads(secure(false));
    let (page, facts) = page_of(&server, &ours());
    assert_eq!(
        shown(&page),
        each(|rule| if rule.on_github() {
            (true, GitHubRuleState::Applied)
        } else {
            (false, not_applicable(FILE))
        })
    );
    assert_eq!(page.full_name, "octo-cat/tool");
    assert!(!page.private);
    assert_eq!(page.checks, ["build", "test"]);
    assert_eq!(
        facts,
        GitHubRepoFacts {
            private: false,
            ci: GitHubCi::Checks(vec!["build".into(), "test".into()]),
        }
    );
    assert_eq!(
        sorted(&server),
        every_get(&[
            "",
            "rulesets",
            "rulesets/11",
            "vulnerability-alerts",
            "automated-security-fixes",
            "private-vulnerability-reporting",
            "actions/permissions",
            "actions/permissions/selected-actions",
            "actions/permissions/workflow",
            "actions/permissions/fork-pr-contributor-approval",
            "code-scanning/default-setup",
            "actions/workflows",
            "commits/main/check-runs",
        ])
    );
    let seen = server.seen();
    let rulesets = seen
        .iter()
        .find(|seen| seen.path == format!("{REPO}/rulesets"))
        .unwrap();
    assert_eq!(
        rulesets
            .query_pairs()
            .get("includes_parents")
            .map(String::as_str),
        Some("false")
    );
}

#[test]
fn a_repository_on_githubs_own_defaults_reads_every_switch_off() {
    let mut answers = secure(false);
    answer(
        &mut answers,
        "",
        200,
        Some(json!({
            "full_name": "octo-cat/tool",
            "private": false,
            "default_branch": "main",
            "allow_squash_merge": true,
            "allow_merge_commit": true,
            "allow_rebase_merge": true,
            "delete_branch_on_merge": false,
            "has_issues": false,
            "has_wiki": true,
            "has_projects": true,
            "allow_auto_merge": true,
            "security_and_analysis": {
                "secret_scanning": { "status": "disabled" },
                "secret_scanning_push_protection": { "status": "disabled" },
            },
        })),
    );
    answer(&mut answers, "rulesets", 200, Some(json!([])));
    answer(
        &mut answers,
        "vulnerability-alerts",
        404,
        Some(json!({ "message": "Not Found" })),
    );
    answer(
        &mut answers,
        "automated-security-fixes",
        404,
        Some(json!({ "message": "Not Found" })),
    );
    answer(
        &mut answers,
        "private-vulnerability-reporting",
        200,
        Some(json!({ "enabled": false })),
    );
    answer(
        &mut answers,
        "actions/permissions",
        200,
        Some(json!({ "enabled": true, "allowed_actions": "all" })),
    );
    answer(
        &mut answers,
        "actions/permissions/workflow",
        200,
        Some(json!({
            "default_workflow_permissions": "write",
            "can_approve_pull_request_reviews": false,
        })),
    );
    answer(
        &mut answers,
        "actions/permissions/fork-pr-contributor-approval",
        200,
        Some(json!({ "approval_policy": "first_time_contributors" })),
    );
    answer(
        &mut answers,
        "code-scanning/default-setup",
        200,
        Some(json!({ "state": "not-configured" })),
    );
    answer(
        &mut answers,
        "actions/workflows",
        200,
        Some(json!({ "total_count": 0, "workflows": [] })),
    );
    let server = reads(answers);
    let (page, facts) = page_of(&server, &RepoRecord::default());
    assert_eq!(
        shown(&page),
        each(|rule| match rule {
            GitHubRule::ConversationResolution => (false, not_applicable("needs pull requests")),
            GitHubRule::RequiredChecks => (false, not_applicable(NO_CI)),
            GitHubRule::DependabotVersionUpdates | GitHubRule::SecurityPolicy => {
                (false, not_applicable(FILE))
            }
            _ => (false, GitHubRuleState::Applied),
        })
    );
    assert_eq!(
        facts,
        GitHubRepoFacts {
            private: false,
            ci: GitHubCi::None,
        }
    );
    assert!(page.checks.is_empty());
    let asked = calls(&server);
    for tail in [
        "rulesets/11",
        "actions/permissions/selected-actions",
        "commits/main/check-runs",
    ] {
        assert!(!asked.contains(&format!("GET {REPO}/{tail}")), "{asked:?}");
    }
}

#[test]
fn required_checks_wait_for_the_first_ci_run_then_show_its_names_and_a_read_makes_nothing() {
    let mut answers = secure(false);
    let mut listing = listed(&[]);
    listing
        .as_array_mut()
        .unwrap()
        .retain(|listed| listed["id"] != 15);
    answer(&mut answers, "rulesets", 200, Some(listing));
    answer(
        &mut answers,
        "commits/main/check-runs",
        200,
        Some(json!({ "total_count": 0, "check_runs": [] })),
    );
    let mut record = ours();
    record.rulesets.remove(&GitHubRule::RequiredChecks);
    record.on = Some(GitHubRule::defaults());
    let state_of_checks = |page: &GitHubRepoRules| {
        shown(page)
            .into_iter()
            .find(|(rule, _, _)| *rule == GitHubRule::RequiredChecks)
            .map(|(_, on, state)| (on, state))
    };
    let waiting = reads(answers.clone());
    let (page, facts) = page_of(&waiting, &record);
    assert_eq!(
        state_of_checks(&page),
        Some((
            false,
            GitHubRuleState::Waiting {
                reason: WAITING.into(),
            }
        ))
    );
    assert_eq!(facts.ci, GitHubCi::Waiting);
    assert!(page.checks.is_empty());
    answer(
        &mut answers,
        "commits/main/check-runs",
        200,
        Some(json!({
            "total_count": 2,
            "check_runs": [{ "name": "lint" }, { "name": "build" }],
        })),
    );
    let named = reads(answers);
    let (page, facts) = page_of(&named, &record);
    assert_eq!(
        state_of_checks(&page),
        Some((false, GitHubRuleState::Applied))
    );
    assert_eq!(page.checks, ["build", "lint"]);
    assert_eq!(
        facts.ci,
        GitHubCi::Checks(vec!["build".into(), "lint".into()])
    );
    for server in [&waiting, &named] {
        assert!(
            calls(server).iter().all(|call| call.starts_with("GET ")),
            "{:?}",
            calls(server)
        );
    }
}

#[test]
fn a_read_github_refuses_shows_its_reason_and_the_other_rules_still_read() {
    let mut answers = secure(true);
    answer(
        &mut answers,
        "code-scanning/default-setup",
        403,
        Some(json!({ "message": PLAN })),
    );
    answer(
        &mut answers,
        "rulesets",
        403,
        Some(json!({ "message": PLAN })),
    );
    let server = reads(answers);
    let (page, facts) = page_of(&server, &ours());
    let plan = GitHubRuleState::Refused {
        status: 403,
        message: PLAN.into(),
    };
    assert_eq!(
        shown(&page),
        each(|rule| match rule {
            GitHubRule::PullRequest
            | GitHubRule::ConversationResolution
            | GitHubRule::NoForcePush
            | GitHubRule::NoDeletion
            | GitHubRule::LinearHistory
            | GitHubRule::RequiredChecks
            | GitHubRule::SignedCommits
            | GitHubRule::TagProtection
            | GitHubRule::CodeqlDefaultSetup => (false, plan.clone()),
            GitHubRule::PrivateVulnerabilityReporting => {
                (false, not_applicable("public repositories only"))
            }
            GitHubRule::ActionsOutsidePrApproval => (
                false,
                not_applicable("a private repository runs no outside pull request workflows"),
            ),
            GitHubRule::DependabotVersionUpdates | GitHubRule::SecurityPolicy => {
                (false, not_applicable(FILE))
            }
            _ => (true, GitHubRuleState::Applied),
        })
    );
    assert!(page.private && facts.private);
    let asked = calls(&server);
    for tail in [
        "rulesets/11",
        "private-vulnerability-reporting",
        "actions/permissions/fork-pr-contributor-approval",
    ] {
        assert!(!asked.contains(&format!("GET {REPO}/{tail}")), "{asked:?}");
    }
}

#[test]
fn a_ruleset_turned_off_on_github_or_made_by_someone_else_reads_off() {
    let mut answers = secure(false);
    let mut listing = listed(&[(11, "disabled")]);
    for listed in listing.as_array_mut().unwrap() {
        if listed["id"] == 12 {
            listed["id"] = json!(99);
        }
    }
    answer(&mut answers, "rulesets", 200, Some(listing));
    let server = reads(answers);
    let (page, _) = page_of(&server, &ours());
    assert_eq!(
        shown(&page),
        each(|rule| match rule {
            GitHubRule::PullRequest => (false, GitHubRuleState::Applied),
            GitHubRule::ConversationResolution => (false, not_applicable("needs pull requests")),
            GitHubRule::NoForcePush => (false, not_sent(FOREIGN)),
            GitHubRule::DependabotVersionUpdates | GitHubRule::SecurityPolicy => {
                (false, not_applicable(FILE))
            }
            _ => (true, GitHubRuleState::Applied),
        })
    );
    assert!(!calls(&server).contains(&format!("GET {REPO}/rulesets/11")));
}

#[test]
fn a_repository_github_will_not_show_stops_the_page_before_any_other_read() {
    let mut answers = secure(false);
    answer(
        &mut answers,
        "",
        404,
        Some(json!({ "message": "Not Found" })),
    );
    let server = reads(answers);
    let endpoints = Endpoints::loopback(server.port);
    assert_eq!(
        read(&endpoints, &token(), "octo-cat/tool", &ours())
            .map(|_| ())
            .map_err(|error| error.0),
        Err("github_repo_unreadable".to_owned())
    );
    assert_eq!(calls(&server), [format!("GET {REPO}")]);
    let refused = Server::start(|_, _| Answer::json(401, json!({ "message": "Bad credentials" })));
    assert_eq!(
        read(
            &Endpoints::loopback(refused.port),
            &token(),
            "octo-cat/tool",
            &ours()
        )
        .map(|_| ())
        .map_err(|error| error.0),
        Err("github_token_refused".to_owned())
    );
    assert_eq!(
        read(&endpoints, &token(), "octo-cat/../x", &ours())
            .map(|_| ())
            .map_err(|error| error.0),
        Err("github_repo_name_invalid".to_owned())
    );
}
