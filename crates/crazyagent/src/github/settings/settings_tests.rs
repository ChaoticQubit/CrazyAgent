use super::super::state;
use super::*;
use crate::credentials::test_keychain::TestKeychain;
use crate::credentials::{Keychain, SecretString, Store, reference};
use crate::github::test_server::{Answer, Seen, Server};
use crate::github::{
    Endpoints, GitHubPublishJob, GitHubPublishPlan, GitHubPublishStep, GitHubRuleOutcome,
    GitHubRuleState,
};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const TOKEN: &str = "gho_settingsstandin0123456789";
const REPO: &str = "/repos/octo-cat/tool";
const FULL_NAME: &str = "octo-cat/tool";
const PLAN: &str = "Upgrade to GitHub Pro or make this repository public to enable this feature.";

struct Scratch {
    root: tempfile::TempDir,
    namespace: String,
    store: Arc<Keychain>,
    _keychain: TestKeychain,
}

impl Scratch {
    fn new() -> Self {
        let mut bytes = [0_u8; 6];
        getrandom::fill(&mut bytes).unwrap();
        let suffix: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let namespace = format!("dev.crazyagent.test-{suffix}");
        let mut keychain = TestKeychain::temporary();
        keychain.serve(&namespace);
        Self {
            root: tempfile::tempdir().unwrap(),
            store: Arc::new(Keychain::new(&namespace).unwrap()),
            namespace,
            _keychain: keychain,
        }
    }

    fn root(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn account(&self, server: &Server) -> GitHub {
        let account = GitHub::new(
            &self.root(),
            self.namespace.clone(),
            Arc::clone(&self.store) as Arc<dyn Store>,
            Endpoints::loopback(server.port),
        );
        account
            .use_token(SecretString::new(TOKEN.as_bytes().to_vec()).unwrap())
            .unwrap();
        account
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let item = reference(&format!("{}.github", self.namespace), "token").unwrap();
        let _ = self.store.delete(&item);
    }
}

/// A GitHub stand-in that keeps one repository's settings and changes them as GitHub
/// would on each write, so a switch's own read afterwards shows what the write did.
struct Hub {
    repo: Value,
    rulesets: Vec<(u64, Value)>,
    next: u64,
    alerts: bool,
    fixes: bool,
    reporting: bool,
    allowed: String,
    verified: bool,
    workflow: String,
    fork: String,
    codeql: String,
    workflows: u64,
    checks: Vec<&'static str>,
    refuse: Vec<(&'static str, String)>,
}

fn hub(private: bool) -> Hub {
    Hub {
        repo: json!({
            "full_name": "octo-cat/tool",
            "private": private,
            "default_branch": "main",
            "allow_squash_merge": true,
            "allow_merge_commit": true,
            "allow_rebase_merge": true,
            "delete_branch_on_merge": false,
            "has_issues": true,
            "has_wiki": true,
            "has_projects": true,
            "allow_auto_merge": false,
            "security_and_analysis": {
                "secret_scanning": { "status": "disabled" },
                "secret_scanning_push_protection": { "status": "disabled" },
            },
        }),
        rulesets: Vec::new(),
        next: 40,
        alerts: false,
        fixes: false,
        reporting: false,
        allowed: "all".into(),
        verified: false,
        workflow: "write".into(),
        fork: "first_time_contributors".into(),
        codeql: "not-configured".into(),
        workflows: 1,
        checks: vec!["build"],
        refuse: Vec::new(),
    }
}

fn merge(repo: &mut Value, body: &Value) {
    for (key, value) in body.as_object().unwrap() {
        if key == "security_and_analysis" {
            for (feature, status) in value.as_object().unwrap() {
                repo["security_and_analysis"][feature] = status.clone();
            }
        } else {
            repo[key] = value.clone();
        }
    }
}

fn text(body: &Value, key: &str) -> String {
    body[key].as_str().unwrap().to_owned()
}

fn serve(hub: &mut Hub, seen: &Seen) -> Answer {
    let missing = || Answer::json(404, json!({ "message": "Not Found" }));
    let done = || Answer::bytes(204, Vec::new());
    if seen.path == "/user" {
        return Answer::json(200, json!({ "login": "octo-cat", "id": 583_231 }));
    }
    if seen.header("authorization") != Some(format!("Bearer {TOKEN}").as_str()) {
        return Answer::json(401, json!({ "message": "Bad credentials" }));
    }
    if hub
        .refuse
        .iter()
        .any(|(method, path)| *method == seen.method && *path == seen.path)
    {
        return Answer::json(403, json!({ "message": PLAN }));
    }
    let body: Value = serde_json::from_slice(&seen.body).unwrap_or_default();
    let Some(tail) = seen.path.strip_prefix(REPO) else {
        return missing();
    };
    let tail = tail.strip_prefix('/').unwrap_or(tail);
    if let Some(id) = tail.strip_prefix("rulesets/") {
        let id: u64 = id.parse().unwrap();
        let Some(at) = hub.rulesets.iter().position(|(listed, _)| *listed == id) else {
            return missing();
        };
        return match seen.method.as_str() {
            "GET" => {
                let mut detail = hub.rulesets[at].1.clone();
                detail["id"] = json!(id);
                Answer::json(200, detail)
            }
            "PUT" => {
                hub.rulesets[at].1 = body;
                Answer::json(200, json!({ "id": id }))
            }
            "DELETE" => {
                hub.rulesets.remove(at);
                done()
            }
            _ => missing(),
        };
    }
    match (seen.method.as_str(), tail) {
        ("GET", "") => Answer::json(200, hub.repo.clone()),
        ("PATCH", "") => {
            merge(&mut hub.repo, &body);
            Answer::json(200, hub.repo.clone())
        }
        ("GET", "rulesets") => Answer::json(
            200,
            Value::Array(
                hub.rulesets
                    .iter()
                    .map(|(id, body)| {
                        json!({
                            "id": id,
                            "name": body["name"],
                            "source_type": "Repository",
                            "enforcement": body["enforcement"],
                        })
                    })
                    .collect(),
            ),
        ),
        ("POST", "rulesets") => {
            hub.next += 1;
            hub.rulesets.push((hub.next, body));
            Answer::json(201, json!({ "id": hub.next }))
        }
        ("GET", "vulnerability-alerts") if hub.alerts => done(),
        ("GET", "vulnerability-alerts") => missing(),
        ("PUT", "vulnerability-alerts") => {
            hub.alerts = true;
            done()
        }
        ("DELETE", "vulnerability-alerts") => {
            hub.alerts = false;
            hub.fixes = false;
            done()
        }
        ("GET", "automated-security-fixes") if hub.fixes => {
            Answer::json(200, json!({ "enabled": true, "paused": false }))
        }
        ("GET", "automated-security-fixes") => missing(),
        ("PUT", "automated-security-fixes") => {
            hub.fixes = true;
            done()
        }
        ("DELETE", "automated-security-fixes") => {
            hub.fixes = false;
            done()
        }
        ("GET", "private-vulnerability-reporting") => {
            Answer::json(200, json!({ "enabled": hub.reporting }))
        }
        ("PUT", "private-vulnerability-reporting") => {
            hub.reporting = true;
            done()
        }
        ("DELETE", "private-vulnerability-reporting") => {
            hub.reporting = false;
            done()
        }
        ("GET", "actions/permissions") => Answer::json(
            200,
            json!({ "enabled": true, "allowed_actions": hub.allowed }),
        ),
        ("PUT", "actions/permissions") => {
            hub.allowed = text(&body, "allowed_actions");
            done()
        }
        ("GET", "actions/permissions/selected-actions") => Answer::json(
            200,
            json!({
                "github_owned_allowed": hub.verified,
                "verified_allowed": hub.verified,
                "patterns_allowed": [],
            }),
        ),
        ("PUT", "actions/permissions/selected-actions") => {
            hub.verified = body["verified_allowed"] == true && body["github_owned_allowed"] == true;
            done()
        }
        ("GET", "actions/permissions/workflow") => Answer::json(
            200,
            json!({
                "default_workflow_permissions": hub.workflow,
                "can_approve_pull_request_reviews": false,
            }),
        ),
        ("PUT", "actions/permissions/workflow") => {
            hub.workflow = text(&body, "default_workflow_permissions");
            done()
        }
        ("GET", "actions/permissions/fork-pr-contributor-approval") => {
            Answer::json(200, json!({ "approval_policy": hub.fork }))
        }
        ("PUT", "actions/permissions/fork-pr-contributor-approval") => {
            hub.fork = text(&body, "approval_policy");
            done()
        }
        ("GET", "code-scanning/default-setup") => Answer::json(200, json!({ "state": hub.codeql })),
        ("PATCH", "code-scanning/default-setup") => {
            hub.codeql = text(&body, "state");
            Answer::json(202, json!({ "run_id": 7 }))
        }
        ("GET", "actions/workflows") => Answer::json(
            200,
            json!({ "total_count": hub.workflows, "workflows": [] }),
        ),
        ("GET", "commits/main/check-runs") => Answer::json(
            200,
            json!({
                "total_count": hub.checks.len(),
                "check_runs": hub.checks.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
            }),
        ),
        _ => missing(),
    }
}

fn github(hub: Hub) -> (Arc<Mutex<Hub>>, Server) {
    let held = Arc::new(Mutex::new(hub));
    let served = Arc::clone(&held);
    let server = Server::start(move |seen, _| serve(&mut served.lock().unwrap(), seen));
    (held, server)
}

fn writes(server: &Server) -> Vec<String> {
    server
        .seen()
        .iter()
        .filter(|seen| seen.method != "GET")
        .map(|seen| {
            let path = match seen.path.split_once("/rulesets/") {
                Some((repo, _)) => format!("{repo}/rulesets/{{id}}"),
                None => seen.path.clone(),
            };
            format!("{} {path}", seen.method)
        })
        .collect()
}

fn job(project: &Path, step: GitHubPublishStep) -> GitHubPublishJob {
    GitHubPublishJob {
        project: project.to_path_buf(),
        plan: GitHubPublishPlan {
            owner: "octo-cat".into(),
            name: "tool".into(),
            description: String::new(),
            private: false,
            docs: false,
        },
        lines: Vec::new(),
        written: Vec::new(),
        step,
        stopped: None,
        made: Some("octo-cat/tool".into()),
        rules: GitHubRule::defaults(),
    }
}

fn published(scratch: &Scratch) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().canonicalize().unwrap();
    state::save_job(
        &scratch.root(),
        &job(
            &project,
            GitHubPublishStep::Done {
                full_name: "octo-cat/tool".into(),
                outcomes: Vec::new(),
            },
        ),
    )
    .unwrap();
    (folder, project)
}

fn entry(page: &GitHubRepoRules, rule: GitHubRule) -> Option<(bool, GitHubRuleState)> {
    page.rules
        .iter()
        .find(|outcome| outcome.rule == rule)
        .map(|outcome: &GitHubRuleOutcome| (outcome.on, outcome.state.clone()))
}

fn sent(rule: GitHubRule, on: bool) -> Vec<String> {
    use GitHubRule as R;
    let repo = format!("PATCH {REPO}");
    let ruleset = format!("{REPO}/rulesets");
    let at = |tail: &str| format!("{REPO}/{tail}");
    let toggle = |tail: &str| {
        let verb = if on { "PUT" } else { "DELETE" };
        vec![format!("{verb} {}", at(tail))]
    };
    match rule {
        R::SquashOnly
        | R::DeleteHeadBranches
        | R::IssuesOn
        | R::WikiOff
        | R::ProjectsOff
        | R::AutoMergeOff
        | R::SecretScanning
        | R::PushProtection => vec![repo],
        R::ConversationResolution => vec![format!("PUT {ruleset}/{{id}}")],
        R::PullRequest
        | R::NoForcePush
        | R::NoDeletion
        | R::LinearHistory
        | R::RequiredChecks
        | R::SignedCommits
        | R::TagProtection
            if on =>
        {
            vec![format!("POST {ruleset}")]
        }
        R::PullRequest
        | R::NoForcePush
        | R::NoDeletion
        | R::LinearHistory
        | R::RequiredChecks
        | R::SignedCommits
        | R::TagProtection => vec![format!("DELETE {ruleset}/{{id}}")],
        R::DependabotAlerts => toggle("vulnerability-alerts"),
        R::DependabotSecurityUpdates => toggle("automated-security-fixes"),
        R::PrivateVulnerabilityReporting => toggle("private-vulnerability-reporting"),
        R::ActionsVerifiedOnly if on => vec![
            format!("PUT {}", at("actions/permissions")),
            format!("PUT {}", at("actions/permissions/selected-actions")),
        ],
        R::ActionsVerifiedOnly => vec![format!("PUT {}", at("actions/permissions"))],
        R::ActionsReadOnlyToken => vec![format!("PUT {}", at("actions/permissions/workflow"))],
        R::ActionsOutsidePrApproval => vec![format!(
            "PUT {}",
            at("actions/permissions/fork-pr-contributor-approval")
        )],
        R::CodeqlDefaultSetup => vec![format!("PATCH {}", at("code-scanning/default-setup"))],
        R::DependabotVersionUpdates | R::SecurityPolicy => Vec::new(),
    }
}

#[test]
fn the_global_rule_defaults_start_as_dec_182_and_keep_what_the_owner_saves() {
    let scratch = Scratch::new();
    let (_hub, server) = github(hub(false));
    let account = scratch.account(&server);
    assert_eq!(
        account.rule_defaults().map_err(|error| error.0),
        Ok(GitHubRule::defaults())
    );
    let mut chosen = GitHubRule::defaults();
    chosen.insert(GitHubRule::SignedCommits);
    chosen.remove(&GitHubRule::CodeqlDefaultSetup);
    assert_eq!(
        account.set_rule_defaults(&chosen).map_err(|error| error.0),
        Ok(chosen.clone())
    );
    assert_eq!(account.rule_defaults().map_err(|error| error.0), Ok(chosen));
    let file = scratch.root().join("github-defaults.json");
    let metadata = std::fs::symlink_metadata(&file).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    std::fs::write(&file, br#"{"on":["no_such_rule"]}"#).unwrap();
    assert_eq!(
        account.rule_defaults().map_err(|error| error.0),
        Err("github_state_invalid".to_owned())
    );
    std::fs::remove_file(&file).unwrap();
    let elsewhere = scratch.root().join("elsewhere.json");
    std::fs::write(&elsewhere, br#"{"on":[]}"#).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
    assert_eq!(
        account.rule_defaults().map_err(|error| error.0),
        Err("github_config_unsafe".to_owned())
    );
    assert!(server.seen().iter().all(|seen| seen.path == "/user"));
}

#[test]
fn the_repository_page_opens_only_once_crazyagent_finished_publishing_the_project() {
    let scratch = Scratch::new();
    let (_hub, server) = github(hub(false));
    let account = scratch.account(&server);
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().canonicalize().unwrap();
    let refused = |account: &GitHub| {
        (
            account
                .repo_rules(&project)
                .map(|_| ())
                .map_err(|error| error.0),
            account
                .set_repo_rule(&project, FULL_NAME, GitHubRule::WikiOff, false)
                .map(|_| ())
                .map_err(|error| error.0),
        )
    };
    let not_published = Err("github_rules_not_published".to_owned());
    assert_eq!(
        refused(&account),
        (not_published.clone(), not_published.clone())
    );
    state::save_job(
        &scratch.root(),
        &job(
            &project,
            GitHubPublishStep::Pushed {
                full_name: "octo-cat/tool".into(),
                private: false,
                workflows: true,
            },
        ),
    )
    .unwrap();
    assert_eq!(refused(&account), (not_published.clone(), not_published));
    assert!(server.seen().iter().all(|seen| seen.path == "/user"));
    let (_published, done) = published(&scratch);
    let page = account.repo_rules(&done).unwrap();
    assert_eq!(page.full_name, "octo-cat/tool");
    assert_eq!(page.rules.len(), GitHubRule::ALL.len());
    assert!(writes(&server).is_empty(), "{:?}", writes(&server));
}

#[test]
fn a_switch_for_another_repository_than_the_publish_made_sends_nothing() {
    let scratch = Scratch::new();
    let (_hub, server) = github(hub(false));
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    let before = server.seen().len();
    for asked in [
        "octo-cat/other",
        "other/tool",
        "Octo-Cat/tool",
        "octo-cat/tool ",
        "",
    ] {
        assert_eq!(
            account
                .set_repo_rule(&project, asked, GitHubRule::WikiOff, true)
                .map(|_| ())
                .map_err(|error| error.0),
            Err("github_rules_repo_mismatch".to_owned()),
            "{asked:?}"
        );
        assert_eq!(
            server.seen().len(),
            before,
            "{asked:?} {:?}",
            writes(&server)
        );
    }
    let page = account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::WikiOff, true)
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::WikiOff),
        Some((true, GitHubRuleState::Applied))
    );
}

#[test]
fn each_switch_on_the_repository_page_sends_its_own_call_both_ways_and_answers_the_fresh_state() {
    let scratch = Scratch::new();
    let (_hub, server) = github(hub(false));
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    let switches: Vec<GitHubRule> = GitHubRule::ALL
        .into_iter()
        .filter(|rule| rule.on_github())
        .collect();
    assert_eq!(switches.len(), GitHubRule::ALL.len() - 2);
    for (pass, on) in [
        (switches.clone(), true),
        (switches.into_iter().rev().collect(), false),
    ] {
        for rule in pass {
            let before = writes(&server).len();
            let page = account
                .set_repo_rule(&project, FULL_NAME, rule, on)
                .unwrap();
            assert_eq!(
                entry(&page, rule),
                Some((on, GitHubRuleState::Applied)),
                "{rule:?} {on}"
            );
            assert_eq!(writes(&server)[before..], sent(rule, on), "{rule:?} {on}");
        }
    }
    let before = writes(&server).len();
    let page = account.repo_rules(&project).unwrap();
    assert_eq!(writes(&server).len(), before);
    assert!(
        page.rules
            .iter()
            .filter(|outcome| outcome.rule.on_github())
            .all(|outcome| !outcome.on),
        "{page:?}"
    );
}

#[test]
fn required_checks_turned_on_require_the_names_the_first_ci_run_reported() {
    let scratch = Scratch::new();
    let mut waiting = hub(false);
    waiting.checks = Vec::new();
    let (held, server) = github(waiting);
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    let page = account.repo_rules(&project).unwrap();
    assert_eq!(
        entry(&page, GitHubRule::RequiredChecks),
        Some((
            false,
            GitHubRuleState::Waiting {
                reason: "waiting for the first CI run to name its checks".into(),
            }
        ))
    );
    held.lock().unwrap().checks = vec!["test", "build"];
    let page = account.repo_rules(&project).unwrap();
    assert_eq!(page.checks, ["build", "test"]);
    assert_eq!(
        entry(&page, GitHubRule::RequiredChecks),
        Some((false, GitHubRuleState::Applied))
    );
    assert!(writes(&server).is_empty(), "{:?}", writes(&server));
    let page = account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::RequiredChecks, true)
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::RequiredChecks),
        Some((true, GitHubRuleState::Applied))
    );
    let hub = held.lock().unwrap();
    let (_, body) = hub.rulesets.last().unwrap();
    assert_eq!(body["name"], "CrazyAgent required checks");
    assert_eq!(
        body["rules"][0]["parameters"]["required_status_checks"],
        json!([{ "context": "build" }, { "context": "test" }])
    );
}

#[test]
fn a_switch_github_refuses_keeps_githubs_reason_on_its_rule() {
    let scratch = Scratch::new();
    let mut refusing = hub(true);
    refusing
        .refuse
        .push(("PATCH", format!("{REPO}/code-scanning/default-setup")));
    let (_hub, server) = github(refusing);
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    let page = account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::CodeqlDefaultSetup, true)
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::CodeqlDefaultSetup),
        Some((
            false,
            GitHubRuleState::Refused {
                status: 403,
                message: PLAN.into(),
            }
        ))
    );
    assert_eq!(
        entry(&page, GitHubRule::WikiOff),
        Some((false, GitHubRuleState::Applied))
    );
    assert_eq!(
        account
            .set_repo_rule(&project, FULL_NAME, GitHubRule::SecurityPolicy, true)
            .map(|_| ())
            .map_err(|error| error.0),
        Err("github_rule_invalid".to_owned())
    );
    assert_eq!(
        writes(&server),
        [format!("PATCH {REPO}/code-scanning/default-setup")]
    );
}

#[test]
fn pull_requests_turned_off_and_on_again_keep_the_conversation_rule_the_owner_wanted() {
    let scratch = Scratch::new();
    let (held, server) = github(hub(false));
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    for (rule, on) in [
        (GitHubRule::PullRequest, true),
        (GitHubRule::ConversationResolution, true),
        (GitHubRule::PullRequest, false),
    ] {
        account
            .set_repo_rule(&project, FULL_NAME, rule, on)
            .unwrap();
    }
    let page = account.repo_rules(&project).unwrap();
    assert_eq!(
        entry(&page, GitHubRule::ConversationResolution),
        Some((
            false,
            GitHubRuleState::NotApplicable {
                reason: "needs pull requests".into(),
            }
        ))
    );
    let record = state::load_record(&scratch.root(), "octo-cat/tool").unwrap();
    let wanted = record.on.unwrap();
    assert!(wanted.contains(&GitHubRule::ConversationResolution));
    assert!(!wanted.contains(&GitHubRule::PullRequest));
    let page = account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::PullRequest, true)
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::ConversationResolution),
        Some((true, GitHubRuleState::Applied))
    );
    let hub = held.lock().unwrap();
    let (_, body) = hub.rulesets.last().unwrap();
    assert_eq!(body["name"], "CrazyAgent pull requests");
    assert_eq!(
        body["rules"][0]["parameters"]["required_review_thread_resolution"],
        true
    );
}

#[test]
fn a_refused_switch_keeps_the_wanted_rules_github_still_holds() {
    let scratch = Scratch::new();
    let mut refusing = hub(true);
    refusing.alerts = true;
    refusing
        .refuse
        .push(("DELETE", format!("{REPO}/vulnerability-alerts")));
    let (_hub, server) = github(refusing);
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    let page = account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::DependabotAlerts, false)
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::DependabotAlerts),
        Some((
            true,
            GitHubRuleState::Refused {
                status: 403,
                message: PLAN.into(),
            }
        ))
    );
    let page = account
        .set_repo_rule(
            &project,
            FULL_NAME,
            GitHubRule::DependabotSecurityUpdates,
            true,
        )
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::DependabotSecurityUpdates),
        Some((true, GitHubRuleState::Applied))
    );
    assert_eq!(
        writes(&server),
        [
            format!("DELETE {REPO}/vulnerability-alerts"),
            format!("PUT {REPO}/automated-security-fixes"),
        ]
    );
}

/// rv-secreview gh-s7 F1: a switch for a rule CrazyAgent's ruleset holds reads that ruleset
/// as GitHub has it first and changes only CrazyAgent's own part, so what the owner set on
/// github.com stays: a rule added there, the approvals asked for, the bypass list and the
/// branches. An approved GitHub rule proposal runs this same switch.
#[test]
fn an_owner_added_rule_survives_an_approve() {
    let scratch = Scratch::new();
    let (held, server) = github(hub(false));
    let account = scratch.account(&server);
    let (_folder, project) = published(&scratch);
    account
        .set_repo_rule(&project, FULL_NAME, GitHubRule::PullRequest, true)
        .unwrap();
    let branches = json!(["~DEFAULT_BRANCH", "refs/heads/release"]);
    {
        let mut hub = held.lock().unwrap();
        assert_eq!(hub.rulesets.len(), 1);
        let (_, body) = hub.rulesets.last_mut().unwrap();
        assert_eq!(body["name"], "CrazyAgent pull requests");
        body["rules"][0]["parameters"]["required_approving_review_count"] = json!(2);
        body["rules"][0]["parameters"]["require_code_owner_review"] = json!(true);
        body["rules"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "required_signatures" }));
        body["bypass_actors"] =
            json!([{ "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }]);
        body["conditions"]["ref_name"]["include"] = branches.clone();
    }
    let page = account
        .set_repo_rule(
            &project,
            FULL_NAME,
            GitHubRule::ConversationResolution,
            true,
        )
        .unwrap();
    assert_eq!(
        entry(&page, GitHubRule::ConversationResolution),
        Some((true, GitHubRuleState::Applied))
    );
    let hub = held.lock().unwrap();
    assert_eq!(hub.rulesets.len(), 1);
    let (_, body) = &hub.rulesets[0];
    let rules = body["rules"].as_array().unwrap();
    let pull = rules
        .iter()
        .find(|rule| rule["type"] == "pull_request")
        .unwrap();
    assert_eq!(
        pull["parameters"]["required_review_thread_resolution"], true,
        "the switched rule changed"
    );
    assert_eq!(pull["parameters"]["required_approving_review_count"], 2);
    assert_eq!(pull["parameters"]["require_code_owner_review"], true);
    assert!(
        rules
            .iter()
            .any(|rule| rule["type"] == "required_signatures"),
        "{body}"
    );
    assert_eq!(body["bypass_actors"][0]["actor_id"], 5);
    assert_eq!(body["conditions"]["ref_name"]["include"], branches);
    assert_eq!(body["name"], "CrazyAgent pull requests");
    assert_eq!(body["enforcement"], "active");
}
