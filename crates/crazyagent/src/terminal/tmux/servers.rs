use super::super::{
    AgentId, Host, HostError, OWN_SERVER_UNAVAILABLE, PaneInfo, PaneRef, TerminalStream, failure,
};
use super::{Tmux, harness_socket, user};
use crate::harnesses::interactive::InteractiveCommand;
use std::collections::BTreeMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most agent folders the engine reads when it looks for native-mode leads whose own
/// servers still run, such as after an engine restart.
const LEADS_SCAN_MAX: usize = 4096;
/// The most lead servers the engine keeps a client for at once.
const LEADS_KEPT_MAX: usize = 256;

/// The engine's terminal host across its tmux servers: its own private server, and one
/// server per native-mode lead started inside that lead's Seatbelt profile. Every call for
/// an agent whose own server listens goes to that server, through a client inside the
/// lead's profile; every other call goes to the engine's server. So team code, viewers and
/// stops reach a native lead and its teammate panes by name, and no unconfined client ever
/// connects to a server a lead controls.
pub struct Servers {
    engine: Tmux,
    root: PathBuf,
    program: Option<PathBuf>,
    leads: Mutex<BTreeMap<AgentId, Arc<Tmux>>>,
}

/// Whether a lead's own socket is a live socket of this user, read without following a link.
/// Takes the socket path. Returns the answer.
fn listening(socket: &Path) -> bool {
    std::fs::symlink_metadata(socket)
        .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.uid() == user())
}

/// The installed Seatbelt profile of a team agent, which its own server and every engine
/// client on it run inside. Takes the engine root and the agent. Returns the profile path,
/// or `terminal_own_server_unavailable` when it is not this user's plain file.
fn profile_of(root: &Path, agent: &AgentId) -> Result<PathBuf, HostError> {
    let id = agent
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    let profile = crate::team::runtime::agent_profile(&root.join("team"), id);
    let plain = std::fs::symlink_metadata(&profile)
        .is_ok_and(|metadata| metadata.is_file() && metadata.uid() == user());
    if !plain {
        return Err(failure(OWN_SERVER_UNAVAILABLE));
    }
    Ok(profile)
}

impl Servers {
    /// Opens the host on the engine's own server and finds lead servers by name. Takes the
    /// engine's server host, the engine root and the tmux program. Returns the host; nothing
    /// starts until the first call.
    pub fn new(engine: Tmux, root: &Path, program: Option<PathBuf>) -> Self {
        Self {
            engine,
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            program,
            leads: Mutex::new(BTreeMap::new()),
        }
    }

    /// The lead's own server when one listens for it now, else none; a name that is not a
    /// team agent has none. Takes the agent. Returns the server host or the failure to open
    /// one.
    fn own(&self, agent: &AgentId) -> Result<Option<Arc<Tmux>>, HostError> {
        let Ok(socket) = harness_socket(&self.root, agent) else {
            return Ok(None);
        };
        if !listening(&socket) {
            if let Ok(mut leads) = self.leads.lock() {
                leads.remove(agent);
            }
            return Ok(None);
        }
        self.server(agent, &socket).map(Some)
    }

    /// The client host for one lead's own server, opened once and kept while it listens.
    /// Takes the lead and its socket. Returns the host, or a refusal when the lead has no
    /// installed profile or too many lead servers are kept.
    fn server(&self, lead: &AgentId, socket: &Path) -> Result<Arc<Tmux>, HostError> {
        let mut leads = self
            .leads
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if let Some(found) = leads.get(lead) {
            return Ok(found.clone());
        }
        leads.retain(|_, server| listening(&server.socket()));
        if leads.len() >= LEADS_KEPT_MAX {
            return Err(failure("terminal_unavailable"));
        }
        let profile = profile_of(&self.root, lead)?;
        let server = Arc::new(Tmux::confined(self.program.clone(), socket, &profile)?);
        leads.insert(lead.clone(), server.clone());
        assert!(leads.len() <= LEADS_KEPT_MAX);
        Ok(server)
    }

    /// The native-mode leads whose own servers listen now, found from the team's agent
    /// folders, at most `LEADS_SCAN_MAX` read. Takes nothing. Returns the leads in order.
    fn leads_listening(&self) -> Vec<AgentId> {
        let Ok(entries) = std::fs::read_dir(self.root.join("team").join("agents")) else {
            return Vec::new();
        };
        let mut leads: Vec<AgentId> = entries
            .take(LEADS_SCAN_MAX)
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = name.to_str()?.strip_prefix("agent-")?.parse::<i64>().ok()?;
                AgentId::team(id).ok()
            })
            .filter(|lead| harness_socket(&self.root, lead).is_ok_and(|socket| listening(&socket)))
            .collect();
        leads.sort();
        assert!(leads.len() <= LEADS_SCAN_MAX);
        leads
    }

    /// Runs one call on the server that holds an agent's terminal. Takes the agent and the
    /// call. Returns the call's result.
    fn on<T>(
        &self,
        agent: &AgentId,
        call: impl FnOnce(&Tmux) -> Result<T, HostError>,
    ) -> Result<T, HostError> {
        match self.own(agent)? {
            Some(server) => call(&server),
            None => call(&self.engine),
        }
    }
}

impl Host for Servers {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        if let Some(server) = self.own(agent)?
            && server.alive()?.contains(agent)
        {
            return Err(failure("terminal_agent_running"));
        }
        self.engine.spawn(agent, command)
    }

    fn spawn_own_server(
        &self,
        agent: &AgentId,
        command: &InteractiveCommand,
    ) -> Result<PaneRef, HostError> {
        if !cfg!(target_os = "macos") || self.program.is_none() {
            return Err(failure(OWN_SERVER_UNAVAILABLE));
        }
        if self.engine.alive()?.contains(agent) {
            return Err(failure("terminal_agent_running"));
        }
        let socket = harness_socket(&self.root, agent)?;
        let server = self.server(agent, &socket)?;
        server.spawn(agent, command)
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.on(agent, |server| server.attach(agent))
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.on(agent, |server| server.snapshot(agent))
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.on(agent, |server| server.input(agent, bytes))
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.on(agent, |server| server.input_live(agent))
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<Duration>, HostError> {
        self.on(agent, |server| server.quiet(agent))
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.on(agent, |server| server.resize(agent, cols, rows))
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let Some(server) = self.own(agent)? else {
            return self.engine.kill(agent);
        };
        server.close(&[])?;
        if let Ok(mut leads) = self.leads.lock() {
            leads.remove(agent);
        }
        Ok(())
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        self.on(agent, |server| server.panes(agent))
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        self.on(agent, |server| server.kill_pane(agent, pane))
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.on(agent, |server| server.attach_pane(agent, pane))
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.on(agent, |server| server.snapshot_pane(agent, pane))
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.on(agent, |server| server.input_pane(agent, pane, bytes))
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        self.on(agent, |server| server.input_live_pane(agent, pane))
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        let mut alive = self.engine.alive()?;
        for lead in self.leads_listening() {
            let running = self
                .own(&lead)
                .ok()
                .flatten()
                .and_then(|server| server.alive().ok())
                .is_some_and(|agents| agents.contains(&lead));
            if running {
                alive.push(lead);
            }
        }
        alive.sort();
        alive.dedup();
        Ok(alive)
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        for lead in self.leads_listening() {
            if keep.contains(&lead) {
                continue;
            }
            match self.kill(&lead) {
                Ok(()) => {}
                Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                Err(problem) => return Err(problem),
            }
        }
        self.engine.close(keep)
    }
}
