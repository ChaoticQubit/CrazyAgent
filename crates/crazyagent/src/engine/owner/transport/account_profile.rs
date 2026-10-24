use super::*;
use crate::config::ModelAuthentication;
use crate::engine::owner::account_profile::*;
use crate::storage::account_profile::{Command, ResultValue};
use serde::{Deserialize, Serialize};

mod writer;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Configure,
    Read,
    Capabilities,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    account_profile: Action,
    request: AccountProfileRequest,
    secret_present: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireReply {
    AcceptSecret {
        request_id: RequestId,
        database_instance: String,
        engine_epoch: String,
    },
    Profile {
        reply: AccountProfileReply,
        settled: bool,
    },
    Capabilities {
        reply: Box<AccountAuthCapabilitiesReply>,
    },
    Rejected {
        request_id: RequestId,
        code: String,
    },
}

pub(crate) struct ClientFailure {
    pub(crate) error: EngineError,
    pub(crate) may_have_effects: bool,
}

pub(crate) struct ClientProfileReply {
    pub(crate) reply: AccountProfileReply,
    pub(crate) settled: bool,
}

impl OwnerClient {
    pub async fn configure_account_profile(
        &self,
        request: AccountProfileRequest,
        secret: Option<AccountSecret>,
    ) -> Result<AccountProfileReply, EngineError> {
        self.configure_account_profile_private(request, secret)
            .await
            .map(|result| result.reply)
            .map_err(|failure| failure.error)
    }

    pub async fn read_account_profile(
        &self,
        request: AccountProfileRequest,
    ) -> Result<AccountProfileReply, EngineError> {
        self.read_account_profile_private(request)
            .await
            .map(|result| result.reply)
    }

    pub async fn read_account_auth_capabilities(
        &self,
        request: AccountProfileRequest,
    ) -> Result<AccountAuthCapabilitiesReply, EngineError> {
        match self
            .account_profile_exchange(Action::Capabilities, request, None)
            .await
            .map_err(|failure| failure.error)?
        {
            WireReply::Capabilities { reply } => Ok(*reply),
            _ => Err(unknown()),
        }
    }

    pub(crate) async fn configure_account_profile_private(
        &self,
        request: AccountProfileRequest,
        secret: Option<AccountSecret>,
    ) -> Result<ClientProfileReply, ClientFailure> {
        match self
            .account_profile_exchange(Action::Configure, request, secret)
            .await?
        {
            WireReply::Profile { reply, settled } => Ok(ClientProfileReply { reply, settled }),
            _ => Err(ClientFailure {
                error: unknown(),
                may_have_effects: true,
            }),
        }
    }

    pub(crate) async fn read_account_profile_private(
        &self,
        request: AccountProfileRequest,
    ) -> Result<ClientProfileReply, EngineError> {
        match self
            .account_profile_exchange(Action::Read, request, None)
            .await
            .map_err(|failure| failure.error)?
        {
            WireReply::Profile { reply, settled } => Ok(ClientProfileReply { reply, settled }),
            _ => Err(unknown()),
        }
    }

    async fn account_profile_exchange(
        &self,
        action: Action,
        request: AccountProfileRequest,
        secret: Option<AccountSecret>,
    ) -> Result<WireReply, ClientFailure> {
        let mut committed = false;
        let result = tokio::time::timeout(self.timeout, async {
            let envelope = Envelope {
                account_profile: action,
                request: request.clone(),
                secret_present: secret.is_some(),
            };
            let frame =
                encode_frame(&envelope).map_err(|_| failure("account_profile_request_bounds"))?;
            if frame.len() > 8196 {
                return Err(failure("account_profile_request_bounds"));
            }
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = UnixStream::connect(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("account_profile_owner_unavailable"))?;
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
            stream
                .write_all(&packet)
                .await
                .map_err(|_| failure("account_profile_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("account_profile_owner_unavailable"))?;
            let first = read_wire(&mut stream).await?;
            let result = if let WireReply::AcceptSecret {
                request_id,
                database_instance,
                engine_epoch,
            } = first
            {
                if !matches!(action, Action::Configure)
                    || request_id != request.request_id
                    || database_instance != request.scope.expected_database_instance
                    || !hex_id(&engine_epoch)
                {
                    return Err(unknown());
                }
                if self.root.socket_identity().ok() != Some(self.socket) {
                    return Err(unknown());
                }
                let mut bytes = secret.map(AccountSecret::into_bytes).unwrap_or_default();
                committed = true;
                let write = async {
                    stream
                        .write_all(&(bytes.len() as u32).to_be_bytes())
                        .await?;
                    stream.write_all(&bytes).await?;
                    stream.shutdown().await
                }
                .await;
                bytes.fill(0);
                write.map_err(|_| unknown())?;
                let result = read_wire(&mut stream).await?;
                if reply_epoch(&result).is_some_and(|value| value != engine_epoch) {
                    return Err(unknown());
                }
                result
            } else {
                first
            };
            if self.root.socket_identity().ok() != Some(self.socket) {
                return Err(unknown());
            }
            validate_wire(&result, &request)?;
            match result {
                WireReply::Rejected { code, .. } => Err(EngineError(code)),
                WireReply::AcceptSecret { .. } => Err(unknown()),
                result => Ok(result),
            }
        })
        .await;
        match result {
            Ok(result) => result.map_err(|error| ClientFailure {
                error,
                may_have_effects: committed,
            }),
            Err(_) => Err(ClientFailure {
                error: unknown(),
                may_have_effects: committed,
            }),
        }
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
    if bytes.len() > 8192 {
        return Err(failure("account_profile_request_bounds"));
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| failure("account_profile_request_invalid"))?;
    let request = envelope.request;
    let result = async {
        authenticate(engine, authority, packet, admission).await?;
        match envelope.account_profile {
            Action::Configure => {
                let saved = if envelope.secret_present {
                    None
                } else {
                    saved_key(engine, authority, admission, &request).await?
                };
                let secret_present = envelope.secret_present || saved.is_some();
                let reserved = actor(
                    engine,
                    admission,
                    Command::Reserve(request.clone(), secret_present),
                )
                .await?;
                if let ResultValue::Profile(reply, settled) = reserved {
                    return Ok(WireReply::Profile { reply, settled });
                }
                if !matches!(reserved, ResultValue::Accepted) {
                    return Err(unknown());
                }
                if secret_present {
                    let runtime = OwnerRuntimeHandle {
                        engine: engine.clone(),
                        authority: authority.clone(),
                    };
                    if runtime.registered_rcli_private_key_executable().is_err() {
                        let _ = actor(engine, admission, Command::Abandon(request.clone())).await;
                        return Err(failure("rcli_private_key_writer_unavailable"));
                    }
                }
                let accepted = WireReply::AcceptSecret {
                    request_id: request.request_id.clone(),
                    database_instance: admission.db_instance.clone(),
                    engine_epoch: admission.engine_epoch.clone(),
                };
                authenticate(engine, authority, packet, admission).await?;
                let secret = async {
                    write_wire(stream, &accepted).await?;
                    receive_secret(stream, envelope.secret_present).await
                }
                .await;
                let secret = match secret {
                    Ok(secret) => secret.or(saved),
                    Err(error) => {
                        let _ = actor(engine, admission, Command::Abandon(request.clone())).await;
                        return Err(error);
                    }
                };
                authenticate(engine, authority, packet, admission).await?;
                let engine = engine.clone();
                let authority = authority.clone();
                let packet = *packet;
                let admission = admission.clone();
                let request = request.clone();
                tokio::spawn(async move {
                    let begun = actor(&engine, &admission, Command::Apply(request, secret)).await?;
                    match begun {
                        ResultValue::Work(work) => {
                            let runtime = OwnerRuntimeHandle {
                                engine: engine.clone(),
                                authority: authority.clone(),
                            };
                            let (work, succeeded) = if work.secret.is_some() {
                                writer::run(&runtime, work).await?
                            } else {
                                let succeeded = work.guard.revalidate_retained().is_ok();
                                (work, succeeded)
                            };
                            authenticate(&engine, &authority, &packet, &admission).await?;
                            actor(&engine, &admission, Command::Finish(work, succeeded)).await
                        }
                        value => Ok(value),
                    }
                })
                .await
                .map_err(|_| unknown())?
                .and_then(profile_wire)
            }
            Action::Read => {
                if envelope.secret_present {
                    return Err(failure("account_profile_request_invalid"));
                }
                profile_wire(actor(engine, admission, Command::Read(request.clone())).await?)
            }
            Action::Capabilities => {
                if envelope.secret_present {
                    return Err(failure("account_profile_request_invalid"));
                }
                let ResultValue::Capabilities(mut reply) =
                    actor(engine, admission, Command::Capabilities(request.clone())).await?
                else {
                    return Err(unknown());
                };
                let runtime = OwnerRuntimeHandle {
                    engine: engine.clone(),
                    authority: authority.clone(),
                };
                reply.configured = configured_capability(&runtime, &reply.connection, &request);
                if matches!(
                    reply.connection.auth,
                    ModelAuthentication::NativeManaged { .. }
                ) {
                    let capability = match reply.connection.harness.as_str() {
                        "codex" => {
                            let producer = if runtime.registered_codex_executable().is_ok() {
                                ProducerCapability::Available
                            } else {
                                unavailable("native_auth_host_unavailable")
                            };
                            vec![
                                ManagedAuthCapability {
                                    method: NativeAuthMethod::CodexBrowser,
                                    producer: producer.clone(),
                                },
                                ManagedAuthCapability {
                                    method: NativeAuthMethod::CodexDeviceCode,
                                    producer: producer.clone(),
                                },
                                ManagedAuthCapability {
                                    method: NativeAuthMethod::CodexExisting,
                                    producer,
                                },
                            ]
                        }
                        "claude" => vec![ManagedAuthCapability {
                            method: NativeAuthMethod::ClaudeOfficial,
                            producer: if runtime.registered_claude_executable().is_ok() {
                                ProducerCapability::Available
                            } else {
                                unavailable("claude_official_producer_unavailable")
                            },
                        }],
                        _ => Vec::new(),
                    };
                    reply.managed = capability;
                }
                Ok(WireReply::Capabilities { reply })
            }
        }
    }
    .await;
    authenticate(engine, authority, packet, admission).await?;
    let reply = match result {
        Ok(reply) => reply,
        Err(error) => WireReply::Rejected {
            request_id: request.request_id,
            code: public_code(error),
        },
    };
    write_wire(stream, &reply).await
}

/// Finds the key saved with a registry account when a client configures its harness profile
/// without sending one, so the key is read only from the engine's account key store.
/// Inputs: engine, authority, admission and request. Output: the saved key, if any.
async fn saved_key(
    engine: &Engine,
    authority: &Authority,
    admission: &Admission,
    request: &AccountProfileRequest,
) -> Result<Option<AccountSecret>, EngineError> {
    let Ok(ResultValue::Capabilities(reply)) =
        actor(engine, admission, Command::Capabilities(request.clone())).await
    else {
        return Ok(None);
    };
    match &reply.connection.auth {
        ModelAuthentication::ApiKey { credential_ref } => {
            super::accounts::saved_key(&authority.root, credential_ref)
        }
        _ => Ok(None),
    }
}

async fn actor(
    engine: &Engine,
    admission: &Admission,
    command: Command,
) -> Result<ResultValue, EngineError> {
    engine
        .native_auth_job(|reply| Job::AccountProfile(admission.clone(), command, reply))
        .await
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

fn profile_wire(value: ResultValue) -> Result<WireReply, EngineError> {
    match value {
        ResultValue::Profile(reply, settled) => Ok(WireReply::Profile { reply, settled }),
        _ => Err(unknown()),
    }
}

async fn receive_secret(
    stream: &mut UnixStream,
    present: bool,
) -> Result<Option<AccountSecret>, EngineError> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut size = [0; 4];
        stream
            .read_exact(&mut size)
            .await
            .map_err(|_| failure("account_profile_secret_frame_invalid"))?;
        let size = u32::from_be_bytes(size) as usize;
        if size > 8192 || (present && size == 0) || (!present && size != 0) {
            return Err(failure("account_profile_secret_frame_invalid"));
        }
        let mut bytes = vec![0; size];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| failure("account_profile_secret_frame_invalid"))?;
        let mut extra = [0; 1];
        if stream
            .read(&mut extra)
            .await
            .map_err(|_| failure("account_profile_secret_frame_invalid"))?
            != 0
        {
            return Err(failure("account_profile_secret_frame_invalid"));
        }
        if present {
            AccountSecret::new(bytes).map(Some)
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|_| failure("account_profile_secret_frame_invalid"))?
}

async fn write_wire(stream: &mut UnixStream, reply: &WireReply) -> Result<(), EngineError> {
    stream
        .write_all(&encode_frame(reply).map_err(|_| failure("account_profile_response_bounds"))?)
        .await
        .map_err(io_error)
}

async fn read_wire(stream: &mut UnixStream) -> Result<WireReply, EngineError> {
    let frame = read_frame(stream).await.map_err(|_| unknown())?;
    serde_json::from_slice(&frame[4..]).map_err(|_| unknown())
}

fn reply_epoch(reply: &WireReply) -> Option<&str> {
    match reply {
        WireReply::Profile { reply, .. } => Some(&reply.engine_epoch),
        WireReply::Capabilities { reply } => Some(&reply.engine_epoch),
        _ => None,
    }
}

fn validate_wire(reply: &WireReply, request: &AccountProfileRequest) -> Result<(), EngineError> {
    let valid = match reply {
        WireReply::Profile { reply, .. } => {
            let identity = reply.request_id == request.request_id
                && reply.database_instance == request.scope.expected_database_instance
                && hex_id(&reply.engine_epoch);
            identity
                && match &reply.outcome {
                    AccountProfileOutcome::NotRecorded => true,
                    AccountProfileOutcome::ConfiguredOnly { receipt, .. } => {
                        receipt.config_id == request.scope.expected_config_id
                            && receipt.source_hash == request.scope.expected_source_hash
                            && receipt.connection.id == request.scope.connection
                            && hex_id(&receipt.custody_id)
                            && hex_id(&receipt.profile_config_sha256)
                            && receipt.profile_generation > 0
                    }
                }
        }
        WireReply::Capabilities { reply } => {
            reply.request_id == request.request_id
                && reply.database_instance == request.scope.expected_database_instance
                && hex_id(&reply.engine_epoch)
                && reply.config_id == request.scope.expected_config_id
                && reply.source_hash == request.scope.expected_source_hash
                && reply.connection.id == request.scope.connection
        }
        WireReply::Rejected { request_id, code } => {
            request_id == &request.request_id
                && code.len() <= 96
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }
        WireReply::AcceptSecret { .. } => false,
    };
    if valid { Ok(()) } else { Err(unknown()) }
}

fn configured_capability(
    runtime: &OwnerRuntimeHandle,
    connection: &crate::config::ModelConnection,
    request: &AccountProfileRequest,
) -> ProducerCapability {
    if let Err(error) = authorize_account_profile_scope(
        &request.scope,
        &request.scope.expected_database_instance,
        &request.scope.expected_config_id,
        &request.scope.expected_source_hash,
        connection,
    ) {
        return unavailable(&error.0);
    }
    match connection.auth {
        ModelAuthentication::None {} => ProducerCapability::Available,
        ModelAuthentication::ApiKey { .. } => {
            if runtime.registered_rcli_private_key_executable().is_ok() {
                ProducerCapability::Available
            } else {
                unavailable("rcli_private_key_writer_unavailable")
            }
        }
        _ => unavailable("account_profile_auth_unsupported"),
    }
}

fn unavailable(code: &str) -> ProducerCapability {
    ProducerCapability::Unavailable { code: code.into() }
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
        "account_profile_outcome_unknown".into()
    }
}

fn unknown() -> EngineError {
    failure("account_profile_outcome_unknown")
}

fn hex_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
