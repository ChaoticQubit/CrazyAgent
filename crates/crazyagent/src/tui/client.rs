use crate::client_controller::OwnerController;
use crate::conversation::{ConversationCursor, ConversationMutation};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerPageCursor, OwnerRead, OwnerReadReply,
    OwnerResponse, OwnerSummary, OwnerView,
};
use crate::engine::{Client, ClientError};
use crate::protocol::{
    BaselineId, ClientId, Command, ConfigId, Cursor, PROTOCOL_VERSION, ProjectId, RequestId,
    WireReply, WireRequest,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

enum WorkerMode {
    #[cfg(test)]
    Ordinary(Box<Client>),
    Paired {
        socket: PathBuf,
        timeout: Duration,
    },
}

enum Job {
    Wire(Box<WireRequest>),
    Reconnect,
    Conversation(Box<ConversationJob>),
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
    #[cfg(test)]
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
                    #[cfg(test)]
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
                    match job {
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
                        Job::Conversation(job) => {
                            let native = native
                                .as_mut()
                                .expect("conversation requires native configuration");
                            let event = runtime.block_on(native.conversation(*job));
                            let failed = match &event {
                                ConversationEvent::Read { result, .. } => result.is_err(),
                                ConversationEvent::Mutation { result, .. } => result.is_err(),
                            };
                            if failed {
                                native.detach();
                                client = native.ordinary();
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
                        Job::Evidence {
                            project,
                            generation,
                            first,
                        } => {
                            let native = native
                                .as_mut()
                                .expect("owner read requires native configuration");
                            let result =
                                runtime.block_on(native.step(project.clone(), generation, first));
                            if result.is_err() {
                                native.detach();
                                client = native.ordinary();
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
    socket: PathBuf,
    timeout: Duration,
    controller: Option<OwnerController>,
    ready: bool,
    scan: Option<Scan>,
    conversation_pending: Option<ConversationSubmission>,
    conversation_operation: Option<(RequestId, String)>,
}

impl NativeOwner {
    fn new(socket: PathBuf, timeout: Duration) -> Self {
        Self {
            socket,
            timeout,
            controller: None,
            ready: false,
            scan: None,
            conversation_pending: None,
            conversation_operation: None,
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
                let retained = pending.is_some();
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
                    || self.conversation_pending.is_some()
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

#[cfg(test)]
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
            .tempdir_in("/private/tmp")
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
            .tempdir_in("/private/tmp")
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
            .tempdir_in("/private/tmp")
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
