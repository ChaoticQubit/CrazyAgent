use super::{
    GitHub, GitHubCi, GitHubCommitted, GitHubCreated, GitHubHistory, GitHubNewRepo, GitHubPrepare,
    GitHubRepoFacts, GitHubRule, GitHubRuleOutcome, GitHubSecretScan, api, draft, publish, state,
};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const WORKFLOW: [&str; 1] = ["workflow"];
const CREATE_UNSENT: [&str; 9] = [
    "github_repo_create_refused",
    "github_repo_name_invalid",
    "github_repo_description_invalid",
    "github_token_refused",
    "github_not_signed_in",
    "credential_missing",
    "credential_store_unavailable",
    "github_state_invalid",
    "github_state_unavailable",
];
const EXCLUDE_SPECIAL: &[u8] = b"*?[\\";

static PUBLISHING: Mutex<BTreeSet<(PathBuf, PathBuf)>> = Mutex::new(BTreeSet::new());

/// What the owner chose for a publish: the account and name of the new repository, its
/// description, whether it is private, and whether the docs folder goes up with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPublishPlan {
    pub owner: String,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub docs: bool,
}

/// Who drafted the .gitignore lines a publish proposes: the project's agent, or nobody,
/// so the lines are the fixed baseline, with the code that says why no agent drafted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubIgnoreDraft {
    Agent,
    Baseline { reason: String },
}

/// The draft a record from before the agent draft holds: none was asked for. Input: none.
/// Output: the baseline draft with `github_publish_draft_not_asked`.
fn not_asked() -> GitHubIgnoreDraft {
    GitHubIgnoreDraft::Baseline {
        reason: draft::NOT_ASKED.to_owned(),
    }
}

/// What a discarded publish left on GitHub: the repository it made, empty since nothing
/// was pushed, or the one its create may have made, when GitHub's answer to that create
/// was lost or refused after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "left", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubPublishLeft {
    Made { full_name: String },
    Maybe { full_name: String },
}

/// Where a publish job stands: its .gitignore lines still being drafted, waiting for the
/// owner to approve them or to deal with a secret, or the last step it finished on the way
/// to the rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubPublishStep {
    Drafting,
    Lines {
        lines: Vec<String>,
        #[serde(default = "not_asked")]
        draft: GitHubIgnoreDraft,
    },
    Blocked {
        scan: GitHubSecretScan,
    },
    Checked,
    Committed {
        committed: GitHubCommitted,
    },
    Creating {
        committed: GitHubCommitted,
        sent_at: u64,
    },
    Created {
        committed: GitHubCommitted,
        full_name: String,
        private: bool,
    },
    Pushed {
        full_name: String,
        private: bool,
        workflows: bool,
    },
    Done {
        full_name: String,
        outcomes: Vec<GitHubRuleOutcome>,
    },
}

/// One project's publish as the engine records it: the plan, the lines the owner
/// approved, the paths publish wrote, the step reached, the code of the failure that
/// stopped it there, if one did, the `owner/name` of the repository it made, kept
/// once made so a check and commit again never create a second one, and the rules it
/// sets, copied from the global defaults at Start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPublishJob {
    pub project: PathBuf,
    pub plan: GitHubPublishPlan,
    pub lines: Vec<String>,
    pub written: Vec<String>,
    pub step: GitHubPublishStep,
    pub stopped: Option<String>,
    #[serde(default)]
    pub made: Option<String>,
    #[serde(default = "GitHubRule::defaults")]
    pub rules: BTreeSet<GitHubRule>,
}

/// Holds one project's publish for the engine, so a second run of it is refused while
/// this one goes on; dropping it lets the next run take it.
pub(super) struct Publishing {
    key: (PathBuf, PathBuf),
}

impl Drop for Publishing {
    fn drop(&mut self) {
        let removed = PUBLISHING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
        assert!(removed, "a publish hold is released once");
    }
}

/// A publish Start past its checks, its record saved at Drafting and the project's publish
/// still held, so the agent draft runs after Start has answered and no other run comes
/// between (DEC-182). `run` drafts and records the lines; one dropped unrun lets the
/// publish go with the record at Drafting, which the next Status closes with the baseline.
pub struct GitHubDraft {
    root: PathBuf,
    job: GitHubPublishJob,
    prompt: String,
    existing: Option<String>,
    held: Publishing,
}

impl GitHubDraft {
    /// The job as Start recorded it, at Drafting. Input: none. Output: the job.
    pub fn job(&self) -> &GitHubPublishJob {
        &self.job
    }

    /// Asks the project's agent through `ask` for .gitignore lines from the project's names
    /// only, records the lines the owner is shown first, the baseline then the agent's, or
    /// the baseline alone with the code `ask` gave, and then lets the publish go.
    /// Input: `ask`, which takes the prompt and gives the reply or a code. Output: the job
    /// at its lines, or the record's save failure.
    pub fn run(
        self,
        ask: impl FnOnce(&str) -> Result<String, &'static str>,
    ) -> Result<GitHubPublishJob, EngineError> {
        let Self {
            root,
            job,
            prompt,
            existing,
            held,
        } = self;
        let (agent, by) = match ask(&prompt) {
            Ok(reply) => (draft::reply_lines(&reply), GitHubIgnoreDraft::Agent),
            Err(code) => (
                Vec::new(),
                GitHubIgnoreDraft::Baseline {
                    reason: code.to_owned(),
                },
            ),
        };
        let job = GitHubPublishJob {
            step: GitHubPublishStep::Lines {
                lines: draft::merged(&agent, existing.as_deref()),
                draft: by,
            },
            ..job
        };
        state::save_job(&root, &job)?;
        drop(held);
        Ok(job)
    }

    /// Runs the agent draft through `ask` on its own thread and answers at once with the
    /// job at Drafting, so a Start never waits on the agent (RV gh-s4b F1). A thread that
    /// cannot start drops the draft, which lets the publish go. Input: `ask`, as `run` takes
    /// it. Output: the job the Start answers with.
    pub fn spawn(
        self,
        ask: impl FnOnce(&str) -> Result<String, &'static str> + Send + 'static,
    ) -> GitHubPublishJob {
        let job = self.job().clone();
        let _ = std::thread::Builder::new()
            .name("github-draft".into())
            .spawn(move || {
                let _ = self.run(ask);
            });
        job
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

/// Takes one project's publish for this engine root. Inputs: the engine root and the
/// project folder. Output: the hold, or `github_publish_running`.
pub(super) fn publishing(root: &Path, project: &Path) -> Result<Publishing, EngineError> {
    let key = (root.to_path_buf(), project.to_path_buf());
    let mut held = PUBLISHING.lock().unwrap_or_else(PoisonError::into_inner);
    if !held.insert(key.clone()) {
        return Err(failure("github_publish_running"));
    }
    Ok(Publishing { key })
}

/// The repository a plan asks GitHub to make. Input: the plan. Output: the new repository.
/// Tells whether a job at Creating surely made no repository: its create was refused, or
/// never sent for want of a valid name, a sign-in or a token. Any other stop, or none, as
/// when the engine stopped, GitHub's answer was lost or refused after a 201, or the name
/// is taken by a repository this job cannot tell from its own, may have left one. Input:
/// the job. Output: true when nothing can have been made.
fn create_unsent(job: &GitHubPublishJob) -> bool {
    assert!(matches!(job.step, GitHubPublishStep::Creating { .. }));
    job.made.is_none()
        && job
            .stopped
            .as_deref()
            .is_some_and(|code| CREATE_UNSENT.contains(&code))
}

fn new_repo(plan: &GitHubPublishPlan) -> GitHubNewRepo {
    GitHubNewRepo {
        owner: plan.owner.clone(),
        name: plan.name.clone(),
        description: plan.description.clone(),
        private: plan.private,
    }
}

/// The time now, in seconds since the epoch. Output: the seconds, or
/// `github_publish_failed` for a clock before 1970.
fn now() -> Result<u64, EngineError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| failure("github_publish_failed"))
}

impl GitHub {
    /// Starts a publish: checks the plan, the sign-in and the project's git, reads the
    /// project's .gitignore and names for the draft prompt, and records the job at
    /// Drafting, still holding the project's publish, so Start answers before the agent
    /// drafts. Nothing is written into the project. A job that made its repository
    /// already, or whose create may have made it, is gone on with Continue, which knows
    /// that repository again, or forgotten with Discard instead, and a start over an
    /// earlier job keeps the paths it wrote, which the commit then takes.
    /// Inputs: the project folder and the plan. Output: the draft to run, or
    /// `github_repo_name_invalid`, `github_repo_description_invalid`,
    /// `github_publish_running`, `github_not_signed_in`, `github_publish_in_progress` or the
    /// project's git refusal.
    pub fn publish_begin(
        &self,
        project: &Path,
        plan: &GitHubPublishPlan,
    ) -> Result<GitHubDraft, EngineError> {
        api::check_new(&new_repo(plan))?;
        let held = publishing(&self.root, project)?;
        self.saved()?;
        let rules = state::load_defaults(&self.root)?;
        let written = match state::load_job(&self.root, project)? {
            Some(job)
                if match &job.step {
                    GitHubPublishStep::Done { .. } => false,
                    GitHubPublishStep::Created { .. } | GitHubPublishStep::Pushed { .. } => true,
                    GitHubPublishStep::Creating { .. } => !create_unsent(&job),
                    _ => job.made.is_some(),
                } =>
            {
                return Err(failure("github_publish_in_progress"));
            }
            Some(job) => job.written,
            None => Vec::new(),
        };
        self.publish_inspect(project)?;
        let existing = publish::read_gitignore(&project.join(publish::GITIGNORE))?;
        let (listed, cut) = draft::listing(project)?;
        let job = GitHubPublishJob {
            project: project.to_path_buf(),
            plan: plan.clone(),
            lines: Vec::new(),
            written,
            step: GitHubPublishStep::Drafting,
            stopped: None,
            made: None,
            rules,
        };
        state::save_job(&self.root, &job)?;
        Ok(GitHubDraft {
            root: self.root.clone(),
            prompt: draft::prompt(&listed, cut, existing.as_deref()),
            existing,
            job,
            held,
        })
    }

    /// Starts a publish and runs its draft at once, as the engine's own rows do. Inputs:
    /// the project folder, the plan and `ask`. Output: the job at its lines, or the
    /// refusal `publish_begin` or the draft gave.
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn publish_start(
        &self,
        project: &Path,
        plan: &GitHubPublishPlan,
        ask: impl FnOnce(&str) -> Result<String, &'static str>,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.publish_begin(project, plan)?.run(ask)
    }

    /// Takes the .gitignore lines the owner approved, gets the project ready, checks it
    /// for secrets and runs the publish as far as it goes. Inputs: the project folder, the
    /// owner's home folder and the lines. Output: the job where it stands, with the code
    /// that stopped it, or `github_publish_not_started`, `github_publish_step_invalid`,
    /// `github_publish_ignore_invalid`, `github_publish_running`.
    pub fn publish_approve(
        &self,
        project: &Path,
        owner_home: &Path,
        lines: &[String],
    ) -> Result<GitHubPublishJob, EngineError> {
        let _held = publishing(&self.root, project)?;
        let mut job = self.job(project)?;
        if !matches!(job.step, GitHubPublishStep::Lines { .. }) {
            return Err(failure("github_publish_step_invalid"));
        }
        if !publish::lines_valid(lines) {
            return Err(failure("github_publish_ignore_invalid"));
        }
        job.lines = lines.to_vec();
        self.run(job, |job| {
            self.prepared(job)?;
            self.advance(job, owner_home)
        })
    }

    /// Leaves a file that holds a secret out of the publish through a `/<path>` line in
    /// the .gitignore, then checks again and runs on. Only a file of a project with no
    /// history of the owner's can be left out, since a commit keeps it. Inputs: the
    /// project folder, the owner's home folder and the file's path as the check named it.
    /// Output: the job where it stands, or `github_publish_step_invalid`,
    /// `github_publish_exclude_invalid`, `github_publish_secret_in_history`.
    pub fn publish_exclude(
        &self,
        project: &Path,
        owner_home: &Path,
        path: &str,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _held = publishing(&self.root, project)?;
        let mut job = self.job(project)?;
        let GitHubPublishStep::Blocked { scan } = &job.step else {
            return Err(failure("github_publish_step_invalid"));
        };
        let plain = !path.is_empty()
            && path.trim_end() == path
            && !path.chars().any(char::is_control)
            && !path.bytes().any(|byte| EXCLUDE_SPECIAL.contains(&byte));
        let line = format!("/{path}");
        let mut lines = job.lines.clone();
        if !lines.contains(&line) {
            lines.push(line);
        }
        if !plain
            || !scan.findings.iter().any(|found| found.path == path)
            || !publish::lines_valid(&lines)
        {
            return Err(failure("github_publish_exclude_invalid"));
        }
        if self.publish_inspect(project)?.history == GitHubHistory::Owner {
            return Err(failure("github_publish_secret_in_history"));
        }
        job.lines = lines;
        self.run(job, |job| {
            self.prepared(job)?;
            self.advance(job, owner_home)
        })
    }

    /// Checks a blocked publish for secrets again, after the owner fixed the files, and
    /// runs on. Inputs: the project folder and the owner's home folder. Output: the job
    /// where it stands, or `github_publish_step_invalid`.
    pub fn publish_recheck(
        &self,
        project: &Path,
        owner_home: &Path,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _held = publishing(&self.root, project)?;
        let job = self.job(project)?;
        if !matches!(job.step, GitHubPublishStep::Blocked { .. }) {
            return Err(failure("github_publish_step_invalid"));
        }
        self.run(job, |job| {
            self.checked(job)?;
            self.advance(job, owner_home)
        })
    }

    /// Runs a publish on from the last step it finished, such as after a stop or an engine
    /// that stopped part way. A job waiting for the owner's lines or a secret needs that
    /// answer first, and one stopped before its commit is checked for secrets again, since
    /// the files may have changed since. Inputs: the project folder and the owner's home
    /// folder. Output: the job where it stands, or `github_publish_not_started`,
    /// `github_publish_step_invalid`.
    pub fn publish_continue(
        &self,
        project: &Path,
        owner_home: &Path,
    ) -> Result<GitHubPublishJob, EngineError> {
        let _held = publishing(&self.root, project)?;
        let job = self.job(project)?;
        if matches!(
            job.step,
            GitHubPublishStep::Drafting
                | GitHubPublishStep::Lines { .. }
                | GitHubPublishStep::Blocked { .. }
        ) {
            return Err(failure("github_publish_step_invalid"));
        }
        self.run(job, |job| {
            if matches!(job.step, GitHubPublishStep::Checked) {
                self.checked(job)?;
            }
            self.advance(job, owner_home)
        })
    }

    /// Forgets a project's publish before its push, so Start runs fresh: the way out of a
    /// job whose repository is gone on GitHub or whose name or visibility the owner wants
    /// changed. A job at Pushed holds the code on GitHub with its rules still to apply,
    /// which Continue does, so it is kept. Nothing on GitHub or in the project is touched.
    /// Input: the project folder. Output: the empty repository the job made, or the one
    /// its create may have made, if any, or `github_publish_not_started`,
    /// `github_publish_step_invalid` for a job at Pushed or Done.
    pub fn publish_discard(
        &self,
        project: &Path,
    ) -> Result<Option<GitHubPublishLeft>, EngineError> {
        let _held = publishing(&self.root, project)?;
        let job = self.job(project)?;
        let left = match &job.step {
            GitHubPublishStep::Pushed { .. } | GitHubPublishStep::Done { .. } => {
                return Err(failure("github_publish_step_invalid"));
            }
            GitHubPublishStep::Created { full_name, .. } => Some(GitHubPublishLeft::Made {
                full_name: full_name.clone(),
            }),
            _ if job.made.is_some() => job
                .made
                .clone()
                .map(|full_name| GitHubPublishLeft::Made { full_name }),
            GitHubPublishStep::Creating { .. } if !create_unsent(&job) => {
                Some(GitHubPublishLeft::Maybe {
                    full_name: format!("{}/{}", job.plan.owner, job.plan.name),
                })
            }
            _ => None,
        };
        state::remove_job(&self.root, project)?;
        Ok(left)
    }

    /// Reads a project's publish record, which a run in progress keeps up to date step by
    /// step. A record at Drafting with no draft holding the publish, as after the engine
    /// stopped mid draft, is closed with the baseline lines and
    /// `github_publish_draft_stopped`, so the owner can go on. Input: the project folder.
    /// Output: the job, or None when the project has none.
    pub fn publish_status(&self, project: &Path) -> Result<Option<GitHubPublishJob>, EngineError> {
        let job = state::load_job(&self.root, project)?;
        if !job
            .as_ref()
            .is_some_and(|job| job.step == GitHubPublishStep::Drafting)
        {
            return Ok(job);
        }
        let Ok(_held) = publishing(&self.root, project) else {
            return Ok(job);
        };
        let Some(job) = state::load_job(&self.root, project)? else {
            return Ok(None);
        };
        if job.step != GitHubPublishStep::Drafting {
            return Ok(Some(job));
        }
        let existing = publish::read_gitignore(&project.join(publish::GITIGNORE))?;
        let job = GitHubPublishJob {
            step: GitHubPublishStep::Lines {
                lines: draft::merged(&[], existing.as_deref()),
                draft: GitHubIgnoreDraft::Baseline {
                    reason: draft::STOPPED.to_owned(),
                },
            },
            ..job
        };
        state::save_job(&self.root, &job)?;
        Ok(Some(job))
    }

    /// Reads the job a step works on. Input: the project folder. Output: the job, or
    /// `github_publish_not_started`.
    fn job(&self, project: &Path) -> Result<GitHubPublishJob, EngineError> {
        state::load_job(&self.root, project)?.ok_or_else(|| failure("github_publish_not_started"))
    }

    /// Runs one owner action on a job and records where it ended: the step reached and
    /// the code of the failure that stopped it, if one did. Inputs: the job and the work.
    /// Output: the recorded job, or the record's save failure.
    fn run(
        &self,
        mut job: GitHubPublishJob,
        work: impl FnOnce(&mut GitHubPublishJob) -> Result<(), EngineError>,
    ) -> Result<GitHubPublishJob, EngineError> {
        job.stopped = work(&mut job).err().map(|error| error.0);
        state::save_job(&self.root, &job)?;
        Ok(job)
    }

    /// Gets the project ready with the job's lines and choices, keeps every path publish
    /// wrote, then checks it for secrets. Input: the job. Output: `()`, or the refusal.
    fn prepared(&self, job: &mut GitHubPublishJob) -> Result<(), EngineError> {
        let local = self.publish_inspect(&job.project)?;
        let choices = GitHubPrepare {
            private: job.plan.private,
            docs: job.plan.docs,
            approved: job.lines.clone(),
            rules: job.rules.clone(),
        };
        for path in self.publish_prepare(&job.project, &local, &choices)? {
            if !job.written.contains(&path) {
                job.written.push(path);
            }
        }
        self.checked(job)
    }

    /// Checks what the publish would push for secrets, with owner history the files publish
    /// wrote included, which its commit takes by name, and records the job as checked, or
    /// blocked on what was found. Input: the job. Output: `()`, or the refusal.
    fn checked(&self, job: &mut GitHubPublishJob) -> Result<(), EngineError> {
        let history = self.publish_inspect(&job.project)?.history;
        let mut scan = self.publish_secrets(&job.project, history)?;
        if history == GitHubHistory::Owner {
            publish::written_findings(&job.project, &job.written, &mut scan)?;
        }
        job.step = if scan.findings.is_empty() {
            assert!(!scan.truncated);
            GitHubPublishStep::Checked
        } else {
            GitHubPublishStep::Blocked { scan }
        };
        state::save_job(&self.root, job)
    }

    /// Runs a checked job on step by step, recording each step before the next starts:
    /// the commit, "create sent" before the create request, the repository, the push and
    /// the rules. A step that fails leaves the job at the last one finished. A push refused
    /// because HEAD moved since the commit checks the project again and commits on the new
    /// HEAD once per call, reusing the repository made. A commit that finds a secret or a
    /// moved HEAD checks the project again once per call (rv-sec2 s2c N3). Inputs: the job
    /// and the owner's home folder. Output: `()`, or the failure.
    fn advance(&self, job: &mut GitHubPublishJob, owner_home: &Path) -> Result<(), EngineError> {
        let mut rechecked = false;
        let mut recommitted = false;
        loop {
            let next = match &job.step {
                GitHubPublishStep::Drafting
                | GitHubPublishStep::Lines { .. }
                | GitHubPublishStep::Blocked { .. }
                | GitHubPublishStep::Done { .. } => return Ok(()),
                GitHubPublishStep::Checked => {
                    match self.publish_commit(&job.project, owner_home, &job.written) {
                        Err(error)
                            if !recommitted
                                && matches!(
                                    error.0.as_str(),
                                    "github_publish_secret_found" | "github_publish_head_moved"
                                ) =>
                        {
                            recommitted = true;
                            GitHubPublishStep::Checked
                        }
                        committed => GitHubPublishStep::Committed {
                            committed: committed?,
                        },
                    }
                }
                GitHubPublishStep::Committed { committed } => GitHubPublishStep::Creating {
                    committed: committed.clone(),
                    sent_at: now()?,
                },
                GitHubPublishStep::Creating { committed, sent_at } => {
                    let (full_name, private) = match &job.made {
                        Some(full_name) => (full_name.clone(), job.plan.private),
                        None => {
                            let created = self.made(&job.plan, *sent_at)?;
                            (created.full_name, created.private)
                        }
                    };
                    GitHubPublishStep::Created {
                        committed: committed.clone(),
                        full_name,
                        private,
                    }
                }
                GitHubPublishStep::Created {
                    committed,
                    full_name,
                    private,
                } => {
                    let workflows = self.workflows_allowed(&job.project, committed)?;
                    match self.publish_push(&job.project, full_name, committed) {
                        Err(error) if error.0 == "github_publish_head_moved" && !rechecked => {
                            rechecked = true;
                            GitHubPublishStep::Checked
                        }
                        pushed => {
                            pushed?;
                            GitHubPublishStep::Pushed {
                                full_name: full_name.clone(),
                                private: *private,
                                workflows,
                            }
                        }
                    }
                }
                GitHubPublishStep::Pushed {
                    full_name,
                    private,
                    workflows,
                } => {
                    let facts = GitHubRepoFacts {
                        private: *private,
                        ci: if *workflows {
                            GitHubCi::Waiting
                        } else {
                            GitHubCi::None
                        },
                    };
                    let outcomes = self.apply_rules(full_name, &job.rules, &facts)?;
                    GitHubPublishStep::Done {
                        full_name: full_name.clone(),
                        outcomes,
                    }
                }
            };
            if next == GitHubPublishStep::Checked {
                assert!(
                    (rechecked && matches!(job.step, GitHubPublishStep::Created { .. }))
                        || (recommitted && job.step == GitHubPublishStep::Checked)
                );
                self.checked(job)?;
                continue;
            }
            if let GitHubPublishStep::Created { full_name, .. } = &next {
                job.made = Some(full_name.clone());
            }
            job.step = next;
            job.stopped = None;
            state::save_job(&self.root, job)?;
        }
    }

    /// Makes the job's repository, or, when GitHub says the name is taken, finds the one an
    /// earlier create of this job made since `sent_at`. Inputs: the plan and when the create
    /// was first sent. Output: the repository, or `github_repo_name_taken` for a name
    /// another repository holds, or the create's refusal.
    fn made(&self, plan: &GitHubPublishPlan, sent_at: u64) -> Result<GitHubCreated, EngineError> {
        let repo = new_repo(plan);
        match self.create_repo(&repo) {
            Err(error) if error.0 == "github_repo_name_taken" => {
                let token = self.store.get(&self.saved()?.credential)?;
                api::made_since(&self.endpoints, &token, &repo, sent_at)?.ok_or(error)
            }
            made => made,
        }
    }

    /// Tells whether the commit to push holds workflows, and refuses one the token cannot
    /// push: GitHub needs the `workflow` scope for it, and a token whose scopes GitHub does
    /// not name goes on. Inputs: the project folder and the commit. Output: whether there
    /// are workflows, or `github_publish_workflow_scope`.
    fn workflows_allowed(
        &self,
        project: &Path,
        committed: &GitHubCommitted,
    ) -> Result<bool, EngineError> {
        let git = publish::git()?;
        let home = publish::GitHome::new(&self.root)?;
        let workflows = publish::has_workflows(&git, home.path(), project, &committed.commit)?;
        if workflows {
            let token = self.store.get(&self.saved()?.credential)?;
            if api::scopes(&self.endpoints, &token)?
                .is_some_and(|scopes| !api::grants(&scopes, &WORKFLOW))
            {
                return Err(failure("github_publish_workflow_scope"));
            }
        }
        Ok(workflows)
    }
}
