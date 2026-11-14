use super::super::managed_agent::{AGENT, ManagedAgent, PROJECT as AGENT_PROJECT};
use super::*;

const RUN_TIME_PASSED: &str = "The native lead's run time ended before it was taken over";

/// A per-turn agent on the engine's own managed profile, run by a stand-in Codex that
/// records what it is typed and holds a signed-in ChatGPT record, so the account handoff
/// reaches its time check. Input: the fixture. Output: the prepared agent.
fn stand_in_codex(fixture: &Fixture) -> ManagedAgent {
    let bin = fixture.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, recording_codex()).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agent = ManagedAgent::in_pane("codex", &codex, fixture.host(), AgentRole::Sub);
    let record = agent.profile_home.join("auth.json");
    std::fs::write(&record, signed_in_codex_record()).unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    agent
}

/// The account grant the runtime's first order to a native lead carries, held from the
/// stand-in agent's managed profile. Inputs: the agent and the launch's name. Output: the
/// grant.
fn grant(agent: &mut ManagedAgent, launch: &str) -> Box<Grant> {
    let context = agent
        .runtime
        .context(
            &crate::protocol::ProjectId::new(AGENT_PROJECT).unwrap(),
            AGENT,
            AgentRole::Sub,
            true,
            crate::agent_git::Layout::Linked,
        )
        .unwrap();
    Box::new(Grant {
        profile: agent.acquire(launch),
        context,
        access: AgentAccess::Sandboxed,
    })
}

/// RV-DS241-130: a native lead an earlier engine left running, taken over after its
/// launch's run time passed, is ended with the launch deadline before any account handoff:
/// its pane is gone and its launch's context files, which hold MCP server secrets, are
/// removed, whether its first order is a turn without the account or a start with it. A
/// taken turn ends timed out with the deadline. Before the fix the granted start handed the
/// account a zero-length turn and panicked the worker, and the one without the account
/// ended as taken over without it.
#[test]
fn a_native_lead_taken_over_past_its_run_time_ends_at_the_deadline_before_any_handoff() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut agent = stand_in_codex(&fixture);
    let cases = [
        (
            Order::Turn(
                "Plan the page".into(),
                None,
                TurnRules {
                    limit: None,
                    background: crate::team::BackgroundAtEnd::Wait,
                },
            ),
            None,
        ),
        (
            Order::Start(
                "Your brief".into(),
                Some(grant(&mut agent, "ds130-lead-late")),
            ),
            Some(agent.home.clone()),
        ),
    ];
    for (first, granted_home) in cases {
        let taken = matches!(first, Order::Turn(..));
        let id = fixture.agent(AgentRole::Sub, "claude-code", AgentStatus::Working);
        let home = granted_home.unwrap_or_else(|| {
            let home = fixture.base.join(format!("lead-home-{id}"));
            std::fs::create_dir(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            home
        });
        let state = home.join(crate::launch_context::STATE);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("mcp-secret.json"), "{}").unwrap();
        let limits = LaunchLimits::default();
        let mut record = fixture.pane(&host, id);
        record.context = Some(crate::launch_context::InstalledFiles {
            home: home.clone(),
            codex: false,
        });
        record.since_ms -= u64::try_from(limits.run_time.as_millis()).unwrap() + 60_000;
        assert_eq!(record.run_time_left(limits.run_time), Duration::ZERO);
        let agent_id = AgentId::team(id).unwrap();
        assert!(host.alive().unwrap().contains(&agent_id));
        let adoption = Adoption {
            host: host.clone(),
            agent: agent_id.clone(),
            record,
            limits,
            live: false,
            managed: Some(Managed {
                home: home.clone(),
                executable: PathBuf::from("/bin/sh"),
            }),
            access: AgentAccess::Sandboxed,
            toolchains: Some(Vec::new()),
            network: crate::sandbox::policy::AgentNetwork::Open,
            proxy: None,
            task_start: None,
            grants: Vec::new(),
        };
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders.send(first).unwrap();
        let problem = native_adopted(
            adoption,
            &ordered,
            &reports,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &test_by(),
        );
        drop(reports);
        let reports: Vec<Report> = reported.iter().collect();
        assert_eq!(
            problem,
            Some(with_code(RUN_TIME_PASSED, Some("launch_deadline"))),
            "taken {taken}"
        );
        assert!(
            !host.alive().unwrap().contains(&agent_id),
            "taken {taken}: a lead past its run time never runs on unsupervised"
        );
        assert!(
            !state.exists(),
            "taken {taken}: the launch's context files are removed with the lead"
        );
        assert_eq!(reports.len(), usize::from(taken), "taken {taken}");
        assert!(reports.iter().all(|report| matches!(
            report,
            Report::Finished {
                end: TurnEnd::TimedOut,
                cause: Some(cause),
                ..
            } if cause == "launch_deadline"
        )));
    }
}

/// RV-DS241-130: a per-turn agent given a turn with no time left, as a session limit its
/// agent already used up gives one, ends that turn timed out by the turn limit without
/// handing it the account or starting its harness. Before the fix the zero bound reached
/// the account handoff's assert and panicked the worker.
#[test]
fn a_per_turn_agent_given_no_time_left_times_out_without_a_handoff() {
    let fixture = Fixture::new(1);
    let mut agent = stand_in_codex(&fixture);
    let turns = vec![("Write the page", agent.acquire("ds130-zero-turn"))];
    let (reports, problem) = agent.run_turns(None, turns, Duration::ZERO);
    assert_eq!(problem, None);
    assert_eq!(reports.len(), 1, "one report: the turn's end, no start");
    let ends: Vec<(TurnEnd, Option<String>)> = reports
        .into_iter()
        .filter_map(|report| match report {
            Report::Finished { end, cause, .. } => Some((end, cause)),
            _ => None,
        })
        .collect();
    assert_eq!(ends, [(TurnEnd::TimedOut, Some(TURN_LIMIT.to_owned()))]);
    assert!(
        !agent.home.join("typed").exists(),
        "the harness never starts for a turn with no time left"
    );
}
