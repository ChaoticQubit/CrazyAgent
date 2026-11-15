use super::{
    GitHub, GitHubCi, GitHubCommitted, GitHubCreated, GitHubHistory, GitHubNewRepo, GitHubPrepare,
    GitHubRepoFacts, GitHubRule, GitHubRuleOutcome, GitHubSecretScan, api, publish, state,
};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const BASELINE: [&str; 4] = [".env*", "*.pem", "*.key", ".DS_Store"];
const WORKFLOW: [&str; 1] = ["workflow"];
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

/// Where a publish job stands: waiting for the owner to approve the .gitignore lines or
/// to deal with a secret, or the last step it finished on the way to the rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitHubPublishStep {
    Lines {
        lines: Vec<String>,
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
/// approved, the paths publish wrote, the step reached, and the code of the failure that
/// stopped it there, if one did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubPublishJob {
    pub project: PathBuf,
    pub plan: GitHubPublishPlan,
    pub lines: Vec<String>,
    pub written: Vec<String>,
    pub step: GitHubPublishStep,
    pub stopped: Option<String>,
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
    /// Starts a publish: checks the plan, the sign-in and the project's git, and records
    /// the .gitignore lines the owner is shown first. Nothing is written into the project.
    /// A job that made its repository already is gone on with Continue instead, and a
    /// start over an earlier job keeps the paths it wrote, which the commit then takes.
    /// Inputs: the project folder and the plan. Output: the job at its lines, or
    /// `github_repo_name_invalid`, `github_repo_description_invalid`,
    /// `github_publish_running`, `github_not_signed_in`, `github_publish_in_progress` or the
    /// project's git refusal.
    pub fn publish_start(
        &self,
        project: &Path,
        plan: &GitHubPublishPlan,
    ) -> Result<GitHubPublishJob, EngineError> {
        api::check_new(&new_repo(plan))?;
        let _held = publishing(&self.root, project)?;
        self.saved()?;
        let written = match state::load_job(&self.root, project)? {
            Some(job)
                if matches!(
                    job.step,
                    GitHubPublishStep::Created { .. } | GitHubPublishStep::Pushed { .. }
                ) =>
            {
                return Err(failure("github_publish_in_progress"));
            }
            Some(job) => job.written,
            None => Vec::new(),
        };
        self.publish_inspect(project)?;
        let job = GitHubPublishJob {
            project: project.to_path_buf(),
            plan: plan.clone(),
            lines: Vec::new(),
            written,
            step: GitHubPublishStep::Lines {
                lines: BASELINE.map(String::from).to_vec(),
            },
            stopped: None,
        };
        state::save_job(&self.root, &job)?;
        Ok(job)
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
            GitHubPublishStep::Lines { .. } | GitHubPublishStep::Blocked { .. }
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

    /// Reads a project's publish record, which a run in progress keeps up to date step by
    /// step. Input: the project folder. Output: the job, or None when the project has none.
    pub fn publish_status(&self, project: &Path) -> Result<Option<GitHubPublishJob>, EngineError> {
        state::load_job(&self.root, project)
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
        };
        for path in self.publish_prepare(&job.project, &local, &choices)? {
            if !job.written.contains(&path) {
                job.written.push(path);
            }
        }
        self.checked(job)
    }

    /// Checks what the publish would push for secrets, the files publish wrote included,
    /// and records the job as checked, or blocked on what was found. Input: the job.
    /// Output: `()`, or the refusal.
    fn checked(&self, job: &mut GitHubPublishJob) -> Result<(), EngineError> {
        let history = self.publish_inspect(&job.project)?.history;
        let mut scan = self.publish_secrets(&job.project, history)?;
        publish::written_findings(&job.project, &job.written, &mut scan)?;
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
    /// the rules. A step that fails leaves the job at the last one finished. Inputs: the
    /// job and the owner's home folder. Output: `()`, or the failure.
    fn advance(&self, job: &mut GitHubPublishJob, owner_home: &Path) -> Result<(), EngineError> {
        loop {
            let next = match &job.step {
                GitHubPublishStep::Lines { .. }
                | GitHubPublishStep::Blocked { .. }
                | GitHubPublishStep::Done { .. } => return Ok(()),
                GitHubPublishStep::Checked => GitHubPublishStep::Committed {
                    committed: self.publish_commit(&job.project, owner_home, &job.written)?,
                },
                GitHubPublishStep::Committed { committed } => GitHubPublishStep::Creating {
                    committed: committed.clone(),
                    sent_at: now()?,
                },
                GitHubPublishStep::Creating { committed, sent_at } => {
                    let created = self.made(&job.plan, *sent_at)?;
                    GitHubPublishStep::Created {
                        committed: committed.clone(),
                        full_name: created.full_name,
                        private: created.private,
                    }
                }
                GitHubPublishStep::Created {
                    committed,
                    full_name,
                    private,
                } => {
                    let workflows = self.workflows_allowed(&job.project, committed)?;
                    self.publish_push(&job.project, full_name, committed)?;
                    GitHubPublishStep::Pushed {
                        full_name: full_name.clone(),
                        private: *private,
                        workflows,
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
                    let outcomes = self.apply_rules(full_name, &GitHubRule::defaults(), &facts)?;
                    GitHubPublishStep::Done {
                        full_name: full_name.clone(),
                        outcomes,
                    }
                }
            };
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
