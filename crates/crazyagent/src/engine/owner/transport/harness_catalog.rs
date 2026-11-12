use super::*;
use crate::harnesses::finding::{self, Found};
use crate::harnesses::update::{
    CheckWhen, HarnessVersions, InstallFromApp, OnlineLookup, UpdateMode,
};
use crate::harnesses::{HarnessEntry, Search};
use serde::{Deserialize, Serialize};

mod updates;
pub(super) use updates::{UPDATES_TIMEOUT, slow};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    harness_catalog: Action,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    List,
    Check,
    Versions {
        id: String,
        look: bool,
    },
    Update {
        id: String,
    },
    Install {
        id: String,
    },
    Settings,
    SetSettings {
        mode: UpdateMode,
        when: CheckWhen,
        online: OnlineLookup,
        install: InstallFromApp,
    },
    Find {
        id: String,
    },
    ChoosePath {
        id: String,
        path: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireReply {
    Catalog {
        entries: Vec<HarnessEntry>,
    },
    Rejected {
        code: String,
    },
    Versions {
        versions: Box<HarnessVersions>,
    },
    Started,
    Settings {
        mode: UpdateMode,
        when: CheckWhen,
        online: OnlineLookup,
        install: InstallFromApp,
    },
    Found {
        found: Found,
    },
}

impl OwnerClient {
    /// Lists every harness the engine knows with its status on this machine, from the
    /// engine's last probe unless `check` asks it to look again now. Input: whether to probe now,
    /// beyond the attached owner capability. Output: catalog entries in owner priority order, or
    /// the engine's plain refusal code.
    pub async fn harness_catalog(&self, check: bool) -> Result<Vec<HarnessEntry>, EngineError> {
        tokio::time::timeout(self.timeout, async {
            let frame = encode_frame(&Envelope {
                harness_catalog: if check { Action::Check } else { Action::List },
            })
            .map_err(|_| failure("harness_catalog_request_bounds"))?;
            self.root.verify()?;
            if self.root.socket_identity()? != self.socket {
                return Err(failure("owner_socket_replaced"));
            }
            let mut stream = crate::spawn_gate::connect_unix(self.root.path.join(SOCKET))
                .await
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
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
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
            stream
                .write_all(&frame)
                .await
                .map_err(|_| failure("harness_catalog_owner_unavailable"))?;
            let reply = read_frame(&mut stream)
                .await
                .map_err(|_| failure("harness_catalog_reply_invalid"))?;
            match serde_json::from_slice(&reply[4..]).map_err(|error| {
                let read = if check {
                    "harness_catalog.check"
                } else {
                    "harness_catalog.list"
                };
                self.skewed_reply(&error, Some(read), || {
                    failure("harness_catalog_reply_invalid")
                })
            })? {
                WireReply::Catalog { entries } if !entries.is_empty() => Ok(entries),
                WireReply::Rejected { code } => Err(EngineError(code)),
                _ => Err(failure("harness_catalog_reply_invalid")),
            }
        })
        .await
        .map_err(|_| failure("harness_catalog_timeout"))?
    }
}

/// Has Check again pin Codex and Claude Code when the engine started without them, after the
/// grant is checked, so the list and sign-in agree without a restart; a harness that is
/// still missing or refused only stays out of the pinned slots. Inputs: the engine,
/// authority, packet, admission and runtime. Output: Ok, or the grant refusal.
async fn look_again(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    runtime: &OwnerRuntimeHandle,
) -> Result<(), EngineError> {
    {
        let _barrier = engine.owner_egress.write().await;
        if &authority.authenticate(packet)? != admission {
            return Err(failure("owner_grant_stale_or_revoked"));
        }
    }
    for id in finding::CHOOSABLE {
        if let Ok(Found::Refused { code, .. }) = runtime.look_again(id) {
            eprintln!("{id} was found on Check again but not used ({code})");
        }
    }
    Ok(())
}

/// Encodes this protocol's refusal of a request the engine did not run.
/// Input: the refusal code. Output: the reply frame.
pub(super) fn refusal(code: &str) -> Result<Vec<u8>, EngineError> {
    encode_frame(&WireReply::Rejected { code: code.into() })
        .map_err(|_| failure("harness_catalog_response_bounds"))
}

pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    admission: &Admission,
    bytes: &[u8],
) -> Result<(), EngineError> {
    if bytes.len() > updates::REQUEST_BYTES_MAX {
        return Err(failure("harness_catalog_request_bounds"));
    }
    let Envelope {
        harness_catalog: action,
    } = serde_json::from_slice(bytes).map_err(|_| failure("harness_catalog_request_invalid"))?;
    let runtime = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    if matches!(action, Action::Check) {
        look_again(engine, authority, packet, admission, &runtime).await?;
    }
    let mut search = runtime
        .held_search()?
        .unwrap_or_else(Search::from_environment);
    for (id, executable) in [
        ("codex", runtime.registered_codex_executable()),
        ("claude-code", runtime.registered_claude_executable()),
        ("rcli", runtime.registered_rcli_master_executable()),
    ] {
        if let Ok(executable) = executable
            && let Ok(path) = executable.verified_path()
        {
            search = search.with_registered(id, path.to_path_buf());
        }
    }
    if !matches!(action, Action::List | Action::Check) {
        return updates::serve(stream, engine, authority, packet, admission, search, action).await;
    }
    let entries = engine
        .harness_probe
        .entries(search, matches!(action, Action::Check))
        .await
        .map_err(|_| failure("harness_catalog_unavailable"))?;
    assert!(!entries.is_empty());
    let _barrier = engine.owner_egress.write().await;
    let reply = if &authority.authenticate(packet)? == admission {
        WireReply::Catalog { entries }
    } else {
        WireReply::Rejected {
            code: "owner_grant_stale_or_revoked".into(),
        }
    };
    let frame = encode_frame(&reply).map_err(|_| failure("harness_catalog_response_bounds"))?;
    stream.write_all(&frame).await.map_err(io_error)
}
