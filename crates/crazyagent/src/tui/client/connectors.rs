use super::settings::problem;
use super::*;
use crate::connectors::ConnectorSettings;
use crate::connectors::audit::ActivityView;
use crate::engine::owner::ConnectorsSaved;

/// One Connectors request a Settings page sends: read the activity log with its filters,
/// or save the connector settings with the version the page read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ConnectorsJob {
    Activity {
        connector: Option<String>,
        project: Option<String>,
    },
    SaveSettings {
        settings: ConnectorSettings,
        version: u64,
    },
}

/// The engine's answer to one Connectors request: the activity view, or how a settings
/// save ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ConnectorsEvent {
    Activity(Result<ActivityView, String>),
    Saved(Result<ConnectorsSaved, String>),
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
    }
}

/// The name a job is logged and refused under. Input: the job. Output: the name.
pub(in crate::tui) fn job_name(job: &ConnectorsJob) -> &'static str {
    match job {
        ConnectorsJob::Activity { .. } => "ConnectorActivity",
        ConnectorsJob::SaveSettings { .. } => "SetConnectorSettings",
    }
}

impl NativeOwner {
    /// Runs one Connectors request through the trusted owner controller. Inputs: the job
    /// and the lost-connection flag. Output: the event; a transport failure sets the flag
    /// so the shell reconnects.
    pub(super) async fn connectors(
        &mut self,
        job: ConnectorsJob,
        lost: &mut bool,
    ) -> ConnectorsEvent {
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => return failed(&job, code),
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
        }
    }
}
