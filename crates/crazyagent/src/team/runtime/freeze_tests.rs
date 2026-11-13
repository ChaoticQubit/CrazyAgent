use super::*;
use crate::harnesses::terminal_session::end_left_pane;
use std::net::TcpListener;

/// A sandbox profile that allows everything but signals to processes outside the sandbox,
/// so the pane's warden is armed as under an agent profile.
const CONFINED_SIGNALS: &str =
    "(version 1)\n(allow default)\n(deny signal)\n(allow signal (target same-sandbox))\n";

/// How long a row watches a held port for a request from a frozen pane.
const HELD_WATCH: Duration = Duration::from_secs(3);

/// Opens team runtimes whose project's account is an endpoint, so a kept pane with a model
/// proxy is taken over with that endpoint's host. Input: the fixture. Output: the opener.
fn endpoint_runtimes(fixture: &Fixture) -> impl Fn() -> TeamRuntime + '_ {
    let mut access = fixture.access.clone();
    access.connections[0].endpoint = ModelEndpoint::Http {
        url: "https://models.crazyagent.test/v1".into(),
    };
    move || {
        let access = access.clone();
        TeamRuntime::open(
            &fixture.base.join("engine.db"),
            &fixture.base.join("team"),
            BTreeMap::from([("codex", fixture.base.join("codex"))]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap()
    }
}

/// Finds a model proxy port no socket holds now. Output: the port.
fn free_proxy_port() -> u16 {
    let model = ModelHost::new("models.crazyagent.test", 443).unwrap();
    let port = ModelProxy::bind(7, vec![model], None, &[]).unwrap().port();
    assert!(crate::sandbox::model_proxy::PROXY_PORTS.contains(&port));
    port
}

/// One proxied pane under the keeper, its warden armed, whose stand-in harness started a
/// process in a session of its own and dials both loopback addresses of its proxy port
/// every 100 ms, counting each dial that connected.
struct Proxied {
    agent: AgentId,
    folder: PathBuf,
    port: u16,
}

impl Proxied {
    /// Starts the pane for team agent `id` as an earlier engine would have and keeps its
    /// record. Inputs: the fixture, the host, the agent and its proxy port. Output: the pane.
    fn keep(fixture: &Fixture, host: &Arc<dyn Host>, id: i64, port: u16) -> Self {
        let folder = fixture.base.join(format!("frozen-{id}"));
        std::fs::create_dir(&folder).unwrap();
        let profile = folder.join("agent.sb");
        std::fs::write(&profile, CONFINED_SIGNALS).unwrap();
        let script = format!(
            "/usr/bin/perl -MPOSIX -e 'exit 0 if fork; POSIX::setsid(); open(my $out, \">\", \"$ARGV[0]/child.pid.new\") or die; print $out \"$$\\n\"; close($out); rename(\"$ARGV[0]/child.pid.new\", \"$ARGV[0]/child.pid\"); sleep 1 while 1' '{dir}'\n\
             echo $$ > '{dir}/harness.pid.new' && mv '{dir}/harness.pid.new' '{dir}/harness.pid'\n\
             while :; do for address in 127.0.0.1 ::1; do /usr/bin/nc -z -G 1 $address {port} 2>/dev/null && echo x >> '{dir}/dialed'; done; sleep 0.1; done\n",
            dir = folder.display(),
        );
        let argv = crate::sandbox::kept_pane(vec![
            crate::sandbox::SANDBOX_EXEC.to_owned(),
            "-f".to_owned(),
            profile.display().to_string(),
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            script,
        ])
        .unwrap();
        let file = fixture.base.join(format!("session-{id}.jsonl"));
        std::fs::write(&file, format!("{TURN_OPEN}\n{TURN_DONE}\n")).unwrap();
        let state = agent_pane_state(&fixture.base.join("team"), id);
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        let command = InteractiveCommand {
            argv,
            cwd: fixture.base.clone(),
            env: vec![(
                crate::sandbox::KEEPER_STATE.to_owned(),
                state.display().to_string(),
            )],
            transcript: Transcript::ClaudeSession { file },
        };
        let agent = AgentId::team(id).unwrap();
        let since = SystemTime::now();
        let pane = host.spawn(&agent, &command).unwrap();
        let record = PaneRecord {
            access: Some(AgentAccess::Sandboxed),
            toolchains: Some(Vec::new()),
            proxy: Some(port),
            freeze: Some(true),
            ..PaneRecord::new(&command.transcript, &command.cwd, since, pane.pid, None).unwrap()
        };
        fixture.keep(id, &record);
        let proxied = Self {
            agent,
            folder,
            port,
        };
        proxied.pid("harness.pid");
        proxied.pid("child.pid");
        proxied
    }

    /// Reads a process id the stand-in wrote, waiting for it. Input: the file name. Output:
    /// the id.
    fn pid(&self, name: &str) -> u32 {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(text) = std::fs::read_to_string(self.folder.join(name)) {
                return text.trim().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the stand-in never wrote {name}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The stand-in's processes: the harness and the one in a session of its own.
    fn pids(&self) -> [u32; 2] {
        [self.pid("harness.pid"), self.pid("child.pid")]
    }

    /// How many of the stand-in's dials connected so far.
    fn dialed(&self) -> usize {
        std::fs::read_to_string(self.folder.join("dialed")).map_or(0, |text| text.lines().count())
    }

    /// Waits until every process of the stand-in is gone. Input: what the row checks.
    fn gone(&self, what: &str) {
        let deadline = Instant::now() + WAIT;
        for pid in self.pids() {
            while state_of(pid).is_some() {
                assert!(
                    Instant::now() < deadline,
                    "{what}: process {pid} still runs"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// Ends the pane and every process its stand-in started. Input: the host.
    fn end(&self, host: &Arc<dyn Host>) {
        let _ = end_left_pane(host.as_ref(), &self.agent, "team");
        self.gone("the pane ended at the row's end");
    }
}

/// The first letter of a process's state as `ps` shows it, `T` for stopped, or none for a
/// process that is gone. Input: the process id. Output: the letter.
fn state_of(pid: u32) -> Option<char> {
    let shown = crate::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    String::from_utf8(shown.stdout)
        .unwrap()
        .trim()
        .chars()
        .next()
}

/// Holds a loopback address on `port` that answers nothing. Inputs: the address and the
/// port. Output: the listener, not blocking.
fn hold(address: &str, port: u16) -> TcpListener {
    let listener = crate::spawn_gate::bind_tcp_std((address, port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

/// Tells whether a held listener took a connection. Input: the listener. Output: true then.
fn requested(listener: &TcpListener) -> bool {
    crate::spawn_gate::accept_tcp_std(listener).is_ok()
}

/// DEC-158: a kept pane with a model proxy is stopped once the engine quits, every process
/// of it, a process in a session of its own included, so it sends nothing while its proxy
/// port is free, and the agent shows as frozen.
#[test]
fn a_kept_proxied_pane_is_stopped_after_shutdown_with_its_setsid_child() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let open = endpoint_runtimes(&fixture);
    let id = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let pane = Proxied::keep(&fixture, &host, id, free_proxy_port());
    let mut team = open().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the proxied pane is taken over", |f| {
        f.agent_status(id) == (AgentStatus::Idle, None)
    });

    assert_eq!(team.shutdown().unwrap(), names(&[id]));
    for pid in pane.pids() {
        assert_eq!(state_of(pid), Some('T'), "process {pid} still runs");
    }
    assert_eq!(
        fixture.agent_status(id),
        (AgentStatus::Stopped, Some(FROZEN_NOTE.into()))
    );
    assert!(fixture.recorded(id));
    pane.end(&host);
    host.close(&[]).unwrap();
}

/// DEC-158: a listener that takes a frozen pane's proxy port while no engine runs gets no
/// request from it, and the next engine, finding the port held, ends the pane without
/// letting it run again.
#[test]
fn a_port_taken_while_the_engine_is_closed_gets_no_request_and_ends_the_frozen_pane() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let open = endpoint_runtimes(&fixture);
    let id = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let pane = Proxied::keep(&fixture, &host, id, free_proxy_port());
    let mut team = open().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the proxied pane is taken over", |f| {
        f.agent_status(id) == (AgentStatus::Idle, None)
    });
    assert_eq!(team.shutdown().unwrap(), names(&[id]));

    let held = [hold("127.0.0.1", pane.port), hold("::1", pane.port)];
    let until = Instant::now() + HELD_WATCH;
    while Instant::now() < until {
        assert!(
            !held.iter().any(requested),
            "the frozen pane reached the listener on its proxy port"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut again = open().with_terminal(host.clone()).unwrap();
    assert!(!host.alive().unwrap().contains(&pane.agent));
    assert!(fixture.note(&format!(
        "The terminal of team agent {id} was closed: its model proxy port {} is taken.",
        pane.port
    )));
    pane.gone("the frozen pane whose port is held ended");
    assert!(
        !held.iter().any(requested),
        "the ended pane reached the listener on its proxy port"
    );
    assert!(again.shutdown().unwrap().is_empty());
    host.close(&[]).unwrap();
}

/// DEC-158: a frozen pane runs again only once its proxy holds both loopback addresses:
/// with only ::1 taken the next engine ends it unresumed, and with the port free it runs
/// and reaches its proxy again.
#[test]
fn a_frozen_pane_continues_only_after_both_proxy_addresses_bind() {
    let fixture = Fixture::new(2);
    let host = fixture.host();
    let open = endpoint_runtimes(&fixture);
    let half = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let partly = Proxied::keep(&fixture, &host, half, free_proxy_port());
    let whole = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let freed = Proxied::keep(&fixture, &host, whole, free_proxy_port());
    let mut team = open().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "both proxied panes are taken over", |f| {
        f.agent_status(half) == (AgentStatus::Idle, None)
            && f.agent_status(whole) == (AgentStatus::Idle, None)
    });
    assert_eq!(team.shutdown().unwrap(), names(&[half, whole]));

    let held = hold("::1", partly.port);
    let before = freed.dialed();
    let mut again = open().with_terminal(host.clone()).unwrap();
    assert!(fixture.note(&format!(
        "The terminal of team agent {half} was closed: its model proxy port {} is taken.",
        partly.port
    )));
    partly.gone("the frozen pane with ::1 held ended");
    assert!(
        !requested(&held),
        "the pane reached ::1 although its proxy did not hold it"
    );

    fixture.until(&mut again, "the pane whose port is free runs again", |f| {
        f.agent_status(whole) == (AgentStatus::Idle, None) && freed.dialed() > before
    });
    for pid in freed.pids() {
        assert_ne!(state_of(pid), Some('T'), "process {pid} is still stopped");
    }
    assert_eq!(again.shutdown().unwrap(), names(&[whole]));
    freed.end(&host);
    host.close(&[]).unwrap();
}

/// DEC-158 Q4: the time a pane spent frozen while no engine ran does not count toward its
/// session time once a later engine takes it over.
#[test]
fn a_frozen_span_does_not_count_toward_the_session_time() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let id = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let now = u64::try_from(now_ms()).unwrap();
    let record = PaneRecord {
        since_ms: now - 600_000,
        frozen_ms: Some(now - 540_000),
        ..fixture.pane(&host, id)
    };
    fixture.keep(id, &record);
    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the pane is taken over", |f| {
        f.agent_status(id) == (AgentStatus::Idle, None)
    });
    let ran = team.workers[&id].ran();
    assert!(
        ran >= Duration::from_secs(55) && ran < Duration::from_secs(120),
        "the frozen span counted toward the session: {ran:?}"
    );
    assert_eq!(team.shutdown().unwrap(), names(&[id]));
    host.close(&[]).unwrap();
}

/// One native lead started on its own server and its teammates' server as the lead's
/// worker does, with [`LEAVING_CLAUDE`] as its stand-in, its session left as an engine that
/// quits leaves it, and its pane record naming a model proxy port.
struct NativeLead {
    fixture: Fixture,
    runtime: TeamRuntime,
    host: Arc<dyn Host>,
    id: i64,
    record: PaneRecord,
    left: u32,
    beats: PathBuf,
    _down: LeadServersDown,
}

/// A stand-in Claude Code that, run as a native lead, starts one process in a session of
/// its own, as [`LEAVING_CLAUDE`] does, which then adds a line to `beats` in the lead's
/// folder every 50 ms, so a row sees whether it ran at all.
const BEATING_CLAUDE: &str = r#"#!/bin/sh
/usr/bin/perl -MPOSIX -e 'POSIX::setsid() > 0 or die; open(my $out, ">", "$ARGV[0].part") or die; print $out "$$\n"; close($out); rename("$ARGV[0].part", $ARGV[0]) or die; while (1) { open(my $beat, ">>", $ARGV[1]) or die; print $beat "x\n"; close($beat); select(undef, undef, undef, 0.05) }' "$PWD/left.pid" "$PWD/beats" &
exec /bin/sleep 300
"#;

impl NativeLead {
    /// Starts the lead with [`LEAVING_CLAUDE`]. Output: the lead.
    fn start() -> Self {
        Self::start_with(LEAVING_CLAUDE)
    }

    /// Starts the lead with the stand-in `script` and waits for its process in a session of
    /// its own. Input: the stand-in. Output: the lead.
    fn start_with(script: &str) -> Self {
        use crate::coord::settings::{HarnessKind, SubagentMode, set_mode};
        let fixture = Fixture::new(1);
        let project = ProjectId::new(PROJECT).unwrap();
        set_mode(
            &fixture.connection,
            &project,
            HarnessKind::ClaudeCode,
            SubagentMode::Native,
        )
        .unwrap();
        std::fs::set_permissions(
            fixture.base.join("engine.db"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        crate::sandbox::policy::save(
            &fixture.connection,
            PROJECT,
            &crate::sandbox::policy::AgentPolicy {
                network: crate::sandbox::policy::AgentNetwork::None,
                ..crate::sandbox::policy::AgentPolicy::default()
            },
        )
        .unwrap();
        let host = crate::terminal::tmux_host(&fixture.base, Some(fixture.tmux.clone()));
        let runtime = standin_claude_runtime(&fixture, script)
            .with_terminal(host.clone())
            .unwrap();
        let profile = reference("managed-profile");
        let managed = runtime.custody.as_ref().unwrap().profile_home(&profile);
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o700)).unwrap();
        let account = TeamAccount {
            connection: reference("managed-connection"),
            reach: TeamReach::Managed {
                profile_ref: profile,
                model: None,
            },
        };
        let id = fixture.agent(AgentRole::Sub, "claude-code", AgentStatus::Starting);
        let (_, plan, _) = runtime
            .prepare(
                &fixture.setup,
                id,
                AgentRole::Sub,
                "claude-code",
                true,
                &account,
                false,
                Layout::Linked,
            )
            .unwrap();
        assert!(plan.native, "the lead starts with native teammates");
        let lead = AgentId::team(id).unwrap();
        let mates_socket = crate::terminal::teammates_socket(&fixture.base, &lead).unwrap();
        let down = LeadServersDown {
            host: host.clone(),
            lead: lead.clone(),
            tmux: fixture.tmux.clone(),
            scratch: mates_socket.parent().unwrap().to_path_buf(),
        };
        let home = agent_home(&runtime.state, id);
        let context = runtime
            .context(&project, id, AgentRole::Sub, true, Layout::Linked)
            .unwrap();
        let request = plan
            .request(AccountBinding::Subscription { home }, plan.access)
            .unwrap();
        let session = AgentSession::start_resuming(
            request,
            plan.team_context(context),
            plan.pane.clone(),
            None,
        )
        .unwrap();
        let written = plan.working_dir.join("left.pid");
        let deadline = Instant::now() + WAIT;
        let left = loop {
            if let Some(pid) = std::fs::read_to_string(&written)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "the stand-in never wrote left.pid"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let record = session.pane_record().expect("the lead's pane has a record");
        assert!(
            record.proxy.is_some(),
            "a native lead under None gets a model proxy"
        );
        assert_eq!(record.freeze, Some(true));
        fixture.keep(id, &record);
        session.leave();
        let beats = plan.working_dir.join("beats");
        Self {
            fixture,
            runtime,
            host,
            id,
            record,
            left,
            beats,
            _down: down,
        }
    }

    /// How many beats a [`BEATING_CLAUDE`] lead's process wrote so far.
    fn beats(&self) -> usize {
        std::fs::read_to_string(&self.beats).map_or(0, |text| text.lines().count())
    }

    /// The processes each server keeper of a frozen record keeps: the lead's own server and
    /// its teammates' server, each started as the keeper's child. Input: the frozen record.
    /// Output: their ids.
    fn servers(frozen: &PaneRecord) -> Vec<u32> {
        frozen
            .keepers
            .as_ref()
            .expect("a frozen native lead keeps its server keepers")
            .iter()
            .flat_map(|keeper| {
                let shown = crate::spawn_gate::output(
                    Command::new("/usr/bin/pgrep")
                        .args(["-P", &keeper.pid.to_string()])
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null()),
                )
                .unwrap();
                String::from_utf8(shown.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| line.trim().parse::<u32>().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Ends both servers through their keepers, frozen or not, and waits for the lead's
    /// process in a session of its own to be gone. Input: the frozen record.
    fn end(&self, frozen: &PaneRecord) {
        for keeper in frozen.keepers.iter().flatten() {
            let pid = rustix::process::Pid::from_raw(i32::try_from(keeper.pid).unwrap()).unwrap();
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let deadline = Instant::now() + WAIT;
        while state_of(self.left).is_some() {
            assert!(
                Instant::now() < deadline,
                "the lead's process outlived its servers"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// DEC-158 Q3: a kept native lead with a model proxy is frozen through the keepers of both
/// its servers, so its own server, its teammates' server and a process it started in a
/// session of its own all stop.
#[test]
fn a_kept_proxied_native_lead_is_stopped_in_both_servers_when_frozen() {
    let lead = NativeLead::start();
    let frozen = lead
        .runtime
        .freeze_kept(&lead.host, lead.id, &lead.record)
        .expect("the native lead froze");
    assert!(frozen.frozen_ms.is_some());
    assert_eq!(frozen.keepers.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        state_of(lead.left),
        Some('T'),
        "the lead's own process runs"
    );
    for pid in NativeLead::servers(&frozen) {
        assert_eq!(state_of(pid), Some('T'), "server process {pid} runs");
    }
    lead.end(&frozen);
}

/// DEC-158 Q3: an engine that starts while a native lead is frozen does not wait on its
/// stopped servers, and leaves no process of the lead stopped: the lead runs again after
/// its proxy binds, or ends.
#[test]
fn an_engine_start_does_not_wait_on_a_frozen_native_lead_and_leaves_it_unfrozen() {
    let lead = NativeLead::start();
    let frozen = lead
        .runtime
        .freeze_kept(&lead.host, lead.id, &lead.record)
        .expect("the native lead froze");
    lead.fixture
        .connection
        .execute(
            "UPDATE team_panes SET record=?2 WHERE agent=?1",
            params![lead.id, frozen.encode().unwrap()],
        )
        .unwrap();
    let (opened, started) = std::sync::mpsc::channel();
    let host = lead.host.clone();
    let next = standin_claude_runtime(&lead.fixture, LEAVING_CLAUDE);
    std::thread::spawn(move || {
        let _ = opened.send(next.with_terminal(host).map(|_| ()).map_err(|e| e.0));
    });
    started
        .recv_timeout(WAIT)
        .expect("the engine start waited on the frozen lead's servers")
        .unwrap();
    assert_ne!(
        state_of(lead.left),
        Some('T'),
        "the lead's own process stayed stopped"
    );
    for pid in NativeLead::servers(&frozen) {
        assert_ne!(
            state_of(pid),
            Some('T'),
            "server process {pid} stayed stopped"
        );
    }
    lead.end(&frozen);
}

/// Moves a native lead onto a Claude Code endpoint account named by an https host, so the
/// next engine can bind its model proxy again and nothing but a takeover check refuses
/// it. Takes the lead. Returns the access the next engine opens with.
fn admissible(lead: &NativeLead) -> ModelAccess {
    let mut access = lead.fixture.access.clone();
    let mut named = access.connections[0].clone();
    named.id = reference("named-claude");
    named.harness = reference("claude");
    named.protocol = reference("anthropic-messages");
    named.endpoint = ModelEndpoint::Http {
        url: "https://models.crazyagent.test/v1".into(),
    };
    access.connections.push(named);
    access.pools[0].members.push(reference("named-claude"));
    lead.fixture
        .connection
        .execute(
            "UPDATE team_agents SET connection='named-claude' WHERE id=?1",
            [lead.id],
        )
        .unwrap();
    access
}

/// DEC-158: a frozen native lead on its signed-in account that every takeover check
/// admits, its native teammates checked against the engine's terminal host, is thawed
/// before the next engine lists its panes and is taken over: its process runs again.
#[test]
fn a_frozen_native_lead_the_next_engine_admits_runs_again() {
    let mut lead = NativeLead::start_with(BEATING_CLAUDE);
    let deadline = Instant::now() + WAIT;
    while lead.beats() == 0 {
        assert!(Instant::now() < deadline, "the lead's process never beat");
        std::thread::sleep(Duration::from_millis(50));
    }
    let frozen = lead
        .runtime
        .freeze_kept(&lead.host, lead.id, &lead.record)
        .expect("the native lead froze");
    lead.fixture
        .connection
        .execute(
            "UPDATE team_panes SET record=?2 WHERE agent=?1",
            params![lead.id, frozen.encode().unwrap()],
        )
        .unwrap();
    let mut setup = lead.fixture.setup.clone();
    setup.team.sub_harnesses = vec![HarnessSlot {
        harness: "claude-code".into(),
        agents: 1,
        tools: false,
    }];
    lead.fixture
        .connection
        .execute(
            "UPDATE project_setups SET body=?2 WHERE project=?1",
            params![PROJECT, serde_json::to_string(&setup).unwrap()],
        )
        .unwrap();
    let managed = ModelConnection {
        id: reference("managed-connection"),
        provider: reference("anthropic"),
        harness: reference("claude-code"),
        protocol: reference("native"),
        account_ref: reference("managed-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("anthropic"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("managed-profile"),
        },
    };
    lead.fixture
        .access
        .accounts
        .push(managed.account_ref.clone());
    lead.fixture.access.pools[0]
        .members
        .push(managed.id.clone());
    lead.fixture.access.connections.push(managed);
    lead.fixture
        .connection
        .execute(
            "UPDATE team_agents SET connection='managed-connection' WHERE id=?1",
            [lead.id],
        )
        .unwrap();
    let before = lead.beats();

    let mut next = standin_claude_runtime(&lead.fixture, BEATING_CLAUDE)
        .with_terminal(lead.host.clone())
        .unwrap();
    assert!(
        next.workers.contains_key(&lead.id),
        "the admitted frozen lead was not taken over: {:#?}",
        lead.fixture.status()
    );
    let deadline = Instant::now() + WAIT;
    while lead.beats() == before {
        assert!(
            Instant::now() < deadline,
            "the taken-over lead never ran again"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = next.shutdown();
    lead.end(&frozen);
}

/// A terminal host that cannot list its panes, as a tmux server that does not answer;
/// every other call goes to the real host.
struct ListFails(Arc<dyn Host>);

impl Host for ListFails {
    fn spawn(
        &self,
        agent: &AgentId,
        command: &InteractiveCommand,
    ) -> Result<crate::terminal::PaneRef, crate::terminal::HostError> {
        self.0.spawn(agent, command)
    }

    fn attach(
        &self,
        agent: &AgentId,
    ) -> Result<crate::terminal::TerminalStream, crate::terminal::HostError> {
        self.0.attach(agent)
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), crate::terminal::HostError> {
        self.0.input(agent, bytes)
    }

    fn resize(
        &self,
        agent: &AgentId,
        cols: u16,
        rows: u16,
    ) -> Result<(), crate::terminal::HostError> {
        self.0.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), crate::terminal::HostError> {
        self.0.kill(agent)
    }

    fn alive(&self) -> Result<Vec<AgentId>, crate::terminal::HostError> {
        Err(crate::terminal::HostError("terminal_list_failed".into()))
    }
}

/// A host found for a runtime that started without one but that cannot list its panes is
/// not kept, now that frozen native leads thaw against the host before the listing, so the
/// next live master start looks for a host again.
#[test]
fn a_host_that_cannot_list_its_panes_is_not_kept() {
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime();
    assert!(team.terminal.is_none());
    let failing: Arc<dyn Host> = Arc::new(ListFails(fixture.host()));
    assert!(team.attach_terminal(failing).is_err());
    assert!(
        team.terminal.is_none(),
        "a host that cannot list its panes was kept"
    );
}

/// DEC-158: a frozen native lead that the next engine refuses, here because its team was
/// paused while no engine ran, is ended while still frozen: no process of it runs again,
/// not even for the moment before the refusal, so it never runs with its proxy port free.
#[test]
fn a_frozen_native_lead_the_next_engine_refuses_ends_without_running_again() {
    let mut lead = NativeLead::start_with(BEATING_CLAUDE);
    let deadline = Instant::now() + WAIT;
    while lead.beats() == 0 {
        assert!(Instant::now() < deadline, "the lead's process never beat");
        std::thread::sleep(Duration::from_millis(50));
    }
    let frozen = lead
        .runtime
        .freeze_kept(&lead.host, lead.id, &lead.record)
        .expect("the native lead froze");
    assert_eq!(
        state_of(lead.left),
        Some('T'),
        "the lead's own process runs"
    );
    lead.fixture
        .connection
        .execute(
            "UPDATE team_panes SET record=?2 WHERE agent=?1",
            params![lead.id, frozen.encode().unwrap()],
        )
        .unwrap();
    lead.fixture
        .connection
        .execute(
            "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,1)",
            [PROJECT],
        )
        .unwrap();
    let access = admissible(&lead);
    assert!(
        matches!(
            lead.runtime.rebind(lead.id, &frozen, &access),
            Ok(Ok(Some(_)))
        ),
        "the lead's proxy cannot bind again, so the pause is not what refuses it"
    );
    lead.fixture.access = access;
    let before = lead.beats();

    let mut next = standin_claude_runtime(&lead.fixture, BEATING_CLAUDE)
        .with_terminal(lead.host.clone())
        .unwrap();
    assert!(lead.fixture.note(&format!(
        "The terminal of team agent {} was closed: the team is paused.",
        lead.id
    )));
    let deadline = Instant::now() + WAIT;
    while state_of(lead.left).is_some() {
        assert!(
            Instant::now() < deadline,
            "the refused lead's process still runs"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        lead.beats(),
        before,
        "the refused lead ran again after its freeze"
    );
    assert!(next.shutdown().unwrap().is_empty());
    lead.end(&frozen);
}

/// DEC-158: when the quitting engine cannot store one frozen pane's record, that pane is
/// ended rather than left frozen with nothing to thaw it by, every later proxied pane is
/// still frozen, and the failure is still reported.
#[test]
fn a_frozen_record_that_cannot_be_stored_ends_its_pane_and_later_panes_still_freeze() {
    let fixture = Fixture::new(2);
    let host = fixture.host();
    let open = endpoint_runtimes(&fixture);
    let first = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let failing = Proxied::keep(&fixture, &host, first, free_proxy_port());
    let second = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let later = Proxied::keep(&fixture, &host, second, free_proxy_port());
    assert!(first < second, "the failing pane is frozen first");
    let mut team = open().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "both proxied panes are taken over", |f| {
        f.agent_status(first) == (AgentStatus::Idle, None)
            && f.agent_status(second) == (AgentStatus::Idle, None)
    });
    fixture
        .connection
        .execute_batch(&format!(
            "CREATE TRIGGER refused_record BEFORE UPDATE ON team_panes WHEN NEW.agent={first} \
             BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;"
        ))
        .unwrap();

    assert!(
        team.shutdown().is_err(),
        "the failed store was not reported"
    );
    for pid in later.pids() {
        assert_eq!(
            state_of(pid),
            Some('T'),
            "process {pid} of the later pane still runs"
        );
    }
    failing.gone("the pane whose frozen record could not be stored ended");
    later.end(&host);
    host.close(&[]).unwrap();
}

/// DEC-158: a pane whose record no longer fits its bound once it says when the pane froze
/// is ended while the engine quits, not kept frozen with no record to thaw it by.
#[test]
fn a_pane_whose_frozen_record_does_not_fit_is_ended_not_kept() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let open = endpoint_runtimes(&fixture);
    let id = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let pane = Proxied::keep(&fixture, &host, id, free_proxy_port());
    let mut team = open().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the proxied pane is taken over", |f| {
        f.agent_status(id) == (AgentStatus::Idle, None)
    });
    let kept = team
        .kept_record(id)
        .unwrap()
        .expect("the taken-over pane has a record");
    let padded = |length: usize| PaneRecord {
        key: Some(PathBuf::from(format!("/{}", "k".repeat(length)))),
        ..kept.clone()
    };
    let record = (1..)
        .map(padded)
        .take_while(|record| record.encode().is_some())
        .last()
        .expect("the record fits with a short key");
    let at = u64::try_from(now_ms()).unwrap();
    assert!(
        PaneRecord {
            frozen_ms: Some(at),
            ..record.clone()
        }
        .encode()
        .is_none(),
        "the frozen record still fits"
    );
    fixture
        .connection
        .execute(
            "UPDATE team_panes SET record=?2 WHERE agent=?1",
            params![id, record.encode().unwrap()],
        )
        .unwrap();

    assert!(team.shutdown().unwrap().is_empty());
    pane.gone("the pane whose frozen record does not fit ended");
    assert!(!fixture.recorded(id));
    host.close(&[]).unwrap();
}

/// DEC-158: a quitting engine that cannot store a native lead's frozen record ends the
/// lead through the keepers of both its servers, so no stopped process of it is left with
/// nothing to thaw it by, and the failure is still reported.
#[test]
fn a_native_lead_whose_frozen_record_cannot_be_stored_is_ended() {
    let lead = NativeLead::start();
    lead.fixture
        .connection
        .execute_batch(&format!(
            "CREATE TRIGGER refused_record BEFORE UPDATE ON team_panes WHEN NEW.agent={} \
             BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
            lead.id
        ))
        .unwrap();

    let (held, problem) = lead.runtime.freeze_or_end(lead.id);
    assert!(
        !held,
        "the lead whose record could not be stored stayed frozen"
    );
    assert!(problem.is_some(), "the failed store was not reported");
    let deadline = Instant::now() + WAIT;
    while state_of(lead.left).is_some() {
        assert!(
            Instant::now() < deadline,
            "the lead's process outlived the failed store"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// DEC-158: a proxied agent's pane starts under a keeper given the engine's freeze record,
/// and its pane record says so, so a quitting engine may freeze it and a later one thaw it.
#[test]
fn a_proxied_agent_s_pane_record_says_its_keeper_can_freeze_it() {
    let fixture = Fixture::new(1);
    let (runtime, id, plan, _) = proxied_agent(&fixture);
    let (request, context) = proxied_launch(&fixture, &runtime, id, &plan);
    let host = fixture.host();
    let session = crate::harnesses::terminal_session::TerminalSession::start(
        host.clone(),
        AgentId::team(id).unwrap(),
        request,
        Some(context),
    )
    .unwrap();
    assert_eq!(
        session.record().and_then(|record| record.freeze),
        Some(true)
    );
    drop(session);
    host.close(&[]).unwrap();
}
