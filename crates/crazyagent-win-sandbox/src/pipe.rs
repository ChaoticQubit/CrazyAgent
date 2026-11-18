#[cfg(windows)]
pub use crate::win::pipe::{INSTANCES_MAX, client, create};

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
