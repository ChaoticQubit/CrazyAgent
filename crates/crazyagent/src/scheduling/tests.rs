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

pub(crate) fn access(members: &[&str]) -> ModelAccess {
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

pub(crate) fn database() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    (directory, connection)
}

/// Registers a project the way setup does, so a project's override has a target. Inputs: the
/// store and the project id. Output: none.
fn known_project(connection: &Connection, id: &str) {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    connection
        .execute("INSERT INTO projects (id, body) VALUES (?1, '{}')", [id])
        .unwrap();
}

fn change(connection: &mut Connection, access: &ModelAccess, change: ScheduleChange) -> Outcome {
    let tx = connection.transaction().unwrap();
    let version = load(&tx).unwrap().version;
    let outcome = apply(&tx, access, &change, version).unwrap();
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
                provider: Some(crate::usage::poll::Provider::Claude),
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
    known_project(&connection, "project-a");
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
            version: 4,
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
fn a_strategy_for_a_project_that_does_not_exist_is_refused_and_can_still_be_cleared() {
    let (_directory, mut connection) = database();
    let access = access(&["a"]);
    known_project(&connection, "another-project");
    let edit = |strategy| ScheduleChange::ProjectStrategy {
        project: reference("project-gone"),
        strategy,
    };
    assert_eq!(
        change(&mut connection, &access, edit(Some(Strategy::Balanced))),
        Outcome::Rejected {
            code: "project_not_found".into()
        }
    );
    assert_eq!(load(&connection).unwrap(), Schedule::default());
    assert_eq!(
        change(&mut connection, &access, edit(None)),
        Outcome::Applied
    );
}

#[test]
fn removing_a_project_with_an_override_moves_the_schedule_version_once() {
    let (_directory, mut connection) = database();
    let access = access(&["a"]);
    known_project(&connection, "project-a");
    change(
        &mut connection,
        &access,
        ScheduleChange::ProjectStrategy {
            project: reference("project-a"),
            strategy: Some(Strategy::Balanced),
        },
    );
    let version = |connection: &Connection| load(connection).unwrap().version;
    let forget = |connection: &mut Connection| {
        let tx = connection.transaction().unwrap();
        forget_project(&tx, "project-a").unwrap();
        tx.commit().unwrap();
    };
    let before = version(&connection);
    forget(&mut connection);
    assert_eq!(version(&connection), before + 1);
    forget(&mut connection);
    assert_eq!(version(&connection), before + 1);
}

#[test]
fn removing_a_pool_or_connection_moves_the_schedule_version_once_and_no_removal_leaves_it() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b"]);
    for edit in [
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: Some(Strategy::Balanced),
        },
        ScheduleChange::Account {
            connection: reference("b"),
            plan: AccountPlan {
                rank: Some(1),
                ..AccountPlan::default()
            },
        },
    ] {
        assert_eq!(change(&mut connection, &access, edit), Outcome::Applied);
    }
    let remove = |connection: &mut Connection, access: &ModelAccess| {
        let tx = connection.transaction().unwrap();
        forget_removed(&tx, access).unwrap();
        tx.commit().unwrap();
    };
    let before = load(&connection).unwrap().version;
    remove(&mut connection, &access);
    assert_eq!(load(&connection).unwrap().version, before);
    let mut fewer = access.clone();
    fewer.pools.clear();
    fewer.connections.retain(|known| known.id.as_str() == "a");
    remove(&mut connection, &fewer);
    let after = load(&connection).unwrap();
    assert_eq!((after.pools.len(), after.accounts.len()), (0, 0));
    assert_eq!(after.version, before + 1);
    remove(&mut connection, &fewer);
    assert_eq!(load(&connection).unwrap().version, before + 1);
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
fn stale_account_schedule_write_is_refused() {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b"]);
    let at = |connection: &mut Connection, edit: &ScheduleChange, version: u64| {
        let tx = connection.transaction().unwrap();
        let outcome = apply(&tx, &access, edit, version).unwrap();
        tx.commit().unwrap();
        outcome
    };
    let stale = Outcome::Rejected {
        code: "stale_revision".into(),
    };
    let balanced = ScheduleChange::AppStrategy {
        strategy: Strategy::Balanced,
    };
    assert_eq!(at(&mut connection, &balanced, 1), stale);
    assert_eq!(load(&connection).unwrap(), Schedule::default());
    let unknown = ScheduleChange::PoolStrategy {
        pool: reference("pool-z"),
        strategy: Some(Strategy::ManualRank),
    };
    assert_eq!(
        at(&mut connection, &unknown, 7),
        Outcome::Rejected {
            code: "pool_not_found".into()
        }
    );
    let too_much = ScheduleChange::Account {
        connection: reference("a"),
        plan: AccountPlan {
            reserve_percent: 101,
            ..AccountPlan::default()
        },
    };
    assert_eq!(
        at(&mut connection, &too_much, 7),
        Outcome::Rejected {
            code: "account_reserve_invalid".into()
        }
    );

    assert_eq!(at(&mut connection, &balanced, 0), Outcome::Applied);
    let saved = load(&connection).unwrap();
    assert_eq!(saved.strategy, Strategy::Balanced);
    assert_eq!(saved.version, 1);
    let manual = ScheduleChange::AppStrategy {
        strategy: Strategy::ManualRank,
    };
    assert_eq!(at(&mut connection, &manual, 0), stale);
    let ranked = ScheduleChange::Account {
        connection: reference("a"),
        plan: AccountPlan {
            rank: Some(1),
            ..AccountPlan::default()
        },
    };
    assert_eq!(at(&mut connection, &ranked, 0), stale);
    assert_eq!(at(&mut connection, &ranked, 2), stale);
    assert_eq!(load(&connection).unwrap(), saved);

    assert_eq!(at(&mut connection, &manual, 1), Outcome::Applied);
    assert_eq!(at(&mut connection, &ranked, 2), Outcome::Applied);
    let redone = load(&connection).unwrap();
    assert_eq!(redone.strategy, Strategy::ManualRank);
    assert_eq!(redone.plan(&reference("a")).rank, Some(1));
    assert_eq!(redone.version, 3);
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
    let pools = ranked(&connection, &access);
    assert_eq!(
        standings(&pools[0]),
        [
            (
                "c",
                next(
                    Some(NOW_MS + 24 * HOUR_MS),
                    Some(50),
                    false,
                    OrderWhy::ResetsSoonest {
                        resets_ms: NOW_MS + 24 * HOUR_MS,
                    }
                )
            ),
            (
                "a",
                next(
                    Some(NOW_MS + HOUR_MS),
                    Some(90),
                    false,
                    OrderWhy::LimitReported
                )
            ),
            (
                "b",
                next(
                    Some(NOW_MS + 2 * HOUR_MS),
                    Some(0),
                    false,
                    OrderWhy::LimitReported
                )
            ),
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
    known_project(&connection, "project-a");
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

fn next(resets_ms: Option<i64>, left: Option<u8>, use_last: bool, why: OrderWhy) -> Standing {
    Standing::Next {
        weekly_resets_ms: resets_ms,
        weekly_left_percent: left,
        use_last,
        why,
    }
}

/// Ranks the pools inside a transaction, as the owner read does. Takes the store and the model
/// access. Returns the rankings.
fn ranked(connection: &Connection, access: &ModelAccess) -> Vec<PoolRanking> {
    let tx = connection.unchecked_transaction().unwrap();
    ranking(&tx, access, NOW_MS).unwrap()
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
    let pools = ranked(&connection, &access);
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].pool, reference("pool-a"));
    assert_eq!(pools[0].strategy, Strategy::UseItOrLoseIt);
    assert_eq!(
        standings(&pools[0]),
        [
            (
                "b",
                next(
                    Some(NOW_MS + 24 * HOUR_MS),
                    Some(60),
                    false,
                    OrderWhy::ResetsSoonest {
                        resets_ms: NOW_MS + 24 * HOUR_MS,
                    }
                )
            ),
            (
                "a",
                next(
                    Some(NOW_MS + 72 * HOUR_MS),
                    Some(60),
                    true,
                    OrderWhy::UseLast
                )
            ),
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
    let balanced = ranked(&connection, &access);
    assert_eq!(balanced[0].strategy, Strategy::Balanced);
    assert_eq!(
        standings(&balanced[0])[..2],
        [
            (
                "b",
                next(
                    Some(NOW_MS + 24 * HOUR_MS),
                    Some(60),
                    false,
                    OrderWhy::LeastUsed
                )
            ),
            (
                "a",
                next(
                    Some(NOW_MS + 72 * HOUR_MS),
                    Some(60),
                    true,
                    OrderWhy::UseLast
                )
            ),
        ]
    );
}

#[test]
fn before_any_usage_read_or_pause_every_account_is_next_in_pool_order() {
    let (_directory, connection) = database();
    let access = access(&["a", "b"]);
    let pools = ranked(&connection, &access);
    assert_eq!(
        standings(&pools[0]),
        [
            ("a", next(None, None, false, OrderWhy::NoReading)),
            ("b", next(None, None, false, OrderWhy::NoReading))
        ]
    );
    assert!(ranked(&connection, &ModelAccess::default()).is_empty());
}

const RESET_MS: i64 = NOW_MS + 24 * HOUR_MS;
const LATER_RESET_MS: i64 = NOW_MS + 72 * HOUR_MS;

/// The reason each ready account of the only pool carries, in the real order. Input: the
/// ranking. Output: account and reason pairs.
fn reasons(pools: &[PoolRanking]) -> Vec<(String, OrderWhy)> {
    assert_eq!(pools.len(), 1);
    pools[0]
        .accounts
        .iter()
        .filter_map(|account| match account.standing {
            Standing::Next { why, .. } => Some((account.connection.as_str().to_owned(), why)),
            _ => None,
        })
        .collect()
}

/// Ranks accounts a and b through the real engine read. Inputs: the pool's strategy, the
/// usage readings and each account's rank. Output: the reason each ready account carries.
fn why_under(
    strategy: Strategy,
    rows: &[LimitsRow<'_>],
    ranks: [Option<u16>; 2],
) -> Vec<(String, OrderWhy)> {
    let (_directory, mut connection) = database();
    let access = access(&["a", "b"]);
    limits(&connection, rows);
    change(
        &mut connection,
        &access,
        ScheduleChange::PoolStrategy {
            pool: reference("pool-a"),
            strategy: Some(strategy),
        },
    );
    for (id, rank) in ["a", "b"].into_iter().zip(ranks) {
        plan(
            &mut connection,
            &access,
            id,
            AccountPlan {
                rank,
                reserve_percent: 0,
                pin: None,
            },
        );
    }
    reasons(&ranked(&connection, &access))
}

/// Two reasons with the accounts named, for comparing with `why_under`. Inputs: the first
/// and second account with their reasons. Output: the pairs.
fn pair(first: (&str, OrderWhy), second: (&str, OrderWhy)) -> Vec<(String, OrderWhy)> {
    vec![
        (first.0.to_owned(), first.1),
        (second.0.to_owned(), second.1),
    ]
}

const SOONEST: OrderWhy = OrderWhy::ResetsSoonest {
    resets_ms: RESET_MS,
};

#[test]
fn use_it_or_lose_it_says_which_account_resets_soonest_and_which_later() {
    assert_eq!(
        why_under(
            Strategy::UseItOrLoseIt,
            &[
                ("a", 10.0, None, 40.0, Some(LATER_RESET_MS)),
                ("b", 10.0, None, 30.0, Some(RESET_MS)),
            ],
            [None, None],
        ),
        pair(
            ("b", SOONEST),
            (
                "a",
                OrderWhy::ResetsLater {
                    resets_ms: LATER_RESET_MS,
                }
            ),
        )
    );
}

#[test]
fn balanced_says_which_account_is_least_used_and_which_more() {
    assert_eq!(
        why_under(
            Strategy::Balanced,
            &[
                ("a", 10.0, None, 40.0, Some(RESET_MS)),
                ("b", 10.0, None, 10.0, Some(RESET_MS)),
            ],
            [None, None],
        ),
        pair(("b", OrderWhy::LeastUsed), ("a", OrderWhy::MoreUsed))
    );
}

#[test]
fn balanced_with_equal_weekly_use_names_the_five_hour_use_that_set_them_apart() {
    assert_eq!(
        why_under(
            Strategy::Balanced,
            &[
                ("a", 30.0, None, 40.0, Some(RESET_MS)),
                ("b", 10.0, None, 40.0, Some(RESET_MS)),
            ],
            [None, None],
        ),
        pair(
            ("b", OrderWhy::LeastUsed),
            ("a", OrderWhy::MoreFiveHourUsed)
        )
    );
}

#[test]
fn manual_rank_says_the_rank_and_an_unranked_account_says_it_has_none() {
    assert_eq!(
        why_under(Strategy::ManualRank, &[], [None, Some(2)]),
        pair(
            ("b", OrderWhy::Ranked { rank: 2 }),
            ("a", OrderWhy::Unranked),
        )
    );
}

#[test]
fn a_later_row_with_the_same_reset_says_its_rank_set_it_apart() {
    let same = [
        ("a", 10.0, None, 40.0, Some(RESET_MS)),
        ("b", 10.0, None, 40.0, Some(RESET_MS)),
    ];
    assert_eq!(
        why_under(Strategy::UseItOrLoseIt, &same, [Some(2), Some(1)]),
        pair(("b", SOONEST), ("a", OrderWhy::TiedRank { rank: Some(2) }),)
    );
    assert_eq!(
        why_under(Strategy::UseItOrLoseIt, &same, [None, Some(1)]),
        pair(("b", SOONEST), ("a", OrderWhy::TiedRank { rank: None }))
    );
    assert_eq!(
        why_under(Strategy::Balanced, &same, [None, Some(1)]),
        pair(
            ("b", OrderWhy::LeastUsed),
            ("a", OrderWhy::TiedRank { rank: None }),
        )
    );
}

#[test]
fn a_later_row_with_nothing_to_set_it_apart_keeps_its_pool_order() {
    let same = [
        ("a", 10.0, None, 40.0, Some(RESET_MS)),
        ("b", 10.0, None, 40.0, Some(RESET_MS)),
    ];
    assert_eq!(
        why_under(Strategy::UseItOrLoseIt, &same, [None, None]),
        pair(("a", SOONEST), ("b", OrderWhy::TiedOrder))
    );
    assert_eq!(
        why_under(Strategy::Balanced, &same, [Some(3), Some(3)]),
        pair(("a", OrderWhy::LeastUsed), ("b", OrderWhy::TiedOrder))
    );
}

#[test]
fn an_account_with_no_reading_says_so_under_the_strategies_that_need_one() {
    for strategy in [Strategy::UseItOrLoseIt, Strategy::Balanced] {
        assert_eq!(
            why_under(strategy, &[], [None, None]),
            pair(("a", OrderWhy::NoReading), ("b", OrderWhy::NoReading))
        );
    }
}

/// A store with the real sign-in tables, where a native account is not signed in until a
/// row records it, running under the engine run these rows prove sign-ins in. Output: the
/// store's folder and connection.
fn signed_out_database() -> (tempfile::TempDir, Connection) {
    let (directory, connection) = database();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS owner_meta(singleton INTEGER PRIMARY KEY,
             schema_version INTEGER,db_instance TEXT,engine_epoch TEXT,runtime_state TEXT)",
        )
        .unwrap();
    let run = this_run();
    connection
        .execute(
            "INSERT OR REPLACE INTO owner_meta(singleton,schema_version,db_instance,engine_epoch,runtime_state) VALUES (1,1,?1,?2,'active')",
            params![run.db_instance, run.engine_epoch],
        )
        .unwrap();
    assert_eq!(
        connection
            .execute("UPDATE state SET revision=MAX(revision,1) WHERE id=1", [])
            .unwrap(),
        1,
        "the store keeps one revision row"
    );
    (directory, connection)
}

/// The engine run these rows read under. Output: its host stamp.
fn this_run() -> crate::engine::owner::HostStamp {
    crate::engine::owner::HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "b".repeat(64),
    }
}

/// Records one account's latest sign-in as the native sign-in flow stores it, under this run.
/// Inputs: the store, the account, its action and the phase it ended in. Output: none.
fn sign_in_ended(
    connection: &Connection,
    id: &str,
    action: crate::engine::owner::NativeAuthAction,
    phase: crate::engine::owner::NativeAuthPhase,
) {
    sign_in_under(connection, id, action, phase, &this_run().engine_epoch);
}

/// Records one account's latest sign-in as the native sign-in flow stores it. Inputs: the
/// store, the account, its action, the phase it ended in and the engine run it ended under.
/// Output: none.
fn sign_in_under(
    connection: &Connection,
    id: &str,
    action: crate::engine::owner::NativeAuthAction,
    phase: crate::engine::owner::NativeAuthPhase,
    epoch: &str,
) {
    use crate::engine::owner::{
        NativeAuthAction, NativeAuthFact, NativeAuthMethod, NativeAuthPhase, NativeAuthScope,
        NativeAuthStart, NativeProfileBinding,
    };
    let model = managed(id);
    let ModelAuthentication::NativeManaged { profile_ref } = &model.auth else {
        unreachable!("every account in these rows is native managed");
    };
    let column = match phase {
        NativeAuthPhase::AuthenticatedObserved => "authenticated_observed",
        NativeAuthPhase::UnauthenticatedObserved => "unauthenticated_observed",
        other => unreachable!("these rows end a sign-in, not {other:?}"),
    };
    let operation = format!("operation-{id}");
    let method = match action {
        NativeAuthAction::SignIn { method } => method,
        _ => NativeAuthMethod::ClaudeOfficial,
    };
    let request = serde_json::to_vec(&NativeAuthStart {
        request_id: crate::protocol::RequestId::new(operation.clone()).unwrap(),
        scope: NativeAuthScope {
            expected_database_instance: "a".repeat(64),
            connection: model.id.clone(),
            expected_config_id: crate::protocol::ConfigId::new("c".repeat(64)).unwrap(),
            expected_source_hash: "d".repeat(64),
        },
        method,
    })
    .unwrap();
    let fact = crate::engine::owner::native_auth::encode_fact(&NativeAuthFact {
        operation_id: crate::protocol::RequestId::new(operation.clone()).unwrap(),
        action,
        binding: NativeProfileBinding {
            database_instance: "a".repeat(64),
            engine_epoch: epoch.to_owned(),
            config_id: crate::protocol::ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: crate::protocol::Cursor::new(1),
            connection: model.clone(),
            custody_id: "e".repeat(64),
        },
        generation: 1,
        phase,
        code: None,
    })
    .unwrap();
    connection
        .execute(
            "INSERT INTO native_auth_operations(id,profile_key,generation,phase,request,fact)
             VALUES (?1,?2,1,?3,?4,?5)",
            params![
                operation,
                crate::memory::hash(profile_ref.as_str().as_bytes()),
                column,
                request,
                fact
            ],
        )
        .unwrap();
}

/// Records a finished sign-in that ended authenticated. Inputs: the store and the account.
/// Output: none.
fn signed_in(connection: &Connection, id: &str) {
    sign_in_ended(
        connection,
        id,
        crate::engine::owner::NativeAuthAction::SignIn {
            method: crate::engine::owner::NativeAuthMethod::ClaudeOfficial,
        },
        crate::engine::owner::NativeAuthPhase::AuthenticatedObserved,
    );
}

/// Records a provider's rejection of an account's sign-in as the model gateway stores it.
/// Inputs: the store and the account. Output: none.
fn provider_rejected(connection: &Connection, id: &str) {
    let model = managed(id);
    let at_ms = u64::try_from(NOW_MS).unwrap();
    let observation = crate::model_gateway::Observation {
        id: crate::protocol::RequestId::new(format!("rejected-{id}")).unwrap(),
        connection: model.clone(),
        sequence: 1,
        observed_at_ms: at_ms,
        valid_until_ms: at_ms + 1,
        kind: crate::model_gateway::ObservationKind::InvalidAuthentication,
    };
    connection
        .execute(
            "INSERT INTO model_gateway_observations(id,connection_revision,sequence,body)
             VALUES (?1,?2,1,?3)",
            params![
                observation.id.as_str(),
                crate::memory::hash(&serde_json::to_vec(&model).unwrap()),
                serde_json::to_string(&observation).unwrap()
            ],
        )
        .unwrap();
}

/// Every account's standing in the only pool's ranking, in ranking order. Inputs: the store
/// and the access. Output: the standings.
fn held_as(connection: &Connection, access: &ModelAccess) -> Vec<Standing> {
    ranked(connection, access)[0]
        .accounts
        .iter()
        .map(|account| account.standing)
        .collect()
}

#[test]
fn an_account_with_no_sign_in_is_held_as_not_signed_in_and_a_signed_in_one_is_next() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a", "b"]);
    signed_in(&connection, "b");
    assert_eq!(
        standings(&ranked(&connection, &access)[0]),
        [
            ("b", next(None, None, false, OrderWhy::NoReading)),
            ("a", Standing::NotSignedIn),
        ]
    );
}

#[test]
fn never_use_is_what_a_never_used_account_reads_even_when_it_is_not_signed_in() {
    let (_directory, mut connection) = signed_out_database();
    let access = access(&["a"]);
    plan(
        &mut connection,
        &access,
        "a",
        AccountPlan {
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        },
    );
    assert_eq!(held_as(&connection, &access), [Standing::NeverUse]);
}

#[test]
fn not_signed_in_is_what_a_paused_account_reads_when_it_has_no_sign_in() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a"]);
    connection
        .execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES ('a',?1)",
            [NOW_MS + HOUR_MS],
        )
        .unwrap();
    assert_eq!(held_as(&connection, &access), [Standing::NotSignedIn]);
}

#[test]
fn a_sign_in_a_provider_rejected_reads_not_signed_in() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a"]);
    signed_in(&connection, "a");
    provider_rejected(&connection, "a");
    assert_eq!(held_as(&connection, &access), [Standing::NotSignedIn]);
}

#[test]
fn a_latest_sign_in_that_did_not_end_authenticated_reads_not_signed_in() {
    use crate::engine::owner::{NativeAuthAction, NativeAuthMethod, NativeAuthPhase};
    let (_directory, connection) = signed_out_database();
    let access = access(&["a", "b"]);
    sign_in_ended(
        &connection,
        "a",
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::ClaudeOfficial,
        },
        NativeAuthPhase::UnauthenticatedObserved,
    );
    sign_in_ended(
        &connection,
        "b",
        NativeAuthAction::Logout,
        NativeAuthPhase::AuthenticatedObserved,
    );
    assert_eq!(
        held_as(&connection, &access),
        [Standing::NotSignedIn, Standing::NotSignedIn]
    );
}

/// Holds an account's sign-in for an account check as the engine does when it could not use
/// the credential locally. Inputs: the store and the account. Output: none; it panics unless
/// the sign-in passed the pick's check and is now held.
fn doubted(connection: &Connection, id: &str) {
    signed_in(connection, id);
    let tx = connection.unchecked_transaction().unwrap();
    crate::storage::doubt_sign_in(&tx, &this_run(), &managed(id)).unwrap();
    tx.commit().unwrap();
    let held: i64 = connection
        .query_row("SELECT COUNT(*) FROM native_auth_doubts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        held, 1,
        "only a sign-in that passes the pick can be doubted"
    );
}

/// Records a sign-in whose profile a check still holds. Inputs: the store and the account.
/// Output: none.
fn fenced(connection: &Connection, id: &str) {
    signed_in(connection, id);
    let ModelAuthentication::NativeManaged { profile_ref } = &managed(id).auth else {
        unreachable!("every account in these rows is native managed");
    };
    connection
        .execute(
            "INSERT INTO native_auth_fences(profile_key,operation_id,generation,db_instance,
             engine_epoch,custody_id,owner_id,owner_epoch) VALUES (?1,'operation-a',1,?2,?2,?2,?2,1)",
            params![
                crate::memory::hash(profile_ref.as_str().as_bytes()),
                "a".repeat(64)
            ],
        )
        .unwrap();
}

/// Records a sign-in proved under an earlier engine run. Inputs: the store and the account.
/// Output: none.
fn from_an_earlier_run(connection: &Connection, id: &str) {
    use crate::engine::owner::{NativeAuthAction, NativeAuthMethod, NativeAuthPhase};
    sign_in_under(
        connection,
        id,
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::ClaudeOfficial,
        },
        NativeAuthPhase::AuthenticatedObserved,
        &"f".repeat(64),
    );
}

#[test]
fn a_doubted_sign_in_does_not_read_next() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a"]);
    doubted(&connection, "a");
    assert_eq!(held_as(&connection, &access), [Standing::SignInUnconfirmed]);
}

#[test]
fn a_profile_a_check_still_holds_does_not_read_next() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a"]);
    fenced(&connection, "a");
    assert_eq!(held_as(&connection, &access), [Standing::SignInUnconfirmed]);
}

#[test]
fn a_sign_in_proved_under_an_earlier_engine_run_does_not_read_next() {
    let (_directory, connection) = signed_out_database();
    let access = access(&["a"]);
    from_an_earlier_run(&connection, "a");
    assert_eq!(held_as(&connection, &access), [Standing::SignInUnconfirmed]);
}

#[test]
fn a_sign_in_that_needs_a_check_reads_so_over_a_pause_and_never_use_over_it() {
    let (_directory, mut connection) = signed_out_database();
    let access = access(&["a", "b"]);
    fenced(&connection, "a");
    from_an_earlier_run(&connection, "b");
    connection
        .execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES ('a',?1)",
            [NOW_MS + HOUR_MS],
        )
        .unwrap();
    plan(
        &mut connection,
        &access,
        "b",
        AccountPlan {
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        },
    );
    assert_eq!(
        held_as(&connection, &access),
        [Standing::SignInUnconfirmed, Standing::NeverUse]
    );
}

#[test]
fn the_ranking_reads_next_exactly_when_the_real_pick_would_take_the_account() {
    use crate::engine::owner::{NativeAuthAction, NativeAuthMethod, NativeAuthPhase};
    type Case = (&'static str, bool, fn(&Connection));
    let cases: [Case; 7] = [
        ("proved", true, |store| signed_in(store, "a")),
        ("doubted", false, |store| doubted(store, "a")),
        ("fenced", false, |store| fenced(store, "a")),
        ("earlier run", false, |store| {
            from_an_earlier_run(store, "a")
        }),
        ("no sign-in", false, |_| {}),
        ("rejected", false, |store| {
            signed_in(store, "a");
            provider_rejected(store, "a");
        }),
        ("unauthenticated", false, |store| {
            sign_in_ended(
                store,
                "a",
                NativeAuthAction::SignIn {
                    method: NativeAuthMethod::ClaudeOfficial,
                },
                NativeAuthPhase::UnauthenticatedObserved,
            );
        }),
    ];
    for (state, taken, seed) in cases {
        let (_directory, connection) = signed_out_database();
        let access = access(&["a"]);
        seed(&connection);
        assert_eq!(
            crate::research::accounts::managed_sign_in(&connection, &managed("a")).is_ok(),
            taken,
            "the real pick on {state}"
        );
        assert_eq!(
            matches!(held_as(&connection, &access)[0], Standing::Next { .. }),
            taken,
            "the ranking on {state}"
        );
    }
}

#[test]
fn an_account_with_no_native_sign_in_is_never_held_for_one() {
    let (_directory, connection) = signed_out_database();
    let mut access = access(&["a"]);
    access.connections[0].auth = ModelAuthentication::ApiKey {
        credential_ref: reference("a-key"),
    };
    assert_eq!(
        held_as(&connection, &access),
        [next(None, None, false, OrderWhy::NoReading)]
    );
}

#[test]
fn a_ranking_counts_only_this_pools_projects_that_set_their_own_order() {
    let (_directory, mut connection) = database();
    let access = access(&["a"]);
    for id in ["project-a", "project-b"] {
        known_project(&connection, id);
    }
    let count = |connection: &Connection| ranked(connection, &access)[0].own_order_projects;
    assert_eq!(count(&connection), 0);
    change(
        &mut connection,
        &access,
        ScheduleChange::ProjectStrategy {
            project: reference("project-b"),
            strategy: Some(Strategy::Balanced),
        },
    );
    assert_eq!(count(&connection), 0);
    change(
        &mut connection,
        &access,
        ScheduleChange::ProjectStrategy {
            project: reference("project-a"),
            strategy: Some(Strategy::Balanced),
        },
    );
    assert_eq!(count(&connection), 1);
}

#[test]
#[should_panic(expected = "the order sorted by these keys")]
fn a_why_read_against_a_key_that_should_follow_it_is_refused() {
    let first: SortKey = (false, false, (false, 0, 0), 1, true, 0, 0);
    let second: SortKey = (false, false, (false, 0, 0), 1, true, 0, 1);
    Choice::default().why(&first, Some(&second));
}
