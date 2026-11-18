use crazyagent_win_sandbox::pipe::{Peer, judge_peer};

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
