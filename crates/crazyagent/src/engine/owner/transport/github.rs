use super::*;
use crate::credentials::{SECRET_BYTES_MAX, SecretString};
use crate::engine::owner::AccountSecret;
use crate::github::{CloneReceipt, GitHub, GitHubDeviceCode, GitHubRepoPage, GitHubStatus};
use crate::protocol::encode_frame;
use serde::{Deserialize, Serialize};

const TOKEN_FRAME_TIME: Duration = Duration::from_secs(5);
const ACTION_TIME: Duration = Duration::from_secs(75);
const CLONE_TIME: Duration = Duration::from_secs(1900);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status,
    SignIn,
    Token,
    UseCli,
    SignOut,
    Repos { page: u32 },
    Clone { full_name: String },
}

impl Action {
    fn time(&self) -> Duration {
        match self {
            Self::Clone { .. } => CLONE_TIME,
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

    async fn github_exchange(
        &self,
        action: Action,
        token: Option<AccountSecret>,
    ) -> Result<Reply, EngineError> {
        let time = self.timeout.max(action.time());
        tokio::time::timeout(time, async {
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
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("github_request_invalid"))?;
    let token = if envelope.secret_present {
        Some(receive_token(stream).await)
    } else {
        None
    };
    let outcome = match token.transpose() {
        Ok(token) => handle(engine, authority, packet, admission, envelope.github, token).await,
        Err(error) => Err(error),
    };
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
    stream.write_all(&frame).await.map_err(io_error)
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
        })
    })
    .await
    .map_err(|_| unknown())?
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

fn unknown() -> EngineError {
    failure("github_outcome_unknown")
}
