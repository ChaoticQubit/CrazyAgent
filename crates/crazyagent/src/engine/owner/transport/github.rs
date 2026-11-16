use super::*;
use crate::credentials::{SECRET_BYTES_MAX, SecretString};
use crate::engine::owner::AccountSecret;
use crate::github::{
    CloneReceipt, GitHub, GitHubDeviceCode, GitHubOwners, GitHubPublishJob, GitHubPublishLeft,
    GitHubPublishPlan, GitHubRepoPage, GitHubStatus,
};
use crate::protocol::{ProjectId, encode_frame};
use serde::{Deserialize, Serialize};

const TOKEN_FRAME_TIME: Duration = Duration::from_secs(5);
const ACTION_TIME: Duration = Duration::from_secs(75);
const CLONE_TIME: Duration = Duration::from_secs(1900);
const PUBLISH_TIME: Duration = Duration::from_secs(1900);
const DRAFT_AGENT: &str = "gitignore-draft";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Status,
    SignIn,
    Token,
    UseCli,
    SignOut,
    Repos {
        page: u32,
    },
    Clone {
        full_name: String,
    },
    Owners,
    PublishStart {
        project: ProjectId,
        plan: GitHubPublishPlan,
    },
    PublishApprove {
        project: ProjectId,
        lines: Vec<String>,
    },
    PublishExclude {
        project: ProjectId,
        path: String,
    },
    PublishRecheck {
        project: ProjectId,
    },
    PublishContinue {
        project: ProjectId,
    },
    PublishStatus {
        project: ProjectId,
    },
    PublishDiscard {
        project: ProjectId,
    },
}

impl Action {
    pub(super) fn time(&self) -> Duration {
        match self {
            Self::Clone { .. } => CLONE_TIME,
            Self::PublishApprove { .. }
            | Self::PublishExclude { .. }
            | Self::PublishRecheck { .. }
            | Self::PublishContinue { .. } => PUBLISH_TIME,
            _ => ACTION_TIME,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    github: Action,
    secret_present: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Status { status: GitHubStatus },
    Code { code: GitHubDeviceCode },
    Repos { page: GitHubRepoPage },
    Cloned { receipt: CloneReceipt },
    Owners { owners: GitHubOwners },
    Publish { job: GitHubPublishJob },
    PublishStatus { job: Option<GitHubPublishJob> },
    Discarded { left: Option<GitHubPublishLeft> },
    Rejected { code: String },
}

impl OwnerClient {
    /// Reads the GitHub account status the settings page shows.
    /// Output: signed in or not, login, method, waiting and a plain problem sentence.
    pub async fn github_status(&self) -> Result<GitHubStatus, EngineError> {
        match self.github_exchange(Action::Status, None).await? {
            Reply::Status { status } => Ok(status),
            _ => Err(unknown()),
        }
    }

    /// Starts a GitHub device sign-in; the engine finishes it once the owner enters the
    /// code, and `github_status` shows `waiting` until then. Output: the code to show.
    pub async fn github_sign_in(&self) -> Result<GitHubDeviceCode, EngineError> {
        match self.github_exchange(Action::SignIn, None).await? {
            Reply::Code { code } => Ok(code),
            _ => Err(unknown()),
        }
    }

    /// Signs in with a pasted GitHub token. The token travels only as a separate socket
    /// frame and the engine keeps it in the system credential store. Output: the status.
    pub async fn github_token(&self, token: AccountSecret) -> Result<GitHubStatus, EngineError> {
        match self.github_exchange(Action::Token, Some(token)).await? {
            Reply::Status { status } => Ok(status),
            _ => Err(unknown()),
        }
    }

    /// Signs in with the token of the signed-in GitHub CLI. Output: the status.
    pub async fn github_use_cli(&self) -> Result<GitHubStatus, EngineError> {
        match self.github_exchange(Action::UseCli, None).await? {
            Reply::Status { status } => Ok(status),
            _ => Err(unknown()),
        }
    }

    /// Signs out of GitHub and deletes the stored token. Output: the signed-out status.
    pub async fn github_sign_out(&self) -> Result<GitHubStatus, EngineError> {
        match self.github_exchange(Action::SignOut, None).await? {
            Reply::Status { status } => Ok(status),
            _ => Err(unknown()),
        }
    }

    /// Lists one page of the owner's repositories, most recently pushed first.
    /// Input: a page from 1 to 100. Output: the repositories and the next page, if any.
    pub async fn github_repos(&self, page: u32) -> Result<GitHubRepoPage, EngineError> {
        match self.github_exchange(Action::Repos { page }, None).await? {
            Reply::Repos { page } => Ok(page),
            _ => Err(unknown()),
        }
    }

    /// Clones one repository into the projects home, where it then shows as Found.
    /// Input: `owner/name`. Output: the receipt with the new project folder.
    pub async fn clone_repo(&self, full_name: String) -> Result<CloneReceipt, EngineError> {
        match self
            .github_exchange(Action::Clone { full_name }, None)
            .await?
        {
            Reply::Cloned { receipt } => Ok(receipt),
            _ => Err(unknown()),
        }
    }

    /// Lists the accounts a published project's repository can go under, the owner's own
    /// account first. Output: the accounts and a problem sentence, if any.
    pub async fn github_owners(&self) -> Result<GitHubOwners, EngineError> {
        match self.github_exchange(Action::Owners, None).await? {
            Reply::Owners { owners } => Ok(owners),
            _ => Err(unknown()),
        }
    }

    /// Starts publishing a project on GitHub: the engine checks the plan, the sign-in and
    /// the project's git, answers with the job at Drafting and drafts the .gitignore lines
    /// to show first after. Inputs: the project and the plan. Output: the job at Drafting.
    pub async fn github_publish_start(
        &self,
        project: ProjectId,
        plan: GitHubPublishPlan,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.github_publish(Action::PublishStart { project, plan })
            .await
    }

    /// Approves a project's .gitignore lines and runs its publish as far as it goes.
    /// Inputs: the project and the lines. Output: the job where it stands.
    pub async fn github_publish_approve(
        &self,
        project: ProjectId,
        lines: Vec<String>,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.github_publish(Action::PublishApprove { project, lines })
            .await
    }

    /// Leaves a file that holds a secret out of a project's publish and runs on.
    /// Inputs: the project and the file's path as the check named it. Output: the job.
    pub async fn github_publish_exclude(
        &self,
        project: ProjectId,
        path: String,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.github_publish(Action::PublishExclude { project, path })
            .await
    }

    /// Checks a blocked publish for secrets again and runs on. Input: the project.
    /// Output: the job where it stands.
    pub async fn github_publish_recheck(
        &self,
        project: ProjectId,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.github_publish(Action::PublishRecheck { project })
            .await
    }

    /// Runs a project's publish on from the last step it finished. Input: the project.
    /// Output: the job where it stands.
    pub async fn github_publish_continue(
        &self,
        project: ProjectId,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.github_publish(Action::PublishContinue { project })
            .await
    }

    /// Reads where a project's publish stands. Input: the project. Output: the job, or
    /// None when the project has none.
    pub async fn github_publish_status(
        &self,
        project: ProjectId,
    ) -> Result<Option<GitHubPublishJob>, EngineError> {
        match self
            .github_exchange(Action::PublishStatus { project }, None)
            .await?
        {
            Reply::PublishStatus { job } => Ok(job),
            _ => Err(unknown()),
        }
    }

    /// Forgets a project's publish before its push, so Start runs fresh. Input: the
    /// project. Output: the empty repository the job made, or the one its create may have
    /// made, if any; a job at Pushed or Done gives `github_publish_step_invalid`.
    pub async fn github_publish_discard(
        &self,
        project: ProjectId,
    ) -> Result<Option<GitHubPublishLeft>, EngineError> {
        match self
            .github_exchange(Action::PublishDiscard { project }, None)
            .await?
        {
            Reply::Discarded { left } => Ok(left),
            _ => Err(unknown()),
        }
    }

    async fn github_publish(&self, action: Action) -> Result<GitHubPublishJob, EngineError> {
        match self.github_exchange(action, None).await? {
            Reply::Publish { job } => Ok(job),
            _ => Err(unknown()),
        }
    }

    async fn github_exchange(
        &self,
        action: Action,
        token: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        let time = self.timeout.max(action.time());
        tokio::time::timeout(time, async {
            let read = read_kind(&action);
            let envelope = Envelope {
                github: action,
                secret_present: token.is_some(),
            };
            let frame = encode_frame(&envelope).map_err(|_| failure("github_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("github_owner_unavailable"))?;
            verify_peer(&stream)?;
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("github_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("github_owner_unavailable"))?;
            if let Some(token) = token {
                let mut bytes = token.into_bytes();
                let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
                framed.extend_from_slice(&bytes);
                bytes.fill(0);
                let sent = stream.write_all(&framed).await;
                framed.fill(0);
                sent.map_err(|_| failure("github_owner_unavailable"))?;
            }
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..])
                .map_err(|error| self.skewed_reply(&error, read, unknown))?
            {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

/// Serves one GitHub owner request after the owner exchange deadline: the action runs
/// under its own budget (75 s, a clone 1900 s) and the reply write under the owner wait.
/// Inputs: the stream, engine, authority, owner packet, admission and request bytes.
/// Output: the reply written, a rejection code included.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let Envelope {
        github: action,
        secret_present,
    } = serde_json::from_slice(bytes).map_err(|_| failure("github_request_invalid"))?;
    let time = action.time();
    let work = async {
        let token = if secret_present {
            Some(receive_token(stream).await)
        } else {
            None
        };
        match token.transpose() {
            Ok(token) => handle(engine, authority, packet, admission, action, token).await,
            Err(error) => Err(error),
        }
    };
    let outcome = tokio::time::timeout(time, work)
        .await
        .unwrap_or_else(|_| Err(unknown()));
    let reply = match outcome {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected {
            code: public_code(error),
        },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "github_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    tokio::time::timeout(engine.timeout, stream.write_all(&frame))
        .await
        .map_err(|_| unknown())?
        .map_err(io_error)
}

/// The engine root the GitHub sign-in lives under: the parent of the owner runtime's
/// per-start generation folder, so the saved sign-in and its credential store namespace
/// stay the same across engine restarts. Input: the authority. Output: the engine root,
/// or `github_store_unavailable`.
fn engine_root(authority: &Authority) -> Result<PathBuf, EngineError> {
    authority.root.verify()?;
    authority
        .root
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| failure("github_store_unavailable"))
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    action: Action,
    token: Option<SecretString>,
) -> Result<Reply, EngineError> {
    authenticate(engine, authority, packet, admission).await?;
    if token.is_some() != matches!(action, Action::Token) {
        return Err(failure("github_token_frame_mismatch"));
    }
    let home = match &action {
        Action::Clone { .. } => Some(projects_home(engine, admission).await?),
        _ => None,
    };
    let drafting = match &action {
        Action::PublishStart { .. } => Some(draft_tools(engine, authority)),
        _ => None,
    };
    let root = engine_root(authority)?;
    tokio::task::spawn_blocking(move || -> Result<Reply, EngineError> {
        let account = GitHub::open(&root)?;
        Ok(match action {
            Action::Status => Reply::Status {
                status: account.status(),
            },
            Action::SignIn => Reply::Code {
                code: account.sign_in()?,
            },
            Action::Token => Reply::Status {
                status: account.use_token(token.ok_or_else(unknown)?)?,
            },
            Action::UseCli => Reply::Status {
                status: account.use_cli()?,
            },
            Action::SignOut => Reply::Status {
                status: account.sign_out()?,
            },
            Action::Repos { page } => Reply::Repos {
                page: account.repos(page)?,
            },
            Action::Clone { full_name } => Reply::Cloned {
                receipt: account.clone_repo(&home.ok_or_else(unknown)?, &full_name)?,
            },
            Action::Owners => Reply::Owners {
                owners: account.owners()?,
            },
            Action::PublishStart { project, plan } => {
                let (settings, codex, claude) = drafting.ok_or_else(unknown)?;
                let draft = account.publish_begin(&project_folder(&root, &project)?, &plan)?;
                let job = draft.job().clone();
                let draft_root = root.clone();
                let _ = std::thread::Builder::new()
                    .name("github-draft".into())
                    .spawn(move || {
                        let _ = draft.run(|prompt| {
                            let (global, local) = settings
                                .as_ref()
                                .ok_or(crate::instruction_cleanup::UNAVAILABLE)?;
                            crate::instruction_cleanup::run(&crate::instruction_cleanup::Request {
                                root: &draft_root,
                                settings: (global, local),
                                codex: codex.as_deref(),
                                claude: claude.as_deref(),
                                project: Some(&project),
                                pool: None,
                                prompt,
                                agent: DRAFT_AGENT,
                            })
                        });
                    });
                Reply::Publish { job }
            }
            Action::PublishApprove { project, lines } => Reply::Publish {
                job: account.publish_approve(
                    &project_folder(&root, &project)?,
                    &crate::projects_home::owner_home()?,
                    &lines,
                )?,
            },
            Action::PublishExclude { project, path } => Reply::Publish {
                job: account.publish_exclude(
                    &project_folder(&root, &project)?,
                    &crate::projects_home::owner_home()?,
                    &path,
                )?,
            },
            Action::PublishRecheck { project } => Reply::Publish {
                job: account.publish_recheck(
                    &project_folder(&root, &project)?,
                    &crate::projects_home::owner_home()?,
                )?,
            },
            Action::PublishContinue { project } => Reply::Publish {
                job: account.publish_continue(
                    &project_folder(&root, &project)?,
                    &crate::projects_home::owner_home()?,
                )?,
            },
            Action::PublishStatus { project } => Reply::PublishStatus {
                job: account.publish_status(&project_folder(&root, &project)?)?,
            },
            Action::PublishDiscard { project } => Reply::Discarded {
                left: account.publish_discard(&project_folder(&root, &project)?)?,
            },
        })
    })
    .await
    .map_err(|_| unknown())?
}

/// What a publish's .gitignore draft runs with: the engine's settings files and the
/// model harnesses it verified, as Edit with AI runs. Inputs: the engine and the
/// authority. Output: the settings files and the Codex and Claude Code paths, each none
/// when the engine has not got it, so the draft falls back to the baseline.
fn draft_tools(
    engine: &Engine,
    authority: &Authority,
) -> (Option<(PathBuf, PathBuf)>, Option<PathBuf>, Option<PathBuf>) {
    let (codex, claude) = match authority.native_auth_host.lock() {
        Ok(host) => host.model_harnesses(),
        Err(_) => (None, None),
    };
    let path = |host: Option<Arc<crate::engine::native_auth_host::RegisteredNativeAuthHost>>| {
        host.and_then(|host| host.verified_path().ok().map(Path::to_path_buf))
    };
    (engine.settings_files(), path(codex), path(claude))
}

/// Finds the folder a publish works on from the project's stored setup, so a client names
/// a project by its id and never by a path. Inputs: the engine root and the project.
/// Output: the absolute folder, or `github_publish_project_unknown`,
/// `github_publish_unavailable`.
fn project_folder(root: &Path, project: &ProjectId) -> Result<PathBuf, EngineError> {
    let unavailable = || failure("github_publish_unavailable");
    let db = crate::landing::open(root).map_err(|_| unavailable())?;
    let folder = crate::project_setup::load_setup(&db, project)
        .map_err(|_| unavailable())?
        .ok_or_else(|| failure("github_publish_project_unknown"))?
        .project_folder();
    if !folder.is_absolute() {
        return Err(failure("github_publish_project_unknown"));
    }
    Ok(folder)
}

async fn projects_home(engine: &Engine, admission: &Admission) -> Result<PathBuf, EngineError> {
    let read = engine
        .owner_job(|reply| Job::OwnerRead(admission.clone(), OwnerRead::ProjectsHome, reply))
        .await?;
    let OwnerData::ProjectsHome { home } = read.data else {
        return Err(unknown());
    };
    home.path
        .map(PathBuf::from)
        .ok_or_else(|| failure("projects_home_not_set"))
}

async fn authenticate(
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
    admission: &Admission,
) -> Result<(), EngineError> {
    let _barrier = engine.owner_egress.write().await;
    if &authority.authenticate(packet)? != admission {
        return Err(failure("owner_grant_stale_or_revoked"));
    }
    Ok(())
}

async fn receive_token(stream: &mut UnixStream) -> Result<SecretString, EngineError> {
    tokio::time::timeout(TOKEN_FRAME_TIME, async {
        let invalid = || failure("github_token_frame_invalid");
        let size = stream.read_u32().await.map_err(|_| invalid())? as usize;
        if size == 0 || size > SECRET_BYTES_MAX {
            return Err(invalid());
        }
        let mut bytes = vec![0; size];
        if stream.read_exact(&mut bytes).await.is_err() {
            bytes.fill(0);
            return Err(invalid());
        }
        SecretString::new(bytes).map_err(|_| failure("github_token_invalid"))
    })
    .await
    .map_err(|_| failure("github_token_frame_invalid"))?
}

fn public_code(error: EngineError) -> String {
    if error.0.len() <= 96
        && error
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        error.0
    } else {
        "github_outcome_unknown".into()
    }
}

/// Names the requests that only read, for the log of replies this client could not read.
/// Input: the request. Output: its name, None for a request that changes something.
fn read_kind(action: &Action) -> Option<&'static str> {
    match action {
        Action::Status => Some("github.status"),
        Action::Repos { .. } => Some("github.repos"),
        Action::Owners => Some("github.owners"),
        Action::PublishStatus { .. } => Some("github.publish_status"),
        _ => None,
    }
}

fn unknown() -> EngineError {
    failure("github_outcome_unknown")
}
