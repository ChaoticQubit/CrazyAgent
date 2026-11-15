use super::api::{self, Endpoints};
use super::http::{Reply, Verb};
use crate::credentials::SecretString;
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(all(test, target_os = "macos"))]
mod rules_tests;

const RULESETS_PER_PAGE: usize = 100;
const MESSAGE_SHOWN_MAX: usize = 300;
const HELD_SHOWN_MAX: usize = 40;

/// One rule of CrazyAgent's standard secure setup for a published repository (DEC-182).
/// Each is a switch on the GitHub settings page, and all start on except signed commits.
/// `ALL` lists them in the order they are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubRule {
    SquashOnly,
    DeleteHeadBranches,
    IssuesOn,
    WikiOff,
    ProjectsOff,
    AutoMergeOff,
    PullRequest,
    ConversationResolution,
    NoForcePush,
    NoDeletion,
    LinearHistory,
    RequiredChecks,
    SignedCommits,
    TagProtection,
    DependabotAlerts,
    DependabotSecurityUpdates,
    SecretScanning,
    PushProtection,
    PrivateVulnerabilityReporting,
    ActionsVerifiedOnly,
    ActionsReadOnlyToken,
    ActionsOutsidePrApproval,
    CodeqlDefaultSetup,
    DependabotVersionUpdates,
    SecurityPolicy,
}

impl GitHubRule {
    pub const ALL: [Self; 25] = [
        Self::SquashOnly,
        Self::DeleteHeadBranches,
        Self::IssuesOn,
        Self::WikiOff,
        Self::ProjectsOff,
        Self::AutoMergeOff,
        Self::PullRequest,
        Self::ConversationResolution,
        Self::NoForcePush,
        Self::NoDeletion,
        Self::LinearHistory,
        Self::RequiredChecks,
        Self::SignedCommits,
        Self::TagProtection,
        Self::DependabotAlerts,
        Self::DependabotSecurityUpdates,
        Self::SecretScanning,
        Self::PushProtection,
        Self::PrivateVulnerabilityReporting,
        Self::ActionsVerifiedOnly,
        Self::ActionsReadOnlyToken,
        Self::ActionsOutsidePrApproval,
        Self::CodeqlDefaultSetup,
        Self::DependabotVersionUpdates,
        Self::SecurityPolicy,
    ];

    /// The rules on for a new repository: every rule but signed commits (DEC-182).
    /// Output: the set of rules that start on.
    pub fn defaults() -> BTreeSet<Self> {
        Self::ALL
            .into_iter()
            .filter(|rule| *rule != Self::SignedCommits)
            .collect()
    }

    /// Tells whether GitHub's settings hold the rule. Dependabot version updates and the
    /// security policy are files the publish writes into the project instead.
    /// Output: true for a rule set through GitHub's API.
    fn on_github(self) -> bool {
        !matches!(self, Self::DependabotVersionUpdates | Self::SecurityPolicy)
    }

    /// The name of the CrazyAgent ruleset that holds the rule, for the rules a ruleset
    /// holds. Conversation resolution lives in the pull request ruleset and has none.
    /// Output: the name, or None.
    fn ruleset(self) -> Option<&'static str> {
        Some(match self {
            Self::PullRequest => "CrazyAgent pull requests",
            Self::NoForcePush => "CrazyAgent no force push",
            Self::NoDeletion => "CrazyAgent no branch deletion",
            Self::LinearHistory => "CrazyAgent linear history",
            Self::RequiredChecks => "CrazyAgent required checks",
            Self::SignedCommits => "CrazyAgent signed commits",
            Self::TagProtection => "CrazyAgent release tags",
            _ => return None,
        })
    }
}

/// What became of one rule: applied, refused by GitHub with its status and message, not
/// applicable to this repository with the reason, or waiting for something first, such as
/// the first CI run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubRuleState {
    Applied,
    Refused { status: u16, message: String },
    NotApplicable { reason: String },
    Waiting { reason: String },
}

/// One rule, whether it was set on or off, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubRuleOutcome {
    pub rule: GitHubRule,
    pub on: bool,
    pub state: GitHubRuleState,
}

/// The CI a repository has, for the required-checks rule: none, workflows that have not
/// run yet, or the check names its first run reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubCi {
    None,
    Waiting,
    Checks(Vec<String>),
}

/// What the rules need to know about a repository that its settings calls do not
/// return: whether it is private, and its CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepoFacts {
    pub private: bool,
    pub ci: GitHubCi,
}

/// What CrazyAgent remembers about one repository it set rules on: the id of each ruleset
/// it made, by the rule the ruleset holds, so a pass only ever changes CrazyAgent's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepoRecord {
    #[serde(default)]
    pub(super) rulesets: BTreeMap<GitHubRule, u64>,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn not_applicable(reason: &str) -> GitHubRuleState {
    GitHubRuleState::NotApplicable {
        reason: reason.to_owned(),
    }
}

/// The refused state for a reply outside a call's success statuses: GitHub's status and
/// its message, escaped and bounded, or a plain sentence when it sent none.
/// Input: the reply. Output: the state.
fn refused(reply: &Reply) -> GitHubRuleState {
    #[derive(Deserialize)]
    struct Said {
        message: String,
    }
    let message = serde_json::from_slice::<Said>(&reply.body)
        .ok()
        .map(|said| api::shown(&said.message, MESSAGE_SHOWN_MAX))
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| format!("GitHub answered {}.", reply.status));
    GitHubRuleState::Refused {
        status: reply.status,
        message,
    }
}

/// Sets every rule GitHub holds on one repository in `ALL`'s order: each rule in `on` is
/// turned on and every other one off, and one refusal never stops the rest.
/// Inputs: endpoints, token, `owner/name`, the rules on, the facts and the repository's
/// record. Output: one outcome per rule, or `github_token_refused`, `github_unreachable`.
pub(super) fn apply(
    endpoints: &Endpoints,
    token: &SecretString,
    full_name: &str,
    on: &BTreeSet<GitHubRule>,
    facts: &GitHubRepoFacts,
    made: &mut RepoRecord,
) -> Result<Vec<GitHubRuleOutcome>, EngineError> {
    assert!(made.rulesets.len() <= GitHubRule::ALL.len());
    let mut pass = Pass::new(endpoints, token, full_name, on, facts)?;
    let mut outcomes: Vec<GitHubRuleOutcome> = Vec::new();
    for rule in GitHubRule::ALL.into_iter().filter(|rule| rule.on_github()) {
        let state = match outcomes.last() {
            Some(pull)
                if rule == GitHubRule::ConversationResolution
                    && on.contains(&GitHubRule::PullRequest) =>
            {
                assert_eq!(pull.rule, GitHubRule::PullRequest);
                pull.state.clone()
            }
            _ => pass.state(rule)?,
        };
        outcomes.push(GitHubRuleOutcome {
            rule,
            on: on.contains(&rule),
            state,
        });
    }
    assert_eq!(
        outcomes.len(),
        GitHubRule::ALL
            .iter()
            .filter(|rule| rule.on_github())
            .count()
    );
    Ok(outcomes)
}

/// Sets one rule on one repository, as `apply` does for each. Inputs: endpoints, token,
/// `owner/name`, the rule, the rules on, the facts and the repository's record.
/// Output: the rule's outcome, or `github_token_refused`, `github_unreachable`.
pub(super) fn set(
    endpoints: &Endpoints,
    token: &SecretString,
    full_name: &str,
    rule: GitHubRule,
    on: &BTreeSet<GitHubRule>,
    facts: &GitHubRepoFacts,
    made: &mut RepoRecord,
) -> Result<GitHubRuleOutcome, EngineError> {
    assert!(made.rulesets.len() <= GitHubRule::ALL.len());
    let mut pass = Pass::new(endpoints, token, full_name, on, facts)?;
    Ok(GitHubRuleOutcome {
        rule,
        on: on.contains(&rule),
        state: pass.state(rule)?,
    })
}

/// One of the repository's own rulesets, as GitHub lists it.
#[derive(Deserialize)]
struct Listed {
    id: u64,
    name: String,
    #[serde(default)]
    source_type: Option<String>,
}

/// One pass of setting rules on one repository, which reads its rulesets at most once.
struct Pass<'a> {
    endpoints: &'a Endpoints,
    token: &'a SecretString,
    full_name: &'a str,
    on: &'a BTreeSet<GitHubRule>,
    facts: &'a GitHubRepoFacts,
    rulesets: Option<Result<Vec<Listed>, GitHubRuleState>>,
}

impl<'a> Pass<'a> {
    /// Starts a pass once the repository name is one CrazyAgent accepts. Inputs: as
    /// `apply`. Output: the pass, or `github_repo_name_invalid`.
    fn new(
        endpoints: &'a Endpoints,
        token: &'a SecretString,
        full_name: &'a str,
        on: &'a BTreeSet<GitHubRule>,
        facts: &'a GitHubRepoFacts,
    ) -> Result<Self, EngineError> {
        super::clone::full_name(full_name)?;
        Ok(Self {
            endpoints,
            token,
            full_name,
            on,
            facts,
            rulesets: None,
        })
    }

    /// Sets one rule to its place in `on`, after checking what it needs: a rule another
    /// one depends on, a public repository, or a file the publish writes instead.
    /// Input: the rule. Output: its state.
    fn state(&mut self, rule: GitHubRule) -> Result<GitHubRuleState, EngineError> {
        use GitHubRule as R;
        let on = self.on;
        let wanted = on.contains(&rule);
        let private = self.facts.private;
        match rule {
            R::SquashOnly
            | R::DeleteHeadBranches
            | R::IssuesOn
            | R::WikiOff
            | R::ProjectsOff
            | R::AutoMergeOff => self.fields(&fields(rule, wanted)),
            R::ConversationResolution if !on.contains(&R::PullRequest) => Ok(if wanted {
                not_applicable("needs pull requests")
            } else {
                GitHubRuleState::Applied
            }),
            R::ConversationResolution => self.ruleset(R::PullRequest),
            R::PullRequest
            | R::NoForcePush
            | R::NoDeletion
            | R::LinearHistory
            | R::RequiredChecks
            | R::SignedCommits
            | R::TagProtection => self.ruleset(rule),
            R::DependabotAlerts => self.toggle("vulnerability-alerts", wanted),
            R::DependabotSecurityUpdates if wanted && !on.contains(&R::DependabotAlerts) => {
                Ok(not_applicable("needs Dependabot alerts"))
            }
            R::DependabotSecurityUpdates => self.toggle("automated-security-fixes", wanted),
            R::SecretScanning => self.security("secret_scanning", wanted),
            R::PushProtection if wanted && !on.contains(&R::SecretScanning) => {
                Ok(not_applicable("needs secret scanning"))
            }
            R::PushProtection => self.security("secret_scanning_push_protection", wanted),
            R::PrivateVulnerabilityReporting if private => {
                Ok(not_applicable("public repositories only"))
            }
            R::ActionsOutsidePrApproval if private => Ok(not_applicable(
                "a private repository runs no outside pull request workflows",
            )),
            R::PrivateVulnerabilityReporting => {
                self.toggle("private-vulnerability-reporting", wanted)
            }
            R::ActionsVerifiedOnly => self.allowed_actions(wanted),
            R::ActionsReadOnlyToken => {
                let permissions = if wanted { "read" } else { "write" };
                let body = json!({
                    "default_workflow_permissions": permissions,
                    "can_approve_pull_request_reviews": false,
                });
                self.send(
                    Verb::Put,
                    "actions/permissions/workflow",
                    Some(&body),
                    &[204],
                )
            }
            R::ActionsOutsidePrApproval => {
                let policy = if wanted {
                    "all_external_contributors"
                } else {
                    "first_time_contributors"
                };
                let body = json!({ "approval_policy": policy });
                self.send(
                    Verb::Put,
                    "actions/permissions/fork-pr-contributor-approval",
                    Some(&body),
                    &[204],
                )
            }
            R::CodeqlDefaultSetup => {
                let state = if wanted {
                    "configured"
                } else {
                    "not-configured"
                };
                let body = json!({ "state": state });
                self.send(
                    Verb::Patch,
                    "code-scanning/default-setup",
                    Some(&body),
                    &[200, 202],
                )
            }
            R::DependabotVersionUpdates | R::SecurityPolicy => Ok(not_applicable(
                "a file in the project, written when it is published",
            )),
        }
    }

    /// The path of a settings call for this repository. Input: the part below
    /// `repos/{owner}/{name}`, empty for the repository itself. Output: the API path.
    fn path(&self, tail: &str) -> String {
        if tail.is_empty() {
            format!("repos/{}", self.full_name)
        } else {
            format!("repos/{}/{tail}", self.full_name)
        }
    }

    /// Sends one settings call. Inputs: the method, the path below the repository and the
    /// JSON body. Output: the reply, or `github_token_refused` on a 401.
    fn call(&self, verb: Verb, tail: &str, body: Option<&Value>) -> Result<Reply, EngineError> {
        let url = self.endpoints.api(&self.path(tail))?;
        let text = body.map(Value::to_string);
        let reply = self
            .endpoints
            .request(url, api::API_JSON, Some(self.token), None)
            .send_body(verb, text.as_deref())?;
        if reply.status == 401 {
            return Err(failure("github_token_refused"));
        }
        Ok(reply)
    }

    /// Sends one settings call and reads its state from the status. Inputs: as `call`,
    /// and the call's success statuses. Output: applied, or refused with GitHub's reason.
    fn send(
        &self,
        verb: Verb,
        tail: &str,
        body: Option<&Value>,
        ok: &[u16],
    ) -> Result<GitHubRuleState, EngineError> {
        assert!(!ok.is_empty());
        let reply = self.call(verb, tail, body)?;
        Ok(if ok.contains(&reply.status) {
            GitHubRuleState::Applied
        } else {
            refused(&reply)
        })
    }

    /// Turns a feature GitHub switches with PUT and DELETE on or off. Inputs: the path
    /// below the repository and whether it is on. Output: the state.
    fn toggle(&self, tail: &str, wanted: bool) -> Result<GitHubRuleState, EngineError> {
        let verb = if wanted { Verb::Put } else { Verb::Delete };
        self.send(verb, tail, None, &[204])
    }

    /// PATCHes repository fields and checks GitHub kept each one, since an organization
    /// policy can hold a field without refusing the call. Input: the fields.
    /// Output: the state.
    fn fields(&self, fields: &Value) -> Result<GitHubRuleState, EngineError> {
        let reply = self.call(Verb::Patch, "", Some(fields))?;
        if reply.status != 200 {
            return Ok(refused(&reply));
        }
        let repository: Value = api::json(&reply)?;
        let sent = fields.as_object().expect("repository fields are an object");
        assert!(!sent.is_empty());
        Ok(
            match sent
                .iter()
                .find(|(field, value)| repository.get(field.as_str()) != Some(*value))
            {
                Some((field, value)) => GitHubRuleState::Refused {
                    status: reply.status,
                    message: format!("GitHub kept {field} from becoming {value}."),
                },
                None => GitHubRuleState::Applied,
            },
        )
    }

    /// Turns a security and analysis feature on or off and checks the status GitHub
    /// answers with, when it answers with one. Inputs: the feature and whether it is on.
    /// Output: the state.
    fn security(&self, feature: &str, wanted: bool) -> Result<GitHubRuleState, EngineError> {
        let status = if wanted { "enabled" } else { "disabled" };
        let body = json!({ "security_and_analysis": { feature: { "status": status } } });
        let reply = self.call(Verb::Patch, "", Some(&body))?;
        if reply.status != 200 {
            return Ok(refused(&reply));
        }
        let repository: Value = api::json(&reply)?;
        let held = repository
            .pointer(&format!("/security_and_analysis/{feature}/status"))
            .and_then(Value::as_str);
        Ok(match held {
            Some(held) if held != status => GitHubRuleState::Refused {
                status: reply.status,
                message: format!(
                    "GitHub kept {feature} {}.",
                    api::shown(held, HELD_SHOWN_MAX)
                ),
            },
            _ => GitHubRuleState::Applied,
        })
    }

    /// Sets which actions may run: only GitHub-made and verified ones when on, all of
    /// them when off. Input: whether it is on. Output: the state of the first call that
    /// was refused, else applied.
    fn allowed_actions(&self, wanted: bool) -> Result<GitHubRuleState, EngineError> {
        let allowed = if wanted { "selected" } else { "all" };
        let policy = json!({ "enabled": true, "allowed_actions": allowed });
        let state = self.send(Verb::Put, "actions/permissions", Some(&policy), &[204])?;
        if !wanted || state != GitHubRuleState::Applied {
            return Ok(state);
        }
        let selected = json!({
            "github_owned_allowed": true,
            "verified_allowed": true,
            "patterns_allowed": [],
        });
        self.send(
            Verb::Put,
            "actions/permissions/selected-actions",
            Some(&selected),
            &[204],
        )
    }

    /// The repository's own rulesets, read once per pass; GitHub's refusal to list them
    /// stands for every ruleset rule. Output: the rulesets or that refusal, or
    /// `github_token_refused`, `github_reply_invalid`.
    fn listed(&mut self) -> Result<Result<&[Listed], GitHubRuleState>, EngineError> {
        if self.rulesets.is_none() {
            let mut url = self.endpoints.api(&self.path("rulesets"))?;
            url.query_pairs_mut()
                .append_pair("includes_parents", "false")
                .append_pair("per_page", &RULESETS_PER_PAGE.to_string());
            let reply = self
                .endpoints
                .request(url, api::API_JSON, Some(self.token), None)
                .send()?;
            let read = match reply.status {
                200 => {
                    let listed: Vec<Listed> = api::json(&reply)?;
                    if listed.len() > RULESETS_PER_PAGE {
                        return Err(failure("github_reply_invalid"));
                    }
                    Ok(listed
                        .into_iter()
                        .filter(|listed| {
                            listed
                                .source_type
                                .as_deref()
                                .is_none_or(|source| source == "Repository")
                        })
                        .collect())
                }
                401 => return Err(failure("github_token_refused")),
                _ => Err(refused(&reply)),
            };
            self.rulesets = Some(read);
        }
        Ok(
            match self
                .rulesets
                .as_ref()
                .expect("the rulesets were read above")
            {
                Ok(listed) => Ok(listed.as_slice()),
                Err(refusal) => Err(refusal.clone()),
            },
        )
    }

    /// Makes, updates or deletes the CrazyAgent ruleset that holds a rule, found by name
    /// among the repository's own rulesets, so applying twice never makes a second one.
    /// Input: the rule. Output: its state.
    fn ruleset(&mut self, rule: GitHubRule) -> Result<GitHubRuleState, EngineError> {
        let name = rule.ruleset().expect("a rule a ruleset holds");
        let on = self.on;
        let facts = self.facts;
        let wanted = on.contains(&rule);
        let checks: &[String] = match &facts.ci {
            GitHubCi::Checks(checks) => checks,
            GitHubCi::None | GitHubCi::Waiting => &[],
        };
        if rule == GitHubRule::RequiredChecks && wanted {
            if facts.ci == GitHubCi::None {
                return Ok(not_applicable("no CI workflows in this repository"));
            }
            if checks.is_empty() {
                return Ok(GitHubRuleState::Waiting {
                    reason: "waiting for the first CI run to name its checks".into(),
                });
            }
        }
        let found = match self.listed()? {
            Ok(listed) => listed
                .iter()
                .find(|listed| listed.name == name)
                .map(|listed| listed.id),
            Err(refusal) => return Ok(refusal),
        };
        let body = ruleset_body(rule, name, on, checks);
        match (wanted, found) {
            (true, None) => self.send(Verb::Post, "rulesets", Some(&body), &[201]),
            (true, Some(id)) => {
                self.send(Verb::Put, &format!("rulesets/{id}"), Some(&body), &[200])
            }
            (false, Some(id)) => self.send(Verb::Delete, &format!("rulesets/{id}"), None, &[204]),
            (false, None) => Ok(GitHubRuleState::Applied),
        }
    }
}

/// The repository fields a repository rule sets: DEC-182's value when on, the opposite
/// when off, except squash-only, which allows every merge method when off.
/// Inputs: the rule and whether it is on. Output: the fields.
fn fields(rule: GitHubRule, wanted: bool) -> Value {
    match rule {
        GitHubRule::SquashOnly => json!({
            "allow_squash_merge": true,
            "allow_merge_commit": !wanted,
            "allow_rebase_merge": !wanted,
        }),
        GitHubRule::DeleteHeadBranches => json!({ "delete_branch_on_merge": wanted }),
        GitHubRule::IssuesOn => json!({ "has_issues": wanted }),
        GitHubRule::WikiOff => json!({ "has_wiki": !wanted }),
        GitHubRule::ProjectsOff => json!({ "has_projects": !wanted }),
        _ => {
            assert_eq!(rule, GitHubRule::AutoMergeOff);
            json!({ "allow_auto_merge": !wanted })
        }
    }
}

/// The body of the CrazyAgent ruleset for one rule: active and with no bypass, on the
/// default branch, or on every tag for release tags. Inputs: the rule, its ruleset name,
/// the rules on and the CI's check names. Output: the JSON body.
fn ruleset_body(
    rule: GitHubRule,
    name: &str,
    on: &BTreeSet<GitHubRule>,
    checks: &[String],
) -> Value {
    assert_eq!(rule.ruleset(), Some(name));
    let branch = "~DEFAULT_BRANCH";
    let (target, include, rules) = match rule {
        GitHubRule::PullRequest => {
            let resolution = on.contains(&GitHubRule::ConversationResolution);
            let rules = json!([{
                "type": "pull_request",
                "parameters": {
                    "required_approving_review_count": 0,
                    "dismiss_stale_reviews_on_push": false,
                    "require_code_owner_review": false,
                    "require_last_push_approval": false,
                    "required_review_thread_resolution": resolution,
                },
            }]);
            ("branch", branch, rules)
        }
        GitHubRule::NoForcePush => ("branch", branch, json!([{ "type": "non_fast_forward" }])),
        GitHubRule::NoDeletion => ("branch", branch, json!([{ "type": "deletion" }])),
        GitHubRule::LinearHistory => (
            "branch",
            branch,
            json!([{ "type": "required_linear_history" }]),
        ),
        GitHubRule::RequiredChecks => {
            let contexts: Vec<Value> = checks
                .iter()
                .map(|check| json!({ "context": check }))
                .collect();
            let rules = json!([{
                "type": "required_status_checks",
                "parameters": {
                    "required_status_checks": contexts,
                    "strict_required_status_checks_policy": false,
                },
            }]);
            ("branch", branch, rules)
        }
        GitHubRule::SignedCommits => ("branch", branch, json!([{ "type": "required_signatures" }])),
        _ => {
            assert_eq!(rule, GitHubRule::TagProtection);
            let rules = json!([
                { "type": "deletion" },
                { "type": "non_fast_forward" },
                { "type": "update", "parameters": { "update_allows_fetch_and_merge": false } },
            ]);
            ("tag", "~ALL", rules)
        }
    };
    json!({
        "name": name,
        "target": target,
        "enforcement": "active",
        "bypass_actors": [],
        "conditions": { "ref_name": { "include": [include], "exclude": [] } },
        "rules": rules,
    })
}
