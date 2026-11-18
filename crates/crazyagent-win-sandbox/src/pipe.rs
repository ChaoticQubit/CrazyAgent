#[cfg(windows)]
pub use crate::win::pipe::{INSTANCES_MAX, client, create, pin_of, server};

const MEDIUM_LEVEL: u32 = 0x2000;

/// What a token says about the process at the other end of an engine pipe (DESIGN-0256 D9,
/// D10): its user SID, whether it is an AppContainer token, whether it has restricting SIDs,
/// and its integrity level, the last part of its mandatory label SID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub user: Vec<u8>,
    pub app_container: bool,
    pub restricted: bool,
    pub integrity: u32,
}

/// Judges the process at the other end of an engine pipe (DESIGN-0140 D9 step 2,
/// DESIGN-0256 D10 step 2): the engine's own user, not an AppContainer, not restricted, and
/// at medium integrity or above. Inputs: the peer and the engine's user SID. Output: Ok, or
/// why the peer is refused.
pub fn judge_peer(peer: &Peer, user: &[u8]) -> Result<(), &'static str> {
    assert!(!user.is_empty(), "the engine's user SID is never empty");
    if peer.user != user {
        return Err("another account is at the other end of the pipe");
    }
    if peer.app_container {
        return Err("an app container is at the other end of the pipe");
    }
    if peer.restricted {
        return Err("a restricted token is at the other end of the pipe");
    }
    if peer.integrity < MEDIUM_LEVEL {
        return Err("a lower integrity process is at the other end of the pipe");
    }
    Ok(())
}

/// A process pinned by its ID and creation time (DESIGN-0256 D10 step 1): a reused ID
/// belongs to a process made at another time, so the pair names one process for its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    pub pid: u32,
    pub created: u64,
}

/// What a client reads off its connected pipe before writing a byte (DESIGN-0256 D10): the
/// pin of the process that serves it, that process's token, and the pipe's integrity label
/// level, None when the pipe carries no label, which Windows treats as medium.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub pin: Pin,
    pub peer: Peer,
    pub label: Option<u32>,
}

/// Judges who serves a pipe, in D10's order: the pin must be the expected server's, its
/// token must pass [`judge_peer`], and the pipe's label must not be below medium. Inputs:
/// what the client read, the expected server's pin and the user's SID. Output: Ok, or why
/// the client refuses before writing.
pub fn judge_server(served: &Served, expected: Pin, user: &[u8]) -> Result<(), &'static str> {
    if served.pin != expected {
        return Err("another process serves the pipe");
    }
    judge_peer(&served.peer, user)?;
    if served.label.is_some_and(|level| level < MEDIUM_LEVEL) {
        return Err("the pipe carries a label below medium");
    }
    Ok(())
}
