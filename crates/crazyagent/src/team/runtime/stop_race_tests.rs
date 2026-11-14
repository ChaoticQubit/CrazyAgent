use super::*;

const STOPPED_LEAD_REFUSAL: &str = "Not done: you were stopped, so you cannot start a worker, merge or change work now. End your turn here.";

/// Calls `spawn` as the sub-agent `lead` does through its team tools, on a database
/// connection of its own. Takes the fixture, the lead and the task. Returns the answer
/// text and whether it is a refusal.
fn lead_spawn_answer(fixture: &Fixture, lead: i64, task: &str) -> (String, bool) {
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let folder = fixture.base.join("team");
    let answering = super::super::super::master::Answering {
        access: &fixture.access,
        stored: &fixture.setup,
        landing: &folder,
        agents: &folder,
        events: None,
        host: None,
    };
    super::super::super::master::answer_sub(
        &mut connection,
        &answering,
        lead,
        super::super::super::tool::Call::Spawn {
            task: task.into(),
            harness: None,
            role: None,
            mode: None,
        },
    )
    .unwrap()
    .unwrap()
}

/// Counts the tasks queued under one prompt. Takes the fixture and the prompt.
fn tasks_named(fixture: &Fixture, prompt: &str) -> i64 {
    fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE prompt=?1",
            [prompt],
            |row| row.get(0),
        )
        .unwrap()
}

/// Counts the agents whose row names `lead` as their lead. Takes the fixture and the lead.
fn workers_of(fixture: &Fixture, lead: i64) -> i64 {
    fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_agents WHERE parent=?1",
            [lead],
            |row| row.get(0),
        )
        .unwrap()
}

/// RV-DS241-122 (a): a lead's `spawn` call still in its mailbox on the tick that stops the
/// lead is drained with a plain refusal, and no task is queued from it, so a stopping lead
/// never starts a worker.
#[test]
fn a_stopping_leads_spawn_drained_on_its_stop_tick_starts_no_worker() {
    let mut fixture = Fixture::new(2);
    let lead = 7;
    let (host, mut team) = asking_team(&mut fixture, &[lead]);
    let mailbox = team.mailbox(lead);
    let tmp = mailbox.parent().unwrap().to_path_buf();
    for folder in [team.agent_folder(lead), tmp, mailbox.clone()] {
        private_directory(&folder).unwrap();
    }
    let prompt = "Write the help page.";
    let call = format!("{:032x}", 1);
    std::fs::write(
        mailbox.join(format!("{call}.call")),
        serde_json::to_vec(&serde_json::json!({"tool": "spawn", "task": prompt})).unwrap(),
    )
    .unwrap();
    team.workers
        .get_mut(&lead)
        .expect("the lead runs")
        .stop_for("rate_limited");
    team.serve_agent_calls(&fixture.setup, &fixture.access);
    let reply: serde_json::Value =
        serde_json::from_slice(&std::fs::read(mailbox.join(format!("{call}.reply"))).unwrap())
            .unwrap();
    assert_eq!(
        (
            reply["text"].as_str().unwrap(),
            reply["error"].as_bool().unwrap()
        ),
        (STOPPED_LEAD_REFUSAL, true)
    );
    assert_eq!(tasks_named(&fixture, prompt), 0, "no task is queued");
    assert_eq!(workers_of(&fixture, lead), 0);
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// RV-DS241-122 (b): a lead whose stop is recorded, by the owner, the master or its own
/// lead, is refused a new worker in the same words, while the same lead before the stop
/// has its worker queued, so the refusal is the stop's and no other check's.
#[test]
fn a_stopped_lead_is_refused_a_new_worker() {
    let fixture = Fixture::new(2);
    let _home = committed_project(&fixture);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, LEAD_CODEX, TeamLimits::default());
    let lead = idle_sub_agent(&fixture, &mut team);
    let (queued, refused) = lead_spawn_answer(&fixture, lead, "Write the notes page.");
    assert!(!refused && queued.starts_with("Task "), "{queued}");

    fixture
        .connection
        .execute(
            "INSERT INTO team_owner_stops(agent,project,stopped_ms,stopped_by) VALUES (?1,?2,1,'master')",
            params![lead, PROJECT],
        )
        .unwrap();
    let prompt = "Write the help page.";
    assert_eq!(
        lead_spawn_answer(&fixture, lead, prompt),
        (STOPPED_LEAD_REFUSAL.to_owned(), true)
    );
    assert_eq!(tasks_named(&fixture, prompt), 0, "no task is queued");
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// RV-DS241-122 (c): a worker task a lead queued before the owner stopped that lead is held
/// with a wait line that says so, never dispatched to a new worker of the stopped lead.
#[test]
fn a_task_queued_by_a_lead_the_owner_stopped_waits_for_the_lead() {
    let fixture = Fixture::new(2);
    let _home = committed_project(&fixture);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, LEAD_CODEX, TeamLimits::default());
    let lead = idle_sub_agent(&fixture, &mut team);
    let prompt = "Write the notes page.";
    let (queued, refused) = lead_spawn_answer(&fixture, lead, prompt);
    assert!(!refused, "{queued}");
    owner(
        &fixture,
        &[crate::team::TeamAction::StopAgent {
            project_id: fixture.setup.project_id.clone(),
            agent: lead,
        }],
    );
    fixture.until(&mut team, "the owner's stop ends the lead", |f| {
        f.agent_status(lead).0 == AgentStatus::Stopped
    });
    idle_ticks(&mut team);
    let (status, wait): (String, Option<String>) = fixture
        .connection
        .query_row(
            "SELECT status,wait FROM team_tasks WHERE prompt=?1",
            [prompt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), wait.as_deref()),
        ("queued", Some("Waits for you to start its lead again"))
    );
    assert_eq!(
        workers_of(&fixture, lead),
        0,
        "no worker of the stopped lead"
    );
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// RV-DS241-122 r2 (rv-code4 F1): more queued tasks than dispatch reads at once are held for
/// a lead the owner stopped, and a task queued after them with no lead still starts: held
/// tasks stay out of dispatch's window, and every one of them says it waits for the lead.
#[test]
fn tasks_held_for_an_owner_stopped_lead_never_fill_the_dispatch_window() {
    let fixture = Fixture::new(2);
    let _home = committed_project(&fixture);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, LEAD_CODEX, TeamLimits::default());
    let lead = idle_sub_agent(&fixture, &mut team);
    let prompt = "Write the notes page.";
    let (queued, refused) = lead_spawn_answer(&fixture, lead, prompt);
    assert!(!refused, "{queued}");
    let held = DISPATCH_TASKS_MAX + 1;
    for copy in 1..held {
        fixture
            .connection
            .execute(
                "INSERT INTO team_tasks(project,source,turn,harness,tools,prompt,status,created_ms,parent,mode)
                 SELECT project,source,turn,harness,tools,?2,'queued',created_ms,parent,mode
                 FROM team_tasks WHERE prompt=?1",
                params![prompt, format!("Write the notes page, part {copy}.")],
            )
            .unwrap();
    }
    let of_lead = |fixture: &Fixture| -> i64 {
        fixture
            .connection
            .query_row(
                "SELECT COUNT(*) FROM team_tasks WHERE parent=?1 AND status='queued'",
                [lead],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(of_lead(&fixture), held, "the lead's tasks are queued");
    owner(
        &fixture,
        &[crate::team::TeamAction::StopAgent {
            project_id: fixture.setup.project_id.clone(),
            agent: lead,
        }],
    );
    fixture.until(&mut team, "the owner's stop ends the lead", |f| {
        f.agent_status(lead).0 == AgentStatus::Stopped
    });
    let later = "Write the index page.";
    owner(&fixture, &[hand_off(&fixture, later)]);
    fixture.until(
        &mut team,
        "a task queued after the held ones starts",
        |f| task_of(f, later).0 != "queued",
    );
    let waiting: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE parent=?1 AND status='queued' AND wait=?2",
            params![lead, WAIT_LEAD_OWNER_STOPPED],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (of_lead(&fixture), waiting),
        (held, held),
        "every held task waits for its lead, none waits behind the window"
    );
    assert_eq!(
        workers_of(&fixture, lead),
        0,
        "no worker of the stopped lead"
    );
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// RV-DS241-125: the owner stops a sub-agent while it is stopping to load changed
/// instructions. The restart is dropped with one team note: the agent is not started again
/// and the owner's stop stays, so only the owner's Wake starts it (AW-1).
#[test]
fn an_owner_stop_during_an_instruction_restart_keeps_the_agent_stopped() {
    let fixture = Fixture::new(2);
    let _home = committed_project(&fixture);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, LEAD_CODEX, TeamLimits::default());
    let agent = idle_sub_agent(&fixture, &mut team);
    let project = fixture.setup.project_id.clone();
    let worker = team.workers.get_mut(&agent).expect("the agent runs");
    worker.restarting = true;
    worker.stop_for("engine_stop:instructions");
    let _ = worker.orders.send(Order::Stop);
    team.instruction_restarts.insert(agent, project.clone());
    owner(
        &fixture,
        &[crate::team::TeamAction::StopAgent {
            project_id: project.clone(),
            agent,
        }],
    );
    let began = Instant::now();
    while team.workers.contains_key(&agent) {
        assert!(began.elapsed() < Duration::from_secs(20), "the agent ends");
        team.tick().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    idle_ticks(&mut team);
    assert!(
        !team.workers.contains_key(&agent),
        "it is not started again"
    );
    assert_eq!(fixture.agent_status(agent).0, AgentStatus::Stopped);
    assert!(crate::team::owner_stopped(&fixture.connection, agent).unwrap());
    assert_eq!(
        noted(
            &fixture,
            "An agent that stopped to load your changed instructions was stopped meanwhile, so it was not started again."
        ),
        1
    );
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// RV-DS241-126: the owner switches the Master run time to No limit while the runtime ends a
/// live master past it, as a second connection flipping it between No limit and the default
/// does here. Ending the run never panics on a run time read twice: whichever value it
/// read, the call returns.
#[test]
fn a_run_time_switched_to_no_limit_while_the_master_run_ends_never_panics() {
    const ROUNDS: u32 = 2000;
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let secs = crate::team::master_limits::RUN_DEFAULT_SECS;
    choose_run_time(&fixture, Some(secs));
    let (mut team, id) = idle_live_master(&fixture, &host, Duration::from_secs(3600));
    stand_in_work(&fixture);
    owner_last_seen(&fixture, now_ms() - i64::from(secs) * 1000 - 1000);
    fixture
        .connection
        .busy_timeout(Duration::from_secs(5))
        .unwrap();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flipper = {
        let done = done.clone();
        let path = fixture.base.join("engine.db");
        std::thread::spawn(move || {
            let connection = Connection::open(path).unwrap();
            connection.busy_timeout(Duration::from_secs(5)).unwrap();
            let began = Instant::now();
            let mut flips = 0_u64;
            while !done.load(Ordering::SeqCst) && began.elapsed() < Duration::from_secs(60) {
                let run = if flips % 2 == 0 { 0 } else { i64::from(secs) };
                if connection
                    .execute(
                        "UPDATE team_master_limits SET run_secs=?2 WHERE project=?1",
                        params![PROJECT, run],
                    )
                    .is_err()
                {
                    break;
                }
                flips += 1;
            }
            flips
        })
    };
    for _ in 0..ROUNDS {
        team.end_master_run(&fixture.setup).unwrap();
        team.workers
            .get_mut(&id)
            .expect("the master's worker stays until a tick")
            .stopping = false;
        super::super::super::master::set_live(&fixture.connection, &fixture.setup.project_id, true)
            .unwrap();
    }
    done.store(true, Ordering::SeqCst);
    assert!(flipper.join().unwrap() > 0, "the run time was flipped");
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}
