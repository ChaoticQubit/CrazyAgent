use super::*;
use crate::team::AgentRole;

const NOW: i64 = 1_800_000_000_000;

fn project() -> ProjectId {
    ProjectId::new("banner-project").unwrap()
}

fn run(agent: i64, label: &str, state: RunShown, reason: Option<&str>) -> BannerRun {
    BannerRun {
        agent,
        label: label.into(),
        role: AgentRole::Sub,
        harness: "Codex".into(),
        at_ms: NOW,
        turn: 3,
        asked: Some("Plan the page".into()),
        state,
        reason: reason.map(str::to_owned),
        paused: false,
    }
}

fn notice(kind: ActionKind, id: i64, shown: NoticeShown) -> BannerNotice {
    let landing = kind == ActionKind::LandingAccept;
    BannerNotice {
        action_kind: kind,
        id,
        agent: (!landing).then_some(2),
        label: (!landing).then(|| "Ada".to_owned()),
        change_set: landing.then(|| "set-1".to_owned()),
        shown,
        started_ms: NOW,
    }
}

/// Reads one banner into a fresh project banner, as the engine's reply to its first read.
/// Input: the banner. Output: the project banner holding it.
fn shown(banner: RecoveryBanner) -> ProjectRecovery {
    let mut recovery = ProjectRecovery::new(project());
    let query = recovery.next_query(NOW).expect("a first read");
    assert_eq!(query, RecoveryQuery::Read { project: project() });
    assert_eq!(recovery.next_query(NOW), None, "one request at a time");
    recovery.apply(&RecoveryEvent {
        query,
        result: Ok(RecoveryReply::Banner(banner)),
    });
    recovery
}

fn text(recovery: &ProjectRecovery, width: usize) -> Vec<String> {
    recovery
        .rows(width, &Look::plain())
        .iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

#[test]
fn recovery_banner_lists_interrupted_resumed_failed() {
    let mut recovery = shown(RecoveryBanner {
        runs: vec![
            run(2, "Ada", RunShown::Interrupted, None),
            run(
                3,
                "Sub-agent 3",
                RunShown::Failed,
                Some("The account is unavailable."),
            ),
            run(4, "Sub-agent 4", RunShown::Resumed, None),
        ],
        notices: vec![
            notice(ActionKind::AgentSpawn, 7, NoticeShown::Unknown),
            notice(ActionKind::LandingAccept, 8, NoticeShown::Landed),
        ],
        more: 0,
    });
    let lines = text(&recovery, 120);
    assert_eq!(
        lines,
        [
            "Interrupted work  4 to resolve",
            "Interrupted  Ada (Codex), turn 3: \"Plan the page\", resumes on its own",
            "Resumed      Sub-agent 4 (Codex)",
            "Failed       Sub-agent 3 (Codex): The account is unavailable.",
            "Check        Ada's start may or may not have run; check the agent first; change set set-1 landed while CrazyAgent",
            "             stopped",
            "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start",
            "",
        ]
    );

    assert!(recovery.key(&Key::Ctrl('r')));
    let retry = recovery.next_query(NOW).expect("the retry is sent");
    assert_eq!(
        retry,
        RecoveryQuery::Retry {
            project: project(),
            agent: 3
        }
    );
    assert!(recovery.key(&Key::Ctrl('d')), "the key is the banner's");
    assert_eq!(recovery.next_query(NOW), None, "a second choice waits");
    recovery.apply(&RecoveryEvent {
        query: retry,
        result: Ok(RecoveryReply::Done),
    });
    assert!(text(&recovery, 120).contains(&"Sub-agent 3 will try to resume again.".to_owned()));
    assert_eq!(
        recovery.next_query(NOW),
        Some(RecoveryQuery::Read { project: project() }),
        "a done choice reads the list again"
    );
    assert!(!recovery.key(&Key::Ctrl('x')));
}

#[test]
fn a_banner_with_nothing_to_resolve_shows_nothing_and_keeps_no_keys() {
    for banner in [
        RecoveryBanner::default(),
        RecoveryBanner {
            runs: vec![run(4, "Sub-agent 4", RunShown::Resumed, None)],
            notices: vec![],
            more: 0,
        },
    ] {
        let mut recovery = shown(banner);
        assert!(text(&recovery, 80).is_empty());
        assert!(!recovery.key(&Key::Ctrl('r')));
        assert!(!recovery.key(&Key::Ctrl('d')));
        assert_eq!(
            recovery.next_query(NOW + POLL_MS),
            None,
            "nothing waits to resume"
        );
    }
}

#[test]
fn a_waiting_run_is_read_again_and_a_failed_read_says_so_plainly() {
    let mut recovery = shown(RecoveryBanner {
        runs: vec![run(2, "Ada", RunShown::Resuming, None)],
        notices: vec![],
        more: 2,
    });
    assert!(text(&recovery, 80)[0].ends_with("3 to resolve"));
    assert_eq!(recovery.next_query(NOW + POLL_MS - 1), None);
    let read = recovery.next_query(NOW + POLL_MS).expect("read again");
    recovery.apply(&RecoveryEvent {
        query: read.clone(),
        result: Err("recovery_unavailable".into()),
    });
    assert!(
        text(&recovery, 80)[1].contains("Ada (Codex), turn 3"),
        "the last list stays"
    );
    recovery.forget_reads();
    let mut fresh = ProjectRecovery::new(project());
    let read = fresh.next_query(NOW).unwrap();
    fresh.apply(&RecoveryEvent {
        query: read,
        result: Err("recovery_unavailable".into()),
    });
    assert_eq!(
        text(&fresh, 80),
        [
            "Interrupted work couldn't be read. CrazyAgent couldn't read its records. Try again.",
            ""
        ]
    );
    let mut offline = ProjectRecovery::new(project());
    let read = offline.next_query(NOW).unwrap();
    offline.apply(&RecoveryEvent {
        query: read,
        result: Err("trusted_connection_unavailable".into()),
    });
    assert!(
        text(&offline, 80).is_empty(),
        "the shell already says the engine is away"
    );
}
