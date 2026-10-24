use super::*;
use crate::engine::native_auth_host::claude_auth::types::{
    ClaudeAuthChallenge, ClaudeAuthObservation,
};
use crate::engine::owner::{AccountSecret, NativeAuthQuery};

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "channel_actor_tests.rs"]
mod actor_tests;

#[derive(Clone)]
pub(crate) enum CustodyEvent {
    Codex(CodexAuthObservation),
    Claude(ClaudeAuthObservation),
}

impl NativeAuthCustodyObservation {
    pub(crate) fn apply(&self, fact: &mut NativeAuthFact) -> Result<(), EngineError> {
        match &self.event {
            CustodyEvent::Codex(event) => event.apply(fact),
            CustodyEvent::Claude(event) => event.apply(fact),
        }
    }
}

pub(super) struct PrivateBytes(Vec<u8>);

impl Drop for PrivateBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Default)]
pub(super) struct ClaudeInputSlot {
    open: bool,
    closed: bool,
    request: Option<RequestId>,
    pending: Option<PrivateBytes>,
    consumed: bool,
}

impl ClaudeInputSlot {
    fn open(&mut self) {
        if !self.closed {
            self.open = true;
        }
    }

    fn reserve(&mut self, request: &RequestId, prompt: u64) -> Result<(), EngineError> {
        if !self.open || self.closed || self.request.is_some() || self.consumed || prompt != 1 {
            return Err(failure("native_auth_claude_input_not_pending"));
        }
        self.request = Some(request.clone());
        assert!(self.pending.is_none());
        Ok(())
    }

    fn submit(&mut self, request: &RequestId, secret: AccountSecret) -> Result<(), EngineError> {
        let bytes = PrivateBytes(secret.into_bytes());
        if !self.open
            || self.closed
            || self.consumed
            || self.request.as_ref() != Some(request)
            || self.pending.is_some()
        {
            return Err(failure("native_auth_claude_input_not_pending"));
        }
        self.pending = Some(bytes);
        assert!(self.pending.is_some());
        Ok(())
    }

    fn take(&mut self) -> Option<AccountSecret> {
        if self.closed || self.consumed {
            return None;
        }
        let mut bytes = self.pending.take()?;
        self.consumed = true;
        AccountSecret::new(std::mem::take(&mut bytes.0)).ok()
    }

    fn close(&mut self) {
        self.open = false;
        self.closed = true;
        self.pending.take();
        assert!(self.pending.is_none());
    }
}

pub(super) struct ClaudeProgress {
    fact: NativeAuthFact,
    challenge: Option<ClaudeAuthChallenge>,
    account: Option<crate::engine::owner::native_auth::presentation::ClaudeStatus>,
    input: ClaudeInputSlot,
}

impl Session {
    pub(super) fn validate_claude(&self, event: &ClaudeAuthObservation) -> Result<(), EngineError> {
        let owner = self.validate()?;
        let current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_ref()
            .filter(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
            })
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        if let ClaudeAuthObservation::Challenge { challenge } = event
            && let Some(previous) = slot
                .claude_progress
                .as_ref()
                .and_then(|progress| progress.challenge.as_ref())
            && (previous.url() != challenge.url()
                || (previous.accepts_code() && !challenge.accepts_code()))
        {
            return Err(failure("native_auth_observation_order"));
        }
        let mut fact = slot
            .claude_progress
            .as_ref()
            .map_or_else(|| slot.fact.clone(), |progress| progress.fact.clone());
        event.apply(&mut fact)
    }

    pub(super) fn record_claude(
        &self,
        event: &ClaudeAuthObservation,
        fact: NativeAuthFact,
    ) -> Result<(), EngineError> {
        let owner = self.validate()?;
        let mut current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .filter(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
            })
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        let mut progress = slot
            .claude_progress
            .take()
            .unwrap_or_else(|| ClaudeProgress {
                fact: fact.clone(),
                challenge: None,
                account: None,
                input: ClaudeInputSlot::default(),
            });
        progress.fact = fact;
        progress.challenge = match event {
            ClaudeAuthObservation::Challenge { challenge }
                if progress.fact.phase == NativeAuthPhase::AwaitingOwner =>
            {
                Some(challenge.clone())
            }
            _ => None,
        };
        progress.account = if matches!(
            progress.fact.phase,
            NativeAuthPhase::AuthenticatedObserved | NativeAuthPhase::UnauthenticatedObserved
        ) {
            event
                .account()?
                .as_ref()
                .map(crate::engine::owner::native_auth::presentation::ClaudeStatus::from_native)
        } else {
            None
        };
        if progress
            .challenge
            .as_ref()
            .is_some_and(ClaudeAuthChallenge::accepts_code)
        {
            progress.input.open();
        } else if progress.fact.phase != NativeAuthPhase::AwaitingOwner {
            progress.input.close();
        }
        slot.claude_progress = Some(progress);
        assert!(!slot.committed);
        Ok(())
    }

    pub(super) fn take_claude_code(
        &self,
        fact: &NativeAuthFact,
    ) -> Result<Option<PrivateBytes>, EngineError> {
        let owner = self.validate()?;
        let mut current = owner
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        let slot = current
            .as_mut()
            .filter(|slot| {
                slot.dispatch
                    .as_ref()
                    .is_some_and(|dispatch| Arc::ptr_eq(dispatch, &self.dispatch))
            })
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        let Some(progress) = slot.claude_progress.as_mut() else {
            return Ok(None);
        };
        if fact.phase != NativeAuthPhase::AwaitingOwner || progress.fact != *fact {
            progress.input.close();
            return Ok(None);
        }
        Ok(progress
            .input
            .take()
            .map(|secret| PrivateBytes(secret.into_bytes())))
    }
}

impl Authority {
    pub(super) fn claude_presentation(
        &self,
        slot: &NativeAuthSlot,
        reply: &mut NativeAuthReply,
    ) -> Result<bool, EngineError> {
        let Some(progress) = slot
            .claude_progress
            .as_ref()
            .filter(|progress| progress.fact == reply.fact)
        else {
            return Ok(false);
        };
        self.verify_native_auth_origin(
            slot.dispatch
                .as_ref()
                .ok_or_else(|| failure("native_auth_recovery_required"))?,
        )?;
        reply.claude_challenge = progress.challenge.clone();
        reply.account = progress
            .account
            .as_ref()
            .map(crate::engine::owner::native_auth::presentation::ClaudeStatus::materialize)
            .transpose()?
            .map(crate::engine::owner::NativeAuthAccount::Claude);
        crate::engine::owner::native_auth::presentation::encode(reply)?;
        Ok(true)
    }

    fn claude_input_slot<'a>(
        &self,
        current: &'a mut Option<NativeAuthSlot>,
        request: &ClaudeAuthInput,
        fact: &NativeAuthFact,
    ) -> Result<&'a mut ClaudeInputSlot, EngineError> {
        let slot = current
            .as_mut()
            .filter(|slot| {
                !slot.expired
                    && slot.attached
                    && slot.live.load(Ordering::SeqCst)
                    && Instant::now() < slot.deadline
                    && !slot.committed
                    && !slot.cleaning
            })
            .ok_or_else(|| failure("native_auth_claude_input_not_pending"))?;
        let dispatch = slot
            .dispatch
            .as_ref()
            .ok_or_else(|| failure("native_auth_recovery_required"))?;
        self.verify_native_auth_origin(dispatch)?;
        if request.generation != fact.generation
            || request.query.operation_id != fact.operation_id
            || fact.binding != slot.fact.binding
            || fact.operation_id != slot.fact.operation_id
            || fact.generation != slot.fact.generation
            || fact.phase != NativeAuthPhase::AwaitingOwner
            || fact.binding.connection.harness.as_str() != "claude"
            || fact.action
                != (NativeAuthAction::SignIn {
                    method: crate::engine::owner::NativeAuthMethod::ClaudeOfficial,
                })
        {
            return Err(failure("native_auth_claude_input_scope"));
        }
        let progress = slot
            .claude_progress
            .as_mut()
            .filter(|progress| {
                progress.fact == *fact
                    && progress
                        .challenge
                        .as_ref()
                        .is_some_and(ClaudeAuthChallenge::accepts_code)
            })
            .ok_or_else(|| failure("native_auth_claude_input_not_pending"))?;
        Ok(&mut progress.input)
    }
}

#[cfg(unix)]
impl NativeAuthCustodyChannel {
    pub(crate) async fn observe_claude(
        &mut self,
        guard: &NativeProfileUse,
        event: ClaudeAuthObservation,
    ) -> Result<NativeAuthCustodyControl, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.guard_or_close(guard)?;
        if let Some(account) = event.account()?
            && account.config_directory.as_str()
                != guard
                    .profile_path()?
                    .to_str()
                    .ok_or_else(|| failure("native_auth_claude_profile_invalid"))?
        {
            return Err(failure("native_auth_claude_profile_mismatch"));
        }
        let mut request = self.next_request(RequestKind::ObserveClaude, None)?;
        request.claude_observation = Some(event);
        let reply = self.exchange_request(request).await?;
        self.guard_or_close(guard)?;
        match reply.kind {
            ReplyKind::Continue => Ok(NativeAuthCustodyControl::Continue),
            ReplyKind::Cancel => Ok(NativeAuthCustodyControl::Cancel),
            ReplyKind::Retired => Err(failure("native_auth_reply_invalid")),
        }
    }

    pub(crate) async fn take_claude_input(
        &mut self,
        guard: &NativeProfileUse,
    ) -> Result<Option<AccountSecret>, EngineError> {
        if self.sealed {
            return Err(failure("native_auth_recovery_required"));
        }
        self.guard_or_close(guard)?;
        let reply = self.exchange(RequestKind::TakeClaudeInput, None).await?;
        let result = if reply.claude_input {
            if reply.kind != ReplyKind::Continue {
                return Err(failure("native_auth_reply_invalid"));
            }
            let stream = self
                .stream
                .as_mut()
                .ok_or_else(|| failure("native_auth_channel_lost"))?;
            tokio::time::timeout_at(
                self.deadline.min(Instant::now() + READ_BOUND),
                read_secret(stream),
            )
            .await
            .map_err(|_| failure("native_auth_channel_timeout"))
            .and_then(|result| result)
            .map(Some)
        } else {
            Ok(None)
        };
        if result.is_err() {
            self.close();
        }
        self.guard_or_close(guard)?;
        result.and_then(|bytes| {
            bytes
                .map(|mut bytes| AccountSecret::new(std::mem::take(&mut bytes.0)))
                .transpose()
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeAuthInput {
    pub query: NativeAuthQuery,
    pub generation: u64,
    pub prompt_id: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEnvelope {
    claude_auth_input: ClaudeAuthInput,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputReply {
    request_id: RequestId,
    operation_id: RequestId,
    generation: u64,
    database_instance: String,
    engine_epoch: String,
    accepted: bool,
}

async fn read_secret(stream: &mut UnixStream) -> Result<PrivateBytes, EngineError> {
    let mut size = [0; 4];
    stream
        .read_exact(&mut size)
        .await
        .map_err(|_| failure("native_auth_claude_input_invalid"))?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > 8192 {
        return Err(failure("native_auth_claude_input_invalid"));
    }
    let mut bytes = PrivateBytes(vec![0; size]);
    stream
        .read_exact(&mut bytes.0)
        .await
        .map_err(|_| failure("native_auth_claude_input_invalid"))?;
    if std::str::from_utf8(&bytes.0).is_err()
        || bytes.0.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
    {
        return Err(failure("native_auth_claude_input_invalid"));
    }
    Ok(bytes)
}

pub(super) async fn write_secret<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: PrivateBytes,
) -> Result<(), EngineError> {
    writer
        .write_all(&(bytes.0.len() as u32).to_be_bytes())
        .await
        .map_err(io_error)?;
    writer.write_all(&bytes.0).await.map_err(io_error)
}

impl OwnerClient {
    pub async fn submit_claude_auth_code(
        &self,
        request: ClaudeAuthInput,
        secret: AccountSecret,
    ) -> Result<(), EngineError> {
        let secret = PrivateBytes(secret.into_bytes());
        crate::engine::owner::native_auth::Command::Read(request.query.clone()).validate()?;
        tokio::time::timeout(self.timeout, async {
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(io_error)?;
            verify_peer(&stream)?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut packet = [0; 37];
            packet[..4].copy_from_slice(b"CAO1");
            packet[4] = match self.kind {
                OwnerClientKind::Tui => 0,
                OwnerClientKind::Desktop => 1,
            };
            packet[5..].copy_from_slice(&self.token);
            stream.write_all(&packet).await.map_err(io_error)?;
            packet.fill(0);
            stream
                .write_all(
                    &encode_frame(&InputEnvelope {
                        claude_auth_input: request.clone(),
                    })
                    .map_err(io_error)?,
                )
                .await
                .map_err(io_error)?;
            let reply = read_input_reply(&mut stream).await?;
            validate_input_reply(&reply, &request)?;
            if reply.accepted || self.root.socket_identity()? != self.socket {
                return Err(failure("native_auth_claude_input_invalid"));
            }
            write_secret(&mut stream, secret).await?;
            stream.shutdown().await.map_err(io_error)?;
            let committed = read_input_reply(&mut stream).await?;
            validate_input_reply(&committed, &request)?;
            if !committed.accepted
                || committed.engine_epoch != reply.engine_epoch
                || self.root.socket_identity()? != self.socket
            {
                return Err(failure("native_auth_claude_input_outcome_unknown"));
            }
            Ok(())
        })
        .await
        .map_err(|_| failure("native_auth_claude_input_outcome_unknown"))?
    }
}

async fn read_input_reply(stream: &mut UnixStream) -> Result<InputReply, EngineError> {
    let frame = read_frame(stream).await?;
    serde_json::from_slice(&frame[4..]).map_err(|_| failure("native_auth_claude_input_invalid"))
}

fn validate_input_reply(reply: &InputReply, request: &ClaudeAuthInput) -> Result<(), EngineError> {
    if reply.request_id != request.query.request_id
        || reply.operation_id != request.query.operation_id
        || reply.generation != request.generation
        || reply.database_instance != request.query.scope.expected_database_instance
        || !crate::engine::owner::is_hex_id(&reply.engine_epoch)
    {
        return Err(failure("native_auth_claude_input_scope"));
    }
    Ok(())
}

pub(in crate::engine::owner::transport) async fn serve_input(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    if bytes.len() > crate::engine::owner::native_auth::REQUEST_BYTES_MAX {
        return Err(failure("native_auth_request_bounds"));
    }
    let envelope: InputEnvelope =
        serde_json::from_slice(bytes).map_err(|_| failure("native_auth_claude_input_invalid"))?;
    let request = envelope.claude_auth_input;
    let host = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    let read = crate::engine::owner::native_auth::Command::Read(request.query.clone());
    read.validate()?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let fact = host
        .native_auth_command(admission.clone(), read.clone())
        .await?
        .fact;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
        let mut current = authority
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        authority
            .claude_input_slot(&mut current, &request, &fact)?
            .reserve(&request.query.request_id, request.prompt_id)?;
    }
    let mut reply = InputReply {
        request_id: request.query.request_id.clone(),
        operation_id: request.query.operation_id.clone(),
        generation: request.generation,
        database_instance: fact.binding.database_instance.clone(),
        engine_epoch: fact.binding.engine_epoch.clone(),
        accepted: false,
    };
    stream
        .write_all(&encode_frame(&reply).map_err(io_error)?)
        .await
        .map_err(io_error)?;
    let mut secret = read_secret(stream).await?;
    let mut extra = [0];
    if stream.read(&mut extra).await.map_err(io_error)? != 0 {
        return Err(failure("native_auth_claude_input_invalid"));
    }
    let fact = host
        .native_auth_command(admission.clone(), read)
        .await?
        .fact;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
        let mut current = authority
            .native_auth
            .lock()
            .map_err(|_| failure("native_auth_recovery_required"))?;
        authority
            .claude_input_slot(&mut current, &request, &fact)?
            .submit(
                &request.query.request_id,
                AccountSecret::new(std::mem::take(&mut secret.0))?,
            )?;
    }
    reply.accepted = true;
    stream
        .write_all(&encode_frame(&reply).map_err(io_error)?)
        .await
        .map_err(io_error)
}
