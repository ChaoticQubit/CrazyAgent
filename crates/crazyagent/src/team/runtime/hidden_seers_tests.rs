use super::*;

const TEAMMATE: i64 = 9_001;

/// The digests a launch records for the folders its sandbox hid. Input: the folders.
/// Output: the digests, as a worker or pane record keeps them.
fn hid(folders: &[PathBuf]) -> Option<Vec<String>> {
    Some(
        folders
            .iter()
            .map(|folder| kernel_sandbox::hidden_paths::digest(folder))
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

/// The owner's sentence on an agent the engine stopped for a folder hidden after it started.
const FOLDER_HIDDEN: &str = "A folder was hidden from agents after this agent started, so the agent was stopped; Restart them starts it again without access to it. (agent_folder_hidden_held)";

/// A running agent that started before the owner hid a folder can still open it, so the
/// engine stops it at its next check with its work held, the live master too, and keeps both
/// listed for the owner's Restart them; an agent whose launch hid that folder or one above
/// it, one at Full access and a teammate, whose lead is the one listed, keep running.
#[test]
fn an_agent_that_can_open_a_newly_hidden_folder_is_stopped_held_and_listed() {
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
    let (mut worker, after_orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = hid(std::slice::from_ref(&secrets));
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
    assert!(team.workers.values().all(|worker| !worker.stopping));
    assert_eq!(restart_them(&fixture), refused);

    crate::sandbox::access_store::hidden::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    for (id, orders) in [(before, &orders), (master, &master_orders)] {
        let stopped = &team.workers[&id];
        assert!(stopped.stopping && stopped.held, "agent {id} stops held");
        assert_eq!(stopped.stop_why, "engine_stop:hidden");
        assert_eq!(
            stopped.narrowed.map(Narrowed::problem).as_deref(),
            Some(FOLDER_HIDDEN)
        );
        assert!(matches!(orders.try_recv(), Ok(Order::Stop)));
    }
    for id in [after, full, TEAMMATE] {
        assert!(!team.workers[&id].stopping, "agent {id} keeps running");
    }
    assert!(after_orders.try_recv().is_err());
    assert_eq!(
        listed(&fixture),
        vec![before, master],
        "the master's pane kept no hidden folders, so it hides none"
    );
    assert_eq!(
        crate::sandbox::access_store::hidden::seers(&fixture.connection, PROJECT).unwrap(),
        BTreeSet::from([before, master]),
        "never Full access or a teammate"
    );

    team.mark_seers(&fixture.setup).unwrap();
    assert!(orders.try_recv().is_err(), "a stopped seer is stopped once");
    assert_eq!(listed(&fixture), vec![before, master], "it stays listed");
    team.workers.clear();
}

/// The owner's Restart them starts again a seer the engine stopped once its process ended: a
/// sub-agent in its own worktree and conversation, the live master as a planned restart in
/// its own session; then neither is listed.
#[test]
fn the_owner_restart_starts_a_seer_the_engine_stopped_again() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime();
    let keys = fixture.setup.project_folder().join("secrets").join("keys");
    let before = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let (mut worker, _orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = Some(Vec::new());
    team.workers.insert(before, worker);
    let (mut worker, _master_orders) = held_worker(&project, AgentRole::Master);
    worker.live = true;
    team.workers.insert(master, worker);
    crate::sandbox::access_store::hidden::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    team.workers.clear();
    team.mark_seers(&fixture.setup).unwrap();
    assert_eq!(
        listed(&fixture),
        vec![before, master],
        "an ended seer stays listed"
    );

    assert_eq!(restart_them(&fixture), crate::protocol::Outcome::Applied);
    team.restart_seers(&fixture.setup).unwrap();
    assert_eq!(
        team.restarts.get(&before),
        Some(&(project.clone(), Restart::Hidden))
    );
    assert!(team.master_restarts.contains(&project));
    team.mark_seers(&fixture.setup).unwrap();
    assert!(
        listed(&fixture).is_empty(),
        "a restarted seer is not listed"
    );
    team.restarts.clear();
}

/// An agent whose process ended on its own before the owner hid a folder can open nothing,
/// so the team status never lists it as a seer, while a running one that can still open the
/// folder is stopped, held and listed.
#[test]
fn an_agent_that_ended_before_a_folder_was_hidden_is_not_listed() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime();
    let keys = fixture.setup.project_folder().join("secrets").join("keys");
    let ended = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    let running = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let (mut worker, _orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = Some(Vec::new());
    team.workers.insert(running, worker);

    crate::sandbox::access_store::hidden::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert!(team.workers[&running].stopping, "the running seer stops");
    assert_eq!(listed(&fixture), vec![running], "only the running seer");
    let status = fixture.status();
    let row = status.agents.iter().find(|agent| agent.id == ended);
    assert!(
        !row.expect("the ended agent is in the status").sees_hidden,
        "an agent that ended before is not listed"
    );
    team.workers.clear();
}

/// A seer the engine stopped and held, whose process ended, stays listed after the engine
/// starts again, and the owner's Restart them then starts it again: a sub-agent in its own
/// worktree and conversation, the live master as a planned restart.
#[test]
fn a_seer_held_before_an_engine_restart_stays_listed_and_restarts() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let keys = fixture.setup.project_folder().join("secrets").join("keys");
    let before = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Stopped);
    crate::team::master::set_live(&fixture.connection, &project, true).unwrap();
    assert!(crate::team::master::live(&fixture.connection, &project).unwrap());
    let mut team = fixture.runtime();
    let (mut worker, _orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = Some(Vec::new());
    team.workers.insert(before, worker);
    let (mut worker, _master_orders) = held_worker(&project, AgentRole::Master);
    worker.live = true;
    team.workers.insert(master, worker);
    crate::sandbox::access_store::hidden::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    team.workers.clear();
    drop(team);
    for id in [before, master] {
        fixture
            .connection
            .execute(
                "UPDATE team_agents SET error=?2 WHERE id=?1",
                params![id, FOLDER_HIDDEN],
            )
            .unwrap();
    }

    let mut team = fixture.runtime();
    team.restart_seers(&fixture.setup).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert_eq!(
        listed(&fixture),
        vec![before, master],
        "listed after a restart"
    );
    assert_eq!(restart_them(&fixture), crate::protocol::Outcome::Applied);
    team.restart_seers(&fixture.setup).unwrap();
    assert_eq!(
        team.restarts.get(&before),
        Some(&(project.clone(), Restart::Hidden))
    );
    assert!(team.master_restarts.contains(&project));
    team.mark_seers(&fixture.setup).unwrap();
    assert!(
        listed(&fixture).is_empty(),
        "a restarted seer is not listed"
    );
    team.restarts.clear();
}

/// After the engine starts again, an agent its list still names is listed again only while
/// its row says the engine stopped it for a hidden folder: one started again, one whose row
/// is gone, one that ended for another reason and a teammate are dropped from the list.
#[test]
fn a_listed_agent_started_again_gone_or_ended_otherwise_is_not_listed_after_a_restart() {
    let fixture = Fixture::new(1);
    let started = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let ended = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    let teammate = fixture.agent(AgentRole::Teammate, "codex", AgentStatus::Stopped);
    let gone = teammate + 1_000;
    for id in [started, teammate] {
        fixture
            .connection
            .execute(
                "UPDATE team_agents SET error=?2 WHERE id=?1",
                params![id, FOLDER_HIDDEN],
            )
            .unwrap();
    }
    let named = BTreeSet::from([started, ended, teammate, gone]);
    crate::sandbox::access_store::hidden::set_seers(&fixture.connection, PROJECT, &named).unwrap();

    let mut team = fixture.runtime();
    team.restart_seers(&fixture.setup).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert!(listed(&fixture).is_empty(), "none is listed again");
    assert!(
        crate::sandbox::access_store::hidden::seers(&fixture.connection, PROJECT)
            .unwrap()
            .is_empty(),
        "the list is emptied"
    );
    assert!(team.hidden_held.is_empty());
}

/// macOS only, as team panes run only there (DEC-094): a pane a later engine takes over is
/// judged by what its record says its sandbox hid, a pane from before the engine kept that
/// list counting as hiding none. A pane that can still see the hidden folder is stopped,
/// held and listed, so an engine restart or a thaw never drops it; one whose record hid the
/// folder keeps running and is not listed.
#[cfg(target_os = "macos")]
#[test]
fn a_pane_taken_over_after_a_folder_was_hidden_is_listed_by_what_its_record_hid() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let secrets = fixture.setup.project_folder().join("secrets");
    crate::sandbox::policy_store::save(
        &fixture.connection,
        PROJECT,
        &kernel_sandbox::policy::AgentPolicy {
            access: AgentAccess::Sandboxed,
            ..Default::default()
        },
    )
    .unwrap();
    crate::sandbox::access_store::hidden::save(
        &fixture.connection,
        PROJECT,
        std::slice::from_ref(&secrets),
    )
    .unwrap();
    let sees = |fixture: &Fixture, id: i64| listed(fixture).contains(&id);

    for (kept, seen) in [
        (Some(Vec::new()), true),
        (hid(std::slice::from_ref(&secrets)), false),
        (None, true),
    ] {
        let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
        let record = PaneRecord {
            hidden: kept.clone(),
            ..fixture.pane(&host, master)
        };
        fixture.keep(master, &record);
        let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
        assert_eq!(
            team.workers[&master].hidden, kept,
            "the record's list is kept"
        );
        if seen {
            fixture.until(&mut team, "the taken-over pane is stopped and held", |f| {
                f.agent_status(master) == (AgentStatus::Stopped, Some(FOLDER_HIDDEN.to_owned()))
            });
        } else {
            fixture.until(&mut team, "the pane is taken over", |f| {
                f.agent_status(master) == (AgentStatus::Idle, None) && f.recorded(master)
            });
            for _ in 0..20 {
                team.tick().unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        assert_eq!(sees(&fixture, master), seen, "a record that hid {kept:?}");
        let left = if seen { names(&[]) } else { names(&[master]) };
        assert_eq!(
            team.shutdown().unwrap(),
            left,
            "a held pane is not left running"
        );
        host.close(&[]).unwrap();
    }
}

/// A project whose only agent is a seer the engine stopped and whose process ended is not
/// stepped on its own; the owner's Restart them makes it stepped until the ask is served,
/// and once served the project rests again.
#[test]
fn the_owner_restart_steps_a_project_whose_only_seer_ended() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime();
    let keys = fixture.setup.project_folder().join("secrets").join("keys");
    let before = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    let (mut worker, _orders) = held_worker(&project, AgentRole::Sub);
    worker.hidden = Some(Vec::new());
    team.workers.insert(before, worker);
    crate::sandbox::access_store::hidden::save(&fixture.connection, PROJECT, &[keys]).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    team.workers.clear();
    team.mark_seers(&fixture.setup).unwrap();
    assert!(
        !team.active_projects().unwrap().contains(&project),
        "an ended seer alone does not wake the project"
    );

    assert_eq!(restart_them(&fixture), crate::protocol::Outcome::Applied);
    assert!(
        team.active_projects().unwrap().contains(&project),
        "the owner's Restart them wakes the project"
    );
    team.restart_seers(&fixture.setup).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert_eq!(
        team.restarts.remove(&before),
        Some((project.clone(), Restart::Hidden))
    );
    assert!(
        !team.active_projects().unwrap().contains(&project),
        "a served ask no longer wakes the project"
    );
}

/// A restart ask for an agent the engine holds nothing of, as after the engine restarted,
/// wakes the project once, and the hidden check of that one step drops it, so the project
/// rests again.
#[test]
fn a_stale_restart_ask_leaves_the_project_resting_after_one_step() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let mut team = fixture.runtime();
    let ghost = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    crate::sandbox::access_store::hidden::set_seers(
        &fixture.connection,
        PROJECT,
        &BTreeSet::from([ghost]),
    )
    .unwrap();
    assert_eq!(restart_them(&fixture), crate::protocol::Outcome::Applied);
    assert!(team.active_projects().unwrap().contains(&project));
    team.restart_seers(&fixture.setup).unwrap();
    team.mark_seers(&fixture.setup).unwrap();
    assert!(team.restarts.is_empty());
    assert!(
        !team.active_projects().unwrap().contains(&project),
        "a stale ask is dropped by the step it woke"
    );
}
