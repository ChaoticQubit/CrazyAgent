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
}

fn stand(rulesets: Value) -> Stand {
    Stand {
        rulesets,
        refuse: Vec::new(),
        keep_wiki: false,
    }
}

fn github(stand: Stand) -> Server {
    Server::start(move |seen, _| {
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
            "POST" if seen.path == rulesets => Answer::json(201, json!({ "id": 41 })),
            "PUT" if seen.path.starts_with(&format!("{rulesets}/")) => {
                Answer::json(200, json!({ "id": 7 }))
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
    apply(
        &Endpoints::loopback(server.port),
        &token(),
        "octo-cat/tool",
        on,
        facts,
    )
    .unwrap()
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
                GitHubRule::PrivateVulnerabilityReporting
                | GitHubRule::ActionsOutsidePrApproval => GitHubRuleState::NotApplicable {
                    reason: "public repositories only".into(),
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
fn a_second_apply_updates_crazyagents_own_rulesets_by_name_and_posts_none_twice() {
    let r = REPO;
    let server = github(stand(json!([
        { "id": 7, "name": "CrazyAgent pull requests", "source_type": "Repository" },
        { "id": 8, "name": "CrazyAgent no force push", "source_type": "Repository" },
        { "id": 9, "name": "CrazyAgent signed commits", "source_type": "Repository" },
        { "id": 10, "name": "CrazyAgent no branch deletion", "source_type": "Organization" },
    ])));
    let outcomes = applied(&server, &GitHubRule::defaults(), &private());
    let made: Vec<String> = calls(&server)
        .into_iter()
        .filter(|call| call.contains("/rulesets"))
        .collect();
    assert_eq!(
        made,
        [
            format!("GET {r}/rulesets"),
            format!("PUT {r}/rulesets/7"),
            format!("PUT {r}/rulesets/8"),
            format!("POST {r}/rulesets"),
            format!("POST {r}/rulesets"),
            format!("DELETE {r}/rulesets/9"),
            format!("POST {r}/rulesets"),
        ]
    );
    assert_eq!(
        body(&server, "PUT", &format!("{r}/rulesets/7"), 0)["name"],
        "CrazyAgent pull requests"
    );
    let signed = outcomes
        .iter()
        .find(|outcome| outcome.rule == GitHubRule::SignedCommits)
        .unwrap();
    assert!(!signed.on);
    assert_eq!(signed.state, GitHubRuleState::Applied);
}

#[test]
fn a_rule_set_later_sends_only_that_rule_and_checks_what_it_needs_first() {
    let r = REPO;
    let server = github(stand(json!([
        { "id": 7, "name": "CrazyAgent pull requests", "source_type": "Repository" },
    ])));
    let endpoints = Endpoints::loopback(server.port);
    let facts = private();
    let mut on = GitHubRule::defaults();
    on.remove(&GitHubRule::ConversationResolution);
    let outcome = set(
        &endpoints,
        &token(),
        "octo-cat/tool",
        GitHubRule::ConversationResolution,
        &on,
        &facts,
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
        let outcome = set(&endpoints, &token(), "octo-cat/tool", rule, &on, &facts).unwrap();
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
            &facts
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
    )
    .unwrap();
    assert_eq!(
        waiting.state,
        GitHubRuleState::Waiting {
            reason: "waiting for the first CI run to name its checks".into()
        }
    );
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
    )
    .unwrap_err();
    assert_eq!(refused.0, "github_token_refused");
    assert_eq!(server.seen().len(), 1);
}
