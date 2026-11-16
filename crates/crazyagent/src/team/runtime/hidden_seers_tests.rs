use super::*;

const TEAMMATE: i64 = 9_001;

/// The digests a launch records for the folders its sandbox hid. Input: the folders.
/// Output: the digests, as a worker or pane record keeps them.
fn hid(folders: &[PathBuf]) -> Option<Vec<String>> {
    Some(
        folders
            .iter()
            .map(|folder| crate::sandbox::hidden_paths::digest(folder))
            .collect(),
    )
}

/// The agents the team status lists as able to open a folder hidden after they started.
/// Input: the fixture. Output: their ids in the status order.
fn listed(fixture: &Fixture) -> Vec<i64> {
    fixture
        .status()
        .agents
        .iter()
        .filter(|agent| agent.sees_hidden)
        .map(|agent| agent.id)
        .collect()
}

/// Presses Restart them for the fixture's project as the owner does, in its own
/// transaction. Input: the fixture. Output: the outcome.
fn restart_them(fixture: &Fixture) -> crate::protocol::Outcome {
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let tx = connection.transaction().unwrap();
    let outcome = crate::team::execute(
        &tx,
        &fixture.access,
        &crate::team::TeamAction::RestartHiddenSeers {
            project_id: fixture.setup.project_id.clone(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    outcome
}

/// SA-VID14-A: a running agent that started before the owner hid a folder can still open
/// it, so the team status lists it, and the live master too, until the owner's Restart them
/// stops each to start again; an agent whose launch hid that folder or one above it, one at
/// Full access and a teammate, whose lead is the one listed, never are.
#[test]
fn an_agent_started_before_a_folder_was_hidden_is_listed_until_the_owner_restarts_it() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime();
    let secrets = fixture.setup.project_folder().join("secrets");
    let keys = secrets.join("keys");
    let before = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let after = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let full = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let (mut worker, orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = Some(Vec::new());
    team.workers.insert(before, worker);
    let (mut worker, _) = held_worker(&project, AgentRole::Sub);
    worker.hidden = hid(&[secrets.clone()]);
    team.workers.insert(after, worker);
    let (mut worker, _) = held_worker(&project, AgentRole::Sub);
    worker.access = AgentAccess::Full;
    team.workers.insert(full, worker);
    let (mut worker, master_orders) = held_worker(&project, AgentRole::Master);
    worker.live = true;
    team.workers.insert(master, worker);
    let (worker, _) = held_worker(&project, AgentRole::Teammate);
    team.workers.insert(TEAMMATE, worker);
    let refused = crate::protocol::Outcome::Rejected {
        code: "team_no_hidden_seers".into(),
    };

    team.mark_seers(&fixture.setup).unwrap();
    assert!(listed(&fixture).is_empty(), "nothing is hidden yet");
    assert_eq!(restart_them(&fixture), refused);

    crate::sandbox::hidden_paths::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert_eq!(
        listed(&fixture),
        vec![before, master],
        "the master's pane kept no hidden folders, so it hides none"
    );
    assert_eq!(
        crate::sandbox::hidden_paths::seers(&fixture.connection, PROJECT).unwrap(),
        BTreeSet::from([before, master]),
        "never Full access or a teammate"
    );
    assert!(
        team.workers.values().all(|worker| !worker.stopping),
        "the list stops no one"
    );

    assert_eq!(restart_them(&fixture), crate::protocol::Outcome::Applied);
    team.restart_seers(&fixture.setup).unwrap();
    let restarted = &team.workers[&before];
    assert!(restarted.stopping && restarted.restarting && restarted.held);
    assert_eq!(restarted.stop_why, "engine_stop:hidden");
    assert!(matches!(orders.try_recv(), Ok(Order::Stop)));
    assert!(team.workers[&master].stopping);
    assert!(matches!(master_orders.try_recv(), Ok(Order::Stop)));
    assert!(
        team.master_restarts.contains(&project),
        "the live master starts again in its own session"
    );
    for id in [after, full, TEAMMATE] {
        assert!(!team.workers[&id].stopping, "agent {id} keeps running");
    }
    team.mark_seers(&fixture.setup).unwrap();
    assert!(
        listed(&fixture).is_empty(),
        "a stopping agent is not listed"
    );
    assert_eq!(restart_them(&fixture), refused);
    team.workers.clear();
}

/// macOS only, as team panes run only there (DEC-094): a pane a later engine takes over is
/// listed by what its record says its sandbox hid, a pane from before the engine kept that
/// list counting as hiding none, so an engine restart or a thaw never drops it from the list.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_taken_over_after_a_folder_was_hidden_is_listed_by_what_its_record_hid() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let secrets = fixture.setup.project_folder().join("secrets");
    crate::sandbox::policy::save(
        &fixture.connection,
        PROJECT,
        &crate::sandbox::policy::AgentPolicy {
            access: AgentAccess::Sandboxed,
            ..Default::default()
        },
    )
    .unwrap();
    crate::sandbox::hidden_paths::save(&fixture.connection, PROJECT, &[secrets.clone()]).unwrap();
    let sees = |fixture: &Fixture, id: i64| listed(fixture).contains(&id);

    for (kept, seen) in [
        (Some(Vec::new()), true),
        (hid(&[secrets.clone()]), false),
        (None, true),
    ] {
        let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
        let record = PaneRecord {
            hidden: kept.clone(),
            ..fixture.pane(&host, master)
        };
        fixture.keep(master, &record);
        let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
        fixture.until(&mut team, "the pane is taken over", |f| {
            f.agent_status(master) == (AgentStatus::Idle, None) && f.recorded(master)
        });
        for _ in 0..20 {
            team.tick().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(sees(&fixture, master), seen, "a record that hid {kept:?}");
        assert_eq!(
            team.workers[&master].hidden, kept,
            "the record's list is kept"
        );
        assert_eq!(team.shutdown().unwrap(), names(&[master]));
        host.close(&[]).unwrap();
    }
}
