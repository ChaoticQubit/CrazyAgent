use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, Outcome};
use crate::usage::{AccountLimits, LimitWindow};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The highest rank the owner can give an account; rank 1 is tried first.
pub const RANK_MAX: u16 = 999;
/// The largest weekly reserve in percent. A full reserve is the `never_use` pin.
pub const RESERVE_PERCENT_MAX: u8 = 99;
/// The most pool or project strategy rows the engine keeps.
pub const SCOPED_STRATEGIES_MAX: usize = 4096;
/// The most per-account plans the engine keeps.
pub const ACCOUNT_PLANS_MAX: usize = 4096;

/// How a pool orders the accounts that can take work (DEC-096 item 8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    #[default]
    UseItOrLoseIt,
    ManualRank,
    Balanced,
}

impl Strategy {
    /// Every strategy in the order the owner sees them.
    pub const ALL: [Self; 3] = [Self::UseItOrLoseIt, Self::ManualRank, Self::Balanced];

    /// The plain name the owner sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::UseItOrLoseIt => "Use it or lose it",
            Self::ManualRank => "Manual rank",
            Self::Balanced => "Balanced",
        }
    }

    /// The stored code of this strategy.
    pub fn code(self) -> &'static str {
        match self {
            Self::UseItOrLoseIt => "use_it_or_lose_it",
            Self::ManualRank => "manual_rank",
            Self::Balanced => "balanced",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|strategy| strategy.code() == code)
    }
}

/// An owner pin on one account: never give it work, or only after every other account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pin {
    NeverUse,
    UseLast,
}

impl Pin {
    /// The plain name the owner sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeverUse => "Never use",
            Self::UseLast => "Use last",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::NeverUse => "never_use",
            Self::UseLast => "use_last",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        [Self::NeverUse, Self::UseLast]
            .into_iter()
            .find(|pin| pin.code() == code)
    }
}

/// The owner's choices for one account: its rank (1 first, none after every ranked
/// account), the weekly percent kept back (0 uses it to 100%) and an optional pin.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPlan {
    #[serde(default)]
    pub rank: Option<u16>,
    #[serde(default)]
    pub reserve_percent: u8,
    #[serde(default)]
    pub pin: Option<Pin>,
}

impl AccountPlan {
    fn validate(&self) -> Result<(), &'static str> {
        if self.rank.is_some_and(|rank| rank == 0 || rank > RANK_MAX) {
            return Err("account_rank_invalid");
        }
        if self.reserve_percent > RESERVE_PERCENT_MAX {
            return Err("account_reserve_invalid");
        }
        Ok(())
    }
}

/// A strategy the owner set for one pool or one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedStrategy {
    pub id: CredentialRef,
    pub strategy: Strategy,
}

/// The plan the owner set for one pool account, keyed by its connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEntry {
    pub connection: CredentialRef,
    pub plan: AccountPlan,
}

/// Every stored scheduling choice: the app default strategy, pool strategies, project
/// overrides and account plans, each list sorted by id.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub strategy: Strategy,
    pub pools: Vec<ScopedStrategy>,
    pub projects: Vec<ScopedStrategy>,
    pub accounts: Vec<AccountEntry>,
}

impl Schedule {
    /// The strategy that orders work for a project: its override, else the first of its
    /// pools with a strategy, else the app default. Takes the project and its pools.
    pub fn strategy_for(&self, project: Option<&str>, pools: &[&CredentialRef]) -> Strategy {
        let scoped = |list: &[ScopedStrategy], id: &str| {
            list.iter()
                .find(|entry| entry.id.as_str() == id)
                .map(|entry| entry.strategy)
        };
        project
            .and_then(|project| scoped(&self.projects, project))
            .or_else(|| {
                pools
                    .iter()
                    .find_map(|pool| scoped(&self.pools, pool.as_str()))
            })
            .unwrap_or(self.strategy)
    }

    /// The owner's plan for one account, the default plan when none is stored.
    pub fn plan(&self, connection: &CredentialRef) -> AccountPlan {
        self.accounts
            .iter()
            .find(|entry| &entry.connection == connection)
            .map_or_else(AccountPlan::default, |entry| entry.plan)
    }
}

/// One owner change to the scheduling choices. A pool or project strategy of none and a
/// default account plan remove the stored row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleChange {
    AppStrategy {
        strategy: Strategy,
    },
    PoolStrategy {
        pool: CredentialRef,
        strategy: Option<Strategy>,
    },
    ProjectStrategy {
        project: CredentialRef,
        strategy: Option<Strategy>,
    },
    Account {
        connection: CredentialRef,
        plan: AccountPlan,
    },
}

impl ScheduleChange {
    /// Checks the change's own bounds before it reaches the engine.
    /// Returns the plain refusal code for a rank or reserve out of range.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::Account { plan, .. } => plan.validate().map_err(error),
            _ => Ok(()),
        }
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn rejected(code: &str) -> Outcome {
    Outcome::Rejected { code: code.into() }
}

/// Creates the scheduling tables when the engine database opens.
/// Takes the engine connection. Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS account_strategies (
                scope TEXT NOT NULL CHECK (scope IN ('app','pool','project')),
                id TEXT NOT NULL,
                strategy TEXT NOT NULL,
                PRIMARY KEY (scope, id)
            );
            CREATE TABLE IF NOT EXISTS account_plans (
                connection TEXT PRIMARY KEY,
                rank INTEGER,
                reserve_percent INTEGER NOT NULL,
                pin TEXT
            );",
        )
        .map_err(error)
}

/// Applies one owner scheduling change inside the engine transaction.
/// Takes the transaction, the current model access and the change. Returns Applied, or a
/// rejection for an unknown pool or connection, a value out of range or a full table.
pub fn apply(
    tx: &Transaction<'_>,
    access: &ModelAccess,
    change: &ScheduleChange,
) -> Result<Outcome, EngineError> {
    if let Err(refusal) = change.validate() {
        return Ok(rejected(&refusal.0));
    }
    match change {
        ScheduleChange::AppStrategy { strategy } => {
            set_strategy(tx, "app", "", Some(*strategy))?;
        }
        ScheduleChange::PoolStrategy { pool, strategy } => {
            if !access.pools.iter().any(|known| &known.id == pool) {
                return Ok(rejected("pool_not_found"));
            }
            if !set_strategy(tx, "pool", pool.as_str(), *strategy)? {
                return Ok(rejected("account_schedule_full"));
            }
        }
        ScheduleChange::ProjectStrategy { project, strategy } => {
            if !set_strategy(tx, "project", project.as_str(), *strategy)? {
                return Ok(rejected("account_schedule_full"));
            }
        }
        ScheduleChange::Account { connection, plan } => {
            if !access
                .connections
                .iter()
                .any(|known| &known.id == connection)
            {
                return Ok(rejected("connection_not_found"));
            }
            if !set_plan(tx, connection, plan)? {
                return Ok(rejected("account_schedule_full"));
            }
        }
    }
    Ok(Outcome::Applied)
}

/// Stores or removes one strategy row. Returns false when a new row would pass the bound.
fn set_strategy(
    tx: &Transaction<'_>,
    scope: &str,
    id: &str,
    strategy: Option<Strategy>,
) -> Result<bool, EngineError> {
    assert!(scope == "app" || !id.is_empty());
    let Some(strategy) = strategy else {
        tx.execute(
            "DELETE FROM account_strategies WHERE scope=?1 AND id=?2",
            params![scope, id],
        )
        .map_err(error)?;
        return Ok(true);
    };
    if !row_fits(
        tx,
        "SELECT COUNT(*) FROM account_strategies WHERE scope=?1 AND id<>?2",
        params![scope, id],
        SCOPED_STRATEGIES_MAX,
    )? {
        return Ok(false);
    }
    let changed = tx
        .execute(
            "INSERT INTO account_strategies(scope,id,strategy) VALUES (?1,?2,?3)
             ON CONFLICT(scope,id) DO UPDATE SET strategy=excluded.strategy",
            params![scope, id, strategy.code()],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(true)
}

/// Stores or removes one account plan. Returns false when a new row would pass the bound.
fn set_plan(
    tx: &Transaction<'_>,
    connection: &CredentialRef,
    plan: &AccountPlan,
) -> Result<bool, EngineError> {
    assert!(plan.validate().is_ok());
    if *plan == AccountPlan::default() {
        tx.execute(
            "DELETE FROM account_plans WHERE connection=?1",
            [connection.as_str()],
        )
        .map_err(error)?;
        return Ok(true);
    }
    if !row_fits(
        tx,
        "SELECT COUNT(*) FROM account_plans WHERE connection<>?1",
        params![connection.as_str()],
        ACCOUNT_PLANS_MAX,
    )? {
        return Ok(false);
    }
    let changed = tx
        .execute(
            "INSERT INTO account_plans(connection,rank,reserve_percent,pin) VALUES (?1,?2,?3,?4)
             ON CONFLICT(connection) DO UPDATE SET rank=excluded.rank,
             reserve_percent=excluded.reserve_percent, pin=excluded.pin",
            params![
                connection.as_str(),
                plan.rank,
                plan.reserve_percent,
                plan.pin.map(Pin::code)
            ],
        )
        .map_err(error)?;
    assert_eq!(changed, 1);
    Ok(true)
}

fn row_fits(
    tx: &Transaction<'_>,
    sql: &str,
    values: impl rusqlite::Params,
    max: usize,
) -> Result<bool, EngineError> {
    let others: i64 = tx.query_row(sql, values, |row| row.get(0)).map_err(error)?;
    Ok(usize::try_from(others).map_err(error)? < max)
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Reads every stored scheduling choice. Takes the engine connection; a database
/// without the scheduling tables holds no choices yet.
/// Returns the schedule with lists sorted by id; a stored value this engine cannot read
/// is an `account_schedule_record` failure.
pub fn load(connection: &Connection) -> Result<Schedule, EngineError> {
    if !table_exists(connection, "account_strategies")?
        || !table_exists(connection, "account_plans")?
    {
        return Ok(Schedule::default());
    }
    let bound = i64::try_from(SCOPED_STRATEGIES_MAX * 2 + 1).map_err(error)?;
    let mut statement = connection
        .prepare("SELECT scope,id,strategy FROM account_strategies ORDER BY scope,id LIMIT ?1")
        .map_err(error)?;
    let rows = statement
        .query_map([bound], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut schedule = Schedule::default();
    for (scope, id, code) in rows {
        let strategy = Strategy::parse(&code).ok_or_else(|| error("account_schedule_record"))?;
        match scope.as_str() {
            "app" => schedule.strategy = strategy,
            "pool" | "project" => {
                let entry = ScopedStrategy {
                    id: CredentialRef::new(id).map_err(|_| error("account_schedule_record"))?,
                    strategy,
                };
                if scope == "pool" {
                    schedule.pools.push(entry);
                } else {
                    schedule.projects.push(entry);
                }
            }
            _ => return Err(error("account_schedule_record")),
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT connection,rank,reserve_percent,pin FROM account_plans ORDER BY connection LIMIT ?1",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([i64::try_from(ACCOUNT_PLANS_MAX).map_err(error)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    for (connection, rank, reserve, pin) in rows {
        let record = || error("account_schedule_record");
        let plan = AccountPlan {
            rank: rank
                .map(|rank| u16::try_from(rank).map_err(|_| record()))
                .transpose()?,
            reserve_percent: u8::try_from(reserve).map_err(|_| record())?,
            pin: pin
                .map(|pin| Pin::parse(&pin).ok_or_else(record))
                .transpose()?,
        };
        plan.validate().map_err(|_| record())?;
        schedule.accounts.push(AccountEntry {
            connection: CredentialRef::new(connection).map_err(|_| record())?,
            plan,
        });
    }
    assert!(schedule.pools.len() <= SCOPED_STRATEGIES_MAX);
    assert!(schedule.projects.len() <= SCOPED_STRATEGIES_MAX);
    assert!(schedule.accounts.len() <= ACCOUNT_PLANS_MAX);
    Ok(schedule)
}

/// Why the schedule leaves an account out of the accounts that can take work now, with
/// the reset time that ends it when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Excluded {
    NeverUse,
    FiveHourCap { resets_ms: Option<i64> },
    WeeklyReserve { resets_ms: Option<i64> },
}

impl Excluded {
    /// The plain refusal code a blocked turn stores.
    pub fn code(self) -> &'static str {
        match self {
            Self::NeverUse => "account_never_use",
            Self::FiveHourCap { .. } => "account_five_hour_cap",
            Self::WeeklyReserve { .. } => "account_weekly_reserve",
        }
    }

    /// When the account can take work again, or none for a pin or an unknown reset.
    pub fn until_ms(self) -> Option<i64> {
        match self {
            Self::NeverUse => None,
            Self::FiveHourCap { resets_ms } | Self::WeeklyReserve { resets_ms } => resets_ms,
        }
    }
}

/// One account a caller may give work: its connection, its live agents and whether it
/// is the account the work already runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub connection: CredentialRef,
    pub load: usize,
    pub current: bool,
}

/// The order to try candidates in, as indices into the caller's list, and the candidates
/// left out with why. Every candidate appears exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub ready: Vec<usize>,
    pub excluded: Vec<(usize, Excluded)>,
}

/// How one ready candidate sorts: `use_last` pin, the strategy's own order (unknown
/// last), rank, not the current account, live agents and the caller's order.
type SortKey = (bool, (bool, i64, i64), u16, bool, usize, usize);

/// The schedule, strategy, account limits and clock one choice of account is made with.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Choice {
    pub schedule: Schedule,
    pub strategy: Strategy,
    pub limits: BTreeMap<CredentialRef, AccountLimits>,
    pub now_ms: i64,
}

impl Choice {
    /// Reads the choice for work of `project` on `pools`: the stored schedule, the
    /// strategy that applies and the usage limits of every member of those pools, keyed
    /// by connection. Takes the engine connection, model access, project, pools and clock.
    pub fn read(
        connection: &Connection,
        access: &ModelAccess,
        project: Option<&str>,
        pools: &[&CredentialRef],
        now_ms: i64,
    ) -> Result<Self, EngineError> {
        let schedule = load(connection)?;
        let strategy = schedule.strategy_for(project, pools);
        let mut limits = BTreeMap::new();
        if table_exists(connection, "account_limits")? {
            let members: BTreeSet<&CredentialRef> = access
                .pools
                .iter()
                .filter(|pool| pools.contains(&&pool.id))
                .flat_map(|pool| pool.members.iter())
                .collect();
            for member in members {
                if let Some(found) = crate::usage::limits(connection, member)? {
                    limits.insert(member.clone(), found);
                }
            }
        }
        Ok(Self {
            schedule,
            strategy,
            limits,
            now_ms,
        })
    }

    /// Orders candidates by this choice. `use_last` accounts go after the rest; a
    /// `never_use` pin, a full 5-hour window or weekly use at or above 100 minus the
    /// reserve leaves an account out. Each window is read as it stands now, with the
    /// owner's manual reset applied and a passed reset counted as reset. Ties fall to rank, then the current account, then fewer live agents, then
    /// the caller's order. Takes the candidates in pool order. Returns the order.
    pub fn order(&self, candidates: &[Candidate]) -> Order {
        let mut ready = Vec::new();
        let mut excluded = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            match self.exclusion(candidate) {
                Some(why) => excluded.push((index, why)),
                None => ready.push(index),
            }
        }
        ready.sort_by_key(|index| self.key(&candidates[*index], *index));
        assert_eq!(ready.len() + excluded.len(), candidates.len());
        assert!(
            ready
                .iter()
                .chain(excluded.iter().map(|(index, _)| index))
                .collect::<BTreeSet<_>>()
                .len()
                == candidates.len()
        );
        Order { ready, excluded }
    }

    fn windows(&self, candidate: &Candidate) -> (Option<LimitWindow>, Option<LimitWindow>) {
        let limits = self.limits.get(&candidate.connection);
        (
            limits.and_then(|limits| limits.five_hour_at(self.now_ms)),
            limits.and_then(|limits| limits.weekly_at(self.now_ms)),
        )
    }

    fn exclusion(&self, candidate: &Candidate) -> Option<Excluded> {
        let plan = self.schedule.plan(&candidate.connection);
        if plan.pin == Some(Pin::NeverUse) {
            return Some(Excluded::NeverUse);
        }
        let (five_hour, weekly) = self.windows(candidate);
        if let Some(window) = five_hour.filter(|window| window.used_percent >= 100.0) {
            return Some(Excluded::FiveHourCap {
                resets_ms: window.resets_at_ms,
            });
        }
        let ceiling = f64::from(100 - plan.reserve_percent);
        weekly
            .filter(|window| window.used_percent >= ceiling)
            .map(|window| Excluded::WeeklyReserve {
                resets_ms: window.resets_at_ms,
            })
    }

    fn key(&self, candidate: &Candidate, index: usize) -> SortKey {
        let plan = self.schedule.plan(&candidate.connection);
        let (five_hour, weekly) = self.windows(candidate);
        let permille = |window: Option<LimitWindow>| {
            window.map_or(0, |window| (window.used_percent * 10.0).round() as i64)
        };
        let primary = match self.strategy {
            Strategy::UseItOrLoseIt => match weekly.and_then(|window| window.resets_at_ms) {
                Some(resets_ms) => (false, resets_ms, 0),
                None => (true, 0, 0),
            },
            Strategy::ManualRank => (false, 0, 0),
            Strategy::Balanced => match weekly {
                Some(window) => (false, permille(Some(window)), permille(five_hour)),
                None => (true, 0, 0),
            },
        };
        (
            plan.pin == Some(Pin::UseLast),
            primary,
            plan.rank.unwrap_or(u16::MAX),
            !candidate.current,
            candidate.load,
            index,
        )
    }
}

/// Where one account stands in its pool's ranking now: next in line, with its weekly
/// reset and the weekly percent it may still use before its reserve, or held with why
/// and until when, in ms since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Standing {
    Next {
        weekly_resets_ms: Option<i64>,
        weekly_left_percent: Option<u8>,
        use_last: bool,
    },
    Paused {
        until_ms: i64,
    },
    NeverUse,
    FiveHourCap {
        until_ms: Option<i64>,
    },
    ReserveReached {
        reserve_percent: u8,
        until_ms: Option<i64>,
    },
}

/// One pool account and where it stands now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedAccount {
    pub connection: CredentialRef,
    pub standing: Standing,
}

/// One pool's ranking: the strategy that orders it, the accounts work would try now in
/// that order, then every held account in pool order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolRanking {
    pub pool: CredentialRef,
    pub strategy: Strategy,
    pub accounts: Vec<RankedAccount>,
}

/// Ranks every pool's accounts with the same choice team, research and master work make
/// (DEC-096 item 8): a paused account is held, the rest go through `Choice::order`. Live
/// load and the account work already runs on only break ties, so they are left out.
/// Takes the engine connection, model access and clock. Returns one ranking per pool.
pub fn ranking(
    connection: &Connection,
    access: &ModelAccess,
    now_ms: i64,
) -> Result<Vec<PoolRanking>, EngineError> {
    let pauses = table_exists(connection, "team_paused_accounts")?;
    let mut pools = Vec::with_capacity(access.pools.len());
    for pool in &access.pools {
        let choice = Choice::read(connection, access, None, &[&pool.id], now_ms)?;
        let mut held = Vec::new();
        let mut candidates = Vec::new();
        let mut places = Vec::new();
        for (index, member) in pool.members.iter().enumerate() {
            match paused_until(connection, pauses, member, now_ms)? {
                Some(until_ms) => held.push((index, Standing::Paused { until_ms })),
                None => {
                    candidates.push(Candidate {
                        connection: member.clone(),
                        load: 0,
                        current: false,
                    });
                    places.push(index);
                }
            }
        }
        let order = choice.order(&candidates);
        let mut accounts: Vec<RankedAccount> = order
            .ready
            .iter()
            .map(|&at| RankedAccount {
                connection: candidates[at].connection.clone(),
                standing: choice.next(&candidates[at]),
            })
            .collect();
        held.extend(
            order
                .excluded
                .iter()
                .map(|&(at, why)| (places[at], choice.held(&candidates[at], why))),
        );
        held.sort_by_key(|(index, _)| *index);
        accounts.extend(held.into_iter().map(|(index, standing)| RankedAccount {
            connection: pool.members[index].clone(),
            standing,
        }));
        assert_eq!(accounts.len(), pool.members.len());
        pools.push(PoolRanking {
            pool: pool.id.clone(),
            strategy: choice.strategy,
            accounts,
        });
    }
    Ok(pools)
}

fn paused_until(
    connection: &Connection,
    pauses: bool,
    member: &CredentialRef,
    now_ms: i64,
) -> Result<Option<i64>, EngineError> {
    if !pauses {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT until_ms FROM team_paused_accounts WHERE connection=?1 AND until_ms>?2",
            params![member.as_str(), now_ms],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)
}

impl Choice {
    fn next(&self, candidate: &Candidate) -> Standing {
        let plan = self.schedule.plan(&candidate.connection);
        let (_, weekly) = self.windows(candidate);
        Standing::Next {
            weekly_resets_ms: weekly.and_then(|window| window.resets_at_ms),
            weekly_left_percent: weekly.map(|window| {
                let room = (f64::from(100 - plan.reserve_percent) - window.used_percent).max(0.0);
                assert!(room <= 100.0);
                room.floor() as u8
            }),
            use_last: plan.pin == Some(Pin::UseLast),
        }
    }

    fn held(&self, candidate: &Candidate, why: Excluded) -> Standing {
        match why {
            Excluded::NeverUse => Standing::NeverUse,
            Excluded::FiveHourCap { resets_ms } => Standing::FiveHourCap {
                until_ms: resets_ms,
            },
            Excluded::WeeklyReserve { resets_ms } => Standing::ReserveReached {
                reserve_percent: self.schedule.plan(&candidate.connection).reserve_percent,
                until_ms: resets_ms,
            },
        }
    }
}

#[cfg(test)]
mod tests;
