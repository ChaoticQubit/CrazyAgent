use super::*;
use crate::engine::bootstrap::EngineService;
use crate::usage::report::LIMITS_MAX;
use crate::usage::report::tests::fill_past_one_frame;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const OWNER_WAIT_MS: u64 = 5000;
pub(super) const IN_FLIGHT: Duration = Duration::from_millis(300);
const AT_MS: i64 = 1_800_000_000_000;
static BUSY_STORAGE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Lets one test at a time write and sweep tens of thousands of real files. Run together,
/// those tests share one disk, and each sweep then outlasts the owner waits its test was
/// measured against. Output: the guard the test holds until it ends.
pub(super) async fn busy_storage() -> tokio::sync::MutexGuard<'static, ()> {
    BUSY_STORAGE.lock().await
}

/// A real engine with an owner client and the folder of its usage store.
pub(super) struct Usage {
    pub(super) root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    pub(super) client: OwnerClient,
    pub(super) engine_root: PathBuf,
}

impl Usage {
    pub(super) async fn start() -> Self {
        Self::start_waiting(OWNER_WAIT_MS).await
    }

    /// Starts the engine with its owner wait. Input: the wait in milliseconds.
    /// Output: the engine with a TUI owner client.
    pub(super) async fn start_waiting(wait_ms: u64) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-usage-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, format!("version = 1\ntimeout_ms = {wait_ms}\n")).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        let engine_root =
            super::changes::engine_root(&service.owner_runtime().unwrap().authority).unwrap();
        Self {
            root,
            service,
            _stop: stop,
            client,
            engine_root,
        }
    }

    pub(super) fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    /// The handshake packet the owner client sends. Output: the packet.
    pub(super) fn packet(&self) -> [u8; 37] {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&self.client.token);
        packet
    }

    pub(super) async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
}

/// A connected socket pair whose buffers hold about 1 KiB, so a reply larger than that
/// waits for its client to read. Output: the engine end and the client end.
fn small_buffers() -> (UnixStream, UnixStream) {
    let (server, client) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&server, &client] {
        end.set_nonblocking(true).unwrap();
    }
    let server = UnixStream::from_std(server).unwrap();
    let client = UnixStream::from_std(client).unwrap();
    nix::sys::socket::setsockopt(&server, nix::sys::socket::sockopt::SndBuf, &1024usize).unwrap();
    nix::sys::socket::setsockopt(&client, nix::sys::socket::sockopt::RcvBuf, &1024usize).unwrap();
    (server, client)
}

/// The body of a usage report request over all time. Output: the request body.
fn report_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "usage": { "kind": "report", "since_ms": null, "offset_minutes": 0 }
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offset_at_the_integer_edge_is_refused_over_the_owner_socket() {
    let usage = Usage::start().await;
    for offset in [i32::MIN, i32::MAX] {
        assert_eq!(
            usage
                .client
                .usage_report(None, None, offset)
                .await
                .unwrap_err()
                .0,
            "usage_request_invalid",
            "offset {offset}"
        );
    }
    assert!(usage.client.usage_report(None, None, -840).await.is_ok());
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_report_past_one_frame_is_trimmed_and_answered() {
    let usage = Usage::start().await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    fill_past_one_frame(&db, crate::team::now_ms());
    drop(db);

    let report = usage
        .client
        .usage_report(None, None, 0)
        .await
        .unwrap_or_else(|error| panic!("the full usage report answered {}", error.0));
    assert!(
        !report.limits.is_empty() && report.limits.len() < LIMITS_MAX,
        "{} limits",
        report.limits.len()
    );
    assert!(!report.projects.is_empty() && !report.days.is_empty());
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_money_choice_is_stored_and_reaches_the_report() {
    use crate::usage::money::{self, MainNumber, MoneySettings, PriceSource};
    let usage = Usage::start().await;
    let settings = MoneySettings {
        main_number: MainNumber::Tokens,
        prices: PriceSource::Off,
        ..MoneySettings::default()
    };
    usage.client.usage_set_money(settings).await.unwrap();
    let report = usage.client.usage_report(None, None, 0).await.unwrap();
    assert_eq!(report.money.settings, settings);
    let db = crate::usage::open(&usage.engine_root).unwrap();
    assert_eq!(money::read(&db).unwrap(), settings);
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_change_holds_the_owner_barrier_until_it_is_committed() {
    let usage = Usage::start().await;
    let held = crate::usage::open(&usage.engine_root).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let account = CredentialRef::new("held-account").unwrap();
    let mut change = Box::pin(usage.client.usage_set_reset(
        account.clone(),
        crate::usage::WindowKind::FiveHour,
        Some(AT_MS),
    ));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut change).await.is_err(),
        "the change waits behind the held write"
    );
    assert!(
        usage.runtime().engine.owner_egress.try_write().is_err(),
        "the change holds the owner barrier while it waits to commit"
    );
    held.execute_batch("ROLLBACK").unwrap();
    change
        .await
        .unwrap_or_else(|error| panic!("the held change answered {}", error.0));
    let stored = crate::usage::limits(&held, &account).unwrap().unwrap();
    assert_eq!(stored.five_hour_reset_override_ms, Some(AT_MS));
    usage.stop().await;
}

/// RV-DS241-68: a usage change whose request is cut short while it waits behind a held
/// write keeps the owner barrier until the write commits, so a revocation returns only
/// after that change, never while it is still committing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_usage_change_cut_short_keeps_the_owner_barrier_until_it_commits() {
    let usage = Usage::start().await;
    let runtime = usage.runtime();
    let packet = usage.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, _client) = small_buffers();
    let account = CredentialRef::new("cut-account").unwrap();
    let body = serde_json::to_vec(&json!({
        "usage": {
            "kind": "set_reset",
            "account": account,
            "window": crate::usage::WindowKind::FiveHour,
            "at_ms": AT_MS
        }
    }))
    .unwrap();
    let held = crate::usage::open(&usage.engine_root).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let served = super::usage::serve(
        &mut server,
        &runtime.engine,
        &runtime.authority,
        &packet,
        &admission,
        &body,
    );
    assert!(
        tokio::time::timeout(IN_FLIGHT, served).await.is_err(),
        "the change is cut short while it waits behind the held write"
    );
    let still_held = runtime.engine.owner_egress.try_write().is_err();
    let mut revoke = Box::pin(runtime.control(OwnerControl {
        operation_id: RequestId::new("revoke-during-cut-usage").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
        decision: OwnerDecision::Revoke,
    }));
    let first = tokio::time::timeout(IN_FLIGHT, &mut revoke).await;
    let revoke_waited = first.is_err();
    held.execute_batch("ROLLBACK").unwrap();
    let revoked = match first {
        Ok(done) => done,
        Err(_) => tokio::time::timeout(Duration::from_secs(10), revoke)
            .await
            .expect("the revocation runs once the change commits"),
    }
    .unwrap();
    assert!(
        still_held,
        "the change still owns the owner barrier after its request is cut"
    );
    assert!(
        revoke_waited,
        "the revocation waits for the change it was ordered after"
    );
    assert!(!revoked.active);
    let stored = crate::usage::limits(&held, &account).unwrap().unwrap();
    assert_eq!(
        stored.five_hour_reset_override_ms,
        Some(AT_MS),
        "the change committed before the revocation returned"
    );
    assert!(runtime.engine.owner_egress.try_write().is_ok());
    drop(server);
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revocation_waits_for_a_usage_reply_in_flight_and_refuses_the_next() {
    let usage = Usage::start().await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    fill_past_one_frame(&db, crate::team::now_ms());
    drop(db);
    let runtime = usage.runtime();
    let packet = usage.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, mut client) = small_buffers();
    let body = report_body();
    let (engine, authority) = (runtime.engine.clone(), runtime.authority.clone());
    let serving = tokio::spawn(async move {
        super::usage::serve(&mut server, &engine, &authority, &packet, &admission, &body).await
    });

    let size = tokio::time::timeout(Duration::from_secs(10), client.read_u32())
        .await
        .unwrap()
        .unwrap() as usize;
    assert!(size > 16384 && size <= FRAME_BYTES_MAX, "{size}");
    tokio::time::sleep(IN_FLIGHT).await;
    assert!(
        runtime.engine.owner_egress.try_write().is_err(),
        "the usage reply is written under the owner barrier"
    );
    let mut revoke = Box::pin(runtime.control(OwnerControl {
        operation_id: RequestId::new("revoke-during-usage").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
        decision: OwnerDecision::Revoke,
    }));
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut revoke).await.is_err(),
        "a revocation waits for the usage reply already in flight"
    );
    let mut reply = vec![0; size];
    client.read_exact(&mut reply).await.unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(
        reply["kind"], "report",
        "the reply admitted before the revocation"
    );
    serving.await.unwrap().unwrap();
    assert!(!revoke.await.unwrap().active);

    assert!(
        usage.client.usage_report(None, None, 0).await.is_err(),
        "the revoked client reads no more usage"
    );
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_its_reply_is_dropped_and_frees_the_owner_barrier() {
    let usage = Usage::start().await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    fill_past_one_frame(&db, crate::team::now_ms());
    drop(db);
    let runtime = usage.runtime();
    let packet = usage.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, client) = small_buffers();
    let body = report_body();

    let started = Instant::now();
    let served = tokio::time::timeout(
        Duration::from_secs(10),
        super::usage::serve(
            &mut server,
            &runtime.engine,
            &runtime.authority,
            &packet,
            &admission,
            &body,
        ),
    )
    .await
    .expect("the reply write to a client that stopped reading is bounded");
    assert_eq!(served.unwrap_err().0, "owner_reply_stalled");
    assert!(started.elapsed() >= REPLY_WRITE_TIME);
    assert!(
        tokio::time::timeout(IN_FLIGHT, runtime.engine.owner_egress.write())
            .await
            .is_ok(),
        "the stalled client no longer holds the owner barrier"
    );
    drop(client);
    usage.stop().await;
}

/// Adds a Claude subscription account through the owner client, as Settings > Accounts
/// does. Input: the running engine. Output: the saved account.
async fn saved_claude(usage: &Usage) -> crate::accounts::AccountView {
    usage
        .client
        .add_account(
            crate::accounts::AccountDraft {
                provider: crate::accounts::Provider::Claude,
                method: crate::accounts::SignInMethod::Subscription,
                name: "Claude".into(),
                url: None,
                model: None,
            },
            None,
        )
        .await
        .unwrap()
}

/// Stores one run's limits read for `account`: a five-hour window 40% used that resets in
/// an hour. Inputs: the usage store, the account and the time.
fn read_limits(db: &rusqlite::Connection, account: &CredentialRef, now_ms: i64) {
    let observation = crate::usage::Observation {
        plan: Some("max".into()),
        five_hour: Some(crate::usage::LimitWindow {
            used_percent: 40.0,
            resets_at_ms: Some(now_ms + 3_600_000),
        }),
        weekly: None,
        provider: None,
    };
    crate::usage::record_observation(
        db,
        account,
        &observation,
        crate::usage::LimitSource::Transcript,
        now_ms,
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_account_leaves_no_stored_limits_behind() {
    let usage = Usage::start().await;
    let added = saved_claude(&usage).await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    read_limits(&db, &added.id, crate::team::now_ms());
    assert!(crate::usage::limits(&db, &added.id).unwrap().is_some());

    usage.client.remove_account(added.id.clone()).await.unwrap();

    assert_eq!(crate::usage::limits(&db, &added.id).unwrap(), None);
    drop(db);
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_limits_cards_list_only_saved_accounts_however_many_old_rows_sort_first() {
    let usage = Usage::start().await;
    let added = saved_claude(&usage).await;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    let now = crate::team::now_ms();
    for index in 0..LIMITS_MAX {
        let gone = CredentialRef::new(format!("a-gone-{index:03}")).unwrap();
        assert!(gone.as_str() < added.id.as_str());
        read_limits(&db, &gone, now);
    }
    read_limits(&db, &added.id, now);
    drop(db);

    let report = usage.client.usage_report(None, None, 0).await.unwrap();

    assert_eq!(
        report
            .limits
            .iter()
            .map(|limits| limits.account.as_str())
            .collect::<Vec<_>>(),
        [added.id.as_str()]
    );
    usage.stop().await;
}

/// The Metrics request answers over the owner socket with one project's runs from the
/// engine's usage store, and refuses a range that starts after now.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_metrics_request_answers_one_project_runs_over_the_owner_socket() {
    let usage = Usage::start().await;
    let now = crate::team::now_ms();
    let hour = 60 * 60 * 1000;
    let db = crate::usage::open(&usage.engine_root).unwrap();
    for (project, agent) in [("alpha", "team-7"), ("beta", "team-9")] {
        db.execute(
            "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,\
                input,output,cache_read,cache_write)
             VALUES (?1,'agent',?2,'pool-a','acct-a',?3,'claude-code','s1',100,20,9000,30)",
            rusqlite::params![now - now % hour, project, agent],
        )
        .unwrap();
    }
    drop(db);
    let alpha = crate::protocol::ProjectId::new("alpha").unwrap();

    let metrics = usage
        .client
        .usage_metrics(Some(alpha), now - hour)
        .await
        .unwrap_or_else(|error| panic!("the metrics request answered {}", error.0));
    let runs: Vec<_> = metrics
        .runs
        .iter()
        .map(|run| {
            (
                run.agent.as_str(),
                run.tokens.headline(),
                run.tokens.cache_read,
            )
        })
        .collect();
    assert_eq!(runs, [("team-7", 150, 9000)]);
    assert_eq!(metrics.bucket_ms, 2 * hour);
    assert_eq!(
        usage
            .client
            .usage_metrics(None, now + hour)
            .await
            .unwrap_err()
            .0,
        "usage_request_invalid"
    );
    usage.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recount_choice_with_nothing_to_decide_is_refused_by_its_own_code() {
    let usage = Usage::start().await;
    let refused = usage.client.usage_decide_recount(true).await.unwrap_err();
    assert_eq!(refused.0, "usage_recount_not_ready");
    usage.stop().await;
}

/// The extras read answers one entry per saved account, named by its harness, with nothing
/// read yet as not reported and never as 0. On a fresh install a Claude account's free
/// resets say off in Settings, Don't read being the default (owner answer 2026-11-14);
/// with Through Claude Code stored, it shows what the usage store holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_extras_read_answers_each_saved_account_with_its_stored_resets_and_credits() {
    let usage = Usage::start().await;
    let added = saved_claude(&usage).await;

    let unread = usage.client.usage_extras(false).await.unwrap();

    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0].account, added.id);
    assert_eq!(unread[0].extras.not_reported, "not reported by Claude Code");
    assert_eq!(unread[0].extras.resets, None);
    assert_eq!(unread[0].extras.credits, None);
    assert_eq!(
        unread[0].extras.resets_unread,
        Some(crate::usage::extras::UnreadClass::Off),
        "a fresh install reads Claude free resets as Don't read"
    );

    let db = crate::usage::open(&usage.engine_root).unwrap();
    crate::usage::spend::write(
        &db,
        crate::usage::spend::SpendSettings {
            resets_source: crate::usage::spend::ResetsSource::ThroughClaudeCode,
            ..crate::usage::spend::SpendSettings::default()
        },
    )
    .unwrap();
    let now = crate::team::now_ms();
    let read = crate::usage::extras::Extras {
        resets: crate::usage::extras::Field::Known(crate::usage::extras::Resets {
            count: 2,
            kinds: vec![crate::usage::extras::ResetKind::Weekly],
            next_expiry_ms: Some(now + 86_400_000),
        }),
        credits: crate::usage::extras::Field::Known("4.20 USD left".into()),
    };
    crate::usage::extras::record_extras(&db, &added.id, &read, now).unwrap();
    drop(db);

    let stored = usage.client.usage_extras(false).await.unwrap();

    assert_eq!(stored.len(), 1);
    let resets = stored[0].extras.resets.clone().unwrap();
    assert_eq!(
        (resets.count, resets.kinds),
        (2, vec![crate::usage::extras::ResetKind::Weekly])
    );
    assert_eq!(stored[0].extras.credits.as_deref(), Some("4.20 USD left"));
    usage.stop().await;
}

/// Only a read that says Pools is open asks the usage service for the detail rows; Metrics,
/// the TUI usage screen and the new-reset watch read what is stored and never ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_pools_extras_read_asks_for_the_detail_rows() {
    let usage = Usage::start().await;
    saved_claude(&usage).await;
    let db = crate::usage::open(&usage.engine_root).unwrap();

    let due = |db: &rusqlite::Connection| {
        crate::usage::extras::detail_due(db, None, crate::team::now_ms()).unwrap()
    };

    usage.client.usage_extras(false).await.unwrap();
    assert!(!due(&db), "a stored-only read takes no Pools lease");

    usage.client.usage_extras(true).await.unwrap();
    assert!(due(&db), "a Pools read takes the lease the next poll reads");
    drop(db);
    usage.stop().await;
}
