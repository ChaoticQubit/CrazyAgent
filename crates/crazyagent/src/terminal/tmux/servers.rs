use super::super::teammates_socket;
use super::super::{
    AgentId, Host, HostError, OWN_SERVER_UNAVAILABLE, PANES_LISTED_MAX, PaneInfo, PaneRef,
    TEAMMATES_SERVER_ENDED, TerminalStream, failure,
};
use super::{ServerPin, Tmux, harness_socket, user};
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
/// The most adopted teammates the engine remembers the lead of at once.
const MEMBERS_KEPT_MAX: usize = LEADS_KEPT_MAX * PANES_LISTED_MAX;

/// The engine's terminal host across its tmux servers: its own private server, and per
/// native-mode lead one server started inside that lead's Seatbelt profile and one inside
/// its teammates' profile, where the lead's app splits its teammates (rv-audit S1 F1).
/// Every call for an agent whose own server listens goes to that server, through a client
/// inside the lead's profile, every call for a teammate adopted into one to its lead's
/// teammates' server, or its lead's own for a lead started without one; every other call
/// goes to the engine's server. So team code, viewers and stops reach a native lead and its
/// teammates by name, and no unconfined client ever connects to a server a lead controls.
pub struct Servers {
    engine: Tmux,
    root: PathBuf,
    program: Option<PathBuf>,
    leads: Mutex<BTreeMap<AgentId, Arc<Tmux>>>,
    mates: Mutex<BTreeMap<AgentId, Arc<Tmux>>>,
    members: Mutex<BTreeMap<AgentId, AgentId>>,
    pins: Mutex<BTreeMap<AgentId, Arc<ServerPin>>>,
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
    plain_profile(crate::team::runtime::agent_profile(&root.join("team"), id))
}

/// The installed Seatbelt profile of a native lead's teammates, which their own server runs
/// inside. Takes the engine root and the lead. Returns the profile path, or
/// `terminal_own_server_unavailable` when it is not this user's plain file.
fn teammates_profile_of(root: &Path, lead: &AgentId) -> Result<PathBuf, HostError> {
    let id = lead
        .team_agent()
        .ok_or_else(|| failure("terminal_agent_invalid"))?;
    plain_profile(crate::team::runtime::agent_teammates_profile(
        &root.join("team"),
        id,
    ))
}

/// Checks that an installed profile is this user's plain file, read without following a
/// link. Takes the path. Returns it, or `terminal_own_server_unavailable`.
fn plain_profile(profile: PathBuf) -> Result<PathBuf, HostError> {
    let plain = std::fs::symlink_metadata(&profile)
        .is_ok_and(|metadata| metadata.is_file() && metadata.uid() == user());
    if !plain {
        return Err(failure(OWN_SERVER_UNAVAILABLE));
    }
    Ok(profile)
}

/// A native lead's panes as team code reads them: the window of its anchor on its
/// teammates' server, where the anchor stands for the lead with its own pane's process and
/// state from its own server, so every pane id read for the team comes from one server and
/// no teammate's id can name the lead. Takes both servers and the lead. Returns the panes.
fn lead_panes(own: &Tmux, mates: &Tmux, lead: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
    let lead_pane = own.own_pane(lead)?;
    let anchor = mates.own_pane(lead)?;
    let panes: Vec<PaneInfo> = mates
        .panes(lead)?
        .into_iter()
        .map(|pane| {
            if pane.id == anchor.id {
                PaneInfo {
                    id: anchor.id.clone(),
                    pid: lead_pane.pid,
                    dead: lead_pane.dead,
                }
            } else {
                pane
            }
        })
        .collect();
    assert!(panes.len() <= PANES_LISTED_MAX);
    Ok(panes)
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
            mates: Mutex::new(BTreeMap::new()),
            members: Mutex::new(BTreeMap::new()),
            pins: Mutex::new(BTreeMap::new()),
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

    /// The client host for one native lead's teammates' server at the socket its latest
    /// start named, opened once and kept while it listens; the server starts on its first
    /// call. A kept host of an earlier start's socket ends that server first. Takes the lead
    /// and the socket. Returns the host, or a refusal when the lead lacks either installed
    /// profile or too many are kept.
    fn mates_host(&self, lead: &AgentId, socket: &Path) -> Result<Arc<Tmux>, HostError> {
        let mut mates = self
            .mates
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        match mates.remove(lead) {
            Some(found) if found.socket() == socket => {
                mates.insert(lead.clone(), found.clone());
                return Ok(found);
            }
            Some(earlier) => {
                let _ = earlier.close(&[]);
            }
            None => {}
        }
        mates.retain(|_, server| listening(&server.socket()));
        if mates.len() >= LEADS_KEPT_MAX {
            return Err(failure("terminal_unavailable"));
        }
        let server = Arc::new(Tmux::teammates(
            self.program.clone(),
            socket,
            &profile_of(&self.root, lead)?,
            &teammates_profile_of(&self.root, lead)?,
            self.pin(lead, socket)?,
        )?);
        mates.insert(lead.clone(), server.clone());
        assert!(mates.len() <= LEADS_KEPT_MAX);
        Ok(server)
    }

    /// The pin a native lead's teammates' server pid is kept in for the socket its latest
    /// start named (PLC c): one per socket while the engine runs, kept across the clients it
    /// opens and drops there, so a server under another pid is refused even after a client
    /// was dropped while none listened; a new socket gets a new pin. Each pin reads and keeps
    /// its pid in a record on disk, so a pin made again after a restart or a kill holds the
    /// same pid (PC-L1). Takes the lead and the socket. Returns the pin, or
    /// `terminal_unavailable` when too many are kept.
    fn pin(&self, lead: &AgentId, socket: &Path) -> Result<Arc<ServerPin>, HostError> {
        let mut pins = self
            .pins
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if let Some(pin) = pins.get(lead)
            && pin.socket == socket
        {
            return Ok(pin.clone());
        }
        pins.remove(lead);
        if pins.len() >= LEADS_KEPT_MAX {
            pins.retain(|known, _| {
                harness_socket(&self.root, known).is_ok_and(|socket| listening(&socket))
            });
            if pins.len() >= LEADS_KEPT_MAX {
                return Err(failure("terminal_unavailable"));
            }
        }
        let pin = Arc::new(ServerPin {
            root: self.root.clone(),
            lead: lead.clone(),
            socket: socket.to_path_buf(),
            pid: Mutex::new(None),
        });
        pins.insert(lead.clone(), pin.clone());
        assert!(pins.len() <= LEADS_KEPT_MAX);
        Ok(pin)
    }

    /// A native lead's teammates' server when one accepts connections at the socket its
    /// latest start named, else none; a raw connect, never a tmux client, looks, since an
    /// ended server's socket stays. Takes the agent. Returns the server host or the failure
    /// to open one.
    fn mates(&self, lead: &AgentId) -> Result<Option<Arc<Tmux>>, HostError> {
        let Ok(socket) = teammates_socket(&self.root, lead) else {
            return Ok(None);
        };
        if !listening(&socket) || crate::spawn_gate::connect_unix_std(&socket).is_err() {
            if let Ok(mut mates) = self.mates.lock() {
                mates.remove(lead);
            }
            return Ok(None);
        }
        self.mates_host(lead, &socket).map(Some)
    }

    /// The teammates' server a native lead's team panes are on (PLC d): the one its latest
    /// start named, which must still accept connections, so a pane id read there is never
    /// resolved on the lead's own server, which numbers its panes from the same `%0`. Takes
    /// the lead. Returns the server, none for a lead started without one, whose team panes
    /// are on its own server, or `terminal_teammates_server_ended`.
    fn team_mates(&self, lead: &AgentId) -> Result<Option<Arc<Tmux>>, HostError> {
        if teammates_socket(&self.root, lead).is_err() {
            return Ok(None);
        }
        self.mates(lead)?
            .map(Some)
            .ok_or_else(|| failure(TEAMMATES_SERVER_ENDED))
    }

    /// Forgets the client hosts of a lead's servers and the teammates adopted under it.
    /// Takes the lead.
    fn forget(&self, lead: &AgentId) {
        if let Ok(mut leads) = self.leads.lock() {
            leads.remove(lead);
        }
        if let Ok(mut mates) = self.mates.lock() {
            mates.remove(lead);
        }
        if let Ok(mut members) = self.members.lock() {
            members.retain(|_, known| known != lead);
        }
        if let Ok(mut pins) = self.pins.lock() {
            pins.remove(lead);
        }
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

    /// The server an adopted teammate runs on while it listens: its lead's teammates' server,
    /// or its lead's own for a lead started without one (CA-SEC-002: never its own once a
    /// teammates' server it had ended, since the lead can tag a pane there with the
    /// teammate's name). None answers as unknown; the teammate is kept while its lead's own
    /// server listens, as one failed connect proves no end, and forgotten once both are gone.
    /// Takes the teammate. Returns the server host or the failure to open one.
    fn member(&self, agent: &AgentId) -> Result<Option<Arc<Tmux>>, HostError> {
        let lead = self
            .members
            .lock()
            .ok()
            .and_then(|members| members.get(agent).cloned());
        let Some(lead) = lead else {
            return Ok(None);
        };
        let server = match self.team_mates(&lead) {
            Ok(Some(mates)) => Some(mates),
            Ok(None) => self.own(&lead)?,
            Err(problem) if problem.0 == TEAMMATES_SERVER_ENDED => {
                if self.own(&lead)?.is_some() {
                    return Ok(None);
                }
                None
            }
            Err(problem) => return Err(problem),
        };
        if server.is_none()
            && let Ok(mut members) = self.members.lock()
        {
            members.remove(agent);
        }
        Ok(server)
    }

    /// Remembers which lead an adopted teammate runs under, dropping teammates of leads whose
    /// servers are gone when the map is full. Takes the teammate and the lead. Returns Ok, or
    /// `terminal_unavailable` when it is still full.
    fn remember(&self, agent: &AgentId, lead: &AgentId) -> Result<(), HostError> {
        let mut members = self
            .members
            .lock()
            .map_err(|_| failure("terminal_unavailable"))?;
        if members.len() >= MEMBERS_KEPT_MAX && !members.contains_key(agent) {
            members.retain(|_, known| {
                harness_socket(&self.root, known).is_ok_and(|socket| listening(&socket))
            });
            if members.len() >= MEMBERS_KEPT_MAX {
                return Err(failure("terminal_unavailable"));
            }
        }
        members.insert(agent.clone(), lead.clone());
        assert!(members.len() <= MEMBERS_KEPT_MAX);
        Ok(())
    }

    /// Runs one call on the server that holds an agent's terminal: its own, its lead's when
    /// it is an adopted teammate, else the engine's. Takes the agent and the call. Returns
    /// the call's result.
    fn on<T>(
        &self,
        agent: &AgentId,
        call: impl FnOnce(&Tmux) -> Result<T, HostError>,
    ) -> Result<T, HostError> {
        if let Some(server) = self.own(agent)? {
            return call(&server);
        }
        match self.member(agent)? {
            Some(server) => call(&server),
            None => call(&self.engine),
        }
    }

    /// Runs one call on the server that holds the panes of an agent's window: for a lead
    /// whose own server listens, its [`Self::team_mates`] or its own, else as [`Self::on`].
    /// Takes the agent and the call. Returns the call's result.
    fn on_panes<T>(
        &self,
        agent: &AgentId,
        call: impl FnOnce(&Tmux) -> Result<T, HostError>,
    ) -> Result<T, HostError> {
        if let Some(own) = self.own(agent)? {
            return call(&self.team_mates(agent)?.unwrap_or(own));
        }
        self.on(agent, call)
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
        if server.alive()?.contains(agent) {
            return Err(failure("terminal_agent_running"));
        }
        let pane_env = self
            .mates_host(agent, &teammates_socket(&self.root, agent)?)?
            .anchor_for(agent, command)?;
        server.spawn_with(agent, command, &pane_env)
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
        self.on(agent, |server| server.resize(agent, cols, rows))?;
        if self.own(agent)?.is_some()
            && let Some(mates) = self.mates(agent)?
        {
            mates.resize(agent, cols, rows)?;
        }
        Ok(())
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let mates = self.mates(agent)?;
        if let Some(server) = self.own(agent)? {
            server.close(&[])?;
            if let Some(mates) = mates {
                mates.close(&[])?;
            }
            self.forget(agent);
            return Ok(());
        }
        if let Some(mates) = mates {
            mates.close(&[])?;
            self.forget(agent);
            return Ok(());
        }
        if let Some(server) = self.member(agent)? {
            return server.kill(agent);
        }
        match self.engine.kill(agent) {
            Err(problem) if problem.0 == "terminal_agent_unknown" => {}
            done => return done,
        }
        let mut ended = false;
        for lead in self.leads_listening() {
            let server = match self.team_mates(&lead) {
                Ok(Some(mates)) => Some(mates),
                Ok(None) => self.own(&lead).ok().flatten(),
                Err(_) => None,
            };
            if let Some(server) = server
                && server.holds(agent)
                && server.kill(agent).is_ok()
            {
                ended = true;
            }
        }
        if ended {
            Ok(())
        } else {
            Err(failure("terminal_agent_unknown"))
        }
    }

    fn adopt_pane(&self, agent: &AgentId, lead: &AgentId, pane: &str) -> Result<(), HostError> {
        let own = self
            .own(lead)?
            .ok_or_else(|| failure(OWN_SERVER_UNAVAILABLE))?;
        let server = self.team_mates(lead)?.unwrap_or(own);
        if self.own(agent)?.is_some() || self.engine.alive()?.contains(agent) {
            return Err(failure("terminal_agent_running"));
        }
        server.adopt_pane(agent, lead, pane)?;
        self.remember(agent, lead)
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        if let Some(own) = self.own(agent)? {
            return match self.team_mates(agent)? {
                Some(mates) => lead_panes(&own, &mates, agent),
                None => own.panes(agent),
            };
        }
        self.on(agent, |server| server.panes(agent))
    }

    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        self.on(agent, |server| server.pane_pid(agent))
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.on(agent, |server| Ok(server.unkept(agent)))
            .unwrap_or(true)
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        self.on_panes(agent, |server| server.kill_pane(agent, pane))
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.on_panes(agent, |server| server.attach_pane(agent, pane))
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.on_panes(agent, |server| server.snapshot_pane(agent, pane))
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.on_panes(agent, |server| server.input_pane(agent, pane, bytes))
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        self.on_panes(agent, |server| server.input_live_pane(agent, pane))
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
