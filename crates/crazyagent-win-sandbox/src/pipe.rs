#[cfg(windows)]
pub use crate::win::pipe::{INSTANCES_MAX, client, create, pin_of, server};

const MEDIUM_LEVEL: u32 = 0x2000;
/// Windows' ERROR_NO_TOKEN: the thread holds no token of its own.
pub(crate) const NO_TOKEN: u32 = 1008;

/// What a probe of the thread's own token finds once the D9 check has reverted (rv-code
/// #551 F1): no token, a token still held, or a probe that failed with this Windows error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reverted {
    Clean,
    Held,
    Unread(u32),
}

/// Classifies that probe: an open that succeeds means a token is still held, so the process
/// must abort; ERROR_NO_TOKEN means the thread acts as the process again; any other error
/// leaves that unknown, so the check is refused. Inputs: whether the open succeeded and its
/// error. Output: the classification.
pub fn after_revert(opened: bool, error: u32) -> Reverted {
    match (opened, error) {
        (true, _) => Reverted::Held,
        (false, NO_TOKEN) => Reverted::Clean,
        (false, error) => Reverted::Unread(error),
    }
}

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
