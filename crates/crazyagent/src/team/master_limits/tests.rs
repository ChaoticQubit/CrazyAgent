use super::*;
use crate::team::TeamAction;

const PROJECT: &str = "limits";
const DAY_MS: i64 = 86_400_000;
const START: i64 = 1_800_000_000_000;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::team::initialize(&connection).unwrap();
    connection
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn message(connection: &Connection, author: &str, text: &str, state: &str, created_ms: i64) {
    connection
        .execute(
            "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,?2,?3,?4,?5)",
            params![PROJECT, author, text, state, created_ms],
        )
        .unwrap();
}

fn typed(connection: &Connection, typed_ms: i64) {
    connection
        .execute(
            "INSERT INTO team_live_masters(project,live,typed_ms) VALUES (?1,1,?2)
             ON CONFLICT(project) DO UPDATE SET typed_ms=excluded.typed_ms",
            params![PROJECT, typed_ms],
        )
        .unwrap();
}

#[test]
fn an_unset_project_runs_until_the_goal_is_done_for_seven_days() {
    let connection = database();
    let project = project();
    assert_eq!(
        read(&connection, &project).unwrap(),
        MasterLimits {
            rounds: None,
            run_secs: Some(7 * 86_400),
        }
    );
    assert_eq!(
        MasterLimits::default(),
        read(&connection, &project).unwrap()
    );
    let rounds = rounds(&connection, &project).unwrap();
    assert_eq!(rounds, Rounds::default());
    assert!(rounds.open());
    assert_eq!(run_deadline(&connection, &project).unwrap(), None);
}

#[test]
fn every_rounds_choice_is_saved_and_until_the_goal_is_done_clears_it() {
    let connection = database();
    let project = project();
    set_run_time(&connection, &project, Some(RUN_CHOICES_SECS[0])).unwrap();
    for max in ROUND_CHOICES {
        set_rounds(&connection, &project, Some(max)).unwrap();
        let limits = read(&connection, &project).unwrap();
        assert_eq!(limits.rounds, Some(max));
        assert_eq!(limits.run_secs, Some(RUN_CHOICES_SECS[0]));
        assert_eq!(rounds(&connection, &project).unwrap().max, Some(max));
    }
    set_rounds(&connection, &project, None).unwrap();
    assert_eq!(read(&connection, &project).unwrap().rounds, None);
    assert_eq!(
        read(&connection, &project).unwrap().run_secs,
        Some(RUN_CHOICES_SECS[0])
    );
}

#[test]
fn every_run_time_choice_is_saved_and_no_limit_differs_from_the_unset_default() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(3)).unwrap();
    for secs in RUN_CHOICES_SECS {
        set_run_time(&connection, &project, Some(secs)).unwrap();
        let limits = read(&connection, &project).unwrap();
        assert_eq!(limits.run_secs, Some(secs));
        assert_eq!(limits.rounds, Some(3));
    }
    set_run_time(&connection, &project, None).unwrap();
    assert_eq!(read(&connection, &project).unwrap().run_secs, None);
    let stored: Option<i64> = connection
        .query_row(
            "SELECT run_secs FROM team_master_limits WHERE project=?1",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, Some(0));
    connection
        .execute(
            "UPDATE team_master_limits SET run_secs=NULL WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(
        read(&connection, &project).unwrap().run_secs,
        Some(RUN_DEFAULT_SECS)
    );
}

#[test]
fn a_stored_value_outside_the_choices_is_refused_on_read() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(1)).unwrap();
    connection
        .execute(
            "UPDATE team_master_limits SET rounds=4 WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(
        read(&connection, &project).unwrap_err().0,
        "team_rounds_invalid"
    );
    connection
        .execute(
            "UPDATE team_master_limits SET rounds=NULL, run_secs=3600 WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(
        read(&connection, &project).unwrap_err().0,
        "team_run_time_invalid"
    );
}

#[test]
fn the_owner_actions_refuse_unlisted_choices_and_take_every_listed_one() {
    let project_id = project();
    for max in [Some(0), Some(4), Some(21), Some(255)] {
        let action = TeamAction::MasterRounds {
            project_id: project_id.clone(),
            max,
        };
        assert_eq!(action.validate().unwrap_err().0, "team_rounds_invalid");
    }
    for max in ROUND_CHOICES.map(Some).into_iter().chain([None]) {
        let action = TeamAction::MasterRounds {
            project_id: project_id.clone(),
            max,
        };
        assert!(action.validate().is_ok(), "{max:?}");
    }
    for secs in [Some(0), Some(60), Some(3600), Some(u32::MAX)] {
        let action = TeamAction::MasterRunTime {
            project_id: project_id.clone(),
            secs,
        };
        assert_eq!(action.validate().unwrap_err().0, "team_run_time_invalid");
    }
    for secs in RUN_CHOICES_SECS.map(Some).into_iter().chain([None]) {
        let action = TeamAction::MasterRunTime {
            project_id: project_id.clone(),
            secs,
        };
        assert!(action.validate().is_ok(), "{secs:?}");
    }
}

/// Writes the master's reply that handed work over and returns its id, the hand-off key of
/// that reply. Inputs: the database and the time. Output: the message id.
fn reply(connection: &Connection, created_ms: i64) -> i64 {
    message(connection, "master", "On it.", "reply", created_ms);
    connection.last_insert_rowid()
}

/// Writes one task of the team. Inputs: the database, its source, its hand-off key, its
/// parent task and the time. Output: none.
fn task(
    connection: &Connection,
    source: &str,
    turn: Option<i64>,
    parent: Option<i64>,
    created_ms: i64,
) {
    connection
        .execute(
            "INSERT INTO team_tasks(project,source,turn,harness,tools,prompt,status,created_ms,parent)
             VALUES (?1,?2,?3,'codex',0,'Build it','queued',?4,?5)",
            params![PROJECT, source, turn, created_ms, parent],
        )
        .unwrap();
}

/// Hands one task over under a key the way a master does. Inputs: the database, the key
/// and the time. Output: none.
fn handed(connection: &Connection, key: i64, created_ms: i64) {
    task(connection, "master", Some(key), None, created_ms);
}

#[test]
fn a_round_is_one_master_turn_that_handed_work_over_since_the_owners_last_goal_message() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(2)).unwrap();
    let old = reply(&connection, START);
    handed(&connection, old, START);
    message(&connection, "owner", "build the page", "answered", START);
    assert_eq!(rounds(&connection, &project).unwrap().used, 0);
    let first = reply(&connection, START);
    handed(&connection, first, START);
    handed(&connection, first, START);
    task(&connection, "master", None, None, START);
    task(&connection, "master", Some(first + 1), Some(1), START);
    task(&connection, "owner", Some(first + 2), None, START);
    message(&connection, "engine", "results one", "pending", START + 1);
    let one = rounds(&connection, &project).unwrap();
    assert_eq!((one.used, one.max, one.kept_going), (1, Some(2), false));
    assert!(one.open());
    assert!(joins_round(&connection, &project, first).unwrap());
    assert!(!joins_round(&connection, &project, old).unwrap());
    let live = live_turn_key(4);
    for _ in 0..3 {
        handed(&connection, live, START + 2);
    }
    let two = rounds(&connection, &project).unwrap();
    assert_eq!(
        two.used, 2,
        "three hand-offs of one live turn are one round"
    );
    assert!(!two.open());
    assert!(joins_round(&connection, &project, live).unwrap());
    assert!(!joins_round(&connection, &project, live_turn_key(5)).unwrap());
    message(
        &connection,
        "owner",
        "now the footer",
        "answered",
        START + 3,
    );
    let fresh = rounds(&connection, &project).unwrap();
    assert_eq!((fresh.used, fresh.kept_going), (0, false));
    assert!(fresh.open());
    assert!(!joins_round(&connection, &project, first).unwrap());
    assert!(!joins_round(&connection, &project, live).unwrap());
}

#[test]
fn a_live_turn_key_is_the_turn_that_ends_next_negated() {
    let connection = database();
    let project = project();
    assert_eq!(live_key(&connection, &project).unwrap(), -1);
    typed(&connection, START);
    assert_eq!(live_key(&connection, &project).unwrap(), -1);
    connection
        .execute(
            "UPDATE team_live_masters SET turns=4 WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(live_key(&connection, &project).unwrap(), live_turn_key(5));
    assert_eq!(live_turn_key(5), -5);
}

#[test]
fn keep_going_lifts_the_limit_once_for_the_rest_of_the_goal_only() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(1)).unwrap();
    message(&connection, "owner", "fix the login", "answered", START);
    let first = reply(&connection, START + 1);
    handed(&connection, first, START + 1);
    assert!(!rounds(&connection, &project).unwrap().open());
    message(
        &connection,
        "owner",
        "  Keep going! ",
        "answered",
        START + 2,
    );
    let kept = rounds(&connection, &project).unwrap();
    assert_eq!((kept.used, kept.kept_going), (1, true));
    assert!(kept.open());
    for at in 3..8 {
        let key = reply(&connection, START + at);
        handed(&connection, key, START + at);
    }
    let later = rounds(&connection, &project).unwrap();
    assert_eq!((later.used, later.kept_going), (6, true));
    assert!(later.open());
    message(
        &connection,
        "owner",
        "now the signup",
        "answered",
        START + 8,
    );
    let next = rounds(&connection, &project).unwrap();
    assert_eq!((next.used, next.kept_going), (0, false));
    let key = reply(&connection, START + 9);
    handed(&connection, key, START + 9);
    assert!(!rounds(&connection, &project).unwrap().open());
    message(&connection, "owner", "keep going.", "answered", START + 10);
    assert!(rounds(&connection, &project).unwrap().open());
}

#[test]
fn words_around_keep_going_start_a_new_goal_instead() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(1)).unwrap();
    message(&connection, "owner", "fix the login", "answered", START);
    let first = reply(&connection, START + 1);
    handed(&connection, first, START + 1);
    message(
        &connection,
        "owner",
        "keep going with the tests",
        "answered",
        START + 2,
    );
    let rounds = rounds(&connection, &project).unwrap();
    assert_eq!((rounds.used, rounds.kept_going), (0, false));
    assert!(rounds.open());
}

#[test]
fn the_owners_key_in_the_live_terminal_starts_a_new_goal() {
    let connection = database();
    let project = project();
    set_rounds(&connection, &project, Some(2)).unwrap();
    message(&connection, "owner", "fix the login", "answered", START);
    handed(&connection, live_turn_key(1), START + 1);
    message(&connection, "owner", "keep going", "answered", START + 2);
    handed(&connection, live_turn_key(2), START + 3);
    let before = rounds(&connection, &project).unwrap();
    assert_eq!((before.used, before.kept_going), (2, true));
    typed(&connection, START + 4);
    let after = rounds(&connection, &project).unwrap();
    assert_eq!((after.used, after.kept_going), (0, false));
    handed(&connection, live_turn_key(3), START + 5);
    handed(&connection, live_turn_key(4), START + 6);
    assert!(!rounds(&connection, &project).unwrap().open());
}

#[test]
fn the_deadline_counts_from_the_latest_of_start_owner_message_and_key() {
    let connection = database();
    let project = project();
    let seven = 7 * DAY_MS;
    clock_started(&connection, &project, 9, START).unwrap();
    assert_eq!(
        run_deadline(&connection, &project).unwrap(),
        Some(START + seven)
    );
    message(&connection, "owner", "fix it", "answered", START + 1000);
    message(&connection, "engine", "results", "answered", START + 5000);
    assert_eq!(
        run_deadline(&connection, &project).unwrap(),
        Some(START + 1000 + seven)
    );
    typed(&connection, START + 2000);
    assert_eq!(
        run_deadline(&connection, &project).unwrap(),
        Some(START + 2000 + seven)
    );
    for secs in RUN_CHOICES_SECS {
        set_run_time(&connection, &project, Some(secs)).unwrap();
        assert_eq!(
            run_deadline(&connection, &project).unwrap(),
            Some(START + 2000 + i64::from(secs) * 1000)
        );
    }
    set_run_time(&connection, &project, None).unwrap();
    assert_eq!(run_deadline(&connection, &project).unwrap(), None);
}

#[test]
fn a_restarted_or_adopted_pane_starts_a_fresh_clock_and_an_ended_one_has_none() {
    let connection = database();
    let project = project();
    clock_started(&connection, &project, 9, START).unwrap();
    let restarted = START + 10 * DAY_MS;
    clock_started(&connection, &project, 12, restarted).unwrap();
    assert_eq!(
        run_deadline(&connection, &project).unwrap(),
        Some(restarted + 7 * DAY_MS)
    );
    clock_ended(&connection, &project, 9).unwrap();
    assert_eq!(
        run_deadline(&connection, &project).unwrap(),
        Some(restarted + 7 * DAY_MS)
    );
    clock_ended(&connection, &project, 12).unwrap();
    assert_eq!(run_deadline(&connection, &project).unwrap(), None);
    let clocks: i64 = connection
        .query_row("SELECT COUNT(*) FROM team_master_clocks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(clocks, 0);
}

#[test]
fn the_round_line_names_the_round_and_the_limit() {
    let open = Rounds {
        used: 1,
        max: None,
        kept_going: false,
    };
    assert_eq!(
        open.results_line(),
        "\nSub-agent rounds for the owner's current goal: 1. No round limit."
    );
    let first = Rounds {
        used: 1,
        max: Some(3),
        kept_going: false,
    };
    assert_eq!(
        first.results_line(),
        "\nSub-agent rounds for the owner's current goal: 1 of 3."
    );
    let last = Rounds { used: 3, ..first };
    assert_eq!(
        last.results_line(),
        "\nSub-agent rounds for the owner's current goal: 3 of 3, the limit: hand over no new work until the owner writes."
    );
    let kept = Rounds {
        used: 7,
        max: Some(3),
        kept_going: true,
    };
    assert_eq!(
        kept.results_line(),
        "\nSub-agent rounds for the owner's current goal: 7. The owner said keep going, so no round limit."
    );
    let huge = Rounds {
        used: u32::MAX,
        max: Some(20),
        kept_going: false,
    };
    assert!(huge.results_line().len() <= ROUND_LINE_BYTES_MAX);
}

#[test]
fn the_limit_note_tells_the_owner_how_to_go_on() {
    let one = Rounds {
        used: 1,
        max: Some(1),
        kept_going: false,
    };
    assert_eq!(
        one.limit_note(),
        "The master used all 1 sub-agent round for your last message. Reply keep going to let it finish this goal, or send a new message."
    );
    let five = Rounds {
        used: 5,
        max: Some(5),
        kept_going: true,
    };
    assert!(
        five.limit_note()
            .starts_with("The master used all 5 sub-agent rounds for your last message.")
    );
}

#[test]
fn the_masters_own_call_past_the_limit_is_refused_in_words_that_name_keep_going() {
    let three = Rounds {
        used: 3,
        max: Some(3),
        kept_going: false,
    };
    assert_eq!(
        three.refusal(),
        "Not handed off: the owner's current goal used all 3 sub-agent rounds the owner allows for one message. Tell the owner what was done; a reply of keep going lets you finish the goal."
    );
}

#[test]
fn only_the_run_time_stop_line_reads_as_a_run_time_stop() {
    for secs in RUN_CHOICES_SECS {
        assert!(run_stopped(&run_stopped_line(secs)));
    }
    assert!(!run_stopped(&crate::team::master::idle_stopped_line(
        "15 min"
    )));
    assert!(!run_stopped("it ran 7 days without a message from you"));
    assert!(!crate::team::master::idle_stopped(&run_stopped_line(
        RUN_DEFAULT_SECS
    )));
}

#[test]
fn the_master_view_words_the_time_left_and_the_rounds() {
    let now = START;
    let rounds = Rounds {
        used: 2,
        max: Some(3),
        kept_going: false,
    };
    let view = LimitsView {
        rounds,
        run_secs: Some(RUN_DEFAULT_SECS),
        deadline_ms: Some(now + 90 * 60_000),
    };
    assert_eq!(
        view.words(now),
        "1 h 30 min left. Sub-agent rounds: 2 of 3."
    );
    let unlimited = LimitsView {
        run_secs: None,
        deadline_ms: None,
        ..view
    };
    assert_eq!(
        unlimited.words(now),
        "No time limit. Sub-agent rounds: 2 of 3."
    );
    let waiting = LimitsView {
        rounds: Rounds::default(),
        run_secs: Some(RUN_DEFAULT_SECS),
        deadline_ms: None,
    };
    assert_eq!(waiting.words(now), "Sub-agent rounds: 0.");
    let kept = LimitsView {
        rounds: Rounds {
            used: 4,
            max: Some(3),
            kept_going: true,
        },
        run_secs: Some(RUN_DEFAULT_SECS),
        deadline_ms: Some(now - 1),
    };
    assert_eq!(
        kept.words(now),
        "Time is up; it stops if it works on without you. Sub-agent rounds: 4, no limit for this goal."
    );
}

#[test]
fn time_left_words_round_up_to_the_minute() {
    assert_eq!(
        time_left_words(0),
        "Time is up; it stops if it works on without you"
    );
    assert_eq!(
        time_left_words(-5),
        "Time is up; it stops if it works on without you"
    );
    assert_eq!(time_left_words(1), "1 min left");
    assert_eq!(time_left_words(59 * 60_000), "59 min left");
    assert_eq!(time_left_words((5 * 60 + 10) * 60_000), "5 h 10 min left");
    assert_eq!(
        time_left_words((6 * 1440 + 23 * 60) * 60_000),
        "6 d 23 h left"
    );
    assert_eq!(time_left_words(7 * DAY_MS), "7 d 0 h left");
}

#[test]
fn every_choice_has_words_and_the_stop_line_names_the_run_time() {
    assert_eq!(rounds_words(None), "Until the goal is done");
    assert_eq!(rounds_words(Some(1)), "1 sub-agent round");
    for max in ROUND_CHOICES.into_iter().skip(1) {
        assert_eq!(rounds_words(Some(max)), format!("{max} sub-agent rounds"));
    }
    let words: Vec<String> = RUN_CHOICES_SECS
        .into_iter()
        .map(|secs| run_words(Some(secs)))
        .collect();
    assert_eq!(words, ["1 day", "3 days", "7 days", "14 days", "30 days"]);
    assert_eq!(run_words(None), "No limit");
    let line = run_stopped_line(RUN_DEFAULT_SECS);
    assert!(line.starts_with(crate::team::master::LIVE_STOPPED));
    assert!(line.contains("it ran 7 days without a message from you"));
}
