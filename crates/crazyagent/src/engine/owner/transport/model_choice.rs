use super::*;
use crate::engine::native_auth_host::RegisteredNativeAuthHost;
use crate::harness_settings::{Scope, installed};
use crate::model_choice::rules::{self, Rule, RuleWritten, Rules};
use crate::model_choice::store::{self, Begun};
use crate::model_choice::test_launch::{self, Tested};
use crate::model_choice::{MODEL_BYTES_MAX, Models, Row, Slot, VALUE_JSON_MAX, Written};
use crate::protocol::encode_frame;
use crate::storage::versioned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub(super) const MODEL_CHOICE_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a model choice request may take on either side: a test launch's run bound
/// and the time to pick its account, record its decision and reply (DEC-150 addendum 7).
pub(super) const MODEL_CHOICE_TEST_TIMEOUT: Duration =
    Duration::from_secs(test_launch::WALL_TIME.as_secs() + 30);

const _: () = assert!(MODEL_CHOICE_TEST_TIMEOUT.as_secs() > MODEL_CHOICE_TIMEOUT.as_secs());

const _: () = assert!(VALUE_JSON_MAX + MODEL_BYTES_MAX + 1024 <= REQUEST_BYTES_MAX);
const _: () = assert!(REQUEST_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX);

/// Every code a model choice request can answer an owner client with: the store's
/// refusals, this protocol's own, and the owner connection's it travels on.
pub const MODEL_CHOICE_PROTOCOL_CODES: &[&str] = &[
    "model_choice_custom_full",
    "model_choice_custom_not_pending",
    "model_choice_harness_unknown",
    "model_choice_not_supported",
    "model_choice_outcome_unknown",
    "model_choice_pool_needed",
    "model_choice_pool_unknown",
    "model_choice_project_unknown",
    "model_choice_provider_needed",
    "model_choice_provider_unknown",
    "model_choice_reply_bounds",
    "model_choice_request_bounds",
    "model_choice_request_invalid",
    "model_choice_test_busy",
    "model_choice_unavailable",
    "model_choice_value_invalid",
    "owner_authentication_failed",
    "owner_authority_unavailable",
    "owner_busy",
    "owner_grant_stale_or_revoked",
    "owner_request_failed",
    "owner_root_replaced_or_unsafe",
    "owner_root_unavailable",
    "owner_socket_replaced",
    "owner_socket_unsafe",
    "peer_owner_mismatch",
    "versioned_item_invalid",
    "versioned_unavailable",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Values {
        harness: String,
        project: Option<ProjectId>,
    },
    Set {
        harness: String,
        project: Option<ProjectId>,
        slot: Slot,
        value: Value,
        version: u64,
    },
    Reset {
        harness: String,
        project: Option<ProjectId>,
        slot: Slot,
        version: u64,
    },
    Add {
        harness: String,
        model: String,
        #[serde(default)]
        provider: Option<String>,
    },
    Remove {
        harness: String,
        model: String,
    },
    Test {
        harness: String,
        model: String,
        pool: Option<CredentialRef>,
    },
    Rules {},
    SetRule {
        rule: Rule,
        version: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    model_choice: Action,
}

/// What adding an owner's model id did, as a client shows it: it waits for its test
/// launch, the harness's catalog already lists it, or it already passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Added {
    Pending,
    Listed,
    Verified,
}

impl From<Begun> for Added {
    fn from(begun: Begun) -> Self {
        match begun {
            Begun::Pending => Self::Pending,
            Begun::Listed => Self::Listed,
            Begun::Verified => Self::Verified,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Values {
        models: Models,
    },
    Saved {
        row: Row,
    },
    Stale {
        row: Row,
    },
    Refused {
        code: String,
        row: Row,
    },
    Added {
        added: Added,
        models: Models,
    },
    Removed {
        removed: bool,
        models: Models,
    },
    Tested {
        tested: Tested,
        reason: Option<String>,
        models: Models,
    },
    Rules {
        rules: Rules,
    },
    RuleSaved {
        rules: Rules,
    },
    RuleStale {
        rules: Rules,
    },
    Rejected {
        code: String,
    },
}

impl From<Written> for Reply {
    fn from(written: Written) -> Self {
        match written {
            Written::Saved { row } => Self::Saved { row },
            Written::Stale { row } => Self::Stale { row },
            Written::Refused { code, row } => Self::Refused { code, row },
        }
    }
}

fn unknown() -> EngineError {
    failure("model_choice_outcome_unknown")
}

fn scope(project: Option<ProjectId>) -> Scope {
    project.map_or(Scope::App, Scope::Project)
}

impl OwnerClient {
    /// Reads a harness's Models section app-wide or for one project (DEC-150): the master,
    /// agent and allowed slots with their versions, and every candidate id. Inputs: the
    /// harness and the project, none for the app. Output: the section, or a
    /// `MODEL_CHOICE_PROTOCOL_CODES` refusal.
    pub async fn model_choice_values(
        &self,
        harness: &str,
        project: Option<ProjectId>,
    ) -> Result<Models, EngineError> {
        let action = Action::Values {
            harness: harness.into(),
            project,
        };
        match self.model_choice_exchange(action).await? {
            Reply::Values { models } => Ok(models),
            _ => Err(unknown()),
        }
    }

    /// Sets one slot app-wide or as a project override, naming the version the owner read;
    /// agents and the master use it from their next start. Inputs: the harness, the
    /// project, the slot, the value and the version. Output: Saved, Stale or Refused with
    /// the slot's row, or a refusal.
    pub async fn model_choice_set(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        slot: Slot,
        value: Value,
        version: u64,
    ) -> Result<Written, EngineError> {
        let action = Action::Set {
            harness: harness.into(),
            project,
            slot,
            value,
            version,
        };
        self.model_choice_written(action).await
    }

    /// Resets one slot at one scope, naming the version the owner read. Inputs: as
    /// `model_choice_set`, without a value. Output: as `model_choice_set`.
    pub async fn model_choice_reset(
        &self,
        harness: &str,
        project: Option<ProjectId>,
        slot: Slot,
        version: u64,
    ) -> Result<Written, EngineError> {
        let action = Action::Reset {
            harness: harness.into(),
            project,
            slot,
            version,
        };
        self.model_choice_written(action).await
    }

    /// Adds a model id the owner typed for a harness; it waits for its test launch before
    /// it can be chosen. Inputs: the harness and the id. Output: what was done with the
    /// harness's app-wide section, or a refusal.
    pub async fn model_choice_add(
        &self,
        harness: &str,
        model: &str,
    ) -> Result<(Added, Models), EngineError> {
        let action = Action::Add {
            harness: harness.into(),
            model: model.into(),
        };
        match self.model_choice_exchange(action).await? {
            Reply::Added { added, models } => Ok((added, models)),
            _ => Err(unknown()),
        }
    }

    /// Removes a model id the owner added for a harness. Inputs: the harness and the id.
    /// Output: whether it was there, with the harness's app-wide section, or a refusal.
    pub async fn model_choice_remove(
        &self,
        harness: &str,
        model: &str,
    ) -> Result<(bool, Models), EngineError> {
        let action = Action::Remove {
            harness: harness.into(),
            model: model.into(),
        };
        match self.model_choice_exchange(action).await? {
            Reply::Removed { removed, models } => Ok((removed, models)),
            _ => Err(unknown()),
        }
    }

    /// Runs the test launch of one pending owner id (DEC-150 addendum 7): one tools-off run
    /// of a fixed prompt on an account of the pool the owner picked, with the id as its
    /// model. An answer verifies it, the provider's refusal code fails it, anything else
    /// leaves it pending with the reason. Waits up to `MODEL_CHOICE_TEST_TIMEOUT`. Inputs:
    /// the harness, the id and the pool. Output: the decision, its pending reason and the
    /// harness's app-wide section, or a refusal.
    pub async fn model_choice_test(
        &self,
        harness: &str,
        model: &str,
        pool: CredentialRef,
    ) -> Result<(Tested, Option<String>, Models), EngineError> {
        let action = Action::Test {
            harness: harness.into(),
            model: model.into(),
            pool: Some(pool),
        };
        match self
            .model_choice_send(action, MODEL_CHOICE_TEST_TIMEOUT)
            .await?
        {
            Reply::Tested {
                tested,
                reason,
                models,
            } if (tested == Tested::Pending) == reason.is_some()
                && reason
                    .as_ref()
                    .is_none_or(|reason| reason.len() <= test_launch::REASON_BYTES_MAX) =>
            {
                Ok((tested, reason, models))
            }
            _ => Err(unknown()),
        }
    }

    /// Reads the owner's two model rules (DEC-150 Q2): how a master's model pick is
    /// treated and what a start does when no pool account can run its model, each with
    /// its version. Output: the rules, or a `MODEL_CHOICE_PROTOCOL_CODES` refusal.
    pub async fn model_choice_rules(&self) -> Result<Rules, EngineError> {
        match self.model_choice_exchange(Action::Rules {}).await? {
            Reply::Rules { rules } => Ok(rules),
            _ => Err(unknown()),
        }
    }

    /// Sets one model rule, naming the version the owner read; it applies from the next
    /// pick or start. Inputs: the rule with its value and the version. Output: the saved
    /// or stale rules, or a refusal.
    pub async fn model_choice_set_rule(
        &self,
        rule: Rule,
        version: u64,
    ) -> Result<RuleWritten, EngineError> {
        match self
            .model_choice_exchange(Action::SetRule { rule, version })
            .await?
        {
            Reply::RuleSaved { rules } => Ok(RuleWritten::Saved { rules }),
            Reply::RuleStale { rules } => Ok(RuleWritten::Stale { rules }),
            _ => Err(unknown()),
        }
    }

    async fn model_choice_written(&self, action: Action) -> Result<Written, EngineError> {
        match self.model_choice_exchange(action).await? {
            Reply::Saved { row } => Ok(Written::Saved { row }),
            Reply::Stale { row } => Ok(Written::Stale { row }),
            Reply::Refused { code, row } => Ok(Written::Refused { code, row }),
            _ => Err(unknown()),
        }
    }

    async fn model_choice_exchange(&self, action: Action) -> Result<Reply, EngineError> {
        self.model_choice_send(action, MODEL_CHOICE_TIMEOUT).await
    }

    async fn model_choice_send(
        &self,
        action: Action,
        limit: Duration,
    ) -> Result<Reply, EngineError> {
        tokio::time::timeout(self.timeout.max(limit), async {
            let frame = encode_frame(&Envelope {
                model_choice: action,
            })
            .map_err(|_| failure("model_choice_request_bounds"))?;
            if frame.len() > REQUEST_BYTES_MAX + 4 {
                return Err(failure("model_choice_request_bounds"));
            }
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("model_choice_unavailable"))?;
            verify_peer(&stream)?;
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
                .map_err(|_| failure("model_choice_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("model_choice_unavailable"))?;
            let frame = read_frame(&mut stream).await.map_err(|_| unknown())?;
            match serde_json::from_slice(&frame[4..]).map_err(|_| unknown())? {
                Reply::Rejected { code } => Err(EngineError(code)),
                reply => Ok(reply),
            }
        })
        .await
        .map_err(|_| unknown())?
    }
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&Reply::Rejected { code: code.into() }).map_err(|_| unknown())
}

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let reply = match handle(engine, authority, packet, admission, bytes).await {
        Ok(reply) => reply,
        Err(error) => Reply::Rejected { code: error.0 },
    };
    let frame = match encode_frame(&reply) {
        Ok(frame) => frame,
        Err(_) => encode_frame(&Reply::Rejected {
            code: "model_choice_reply_bounds".into(),
        })
        .map_err(|_| unknown())?,
    };
    stream.write_all(&frame).await.map_err(io_error)
}

async fn handle(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<Reply, EngineError> {
    let action = parse(bytes)?;
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    let root = super::changes::engine_root(authority)?;
    let executable = match &action {
        Action::Values { harness, .. }
        | Action::Add { harness, .. }
        | Action::Remove { harness, .. }
        | Action::Test { harness, .. } => crate::model_choice::harness_id(harness)
            .and_then(|id| super::harness_settings::registered(engine, authority, id)),
        _ => None,
    };
    let host = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    let harnesses = match &action {
        Action::Test { .. } => {
            let (codex, claude) = authority
                .native_auth_host
                .lock()
                .map_err(|_| failure("model_choice_unavailable"))?
                .model_harnesses();
            Some((engine.settings_files(), codex, claude))
        }
        _ => None,
    };
    tokio::task::spawn_blocking(move || {
        let installed =
            executable.and_then(|path| installed::version_cached(&path, installed::LIMIT));
        match action {
            Action::Test {
                harness,
                model,
                pool,
            } => {
                let (settings, codex, claude) =
                    harnesses.ok_or_else(|| failure("model_choice_unavailable"))?;
                let launch: Launch<'_> = Box::new(|pool, provider| {
                    let (global, local) = settings.as_ref().ok_or("model_choice_unavailable")?;
                    if harness == "rcli" {
                        let provider = provider.ok_or("model_choice_provider_needed")?;
                        return super::master_rcli::model_test::run(
                            &host,
                            &root,
                            (global, local),
                            &model,
                            provider,
                            pool,
                        );
                    }
                    let path = |host: &Option<Arc<RegisteredNativeAuthHost>>| match host {
                        Some(host) => host.verified_path().map(|path| Some(path.to_path_buf())),
                        None => Ok(None),
                    };
                    let codex = path(&codex).map_err(|_| "model_choice_unavailable")?;
                    let claude = path(&claude).map_err(|_| "model_choice_unavailable")?;
                    test_launch::run(&test_launch::Request {
                        root: &root,
                        settings: (global, local),
                        codex: codex.as_deref(),
                        claude: claude.as_deref(),
                        harness: &harness,
                        model: &model,
                        pool,
                    })
                });
                tested(
                    &root,
                    &harness,
                    &model,
                    pool.as_ref(),
                    installed.as_deref(),
                    launch,
                )
            }
            action => run(&root, action, installed.as_deref()),
        }
    })
    .await
    .map_err(|_| failure("model_choice_unavailable"))?
}

/// Reads one model choice request from an owner frame body, at most `REQUEST_BYTES_MAX`
/// bytes. Input: the body. Output: the action, or `model_choice_request_bounds` or
/// `model_choice_request_invalid`.
fn parse(bytes: &[u8]) -> Result<Action, EngineError> {
    if bytes.len() > REQUEST_BYTES_MAX {
        return Err(failure("model_choice_request_bounds"));
    }
    let Envelope { model_choice } =
        serde_json::from_slice(bytes).map_err(|_| failure("model_choice_request_invalid"))?;
    Ok(model_choice)
}

/// Runs one authenticated model choice action against the engine database in the engine
/// root, with the harness's model catalog. Inputs: the engine root, the action and the
/// harness's installed version, none when unknown or not read. Output: the reply, or the
/// store's refusal.
fn run(root: &Path, action: Action, installed: Option<&str>) -> Result<Reply, EngineError> {
    let db = versioned::open(root)?;
    let app = |harness: &str| -> Result<Models, EngineError> {
        store::values(
            &db,
            harness,
            &crate::model_choice::catalog(harness),
            &Scope::App,
            installed,
        )
    };
    match action {
        Action::Values { harness, project } => Ok(Reply::Values {
            models: store::values(
                &db,
                &harness,
                &crate::model_choice::catalog(&harness),
                &scope(project),
                installed,
            )?,
        }),
        Action::Set {
            harness,
            project,
            slot,
            value,
            version,
        } => Ok(store::set(
            &db,
            &harness,
            &crate::model_choice::catalog(&harness),
            &scope(project),
            slot,
            &value,
            version,
        )?
        .into()),
        Action::Reset {
            harness,
            project,
            slot,
            version,
        } => Ok(store::reset(
            &db,
            &harness,
            &crate::model_choice::catalog(&harness),
            &scope(project),
            slot,
            version,
        )?
        .into()),
        Action::Add {
            harness,
            model,
            provider,
        } => {
            let begun = store::custom_begin(
                &db,
                &harness,
                &crate::model_choice::catalog(&harness),
                &model,
                provider.as_deref(),
            )?;
            Ok(Reply::Added {
                added: begun.into(),
                models: app(&harness)?,
            })
        }
        Action::Remove { harness, model } => {
            let removed = store::custom_remove(&db, &harness, &model)?;
            Ok(Reply::Removed {
                removed,
                models: app(&harness)?,
            })
        }
        Action::Rules {} => Ok(Reply::Rules {
            rules: rules::rules(&db)?,
        }),
        Action::SetRule { rule, version } => Ok(match rules::set_rule(&db, rule, version)? {
            RuleWritten::Saved { rules } => Reply::RuleSaved { rules },
            RuleWritten::Stale { rules } => Reply::RuleStale { rules },
        }),
        Action::Test { .. } => Err(failure("model_choice_unavailable")),
    }
}

/// The launch a test hands its checked pool and, for an RCLI id, the provider it is tested
/// on: one run on that pool's account, answering the decision and its pending reason, or a
/// plain refusal code.
type Launch<'a> = Box<
    dyn FnOnce(&CredentialRef, Option<&str>) -> Result<(Tested, Option<String>), &'static str> + 'a,
>;

/// Runs one owner id's test launch once it may run (DEC-150 addendum 7): a supported
/// harness, a named pool and an id that waits for its test, so no run is spent otherwise.
/// A verified or failed decision is recorded; a pending one leaves the id as it was.
/// Inputs: the engine root, the harness, the id, the pool, the installed version and the
/// launch. Output: the decision, its reason and the harness's app section, or a refusal.
fn tested(
    root: &Path,
    harness: &str,
    model: &str,
    pool: Option<&CredentialRef>,
    installed: Option<&str>,
    launch: Launch<'_>,
) -> Result<Reply, EngineError> {
    if !test_launch::supported(harness) {
        return Err(failure("model_choice_not_supported"));
    }
    let Some(pool) = pool else {
        return Err(failure(test_launch::POOL_NEEDED));
    };
    let db = versioned::open(root)?;
    if !store::custom_pending(&db, harness, model)? {
        return Err(failure("model_choice_custom_not_pending"));
    }
    let provider = store::pending_provider(&db, harness, model)?;
    if (harness == "rcli") != provider.is_some() {
        return Err(failure("model_choice_provider_needed"));
    }
    let (mut tested, mut reason) = launch(pool, provider.as_deref()).map_err(failure)?;
    if tested != Tested::Pending {
        match store::custom_tested(
            &db,
            harness,
            model,
            provider.as_deref(),
            tested == Tested::Verified,
        ) {
            Ok(()) => {}
            Err(error) if error.0 == "model_choice_custom_not_pending" => {
                tested = Tested::Pending;
                reason =
                    Some("The id changed while it was tested; add it again to test it.".to_owned());
            }
            Err(error) => return Err(error),
        }
    }
    assert_eq!(tested == Tested::Pending, reason.is_some());
    assert!(
        reason
            .as_ref()
            .is_none_or(|reason| reason.len() <= test_launch::REASON_BYTES_MAX)
    );
    Ok(Reply::Tested {
        tested,
        reason,
        models: store::values(
            &db,
            harness,
            &crate::model_choice::catalog(harness),
            &Scope::App,
            installed,
        )?,
    })
}

#[cfg(all(test, unix))]
#[path = "model_choice_tests.rs"]
mod tests;
