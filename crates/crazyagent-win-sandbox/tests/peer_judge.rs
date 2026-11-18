use crazyagent_win_sandbox::pipe::{
    Peer, Pin, Reverted, Served, after_revert, judge_peer, judge_server,
};

/// A user SID shaped as S-1-5-21-1-2-3-1001.
const USER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xe9, 3, 0, 0,
];
/// Another user SID on the same machine, S-1-5-21-1-2-3-1002.
const OTHER: [u8; 28] = [
    1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xea, 3, 0, 0,
];

/// A peer of the user at `integrity`, neither an AppContainer nor restricted. Input: the
/// integrity level. Output: the peer.
fn peer(integrity: u32) -> Peer {
    Peer {
        user: USER.to_vec(),
        app_container: false,
        restricted: false,
        integrity,
    }
}

/// W2 D9 and D10 token rule (DESIGN-0256): the user's own process at medium, medium plus,
/// high or system integrity passes, the unelevated and the elevated client alike (A4).
#[test]
fn a_peer_of_this_user_at_medium_or_above_passes() {
    for integrity in [0x2000, 0x2100, 0x3000, 0x4000] {
        assert_eq!(
            judge_peer(&peer(integrity), &USER),
            Ok(()),
            "{integrity:#x}"
        );
    }
}

/// W2 D9 and D10 token rule: each refusal stands on its own, so another account, an
/// AppContainer, a restricted token and a level below medium are each refused while every
/// other fact would pass.
#[test]
fn each_peer_rule_refuses_on_its_own() {
    let other = Peer {
        user: OTHER.to_vec(),
        ..peer(0x2000)
    };
    assert_eq!(
        judge_peer(&other, &USER),
        Err("another account is at the other end of the pipe")
    );
    let container = Peer {
        app_container: true,
        ..peer(0x2000)
    };
    assert_eq!(
        judge_peer(&container, &USER),
        Err("an app container is at the other end of the pipe")
    );
    let restricted = Peer {
        restricted: true,
        ..peer(0x3000)
    };
    assert_eq!(
        judge_peer(&restricted, &USER),
        Err("a restricted token is at the other end of the pipe")
    );
    for integrity in [0, 0x1000, 0x1fff] {
        assert_eq!(
            judge_peer(&peer(integrity), &USER),
            Err("a lower integrity process is at the other end of the pipe"),
            "{integrity:#x}"
        );
    }
}

/// rv-code #551 F3: the engine always knows its own user SID, so judging a peer against an
/// empty one is a programming error, never a pass or a refusal.
#[test]
fn judging_a_peer_against_an_empty_user_is_a_programming_error() {
    let judged = std::panic::catch_unwind(|| judge_peer(&peer(0x2000), &[]));
    assert!(judged.is_err());
}

/// rv-code #551 F1 (leader 07:48): after the D9 check reverts, only a probe that opens a
/// thread token, so one is still held, aborts; ERROR_NO_TOKEN (1008) is clean; any other
/// failed probe refuses the check, never panicking a pool thread.
#[test]
fn only_a_token_still_held_after_the_revert_aborts() {
    assert_eq!(after_revert(true, 0), Reverted::Held);
    assert_eq!(after_revert(true, 1008), Reverted::Held);
    assert_eq!(after_revert(false, 1008), Reverted::Clean);
    for error in [0, 5, 8, 1450] {
        assert_eq!(after_revert(false, error), Reverted::Unread(error));
    }
}

/// The engine's pin in these rows: a process ID and its creation time.
const ENGINE: Pin = Pin {
    pid: 4242,
    created: 133_000_000_000_000_000,
};

/// What a client reads off a pipe the engine serves at `integrity` with `label`. Inputs:
/// the server's integrity level and the pipe's label level, if any. Output: the reading.
fn served(integrity: u32, label: Option<u32>) -> Served {
    Served {
        pin: ENGINE,
        peer: peer(integrity),
        label,
    }
}

/// W2 D10 (DESIGN-0256): a pipe the pinned engine serves passes, with no label, a medium
/// label or a higher one; no label reads as medium, the label the engine's own pipes carry.
#[test]
fn a_pipe_the_pinned_engine_serves_passes() {
    for label in [None, Some(0x2000), Some(0x3000)] {
        assert_eq!(
            judge_server(&served(0x2000, label), ENGINE, &USER),
            Ok(()),
            "{label:?}"
        );
    }
}

/// W2 D10 step 1: another process ID, or the engine's process ID reused by a process made
/// at another time, is refused before the token or the label is judged.
#[test]
fn a_pipe_another_process_serves_is_refused_first() {
    let other = Served {
        pin: Pin {
            pid: 4243,
            ..ENGINE
        },
        ..served(0x2000, None)
    };
    let reused = Served {
        pin: Pin {
            created: ENGINE.created + 1,
            ..ENGINE
        },
        ..served(0x1000, Some(0x1000))
    };
    for found in [other, reused] {
        assert_eq!(
            judge_server(&found, ENGINE, &USER),
            Err("another process serves the pipe")
        );
    }
}

/// W2 D10 steps 2 and 3: with the pin matching, the server's token is judged as D9 judges a
/// client, and then a pipe label below medium is refused.
#[test]
fn the_pinned_server_is_judged_by_its_token_and_then_its_pipe_label() {
    assert_eq!(
        judge_server(&served(0x1000, None), ENGINE, &USER),
        Err("a lower integrity process is at the other end of the pipe")
    );
    assert_eq!(
        judge_server(&served(0x2000, Some(0x1000)), ENGINE, &USER),
        Err("the pipe carries a label below medium")
    );
}
