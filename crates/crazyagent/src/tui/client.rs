use super::{dispatch, master_session, model_access, native_auth};
use crate::client_controller::OwnerController;
use crate::conversation::{ConversationCursor, ConversationMutation};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerPageCursor, OwnerRead, OwnerReadReply,
    OwnerResponse, OwnerResult, OwnerSummary, OwnerView,
};
use crate::engine::{Client, ClientError};
use crate::protocol::{
    BaselineId, ClientId, Command, ConfigId, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec,
    RequestId, WireReply, WireRequest,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

const MASTER_READ_ATTEMPTS: u32 = 4;

mod account_profile;
pub(super) mod accounts;
pub(super) mod app_setup;
pub(super) mod changes;
mod claude_code;
pub(super) mod cleanup;
pub(super) mod intake;
pub(super) mod process;
mod project_setup;
pub(super) mod settings;
mod shell;
pub(super) mod terminal;
pub(super) use project_setup::{SetupEvent, SetupQuery, SetupView};

enum WorkerMode {
    #[cfg(all(test, unix))]
    Ordinary(Box<Client>),
    Paired {
        socket: PathBuf,
        timeout: Duration,
    },
}

enum Job {
    Creation(Box<CreationJob>),
    Wire(Box<WireRequest>),
    Reconnect,
    Conversation(Box<ConversationJob>),
    Master(Box<master_session::Job>),
    ModelAccess(Box<model_access::Job>),
    Dispatch(Box<dispatch::Query>),
    Setup(Box<SetupQuery>),
    Shell(Box<crate::tui::seam::ShellRequest>),
    Settings(Box<settings::SettingsJob>),
    TeamGraph(Box<ProjectId>),
    ProjectFiles(Box<(ProjectId, String)>),
    Intake(Box<intake::IntakeJob>),
    Changes(Box<changes::ChangesQuery>),
    Process(Box<process::ProcessQuery>),
    Cleanup(Box<cleanup::CleanupQuery>),
    Evidence {
        project: ProjectId,
        generation: u64,
        first: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConversationContext {
    pub identity: OwnerConnectionIdentity,
    pub project: ProjectId,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConversationSubmission {
    pub identity: OwnerConnectionIdentity,
    pub generation: u64,
    pub mutation: ConversationMutation,
}

#[derive(Clone)]
pub(super) enum ConversationQuery {
    History { cursor: Option<ConversationCursor> },
    Brief,
}

#[derive(Clone, Copy)]
pub(super) enum MutationDelivery {
    Submit,
    Reconcile,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CreationSubmission {
    pub identity: OwnerConnectionIdentity,
    pub generation: u64,
    pub navigation: u64,
    pub project_id: ProjectId,
    pub spec: ProjectSpec,
    pub setup: Option<crate::project_setup::ProjectSetup>,
}

#[derive(Clone)]
pub(super) struct CreationJob {
    pub submission: CreationSubmission,
    pub delivery: MutationDelivery,
}

pub(super) struct CreationEvent {
    pub submission: CreationSubmission,
    pub operation: Option<(RequestId, String)>,
    pub result: Result<Box<OwnerResponse>, String>,
    pub retained: bool,
}

#[derive(Clone)]
pub(super) enum ConversationJob {
    Read {
        context: ConversationContext,
        query: ConversationQuery,
    },
    Mutation {
        submission: ConversationSubmission,
        delivery: MutationDelivery,
    },
}

pub(super) enum ConversationEvent {
    Read {
        context: ConversationContext,
        result: Result<Box<OwnerReadReply>, String>,
    },
    Mutation {
        submission: ConversationSubmission,
        operation: Option<(RequestId, String)>,
        result: Result<Box<OwnerResponse>, String>,
        retained: bool,
    },
}

pub(super) struct Evidence {
    pub project: ProjectId,
    pub baseline_id: BaselineId,
    pub requirements: String,
    pub approved: bool,
    pub revoked: bool,
    pub current: bool,
    pub revision: Cursor,
    pub config_id: ConfigId,
}

pub(super) enum OwnerProgress {
    Continue,
    Empty { revision: Cursor },
    Incomplete { revision: Cursor },
    Refresh,
    Ready(Evidence),
}

pub(super) enum OwnerEvent {
    Master(Box<master_session::Event>),
    Creation(Box<CreationEvent>),
    ModelAccess(Box<model_access::Event>),
    Dispatch(Box<dispatch::Event>),
    Setup(Box<SetupEvent>),
    Shell(Box<crate::tui::seam::ShellEvent>),
    Settings(Box<settings::SettingsEvent>),
    TeamGraph {
        project: ProjectId,
        result: Result<Box<crate::team::TeamGraph>, String>,
    },
    ProjectFiles {
        project: ProjectId,
        query: String,
        result: Result<Vec<String>, String>,
    },
    Intake(Box<intake::IntakeEvent>),
    Changes(Box<changes::ChangesEvent>),
    Process(Box<process::ProcessEvent>),
    Cleanup(Box<cleanup::CleanupEvent>),
    Access {
        ready: bool,
        problem: String,
        identity: Option<OwnerConnectionIdentity>,
    },
    Conversation(Box<ConversationEvent>),
    Read {
        project: ProjectId,
        generation: u64,
        result: Result<OwnerProgress, String>,
    },
    Settled(Result<(), String>),
}

struct OwnerReply {
    finished: bool,
    event: OwnerEvent,
}

struct Pending {
    request: WireRequest,
    failed: bool,
}

pub(super) struct Connection {
    sender: Option<SyncSender<Job>>,
    receiver: Option<Receiver<Result<WireReply, ClientError>>>,
    owner_receiver: Option<Receiver<OwnerReply>>,
    owner_busy: bool,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pending: Option<Pending>,
    client_id: ClientId,
    sequence: u64,
}

impl Connection {
    #[cfg(all(test, unix))]
    pub fn connect(client: Client) -> Result<Self, String> {
        Self::spawn(WorkerMode::Ordinary(Box::new(client)))
    }

    pub fn connect_paired(client: Client) -> Result<Self, String> {
        let (socket, timeout) = client
            .into_unbound_configuration()
            .map_err(|e| e.to_string())?;
        Self::spawn(WorkerMode::Paired { socket, timeout })
    }

    fn spawn(mode: WorkerMode) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let (sender, requests) = sync_channel::<Job>(1);
        let (replies, receiver) = sync_channel(1);
        let (owner_replies, owner_receiver) = sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let client_id = ClientId::new(format!(
            "tui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ))
        .map_err(|e| e.to_string())?;
        let worker = std::thread::Builder::new()
            .name("tui-client".into())
            .spawn(move || {
                let (mut client, mut native) = match mode {
                    #[cfg(all(test, unix))]
                    WorkerMode::Ordinary(client) => (*client, None),
                    WorkerMode::Paired { socket, timeout } => {
                        let mut native = NativeOwner::new(socket, timeout);
                        let (client, event) = runtime.block_on(native.attach());
                        if owner_replies
                            .send(OwnerReply {
                                finished: false,
                                event,
                            })
                            .is_err()
                        {
                            return;
                        }
                        (client, Some(native))
                    }
                };
                while let Ok(job) = requests.recv() {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(native) = native.as_mut() {
                        let was_ready = native.ready;
                        if let Some(outcome) = runtime.block_on(native.settle_stray())
                            && owner_replies
                                .send(OwnerReply {
                                    finished: false,
                                    event: OwnerEvent::Settled(outcome),
                                })
                                .is_err()
                        {
                            break;
                        }
                        if !report_loss(was_ready, native, None, &mut client, &owner_replies) {
                            break;
                        }
                    }
                    match job {
                        Job::Creation(job) => {
                            let native = native
                                .as_mut()
                                .expect("creation requires paired configuration");
                            let was_ready = native.ready;
                            let event = runtime.block_on(native.creation(*job));
                            let problem = event.result.as_ref().err().map(String::as_str);
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Creation(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Wire(request) => {
                            let result = runtime.block_on(client.request(*request));
                            if result.is_err()
                                && let Some(native) = native.as_mut().filter(|native| native.ready)
                            {
                                native.detach();
                                client = native.ordinary();
                                let event = OwnerEvent::Access {
                                    ready: false,
                                    problem: "trusted_connection_lost".into(),
                                    identity: None,
                                };
                                if owner_replies
                                    .send(OwnerReply {
                                        finished: false,
                                        event,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            if replies.send(result).is_err() {
                                break;
                            }
                        }
                        Job::Reconnect => {
                            let native = native
                                .as_mut()
                                .expect("paired reconnect requires native configuration");
                            let (ordinary, event) = runtime.block_on(native.attach());
                            client = ordinary;
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::ModelAccess(job) => {
                            let native = native
                                .as_mut()
                                .expect("metadata requires paired configuration");
                            let was_ready = native.ready;
                            let event = runtime.block_on(native.model_access(*job));
                            let problem = match &event {
                                model_access::Event::Read {
                                    result: Err(problem),
                                    ..
                                }
                                | model_access::Event::Mutation {
                                    result: Err(problem),
                                    ..
                                }
                                | model_access::Event::AccountProfile {
                                    result: Err(problem),
                                    ..
                                }
                                | model_access::Event::AccountProfileCapabilities {
                                    result: Err(problem),
                                    ..
                                } => Some(problem.as_str()),
                                model_access::Event::NativeAuth(event) => {
                                    event.result.as_ref().err().map(String::as_str)
                                }
                                _ => None,
                            };
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::ModelAccess(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Conversation(job) => {
                            let native = native
                                .as_mut()
                                .expect("conversation requires native configuration");
                            let was_ready = native.ready;
                            let event = runtime.block_on(native.conversation(*job));
                            let problem = match &event {
                                ConversationEvent::Read { result, .. } => result.as_ref().err(),
                                ConversationEvent::Mutation { result, .. } => result.as_ref().err(),
                            }
                            .map(String::as_str);
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Conversation(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Master(job) => {
                            let native = native
                                .as_mut()
                                .expect("master requires paired configuration");
                            let was_ready = native.ready;
                            let event = runtime.block_on(native.master(*job));
                            let problem = match &event {
                                master_session::Event::Catalog {
                                    result: Err(problem),
                                    ..
                                }
                                | master_session::Event::Read {
                                    result: Err(problem),
                                    ..
                                }
                                | master_session::Event::Mutation {
                                    result: Err(problem),
                                    ..
                                } => Some(problem.as_str()),
                                _ => None,
                            };
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Master(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Dispatch(query) => {
                            let native = native
                                .as_mut()
                                .expect("dispatch read requires paired configuration");
                            let was_ready = native.ready;
                            let event = runtime.block_on(native.dispatch(*query));
                            let problem = event.result.as_ref().err().map(String::as_str);
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Dispatch(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Setup(query) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.project_setup(*query));
                                    let problem = event.result.as_ref().err().map(String::as_str);
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => SetupEvent {
                                    query: *query,
                                    result: Err("trusted_connection_unavailable".into()),
                                },
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Setup(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Shell(request) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.shell(*request));
                                    let problem = None;
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => crate::tui::seam::ShellEvent::unavailable(&request),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Shell(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Settings(job) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.settings(*job));
                                    let problem = None;
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => settings::SettingsEvent::unavailable(&job),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Settings(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::TeamGraph(project) => {
                            let result = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let result = runtime.block_on(native.team_graph(&project));
                                    let problem = result.as_ref().err().map(String::as_str);
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    result
                                }
                                None => Err("trusted_connection_unavailable".into()),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::TeamGraph {
                                        project: *project,
                                        result,
                                    },
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::ProjectFiles(job) => {
                            let (project, query) = *job;
                            let result = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let result =
                                        runtime.block_on(native.project_files(&project, &query));
                                    let problem = result.as_ref().err().map(String::as_str);
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    result
                                }
                                None => Err("trusted_connection_unavailable".into()),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::ProjectFiles {
                                        project,
                                        query,
                                        result,
                                    },
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Intake(job) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.intake(*job));
                                    let problem = None;
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        problem,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => intake::IntakeEvent::unavailable(&job),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Intake(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Changes(query) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.changes(*query));
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        None,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => changes::ChangesEvent::unavailable(*query),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Changes(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Process(query) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.process(*query));
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        None,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => process::ProcessEvent::unavailable(*query),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Process(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Cleanup(query) => {
                            let event = match native.as_mut() {
                                Some(native) => {
                                    let was_ready = native.ready;
                                    let event = runtime.block_on(native.cleanup(*query));
                                    if !report_loss(
                                        was_ready,
                                        native,
                                        None,
                                        &mut client,
                                        &owner_replies,
                                    ) {
                                        break;
                                    }
                                    event
                                }
                                None => cleanup::CleanupEvent::unavailable(*query),
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event: OwnerEvent::Cleanup(Box::new(event)),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Job::Evidence {
                            project,
                            generation,
                            first,
                        } => {
                            let native = native
                                .as_mut()
                                .expect("owner read requires native configuration");
                            let was_ready = native.ready;
                            let result =
                                runtime.block_on(native.step(project.clone(), generation, first));
                            let problem = result.as_ref().err().map(String::as_str);
                            if !report_loss(was_ready, native, problem, &mut client, &owner_replies)
                            {
                                break;
                            }
                            let event = OwnerEvent::Read {
                                project,
                                generation,
                                result,
                            };
                            if owner_replies
                                .send(OwnerReply {
                                    finished: true,
                                    event,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            owner_receiver: Some(owner_receiver),
            owner_busy: false,
            stopped,
            worker: Some(worker),
            pending: None,
            client_id,
            sequence: 0,
        })
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.owner_busy
    }

    pub fn uncertain(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.failed)
    }

    pub fn mutation_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.request.command, Command::CreateProject { .. }))
    }

    pub fn start(&mut self, command: Command, revision: Cursor) -> Result<RequestId, String> {
        if self.busy() {
            return Err("A request is pending; Ctrl-R retries an uncertain project creation with its original identity".into());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Client request counter exhausted")?;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: self.client_id.clone(),
            request_id: RequestId::new(format!("request-{}", self.sequence))
                .map_err(|e| e.to_string())?,
            expected_revision: revision,
            command,
        };
        request.validate().map_err(|e| e.to_string())?;
        self.sender
            .as_ref()
            .ok_or("Client stopped")?
            .try_send(Job::Wire(Box::new(request.clone())))
            .map_err(|e| e.to_string())?;
        let request_id = request.request_id.clone();
        self.pending = Some(Pending {
            request,
            failed: false,
        });
        Ok(request_id)
    }

    pub fn poll(&mut self) -> Result<Option<Result<WireReply, ClientError>>, String> {
        let result = match self.receiver.as_ref().ok_or("Client stopped")?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => return Err("Client worker stopped".into()),
        };
        let pending = self
            .pending
            .as_mut()
            .expect("client reply requires one pending request");
        assert!(!pending.failed);
        if result.is_err() && matches!(pending.request.command, Command::CreateProject { .. }) {
            pending.failed = true;
        } else {
            self.pending = None;
        }
        Ok(Some(result))
    }

    pub fn retry(&mut self) -> Result<(), String> {
        let pending = self
            .pending
            .as_mut()
            .ok_or("No uncertain project creation to retry")?;
        if !pending.failed {
            return Err("Request is still running".into());
        }
        self.sender
            .as_ref()
            .ok_or("Client stopped")?
            .try_send(Job::Wire(Box::new(pending.request.clone())))
            .map_err(|e| e.to_string())?;
        pending.failed = false;
        Ok(())
    }

    pub fn poll_owner(&mut self) -> Result<Option<OwnerEvent>, String> {
        let reply = match self
            .owner_receiver
            .as_ref()
            .ok_or("Client stopped")?
            .try_recv()
        {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => return Err("Client worker stopped".into()),
        };
        if reply.finished {
            assert!(self.owner_busy);
            self.owner_busy = false;
        }
        Ok(Some(reply.event))
    }

    pub fn read_evidence(
        &mut self,
        project: ProjectId,
        generation: u64,
        first: bool,
    ) -> Result<(), String> {
        self.owner_job(Job::Evidence {
            project,
            generation,
            first,
        })
    }

    pub fn reconnect(&mut self) -> Result<(), String> {
        self.owner_job(Job::Reconnect)
    }

    pub(super) fn conversation(&mut self, job: ConversationJob) -> Result<(), String> {
        self.owner_job(Job::Conversation(Box::new(job)))
    }

    pub(super) fn master(&mut self, job: master_session::Job) -> Result<(), String> {
        self.owner_job(Job::Master(Box::new(job)))
    }

    pub(super) fn creation(&mut self, job: CreationJob) -> Result<(), String> {
        self.owner_job(Job::Creation(Box::new(job)))
    }

    pub(super) fn model_access(&mut self, job: model_access::Job) -> Result<(), String> {
        self.owner_job(Job::ModelAccess(Box::new(job)))
    }

    pub(super) fn dispatch(&mut self, query: dispatch::Query) -> Result<(), String> {
        self.owner_job(Job::Dispatch(Box::new(query)))
    }

    /// Reads one project's team graph for the Team screen.
    /// Input: the project. Output: Ok once queued; the graph comes back as
    /// `OwnerEvent::TeamGraph`.
    pub(super) fn team_graph(&mut self, project: ProjectId) -> Result<(), String> {
        self.owner_job(Job::TeamGraph(Box::new(project)))
    }

    /// Lists one project's files for the console's `@` completion: at most
    /// `project_files::FILES_MAX` paths that contain the query.
    /// Input: the project and the query. Output: Ok once queued; the paths come back as
    /// `OwnerEvent::ProjectFiles`.
    pub(super) fn project_files(
        &mut self,
        project: ProjectId,
        query: String,
    ) -> Result<(), String> {
        self.owner_job(Job::ProjectFiles(Box::new((project, query))))
    }

    fn owner_job(&mut self, job: Job) -> Result<(), String> {
        if self.busy() {
            return Err("A native request is still pending".into());
        }
        self.sender
            .as_ref()
            .ok_or("Client stopped")?
            .try_send(job)
            .map_err(|e| e.to_string())?;
        self.owner_busy = true;
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.sender.take();
        self.receiver.take();
        self.owner_receiver.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Scan {
    project: ProjectId,
    generation: u64,
    cursor: Option<OwnerPageCursor>,
    revision: Option<Cursor>,
    config_id: Option<ConfigId>,
    last_id: Option<String>,
    pages: u8,
    selected: Option<BaselineId>,
}

struct NativeOwner {
    master_pending: Option<master_session::Submission>,
    master_operation: Option<(RequestId, String)>,
    creation_pending: Option<CreationSubmission>,
    creation_operation: Option<(RequestId, String)>,
    creation_retry_allowed: bool,
    socket: PathBuf,
    timeout: Duration,
    controller: Option<OwnerController>,
    ready: bool,
    scan: Option<Scan>,
    conversation_pending: Option<ConversationSubmission>,
    conversation_operation: Option<(RequestId, String)>,
    model_pending: Option<model_access::Submission>,
    model_operation: Option<(RequestId, String)>,
}

impl NativeOwner {
    fn new(socket: PathBuf, timeout: Duration) -> Self {
        Self {
            master_pending: None,
            master_operation: None,
            creation_pending: None,
            creation_operation: None,
            creation_retry_allowed: false,
            socket,
            timeout,
            controller: None,
            ready: false,
            scan: None,
            conversation_pending: None,
            conversation_operation: None,
            model_pending: None,
            model_operation: None,
        }
    }

    fn ordinary(&self) -> Client {
        Client::new(self.socket.clone(), self.timeout)
            .expect("retained client configuration was validated")
    }

    fn detach(&mut self) {
        self.ready = false;
        self.scan = None;
        if let Some(controller) = &mut self.controller {
            controller.detach();
        }
    }

    async fn attach(&mut self) -> (Client, OwnerEvent) {
        self.scan = None;
        let ordinary = self.ordinary();
        let result = if let Some(controller) = &mut self.controller {
            controller.reattach_configured(ordinary).await
        } else {
            OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
                .await
                .map(|(ordinary, controller)| {
                    self.controller = Some(controller);
                    ordinary
                })
        };
        match result {
            Ok(ordinary) => {
                self.ready = true;
                (
                    ordinary,
                    OwnerEvent::Access {
                        ready: true,
                        problem: String::new(),
                        identity: self.controller.as_ref().map(|c| c.identity().clone()),
                    },
                )
            }
            Err(error) => {
                self.detach();
                (
                    self.ordinary(),
                    OwnerEvent::Access {
                        ready: false,
                        problem: bounded_problem(&error.0),
                        identity: None,
                    },
                )
            }
        }
    }

    /// Settles the one change a screen left unconfirmed, such as a save that outlasted the
    /// reply deadline, so it stops blocking every later change: a change never sent is
    /// dropped, a recorded one is confirmed, and one the engine never recorded is sent once
    /// more with its original identity and revision.
    /// Takes the owner; creation, master, conversation and model access settle their own.
    /// Returns the outcome to tell the owner, or none while nothing was settled.
    async fn settle_stray(&mut self) -> Option<Result<(), String>> {
        if !self.ready
            || self.master_pending.is_some()
            || self.creation_pending.is_some()
            || self.conversation_pending.is_some()
            || self.model_pending.is_some()
        {
            return None;
        }
        let controller = self.controller.as_mut()?;
        if !controller.pending()?.uncertain() {
            controller.discard_refused().ok()?;
            return None;
        }
        let response = match controller.reconcile_pending().await {
            Ok(OwnerResponse::NotRecordedAtBarrier { .. }) => controller.retry_pending().await,
            other => other,
        };
        match response {
            Ok(OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }) => {
                Some(match operation.result {
                    OwnerResult::Applied => Ok(()),
                    OwnerResult::Rejected { code } => Err(bounded_problem(&code)),
                    _ => Err("change_outcome_unknown".into()),
                })
            }
            Ok(_) => None,
            Err(error) => {
                if job_lost(&error.0) {
                    self.detach();
                }
                None
            }
        }
    }

    async fn dispatch(&mut self, query: dispatch::Query) -> dispatch::Event {
        let result = self.read_dispatch(&query).await;
        dispatch::Event { query, result }
    }

    async fn master(&mut self, job: master_session::Job) -> master_session::Event {
        match job {
            master_session::Job::Catalog { context } => {
                let result = match self.master_controller(&context.identity) {
                    Ok(controller) => tokio::time::timeout(Duration::from_secs(4), async {
                        let mut attempts = 0;
                        loop {
                            attempts += 1;
                            match master_session::read_catalog(&mut *controller).await {
                                Err(problem)
                                    if attempts < MASTER_READ_ATTEMPTS
                                        && problem.starts_with("master_snapshot_changed") => {}
                                result => break result,
                            }
                        }
                    })
                    .await
                    .unwrap_or_else(|_| Err("master_catalog_deadline; inspect Settings".into()))
                    .map(Box::new),
                    Err(problem) => Err(problem),
                };
                master_session::Event::Catalog { context, result }
            }
            master_session::Job::Read {
                context,
                confirmation,
            } => {
                let result = match self.master_controller(&context.identity) {
                    Ok(controller) => tokio::time::timeout(Duration::from_secs(4), async {
                        let mut attempts = 0;
                        loop {
                            attempts += 1;
                            match master_session::read_view(
                                &mut *controller,
                                &context,
                                confirmation.as_deref(),
                            )
                            .await
                            {
                                Err(problem)
                                    if attempts < MASTER_READ_ATTEMPTS
                                        && problem.starts_with("master_snapshot_changed") => {}
                                result => break result,
                            }
                        }
                    })
                    .await
                    .unwrap_or_else(|_| Err("master_read_deadline".into()))
                    .map(Box::new),
                    Err(problem) => Err(problem),
                };
                master_session::Event::Read { context, result }
            }
            master_session::Job::Mutation {
                submission,
                delivery,
            } => {
                let result = self
                    .mutate_master(&submission, delivery)
                    .await
                    .map(Box::new);
                let retained = self.master_pending.is_some()
                    && self
                        .controller
                        .as_ref()
                        .is_some_and(|controller| controller.pending().is_some());
                let operation = self.master_operation.clone();
                assert!(!retained || operation.is_some());
                if !retained {
                    self.master_pending = None;
                    self.master_operation = None;
                }
                master_session::Event::Mutation {
                    submission,
                    operation,
                    result,
                    retained,
                }
            }
        }
    }

    fn master_controller(
        &mut self,
        identity: &OwnerConnectionIdentity,
    ) -> Result<&mut OwnerController, String> {
        if !self.ready {
            return Err("master_owner_unavailable".into());
        }
        let controller = self.controller.as_mut().ok_or("master_owner_unavailable")?;
        if controller.identity() != identity {
            return Err("master_owner_identity_changed".into());
        }
        Ok(controller)
    }

    async fn mutate_master(
        &mut self,
        submission: &master_session::Submission,
        delivery: MutationDelivery,
    ) -> Result<OwnerResponse, String> {
        if !self.ready {
            return Err("master_owner_unavailable".into());
        }
        let controller = self.controller.as_mut().ok_or("master_owner_unavailable")?;
        match delivery {
            MutationDelivery::Submit => {
                if controller.identity() != &submission.identity
                    || controller.pending().is_some()
                    || self.creation_pending.is_some()
                    || self.conversation_pending.is_some()
                    || self.model_pending.is_some()
                    || self.master_pending.is_some()
                {
                    return Err("master_pending_or_owner_changed".into());
                }
                let project = master_session::action_project(&submission.action);
                let state = controller
                    .read(OwnerRead::ProjectMaster {
                        project_id: project.clone(),
                    })
                    .await
                    .map_err(|error| bounded_problem(&error.0))?;
                if !matches!(&state.data, OwnerData::ProjectMaster { .. }) {
                    return Err("master_fresh_read_required".into());
                }
                if let crate::master_session::MasterOwnerAction::Open {
                    expected_config_id, ..
                } = &submission.action
                    && &state.config_id != expected_config_id
                {
                    return Err("master_stale_catalog; inspect Settings".into());
                }
                if let crate::master_session::MasterOwnerAction::RegisterRcliProfile {
                    registration,
                } = &submission.action
                {
                    Self::prepare_rcli_registration(controller, &state, registration).await?;
                }
                let pending = controller
                    .begin_mutation(
                        OwnerMutation::ProjectMaster {
                            action: submission.action.clone(),
                        },
                        submission.generation,
                    )
                    .map_err(|error| bounded_problem(&error.0))?;
                self.master_operation = Some((
                    pending.request().request_id.clone(),
                    pending.hash().to_owned(),
                ));
                self.master_pending = Some(submission.clone());
                controller
                    .send_pending()
                    .await
                    .map_err(|error| bounded_problem(&error.0))
            }
            MutationDelivery::Reconcile | MutationDelivery::Retry => {
                if self.master_pending.as_ref() != Some(submission)
                    || controller.pending().is_none()
                {
                    return Err("master_original_pending_context_required".into());
                }
                match delivery {
                    MutationDelivery::Reconcile => controller.reconcile_pending().await,
                    MutationDelivery::Retry => controller.retry_pending().await,
                    MutationDelivery::Submit => unreachable!(),
                }
                .map_err(|error| bounded_problem(&error.0))
            }
        }
    }

    async fn prepare_rcli_registration(
        controller: &mut OwnerController,
        state: &OwnerReadReply,
        intent: &crate::master_session::RcliProfileRegistration,
    ) -> Result<(), String> {
        if !matches!(&state.data, OwnerData::ProjectMaster { session: Some(session) }
            if **session == intent.expected_session
                && session.phase == crate::master_session::MasterSessionPhase::Configured
                && session.current_turn.is_none())
            || state.config_id != intent.expected_session.config_id
        {
            return Err("rcli_registration_stale_session; reopen setup and review".into());
        }
        let record = controller
            .read(OwnerRead::ModelAccessRecord {
                collection: crate::engine::owner::ModelAccessCollection::Connections,
                id: intent.expected_connection.id.clone(),
            })
            .await
            .map_err(|error| bounded_problem(&error.0))?;
        if record.revision != state.revision
            || record.config_id != intent.expected_session.config_id
            || !matches!(&record.data, OwnerData::ModelAccessRecord {
                record: crate::engine::owner::ModelAccessRecord::Connection { connection },
                source_hash, writable: true,
            } if connection == &intent.expected_connection
                && source_hash == &intent.expected_session.source_hash)
        {
            return Err("rcli_registration_stale_connection; inspect Settings".into());
        }
        Ok(())
    }

    /// Reads one project's team graph on the trusted owner connection.
    /// Input: the project. Output: the graph, or the engine's plain problem code.
    async fn team_graph(
        &mut self,
        project: &ProjectId,
    ) -> Result<Box<crate::team::TeamGraph>, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        match controller
            .read(OwnerRead::TeamGraph {
                project_id: project.clone(),
            })
            .await
        {
            Ok(reply) => match reply.data {
                OwnerData::TeamGraph { graph } if graph.project_id == *project => Ok(graph),
                _ => Err("team_graph_reply_mismatch".into()),
            },
            Err(error) => Err(bounded_problem(&error.0)),
        }
    }

    /// Lists one project's files on the trusted owner connection.
    /// Input: the project and the query. Output: the paths, or the engine's plain problem code.
    async fn project_files(
        &mut self,
        project: &ProjectId,
        query: &str,
    ) -> Result<Vec<String>, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        controller
            .project_files(
                project.clone(),
                query.to_owned(),
                crate::project_files::FILES_MAX,
            )
            .await
            .map_err(|error| bounded_problem(&error.0))
    }

    async fn read_dispatch(&mut self, query: &dispatch::Query) -> Result<OwnerReadReply, String> {
        if !self.ready {
            return Err("dispatch_owner_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("dispatch_owner_unavailable")?;
        if controller.identity().database_instance() != query.selection.database
            || controller.identity().engine_epoch() != query.selection.epoch
        {
            return Err("dispatch_owner_identity_changed".into());
        }
        let expected = crate::worker_dispatch::dispatch_id(
            controller.identity().database_instance(),
            &query.attempt,
        )
        .map_err(|error| bounded_problem(&error.0))?;
        if expected != query.dispatch_id || query.attempt.plan_id != query.selection.plan_id {
            return Err("dispatch_attempt_identity_changed".into());
        }
        assert_eq!(expected, query.dispatch_id);
        controller
            .read(OwnerRead::WorkerDispatch {
                dispatch_id: query.dispatch_id.clone(),
            })
            .await
            .map_err(|error| bounded_problem(&error.0))
    }

    async fn model_access(&mut self, job: model_access::Job) -> model_access::Event {
        match job {
            model_access::Job::ClaudeCode(job) => {
                model_access::Event::ClaudeCode(Box::new(self.claude_code(*job).await))
            }
            model_access::Job::AccountProfileConfigure { submission, secret } => {
                self.account_profile_configure(submission, secret).await
            }
            model_access::Job::AccountProfileRead { submission } => {
                self.account_profile_read(submission).await
            }
            model_access::Job::AccountProfileCapabilities { submission } => {
                self.account_profile_capabilities(submission).await
            }
            model_access::Job::NativeAuth(job) => {
                let result = self.native_auth(&job).await;
                model_access::Event::NativeAuth(Box::new(native_auth::Event { job: *job, result }))
            }
            model_access::Job::Read {
                identity,
                generation,
                query,
            } => {
                let result = self
                    .read_model_access(&identity, query.clone())
                    .await
                    .map(Box::new);
                model_access::Event::Read {
                    identity,
                    generation,
                    query,
                    result,
                }
            }
            model_access::Job::Mutation {
                submission,
                delivery,
            } => {
                let result = self
                    .mutate_model_access(&submission, delivery)
                    .await
                    .map(Box::new);
                let retained = self.model_pending.is_some()
                    && self
                        .controller
                        .as_ref()
                        .is_some_and(|controller| controller.pending().is_some());
                let operation = self.model_operation.clone();
                assert!(!retained || operation.is_some());
                if !retained {
                    self.model_pending = None;
                    self.model_operation = None;
                }
                model_access::Event::Mutation {
                    submission,
                    operation,
                    result,
                    retained,
                }
            }
        }
    }

    async fn native_auth(&mut self, job: &native_auth::Job) -> Result<native_auth::Output, String> {
        if !self.ready {
            return Err("native_auth_owner_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("native_auth_owner_unavailable")?;
        if controller.identity() != &job.context.basis.identity {
            return Err("native_auth_owner_identity_changed".into());
        }
        match &job.command {
            native_auth::Command::Inspect => controller
                .read(OwnerRead::ModelAccessRecord {
                    collection: crate::engine::owner::ModelAccessCollection::Connections,
                    id: job.context.connection.id.clone(),
                })
                .await
                .map(|reply| native_auth::Output::Inspected(Box::new(reply)))
                .map_err(|_| "native_auth_inspection_unavailable".into()),
            native_auth::Command::Using => controller
                .account_use(job.context.connection.id.clone())
                .await
                .map(native_auth::Output::Using)
                .map_err(|error| native_auth::public_problem(&error.0)),
            native_auth::Command::Release => controller
                .release_account(job.context.connection.id.clone(), job.context.generation)
                .await
                .map(native_auth::Output::Using)
                .map_err(|error| native_auth::public_problem(&error.0)),
            command => {
                let reply = match command {
                    native_auth::Command::Start(request) => {
                        controller.start_native_auth(request.clone()).await
                    }
                    native_auth::Command::Read(request) => {
                        controller.read_native_auth(request.clone()).await
                    }
                    native_auth::Command::Cancel(request) => {
                        controller.cancel_native_auth(request.clone()).await
                    }
                    native_auth::Command::Logout(request) => {
                        controller.logout_native_auth(request.clone()).await
                    }
                    native_auth::Command::Inspect
                    | native_auth::Command::Using
                    | native_auth::Command::Release => unreachable!(),
                };
                reply
                    .map(|reply| native_auth::Output::Auth(Box::new(reply)))
                    .map_err(|error| native_auth::public_problem(&error.0))
            }
        }
    }

    async fn read_model_access(
        &mut self,
        identity: &OwnerConnectionIdentity,
        query: OwnerRead,
    ) -> Result<OwnerReadReply, String> {
        if !self.ready
            || !matches!(
                query,
                OwnerRead::ModelAccessPage { .. } | OwnerRead::ModelAccessRecord { .. }
            )
        {
            return Err("metadata_owner_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("metadata_owner_unavailable")?;
        if controller.identity() != identity {
            return Err("metadata_owner_identity_changed".into());
        }
        controller
            .read(query)
            .await
            .map_err(|error| bounded_problem(&error.0))
    }

    async fn mutate_model_access(
        &mut self,
        submission: &model_access::Submission,
        delivery: MutationDelivery,
    ) -> Result<OwnerResponse, String> {
        if !self.ready {
            return Err("metadata_owner_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("metadata_owner_unavailable")?;
        match delivery {
            MutationDelivery::Submit => {
                if controller.identity() != &submission.basis.identity
                    || controller.pending().is_some()
                    || self.creation_pending.is_some()
                    || self.model_pending.is_some()
                    || self.conversation_pending.is_some()
                    || self.master_pending.is_some()
                {
                    return Err("metadata_pending_or_owner_changed".into());
                }
                let collection = match &submission.edit {
                    crate::config::ModelAccessEdit::AddAccount { .. }
                    | crate::config::ModelAccessEdit::RemoveAccount { .. } => {
                        crate::engine::owner::ModelAccessCollection::Accounts
                    }
                    crate::config::ModelAccessEdit::CreateConnection { .. }
                    | crate::config::ModelAccessEdit::ReplaceConnection { .. }
                    | crate::config::ModelAccessEdit::RemoveConnection { .. } => {
                        crate::engine::owner::ModelAccessCollection::Connections
                    }
                    _ => crate::engine::owner::ModelAccessCollection::Pools,
                };
                let fresh = controller
                    .read(OwnerRead::ModelAccessPage {
                        collection,
                        cursor: None,
                        limit: 1,
                    })
                    .await
                    .map_err(|error| bounded_problem(&error.0))?;
                if fresh.revision != submission.basis.revision
                    || fresh.config_id != submission.basis.config_id
                    || !submission.basis.writable
                    || !matches!(&fresh.data, OwnerData::ModelAccessPage { source_hash, writable: true, .. } if source_hash == &submission.basis.source_hash)
                {
                    return Err("metadata_stale_form; refresh and review retained values".into());
                }
                let pending = controller
                    .begin_mutation(
                        OwnerMutation::ModelAccess {
                            expected_config_id: submission.basis.config_id.clone(),
                            expected_source_hash: submission.basis.source_hash.clone(),
                            edit: submission.edit.clone(),
                        },
                        submission.generation,
                    )
                    .map_err(|error| bounded_problem(&error.0))?;
                self.model_operation = Some((
                    pending.request().request_id.clone(),
                    pending.hash().to_owned(),
                ));
                self.model_pending = Some(submission.clone());
                controller
                    .send_pending()
                    .await
                    .map_err(|error| bounded_problem(&error.0))
            }
            MutationDelivery::Reconcile | MutationDelivery::Retry => {
                if self.model_pending.as_ref() != Some(submission) || controller.pending().is_none()
                {
                    return Err("metadata_original_pending_context_required".into());
                }
                match delivery {
                    MutationDelivery::Reconcile => controller.reconcile_pending().await,
                    MutationDelivery::Retry => controller.retry_pending().await,
                    MutationDelivery::Submit => unreachable!(),
                }
                .map_err(|error| bounded_problem(&error.0))
            }
        }
    }

    async fn conversation(&mut self, job: ConversationJob) -> ConversationEvent {
        match job {
            ConversationJob::Read { context, query } => {
                let result = self.read_conversation(&context, query).await.map(Box::new);
                ConversationEvent::Read { context, result }
            }
            ConversationJob::Mutation {
                submission,
                delivery,
            } => {
                let result = self
                    .mutate_conversation(&submission, delivery)
                    .await
                    .map(Box::new);
                let pending = self.controller.as_ref().and_then(|c| c.pending());
                let operation = self.conversation_operation.clone();
                let retained = pending.is_some() && self.conversation_pending.is_some();
                assert!(!retained || (self.conversation_pending.is_some() && operation.is_some()));
                if !retained {
                    self.conversation_pending = None;
                    self.conversation_operation = None;
                }
                ConversationEvent::Mutation {
                    submission,
                    operation,
                    result,
                    retained,
                }
            }
        }
    }

    async fn read_conversation(
        &mut self,
        context: &ConversationContext,
        query: ConversationQuery,
    ) -> Result<OwnerReadReply, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        if controller.identity() != &context.identity {
            return Err("conversation_connection_changed".into());
        }
        let cursor = match &query {
            ConversationQuery::History { cursor } => cursor.clone(),
            ConversationQuery::Brief => None,
        };
        let history = matches!(&query, ConversationQuery::History { .. });
        let read = match query {
            ConversationQuery::History { cursor } => OwnerRead::ConversationHistory {
                project_id: context.project.clone(),
                cursor,
                limit: 32,
            },
            ConversationQuery::Brief => OwnerRead::CurrentBrief {
                project_id: context.project.clone(),
            },
        };
        let state = controller
            .read(read)
            .await
            .map_err(|e| bounded_problem(&e.0))?;
        match &state.data {
            OwnerData::ConversationHistory { page } => {
                if !history || page.project_id != context.project || page.entries.len() > 32 {
                    return Err("conversation_page_context".into());
                }
                let mut after = cursor.as_ref().map_or(0, |c| c.after.value());
                for entry in &page.entries {
                    if entry.text.len() > crate::conversation::TEXT_BYTES_MAX
                        || entry.event_cursor.value() <= after
                        || entry.event_cursor.value() > state.revision.value()
                        || entry.provenance.event_cursor.as_ref() != Some(&entry.event_cursor)
                        || entry.provenance.revision_after != entry.event_cursor
                    {
                        return Err("conversation_entry_context".into());
                    }
                    after = entry.event_cursor.value();
                }
                if let Some(next) = &page.next
                    && (page.entries.is_empty()
                        || next.project_id != context.project
                        || next.db_instance != context.identity.database_instance()
                        || next.engine_epoch != context.identity.engine_epoch()
                        || next.at_revision != state.revision
                        || next.config_id != state.config_id
                        || next.after.value() != after)
                {
                    return Err("conversation_cursor_context".into());
                }
            }
            OwnerData::CurrentBrief { project_id, brief } => {
                if history
                    || project_id != &context.project
                    || brief.as_ref().is_some_and(|entry| {
                        !matches!(
                            entry.content,
                            crate::conversation::ConversationContent::Brief { .. }
                        ) || entry.text.len() > crate::conversation::TEXT_BYTES_MAX
                            || entry.event_cursor.value() == 0
                            || entry.event_cursor.value() > state.revision.value()
                            || entry.provenance.event_cursor.as_ref() != Some(&entry.event_cursor)
                            || entry.provenance.revision_after != entry.event_cursor
                    })
                {
                    return Err("conversation_brief_context".into());
                }
            }
            OwnerData::ResyncRequired => {}
            _ => return Err("conversation_read_kind".into()),
        }
        Ok(state)
    }

    async fn mutate_conversation(
        &mut self,
        submission: &ConversationSubmission,
        delivery: MutationDelivery,
    ) -> Result<OwnerResponse, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        match delivery {
            MutationDelivery::Submit => {
                if controller.identity() != &submission.identity
                    || self.creation_pending.is_some()
                    || self.model_pending.is_some()
                    || self.conversation_pending.is_some()
                    || self.master_pending.is_some()
                    || controller.pending().is_some()
                {
                    return Err("conversation_pending_or_connection_changed".into());
                }
                let project = match &submission.mutation {
                    ConversationMutation::AppendMessage { project_id, .. }
                    | ConversationMutation::ReplaceBrief { project_id, .. } => project_id.clone(),
                };
                let state = controller
                    .read(OwnerRead::CurrentBrief {
                        project_id: project.clone(),
                    })
                    .await
                    .map_err(|e| bounded_problem(&e.0))?;
                if !matches!(state.data, OwnerData::CurrentBrief { project_id, .. } if project_id == project)
                {
                    return Err("conversation_fresh_read_required".into());
                }
                let pending = controller
                    .begin_mutation(
                        OwnerMutation::Conversation {
                            mutation: submission.mutation.clone(),
                        },
                        submission.generation,
                    )
                    .map_err(|e| bounded_problem(&e.0))?;
                self.conversation_operation = Some((
                    pending.request().request_id.clone(),
                    pending.hash().to_owned(),
                ));
                self.conversation_pending = Some(submission.clone());
                controller
                    .send_pending()
                    .await
                    .map_err(|e| bounded_problem(&e.0))
            }
            MutationDelivery::Reconcile | MutationDelivery::Retry => {
                if self.conversation_pending.as_ref() != Some(submission)
                    || controller.pending().is_none()
                {
                    return Err("conversation_pending_context".into());
                }
                match delivery {
                    MutationDelivery::Reconcile => controller.reconcile_pending().await,
                    MutationDelivery::Retry => controller.retry_pending().await,
                    MutationDelivery::Submit => unreachable!(),
                }
                .map_err(|e| bounded_problem(&e.0))
            }
        }
    }

    async fn creation(&mut self, job: CreationJob) -> CreationEvent {
        let result = self
            .mutate_creation(&job.submission, job.delivery)
            .await
            .map(Box::new);
        self.creation_event(job.submission, result)
    }

    fn creation_event(
        &mut self,
        submission: CreationSubmission,
        result: Result<Box<OwnerResponse>, String>,
    ) -> CreationEvent {
        let retained = self.creation_pending.is_some()
            && self
                .controller
                .as_ref()
                .is_some_and(|controller| controller.pending().is_some());
        let operation = self.creation_operation.clone();
        assert!(!retained || operation.is_some());
        self.creation_retry_allowed = retained
            && matches!(&result, Ok(response) if matches!(response.as_ref(), OwnerResponse::NotRecordedAtBarrier { .. }));
        if !retained {
            self.creation_pending = None;
            self.creation_operation = None;
        }
        CreationEvent {
            submission,
            operation,
            result,
            retained,
        }
    }

    async fn prepare_creation(&mut self, submission: &CreationSubmission) -> Result<(), String> {
        if self.creation_pending.is_some()
            || self.conversation_pending.is_some()
            || self.model_pending.is_some()
            || self.master_pending.is_some()
        {
            return Err("creation_cross_kind_pending".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        if controller.identity() != &submission.identity || controller.pending().is_some() {
            return Err("creation_pending_or_connection_changed".into());
        }
        submission.spec.validate().map_err(|e| e.to_string())?;
        if let Some(setup) = &submission.setup {
            setup.validate().map_err(|e| bounded_problem(&e.0))?;
        } else if submission.spec.repositories.is_empty()
            || submission
                .spec
                .repositories
                .iter()
                .any(|path| !path.starts_with('/'))
            || submission.spec.name.len()
                + submission
                    .spec
                    .repositories
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
                > 65536
        {
            return Err("creation_requires_declared_absolute_paths".into());
        }
        controller
            .read(OwnerRead::Page {
                view: OwnerView::Projects,
                cursor: None,
                limit: 1,
            })
            .await
            .map_err(|e| bounded_problem(&e.0))?;
        let mutation = match &submission.setup {
            Some(setup) => OwnerMutation::SetupProject {
                project_id: submission.project_id.clone(),
                spec: submission.spec.clone(),
                setup: setup.clone(),
            },
            None => OwnerMutation::CreateProject {
                project_id: submission.project_id.clone(),
                spec: submission.spec.clone(),
            },
        };
        let pending = controller
            .begin_mutation(mutation, submission.generation)
            .map_err(|e| bounded_problem(&e.0))?;
        self.creation_operation = Some((
            pending.request().request_id.clone(),
            pending.hash().to_owned(),
        ));
        self.creation_pending = Some(submission.clone());
        Ok(())
    }

    async fn mutate_creation(
        &mut self,
        submission: &CreationSubmission,
        delivery: MutationDelivery,
    ) -> Result<OwnerResponse, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        if matches!(delivery, MutationDelivery::Submit) {
            self.prepare_creation(submission).await?;
        } else {
            let controller = self
                .controller
                .as_ref()
                .ok_or("trusted_connection_unavailable")?;
            if self.creation_pending.as_ref() != Some(submission)
                || controller.pending().is_none()
                || controller.identity().database_instance()
                    != submission.identity.database_instance()
                || (matches!(delivery, MutationDelivery::Retry) && !self.creation_retry_allowed)
            {
                return Err("creation_exact_reconcile_or_retry_required".into());
            }
        }
        self.deliver_creation(delivery).await
    }

    async fn deliver_creation(
        &mut self,
        delivery: MutationDelivery,
    ) -> Result<OwnerResponse, String> {
        self.creation_retry_allowed = false;
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        match delivery {
            MutationDelivery::Submit => controller.send_pending().await,
            MutationDelivery::Reconcile => controller.reconcile_pending().await,
            MutationDelivery::Retry => controller.retry_pending().await,
        }
        .map_err(|e| bounded_problem(&e.0))
    }

    async fn step(
        &mut self,
        project: ProjectId,
        generation: u64,
        first: bool,
    ) -> Result<OwnerProgress, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        if first {
            self.scan = Some(Scan {
                project: project.clone(),
                generation,
                cursor: None,
                revision: None,
                config_id: None,
                last_id: None,
                pages: 0,
                selected: None,
            });
        }
        let scan = self.scan.as_mut().ok_or("owner_scan_missing")?;
        if scan.project != project || scan.generation != generation {
            return Err("owner_scan_context_mismatch".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        let query = if let Some(baseline_id) = &scan.selected {
            OwnerRead::Baseline {
                baseline_id: baseline_id.clone(),
            }
        } else {
            OwnerRead::Page {
                view: OwnerView::Baselines,
                cursor: scan.cursor.clone(),
                limit: 32,
            }
        };
        let state = controller
            .read(query)
            .await
            .map_err(|error| bounded_problem(&error.0))?;
        if !controller.view().is_some_and(|view| view.fresh)
            || scan
                .revision
                .as_ref()
                .is_some_and(|revision| revision != &state.revision)
            || scan
                .config_id
                .as_ref()
                .is_some_and(|config| config != &state.config_id)
        {
            self.scan = None;
            return Ok(OwnerProgress::Refresh);
        }
        match state.data {
            OwnerData::Page { items, next } if scan.selected.is_none() => {
                if items.len() > 32 || scan.pages >= 4 {
                    return Err("owner_page_bounds".into());
                }
                scan.pages += 1;
                scan.revision = Some(state.revision.clone());
                scan.config_id = Some(state.config_id.clone());
                for item in items {
                    let OwnerSummary::Baseline {
                        baseline_id,
                        project_id,
                        ..
                    } = item
                    else {
                        return Err("owner_page_kind".into());
                    };
                    if scan
                        .last_id
                        .as_deref()
                        .is_some_and(|last| last >= baseline_id.as_str())
                    {
                        return Err("owner_page_order".into());
                    }
                    scan.last_id = Some(baseline_id.as_str().to_owned());
                    if scan.selected.is_none() && project_id == project {
                        scan.selected = Some(baseline_id);
                    }
                }
                if let Some(cursor) = &next
                    && (Some(cursor.after_id.as_str()) != scan.last_id.as_deref()
                        || cursor.at_revision != state.revision
                        || cursor.config_id != state.config_id
                        || cursor.view != OwnerView::Baselines)
                {
                    return Err("owner_cursor_context".into());
                }
                if scan.selected.is_some() {
                    return Ok(OwnerProgress::Continue);
                }
                if next.is_some() && scan.pages < 4 {
                    scan.cursor = next;
                    return Ok(OwnerProgress::Continue);
                }
                let progress = if next.is_some() {
                    OwnerProgress::Incomplete {
                        revision: state.revision,
                    }
                } else {
                    OwnerProgress::Empty {
                        revision: state.revision,
                    }
                };
                self.scan = None;
                Ok(progress)
            }
            OwnerData::Baseline {
                draft,
                approved,
                revoked,
                current,
            } => {
                let baseline_id = scan
                    .selected
                    .as_ref()
                    .ok_or("owner_detail_without_discovery")?;
                if draft.project_id != project
                    || draft.identity().map_err(|_| "owner_baseline_invalid")? != *baseline_id
                {
                    return Err("owner_detail_identity".into());
                }
                let evidence = Evidence {
                    project,
                    baseline_id: baseline_id.clone(),
                    requirements: draft.requirements,
                    approved,
                    revoked,
                    current,
                    revision: state.revision,
                    config_id: state.config_id,
                };
                self.scan = None;
                Ok(OwnerProgress::Ready(evidence))
            }
            OwnerData::ResyncRequired => {
                self.scan = None;
                Ok(OwnerProgress::Refresh)
            }
            _ => Err("owner_read_kind".into()),
        }
    }
}

/// Tells whether a failed owner job means the trusted connection is gone.
/// Takes the job's problem: a plain code, possibly followed by "; " and advice.
/// Returns true only for a lost, replaced or unreachable engine connection.
fn job_lost(problem: &str) -> bool {
    settings::connection_lost(problem.split(';').next().unwrap_or(problem))
}

/// Detaches the owner after a job that failed because the engine connection is gone,
/// and tells the shell once, with `Access { ready: false }`, so it reconnects.
/// Inputs: readiness before the job, the owner, the failed job's problem, the ordinary
/// client to replace and the owner reply channel. Output: false once the shell is gone.
fn report_loss(
    was_ready: bool,
    native: &mut NativeOwner,
    problem: Option<&str>,
    client: &mut Client,
    replies: &SyncSender<OwnerReply>,
) -> bool {
    if problem.is_some_and(job_lost) {
        native.detach();
    }
    if !was_ready || native.ready {
        return true;
    }
    *client = native.ordinary();
    replies
        .send(OwnerReply {
            finished: false,
            event: OwnerEvent::Access {
                ready: false,
                problem: "trusted_connection_lost".into(),
                identity: None,
            },
        })
        .is_ok()
}

fn bounded_problem(problem: &str) -> String {
    if !problem.is_empty()
        && problem.len() <= 96
        && problem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        problem.to_owned()
    } else {
        "trusted_owner_connection_failed".into()
    }
}

#[cfg(all(test, unix))]
#[path = "dispatch_wiring_tests.rs"]
mod dispatch_wiring_tests;

#[cfg(all(test, unix))]
#[path = "connection_loss_tests.rs"]
mod connection_loss_tests;

#[cfg(all(test, unix))]
mod creation_recovery_tests {
    use super::*;
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::{OwnerControl, OwnerDecision, OwnerResult};
    use crate::protocol::Outcome;
    use std::os::unix::fs::PermissionsExt;

    struct RealNative {
        root: tempfile::TempDir,
        service: EngineService,
        native: NativeOwner,
        _stop: tokio::sync::watch::Sender<bool>,
    }

    impl RealNative {
        async fn new() -> Self {
            let root = tempfile::Builder::new()
                .prefix("ca-native373-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let config = crate::config::resolve_config(
                &root.path().join("global"),
                &root.path().join("project"),
            )
            .unwrap();
            let service = EngineService::start(root.path(), "engine.sock", &config, &stopped)
                .await
                .unwrap();
            let mut native =
                NativeOwner::new(root.path().join("engine.sock"), Duration::from_secs(2));
            let (_, access) = native.attach().await;
            assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
            Self {
                root,
                service,
                native,
                _stop: stop,
            }
        }

        fn submission(&self) -> CreationSubmission {
            CreationSubmission {
                identity: self.native.controller.as_ref().unwrap().identity().clone(),
                generation: 7,
                navigation: 9,
                project_id: ProjectId::new("native-target").unwrap(),
                spec: ProjectSpec {
                    name: "Native target".into(),
                    repositories: vec!["/absent/native target".into()],
                },
                setup: None,
            }
        }

        async fn queued(&mut self) -> (super::super::app::App, CreationSubmission) {
            use super::super::app::{App, UiEvent};
            let (_, access) = self.native.attach().await;
            let mut app = App::new();
            app.apply_owner(access).unwrap();
            app.reduce(UiEvent::FocusNext).unwrap();
            app.reduce(UiEvent::BeginDeclaration).unwrap();
            for character in "Native target".chars() {
                app.reduce(UiEvent::Character(character)).unwrap();
            }
            app.reduce(UiEvent::FocusNext).unwrap();
            for character in "/absent/native target".chars() {
                app.reduce(UiEvent::Character(character)).unwrap();
            }
            app.reduce(UiEvent::Submit).unwrap();
            let job = app.take_creation_job().unwrap();
            (app, job.submission)
        }

        async fn control(&self, decision: OwnerDecision, id: &str) {
            let runtime = self.service.owner_runtime().unwrap();
            let result = runtime
                .control(OwnerControl {
                    operation_id: RequestId::new(id).unwrap(),
                    target: OwnerClientKind::Tui,
                    expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
                    decision,
                })
                .await
                .unwrap();
            assert_eq!(result.active, decision == OwnerDecision::Reactivate);
        }

        async fn close(mut self) {
            drop(self.native);
            self.service.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn actual_pre_auth_revoke_retains_unknown_intent_then_exact_barrier_retry() {
        let mut h = RealNative::new().await;
        let submission = h.submission();
        h.native.prepare_creation(&submission).await.unwrap();
        let operation = h.native.creation_operation.clone().unwrap();
        let request = h
            .native
            .controller
            .as_ref()
            .unwrap()
            .pending()
            .unwrap()
            .request()
            .clone();
        h.control(OwnerDecision::Revoke, "revoke-before-send").await;
        let result = h
            .native
            .deliver_creation(MutationDelivery::Submit)
            .await
            .map(Box::new);
        let event = h.native.creation_event(submission.clone(), result);
        assert!(event.result.is_err());
        assert!(event.retained);
        assert_eq!(event.operation, Some(operation.clone()));
        assert!(
            h.native
                .mutate_creation(&submission, MutationDelivery::Retry)
                .await
                .is_err()
        );
        h.control(OwnerDecision::Reactivate, "reactivate-before-reconcile")
            .await;
        let (_, access) = h.native.attach().await;
        assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
        let barrier = h
            .native
            .creation(CreationJob {
                submission: submission.clone(),
                delivery: MutationDelivery::Reconcile,
            })
            .await;
        assert!(matches!(
            barrier.result.as_deref(),
            Ok(OwnerResponse::NotRecordedAtBarrier { .. })
        ));
        assert_eq!(
            h.native
                .controller
                .as_ref()
                .unwrap()
                .pending()
                .unwrap()
                .request(),
            &request
        );
        let event = h
            .native
            .creation(CreationJob {
                submission,
                delivery: MutationDelivery::Retry,
            })
            .await;
        assert_eq!(event.operation, Some(operation));
        assert!(!event.retained);
        assert!(
            matches!(event.result.as_deref(), Ok(OwnerResponse::Applied { operation })
            if operation.result == OwnerResult::Applied)
        );
        h.close().await;
    }

    #[tokio::test]
    async fn a_creation_the_engine_refuses_frees_its_slot_for_the_next_creation_and_master_change()
    {
        let mut h = RealNative::new().await;
        let refused = h.submission();
        h.native.prepare_creation(&refused).await.unwrap();
        let owner_socket = std::fs::read_dir(h.root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path().join("owner.sock"))
            .find(|path| path.exists())
            .expect("the engine serves its owner socket in a generation under the root");
        let mut silent = Vec::new();
        for _ in 0..3 {
            silent.push(
                crate::spawn_gate::connect_unix(&owner_socket)
                    .await
                    .unwrap(),
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let result = h
            .native
            .deliver_creation(MutationDelivery::Submit)
            .await
            .map(Box::new);
        let event = h.native.creation_event(refused.clone(), result);
        assert!(
            matches!(event.result.as_deref(), Ok(OwnerResponse::Rejected { code }) if code == "owner_busy"),
            "the engine refuses the creation while its owner queue is full: {:?}",
            event.result
        );
        assert!(!event.retained, "a refused creation is not kept");
        assert!(h.native.creation_pending.is_none());
        assert!(h.native.controller.as_ref().unwrap().pending().is_none());
        drop(silent);
        let mut served = false;
        for _ in 0..50 {
            if h.native
                .controller
                .as_mut()
                .unwrap()
                .read(OwnerRead::Capabilities)
                .await
                .is_ok()
            {
                served = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(served, "the engine serves again once the queue empties");

        let created = h
            .native
            .creation(CreationJob {
                submission: refused.clone(),
                delivery: MutationDelivery::Submit,
            })
            .await;
        assert!(
            matches!(created.result.as_deref(), Ok(OwnerResponse::Applied { operation })
                if operation.result == OwnerResult::Applied),
            "the same project can be created again without a restart"
        );
        assert!(!created.retained);

        let master = h
            .native
            .master(master_session::Job::Mutation {
                submission: master_session::Submission {
                    identity: refused.identity.clone(),
                    generation: 8,
                    action: crate::master_session::MasterOwnerAction::Close {
                        project_id: refused.project_id.clone(),
                        session_id: crate::protocol::SessionId::new("absent-session").unwrap(),
                    },
                },
                delivery: MutationDelivery::Submit,
            })
            .await;
        let master_session::Event::Mutation { result, .. } = master else {
            panic!("a master change answers as a mutation");
        };
        assert!(
            result.is_ok(),
            "the master change reaches the engine instead of waiting on the refused creation: {:?}",
            result.err()
        );
        h.close().await;
    }

    #[tokio::test]
    async fn actual_stale_prepared_creation_is_recorded_rejection_without_rebase() {
        let mut h = RealNative::new().await;
        let (mut app, submission) = h.queued().await;
        h.native.prepare_creation(&submission).await.unwrap();
        let operation = h.native.creation_operation.clone();
        let reply = h
            .native
            .ordinary()
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("competing-owner").unwrap(),
                request_id: RequestId::new("competing-project").unwrap(),
                expected_revision: Cursor::new(0),
                command: Command::CreateProject {
                    project_id: ProjectId::new("competitor").unwrap(),
                    spec: ProjectSpec {
                        name: "Competing project".into(),
                        repositories: vec![],
                    },
                },
            })
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
        let result = h
            .native
            .deliver_creation(MutationDelivery::Submit)
            .await
            .map(Box::new);
        let event = h.native.creation_event(submission, result);
        assert_eq!(event.operation, operation);
        assert!(!event.retained);
        assert!(
            matches!(event.result.as_deref(), Ok(OwnerResponse::Applied { operation })
            if matches!(operation.result, OwnerResult::Rejected { .. }))
        );
        assert!(h.native.creation_pending.is_none());
        app.apply_owner(OwnerEvent::Creation(Box::new(event)))
            .unwrap();
        assert!(app.creation_pending_label().is_none());
        assert_eq!(app.draft(), "/absent/native target");
        assert!(app.selected_project().is_none());
        let db = rusqlite::Connection::open(h.root.path().join("engine.db")).unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(db);
        h.close().await;
    }

    #[tokio::test]
    async fn actual_wrong_database_attach_cannot_replace_pending_creation() {
        let mut h = RealNative::new().await;
        let other = RealNative::new().await;
        let submission = h.submission();
        h.native.prepare_creation(&submission).await.unwrap();
        let request = h
            .native
            .controller
            .as_ref()
            .unwrap()
            .pending()
            .unwrap()
            .request()
            .clone();
        h.native.detach();
        h.native.socket = other.root.path().join("engine.sock");
        let (_, access) = h.native.attach().await;
        assert!(matches!(access, OwnerEvent::Access { ready: false, .. }));
        assert_eq!(
            h.native
                .controller
                .as_ref()
                .unwrap()
                .pending()
                .unwrap()
                .request(),
            &request
        );
        let result = h
            .native
            .creation(CreationJob {
                submission,
                delivery: MutationDelivery::Reconcile,
            })
            .await;
        assert!(result.result.is_err());
        assert!(result.retained);
        other.close().await;
        h.close().await;
    }

    #[tokio::test]
    async fn actual_owner_frame_bound_rejects_escaped_content_before_retaining_an_operation() {
        let mut h = RealNative::new().await;
        let mut submission = h.submission();
        submission.spec.name = "x".into();
        submission.spec.repositories = vec![format!("/{}", "\"".repeat(4095)); 15];
        submission
            .spec
            .repositories
            .push(format!("/{}", "\"".repeat(4094)));
        assert_eq!(
            submission.spec.name.len()
                + submission
                    .spec
                    .repositories
                    .iter()
                    .map(String::len)
                    .sum::<usize>(),
            65536
        );
        submission.spec.validate().unwrap();
        let error = h.native.prepare_creation(&submission).await.unwrap_err();
        assert!(error.contains("owner_request_bounds"), "{error}");
        assert!(h.native.controller.as_ref().unwrap().pending().is_none());
        assert!(h.native.creation_pending.is_none());
        assert!(h.native.creation_operation.is_none());
        h.close().await;
    }

    #[tokio::test]
    async fn actual_prepared_intent_trips_missing_operation_invariant() {
        let mut h = RealNative::new().await;
        let submission = h.submission();
        h.native.prepare_creation(&submission).await.unwrap();
        h.native.detach();
        let result = h
            .native
            .mutate_creation(&submission, MutationDelivery::Reconcile)
            .await
            .map(Box::new);
        assert!(result.is_err());
        h.native.creation_operation = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            h.native.creation_event(submission, result)
        }));
        assert!(result.is_err());
        h.close().await;
    }

    fn creation_table_counts(db: &rusqlite::Connection) -> std::collections::BTreeMap<String, i64> {
        let mut statement = db
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(names.len() <= 128);
        names
            .into_iter()
            .map(|name| {
                assert!(
                    name.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                );
                let count = db
                    .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                (name, count)
            })
            .collect()
    }

    fn read_committed_creation(
        db: &rusqlite::Connection,
        id: &RequestId,
    ) -> Option<(crate::engine::owner::OwnerProvenance, OwnerResult)> {
        use rusqlite::OptionalExtension;
        assert!(db.is_autocommit());
        let row = db
            .query_row(
                "SELECT owner_id,request_hash,engine_epoch,admitted_epoch,resulting_epoch,revision_before,revision_after,result,event_cursor FROM owner_operations WHERE origin='native_owner' AND operation_id=?1",
                [id.as_str()],
                |row| {
                    let hash: Vec<u8> = row.get(1)?;
                    assert_eq!(hash.len(), 32);
                    Ok((
                        crate::engine::owner::OwnerProvenance {
                            origin: "native_owner".into(),
                            owner_id: row.get(0)?,
                            operation_id: id.clone(),
                            request_hash: hash.iter().map(|byte| format!("{byte:02x}")).collect(),
                            engine_epoch: row.get(2)?,
                            admitted_epoch: row.get(3)?,
                            resulting_epoch: row.get(4)?,
                            revision_before: Cursor::new(row.get::<_, i64>(5)?.try_into().unwrap()),
                            revision_after: Cursor::new(row.get::<_, i64>(6)?.try_into().unwrap()),
                            event_cursor: row
                        .get::<_, Option<i64>>(8)?
                        .map(|cursor| Cursor::new(cursor.try_into().unwrap())),
                        },
                        row.get::<_, Vec<u8>>(7)?,
                    ))
                },
            )
            .optional()
            .unwrap();
        assert!(db.is_autocommit());
        row.map(|(provenance, bytes)| (provenance, serde_json::from_slice(&bytes).unwrap()))
    }

    async fn drop_creation_delivery_after_commit(
        native: &mut NativeOwner,
        db: &rusqlite::Connection,
        id: &RequestId,
    ) -> Result<(crate::engine::owner::OwnerProvenance, OwnerResult), String> {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let mut delivery = Box::pin(native.deliver_creation(MutationDelivery::Submit));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        for _ in 0..100_000 {
            if std::time::Instant::now() >= deadline {
                return Err("real committed receipt exceeded two-second deadline".into());
            }
            if let Some(receipt) = read_committed_creation(db, id) {
                drop(delivery);
                return Ok(receipt);
            }
            let polled = delivery
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            if let Poll::Ready(result) = polled {
                return Err(format!("delivery settled before cancellation: {result:?}"));
            }
            tokio::task::yield_now().await;
        }
        Err("real committed receipt exceeded 100000 delivery polls".into())
    }

    fn assert_exact_uncertain_creation(
        native: &NativeOwner,
        submission: &CreationSubmission,
        request: &crate::engine::owner::OwnerRequest,
        hash: &str,
    ) {
        let pending = native.controller.as_ref().unwrap().pending().unwrap();
        assert!(pending.uncertain());
        assert_eq!(pending.request(), request);
        assert_eq!(pending.hash(), hash);
        assert_eq!(pending.generation(), submission.generation);
        let retained = native.creation_pending.as_ref().unwrap();
        assert_eq!(retained.identity, submission.identity);
        assert_eq!(retained.generation, submission.generation);
        assert_eq!(retained.navigation, submission.navigation);
        assert_eq!(retained.project_id, submission.project_id);
        assert_eq!(retained.spec, submission.spec);
        assert_eq!(
            native.creation_operation.as_ref(),
            Some(&(request.request_id.clone(), hash.to_owned()))
        );
        assert!(!native.creation_retry_allowed);
    }

    async fn restart_creation_service(h: &mut RealNative) {
        let identity = h.native.controller.as_ref().unwrap().identity().clone();
        h.native.detach();
        h.service.shutdown().await.unwrap();
        let config = crate::config::resolve_config(
            &h.root.path().join("global"),
            &h.root.path().join("project"),
        )
        .unwrap();
        h.service =
            EngineService::start(h.root.path(), "engine.sock", &config, &h._stop.subscribe())
                .await
                .unwrap();
        let (_, access) = h.native.attach().await;
        assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
        let current = h.native.controller.as_ref().unwrap().identity();
        assert_eq!(current.database_instance(), identity.database_instance());
        assert_ne!(current.engine_epoch(), identity.engine_epoch());
    }

    async fn assert_exact_creation_snapshot(native: &NativeOwner, submission: &CreationSubmission) {
        let reply = native
            .ordinary()
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("lost-ack-observer").unwrap(),
                request_id: RequestId::new("lost-ack-snapshot").unwrap(),
                expected_revision: Cursor::new(0),
                command: Command::Snapshot,
            })
            .await
            .unwrap();
        assert_eq!(reply.revision, Cursor::new(1));
        let Outcome::Snapshot { projects, tasks } = reply.outcome else {
            panic!("real snapshot was not returned");
        };
        assert!(tasks.is_empty());
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project_id, submission.project_id);
        assert_eq!(projects[0].spec, submission.spec);
    }

    async fn reconcile_committed_creation(
        native: &mut NativeOwner,
        submission: &CreationSubmission,
        request: &crate::engine::owner::OwnerRequest,
        hash: &str,
        receipt: &(crate::engine::owner::OwnerProvenance, OwnerResult),
    ) {
        let event = native
            .creation(CreationJob {
                submission: submission.clone(),
                delivery: MutationDelivery::Reconcile,
            })
            .await;
        let Ok(OwnerResponse::Committed { operation }) = event.result.as_deref() else {
            panic!(
                "actual reconciliation did not return Committed: {:?}",
                event.result
            );
        };
        assert_eq!(operation.result, receipt.1);
        assert_eq!(operation.provenance, receipt.0);
        assert!(operation.historical_replay);
        assert_eq!(operation.current_revision, Cursor::new(1));
        assert_eq!(
            event.operation,
            Some((request.request_id.clone(), hash.to_owned()))
        );
        assert!(!event.retained);
        assert!(native.controller.as_ref().unwrap().pending().is_none());
        assert!(native.creation_pending.is_none());
        assert!(native.creation_operation.is_none());
        assert!(!native.creation_retry_allowed);
        assert!(
            native
                .mutate_creation(submission, MutationDelivery::Retry)
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_committed_creation_loses_ack_then_reconciles_without_retry() {
        let mut h = RealNative::new().await;
        let (_, submission) = h.queued().await;
        h.native.prepare_creation(&submission).await.unwrap();
        let pending = h.native.controller.as_ref().unwrap().pending().unwrap();
        let request = pending.request().clone();
        let hash = pending.hash().to_owned();
        let db = rusqlite::Connection::open_with_flags(
            h.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut expected = creation_table_counts(&db);
        for table in ["projects", "events", "owner_operations"] {
            assert_eq!(expected[table], 0);
            *expected.get_mut(table).unwrap() += 1;
        }
        let receipt = match drop_creation_delivery_after_commit(
            &mut h.native,
            &db,
            &request.request_id,
        )
        .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                drop(db);
                h.close().await;
                panic!("{error}");
            }
        };
        assert_eq!(receipt.1, OwnerResult::Applied);
        assert_eq!(receipt.0.operation_id, request.request_id);
        assert_eq!(receipt.0.request_hash, hash);
        assert_eq!(receipt.0.engine_epoch, submission.identity.engine_epoch());
        assert_eq!(receipt.0.revision_before, Cursor::new(0));
        assert_eq!(receipt.0.revision_after, Cursor::new(1));
        assert_eq!(receipt.0.event_cursor, Some(Cursor::new(1)));
        assert_exact_uncertain_creation(&h.native, &submission, &request, &hash);
        assert_eq!(creation_table_counts(&db), expected);
        restart_creation_service(&mut h).await;
        assert_exact_uncertain_creation(&h.native, &submission, &request, &hash);
        assert_eq!(
            read_committed_creation(&db, &request.request_id),
            Some(receipt.clone())
        );
        reconcile_committed_creation(&mut h.native, &submission, &request, &hash, &receipt).await;
        assert_eq!(creation_table_counts(&db), expected);
        *expected.get_mut("operations").unwrap() += 1;
        assert_exact_creation_snapshot(&h.native, &submission).await;
        assert_eq!(creation_table_counts(&db), expected);
        drop(db);
        h.close().await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::engine::{Engine, Server};
    use crate::protocol::{Outcome, ProjectId, ProjectSpec};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    async fn response(connection: &mut Connection) -> Result<WireReply, ClientError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = connection.poll().unwrap() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "client response exceeded three seconds"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn unavailable_endpoint_retry_preserves_exact_identity_and_commits_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("engine.sock");
        let mut connection =
            Connection::connect(Client::new(socket.clone(), Duration::from_secs(1)).unwrap())
                .unwrap();
        connection
            .start(
                Command::CreateProject {
                    project_id: ProjectId::new("retry-project").unwrap(),
                    spec: ProjectSpec {
                        name: "Real retry".into(),
                        repositories: Vec::new(),
                    },
                },
                Cursor::new(0),
            )
            .unwrap();
        let original = connection.pending.as_ref().unwrap().request.clone();
        assert!(response(&mut connection).await.is_err());
        assert!(connection.uncertain());
        assert!(connection.start(Command::Snapshot, Cursor::new(0)).is_err());
        assert_eq!(connection.pending.as_ref().unwrap().request, original);
        let config = crate::config::resolve_config(
            &root.path().join("global"),
            &root.path().join("project"),
        )
        .unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("engine.db"), config.clone())
                .await
                .unwrap(),
        );
        let server = Server::bind(&socket, engine.clone(), &config).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let service = tokio::spawn(server.run(stopped));
        connection.retry().unwrap();
        assert_eq!(connection.pending.as_ref().unwrap().request, original);
        let applied = response(&mut connection).await.unwrap();
        assert_eq!(applied.outcome, Outcome::Applied);
        assert!(!connection.busy());
        let observer = Client::new(socket, Duration::from_secs(1)).unwrap();
        assert_eq!(observer.request(original.clone()).await.unwrap(), applied);
        let reply = observer
            .request(WireRequest {
                request_id: RequestId::new("observer").unwrap(),
                command: Command::Snapshot,
                ..original
            })
            .await
            .unwrap();
        let Outcome::Snapshot { projects, .. } = reply.outcome else {
            panic!("snapshot required")
        };
        assert_eq!(projects.len(), 1);
        assert_eq!(reply.revision.value(), 1);
        drop(connection);
        stop.send(()).unwrap();
        service.await.unwrap().unwrap();
        engine.close().await.unwrap();
    }

    #[tokio::test]
    async fn paired_client_configuration_cannot_be_stripped_back_to_unbound() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-bound-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = crate::engine::bootstrap::EngineService::start(
            root.path(),
            "engine.sock",
            &config,
            &stopped,
        )
        .await
        .unwrap();
        let ordinary =
            Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap();
        let (paired, owner, _) = ordinary.with_owner(OwnerClientKind::Tui).await.unwrap();
        let result = paired.into_unbound_configuration();
        drop(owner);
        service.shutdown().await.unwrap();
        assert!(
            matches!(result, Err(error) if error.0 == "tui_requires_original_unbound_configuration")
        );
    }

    #[tokio::test]
    async fn actual_owner_completion_trips_corrupt_pending_invariant() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-pending-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = crate::engine::bootstrap::EngineService::start(
            root.path(),
            "engine.sock",
            &config,
            &stopped,
        )
        .await
        .unwrap();
        let mut connection = Connection::connect_paired(
            Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(access) = connection.poll_owner().unwrap() {
                assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "actual owner attach exceeded deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        connection.reconnect().unwrap();
        connection.owner_busy = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let tripped = loop {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connection.poll_owner()))
            {
                Err(_) => break true,
                Ok(Ok(None)) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                _ => break false,
            }
        };
        drop(connection);
        service.shutdown().await.unwrap();
        assert!(tripped, "real completion must detect corrupt pending state");
    }

    #[tokio::test]
    async fn limited_mode_retains_exact_original_ordinary_endpoint_without_owner_access() {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-limited-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config =
            crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let (_watch, stopped) = tokio::sync::watch::channel(false);
        let mut owner_service = crate::engine::bootstrap::EngineService::start(
            root.path(),
            "engine.sock",
            &config,
            &stopped,
        )
        .await
        .unwrap();
        let engine = Arc::new(
            Engine::open(&root.path().join("other.db"), config.clone())
                .await
                .unwrap(),
        );
        let socket = root.path().join("ordinary.sock");
        let server = Server::bind(&socket, engine.clone(), &config).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run(stopped));
        let mut native = NativeOwner::new(socket, Duration::from_secs(1));
        let (ordinary, access) = native.attach().await;
        let unavailable = native
            .step(ProjectId::new("ordinary-only-project").unwrap(), 1, true)
            .await;
        let created = ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("limited-endpoint").unwrap(),
                request_id: RequestId::new("create-ordinary-only").unwrap(),
                expected_revision: Cursor::new(0),
                command: Command::CreateProject {
                    project_id: ProjectId::new("ordinary-only-project").unwrap(),
                    spec: ProjectSpec {
                        name: "Original ordinary endpoint".into(),
                        repositories: vec![],
                    },
                },
            })
            .await;
        let (_, reconnected) = native.attach().await;
        let actual_owner =
            Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap();
        let observed = actual_owner
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("owner-observer").unwrap(),
                request_id: RequestId::new("owner-snapshot").unwrap(),
                expected_revision: Cursor::new(0),
                command: Command::Snapshot,
            })
            .await;
        drop(native);
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        engine.close().await.unwrap();
        owner_service.shutdown().await.unwrap();
        assert!(matches!(access, OwnerEvent::Access { ready: false, .. }));
        assert!(matches!(
            reconnected,
            OwnerEvent::Access { ready: false, .. }
        ));
        assert!(matches!(unavailable, Err(error) if error == "trusted_connection_unavailable"));
        assert!(matches!(created.unwrap().outcome, Outcome::Applied));
        assert!(
            matches!(observed.unwrap().outcome, Outcome::Snapshot { projects, .. } if projects.is_empty())
        );
    }
}
