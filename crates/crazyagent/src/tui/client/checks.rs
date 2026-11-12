use super::settings::{connection_lost, problem};
use super::*;
use crate::checks::queue::JobState;
use crate::checks::settings::ChecksChoice;
use crate::checks::{ProfileId, ProfileSummary, StoredProfile};
use crate::engine::EngineError;
use crate::engine::owner::{
    CheckGateView, CheckRunView, ChecksReply, ChecksRequest, ChecksSettingsView, OwnerClient,
    RUNS_LATEST_MAX, SettingsSaved,
};
use crate::landing::ChangeSetId;
use crate::tui::screens::tests::{Step, TestViews};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};
use trackwright_cli::views::Client;
use trackwright_core::model::{OpKind, ProjectKey};

/// The most steps one Views job runs, so one job stays within a few tracker calls per key.
pub(in crate::tui) const STEPS_PER_JOB_MAX: usize = 8;
const PROFILE_PAGES_MAX: usize = 2;
const UNKNOWN: &str = "checks_outcome_unknown";

/// One checks request a TUI screen sends. `Tracked` asks for the project's tracker key;
/// `Views` lends the tab's test views to the owner worker, which runs the steps against the
/// owner protocol as the owner and gives them back; the rest read or save one scope's check
/// settings at the version the screen read, list a project's stored profiles, read one
/// whole profile for the owner to review, approve or revoke one, read the newest run of
/// each check of a change set with its Accept gate, and start its checks. No project means
/// the app scope.
pub(in crate::tui) enum ChecksJob {
    Tracked {
        project: ProjectId,
    },
    Views {
        project: ProjectId,
        views: Arc<Mutex<TestViews>>,
        steps: Vec<Step>,
    },
    Settings {
        project: Option<ProjectId>,
    },
    SaveSettings {
        project: Option<ProjectId>,
        values: ChecksChoice,
        version: u64,
    },
    Profiles {
        project: ProjectId,
    },
    Show {
        project: ProjectId,
        profile: ProfileId,
    },
    Approve {
        project: ProjectId,
        profile: ProfileId,
    },
    Revoke {
        project: ProjectId,
        profile: ProfileId,
    },
    Runs {
        project: ProjectId,
        change_set: ChangeSetId,
    },
    Run {
        project: ProjectId,
        change_set: ChangeSetId,
    },
}

/// A change set's checks as its screen shows them: the newest run of each check, at most
/// `RUNS_LATEST_MAX`, how many check jobs still wait or run, and what the Accept gate would
/// meet at the head now, or the code it could not be read with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct ChangeChecks {
    pub runs: Vec<CheckRunView>,
    pub pending: usize,
    pub gate: Result<CheckGateView, String>,
}

/// What the owner worker answered a checks request with, or a bounded plain code. A Views
/// reply names its project and the first transport problem its calls met; the views
/// themselves stay with the tab. `Shown` carries a whole stored profile whose body hashes to
/// the id asked for. `Started` carries the batch the engine queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum ChecksEvent {
    Tracked {
        project: ProjectId,
        result: Result<Option<ProjectKey>, String>,
    },
    Views {
        project: ProjectId,
        problem: Option<String>,
    },
    Settings {
        project: Option<ProjectId>,
        result: Result<ChecksSettingsView, String>,
    },
    Saved {
        project: Option<ProjectId>,
        result: Result<SettingsSaved, String>,
    },
    Profiles {
        project: ProjectId,
        result: Result<Vec<ProfileSummary>, String>,
    },
    Shown {
        project: ProjectId,
        profile: ProfileId,
        result: Result<Box<StoredProfile>, String>,
    },
    Profile {
        project: ProjectId,
        profile: ProfileId,
        approve: bool,
        result: Result<(), String>,
    },
    Runs {
        change_set: ChangeSetId,
        result: Result<ChangeChecks, String>,
    },
    Started {
        change_set: ChangeSetId,
        result: Result<String, String>,
    },
}

impl ChecksEvent {
    /// The reply for a job that reached no trusted connection. Input: the job. Output: its
    /// event carrying `trusted_connection_unavailable`.
    pub(in crate::tui) fn unavailable(job: &ChecksJob) -> Self {
        failed(job, "trusted_connection_unavailable".to_owned())
    }

    fn fail(&mut self, code: String) {
        match self {
            Self::Tracked { result, .. } => *result = Err(code),
            Self::Views { problem, .. } => *problem = Some(code),
            Self::Settings { result, .. } => *result = Err(code),
            Self::Saved { result, .. } => *result = Err(code),
            Self::Profiles { result, .. } => *result = Err(code),
            Self::Shown { result, .. } => *result = Err(code),
            Self::Profile { result, .. } => *result = Err(code),
            Self::Runs { result, .. } => *result = Err(code),
            Self::Started { result, .. } => *result = Err(code),
        }
    }
}

/// The event a job answers with when it fails before reaching the engine. Inputs: the job
/// and the bounded code. Output: the event carrying the code.
fn failed(job: &ChecksJob, code: String) -> ChecksEvent {
    let mut event = match job {
        ChecksJob::Tracked { project } => ChecksEvent::Tracked {
            project: project.clone(),
            result: Ok(None),
        },
        ChecksJob::Views { project, .. } => ChecksEvent::Views {
            project: project.clone(),
            problem: None,
        },
        ChecksJob::Settings { project } => ChecksEvent::Settings {
            project: project.clone(),
            result: Err(String::new()),
        },
        ChecksJob::SaveSettings { project, .. } => ChecksEvent::Saved {
            project: project.clone(),
            result: Err(String::new()),
        },
        ChecksJob::Profiles { project } => ChecksEvent::Profiles {
            project: project.clone(),
            result: Err(String::new()),
        },
        ChecksJob::Show { project, profile } => ChecksEvent::Shown {
            project: project.clone(),
            profile: profile.clone(),
            result: Err(String::new()),
        },
        ChecksJob::Approve { project, profile } | ChecksJob::Revoke { project, profile } => {
            ChecksEvent::Profile {
                project: project.clone(),
                profile: profile.clone(),
                approve: matches!(job, ChecksJob::Approve { .. }),
                result: Err(String::new()),
            }
        }
        ChecksJob::Runs { change_set, .. } => ChecksEvent::Runs {
            change_set: change_set.clone(),
            result: Err(String::new()),
        },
        ChecksJob::Run { change_set, .. } => ChecksEvent::Started {
            change_set: change_set.clone(),
            result: Err(String::new()),
        },
    };
    event.fail(code);
    event
}

/// A stored profile the owner may review before approving the id asked for: one whose id is
/// that id and whose body hashes to it, so the body shown is the one an approval admits.
/// Inputs: the stored profile and the id asked for. Output: the profile, or
/// `checks_outcome_unknown` for any other profile.
fn matching(stored: StoredProfile, asked: &ProfileId) -> Result<Box<StoredProfile>, String> {
    let hashed = stored.profile.identity().ok();
    if stored.id != *asked || hashed.as_ref() != Some(asked) {
        return Err(UNKNOWN.to_owned());
    }
    Ok(Box::new(stored))
}

/// Names a checks job for logs without its contents. Input: the job. Output: the name.
pub(in crate::tui) fn job_name(job: &ChecksJob) -> &'static str {
    match job {
        ChecksJob::Tracked { .. } => "ChecksTracked",
        ChecksJob::Views { .. } => "ChecksViews",
        ChecksJob::Settings { .. } => "ChecksSettings",
        ChecksJob::SaveSettings { .. } => "ChecksSaveSettings",
        ChecksJob::Profiles { .. } => "ChecksProfiles",
        ChecksJob::Show { .. } => "ChecksShow",
        ChecksJob::Approve { .. } => "ChecksApprove",
        ChecksJob::Revoke { .. } => "ChecksRevoke",
        ChecksJob::Runs { .. } => "ChecksRuns",
        ChecksJob::Run { .. } => "ChecksRun",
    }
}

/// Runs the steps of one Views job on the views, in order, through the given client. A
/// poisoned lock means a step panicked earlier; its views are used as they stand. Inputs:
/// the views, the steps and the client.
pub(in crate::tui) fn run(views: &Mutex<TestViews>, steps: &[Step], client: &mut dyn Client) {
    assert!(steps.len() <= STEPS_PER_JOB_MAX);
    let mut views = views.lock().unwrap_or_else(PoisonError::into_inner);
    for step in steps {
        views.step(step, client);
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

/// The views' client on the owner worker: each call runs `tracker_op` as the owner on the
/// shared owner client and waits for it on this blocking thread. The first transport
/// failure is kept so the worker can tell the shell the connection dropped; once it is a
/// lost connection, the job's later calls are refused with its code instead of each
/// waiting out the timeout.
struct OwnerTests {
    client: Arc<OwnerClient>,
    handle: tokio::runtime::Handle,
    failure: Option<EngineError>,
}

impl Client for OwnerTests {
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
    /// Runs one checks job through the trusted owner controller. Inputs: the job and the
    /// lost-connection flag. Output: the event; a transport failure sets the flag so the
    /// shell reconnects.
    pub(super) async fn checks(&mut self, job: ChecksJob, lost: &mut bool) -> ChecksEvent {
        let client = match self
            .settings_controller()
            .and_then(|controller| controller.shared_client().map_err(|error| error.0))
        {
            Ok(client) => client,
            Err(code) => return failed(&job, bounded_problem(&code)),
        };
        match job {
            ChecksJob::Tracked { project } => ChecksEvent::Tracked {
                result: client
                    .tracker_projects()
                    .await
                    .map(|projects| {
                        projects
                            .into_iter()
                            .find(|tracked| tracked.project == project)
                            .map(|tracked| tracked.key)
                    })
                    .map_err(|error| problem(&error, lost)),
                project,
            },
            ChecksJob::Views {
                project,
                views,
                steps,
            } => {
                let handle = tokio::runtime::Handle::current();
                let ran = tokio::task::spawn_blocking(move || {
                    let mut tests = OwnerTests {
                        client,
                        handle,
                        failure: None,
                    };
                    run(&views, &steps, &mut tests);
                    tests.failure
                })
                .await;
                let problem = match ran {
                    Ok(failure) => failure.map(|error| problem(&error, lost)),
                    Err(_) => Some("tracker_views_failed".to_owned()),
                };
                ChecksEvent::Views { project, problem }
            }
            ChecksJob::Settings { project } => {
                let request = ChecksRequest::SettingsShow {
                    project: project.clone(),
                };
                ChecksEvent::Settings {
                    project,
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Settings { settings }) => Ok(settings),
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                }
            }
            ChecksJob::SaveSettings {
                project,
                values,
                version,
            } => {
                let request = ChecksRequest::SettingsSave {
                    project: project.clone(),
                    values,
                    version,
                };
                ChecksEvent::Saved {
                    project,
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Saved { saved }) => Ok(saved),
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                }
            }
            ChecksJob::Profiles { project } => ChecksEvent::Profiles {
                result: profiles(&client, &project, lost).await,
                project,
            },
            ChecksJob::Show { project, profile } => {
                let request = ChecksRequest::ProfileShow {
                    project: project.clone(),
                    id: profile.clone(),
                };
                ChecksEvent::Shown {
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Profile { profile: stored }) => matching(stored, &profile),
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                    project,
                    profile,
                }
            }
            ChecksJob::Approve { project, profile } => {
                let request = ChecksRequest::ProfileApprove {
                    project: project.clone(),
                    id: profile.clone(),
                };
                ChecksEvent::Profile {
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Approved { profile: admitted })
                            if admitted.id == profile =>
                        {
                            Ok(())
                        }
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                    project,
                    profile,
                    approve: true,
                }
            }
            ChecksJob::Revoke { project, profile } => {
                let request = ChecksRequest::ProfileRevoke {
                    project: project.clone(),
                    id: profile.clone(),
                };
                ChecksEvent::Profile {
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Revoked {}) => Ok(()),
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                    project,
                    profile,
                    approve: false,
                }
            }
            ChecksJob::Runs {
                project,
                change_set,
            } => {
                let request = ChecksRequest::Runs {
                    project: project.clone(),
                    change_set: change_set.clone(),
                };
                let (jobs, latest) = match ask(&client, request, lost).await {
                    Ok(ChecksReply::Runs { jobs, latest }) => (jobs, latest),
                    Ok(_) => {
                        return failed(
                            &ChecksJob::Runs {
                                project,
                                change_set,
                            },
                            UNKNOWN.to_owned(),
                        );
                    }
                    Err(code) => {
                        return failed(
                            &ChecksJob::Runs {
                                project,
                                change_set,
                            },
                            code,
                        );
                    }
                };
                let request = ChecksRequest::Gate {
                    project,
                    change_set: change_set.clone(),
                };
                let gate = match ask(&client, request, lost).await {
                    Ok(ChecksReply::Gate { gate }) => Ok(gate),
                    Ok(_) => Err(UNKNOWN.to_owned()),
                    Err(code) => Err(code),
                };
                ChecksEvent::Runs {
                    change_set,
                    result: Ok(ChangeChecks {
                        runs: latest.into_iter().take(RUNS_LATEST_MAX).collect(),
                        pending: jobs
                            .iter()
                            .filter(|job| matches!(job.state, JobState::Queued | JobState::Running))
                            .count(),
                        gate,
                    }),
                }
            }
            ChecksJob::Run {
                project,
                change_set,
            } => {
                let request = ChecksRequest::Run {
                    project,
                    change_set: change_set.clone(),
                    checks: Vec::new(),
                };
                ChecksEvent::Started {
                    change_set,
                    result: match ask(&client, request, lost).await {
                        Ok(ChecksReply::Queued { batch }) => Ok(batch),
                        Ok(_) => Err(UNKNOWN.to_owned()),
                        Err(code) => Err(code),
                    },
                }
            }
        }
    }
}

/// Sends one `checks` request as the owner. Inputs: the shared client, the request and the
/// lost-connection flag. Output: the reply, or the bounded code of the refusal or transport
/// problem; a dropped connection sets the flag.
async fn ask(
    client: &OwnerClient,
    request: ChecksRequest,
    lost: &mut bool,
) -> Result<ChecksReply, String> {
    client
        .checks(request)
        .await
        .map_err(|error| problem(&error, lost))
}

/// Lists a project's stored profiles newest first, following `next` for at most
/// `PROFILE_PAGES_MAX` pages; each later page must add rows and move the cursor. Inputs: the
/// shared client, the project and the lost-connection flag. Output: the rows, or the code.
async fn profiles(
    client: &OwnerClient,
    project: &ProjectId,
    lost: &mut bool,
) -> Result<Vec<ProfileSummary>, String> {
    let mut rows: Vec<ProfileSummary> = Vec::new();
    let mut after = None;
    for _ in 0..PROFILE_PAGES_MAX {
        let request = ChecksRequest::Profiles {
            project: project.clone(),
            after,
        };
        let page = match ask(client, request, lost).await? {
            ChecksReply::Profiles { page } => page,
            _ => return Err(UNKNOWN.to_owned()),
        };
        if after.is_some() && (page.profiles.is_empty() || page.next == after) {
            return Err(UNKNOWN.to_owned());
        }
        rows.extend(page.profiles);
        if page.next.is_none() {
            return Ok(rows);
        }
        after = page.next;
    }
    Ok(rows)
}

#[cfg(all(test, unix))]
pub(in crate::tui) mod tests;
