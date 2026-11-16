use super::intake::copy;
use super::new_project_publish::Publish;
use super::new_project_view::{Look, Row};
use super::project_markdown::sanitize;
use crate::github::{
    GitHubIgnoreDraft, GitHubPromptAnswer, GitHubPublishJob, GitHubPublishLeft, GitHubPublishPlan,
    GitHubPublishStep, GitHubRule, GitHubRuleState, GitHubSecretKind,
};
use crate::protocol::ProjectId;
use crate::tui::client::intake::{IntakeEvent, IntakeQuery};
use crate::tui::keys::Key;
use std::collections::BTreeSet;

pub(in crate::tui) const PUBLISH_TITLE: &str = "Publish on GitHub";
pub(in crate::tui) const DRAFT_POLL_MS: i64 = 2000;
pub(in crate::tui) const DRAFT_AGENT: &str =
    "The project's agent drafted these lines from file names only. Check them before you approve.";
pub(in crate::tui) const DRAFT_STOPPED: &str =
    "The draft stopped, so CrazyAgent's standard lines are shown.";
pub(in crate::tui) const DRAFT_BASELINE: &str =
    "No agent drafted these lines, so CrazyAgent's standard lines are shown.";
pub(in crate::tui) const PART_WAY: &str =
    "Publishing stopped part way. Try again to go on from there.";
const DRAFTING: &str = "Drafting the .gitignore lines...";
const BLOCKED: &str = "These files look like they hold secrets, so nothing was published.";
const MORE: &str = "There are more. Check again after these.";
const DISCARD_ASK: &str = "Discard this publish? CrazyAgent forgets it so you can start again. Files it wrote in the project stay, and a repository it made stays on GitHub.";
const REFUSAL_CHARS_MAX: usize = 300;
pub(in crate::tui) const PROMPT_LINE: &str = "This project is not on GitHub. Publish it?";
pub(in crate::tui) const PROMPT_KEY: (&str, &str) = ("G", "Publish on GitHub");
const CHECKING: &str = "Checking your GitHub sign-in...";
const SIGNED_OUT: &str = "Sign in to GitHub to publish this project.";
const STATUS_FAILED: &str = "CrazyAgent couldn't read your GitHub sign-in.";

/// What one key in the Overview's publish question or its form asks of the page: nothing
/// more, close the pane, keep Not now or Don't ask again, send a GitHub read, open the
/// GitHub sign-in, or start the publish with the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum AskKey {
    Handled,
    Close,
    Answer(GitHubPromptAnswer),
    Read(IntakeQuery),
    SignIn,
    Start(GitHubPublishPlan),
}

/// The question's or the form's words: the title, the body rows and the key hints, as key
/// and label.
pub(in crate::tui) struct AskText {
    pub title: &'static str,
    pub body: Vec<Row>,
    pub hints: Vec<(&'static str, &'static str)>,
}

/// The Overview's question for a project with no GitHub publish (DEC-182 S5b): the question
/// with Publish, Not now and Don't ask again, then the New project publish panel that
/// Publish opens, with the GitHub read it waits on.
#[derive(Debug, Clone, Default)]
pub(in crate::tui) struct AskPane {
    form: Option<Publish>,
    reading: Option<IntakeQuery>,
}

impl AskPane {
    /// Says whether a reply answers the GitHub read the form waits on. Takes the query.
    /// Returns the answer.
    pub fn takes(&self, _query: &IntakeQuery) -> bool {
        false
    }

    /// Handles a key: on the question Enter opens the form and reads the GitHub sign-in,
    /// n keeps Not now, x keeps Don't ask again and Esc closes; in the form Esc closes,
    /// Enter on Publish starts with the plan or moves to the row that stops it, Enter opens
    /// the sign-in while signed out or reads it again after a failed read, and every other
    /// key goes to the panel. Takes the key, the project's name and its about text.
    /// Returns what the page does next.
    pub fn key(&mut self, _key: &Key, _name: &str, _about: &str) -> AskKey {
        AskKey::Handled
    }

    /// Takes the reply to the form's GitHub read: the sign-in, then the accounts while
    /// signed in, which opens the panel on its first row. Takes the reply, one [`takes`]
    /// accepts. Returns the read to send next, if any.
    ///
    /// [`takes`]: AskPane::takes
    pub fn arrived(&mut self, _event: &IntakeEvent) -> Option<IntakeQuery> {
        None
    }

    /// The pane's words: the question, the sign-in being read, signed out, a read that
    /// failed, or the publish panel with Publish as its last row. Takes the project's name,
    /// its about text and the look. Returns the words.
    pub fn text(&self, _name: &str, _about: &str, _look: &Look) -> AskText {
        AskText {
            title: PUBLISH_TITLE,
            body: Vec::new(),
            hints: Vec::new(),
        }
    }
}

/// What the owner asked for in the publish pane, before it becomes a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum PublishAsk {
    Approve(Vec<String>),
    Exclude(String),
    Recheck,
    Continue,
    Discard,
}

/// What one key in the publish pane did: nothing more to send, close it for now, or an ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum PaneKey {
    Handled,
    Later,
    Ask(PublishAsk),
}

/// The publish pane's words: its title, its body lines and its key hints, as key and label.
pub(in crate::tui) struct PaneText {
    pub title: &'static str,
    pub body: Vec<String>,
    pub hints: Vec<(&'static str, &'static str)>,
}

/// The project page's GitHub publish pane over one job (DEC-182): the lines with the ones
/// the owner leaves out, the file picked among those that look like secrets, and whether
/// Discard waits for its yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct PublishPane {
    job: GitHubPublishJob,
    selected: usize,
    dropped: BTreeSet<usize>,
    discarding: bool,
}

impl PublishPane {
    /// Opens the pane on a job, with every line kept and the first row picked.
    /// Input: the job. Output: the pane.
    pub(in crate::tui) fn new(job: GitHubPublishJob) -> Self {
        Self {
            job,
            selected: 0,
            dropped: BTreeSet::new(),
            discarding: false,
        }
    }

    /// The job the pane shows. Input: none. Output: the job.
    pub(in crate::tui) fn job(&self) -> &GitHubPublishJob {
        &self.job
    }

    /// Takes one key: Esc closes the pane for now, d then y discards before the push, and
    /// each step has its own keys: Up and Down pick, Space leaves a line out or keeps it and
    /// Enter approves the kept lines; x leaves the picked file out and r checks again; Enter
    /// tries a stopped publish again and closes a finished one. Input: the key. Output: what
    /// it did.
    pub(in crate::tui) fn key(&mut self, key: &Key) -> PaneKey {
        if self.discarding {
            self.discarding = false;
            return if *key == Key::Char('y') {
                PaneKey::Ask(PublishAsk::Discard)
            } else {
                PaneKey::Handled
            };
        }
        if *key == Key::Esc {
            return PaneKey::Later;
        }
        if *key == Key::Char('d') && discardable(&self.job) {
            self.discarding = true;
            return PaneKey::Handled;
        }
        let rows = self.rows();
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(rows.saturating_sub(1)),
            _ => {}
        }
        match (&self.job.step, key) {
            (GitHubPublishStep::Lines { .. }, Key::Char(' ')) if rows > 0 => {
                if !self.dropped.remove(&self.selected) {
                    self.dropped.insert(self.selected);
                }
                PaneKey::Handled
            }
            (GitHubPublishStep::Lines { lines, .. }, Key::Enter) => {
                PaneKey::Ask(PublishAsk::Approve(
                    lines
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !self.dropped.contains(index))
                        .map(|(_, line)| line.clone())
                        .collect(),
                ))
            }
            (GitHubPublishStep::Blocked { .. }, Key::Char('x')) => self
                .paths()
                .get(self.selected)
                .map_or(PaneKey::Handled, |path| {
                    PaneKey::Ask(PublishAsk::Exclude(path.clone()))
                }),
            (GitHubPublishStep::Blocked { .. }, Key::Char('r')) => {
                PaneKey::Ask(PublishAsk::Recheck)
            }
            (GitHubPublishStep::Done { .. }, Key::Enter) => PaneKey::Later,
            (
                GitHubPublishStep::Drafting
                | GitHubPublishStep::Lines { .. }
                | GitHubPublishStep::Blocked { .. }
                | GitHubPublishStep::Done { .. },
                _,
            ) => PaneKey::Handled,
            (_, Key::Enter) => PaneKey::Ask(PublishAsk::Continue),
            _ => PaneKey::Handled,
        }
    }

    /// The pane's words for where the job stands, with every engine text made safe for the
    /// terminal; a finding names its file, line and kind, never the value. Input: none.
    /// Output: the title, body and hints.
    pub(in crate::tui) fn text(&self) -> PaneText {
        if self.discarding {
            return PaneText {
                title: PUBLISH_TITLE,
                body: vec![DISCARD_ASK.to_owned()],
                hints: vec![("y", "Discard"), ("n", "Keep it")],
            };
        }
        let mark = |index: usize| if index == self.selected { ">" } else { " " };
        let (body, mut hints) = match &self.job.step {
            GitHubPublishStep::Drafting => (vec![DRAFTING.to_owned()], Vec::new()),
            GitHubPublishStep::Lines { lines, draft } => {
                let mut body = vec![draft_note(draft).to_owned(), String::new()];
                body.extend(lines.iter().enumerate().map(|(index, line)| {
                    let kept = if self.dropped.contains(&index) {
                        " "
                    } else {
                        "x"
                    };
                    format!("{} [{kept}] {}", mark(index), sanitize(line))
                }));
                (
                    body,
                    vec![
                        ("Up/Down", "Choose"),
                        ("Space", "Keep or leave out"),
                        ("Enter", "Approve and publish"),
                    ],
                )
            }
            GitHubPublishStep::Blocked { scan } => {
                let mut body = vec![BLOCKED.to_owned()];
                body.extend(scan.findings.iter().map(|finding| {
                    format!(
                        "- {}, line {}: {}",
                        sanitize(&finding.path),
                        finding.line,
                        finding_kind(finding.kind)
                    )
                }));
                if scan.truncated {
                    body.push(MORE.to_owned());
                }
                body.push(String::new());
                body.extend(
                    self.paths()
                        .iter()
                        .enumerate()
                        .map(|(index, path)| format!("{} {}", mark(index), sanitize(path))),
                );
                (
                    body,
                    vec![
                        ("Up/Down", "Choose"),
                        ("x", "Leave this file out"),
                        ("r", "Check again"),
                    ],
                )
            }
            GitHubPublishStep::Done {
                full_name,
                outcomes,
            } => {
                let wanted: Vec<_> = outcomes.iter().filter(|outcome| outcome.on).collect();
                let set = wanted
                    .iter()
                    .filter(|outcome| outcome.state == GitHubRuleState::Applied)
                    .count();
                let mut body = vec![format!(
                    "Published as {}. {set} of {} rules set.",
                    sanitize(full_name),
                    wanted.len()
                )];
                body.extend(wanted.iter().filter_map(|outcome| {
                    rule_state_line(&outcome.state)
                        .map(|line| format!("- {}: {line}", rule_label(outcome.rule)))
                }));
                (body, vec![("Enter", "Close")])
            }
            _ => (
                vec![
                    self.job
                        .stopped
                        .as_deref()
                        .map_or_else(|| PART_WAY.to_owned(), copy),
                ],
                vec![("Enter", "Try again")],
            ),
        };
        if discardable(&self.job) {
            hints.push(("d", "Discard"));
        }
        if !matches!(self.job.step, GitHubPublishStep::Done { .. }) {
            hints.push(("Esc", "Later"));
        }
        PaneText {
            title: PUBLISH_TITLE,
            body,
            hints,
        }
    }

    fn rows(&self) -> usize {
        match &self.job.step {
            GitHubPublishStep::Lines { lines, .. } => lines.len(),
            GitHubPublishStep::Blocked { .. } => self.paths().len(),
            _ => 0,
        }
    }

    fn paths(&self) -> Vec<String> {
        let GitHubPublishStep::Blocked { scan } = &self.job.step else {
            return Vec::new();
        };
        let mut paths: Vec<String> = Vec::new();
        for finding in &scan.findings {
            if !paths.contains(&finding.path) {
                paths.push(finding.path.clone());
            }
        }
        paths
    }
}

fn draft_note(draft: &GitHubIgnoreDraft) -> &'static str {
    match draft {
        GitHubIgnoreDraft::Agent => DRAFT_AGENT,
        GitHubIgnoreDraft::Baseline { reason } if reason == "github_publish_draft_stopped" => {
            DRAFT_STOPPED
        }
        GitHubIgnoreDraft::Baseline { .. } => DRAFT_BASELINE,
    }
}

fn finding_kind(kind: GitHubSecretKind) -> &'static str {
    match kind {
        GitHubSecretKind::Token => "looks like an access token",
        GitHubSecretKind::PrivateKey => "looks like a private key",
        GitHubSecretKind::NamedValue => "looks like a secret value",
    }
}

/// Tells whether Discard is offered: any publish before its push except one still drafting,
/// whose draft holds the publish. Input: the job. Output: true when Discard is offered.
fn discardable(job: &GitHubPublishJob) -> bool {
    !matches!(
        job.step,
        GitHubPublishStep::Drafting
            | GitHubPublishStep::Pushed { .. }
            | GitHubPublishStep::Done { .. }
    )
}

/// Tells whether a status read opens the pane: every publish the owner can act on or
/// follow, not a finished one. Input: the job. Output: true when it opens.
pub(in crate::tui) fn opens(job: &GitHubPublishJob) -> bool {
    !matches!(job.step, GitHubPublishStep::Done { .. })
}

/// Turns an ask into its publish query for the project. Inputs: the project and the ask.
/// Output: the query.
pub(in crate::tui) fn query(project: &ProjectId, ask: PublishAsk) -> IntakeQuery {
    let project = project.clone();
    match ask {
        PublishAsk::Approve(lines) => IntakeQuery::GitHubPublishApprove { project, lines },
        PublishAsk::Exclude(path) => IntakeQuery::GitHubPublishExclude { project, path },
        PublishAsk::Recheck => IntakeQuery::GitHubPublishRecheck { project },
        PublishAsk::Continue => IntakeQuery::GitHubPublishContinue { project },
        PublishAsk::Discard => IntakeQuery::GitHubPublishDiscard { project },
    }
}

/// Says what a discarded publish left on GitHub, in the desktop's words. Input: the
/// discard's answer. Output: the sentence, or "" when it left nothing.
pub(in crate::tui) fn left_line(left: Option<&GitHubPublishLeft>) -> String {
    match left {
        None => String::new(),
        Some(GitHubPublishLeft::Made { full_name }) => format!(
            "The empty repository {} stays on GitHub. Delete it there if you don't want it.",
            sanitize(full_name)
        ),
        Some(GitHubPublishLeft::Maybe { full_name }) => format!(
            "GitHub may have made {}. Check GitHub and delete it if you don't want it.",
            sanitize(full_name)
        ),
    }
}

/// Words one rule outcome's state the way the desktop does: nothing once applied, GitHub's
/// refusal with its status and at most 300 characters of its message made safe, or the
/// engine's reason as a sentence. Input: the state. Output: the line, or none when applied.
pub(in crate::tui) fn rule_state_line(state: &GitHubRuleState) -> Option<String> {
    match state {
        GitHubRuleState::Applied => None,
        GitHubRuleState::Refused { status, message } => {
            let message: String = sanitize(message).chars().take(REFUSAL_CHARS_MAX).collect();
            Some(if *status == 0 {
                format!("Not sent: {message}")
            } else {
                format!("GitHub refused this (HTTP {status}): {message}")
            })
        }
        GitHubRuleState::NotApplicable { reason } | GitHubRuleState::Waiting { reason } => {
            let reason = sanitize(reason);
            let mut characters = reason.chars();
            let first = characters.next()?;
            let sentence = format!("{}{}", first.to_uppercase(), characters.as_str());
            Some(if sentence.ends_with('.') {
                sentence
            } else {
                format!("{sentence}.")
            })
        }
    }
}

/// The words a person reads for one rule of the standard secure setup.
/// Input: the rule. Output: a short plain label, the same text the desktop shows.
pub(in crate::tui) fn rule_label(rule: GitHubRule) -> &'static str {
    match rule {
        GitHubRule::SquashOnly => "Squash merge only",
        GitHubRule::DeleteHeadBranches => "Delete branches after merge",
        GitHubRule::IssuesOn => "Issues on",
        GitHubRule::WikiOff => "Wiki off",
        GitHubRule::ProjectsOff => "Projects off",
        GitHubRule::AutoMergeOff => "Auto-merge off",
        GitHubRule::PullRequest => "Changes go through pull requests",
        GitHubRule::ConversationResolution => "Resolve conversations before merging",
        GitHubRule::NoForcePush => "No force pushes to main",
        GitHubRule::NoDeletion => "Main can't be deleted",
        GitHubRule::LinearHistory => "Linear history",
        GitHubRule::RequiredChecks => "Required checks pass before merging",
        GitHubRule::SignedCommits => "Signed commits",
        GitHubRule::TagProtection => "Release tags protected",
        GitHubRule::DependabotAlerts => "Dependabot alerts",
        GitHubRule::DependabotSecurityUpdates => "Dependabot security updates",
        GitHubRule::SecretScanning => "Secret scanning",
        GitHubRule::PushProtection => "Push protection for secrets",
        GitHubRule::PrivateVulnerabilityReporting => "Private vulnerability reporting",
        GitHubRule::ActionsVerifiedOnly => "Actions from GitHub and verified creators only",
        GitHubRule::ActionsReadOnlyToken => "Read-only token for Actions",
        GitHubRule::ActionsOutsidePrApproval => "Approval for outside contributors' workflows",
        GitHubRule::CodeqlDefaultSetup => "CodeQL code scanning",
        GitHubRule::DependabotVersionUpdates => "Dependabot version updates",
        GitHubRule::SecurityPolicy => "Security policy (SECURITY.md)",
    }
}
