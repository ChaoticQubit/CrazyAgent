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
        .rows(width, &Look::plain(), false)
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
            "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start   Ctrl-N Next item",
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

/// The banner's key line. Input: the banner. Output: the line naming what each key acts on.
fn keys(recovery: &ProjectRecovery) -> String {
    text(recovery, 120)
        .into_iter()
        .find(|line| line.starts_with("Ctrl-"))
        .expect("the banner shows its keys")
}

#[test]
fn while_the_composer_holds_the_keys_the_key_line_says_tab_first() {
    let recovery = shown(RecoveryBanner {
        runs: vec![run(
            3,
            "Sub-agent 3",
            RunShown::Failed,
            Some("The account is unavailable."),
        )],
        notices: vec![notice(ActionKind::AgentSpawn, 7, NoticeShown::Unknown)],
        more: 0,
    });
    let line = |composer: bool| {
        recovery
            .rows(120, &Look::plain(), composer)
            .iter()
            .map(|row| row.text().trim_end().to_owned())
            .find(|line| line.contains("Ctrl-D"))
            .expect("the banner shows its keys")
    };
    assert_eq!(
        line(true),
        "Tab first, then Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start   Ctrl-N Next item",
        "in the composer Ctrl-R and Ctrl-D are the composer's until Tab"
    );
    assert_eq!(
        line(false),
        "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start   Ctrl-N Next item"
    );
}

#[test]
fn key_hints_wrap_between_hints_and_never_pass_the_banner_width() {
    let recovery = shown(RecoveryBanner {
        runs: vec![run(
            3,
            "Sub-agent 3",
            RunShown::Failed,
            Some("The account is unavailable."),
        )],
        notices: vec![notice(ActionKind::AgentSpawn, 7, NoticeShown::Unknown)],
        more: 0,
    });
    let hints = [
        "Ctrl-R Retry Sub-agent 3",
        "Ctrl-D Dismiss Ada's start",
        "Ctrl-N Next item",
    ];
    for width in [76, 60, 40, 30] {
        for composer in [true, false] {
            let lines: Vec<String> = recovery
                .rows(width, &Look::plain(), composer)
                .iter()
                .map(|row| row.text().trim_end().to_owned())
                .collect();
            for line in &lines {
                assert!(
                    cells(line) <= width,
                    "{width} cells, composer {composer}: {line:?} is wider than the banner"
                );
            }
            for hint in hints {
                assert!(
                    lines.iter().any(|line| line.contains(hint)),
                    "{width} cells, composer {composer}: {hint:?} stays on one line: {lines:#?}"
                );
            }
            assert_eq!(
                lines.iter().any(|line| line.starts_with("Tab first, then")),
                composer,
                "{width} cells: the lead shows only while the composer has the keys"
            );
        }
    }
}

/// Answers the request in flight as done, then the read that follows with a banner.
/// Inputs: the project banner and the banner the read returns.
fn done_then_read(recovery: &mut ProjectRecovery, banner: RecoveryBanner) {
    let sent = recovery.sent.clone().expect("a choice is in flight");
    recovery.apply(&RecoveryEvent {
        query: sent,
        result: Ok(RecoveryReply::Done),
    });
    let read = recovery.next_query(NOW).expect("a done choice reads again");
    assert_eq!(read, RecoveryQuery::Read { project: project() });
    recovery.apply(&RecoveryEvent {
        query: read,
        result: Ok(RecoveryReply::Banner(banner)),
    });
}

#[test]
fn ctrl_n_picks_which_failed_resume_or_notice_the_keys_act_on() {
    let failed = |notices: Vec<BannerNotice>| RecoveryBanner {
        runs: vec![
            run(
                3,
                "Sub-agent 3",
                RunShown::Failed,
                Some("The account is unavailable."),
            ),
            run(
                5,
                "Sub-agent 5",
                RunShown::Failed,
                Some("The harness stopped."),
            ),
        ],
        notices,
        more: 0,
    };
    let spawn = notice(ActionKind::AgentSpawn, 7, NoticeShown::Unknown);
    let landed = notice(ActionKind::LandingAccept, 8, NoticeShown::Landed);
    let mut recovery = shown(failed(vec![spawn.clone(), landed.clone()]));
    assert_eq!(
        keys(&recovery),
        "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start   Ctrl-N Next item"
    );

    assert!(recovery.key(&Key::Ctrl('n')));
    assert_eq!(
        keys(&recovery),
        "Ctrl-R Retry Sub-agent 5   Ctrl-D Dismiss Ada's start   Ctrl-N Next item"
    );
    assert!(recovery.key(&Key::Ctrl('r')));
    assert_eq!(
        recovery.next_query(NOW),
        Some(RecoveryQuery::Retry {
            project: project(),
            agent: 5
        }),
        "the picked failed resume is retried, not the first"
    );
    done_then_read(&mut recovery, failed(vec![spawn.clone(), landed]));
    assert!(
        keys(&recovery).starts_with("Ctrl-R Retry Sub-agent 5"),
        "the pick stays on a resume that failed again"
    );

    assert!(recovery.key(&Key::Ctrl('n')));
    assert!(recovery.key(&Key::Ctrl('n')));
    assert_eq!(
        keys(&recovery),
        "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss change set set-1   Ctrl-N Next item"
    );
    assert!(recovery.key(&Key::Ctrl('d')));
    assert_eq!(
        recovery.next_query(NOW),
        Some(RecoveryQuery::Dismiss {
            project: project(),
            kind: ActionKind::LandingAccept,
            id: 8
        }),
        "the picked notice is dismissed, not the first"
    );
    done_then_read(&mut recovery, failed(vec![spawn]));
    assert_eq!(
        keys(&recovery),
        "Ctrl-R Retry Sub-agent 3   Ctrl-D Dismiss Ada's start   Ctrl-N Next item",
        "a pick that is gone falls back to the first of each"
    );
    assert!(recovery.key(&Key::Ctrl('n')));
    assert!(keys(&recovery).starts_with("Ctrl-R Retry Sub-agent 5"));

    let mut single = shown(RecoveryBanner {
        runs: vec![run(3, "Sub-agent 3", RunShown::Failed, None)],
        notices: vec![],
        more: 0,
    });
    assert_eq!(keys(&single), "Ctrl-R Retry Sub-agent 3");
    assert!(
        !single.key(&Key::Ctrl('n')),
        "with one item Ctrl-N is not the banner's"
    );
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
    let mut busy = ProjectRecovery::new(project());
    let read = busy.next_query(NOW).unwrap();
    busy.apply(&RecoveryEvent {
        query: read,
        result: Err("owner_busy".into()),
    });
    assert_eq!(
        text(&busy, 80),
        [
            "Interrupted work couldn't be read. CrazyAgent is busy with other requests. Try again in a moment.",
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
