use super::*;
use crate::engine::bootstrap::{
    NativeBinding, NativeProfileUse, NativeProfileUsePurpose, runner_binding,
};
use crate::engine::native_auth_host::custody::Retirement;
use crate::storage::{NativeAuthDispatch, NativeAuthWork};
use serde::{Deserialize, Serialize};
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

const READ_BOUND: Duration = Duration::from_secs(10);

fn operation_bound(fact: &NativeAuthFact) -> Duration {
    match fact.action {
        NativeAuthAction::SignIn { .. } => Duration::from_secs(600),
        NativeAuthAction::Logout => READ_BOUND,
    }
}

fn purpose(fact: &NativeAuthFact) -> NativeProfileUsePurpose {
    match fact.action {
        NativeAuthAction::SignIn { .. } => NativeProfileUsePurpose::Authentication,
        NativeAuthAction::Logout => NativeProfileUsePurpose::Logout,
    }
}

fn fact_host(fact: &NativeAuthFact) -> HostStamp {
    HostStamp {
        db_instance: fact.binding.database_instance.clone(),
        engine_epoch: fact.binding.engine_epoch.clone(),
    }
}

pub(super) struct NativeAuthSlot {
    pub(super) token: [u8; 32],
    dispatch: Arc<NativeAuthDispatch>,
    attached: bool,
    live: Arc<AtomicBool>,
    deadline: Instant,
    expired: bool,
    supervisor: Option<tokio::task::AbortHandle>,
}

impl NativeAuthSlot {
    pub(super) fn session_is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
}

pub(crate) struct NativeAuthCustodyLaunch {
    token: [u8; 32],
    authority: Weak<Authority>,
    delivered: bool,
}

impl NativeAuthCustodyLaunch {
    pub(crate) fn write_bootstrap(
        mut self,
        mut input: std::process::ChildStdin,
    ) -> Result<(), EngineError> {
        use std::io::Write;
        self.delivered = true;
        input.write_all(&self.token).map_err(io_error)?;
        drop(input);
        Ok(())
    }
}

impl Drop for NativeAuthCustodyLaunch {
    fn drop(&mut self) {
        if !self.delivered
            && let Some(authority) = self.authority.upgrade()
            && let Ok(mut slot) = authority.native_auth.lock()
            && let Some(slot) = slot.as_mut()
            && !slot.attached
            && bool::from(slot.token.ct_eq(&self.token))
        {
            slot.attached = true;
            slot.live.store(false, Ordering::SeqCst);
        }
        self.token.fill(0);
    }
}

pub(crate) enum NativeAuthCustodyControl {
    Continue,
    Cancel,
}

pub(crate) struct NativeAuthCustodyRetirement {
    dispatch: Arc<NativeAuthDispatch>,
    live: Arc<AtomicBool>,
}

impl NativeAuthCustodyRetirement {
    pub(crate) fn dispatch(&self) -> &NativeAuthDispatch {
        &self.dispatch
    }
    pub(crate) fn session_is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
    pub(crate) fn validate_session(&self) -> Result<(), EngineError> {
        if !self.session_is_live() {
            return Err(failure("native_auth_recovery_required"));
        }
        Ok(())
    }
}

impl OwnerRuntimeHandle {
    pub(crate) async fn prepare_native_auth_custody(
        &self,
        work: NativeAuthWork,
    ) -> Result<NativeAuthCustodyLaunch, EngineError> {
        let dispatch = self
            .engine
            .native_auth_job(|reply| Job::NativeAuthDispatch(Box::new(work), reply))
            .await?;
        self.authority.reserve_native_auth(dispatch)
    }

    pub(super) async fn native_auth_command(
        &self,
        admission: Admission,
        command: crate::engine::owner::native_auth::Command,
    ) -> Result<NativeAuthReply, EngineError> {
        command.validate()?;
        let begun = self
            .engine
            .native_auth_job(|reply| Job::NativeAuth(admission.clone(), command, reply))
            .await?;
        match begun.work {
            None => Ok(begun.reply),
            Some(work) => self
                .engine
                .native_auth_job(|reply| Job::NativeAuthFinish(admission, Box::new(work), reply))
                .await
                .map_err(|error| {
                    if error.0 == "owner_queue_not_submitted" {
                        crate::engine::owner::native_auth::outcome_unknown()
                    } else {
                        error
                    }
                }),
        }
    }
}

impl Authority {
    pub(super) fn can_start_native_auth(&self, packet: &[u8; 37]) -> Result<(), EngineError> {
        if &packet[..4] != b"CAN1" || packet[4] != 0 {
            return Err(failure("native_auth_authentication_failed"));
        }
        let current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_ref()
            .ok_or_else(|| failure("native_auth_authentication_failed"))?;
        if slot.attached
            || slot.expired
            || Instant::now() >= slot.deadline
            || !bool::from(slot.token.ct_eq(&packet[5..]))
        {
            return Err(failure("native_auth_authentication_failed"));
        }
        self.verify_native_auth_origin(&slot.dispatch)
    }

    pub(super) fn track_native_auth(&self, packet: &[u8; 37], task: tokio::task::AbortHandle) {
        if let Ok(mut current) = self.native_auth.lock()
            && let Some(slot) = current.as_mut()
            && bool::from(slot.token.ct_eq(&packet[5..]))
            && slot.supervisor.is_none()
        {
            slot.supervisor = Some(task);
        } else {
            task.abort();
        }
    }

    pub(super) fn native_auth_deadline(&self) -> Option<Instant> {
        self.native_auth.lock().ok().and_then(|current| {
            current
                .as_ref()
                .filter(|slot| !slot.expired)
                .map(|slot| slot.deadline)
        })
    }

    pub(super) async fn expire_native_auth(&self, engine: &Engine) {
        let dispatch = {
            let Ok(mut current) = self.native_auth.lock() else {
                return;
            };
            let Some(slot) = current.as_mut() else {
                return;
            };
            if slot.expired || Instant::now() < slot.deadline {
                return;
            }
            slot.expired = true;
            slot.attached = true;
            slot.live.store(false, Ordering::SeqCst);
            if let Some(task) = slot.supervisor.take() {
                task.abort();
            }
            slot.dispatch.clone()
        };
        let _ = engine
            .native_auth_job(|reply| Job::NativeAuthCustodyLost(dispatch, reply))
            .await;
    }

    fn verify_native_auth_origin(&self, dispatch: &NativeAuthDispatch) -> Result<(), EngineError> {
        if !self.usable.load(Ordering::SeqCst)
            || self.host.db_instance != dispatch.fact().binding.database_instance
            || self.host.engine_epoch != dispatch.fact().binding.engine_epoch
        {
            return Err(failure("owner_runtime_stale"));
        }
        let slots = self
            .slots
            .lock()
            .map_err(|_| failure("owner_authority_unavailable"))?;
        let (index, slot) = slots
            .iter()
            .enumerate()
            .find(|(_, slot)| {
                slot.registration.active && &slot.registration.admission == dispatch.admission()
            })
            .ok_or_else(|| failure("owner_grant_stale_or_revoked"))?;
        self.root
            .verify_key(KEYS[index], &slot.file, slot.identity)?;
        let mut actual = [0; 32];
        slot.file.read_exact_at(&mut actual, 0).map_err(io_error)?;
        if !bool::from(slot.token.ct_eq(&actual)) || self.root.socket_identity()? != self.socket {
            return Err(failure("owner_capability_content_changed"));
        }
        Ok(())
    }

    fn reserve_native_auth(
        self: &Arc<Self>,
        dispatch: NativeAuthDispatch,
    ) -> Result<NativeAuthCustodyLaunch, EngineError> {
        self.verify_native_auth_origin(&dispatch)?;
        let mut current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if current.is_some() {
            return Err(failure("native_auth_host_busy"));
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| failure("owner_entropy_unavailable"))?;
        let deadline = Instant::now() + operation_bound(dispatch.fact());
        *current = Some(NativeAuthSlot {
            token,
            dispatch: Arc::new(dispatch),
            attached: false,
            live: Arc::new(AtomicBool::new(false)),
            deadline,
            expired: false,
            supervisor: None,
        });
        assert!(
            current
                .as_ref()
                .is_some_and(|slot| !slot.attached && !slot.session_is_live())
        );
        Ok(NativeAuthCustodyLaunch {
            token,
            authority: Arc::downgrade(self),
            delivered: false,
        })
    }

    fn attach_native_auth(&self, packet: &[u8; 37]) -> Result<Session, EngineError> {
        if &packet[..4] != b"CAN1" || packet[4] != 0 {
            return Err(failure("native_auth_authentication_failed"));
        }
        let mut current = self
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .ok_or_else(|| failure("native_auth_authentication_failed"))?;
        if slot.attached
            || slot.expired
            || Instant::now() >= slot.deadline
            || !bool::from(slot.token.ct_eq(&packet[5..]))
        {
            return Err(failure("native_auth_authentication_failed"));
        }
        self.verify_native_auth_origin(&slot.dispatch)?;
        slot.attached = true;
        slot.live.store(true, Ordering::SeqCst);
        Ok(Session {
            dispatch: slot.dispatch.clone(),
            live: slot.live.clone(),
            deadline: slot.deadline,
            authority: Weak::new(),
        })
    }

    pub(super) fn stop_native_auth(&self) {
        if let Ok(mut current) = self.native_auth.lock()
            && let Some(slot) = current.as_mut()
        {
            slot.expired = true;
            slot.live.store(false, Ordering::SeqCst);
            if let Some(task) = slot.supervisor.take() {
                task.abort();
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AttachKind {
    Attach,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    protocol_version: u16,
    kind: AttachKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrantKind {
    Grant,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    protocol_version: u16,
    kind: GrantKind,
    fact: NativeAuthFact,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RequestKind {
    Check,
    RetireNoEffect,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol_version: u16,
    operation_id: RequestId,
    generation: u64,
    seq: u64,
    kind: RequestKind,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplyKind {
    Continue,
    Cancel,
    Retired,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    protocol_version: u16,
    seq: u64,
    kind: ReplyKind,
    fact: NativeAuthFact,
}

struct Session {
    dispatch: Arc<NativeAuthDispatch>,
    live: Arc<AtomicBool>,
    deadline: Instant,
    authority: Weak<Authority>,
}

impl Session {
    fn validate(&self) -> Result<Arc<Authority>, EngineError> {
        if !self.live.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            return Err(failure("native_auth_recovery_required"));
        }
        let authority = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        authority.verify_native_auth_origin(&self.dispatch)?;
        Ok(authority)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.live.store(false, Ordering::SeqCst);
    }
}

pub(super) async fn serve(
    mut stream: UnixStream,
    packet: [u8; 37],
    engine: Arc<Engine>,
    authority: Weak<Authority>,
    timeout: Duration,
    attach_deadline: Instant,
) {
    let attached = async {
        let owner = authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let mut session = owner.attach_native_auth(&packet)?;
        session.authority = authority;
        drop(owner);
        let frame = read_frame(&mut stream).await?;
        let attach: Attach = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_frame_invalid"))?;
        if attach.protocol_version != PROTOCOL_VERSION {
            return Err(failure("native_auth_protocol_version"));
        }
        let fact = check(&engine, &session).await?;
        let frame = encode_frame(&Grant {
            protocol_version: PROTOCOL_VERSION,
            kind: GrantKind::Grant,
            fact,
        })
        .map_err(io_error)?;
        stream.write_all(&frame).await.map_err(io_error)?;
        Ok::<_, EngineError>(session)
    };
    let Ok(Ok(session)) = tokio::time::timeout_at(attach_deadline, attached).await else {
        return;
    };
    let retired = tokio::time::timeout_at(
        session.deadline,
        serve_session(&mut stream, &engine, &session, timeout),
    )
    .await;
    session.live.store(false, Ordering::SeqCst);
    if !matches!(retired, Ok(Ok(()))) {
        let _ = engine
            .native_auth_job(|reply| Job::NativeAuthCustodyLost(session.dispatch.clone(), reply))
            .await;
    }
    let _ = stream.shutdown().await;
}

async fn check(engine: &Engine, session: &Session) -> Result<NativeAuthFact, EngineError> {
    session.validate()?;
    let fact = engine
        .native_auth_job(|reply| Job::NativeAuthCustodyCheck(session.dispatch.clone(), reply))
        .await?;
    session.validate()?;
    Ok(fact)
}

async fn read_custody_frame(
    reader: &mut tokio::net::unix::ReadHalf<'_>,
    timeout: Duration,
) -> Result<(Vec<u8>, Instant), EngineError> {
    let first = [reader.read_u8().await.map_err(io_error)?];
    let deadline = Instant::now() + timeout;
    let mut input = tokio::io::AsyncReadExt::chain(first.as_slice(), reader);
    let frame = tokio::time::timeout_at(deadline, read_frame(&mut input))
        .await
        .map_err(|_| failure("native_auth_channel_timeout"))??;
    Ok((frame, deadline))
}

impl Session {
    fn request(&self, frame: &[u8], expected: &mut u64) -> Result<Request, EngineError> {
        let request: Request = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_frame_invalid"))?;
        let fact = self.dispatch.fact();
        if request.protocol_version != PROTOCOL_VERSION
            || request.operation_id != fact.operation_id
            || request.generation != fact.generation
            || request.seq != *expected
        {
            return Err(failure("native_auth_frame_scope"));
        }
        *expected = expected
            .checked_add(1)
            .ok_or_else(|| failure("native_auth_sequence_bounds"))?;
        Ok(request)
    }

    fn committed(&self) -> Result<(), EngineError> {
        let owner = self
            .authority
            .upgrade()
            .ok_or_else(|| failure("owner_runtime_stale"))?;
        let mut current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        if current
            .as_ref()
            .is_some_and(|slot| Arc::ptr_eq(&slot.dispatch, &self.dispatch))
        {
            current.take();
        }
        Ok(())
    }
}

async fn custody_reply(
    engine: &Engine,
    session: &Session,
    request: &Request,
    reader: &mut tokio::net::unix::ReadHalf<'_>,
) -> Result<Reply, EngineError> {
    session.validate()?;
    let retirement = request.kind == RequestKind::RetireNoEffect;
    let job = async {
        if retirement {
            engine
                .native_auth_job(|reply| {
                    Job::NativeAuthCustodyRetire(
                        NativeAuthCustodyRetirement {
                            dispatch: session.dispatch.clone(),
                            live: session.live.clone(),
                        },
                        reply,
                    )
                })
                .await
        } else {
            check(engine, session).await
        }
    };
    let fact = tokio::select! {
        biased;
        _ = reader.read_u8() => {
            session.live.store(false, Ordering::SeqCst);
            return Err(failure("native_auth_channel_lost"));
        }
        fact = job => fact?,
    };
    let kind = if retirement {
        session.committed()?;
        ReplyKind::Retired
    } else {
        session.validate()?;
        if fact.phase == NativeAuthPhase::Cancelling {
            ReplyKind::Cancel
        } else {
            ReplyKind::Continue
        }
    };
    Ok(Reply {
        protocol_version: PROTOCOL_VERSION,
        seq: request.seq,
        kind,
        fact,
    })
}

async fn serve_session(
    stream: &mut UnixStream,
    engine: &Engine,
    session: &Session,
    timeout: Duration,
) -> Result<(), EngineError> {
    let (mut reader, mut writer) = stream.split();
    let mut expected = 1_u64;
    loop {
        let (frame, deadline) = read_custody_frame(&mut reader, timeout).await?;
        let request = session.request(&frame, &mut expected)?;
        let retirement = request.kind == RequestKind::RetireNoEffect;
        let exchange = async {
            let reply = custody_reply(engine, session, &request, &mut reader).await?;
            let frame = encode_frame(&reply).map_err(io_error)?;
            writer.write_all(&frame).await.map_err(io_error)
        };
        tokio::time::timeout_at(deadline, exchange)
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))??;
        if retirement {
            return Ok(());
        }
    }
}

pub(crate) struct NativeAuthCustodyChannel {
    stream: Option<UnixStream>,
    binding: NativeBinding,
    root: Root,
    socket: Identity,
    fact: NativeAuthFact,
    next_seq: u64,
    deadline: Instant,
    outstanding: bool,
    sealed: bool,
}

impl NativeAuthCustodyChannel {
    pub(crate) async fn connect(input: &Path, token: [u8; 32]) -> Result<Self, EngineError> {
        let entered = Instant::now();
        tokio::time::timeout(READ_BOUND, Self::connect_pinned(input, token, entered))
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))?
    }

    async fn connect_pinned(
        input: &Path,
        mut token: [u8; 32],
        entered: Instant,
    ) -> Result<Self, EngineError> {
        let (generation, host, binding) = runner_binding(input)?;
        let root = Root::open(&generation)?;
        let socket = root.socket_identity()?;
        let mut stream = UnixStream::connect(root.path.join(SOCKET))
            .await
            .map_err(io_error)?;
        verify_peer(&stream)?;
        binding.verify()?;
        if root.socket_identity()? != socket {
            return Err(failure("owner_socket_replaced"));
        }
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAN1");
        packet[5..].copy_from_slice(&token);
        token.fill(0);
        stream.write_all(&packet).await.map_err(io_error)?;
        packet.fill(0);
        let frame = encode_frame(&Attach {
            protocol_version: PROTOCOL_VERSION,
            kind: AttachKind::Attach,
        })
        .map_err(io_error)?;
        stream.write_all(&frame).await.map_err(io_error)?;
        let frame = read_frame(&mut stream).await?;
        let grant: Grant = serde_json::from_slice(&frame[4..])
            .map_err(|_| failure("native_auth_grant_invalid"))?;
        Self::validate_grant(&grant, &host)?;
        binding.verify()?;
        if root.socket_identity()? != socket {
            return Err(failure("owner_socket_replaced"));
        }
        let deadline = entered + operation_bound(&grant.fact);
        Ok(Self {
            stream: Some(stream),
            binding,
            root,
            socket,
            fact: grant.fact,
            next_seq: 1,
            deadline,
            outstanding: false,
            sealed: false,
        })
    }

    fn validate_grant(
        grant: &Grant,
        host: &crate::engine::bootstrap::OwnerConnectionIdentity,
    ) -> Result<(), EngineError> {
        if grant.protocol_version != PROTOCOL_VERSION
            || grant.fact.generation == 0
            || grant.fact.binding.database_instance != host.database_instance()
            || grant.fact.binding.engine_epoch != host.engine_epoch()
            || !matches!(
                grant.fact.phase,
                NativeAuthPhase::Starting | NativeAuthPhase::Cancelling
            )
        {
            return Err(failure("native_auth_grant_invalid"));
        }
        Ok(())
    }

    pub(crate) fn fact(&self) -> &NativeAuthFact {
        &self.fact
    }

    fn verify(&self) -> Result<(), EngineError> {
        self.binding.verify()?;
        if Instant::now() >= self.deadline || self.root.socket_identity()? != self.socket {
            return Err(failure("native_auth_channel_lost"));
        }
        Ok(())
    }

    fn close(&mut self) {
        self.sealed = true;
        self.stream.take();
    }

    fn guard_or_close(&mut self, guard: &NativeProfileUse) -> Result<(), EngineError> {
        if let Err(error) = self.verify_guard(guard) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn revalidate(
        &mut self,
        guard: &NativeProfileUse,
    ) -> Result<NativeAuthCustodyControl, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.guard_or_close(guard)?;
        let reply = self.exchange(RequestKind::Check).await?;
        self.guard_or_close(guard)?;
        match reply.kind {
            ReplyKind::Continue => Ok(NativeAuthCustodyControl::Continue),
            ReplyKind::Cancel => Ok(NativeAuthCustodyControl::Cancel),
            ReplyKind::Retired => Err(failure("native_auth_reply_invalid")),
        }
    }

    fn verify_guard(&self, guard: &NativeProfileUse) -> Result<(), EngineError> {
        self.verify()?;
        guard.revalidate(
            &fact_host(&self.fact),
            &self.fact.binding,
            &self.fact.operation_id,
            self.fact.generation,
            purpose(&self.fact),
        )
    }

    pub(crate) async fn retire(
        &mut self,
        retirement: Retirement,
    ) -> Result<NativeAuthFact, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.sealed = true;
        self.retirement_or_close(&retirement)?;
        let reply = self.exchange(RequestKind::RetireNoEffect).await?;
        self.retirement_or_close(&retirement)?;
        if reply.kind != ReplyKind::Retired || reply.fact.phase != NativeAuthPhase::Unavailable {
            self.close();
            return Err(failure("native_auth_reply_invalid"));
        }
        self.close();
        drop(retirement);
        Ok(reply.fact)
    }

    fn retirement_or_close(&mut self, retirement: &Retirement) -> Result<(), EngineError> {
        if let Err(error) = retirement.revalidate(
            &fact_host(&self.fact),
            &self.fact.binding,
            &self.fact.operation_id,
            self.fact.generation,
            purpose(&self.fact),
        ) {
            self.close();
            return Err(error);
        }
        Ok(())
    }

    fn next_request(&mut self, kind: RequestKind) -> Result<Request, EngineError> {
        if self.outstanding {
            return Err(failure("native_auth_recovery_required"));
        }
        self.verify()?;
        self.outstanding = true;
        let seq = self.next_seq;
        self.next_seq = seq
            .checked_add(1)
            .ok_or_else(|| failure("native_auth_sequence_bounds"))?;
        Ok(Request {
            protocol_version: PROTOCOL_VERSION,
            operation_id: self.fact.operation_id.clone(),
            generation: self.fact.generation,
            seq,
            kind,
        })
    }

    fn validate_reply(&self, request: &Request, reply: &Reply) -> Result<(), EngineError> {
        if reply.protocol_version != PROTOCOL_VERSION
            || reply.seq != request.seq
            || reply.fact.operation_id != self.fact.operation_id
            || reply.fact.generation != self.fact.generation
            || reply.fact.binding != self.fact.binding
            || reply.fact.action != self.fact.action
        {
            return Err(failure("native_auth_reply_invalid"));
        }
        let valid = match reply.kind {
            ReplyKind::Continue => {
                request.kind == RequestKind::Check && reply.fact.phase == NativeAuthPhase::Starting
            }
            ReplyKind::Cancel => {
                request.kind == RequestKind::Check
                    && reply.fact.phase == NativeAuthPhase::Cancelling
            }
            ReplyKind::Retired => {
                request.kind == RequestKind::RetireNoEffect
                    && reply.fact.phase == NativeAuthPhase::Unavailable
            }
        };
        if !valid {
            return Err(failure("native_auth_reply_invalid"));
        }
        Ok(())
    }

    async fn exchange(&mut self, kind: RequestKind) -> Result<Reply, EngineError> {
        let request = match self.next_request(kind) {
            Ok(request) => request,
            Err(error) => {
                self.close();
                return Err(error);
            }
        };
        let result =
            tokio::time::timeout_at(self.deadline.min(Instant::now() + READ_BOUND), async {
                let stream = self
                    .stream
                    .as_mut()
                    .ok_or_else(|| failure("native_auth_channel_lost"))?;
                stream
                    .write_all(&encode_frame(&request).map_err(io_error)?)
                    .await
                    .map_err(io_error)?;
                let frame = read_frame(stream).await?;
                let reply: Reply = serde_json::from_slice(&frame[4..])
                    .map_err(|_| failure("native_auth_reply_invalid"))?;
                self.validate_reply(&request, &reply)?;
                Ok(reply)
            })
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))
            .and_then(|result| result);
        if result.is_err() {
            self.close();
        }
        self.outstanding = false;
        if let Err(error) = self.verify() {
            self.close();
            return Err(error);
        }
        result
    }
}
