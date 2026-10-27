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

/// One synthetic limits row: account, 5-hour used and reset, weekly used and reset.
type LimitsRow<'a> = (&'a str, f64, Option<i64>, f64, Option<i64>);

/// Writes synthetic limits rows into the `account_limits` shape the usage poll keeps.
/// Takes the connection and the rows.
fn limits(connection: &Connection, rows: &[LimitsRow<'_>]) {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS account_limits (
                account_ref TEXT PRIMARY KEY,
                five_hour_percent REAL,
                five_hour_resets_ms INTEGER,
                weekly_percent REAL,
                weekly_resets_ms INTEGER
            )",
        )
        .unwrap();
    for (id, five, five_resets, weekly, weekly_resets) in rows {
        connection
            .execute(
                "INSERT INTO account_limits VALUES (?1,?2,?3,?4,?5)",
                params![
                    format!("{id}-account"),
                    five,
                    five_resets,
                    weekly,
                    weekly_resets
                ],
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
fn limits_read_as_unknown_without_the_table_and_clamp_out_of_range_percents() {
    let (_directory, connection) = database();
    assert_eq!(
        stored_limits(&connection, &reference("a-account")).unwrap(),
        None
    );
    limits(&connection, &[("a", 140.0, None, -3.0, Some(NOW_MS))]);
    assert_eq!(
        stored_limits(&connection, &reference("a-account")).unwrap(),
        Some(AccountLimits {
            five_hour: Some(LimitWindow {
                used_percent: 100.0,
                resets_ms: None
            }),
            weekly: Some(LimitWindow {
                used_percent: 0.0,
                resets_ms: Some(NOW_MS)
            }),
        })
    );
    assert_eq!(
        stored_limits(&connection, &reference("b-account")).unwrap(),
        None
    );
}
