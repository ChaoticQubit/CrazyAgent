use super::app_plugins::read_manifest;
use super::settings::problem;
use super::*;
use crate::app_plugins::Preview;
use crate::connectors::audit::ActivityView;
use crate::connectors::{ConnectorSettings, ConnectorsView, Who};
use crate::engine::EngineError;
use crate::engine::owner::{AccountSecret, ConnectorsSaved};
use std::path::PathBuf;

/// One Connectors request a Settings page or a project's Connectors page sends. A typed key
/// travels only inside `SetKey`, as the engine's secret type, and is never printed.
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
}

impl std::fmt::Debug for ConnectorsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(job_name(self))
    }
}

/// The engine's answer to one Connectors request: the activity view, how a settings save
/// ended, the Connectors page every page change answers with, or a trust screen (`read`
/// carries the text a typed path held).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ConnectorsEvent {
    Activity(Result<ActivityView, String>),
    Saved(Result<ConnectorsSaved, String>),
    Page(Result<ConnectorsView, String>),
    Preview {
        read: Option<String>,
        result: Result<Preview, String>,
    },
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
        | ConnectorsJob::Grant { .. } => ConnectorsEvent::Page(Err(code)),
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
    }
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
        assert!(!matches!(job, ConnectorsJob::ReadFile { .. }), "a typed path is read above");
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
        ConnectorsJob::Activity { .. }
        | ConnectorsJob::SaveSettings { .. }
        | ConnectorsJob::Preview { .. }
        | ConnectorsJob::ReadFile { .. }
        | ConnectorsJob::Review { .. } => unreachable!("{} has its own answer", job_name(&job)),
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
}
