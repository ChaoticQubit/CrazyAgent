use super::settings::{connection_lost, problem};
use super::*;
use crate::engine::EngineError;
use crate::engine::owner::{OwnerClient, SettingsSaved, TicketSettingsView, TrackedProject};
use crate::tracker::settings::TicketOverride;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};
use trackwright_cli::views::{Client, Key as ViewKey, Page, Views};
use trackwright_core::model::{OpKind, ProjectKey};

/// The most steps one Views job runs, so one job stays within a few tracker calls per key.
pub(in crate::tui) const STEPS_PER_JOB_MAX: usize = 8;

/// One thing the tracker views do on the owner worker: show a page, or act on one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Step {
    Open(Page),
    Key(ViewKey),
}

/// One tracker request a TUI screen sends. `Views` lends the tab's views to the owner worker,
/// which runs the steps against the owner protocol and gives them back; the rest read the
/// tracked projects, give a project its tickets, and read or save one scope's ticket
/// settings at the version the screen read. No project means the app scope.
pub(in crate::tui) enum TrackerJob {
    Projects,
    Attach {
        project: ProjectId,
        key: ProjectKey,
        name: String,
    },
    Views {
        project: ProjectId,
        views: Arc<Mutex<Views>>,
        steps: Vec<Step>,
    },
    Settings {
        project: Option<ProjectId>,
    },
    SaveSettings {
        project: Option<ProjectId>,
        values: TicketOverride,
        version: u64,
    },
}

/// What the owner worker answered a tracker request with, or a bounded plain code. A Views
/// reply names its project and the first transport problem its calls met; the views
/// themselves stay with the tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum TrackerEvent {
    Projects(Result<Vec<TrackedProject>, String>),
    Attached {
        project: ProjectId,
        result: Result<TrackedProject, String>,
    },
    Views {
        project: ProjectId,
        problem: Option<String>,
    },
    Settings {
        project: Option<ProjectId>,
        result: Result<TicketSettingsView, String>,
    },
    Saved {
        project: Option<ProjectId>,
        result: Result<SettingsSaved, String>,
    },
}

impl TrackerEvent {
    /// The reply for a job that reached no trusted connection. Input: the job. Output: its
    /// event carrying `trusted_connection_unavailable`.
    pub(in crate::tui) fn unavailable(job: &TrackerJob) -> Self {
        let code = "trusted_connection_unavailable".to_owned();
        match job {
            TrackerJob::Projects => Self::Projects(Err(code)),
            TrackerJob::Attach { project, .. } => Self::Attached {
                project: project.clone(),
                result: Err(code),
            },
            TrackerJob::Views { project, .. } => Self::Views {
                project: project.clone(),
                problem: Some(code),
            },
            TrackerJob::Settings { project } => Self::Settings {
                project: project.clone(),
                result: Err(code),
            },
            TrackerJob::SaveSettings { project, .. } => Self::Saved {
                project: project.clone(),
                result: Err(code),
            },
        }
    }
}

/// Names a tracker job for logs without its contents. Input: the job. Output: the name.
pub(in crate::tui) fn job_name(job: &TrackerJob) -> &'static str {
    match job {
        TrackerJob::Projects => "TrackerProjects",
        TrackerJob::Attach { .. } => "TrackerAttach",
        TrackerJob::Views { .. } => "TrackerViews",
        TrackerJob::Settings { .. } => "TrackerSettings",
        TrackerJob::SaveSettings { .. } => "TrackerSaveSettings",
    }
}

/// Runs the steps of one Views job on the views, in order, through the given client. A
/// poisoned lock means a step panicked earlier; its views are used as they stand. Inputs:
/// the views, the steps and the client.
pub(in crate::tui) fn run(views: &Mutex<Views>, steps: &[Step], client: &mut dyn Client) {
    assert!(steps.len() <= STEPS_PER_JOB_MAX);
    let mut views = views.lock().unwrap_or_else(PoisonError::into_inner);
    for step in steps {
        match *step {
            Step::Open(page) => views.open(page, client),
            Step::Key(key) => {
                views.key(key, client);
            }
        }
    }
}

/// A reply envelope for a call the owner protocol did not answer, so the views show the
/// code. Input: the bounded code. Output: the refused envelope.
pub(in crate::tui) fn refused(code: &str) -> Value {
    json!({
        "api": 1,
        "ok": false,
        "code": code,
        "message": "the engine did not answer this tracker call",
        "next": [],
    })
}

/// The views' client on the owner worker: each call runs `tracker_op` on the shared owner
/// client and waits for it on this blocking thread. The first transport failure is kept so
/// the worker can tell the shell the connection dropped; once it is a lost connection, the
/// job's later calls are refused with its code instead of each waiting out the timeout.
struct OwnerTracker {
    client: Arc<OwnerClient>,
    handle: tokio::runtime::Handle,
    failure: Option<EngineError>,
}

impl Client for OwnerTracker {
    fn call(&mut self, op: OpKind, input: Value) -> Value {
        if let Some(lost) = self
            .failure
            .as_ref()
            .filter(|error| connection_lost(&error.0))
        {
            return refused(&bounded_problem(&lost.0));
        }
        match self.handle.block_on(self.client.tracker_op(op, input)) {
            Ok(envelope) => envelope,
            Err(error) => {
                let code = bounded_problem(&error.0);
                self.failure.get_or_insert(error);
                refused(&code)
            }
        }
    }
}

impl NativeOwner {
    /// Runs one tracker job through the trusted owner controller. Inputs: the job and the
    /// lost-connection flag. Output: the event; a transport failure sets the flag so the
    /// shell reconnects.
    pub(super) async fn tracker(&mut self, job: TrackerJob, lost: &mut bool) -> TrackerEvent {
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => {
                let mut event = TrackerEvent::unavailable(&job);
                set_code(&mut event, code);
                return event;
            }
        };
        match job {
            TrackerJob::Projects => TrackerEvent::Projects(
                match controller.shared_client() {
                    Ok(client) => client.tracker_projects().await,
                    Err(error) => Err(error),
                }
                .map_err(|error| problem(&error, lost)),
            ),
            TrackerJob::Attach { project, key, name } => TrackerEvent::Attached {
                project: project.clone(),
                result: match controller.shared_client() {
                    Ok(client) => client.tracker_attach(project, key, name).await,
                    Err(error) => Err(error),
                }
                .map_err(|error| problem(&error, lost)),
            },
            TrackerJob::Views {
                project,
                views,
                steps,
            } => {
                let client = match controller.shared_client() {
                    Ok(client) => client,
                    Err(error) => {
                        return TrackerEvent::Views {
                            project,
                            problem: Some(problem(&error, lost)),
                        };
                    }
                };
                let handle = tokio::runtime::Handle::current();
                let ran = tokio::task::spawn_blocking(move || {
                    let mut tracker = OwnerTracker {
                        client,
                        handle,
                        failure: None,
                    };
                    run(&views, &steps, &mut tracker);
                    tracker.failure
                })
                .await;
                let problem = match ran {
                    Ok(failure) => failure.map(|error| problem(&error, lost)),
                    Err(_) => Some("tracker_views_failed".to_owned()),
                };
                TrackerEvent::Views { project, problem }
            }
            TrackerJob::Settings { project } => TrackerEvent::Settings {
                project: project.clone(),
                result: match controller.shared_client() {
                    Ok(client) => client.tracker_settings(project).await,
                    Err(error) => Err(error),
                }
                .map_err(|error| problem(&error, lost)),
            },
            TrackerJob::SaveSettings {
                project,
                values,
                version,
            } => TrackerEvent::Saved {
                project: project.clone(),
                result: match controller.shared_client() {
                    Ok(client) => client.tracker_save_settings(project, values, version).await,
                    Err(error) => Err(error),
                }
                .map_err(|error| problem(&error, lost)),
            },
        }
    }
}

fn set_code(event: &mut TrackerEvent, code: String) {
    match event {
        TrackerEvent::Projects(result) => *result = Err(code),
        TrackerEvent::Attached { result, .. } => *result = Err(code),
        TrackerEvent::Views { problem, .. } => *problem = Some(code),
        TrackerEvent::Settings { result, .. } => *result = Err(code),
        TrackerEvent::Saved { result, .. } => *result = Err(code),
    }
}

#[cfg(all(test, unix))]
pub(in crate::tui) mod tests;
