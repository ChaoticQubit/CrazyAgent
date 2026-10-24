use super::*;
use crate::harnesses::{HarnessEntry, Search, catalog};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    harness_catalog: Action,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireReply {
    Catalog { entries: Vec<HarnessEntry> },
    Rejected { code: String },
}

impl OwnerClient {
    /// Lists every harness the engine knows with its status on this machine.
    /// Input: none beyond the attached owner capability. Output: catalog entries in
    /// owner priority order, or the engine's plain refusal code.
    pub async fn harness_catalog(&self) -> Result<Vec<HarnessEntry>, EngineError> {
        tokio::time::timeout(self.timeout, async {
            let frame = encode_frame(&Envelope {
                harness_catalog: Action::List,
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
            match serde_json::from_slice(&reply[4..])
                .map_err(|_| failure("harness_catalog_reply_invalid"))?
            {
                WireReply::Catalog { entries } if !entries.is_empty() => Ok(entries),
                WireReply::Catalog { .. } => Err(failure("harness_catalog_reply_invalid")),
                WireReply::Rejected { code } => Err(EngineError(code)),
            }
        })
        .await
        .map_err(|_| failure("harness_catalog_timeout"))?
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
    if bytes.len() > 256 {
        return Err(failure("harness_catalog_request_bounds"));
    }
    let Envelope {
        harness_catalog: Action::List,
    } = serde_json::from_slice(bytes).map_err(|_| failure("harness_catalog_request_invalid"))?;
    let runtime = OwnerRuntimeHandle {
        engine: engine.clone(),
        authority: authority.clone(),
    };
    let mut search = Search::from_environment();
    for (id, executable) in [
        ("codex", runtime.registered_codex_executable()),
        ("claude-code", runtime.registered_claude_executable()),
    ] {
        if let Ok(executable) = executable
            && let Ok(path) = executable.verified_path()
        {
            search = search.with_registered(id, path.to_path_buf());
        }
    }
    let entries = tokio::task::spawn_blocking(move || catalog(&search))
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
