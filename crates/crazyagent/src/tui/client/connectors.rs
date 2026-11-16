use super::app_plugins::read_manifest;
use super::settings::problem;
use super::*;
use crate::app_plugins::Preview;
use crate::connectors::approvals::{AllowedWrites, ApprovalView};
use crate::connectors::audit::ActivityView;
use crate::connectors::sign_in::SignInState;
use crate::connectors::{ConnectorSettings, ConnectorsView, SignInWith, Who};
use crate::engine::EngineError;
use crate::engine::owner::{AccountSecret, ConnectorsSaved};
use crate::tui::screens::settings_add_account::open_page;
use std::path::PathBuf;

/// One Connectors request a Settings page, a project's Connectors page or the approval page
/// sends. A typed key travels only inside `SetKey`, and an own app's client secret only inside
/// `SetOwnApp`, as the engine's secret type, and neither is ever printed.
pub(in crate::tui) enum ConnectorsJob {
    Activity {
        connector: Option<String>,
        project: Option<String>,
    },
    SaveSettings {
        settings: ConnectorSettings,
        version: u64,
    },
    View {
        project: Option<String>,
    },
    Preview {
        yaml: String,
    },
    ReadFile {
        path: PathBuf,
    },
    Review {
        name: String,
    },
    Add {
        yaml: String,
        sha256: String,
        replace: bool,
    },
    Trust {
        name: String,
        sha256: String,
    },
    Remove {
        name: String,
    },
    SetKey {
        name: String,
        secret: String,
        key: AccountSecret,
    },
    ClearKey {
        name: String,
        secret: String,
    },
    Grant {
        project: String,
        connector: String,
        enabled: bool,
        who: Who,
    },
    Approval {
        id: i64,
    },
    Answer {
        id: i64,
        allow: bool,
    },
    Writes {
        project: String,
    },
    Revoke {
        project: String,
        connector: String,
    },
    SetSignInWith {
        name: String,
        way: SignInWith,
    },
    SignIn {
        name: String,
    },
    SignInState {
        name: String,
    },
    CancelSignIn {
        name: String,
    },
    SignOut {
        name: String,
    },
    SetOwnApp {
        name: String,
        client_id: String,
        secret: Option<AccountSecret>,
    },
    ClearOwnApp {
        name: String,
    },
}

impl std::fmt::Debug for ConnectorsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(job_name(self))
    }
}

/// The engine's answer to one Connectors request: the activity view, how a settings save
/// ended, the Connectors page every page change answers with, a trust screen (`read`
/// carries the text a typed path held), one write approval as read or answered, or a
/// project's standing write allows, a started sign-in or where one stands. Those after the
/// trust screen name the write, project or connector they answer for, failed or not, so only
/// the page that asked takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ConnectorsEvent {
    Activity(Result<ActivityView, String>),
    Saved(Result<ConnectorsSaved, String>),
    Page(Result<ConnectorsView, String>),
    Preview {
        read: Option<String>,
        result: Result<Preview, String>,
    },
    Approval {
        id: i64,
        result: Result<ApprovalView, String>,
    },
    Writes {
        project: String,
        result: Result<AllowedWrites, String>,
    },
    SignIn {
        name: String,
        result: Result<SignInStarted, String>,
    },
    SignInState {
        name: String,
        result: Result<SignInState, String>,
    },
}

/// A started sign-in as the TUI shows it: the address the owner's browser opens, shown for
/// copying and sent nowhere else, and whether this machine's opener took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct SignInStarted {
    pub url: String,
    pub opened: bool,
}

impl ConnectorsEvent {
    /// The reply for a request that reached no trusted connection. Input: the job. Output:
    /// its event carrying `trusted_connection_unavailable`.
    pub(in crate::tui) fn unavailable(job: &ConnectorsJob) -> Self {
        failed(job, "trusted_connection_unavailable".into())
    }
}

/// The reply a job gets when it fails before or at the engine. Inputs: the job and the
/// bounded code. Output: the event of the job's kind carrying the code.
fn failed(job: &ConnectorsJob, code: String) -> ConnectorsEvent {
    match job {
        ConnectorsJob::Activity { .. } => ConnectorsEvent::Activity(Err(code)),
        ConnectorsJob::SaveSettings { .. } => ConnectorsEvent::Saved(Err(code)),
        ConnectorsJob::Preview { .. }
        | ConnectorsJob::ReadFile { .. }
        | ConnectorsJob::Review { .. } => ConnectorsEvent::Preview {
            read: None,
            result: Err(code),
        },
        ConnectorsJob::View { .. }
        | ConnectorsJob::Add { .. }
        | ConnectorsJob::Trust { .. }
        | ConnectorsJob::Remove { .. }
        | ConnectorsJob::SetKey { .. }
        | ConnectorsJob::ClearKey { .. }
        | ConnectorsJob::Grant { .. }
        | ConnectorsJob::SetSignInWith { .. }
        | ConnectorsJob::SignOut { .. }
        | ConnectorsJob::SetOwnApp { .. }
        | ConnectorsJob::ClearOwnApp { .. } => ConnectorsEvent::Page(Err(code)),
        ConnectorsJob::Approval { id } | ConnectorsJob::Answer { id, .. } => {
            ConnectorsEvent::Approval {
                id: *id,
                result: Err(code),
            }
        }
        ConnectorsJob::Writes { project } | ConnectorsJob::Revoke { project, .. } => {
            ConnectorsEvent::Writes {
                project: project.clone(),
                result: Err(code),
            }
        }
        ConnectorsJob::SignIn { name } => ConnectorsEvent::SignIn {
            name: name.clone(),
            result: Err(code),
        },
        ConnectorsJob::SignInState { name } | ConnectorsJob::CancelSignIn { name } => {
            ConnectorsEvent::SignInState {
                name: name.clone(),
                result: Err(code),
            }
        }
    }
}

/// The name a job is logged and refused under, never its text or key. Input: the job.
/// Output: the name.
pub(in crate::tui) fn job_name(job: &ConnectorsJob) -> &'static str {
    match job {
        ConnectorsJob::Activity { .. } => "ConnectorActivity",
        ConnectorsJob::SaveSettings { .. } => "SetConnectorSettings",
        ConnectorsJob::View { .. } => "Connectors",
        ConnectorsJob::Preview { .. } => "ConnectorPreview",
        ConnectorsJob::ReadFile { .. } => "ConnectorReadFile",
        ConnectorsJob::Review { .. } => "ConnectorReview",
        ConnectorsJob::Add { .. } => "ConnectorAdd",
        ConnectorsJob::Trust { .. } => "ConnectorTrust",
        ConnectorsJob::Remove { .. } => "ConnectorRemove",
        ConnectorsJob::SetKey { .. } => "ConnectorSetKey",
        ConnectorsJob::ClearKey { .. } => "ConnectorClearKey",
        ConnectorsJob::Grant { .. } => "ConnectorGrant",
        ConnectorsJob::Approval { .. } => "ConnectorApproval",
        ConnectorsJob::Answer { .. } => "ConnectorAnswer",
        ConnectorsJob::Writes { .. } => "ConnectorWrites",
        ConnectorsJob::Revoke { .. } => "ConnectorRevokeWrites",
        ConnectorsJob::SetSignInWith { .. } => "ConnectorSetSignInWith",
        ConnectorsJob::SignIn { .. } => "ConnectorSignIn",
        ConnectorsJob::SignInState { .. } => "ConnectorSignInState",
        ConnectorsJob::CancelSignIn { .. } => "ConnectorCancelSignIn",
        ConnectorsJob::SignOut { .. } => "ConnectorSignOut",
        ConnectorsJob::SetOwnApp { .. } => "ConnectorSetOwnApp",
        ConnectorsJob::ClearOwnApp { .. } => "ConnectorClearOwnApp",
    }
}

/// Opens a started sign-in's address in the owner's browser off the async threads; the
/// engine's callback listens on this machine, which the TUI shares. Input: the address.
/// Output: the address with whether the opener took it.
async fn opened(url: String) -> SignInStarted {
    let address = url.clone();
    let job = move || open_page(&address);
    let opened = matches!(tokio::task::spawn_blocking(job).await, Ok(Ok(())));
    SignInStarted { url, opened }
}

impl NativeOwner {
    /// Runs one Connectors request through the trusted owner controller; a typed path is
    /// read here first, off the async threads, and sent as its text. Inputs: the job and the
    /// lost-connection flag. Output: the event; a transport failure sets the flag so the
    /// shell reconnects.
    pub(super) async fn connectors(
        &mut self,
        job: ConnectorsJob,
        lost: &mut bool,
    ) -> ConnectorsEvent {
        let (job, read) = match job {
            ConnectorsJob::ReadFile { path } => {
                let text = tokio::task::spawn_blocking(move || read_manifest(&path))
                    .await
                    .unwrap_or_else(|_| Err("app_plugin_file_unreadable".into()));
                match text {
                    Ok(yaml) => (ConnectorsJob::Preview { yaml: yaml.clone() }, Some(yaml)),
                    Err(code) => {
                        return ConnectorsEvent::Preview {
                            read: None,
                            result: Err(code),
                        };
                    }
                }
            }
            other => (other, None),
        };
        assert!(
            !matches!(job, ConnectorsJob::ReadFile { .. }),
            "a typed path is read above"
        );
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => return failed(&job, code),
        };
        let trust = |result: Result<Preview, EngineError>, read, lost: &mut bool| {
            ConnectorsEvent::Preview {
                read,
                result: result.map_err(|error| problem(&error, lost)),
            }
        };
        match job {
            ConnectorsJob::Activity { connector, project } => ConnectorsEvent::Activity(
                controller
                    .connector_activity(connector, project)
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            ConnectorsJob::SaveSettings { settings, version } => ConnectorsEvent::Saved(
                controller
                    .set_connector_settings(settings, version)
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            ConnectorsJob::Preview { yaml } => {
                trust(controller.connector_preview(yaml).await, read, lost)
            }
            ConnectorsJob::Review { name } => {
                trust(controller.connector_review(name).await, None, lost)
            }
            ConnectorsJob::ReadFile { .. } => unreachable!("a typed path is read above"),
            ConnectorsJob::Approval { id } => ConnectorsEvent::Approval {
                id,
                result: controller
                    .connector_approval(id)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            ConnectorsJob::Answer { id, allow } => ConnectorsEvent::Approval {
                id,
                result: controller
                    .answer_connector_write(id, allow)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            ConnectorsJob::Writes { project } => ConnectorsEvent::Writes {
                project: project.clone(),
                result: controller
                    .connector_writes(project)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            ConnectorsJob::Revoke { project, connector } => ConnectorsEvent::Writes {
                project: project.clone(),
                result: controller
                    .revoke_connector_writes(project, connector)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            ConnectorsJob::SignIn { name } => ConnectorsEvent::SignIn {
                name: name.clone(),
                result: match controller.connector_sign_in(name).await {
                    Ok(url) => Ok(opened(url).await),
                    Err(error) => Err(problem(&error, lost)),
                },
            },
            ConnectorsJob::SignInState { name } => ConnectorsEvent::SignInState {
                name: name.clone(),
                result: controller
                    .connector_sign_in_state(name)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            ConnectorsJob::CancelSignIn { name } => ConnectorsEvent::SignInState {
                name: name.clone(),
                result: controller
                    .cancel_connector_sign_in(name)
                    .await
                    .map_err(|error| problem(&error, lost)),
            },
            other => ConnectorsEvent::Page(
                page(controller, other)
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
        }
    }
}

/// Runs one request the engine answers with the whole Connectors page. Inputs: the
/// controller and the job. Output: the page or the engine's error.
async fn page(
    controller: &mut OwnerController,
    job: ConnectorsJob,
) -> Result<ConnectorsView, EngineError> {
    match job {
        ConnectorsJob::View { project } => controller.connectors(project).await,
        ConnectorsJob::Add {
            yaml,
            sha256,
            replace,
        } => controller.connector_add(yaml, sha256, replace).await,
        ConnectorsJob::Trust { name, sha256 } => controller.connector_trust(name, sha256).await,
        ConnectorsJob::Remove { name } => controller.connector_remove(name).await,
        ConnectorsJob::SetKey { name, secret, key } => {
            controller.connector_set_key(name, secret, key).await
        }
        ConnectorsJob::ClearKey { name, secret } => {
            controller.connector_clear_key(name, secret).await
        }
        ConnectorsJob::Grant {
            project,
            connector,
            enabled,
            who,
        } => {
            controller
                .connector_grant(project, connector, enabled, who)
                .await
        }
        ConnectorsJob::SetSignInWith { name, way } => {
            controller.connector_set_sign_in_with(name, way).await
        }
        ConnectorsJob::SignOut { name } => controller.connector_sign_out(name).await,
        ConnectorsJob::SetOwnApp {
            name,
            client_id,
            secret,
        } => {
            controller
                .connector_set_own_app(name, client_id, secret)
                .await
        }
        ConnectorsJob::ClearOwnApp { name } => controller.connector_clear_own_app(name).await,
        ConnectorsJob::Activity { .. }
        | ConnectorsJob::SaveSettings { .. }
        | ConnectorsJob::Preview { .. }
        | ConnectorsJob::ReadFile { .. }
        | ConnectorsJob::Review { .. }
        | ConnectorsJob::Approval { .. }
        | ConnectorsJob::Answer { .. }
        | ConnectorsJob::Writes { .. }
        | ConnectorsJob::Revoke { .. }
        | ConnectorsJob::SignIn { .. }
        | ConnectorsJob::SignInState { .. }
        | ConnectorsJob::CancelSignIn { .. } => {
            unreachable!("{} has its own answer", job_name(&job))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_connector_job_prints_its_name_never_its_key() {
        let job = ConnectorsJob::SetKey {
            name: "linear".into(),
            secret: "api_key".into(),
            key: AccountSecret::new(b"lin-test-never-printed".to_vec()).unwrap(),
        };
        assert_eq!(format!("{job:?}"), "ConnectorSetKey");
        assert!(!format!("{job:?}").contains("lin-test"));
        let app = ConnectorsJob::SetOwnApp {
            name: "github".into(),
            client_id: "Iv1.app".into(),
            secret: Some(AccountSecret::new(b"gh-app-never-printed".to_vec()).unwrap()),
        };
        assert_eq!(format!("{app:?}"), "ConnectorSetOwnApp");
        assert!(!format!("{app:?}").contains("gh-app") && !format!("{app:?}").contains("Iv1"));
        let clear = ConnectorsJob::ClearOwnApp {
            name: "github".into(),
        };
        assert_eq!(format!("{clear:?}"), "ConnectorClearOwnApp");
        for job in [&app, &clear] {
            assert_eq!(
                ConnectorsEvent::unavailable(job),
                ConnectorsEvent::Page(Err("trusted_connection_unavailable".into()))
            );
        }
        assert_eq!(
            ConnectorsEvent::unavailable(&job),
            ConnectorsEvent::Page(Err("trusted_connection_unavailable".into()))
        );
        assert_eq!(
            ConnectorsEvent::unavailable(&ConnectorsJob::ReadFile {
                path: PathBuf::from("/Users/owner/linear.yaml"),
            }),
            ConnectorsEvent::Preview {
                read: None,
                result: Err("trusted_connection_unavailable".into()),
            }
        );
    }

    #[test]
    fn an_approval_or_writes_job_has_its_own_name_and_unavailable_answer() {
        let gone = || "trusted_connection_unavailable".to_owned();
        let approval = || ConnectorsEvent::Approval {
            id: 7,
            result: Err(gone()),
        };
        let writes = || ConnectorsEvent::Writes {
            project: "alpha".into(),
            result: Err(gone()),
        };
        let cases = [
            (
                ConnectorsJob::Approval { id: 7 },
                "ConnectorApproval",
                approval(),
            ),
            (
                ConnectorsJob::Answer { id: 7, allow: true },
                "ConnectorAnswer",
                approval(),
            ),
            (
                ConnectorsJob::Writes {
                    project: "alpha".into(),
                },
                "ConnectorWrites",
                writes(),
            ),
            (
                ConnectorsJob::Revoke {
                    project: "alpha".into(),
                    connector: "linear".into(),
                },
                "ConnectorRevokeWrites",
                writes(),
            ),
        ];
        for (job, name, unavailable) in cases {
            assert_eq!(format!("{job:?}"), name);
            assert_eq!(ConnectorsEvent::unavailable(&job), unavailable, "{name}");
        }
    }

    #[test]
    fn a_sign_in_job_has_its_own_name_and_unavailable_answer_naming_its_connector() {
        let gone = || "trusted_connection_unavailable".to_owned();
        let notion = || "notion".to_owned();
        let state = || ConnectorsEvent::SignInState {
            name: notion(),
            result: Err(gone()),
        };
        let started = ConnectorsEvent::SignIn {
            name: notion(),
            result: Err(gone()),
        };
        let cases = [
            (
                ConnectorsJob::SignIn { name: notion() },
                "ConnectorSignIn",
                started,
            ),
            (
                ConnectorsJob::SignInState { name: notion() },
                "ConnectorSignInState",
                state(),
            ),
            (
                ConnectorsJob::CancelSignIn { name: notion() },
                "ConnectorCancelSignIn",
                state(),
            ),
            (
                ConnectorsJob::SignOut { name: notion() },
                "ConnectorSignOut",
                ConnectorsEvent::Page(Err(gone())),
            ),
        ];
        for (job, name, unavailable) in cases {
            assert_eq!(format!("{job:?}"), name);
            assert_eq!(ConnectorsEvent::unavailable(&job), unavailable, "{name}");
        }
    }
}
