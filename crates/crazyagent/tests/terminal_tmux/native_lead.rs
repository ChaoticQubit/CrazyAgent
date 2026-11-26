use super::*;

/// One engine root with team agent 1 ready to lead in its own server: its home, its short
/// private temp folder that holds its own and its teammates' sockets and allow-default
/// installed profiles for it and its teammates.
#[cfg(target_os = "macos")]
struct Lead {
    root: tempfile::TempDir,
    tmux: PathBuf,
    home: PathBuf,
    profile: PathBuf,
    socket: PathBuf,
    mates: PathBuf,
}

#[cfg(target_os = "macos")]
impl Lead {
    fn new() -> Self {
        let root = root();
        let tmux = tmux(root.path());
        let path = root.path().canonicalize().unwrap();
        let team = path.join("team");
        let home = crazyagent::team::runtime::agent_home(&team, 1);
        std::fs::create_dir_all(&home).unwrap();
        kernel_sandbox::toolchains::make_tmp(&crazyagent::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
        let profile = crazyagent::team::runtime::agent_profile(&team, 1);
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
        std::fs::write(
            crazyagent::team::runtime::agent_teammates_profile(&team, 1),
            "(version 1)\n(allow default)\n",
        )
        .unwrap();
        let socket =
            crazyagent::terminal::tmux::harness_socket(&path, &AgentId::team(1).unwrap()).unwrap();
        let mates =
            crazyagent::terminal::tmux::renew_teammates_socket(&path, &AgentId::team(1).unwrap())
                .unwrap();
        Self {
            root,
            tmux,
            home,
            profile,
            socket,
            mates,
        }
    }

    fn path(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }

    fn shell(&self, prompt: &str, argv: &[&str]) -> InteractiveCommand {
        shell(
            &self.path(),
            vec![
                ("PS1".into(), prompt.into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
            argv,
        )
    }

    /// The lead's own command: the shell started under its installed profile, as the team
    /// runtime starts a native lead. Takes the prompt and the argv. Returns the command.
    fn confined(&self, prompt: &str, argv: &[&str]) -> InteractiveCommand {
        let profile = self.profile.to_str().unwrap();
        let mut confined = vec![SANDBOX_EXEC, "-f", profile];
        confined.extend_from_slice(argv);
        self.shell(prompt, &confined)
    }

    /// Types a tmux command into the lead's pane that prints the new pane's id into a file,
    /// and waits for it. Takes the host, the lead, the tmux arguments before `-P` and the
    /// command the new pane runs. Returns the new pane id.
    fn new_pane(&self, host: &dyn Host, lead: &AgentId, how: &str, run: &str) -> String {
        let out = self
            .path()
            .join(format!("pane-{}.txt", how.replace(' ', "-")));
        let _ = std::fs::remove_file(&out);
        let line = format!(
            "{} {how} -P -F '#{{pane_id}}' '{run}' > {}\n",
            self.tmux.display(),
            out.display()
        );
        host.input(lead, line.as_bytes()).unwrap();
        let id = wait_text(&out).trim().to_owned();
        assert!(kernel_processes::terminal::pane_id_valid(&id), "{id:?}");
        id
    }
}

#[cfg(target_os = "macos")]
impl Drop for Lead {
    fn drop(&mut self) {
        for socket in [
            self.socket.clone(),
            self.mates.clone(),
            self.path().join(FOLDER).join(SOCKET),
        ] {
            let _ = kernel_processes::spawn_gate::status(
                Command::new(&self.tmux)
                    .arg("-S")
                    .arg(&socket)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
        let _ = kernel_sandbox::toolchains::remove_tmp(
            &crazyagent::sandbox::TOOLCHAIN_HOOKS,
            &self.home,
        );
    }
}

/// Waits until nothing accepts connections at `socket`, as after kill-server, whose server
/// ends a moment after the command returns. Takes the socket.
#[cfg(target_os = "macos")]
fn until_refused(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while kernel_processes::spawn_gate::connect_unix_std(socket).is_ok() {
        assert!(
            Instant::now() < deadline,
            "{} still accepts connections",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the keepers a host holds for a lead's ended servers have exited, as the host
/// reads such a server as ended only then, not when its socket first refuses. Takes the host,
/// the lead and whether only the keeper of its teammates' server counts.
#[cfg(target_os = "macos")]
fn until_keepers_end(host: &dyn Host, lead: &AgentId, mates_only: bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while host
        .server_keepers(lead)
        .iter()
        .any(|(_, mates)| !mates_only || *mates)
    {
        assert!(
            Instant::now() < deadline,
            "the keepers of the lead's ended servers still run"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_runs_in_its_own_server_and_every_call_reaches_it_by_name() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let engine_agent = AgentId::team(2).unwrap();
    let command = lead.confined("lead$ ", &["/bin/sh"]);
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&command),
    )
    .unwrap();
    assert!(
        std::fs::symlink_metadata(&lead.socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    host.spawn(
        &engine_agent,
        &kernel_processes::terminal::PaneCommand::from(&lead.shell("engine$ ", &["/bin/sh"])),
    )
    .unwrap();
    let engine_only = Tmux::open(
        Some(lead.tmux.clone()),
        &root.join(FOLDER),
        kernel_sandbox::confinement(),
        crazyagent::terminal::tmux::tmux_hooks(),
    );
    assert_eq!(
        engine_only.alive().unwrap(),
        std::slice::from_ref(&engine_agent)
    );
    assert_eq!(
        host.alive().unwrap(),
        [leader.clone(), engine_agent.clone()]
    );
    assert_eq!(
        host.spawn(
            &leader,
            &kernel_processes::terminal::PaneCommand::from(&command)
        )
        .unwrap_err()
        .0,
        "terminal_agent_running"
    );
    assert_eq!(
        host.spawn_own_server(
            &engine_agent,
            &kernel_processes::terminal::PaneCommand::from(&command)
        )
        .unwrap_err()
        .0,
        "terminal_agent_running"
    );
    let own_pane = host.panes(&leader).unwrap();
    assert_eq!(own_pane.len(), 1);

    let split_out = root.join("split.txt");
    let split = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window",
        &format!("cat > {}", split_out.display()),
    );
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo routed-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "routed-42\n");
    assert!(
        !std::fs::read_to_string(&split_out)
            .unwrap_or_default()
            .contains("routed")
    );
    let panes = host.panes(&leader).unwrap();
    assert_eq!(panes.len(), 2, "{panes:?}");
    assert!(panes.iter().any(|pane| pane.id == split));
    let snapshot = host.snapshot(&leader).unwrap();
    assert!(
        contains(&snapshot.screen, b"lead$"),
        "the snapshot shows the lead's own pane, not the active split: {:?}",
        String::from_utf8_lossy(&snapshot.screen)
    );
    drop(host.attach_pane(&leader, &split).unwrap());
    let window = lead.new_pane(host.as_ref(), &leader, "new-window -d", "sleep 600");
    assert_eq!(
        host.attach_pane(&leader, &window).unwrap_err().0,
        "terminal_pane_unknown"
    );
    assert_eq!(
        host.attach_pane(&leader, &own_pane[0].id).unwrap_err().0,
        "terminal_pane_unknown",
        "the anchor standing for the lead on its teammates' server is no pane to attach"
    );
    drop(host.attach(&leader).unwrap());

    host.close(&[]).unwrap();
    assert!(host.alive().unwrap().is_empty());
    assert!(
        !std::fs::symlink_metadata(&lead.socket)
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    );
    assert!(
        std::fs::symlink_metadata(&lead.mates)
            .is_ok_and(|metadata| metadata.file_type().is_socket()),
        "an ended teammates' server keeps its socket, so no teammate can bind one there"
    );
    assert_eq!(
        kernel_processes::spawn_gate::connect_unix_std(&lead.mates)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    assert_ne!(
        crazyagent::terminal::tmux::renew_teammates_socket(&root, &leader).unwrap(),
        lead.mates,
        "the lead's next start names a new teammates' socket"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_pane_is_reached_and_ended_by_its_own_name_even_after_a_restart() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob, missing] = [2, 3, 9].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    let own_pane = host.panes(&leader).unwrap()[0].id.clone();
    let ann_out = root.join("ann.txt");
    let ann_pane = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window -d",
        &format!("cat > {}", ann_out.display()),
    );
    let bob_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    let window = lead.new_pane(host.as_ref(), &leader, "new-window -d", "sleep 600");

    for (pane, code) in [
        (&own_pane, "terminal_pane_unknown"),
        (&window, "terminal_pane_unknown"),
    ] {
        assert_eq!(host.adopt_pane(&ann, &leader, pane).unwrap_err().0, code);
    }
    assert_eq!(
        host.adopt_pane(&ann, &missing, &ann_pane).unwrap_err().0,
        "terminal_own_server_unavailable"
    );
    assert_eq!(
        host.adopt_pane(&leader, &leader, &ann_pane).unwrap_err().0,
        "terminal_agent_running"
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&bob, &leader, &bob_pane).unwrap();

    host.input(&ann, b"to-ann\n").unwrap();
    assert_eq!(wait_text(&ann_out), "to-ann\n");
    host.kill(&ann).unwrap();
    let left: Vec<String> = host
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane.clone(), bob_pane.clone()]);

    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    restarted.kill(&bob).unwrap();
    let left: Vec<String> = restarted
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane]);
    assert_eq!(restarted.alive().unwrap(), std::slice::from_ref(&leader));
    assert_eq!(
        restarted.kill(&missing).unwrap_err().0,
        "terminal_agent_unknown"
    );
    restarted.close(&[]).unwrap();
}

/// B19L-H1: through the engine's production host a native lead and each teammate adopted
/// into its window are named by their own pane's first process, though the teammates share
/// one window with the lead's anchor, and ending a left teammate pane ends only that pane.
#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_and_each_adopted_teammate_name_their_own_first_process() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob] = [2, 3].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    let ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    let bob_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 601");
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();
    host.adopt_pane(&bob, &leader, &bob_pane).unwrap();
    let panes = host.panes(&leader).unwrap();
    let own_pane = panes[0].id.clone();
    let pids: BTreeMap<String, u32> = panes.into_iter().map(|pane| (pane.id, pane.pid)).collect();
    assert_eq!(pids.len(), 3, "{pids:?}");

    for (agent, pane) in [(&leader, &own_pane), (&ann, &ann_pane), (&bob, &bob_pane)] {
        assert_eq!(
            host.pane_pid(agent).map_err(|problem| problem.0),
            Ok(Some(pids[pane])),
            "{agent}"
        );
    }

    assert_eq!(
        end_left_pane(host.as_ref(), &bob, "test").map_err(|problem| problem.0),
        Ok(true)
    );
    assert!(ends(&[pids[&bob_pane]]));
    let left: Vec<String> = host
        .panes(&leader)
        .unwrap()
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(left, [own_pane, ann_pane.clone()]);
    assert!(process_running(pids[&ann_pane]));
    host.close(&[]).unwrap();
}
/// PLC (d): a native lead's team panes live only on the teammates' server its start named.
/// Its own server numbers its panes on its own, so a teammate pane's id can name a pane the
/// lead split there; once the teammates' server ends no team pane call goes to the lead's
/// own server, so that pane is neither listed, typed into nor adopted as a teammate under
/// the teammate pane's id.
#[cfg(target_os = "macos")]
#[test]
fn a_native_lead_s_team_pane_calls_never_reach_its_own_server_once_its_teammates_server_ended() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let [ann, bob] = [2, 3].map(|id| AgentId::team(id).unwrap());
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !contains(&host.snapshot(&leader).unwrap().screen, b"lead$") {
        assert!(Instant::now() < deadline, "the lead's shell never prompted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let own_out = root.join("own.txt");
    let own_id = root.join("own-id.txt");
    let line = format!(
        "{} -S {} split-window -d -P -F '#{{pane_id}}' 'cat > {}' > {}\n",
        lead.tmux.display(),
        lead.socket.display(),
        own_out.display(),
        own_id.display()
    );
    host.input(&leader, line.as_bytes()).unwrap();
    let own_split = wait_text(&own_id).trim().to_owned();
    let mut ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    for _ in 0..4 {
        if ann_pane == own_split {
            break;
        }
        ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    }
    assert_eq!(
        ann_pane, own_split,
        "a teammate pane on the teammates' server has the id of the lead's split on its own"
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let killed = kernel_processes::spawn_gate::status(
        Command::new(&lead.tmux)
            .arg("-S")
            .arg(&lead.mates)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(killed.success());
    until_refused(&lead.mates);
    until_keepers_end(host.as_ref(), &leader, true);

    let ended = "terminal_teammates_server_ended";
    assert_eq!(host.panes(&leader).unwrap_err().0, ended);
    assert_eq!(
        host.input_pane(&leader, &ann_pane, b"to-own\n")
            .unwrap_err()
            .0,
        ended
    );
    assert_eq!(
        host.adopt_pane(&bob, &leader, &own_split).unwrap_err().0,
        ended
    );
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo lead-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "lead-42\n");
    assert!(
        !std::fs::read_to_string(&own_out)
            .unwrap_or_default()
            .contains("to-own")
    );
    host.close(&[]).unwrap();
}

/// CA-SEC-002: an adopted teammate's calls go only to its lead's teammates' server. Once that
/// server ended while the lead's own server still runs, a pane the lead split there and
/// tagged with the teammate's name receives none of them, and the teammate reads as gone
/// whether or not the engine's own server runs. Ending the teammate, before or after an
/// engine restart, ends no such pane.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_s_calls_never_reach_its_lead_s_own_server_once_its_teammates_server_ended() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let ann = AgentId::team(2).unwrap();
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !contains(&host.snapshot(&leader).unwrap().screen, b"lead$") {
        assert!(Instant::now() < deadline, "the lead's shell never prompted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let ann_pane = lead.new_pane(host.as_ref(), &leader, "split-window -d", "sleep 600");
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let own_out = root.join("own.txt");
    let own_id = root.join("own-id.txt");
    let line = format!(
        "{} -S {} split-window -d -P -F '#{{pane_id}}' 'cat > {}' > {}\n",
        lead.tmux.display(),
        lead.socket.display(),
        own_out.display(),
        own_id.display()
    );
    host.input(&leader, line.as_bytes()).unwrap();
    let own_split = wait_text(&own_id).trim().to_owned();
    tmux_reply(
        &lead.tmux,
        &lead.socket,
        &[
            "set-option",
            "-p",
            "-t",
            &own_split,
            "@crazyagent",
            ann.as_str(),
        ],
    );

    let killed = kernel_processes::spawn_gate::status(
        Command::new(&lead.tmux)
            .arg("-S")
            .arg(&lead.mates)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(killed.success());
    until_refused(&lead.mates);
    until_keepers_end(host.as_ref(), &leader, true);

    let gone = Some("terminal_agent_unknown".to_owned());
    let reads_as_gone = |host: &dyn Host| {
        assert_eq!(
            host.input(&ann, b"to-own\n").err().map(|problem| problem.0),
            gone
        );
        assert_eq!(host.input_live(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.quiet(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.snapshot(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(host.pane_pid(&ann).err().map(|problem| problem.0), gone);
        assert_eq!(
            host.resize(&ann, 100, 40).err().map(|problem| problem.0),
            gone
        );
    };
    reads_as_gone(host.as_ref());
    let bob = AgentId::team(3).unwrap();
    host.spawn(
        &bob,
        &kernel_processes::terminal::PaneCommand::from(&lead.shell("bob$ ", &["/bin/sh"])),
    )
    .unwrap();
    reads_as_gone(host.as_ref());
    assert_eq!(host.kill(&ann).err().map(|problem| problem.0), gone);
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(restarted.kill(&ann).err().map(|problem| problem.0), gone);
    let panes = tmux_reply(
        &lead.tmux,
        &lead.socket,
        &["list-panes", "-a", "-F", "#{pane_id}"],
    );
    assert!(panes.lines().any(|pane| pane == own_split), "{panes}");
    let routed = root.join("routed.txt");
    host.input(
        &leader,
        format!("echo lead-$((40 + 2)) > {}\n", routed.display()).as_bytes(),
    )
    .unwrap();
    assert_eq!(wait_text(&routed), "lead-42\n");
    assert!(
        !std::fs::read_to_string(&own_out)
            .unwrap_or_default()
            .contains("to-own")
    );
    host.close(&[]).unwrap();
}

/// CA-SEC-002 (L2): a teammates' server that could not be reached once is not an ended one.
/// While its socket is away the adopted teammate's call is refused and goes to no other
/// server; once it is back the same teammate is reached again, so one failed connect does
/// not forget a live teammate until the engine restarts.
#[cfg(target_os = "macos")]
#[test]
fn an_adopted_teammate_survives_one_failed_connect_to_its_teammates_server() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let ann = AgentId::team(2).unwrap();
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    let ann_out = root.join("ann.txt");
    let ann_pane = lead.new_pane(
        host.as_ref(),
        &leader,
        "split-window -d",
        &format!("cat > {}", ann_out.display()),
    );
    host.adopt_pane(&ann, &leader, &ann_pane).unwrap();

    let aside = root.join("mates-aside.sock");
    std::fs::rename(&lead.mates, &aside).unwrap();
    assert_eq!(
        host.input(&ann, b"lost\n").err().map(|problem| problem.0),
        Some("terminal_agent_unknown".to_owned())
    );
    std::fs::rename(&aside, &lead.mates).unwrap();
    host.input(&ann, b"to-ann\n").unwrap();
    assert_eq!(wait_text(&ann_out), "to-ann\n");
    host.close(&[]).unwrap();
}

/// Starts a stand-in tmux server at one of a native lead's sockets, as another process could
/// once the engine's server there ended, shaped like the engine's: its session and holder
/// window, whose one pane is tagged as the lead's. Takes the tmux program, the socket and
/// the lead.
#[cfg(target_os = "macos")]
fn stand_in(tmux: &Path, socket: &Path, lead: &AgentId) {
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    let started = kernel_processes::spawn_gate::status(
        Command::new(tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(socket)
            .args([
                "new-session",
                "-d",
                "-s",
                "crazyagent",
                "-n",
                "_crazyagent",
                "cat",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(started.success());
    tmux_reply(
        tmux,
        socket,
        &[
            "set-option",
            "-p",
            "-t",
            "crazyagent:=_crazyagent",
            "@crazyagent",
            lead.as_str(),
        ],
    );
}

/// PLC (c): the engine pins a native lead's teammates' server to the pid it first answered
/// with. Once that server and the lead's own ended and both their keepers confirmed it, the
/// host reads the lead itself as gone, as for any ended lead (gb67): `pane_pid` and a kill
/// are `terminal_agent_unknown`. A call on its team panes names the end of its pinned
/// teammates' server, `terminal_teammates_server_ended`, and drops the teammates' client.
/// Stand-in servers another process then started at both sockets are refused for the lead's
/// team panes: by the same host, though it dropped that client while nothing listened there,
/// and by a host opened again on the same engine root, as after an engine restart, since the
/// pin is kept on disk (PC-L1).
#[cfg(target_os = "macos")]
#[test]
fn a_server_started_at_a_native_lead_s_teammates_socket_after_its_own_ended_is_refused() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    let command = lead.confined("lead$ ", &["/bin/sh"]);
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&command),
    )
    .unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    for socket in [&lead.socket, &lead.mates] {
        let killed = kernel_processes::spawn_gate::status(
            Command::new(&lead.tmux)
                .arg("-S")
                .arg(socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(killed.success());
        until_refused(socket);
    }
    until_keepers_end(host.as_ref(), &leader, false);
    let ended = "terminal_teammates_server_ended";
    assert_eq!(
        host.panes(&leader).unwrap_err().0,
        ended,
        "the team panes of a lead whose servers ended did not name its teammates' server's end"
    );
    assert_eq!(
        host.input_pane(&leader, "%0", b"x\n").unwrap_err().0,
        ended,
        "a call on a team pane of a lead whose servers ended did not name that end"
    );
    assert_eq!(
        host.pane_pid(&leader).unwrap_err().0,
        "terminal_agent_unknown",
        "a lead whose servers ended and whose keepers confirmed it was not found gone"
    );
    assert_eq!(
        host.kill(&leader).unwrap_err().0,
        "terminal_agent_unknown",
        "the host found a server for a lead whose servers ended"
    );
    for socket in [&lead.socket, &lead.mates] {
        stand_in(&lead.tmux, socket, &leader);
    }

    let replaced = "terminal_teammates_server_replaced";
    assert_eq!(host.panes(&leader).unwrap_err().0, replaced);
    assert_eq!(
        host.input_pane(&leader, "%0", b"x\n").unwrap_err().0,
        replaced
    );
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(restarted.panes(&leader).unwrap_err().0, replaced);
    assert_eq!(
        restarted.input_pane(&leader, "%0", b"x\n").unwrap_err().0,
        replaced
    );
}

/// PLC (c) PC-L1: a native lead's teammates' server pin outlives the engine's kill of that
/// lead, which forgets its clients: stand-in servers another process started at both sockets
/// after the kill are refused for the lead's team panes.
#[cfg(target_os = "macos")]
#[test]
fn a_server_started_at_a_native_lead_s_teammates_socket_after_the_lead_was_killed_is_refused() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    host.kill(&leader).unwrap();
    for socket in [&lead.socket, &lead.mates] {
        until_refused(socket);
        stand_in(&lead.tmux, socket, &leader);
    }
    assert_eq!(
        host.panes(&leader).unwrap_err().0,
        "terminal_teammates_server_replaced"
    );
}

/// PLCC-L1: an engine started again on a root whose teammates' server pin record is gone
/// refuses that server for the lead's team panes, where it used to trust whichever answered.
#[cfg(target_os = "macos")]
#[test]
fn a_restarted_engine_refuses_a_teammates_server_whose_pin_record_is_gone() {
    let lead = Lead::new();
    let root = lead.path();
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    let leader = AgentId::team(1).unwrap();
    host.spawn_own_server(
        &leader,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("lead$ ", &["/bin/sh"])),
    )
    .unwrap();
    assert_eq!(host.panes(&leader).unwrap().len(), 1);
    std::fs::remove_file(crazyagent::team::runtime::agent_teammates_server_pid(
        &root.join("team"),
        1,
    ))
    .unwrap();
    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(
        restarted
            .panes(&leader)
            .map(|panes| panes.len())
            .map_err(|problem| problem.0),
        Err("terminal_own_server_unavailable".to_string())
    );
}

/// PLCC-L1: a lead whose pin is refused does not stop a close. The next lead and the
/// engine's own server still end, and the close reports the refusal.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_lead_does_not_stop_the_close_of_the_next_lead_and_the_engine() {
    let lead = Lead::new();
    let root = lead.path();
    let team = root.join("team");
    let (refused, next) = (AgentId::team(1).unwrap(), AgentId::team(2).unwrap());
    let next_home = crazyagent::team::runtime::agent_home(&team, 2);
    std::fs::create_dir_all(&next_home).unwrap();
    kernel_sandbox::toolchains::make_tmp(&crazyagent::sandbox::TOOLCHAIN_HOOKS, &next_home)
        .unwrap();
    for profile in [
        crazyagent::team::runtime::agent_profile(&team, 2),
        crazyagent::team::runtime::agent_teammates_profile(&team, 2),
    ] {
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    }
    let _next_own = Server {
        tmux: lead.tmux.clone(),
        socket: crazyagent::terminal::tmux::harness_socket(&root, &next).unwrap(),
    };
    let _next_mates = Server {
        tmux: lead.tmux.clone(),
        socket: crazyagent::terminal::tmux::renew_teammates_socket(&root, &next).unwrap(),
    };
    let host = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    host.spawn_own_server(
        &refused,
        &kernel_processes::terminal::PaneCommand::from(&lead.confined("one$ ", &["/bin/sh"])),
    )
    .unwrap();
    let next_profile = crazyagent::team::runtime::agent_profile(&team, 2);
    let next_command = lead.shell(
        "two$ ",
        &[
            SANDBOX_EXEC,
            "-f",
            next_profile.to_str().unwrap(),
            "/bin/sh",
        ],
    );
    host.spawn_own_server(
        &next,
        &kernel_processes::terminal::PaneCommand::from(&next_command),
    )
    .unwrap();
    host.spawn(
        &AgentId::team(3).unwrap(),
        &kernel_processes::terminal::PaneCommand::from(&lead.shell("three$ ", &["/bin/sh"])),
    )
    .unwrap();
    std::fs::remove_file(crazyagent::team::runtime::agent_teammates_server_pid(
        &team, 1,
    ))
    .unwrap();

    let restarted = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(
        restarted.close(&[]).map_err(|problem| problem.0),
        Err("terminal_own_server_unavailable".to_string())
    );
    let after = crazyagent::terminal::tmux_host(&root, Some(lead.tmux.clone()));
    assert_eq!(after.alive().unwrap(), Vec::<AgentId>::new());
    kernel_sandbox::toolchains::remove_tmp(&crazyagent::sandbox::TOOLCHAIN_HOOKS, &next_home)
        .unwrap();
}
