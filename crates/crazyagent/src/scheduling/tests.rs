use super::*;
use crate::config::{
    AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};

const HOUR_MS: i64 = 3_600_000;
const NOW_MS: i64 = 1_800_000_000_000;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn managed(id: &str) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("anthropic"),
        harness: reference("claude"),
        protocol: reference("native"),
        account_ref: reference(&format!("{id}-account")),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("anthropic"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference(&format!("{id}-profile")),
        },
    }
}

fn access(members: &[&str]) -> ModelAccess {
    let connections: Vec<_> = members.iter().map(|id| managed(id)).collect();
    ModelAccess {
        accounts: connections.iter().map(|c| c.account_ref.clone()).collect(),
        connections,
        pools: vec![AccountPool {
            id: reference("pool-a"),
            name: None,
            members: members.iter().map(|id| reference(id)).collect(),
            projects: vec![reference("project-a")],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

fn database() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (directory, connection)
}

fn change(connection: &mut Connection, access: &ModelAccess, change: ScheduleChange) -> Outcome {
    let tx = connection.transaction().unwrap();
    let outcome = apply(&tx, access, &change).unwrap();
    tx.commit().unwrap();
    outcome
}

fn plan(connection: &mut Connection, access: &ModelAccess, id: &str, plan: AccountPlan) {
    let outcome = change(
        connection,
        access,
        ScheduleChange::Account {
            connection: reference(id),
            plan,
        },
    );
    assert_eq!(outcome, Outcome::Applied);
}

/// One synthetic limits row: connection, 5-hour used and reset, weekly used and reset.
type LimitsRow<'a> = (&'a str, f64, Option<i64>, f64, Option<i64>);

/// Records synthetic limits through the usage store the poll writes.
/// Takes the connection and the rows.
fn limits(connection: &Connection, rows: &[LimitsRow<'_>]) {
    readings(connection, rows, crate::usage::LimitSource::Poll, NOW_MS);
}

/// Records synthetic limits through the usage store as read from `source` at `at_ms`.
/// Takes the connection, the rows, the source and the time.
fn readings(
    connection: &Connection,
    rows: &[LimitsRow<'_>],
    source: crate::usage::LimitSource,
    at_ms: i64,
) {
    crate::usage::initialize(connection).unwrap();
    for (id, five, five_resets, weekly, weekly_resets) in rows {
        crate::usage::record_observation(
            connection,
            &reference(id),
            &crate::usage::Observation {
                plan: None,
                five_hour: Some(LimitWindow {
                    used_percent: *five,
                    resets_at_ms: *five_resets,
                }),
                weekly: Some(LimitWindow {
                    used_percent: *weekly,
                    resets_at_ms: *weekly_resets,
                }),
            },
            source,
            at_ms,
        )
        .unwrap();
    }
}

fn candidates(ids: &[&str]) -> Vec<Candidate> {
    ids.iter()
        .map(|id| Candidate {
            connection: reference(id),
            load: 0,
            current: false,
        })
        .collect()
}

fn ordered(choice: &Choice, candidates: &[Candidate]) -> Vec<String> {
    choice
        .order(candidates)
        .ready
        .into_iter()
        .map(|index| candidates[index].connection.as_str().to_owned())
        .collect()
}

fn read(connection: &Connection, access: &ModelAccess) -> Choice {
    Choice::read(
        connection,
        access,
        Some("project-a"),
        &[&reference("pool-a")],
        NOW_MS,
    )
    .unwrap()
}

#[test]
fn owner_choices_are_stored_read_back_and_cleared() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b"]);
    assert_eq!(load(&connection).unwrap(), Schedule::default());
    for edit in [
        ScheduleChange::AppStrategy {
            strategy: Strategy::Balanced,
        },
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: Some(Strategy::ManualRank),
        },
        ScheduleChange::ProjectStrategy {
            project: reference("project-a"),
            strategy: Some(Strategy::UseItOrLoseIt),
        },
    ] {
        assert_eq!(change(&mut connection, &access, edit), Outcome::Applied);
    }
    let kept = AccountPlan {
        rank: Some(2),
        reserve_percent: 15,
        pin: Some(Pin::UseLast),
    };
    plan(&mut connection, &access, "b", kept);
    let schedule = load(&connection).unwrap();
    assert_eq!(
        schedule,
        Schedule {
            strategy: Strategy::Balanced,
            pools: vec![ScopedStrategy {
                id: reference("pool-a"),
                strategy: Strategy::ManualRank,
            }],
            projects: vec![ScopedStrategy {
                id: reference("project-a"),
                strategy: Strategy::UseItOrLoseIt,
            }],
            accounts: vec![AccountEntry {
                connection: reference("b"),
                plan: kept,
            }],
        }
    );
    assert_eq!(schedule.plan(&reference("a")), AccountPlan::default());
    plan(&mut connection, &access, "b", AccountPlan::default());
    change(
        &mut connection,
        &access,
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: None,
        },
    );
    let cleared = load(&connection).unwrap();
    assert!(cleared.accounts.is_empty() && cleared.pools.is_empty());
    assert_eq!(cleared.projects.len(), 1);
}

#[test]
fn out_of_range_values_and_unknown_targets_are_refused() {
    let (_directory, mut connection) = database();
    let access = access(&["a"]);
    let refused =
        |connection: &mut Connection, edit: ScheduleChange| match change(connection, &access, edit)
        {
            Outcome::Rejected { code } => code,
            other => panic!("expected a refusal, got {other:?}"),
        };
    let account = |plan: AccountPlan| ScheduleChange::Account {
        connection: reference("a"),
        plan,
    };
    for (rank, reserve, code) in [
        (Some(0), 0, "account_rank_invalid"),
        (Some(RANK_MAX + 1), 0, "account_rank_invalid"),
        (Some(1), RESERVE_PERCENT_MAX + 1, "account_reserve_invalid"),
    ] {
        let edit = account(AccountPlan {
            rank,
            reserve_percent: reserve,
            pin: None,
        });
        assert!(edit.validate().is_err());
        assert_eq!(refused(&mut connection, edit), code);
    }
    assert_eq!(
        refused(
            &mut connection,
            ScheduleChange::Account {
                connection: reference("missing"),
                plan: AccountPlan {
                    rank: Some(1),
                    ..AccountPlan::default()
                },
            }
        ),
        "connection_not_found"
    );
    assert_eq!(
        refused(
            &mut connection,
            ScheduleChange::PoolStrategy {
                pool: reference("missing"),
                strategy: Some(Strategy::Balanced),
            }
        ),
        "pool_not_found"
    );
    assert_eq!(load(&connection).unwrap(), Schedule::default());
}

#[test]
fn a_project_override_beats_the_pool_strategy_which_beats_the_app_default() {
    let pool = reference("pool-a");
    let mut schedule = Schedule {
        strategy: Strategy::Balanced,
        ..Schedule::default()
    };
    assert_eq!(
        schedule.strategy_for(Some("project-a"), &[&pool]),
        Strategy::Balanced
    );
    schedule.pools.push(ScopedStrategy {
        id: pool.clone(),
        strategy: Strategy::ManualRank,
    });
    assert_eq!(
        schedule.strategy_for(Some("project-a"), &[&pool]),
        Strategy::ManualRank
    );
    schedule.projects.push(ScopedStrategy {
        id: reference("project-a"),
        strategy: Strategy::UseItOrLoseIt,
    });
    assert_eq!(
        schedule.strategy_for(Some("project-a"), &[&pool]),
        Strategy::UseItOrLoseIt
    );
    assert_eq!(
        schedule.strategy_for(Some("project-b"), &[&pool]),
        Strategy::ManualRank
    );
    assert_eq!(
        Schedule::default().strategy_for(None, &[]),
        Strategy::UseItOrLoseIt
    );
}

#[test]
fn use_it_or_lose_it_takes_the_soonest_weekly_reset_first_with_ties_by_rank() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c", "d", "e"]);
    limits(
        &connection,
        &[
            ("a", 10.0, None, 40.0, Some(NOW_MS + 72 * HOUR_MS)),
            ("b", 10.0, None, 40.0, Some(NOW_MS + 24 * HOUR_MS)),
            ("c", 10.0, None, 90.0, Some(NOW_MS + 24 * HOUR_MS)),
            ("d", 10.0, None, 5.0, None),
        ],
    );
    plan(
        &mut connection,
        &access,
        "c",
        AccountPlan {
            rank: Some(1),
            ..AccountPlan::default()
        },
    );
    let choice = read(&connection, &access);
    assert_eq!(choice.strategy, Strategy::UseItOrLoseIt);
    assert_eq!(choice.limits.len(), 4);
    assert_eq!(
        ordered(&choice, &candidates(&["a", "b", "c", "d", "e"])),
        ["c", "b", "a", "d", "e"]
    );
}

#[test]
fn a_reserve_or_a_full_five_hour_window_leaves_the_account_out_until_it_resets() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c", "d"]);
    limits(
        &connection,
        &[
            (
                "a",
                100.0,
                Some(NOW_MS + HOUR_MS),
                10.0,
                Some(NOW_MS + 2 * HOUR_MS),
            ),
            ("b", 20.0, None, 80.0, Some(NOW_MS + 3 * HOUR_MS)),
            ("c", 20.0, None, 79.5, Some(NOW_MS + 4 * HOUR_MS)),
            ("d", 100.0, Some(NOW_MS - 1), 100.0, Some(NOW_MS)),
        ],
    );
    for id in ["b", "c"] {
        plan(
            &mut connection,
            &access,
            id,
            AccountPlan {
                reserve_percent: 20,
                ..AccountPlan::default()
            },
        );
    }
    let choice = read(&connection, &access);
    let list = candidates(&["a", "b", "c", "d"]);
    let order = choice.order(&list);
    assert_eq!(
        order.excluded,
        [
            (
                0,
                Excluded::FiveHourCap {
                    resets_ms: Some(NOW_MS + HOUR_MS)
                }
            ),
            (
                1,
                Excluded::WeeklyReserve {
                    resets_ms: Some(NOW_MS + 3 * HOUR_MS)
                }
            ),
        ]
    );
    assert_eq!(order.excluded[1].1.until_ms(), Some(NOW_MS + 3 * HOUR_MS));
    assert_eq!(ordered(&choice, &list), ["c", "d"]);
    let without_reserve = Choice {
        schedule: Schedule::default(),
        ..choice
    };
    assert_eq!(ordered(&without_reserve, &list), ["b", "c", "d"]);
}

#[test]
fn a_transcript_reading_orders_an_account_after_the_others_but_never_leaves_it_out() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c", "d"]);
    limits(
        &connection,
        &[
            ("c", 20.0, None, 50.0, Some(NOW_MS + 24 * HOUR_MS)),
            (
                "d",
                100.0,
                Some(NOW_MS + 3 * HOUR_MS),
                10.0,
                Some(NOW_MS + 5 * HOUR_MS),
            ),
        ],
    );
    readings(
        &connection,
        &[
            (
                "a",
                100.0,
                Some(NOW_MS + HOUR_MS),
                10.0,
                Some(NOW_MS + HOUR_MS),
            ),
            ("b", 0.0, None, 100.0, Some(NOW_MS + 2 * HOUR_MS)),
        ],
        crate::usage::LimitSource::Transcript,
        NOW_MS,
    );
    let choice = read(&connection, &access);
    let list = candidates(&["a", "b", "c", "d"]);
    assert_eq!(
        choice.order(&list).excluded,
        [(
            3,
            Excluded::FiveHourCap {
                resets_ms: Some(NOW_MS + 3 * HOUR_MS)
            }
        )]
    );
    assert_eq!(ordered(&choice, &list), ["c", "a", "b"]);
    let pools = ranking(&connection, &access, NOW_MS).unwrap();
    assert_eq!(
        standings(&pools[0]),
        [
            ("c", next(Some(NOW_MS + 24 * HOUR_MS), Some(50), false)),
            ("a", next(Some(NOW_MS + HOUR_MS), Some(90), false)),
            ("b", next(Some(NOW_MS + 2 * HOUR_MS), Some(0), false)),
            (
                "d",
                Standing::FiveHourCap {
                    until_ms: Some(NOW_MS + 3 * HOUR_MS),
                }
            ),
        ]
    );
    plan(
        &mut connection,
        &access,
        "c",
        AccountPlan {
            pin: Some(Pin::UseLast),
            ..AccountPlan::default()
        },
    );
    assert_eq!(ordered(&read(&connection, &access), &list), ["a", "b", "c"]);
}

#[test]
fn a_transcript_reading_never_frees_an_account_a_poll_left_out() {
    let (_directory, mut connection) = database();
    let access = access(&["e", "f", "g", "h"]);
    readings(
        &connection,
        &[
            (
                "e",
                100.0,
                Some(NOW_MS + HOUR_MS),
                30.0,
                Some(NOW_MS + 48 * HOUR_MS),
            ),
            (
                "f",
                10.0,
                Some(NOW_MS + HOUR_MS),
                95.0,
                Some(NOW_MS + 48 * HOUR_MS),
            ),
        ],
        crate::usage::LimitSource::Poll,
        NOW_MS - 2 * HOUR_MS,
    );
    readings(
        &connection,
        &[(
            "g",
            100.0,
            Some(NOW_MS - HOUR_MS),
            20.0,
            Some(NOW_MS + 48 * HOUR_MS),
        )],
        crate::usage::LimitSource::Poll,
        NOW_MS - 6 * HOUR_MS,
    );
    limits(
        &connection,
        &[("h", 10.0, None, 60.0, Some(NOW_MS + 72 * HOUR_MS))],
    );
    readings(
        &connection,
        &[
            (
                "e",
                0.0,
                Some(NOW_MS + 5 * HOUR_MS),
                0.0,
                Some(NOW_MS + 48 * HOUR_MS),
            ),
            (
                "f",
                10.0,
                Some(NOW_MS + HOUR_MS),
                96.0,
                Some(NOW_MS + 48 * HOUR_MS),
            ),
            (
                "g",
                100.0,
                Some(NOW_MS + 4 * HOUR_MS),
                25.0,
                Some(NOW_MS + 48 * HOUR_MS),
            ),
        ],
        crate::usage::LimitSource::Transcript,
        NOW_MS,
    );
    plan(
        &mut connection,
        &access,
        "f",
        AccountPlan {
            reserve_percent: 10,
            ..AccountPlan::default()
        },
    );
    let choice = read(&connection, &access);
    let newest = |id: &str| choice.limits[&reference(id)].five_hour_at(NOW_MS);
    assert_eq!(newest("e").map(|window| window.used_percent), Some(0.0));
    assert_eq!(newest("g").map(|window| window.used_percent), Some(100.0));
    let list = candidates(&["e", "f", "g", "h"]);
    assert_eq!(
        choice.order(&list).excluded,
        [
            (
                0,
                Excluded::FiveHourCap {
                    resets_ms: Some(NOW_MS + HOUR_MS)
                }
            ),
            (
                1,
                Excluded::WeeklyReserve {
                    resets_ms: Some(NOW_MS + 48 * HOUR_MS)
                }
            ),
        ]
    );
    assert_eq!(ordered(&choice, &list), ["h", "g"]);
}

#[test]
fn never_use_is_left_out_and_use_last_goes_after_every_other_account() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c"]);
    limits(
        &connection,
        &[
            ("a", 0.0, None, 0.0, Some(NOW_MS + HOUR_MS)),
            ("b", 0.0, None, 0.0, Some(NOW_MS + 2 * HOUR_MS)),
            ("c", 0.0, None, 0.0, Some(NOW_MS + 3 * HOUR_MS)),
        ],
    );
    for (id, pin) in [("a", Pin::UseLast), ("b", Pin::NeverUse)] {
        plan(
            &mut connection,
            &access,
            id,
            AccountPlan {
                rank: Some(1),
                pin: Some(pin),
                ..AccountPlan::default()
            },
        );
    }
    let choice = read(&connection, &access);
    let list = candidates(&["a", "b", "c"]);
    assert_eq!(ordered(&choice, &list), ["c", "a"]);
    assert_eq!(choice.order(&list).excluded, [(1, Excluded::NeverUse)]);
    assert_eq!(Excluded::NeverUse.until_ms(), None);
}

#[test]
fn manual_rank_follows_the_owner_and_balanced_takes_the_least_used_weekly_window() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c", "d"]);
    limits(
        &connection,
        &[
            ("a", 50.0, None, 10.0, Some(NOW_MS + HOUR_MS)),
            ("b", 5.0, None, 60.0, Some(NOW_MS + 9 * HOUR_MS)),
            ("c", 10.0, None, 10.0, Some(NOW_MS + 5 * HOUR_MS)),
        ],
    );
    for (id, rank) in [("b", 1), ("c", 2)] {
        plan(
            &mut connection,
            &access,
            id,
            AccountPlan {
                rank: Some(rank),
                ..AccountPlan::default()
            },
        );
    }
    let list = candidates(&["a", "b", "c", "d"]);
    change(
        &mut connection,
        &access,
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: Some(Strategy::ManualRank),
        },
    );
    assert_eq!(
        ordered(&read(&connection, &access), &list),
        ["b", "c", "a", "d"]
    );
    change(
        &mut connection,
        &access,
        ScheduleChange::ProjectStrategy {
            project: reference("project-a"),
            strategy: Some(Strategy::Balanced),
        },
    );
    assert_eq!(
        ordered(&read(&connection, &access), &list),
        ["c", "a", "b", "d"]
    );
}

#[test]
fn without_limits_or_ranks_the_current_account_then_the_least_loaded_comes_first() {
    let (_directory, connection) = database();
    let access = access(&["a", "b", "c"]);
    let choice = read(&connection, &access);
    assert!(choice.limits.is_empty());
    let mut list = candidates(&["a", "b", "c"]);
    assert_eq!(ordered(&choice, &list), ["a", "b", "c"]);
    list[0].load = 2;
    list[1].load = 1;
    assert_eq!(ordered(&choice, &list), ["c", "b", "a"]);
    list[0].current = true;
    assert_eq!(ordered(&choice, &list), ["a", "c", "b"]);
    assert_eq!(ordered(&Choice::default(), &list), ["a", "c", "b"]);
}

#[test]
fn limits_come_from_the_usage_store_with_the_owners_manual_reset_applied() {
    let (_directory, connection) = database();
    let access = access(&["a", "b"]);
    assert!(read(&connection, &access).limits.is_empty());
    crate::usage::initialize(&connection).unwrap();
    assert!(read(&connection, &access).limits.is_empty());
    limits(
        &connection,
        &[
            ("a", 10.0, None, 50.0, Some(NOW_MS + 5 * HOUR_MS)),
            ("b", 10.0, None, 50.0, Some(NOW_MS + 2 * HOUR_MS)),
        ],
    );
    let list = candidates(&["a", "b"]);
    assert_eq!(ordered(&read(&connection, &access), &list), ["b", "a"]);
    crate::usage::set_reset_override(
        &connection,
        &reference("a"),
        crate::usage::WindowKind::Weekly,
        Some(NOW_MS + HOUR_MS),
    )
    .unwrap();
    let choice = read(&connection, &access);
    assert_eq!(
        choice.limits[&reference("a")].weekly_at(NOW_MS),
        Some(LimitWindow {
            used_percent: 50.0,
            resets_at_ms: Some(NOW_MS + HOUR_MS),
        })
    );
    assert_eq!(ordered(&choice, &list), ["a", "b"]);
}

fn next(resets_ms: Option<i64>, left: Option<u8>, use_last: bool) -> Standing {
    Standing::Next {
        weekly_resets_ms: resets_ms,
        weekly_left_percent: left,
        use_last,
    }
}

fn standings(pool: &PoolRanking) -> Vec<(&str, Standing)> {
    pool.accounts
        .iter()
        .map(|account| (account.connection.as_str(), account.standing))
        .collect()
}

#[test]
fn the_ranking_lists_each_pool_in_pick_order_then_every_held_account_with_why() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b", "c", "d", "e", "f"]);
    crate::team::initialize(&connection).unwrap();
    limits(
        &connection,
        &[
            ("a", 10.0, None, 40.0, Some(NOW_MS + 72 * HOUR_MS)),
            ("b", 10.0, None, 30.0, Some(NOW_MS + 24 * HOUR_MS)),
            ("c", 10.0, None, 85.0, Some(NOW_MS + 48 * HOUR_MS)),
            (
                "d",
                100.0,
                Some(NOW_MS + 2 * HOUR_MS),
                5.0,
                Some(NOW_MS + 6 * HOUR_MS),
            ),
        ],
    );
    for (id, reserve_percent, pin) in [
        ("a", 0, Some(Pin::UseLast)),
        ("b", 10, None),
        ("c", 20, None),
        ("e", 0, Some(Pin::NeverUse)),
    ] {
        plan(
            &mut connection,
            &access,
            id,
            AccountPlan {
                rank: None,
                reserve_percent,
                pin,
            },
        );
    }
    connection
        .execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES ('f',?1),('a',?2)",
            params![NOW_MS + HOUR_MS, NOW_MS - 1],
        )
        .unwrap();
    let pools = ranking(&connection, &access, NOW_MS).unwrap();
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].pool, reference("pool-a"));
    assert_eq!(pools[0].strategy, Strategy::UseItOrLoseIt);
    assert_eq!(
        standings(&pools[0]),
        [
            ("b", next(Some(NOW_MS + 24 * HOUR_MS), Some(60), false)),
            ("a", next(Some(NOW_MS + 72 * HOUR_MS), Some(60), true)),
            (
                "c",
                Standing::ReserveReached {
                    reserve_percent: 20,
                    until_ms: Some(NOW_MS + 48 * HOUR_MS),
                }
            ),
            (
                "d",
                Standing::FiveHourCap {
                    until_ms: Some(NOW_MS + 2 * HOUR_MS),
                }
            ),
            ("e", Standing::NeverUse),
            (
                "f",
                Standing::Paused {
                    until_ms: NOW_MS + HOUR_MS,
                }
            ),
        ]
    );
    change(
        &mut connection,
        &access,
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: Some(Strategy::Balanced),
        },
    );
    let balanced = ranking(&connection, &access, NOW_MS).unwrap();
    assert_eq!(balanced[0].strategy, Strategy::Balanced);
    assert_eq!(
        standings(&balanced[0])[..2],
        [
            ("b", next(Some(NOW_MS + 24 * HOUR_MS), Some(60), false)),
            ("a", next(Some(NOW_MS + 72 * HOUR_MS), Some(60), true)),
        ]
    );
}

#[test]
fn before_any_usage_read_or_pause_every_account_is_next_in_pool_order() {
    let (_directory, connection) = database();
    let access = access(&["a", "b"]);
    let pools = ranking(&connection, &access, NOW_MS).unwrap();
    assert_eq!(
        standings(&pools[0]),
        [
            ("a", next(None, None, false)),
            ("b", next(None, None, false))
        ]
    );
    assert!(
        ranking(&connection, &ModelAccess::default(), NOW_MS)
            .unwrap()
            .is_empty()
    );
}
