use super::*;

impl Team {
    /// The Team screen of one project, waiting for its first graph read.
    /// Inputs: the project id and name. Output: the screen with the tree focused.
    pub fn new(project: ProjectId, name: String) -> Self {
        Self {
            project,
            name,
            master: false,
            asked: false,
            retried: false,
            checked: false,
            switch: None,
            master_agent: None,
            master_live: None,
            want: None,
            refused: None,
            waiting_since: None,
            paused: false,
            graph: Load::Loading,
            cursor: 0,
            offset: 0,
            focus: Focus::Tree,
            attached: None,
            task: None,
            cleanup: cleanup::Cleanup::new(),
            finished: finished::Finished::new(),
            questions: questions::Questions::new(),
            instructions: None,
            messages: None,
            settings: None,
            held: Vec::new(),
        }
    }

    /// Opens the team settings of this project over the team tree: keep warm, limits,
    /// wake rules, branches, merging, stops, caps and the token budget.
    /// Input: the context. Output: none; the first team read is queued.
    pub(super) fn open_settings(&mut self, ctx: &mut Ctx) {
        let mut view = TeamView::new(self.project.clone());
        view.offset = local_offset(now_ms());
        self.settings = Some(view);
        self.pump_settings(ctx);
    }

    /// Sends the open team settings' next read once one is due, and the harness catalog
    /// read once their master harness picker asks for it; a read the full queue refuses is
    /// forgotten so the next key asks again.
    /// Input: the context. Output: none; a Setup or a Settings request may be queued.
    pub(super) fn pump_settings(&mut self, ctx: &mut Ctx) {
        let Some(view) = self.settings.as_mut() else {
            return;
        };
        if view.wants_catalog() {
            ctx.request(Request::Settings(Box::new(SettingsJob::Harnesses)));
        }
        if let Some(query) = view.next_query(now_ms())
            && !ctx.try_request(Request::Setup(query))
        {
            view.forget_reads();
        }
    }

    /// Takes a key while the team settings are open: the settings take it first, a change
    /// goes to the engine as `SetupQuery::TeamChange`, a setup save as `SetupQuery::Change`,
    /// and Esc the settings left alone closes them. Inputs: the key and the context.
    /// Output: handled.
    pub(super) fn settings_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(view) = self.settings.as_mut() else {
            return Flow::Ignored;
        };
        let reply = match key {
            Key::Paste(text) if view.paste(text) => TeamReply::Handled,
            Key::Paste(_) => TeamReply::Ignored,
            other => key_event(other).map_or(TeamReply::Ignored, |event| view.key(event)),
        };
        match reply {
            TeamReply::Change(action) => {
                let generation = ctx.next_generation();
                ctx.request(Request::Setup(SetupQuery::TeamChange {
                    action,
                    generation,
                }));
            }
            TeamReply::Save(action) => {
                let generation = ctx.next_generation();
                ctx.request(Request::Setup(SetupQuery::Change { action, generation }));
            }
            TeamReply::Ignored if *key == Key::Esc => self.settings = None,
            TeamReply::Ignored | TeamReply::Handled => {}
        }
        self.pump_settings(ctx);
        Flow::Handled
    }

    /// The master view of one project: a header with the live master's state, then the
    /// live master's own terminal filling the body, opened as soon as the team shows it.
    /// Every open turns the master on; it stops only on the master idle stop or app quit.
    /// The terminal has the keys from the start; what is typed before it opens waits.
    /// Inputs: the project id and name. Output: the screen, waiting for the master.
    pub fn master(project: ProjectId, name: String) -> Self {
        Self {
            master: true,
            focus: Focus::Terminal,
            ..Self::new(project, name)
        }
    }

    /// Whether the master view expects a terminal it does not have open: none listed yet,
    /// one not attached yet, or one that closed for any reason but a lost link, which
    /// reattaches by itself; the team not stopped, no refused start and the switch not off.
    /// Input: none. Output: true while the next terminal is awaited.
    fn expecting(&self) -> bool {
        self.master
            && self.attached.as_ref().is_none_or(|attached| {
                (!attached.started && attached.closed.is_none())
                    || attached.closed.as_deref().is_some_and(|code| !lost(code))
            })
            && !self.paused
            && self.refused.is_none()
            && self.live_on() != Some(false)
    }

    /// Whether the master view waits for a terminal that is coming: expected and none closed,
    /// so typed keys can wait for it. Input: none. Output: true from the start until the
    /// terminal opens or the start ends.
    pub(super) fn waiting(&self) -> bool {
        self.expecting()
            && self
                .attached
                .as_ref()
                .is_none_or(|attached| attached.closed.is_none())
    }

    /// Whether the terminal has been expected for `OPEN_LIMIT_MS`, so the view stops
    /// waiting in silence, keeps the keys and offers s to try again (owner 11-12).
    /// Input: none. Output: the answer at this moment.
    pub(super) fn overdue(&self) -> bool {
        self.expecting()
            && self
                .waiting_since
                .is_some_and(|since| now_ms().saturating_sub(since) >= OPEN_LIMIT_MS)
    }

    /// Whether the master view still waits for its terminal within the limit.
    /// Input: none. Output: true while typed keys wait for the terminal.
    pub(super) fn starting(&self) -> bool {
        self.waiting() && !self.overdue()
    }

    /// Starts the clock when the view begins to expect a terminal, at open or once one
    /// closed with the switch on, and clears it once a terminal opened or the start ended,
    /// so each wait gets its own minute, as the desktop's area does (DEC-141).
    /// Input: none. Output: none.
    pub(super) fn time_wait(&mut self) {
        if !self.expecting() {
            self.waiting_since = None;
        } else if self.waiting_since.is_none() {
            self.waiting_since = Some(now_ms());
        }
    }

    /// The engine's line for a live master it stopped, shown as is, and what s does then:
    /// after an idle stop it starts the master again, after any other stop it tries again.
    /// None while the master runs, under Stop all, or for a stop with no line.
    /// Input: none. Output: the line and the words for s.
    fn stop_line(&self) -> Option<(&str, &'static str)> {
        if !self.master || self.paused || self.live_on() != Some(false) {
            return None;
        }
        let switch = self.switch.as_ref()?;
        let line = switch.problem.as_deref()?;
        let rest = if switch.idle {
            MASTER_START_AGAIN
        } else {
            MASTER_TRY_AGAIN
        };
        Some((line, rest))
    }

    /// Whether s tries the live master's start again: its terminal is overdue, or the engine
    /// stopped it with a line for any reason but idle, as the desktop's Try again (DEC-141).
    /// Input: none. Output: the answer.
    pub(super) fn try_again(&self) -> bool {
        self.overdue() || (self.stop_line().is_some() && !self.idle_stopped())
    }

    /// Whether keys go to a terminal: the terminal has focus and, in the master view, one is
    /// attached or coming, overdue included, so a typed stream never turns into view keys
    /// at the limit; a stopped, refused or switched-off master with none keeps the view keys.
    /// Input: none. Output: true while every key but Ctrl-] goes to the terminal.
    pub(super) fn terminal_keys(&self) -> bool {
        self.focus == Focus::Terminal && (!self.master || self.attached.is_some() || self.waiting())
    }

    /// Keeps one key typed while the master's terminal starts, up to `HELD_KEYS_MAX` keys
    /// (not bytes) in all; a paste is not kept, since its bracketing needs the open
    /// terminal's mode. Inputs: the key and the context. Output: none; a dropped key says why.
    fn hold(&mut self, key: &Key, ctx: &mut Ctx) {
        assert!(self.waiting());
        if matches!(key, Key::Paste(_)) {
            ctx.say(Message::warning(MASTER_PASTE_WAIT, MASTER_HELD_FULL));
            return;
        }
        if self.held.len() >= HELD_KEYS_MAX {
            ctx.say(Message::warning(MASTER_HELD_FULL, MASTER_HELD_REST));
            return;
        }
        self.held.push(key.clone());
    }

    /// Drops the keys kept for a master terminal that will not get them: one that closed or
    /// was rejected before it opened, one dropped or replaced by another, or a start ended
    /// by a refusal, Stop all or the switch turned off. Says so when any were kept.
    /// Input: the context. Output: none.
    pub(super) fn drop_held(&mut self, ctx: &mut Ctx) {
        if self.held.is_empty() {
            return;
        }
        self.held.clear();
        ctx.say(Message::warning(MASTER_HELD_DROPPED, MASTER_HELD_AGAIN));
    }

    /// Whether the live master is on as the owner sees it: the owner's last change until the
    /// engine shows it, else the engine's switch; None until read or when never set.
    fn live_on(&self) -> Option<bool> {
        self.want
            .or_else(|| self.switch.as_ref().and_then(|switch| switch.on))
    }

    /// Whether an idle stop ended the live master and no Stop all holds the team, the stop
    /// whose s starts it again on the same conversation. Input: none. Output: the answer.
    pub(super) fn idle_stopped(&self) -> bool {
        self.master
            && !self.paused
            && self.live_on() == Some(false)
            && self.switch.as_ref().is_some_and(|switch| switch.idle)
    }

    fn start_live(&mut self, ctx: &mut Ctx) {
        self.asked = true;
        self.want = Some(true);
        self.refused = None;
        self.waiting_since = Some(now_ms());
        if let Some(switch) = self.switch.as_mut() {
            switch.problem = None;
        }
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::LiveMaster {
                project_id: self.project.clone(),
                on: true,
                version: self.switch.as_ref().map_or(0, |switch| switch.version),
            },
            generation,
        }));
    }

    /// Sends the owner's default Open check for a switch already on, once per open of a
    /// running team's master view and after each accepted Start all, so an existing
    /// project's live master with no session gets one and restarts on it (LMS-1).
    /// Input: the context. Output: none; the check is queued.
    fn check_session(&mut self, ctx: &mut Ctx) {
        self.checked = true;
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::OpenLiveSession {
            project: self.project.clone(),
            generation,
        }));
    }

    pub(super) fn read_switch(&self, ctx: &mut Ctx) {
        ctx.request(Request::Setup(SetupQuery::Team {
            project: self.project.clone(),
        }));
    }

    /// Takes a team status read, a live switch change or a session check of this master
    /// view: a good read replaces the switch, and the first one of this open turns a switch
    /// that is not on on, or checks the session of a running team's switch already on, and a
    /// switch seen on stays stopped after its next stop until the owner acts; a
    /// failed read changes nothing but ends a start waiting to be retried; an accepted change
    /// is the switch until the next read, which is read at once, and an accepted Start all
    /// checks the session too; a change refused as stale reads the switch again, and the first
    /// such start of this open stays wanted, keeping the owner's typed keys, until that read
    /// starts it once more; any other refusal keeps the switch; an Open that opened no session
    /// is named; a read whose saved master is another harness than the last read's counts as
    /// a new open, as the desktop's area does (DEC-161).
    /// Inputs: the setup query, its result and the context. Output: none.
    pub(super) fn took_setup(
        &mut self,
        query: &SetupQuery,
        result: &Result<SetupView, String>,
        ctx: &mut Ctx,
    ) {
        if !self.master {
            return;
        }
        match query {
            SetupQuery::Team { project } if *project == self.project => {
                let Ok(SetupView::Team(status)) = result else {
                    if !self.asked && self.want == Some(true) {
                        self.want = None;
                    }
                    return;
                };
                self.paused = status.paused;
                let switch = LiveSwitch {
                    on: status.live_master,
                    problem: status.live_problem.clone(),
                    idle: status.live_idle_stopped,
                    switched: status.live_switch.as_ref().map(switched_words),
                    saved: status
                        .harnesses
                        .iter()
                        .find(|harness| harness.role == AgentRole::Master)
                        .map(|harness| harness.harness.clone()),
                    running: status
                        .agents
                        .iter()
                        .find(|agent| agent.role == AgentRole::Master && agent.status.live())
                        .map(|agent| agent.harness.clone()),
                    version: status.settings_version,
                    limits: Some(crate::team::master_limits::LimitsView {
                        rounds: crate::team::master_limits::Rounds {
                            used: status.rounds_used,
                            max: status.master_rounds,
                            kept_going: status.rounds_kept_going,
                        },
                        run_secs: status.master_run_secs,
                        deadline_ms: status.master_deadline_ms,
                    }),
                };
                if self.want.is_some() && self.want == switch.on {
                    self.want = None;
                }
                let off = switch.on != Some(true);
                let moved = self
                    .switch
                    .as_ref()
                    .and_then(|last| last.saved.as_deref())
                    .zip(switch.saved.as_deref())
                    .is_some_and(|(last, saved)| !crate::harnesses::same(last, saved));
                if moved {
                    self.asked = false;
                    self.checked = false;
                    self.retried = false;
                }
                self.switch = Some(switch);
                if off && !self.asked && !self.paused {
                    self.start_live(ctx);
                } else if !off && !self.paused && !self.asked && !self.checked {
                    self.check_session(ctx);
                }
                self.asked |= !off && !self.paused;
            }
            SetupQuery::TeamChange {
                action:
                    TeamAction::LiveMaster {
                        project_id,
                        on,
                        version,
                    },
                ..
            } if *project_id == self.project => match result {
                Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code)
                    if code == crate::storage::versioned::STALE_CODE =>
                {
                    if self.retried {
                        self.want = None;
                    } else {
                        self.retried = true;
                        self.asked = false;
                    }
                    ctx.say(Message::warning(
                        "The live master switch changed in another window.",
                        plain_problem(code).unwrap_or(code.as_str()),
                    ));
                    self.read_switch(ctx);
                }
                Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                    self.want = None;
                    self.refused = Some(code.clone());
                }
                Ok(view) => {
                    self.want = None;
                    let last = self.switch.take();
                    let (switched, saved, running, limits) = last
                        .map_or((None, None, None, None), |switch| {
                            (switch.switched, switch.saved, switch.running, switch.limits)
                        });
                    self.switch = Some(LiveSwitch {
                        on: Some(*on),
                        problem: None,
                        idle: false,
                        switched,
                        saved,
                        running,
                        version: version + 1,
                        limits,
                    });
                    if let SetupView::LiveWithoutSession(why) = view {
                        ctx.say(Message::error(MASTER_NO_SESSION, &unopened(why)));
                    }
                    self.read_switch(ctx);
                }
            },
            SetupQuery::TeamChange {
                action: TeamAction::Resume { project_id },
                ..
            } if *project_id == self.project => match result {
                Ok(_) => {
                    self.read_switch(ctx);
                    self.check_session(ctx);
                }
                Err(code) => ctx.say(Message::error(&refusal(code), "")),
            },
            SetupQuery::OpenLiveSession { project, .. } if *project == self.project => {
                if let Ok(SetupView::LiveWithoutSession(why)) | Err(why) = result {
                    ctx.say(Message::error(MASTER_NO_SESSION, &unopened(why)));
                }
            }
            _ => {}
        }
    }

    /// Starts every agent of a stopped team again from the master view, the same change as
    /// Start all on the team page; the live master then starts with the team.
    /// Input: the context. Output: none; the change and a fresh team read are queued.
    fn start_team(&mut self, ctx: &mut Ctx) {
        assert!(self.master && self.paused);
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: TeamAction::Resume {
                project_id: self.project.clone(),
            },
            generation,
        }));
    }

    /// Reads the team again when the graph shows the master stopped after showing it running,
    /// so the open master view says why it stopped. Input: the context. Output: none; a team
    /// read may be queued.
    pub(super) fn note_master_stopped(&mut self, ctx: &mut Ctx) {
        let live = self.graph.ready().and_then(|graph| {
            graph
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Master)
                .map(|node| node.status.is_none_or(|status| status.live()))
        });
        let last = std::mem::replace(&mut self.master_live, live);
        if self.master && last == Some(true) && live == Some(false) {
            self.read_switch(ctx);
        }
    }

    /// Reads the team again when the graph shows another master agent than it showed last,
    /// so the master view's harness follows a live master that started again (OP-28).
    /// Input: the context. Output: none; a team read may be queued.
    pub(super) fn note_master_agent(&mut self, ctx: &mut Ctx) {
        if !self.master {
            return;
        }
        let Some(agent) = self.graph.ready().map(|graph| {
            graph
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::Master)
                .and_then(|node| node.agent)
        }) else {
            return;
        };
        if self
            .master_agent
            .replace(agent)
            .is_some_and(|last| last != agent)
        {
            self.read_switch(ctx);
        }
    }

    /// Opens the master's terminal once the team lists it, and the new one
    /// after the master moved to another account.
    /// Input: the context. Output: none; attach and size requests are queued.
    pub(super) fn follow_master(&mut self, ctx: &mut Ctx) {
        if !self.master || self.live_on() == Some(false) {
            return;
        }
        let Some((node, terminal)) = self.graph.ready().and_then(|graph| {
            graph.nodes.iter().enumerate().find_map(|(index, node)| {
                let live = node.status.is_none_or(|status| status.live());
                (node.kind == NodeKind::Master && live)
                    .then(|| node.terminal.clone().map(|terminal| (index, terminal)))
                    .flatten()
            })
        }) else {
            return;
        };
        let current = self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.agent == terminal && attached.closed.is_none());
        if !current {
            self.open(node, ctx);
        }
    }

    pub(super) fn master_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let open = self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.closed.is_none());
        match key {
            Key::Enter | Key::Tab if open => self.focus = Focus::Terminal,
            Key::Char('S') if self.paused => self.start_team(ctx),
            Key::Char('s') if self.idle_stopped() || self.try_again() => self.start_live(ctx),
            Key::Char('r') => ctx.request(Request::TeamGraph {
                project: self.project.clone(),
            }),
            Key::Char(code @ ('n' | 'l')) => {
                ctx.sessions_key = Some((self.project.clone(), *code));
                self.detach(ctx);
                return Flow::Back;
            }
            Key::Esc => {
                self.detach(ctx);
                return Flow::Back;
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    pub(super) fn items(&self, glyphs: &Glyphs) -> Vec<Item> {
        self.graph
            .ready()
            .map(|graph| tree(graph, glyphs, usize::MAX))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.item)
            .collect()
    }

    pub(super) fn take_graph(&mut self, result: &Result<Box<TeamGraph>, String>, glyphs: &Glyphs) {
        match result {
            Ok(graph) => self.graph = Load::Ready(graph.clone()),
            Err(code) if code == NOT_READY => self.graph = Load::NotReady,
            Err(_) if self.graph.ready().is_some() => {}
            Err(code) => self.graph = Load::Failed(code.clone()),
        }
        let count = self.items(glyphs).len();
        self.cursor = self.cursor.min(count.saturating_sub(1));
        let known = self.task.is_some_and(|task| {
            self.graph
                .ready()
                .is_some_and(|graph| graph.edges.iter().any(|edge| edge.task_id == task))
        });
        if !known {
            self.task = None;
        }
    }

    /// Takes one frame of the attached terminal; at its `Attached` frame the keys held while
    /// the master's terminal started are typed in, in the order typed.
    /// Inputs: the agent, the frame and the context. Output: none.
    pub(super) fn stream(&mut self, agent: &AgentId, frame: &TerminalFrame, ctx: &mut Ctx) {
        let Some(attached) = self
            .attached
            .as_mut()
            .filter(|attached| attached.agent == *agent)
        else {
            return;
        };
        match frame {
            TerminalFrame::Attached { cols, rows, .. } => {
                attached.pane.resize(
                    (*rows).clamp(SIZE_MIN, SIZE_MAX),
                    (*cols).clamp(SIZE_MIN, SIZE_MAX),
                );
                attached.pane.show(&[], None);
                attached.started = true;
                let bytes: Vec<u8> = std::mem::take(&mut self.held)
                    .iter()
                    .flat_map(|key| attached.pane.encode(key))
                    .collect();
                for chunk in bytes.chunks(INPUT_BYTES_MAX) {
                    ctx.request(Request::TerminalInput {
                        agent: attached.agent.clone(),
                        bytes: chunk.to_vec(),
                    });
                }
            }
            TerminalFrame::Output { bytes } => attached.pane.feed(bytes),
            TerminalFrame::Closed { reason } => attached.closed = Some(reason.clone()),
            TerminalFrame::Rejected { code } => attached.closed = Some(code.clone()),
        }
    }

    pub(super) fn open(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        if target.status.is_some_and(|status| !status.live()) {
            self.task = None;
            self.instructions = None;
            ctx.say(Message::warning(
                "This agent has stopped.",
                "Its terminal is closed.",
            ));
            return;
        }
        let Some(agent) = target.terminal.clone() else {
            ctx.say(Message::warning(
                "This agent has no terminal yet.",
                "It gets one when it starts.",
            ));
            return;
        };
        let teammate = target
            .pane
            .clone()
            .filter(|_| target.kind == NodeKind::Teammate && target.agent.is_none());
        if teammate
            .as_deref()
            .is_some_and(|pane| !crate::terminal::pane_id_valid(pane))
        {
            ctx.say(Message::warning(
                "This teammate's pane can't be opened.",
                "Press r to read the team again.",
            ));
            return;
        }
        let (id, label) = (target.id.clone(), target.label.clone());
        self.task = None;
        self.instructions = None;
        if self.attached.as_ref().is_some_and(|attached| {
            attached.agent == agent && attached.teammate == teammate && attached.closed.is_none()
        }) {
            self.focus = Focus::Terminal;
            return;
        }
        self.detach(ctx);
        let (rows, cols) = pane_size(ctx, self.master);
        ctx.request(attach_request(&agent, teammate.as_deref()));
        ctx.request(Request::TerminalResize {
            agent: agent.clone(),
            cols,
            rows,
        });
        self.attached = Some(Attached {
            agent,
            teammate,
            node: id,
            label,
            pane: TerminalPane::new(rows, cols),
            size: (rows, cols),
            started: false,
            closed: None,
        });
        self.focus = Focus::Terminal;
    }

    /// Whether a team read says this project's "Live terminal" setting is Off, which closes
    /// the master view, since that master's conversation is the console (DEC-117 Q4).
    /// Input: the setup event. Output: true for a good team read of this project showing Off.
    pub(super) fn live_terminal_off(&self, event: &crate::tui::client::SetupEvent) -> bool {
        self.master
            && matches!(&event.query, SetupQuery::Team { project } if *project == self.project)
            && matches!(&event.result, Ok(SetupView::Team(status)) if !status.master_live_terminal)
    }

    /// Whether a team read names a saved master with no live terminal, such as rcli, which
    /// takes the master view back to the console before it asks for any start, as the
    /// desktop's area shows that master's conversation (DEC-141). Input: the setup event.
    /// Output: true for a good team read of this project naming such a master.
    pub(super) fn no_live_terminal(&self, event: &crate::tui::client::SetupEvent) -> bool {
        use super::super::project_team::master_runs_live;
        self.master
            && matches!(&event.query, SetupQuery::Team { project } if *project == self.project)
            && matches!(&event.result, Ok(SetupView::Team(status)) if !master_runs_live(status))
    }

    pub(super) fn detach(&mut self, ctx: &mut Ctx) {
        if self.attached.is_some() {
            self.drop_held(ctx);
        }
        if let Some(attached) = self.attached.take()
            && attached.closed.is_none()
        {
            ctx.request(Request::TerminalDetach {
                agent: attached.agent,
            });
        }
        self.focus = Focus::Tree;
    }

    /// Opens the attached terminal again after the engine connection came
    /// back; a terminal that ended stays closed.
    /// Input: the context. Output: none; attach and size requests are queued.
    pub(super) fn reattach(&mut self, ctx: &mut Ctx) {
        let (rows, cols) = pane_size(ctx, self.master);
        let Some(attached) = self
            .attached
            .as_mut()
            .filter(|attached| attached.closed.as_deref().is_some_and(lost))
        else {
            return;
        };
        attached.closed = None;
        attached.size = (rows, cols);
        ctx.request(attach_request(
            &attached.agent,
            attached.teammate.as_deref(),
        ));
        ctx.request(Request::TerminalResize {
            agent: attached.agent.clone(),
            cols,
            rows,
        });
    }

    /// Closes the attached terminal once the team shows its agent stopped,
    /// so no key reaches a pane whose agent is gone.
    /// Input: the context. Output: none; a detach is queued for a live view.
    pub(super) fn end_stopped(&mut self, ctx: &mut Ctx) {
        let graph = &self.graph;
        let Some(attached) = self.attached.as_mut() else {
            return;
        };
        let stopped = graph
            .ready()
            .and_then(|graph| graph.nodes.iter().find(|node| node.id == attached.node))
            .and_then(|node| node.status)
            .is_some_and(|status| !status.live());
        if !stopped || attached.closed.as_deref().is_some_and(dead) {
            return;
        }
        if attached.closed.is_none() {
            ctx.request(Request::TerminalDetach {
                agent: attached.agent.clone(),
            });
        }
        attached.closed = Some(STOPPED.into());
    }

    /// Opens the terminal of the agent a Questions row names, as Enter on its node does,
    /// and moves the tree cursor to that node. Inputs: the terminal and the context.
    /// Output: none; an agent the tree does not show yet gets a plain message.
    pub(super) fn open_agent(&mut self, agent: &AgentId, ctx: &mut Ctx) {
        let node = self.graph.ready().and_then(|graph| {
            graph
                .nodes
                .iter()
                .position(|node| node.terminal.as_ref() == Some(agent))
        });
        let Some(node) = node else {
            ctx.say(Message::warning(
                "That agent is not in the team tree yet.",
                "Press r to read the team again.",
            ));
            return;
        };
        if let Some(at) = self
            .items(ctx.glyphs)
            .iter()
            .position(|item| *item == Item::Node(node))
        {
            self.cursor = at;
        }
        self.open(node, ctx);
    }

    pub(super) fn show_task(&mut self, edge: usize) {
        self.task = self
            .graph
            .ready()
            .and_then(|graph| graph.edges.get(edge))
            .map(|edge| edge.task_id);
        self.instructions = None;
        self.focus = Focus::Tree;
    }

    /// Asks the engine what one card's agent received at its last launch and opens the
    /// box for it: the master card reads the project's newest master.
    /// Inputs: the node index and context. Output: none; a Process request is queued.
    pub(super) fn read_instructions(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        let agent = match target.kind {
            NodeKind::Master => None,
            _ => match AgentId::new(&target.id).ok().and_then(|id| id.team_agent()) {
                Some(agent) => Some(agent),
                None => {
                    ctx.say(Message::warning(
                        "This card has no instructions to show.",
                        "Only the master and team agents get instructions at launch.",
                    ));
                    return;
                }
            },
        };
        let label = target.label.clone();
        let query = ProcessQuery::Agent {
            project: self.project.clone(),
            agent,
        };
        ctx.request(Request::Process(Box::new(query.clone())));
        self.task = None;
        self.instructions = Some(Instructions {
            label,
            query,
            read: Load::Loading,
            top: 0,
        });
    }

    /// Opens the Messages list of one card's sub-agent in the right box, the same list the
    /// Team view opens; the master card says where the master's own conversation is.
    /// Inputs: the node index and context. Output: none; the first page read is queued.
    pub(super) fn open_messages(&mut self, node: usize, ctx: &mut Ctx) {
        let Some(target) = self.graph.ready().and_then(|graph| graph.nodes.get(node)) else {
            return;
        };
        if target.kind == NodeKind::Master {
            ctx.say(Message::warning(
                "The master has no Messages list.",
                MASTER_ELSEWHERE,
            ));
            return;
        }
        let Some(agent) = AgentId::new(&target.id).ok().and_then(|id| id.team_agent()) else {
            ctx.say(Message::warning(
                "This card has no messages to show.",
                "Only team sub-agents have a Messages list.",
            ));
            return;
        };
        let now = crate::tui::clock::now_ms();
        self.task = None;
        self.instructions = None;
        self.messages = Some(Messages::new(
            self.project.clone(),
            agent,
            &target.label,
            local_offset(now),
        ));
        self.ask_messages(ctx);
    }

    /// Sends the open Messages list's next page read, if one is due.
    pub(super) fn ask_messages(&mut self, ctx: &mut Ctx) {
        let now = crate::tui::clock::now_ms();
        if let Some(query) = self.messages.as_mut().and_then(|list| list.next_query(now)) {
            ctx.request(Request::Setup(query));
        }
    }

    pub(super) fn take_instructions(&mut self, event: &ProcessEvent) {
        let Some(open) = self
            .instructions
            .as_mut()
            .filter(|open| open.query == event.query)
        else {
            return;
        };
        open.read = match &event.result {
            Ok(ProcessReply::Agent(view)) => Load::Ready(view.clone()),
            Ok(
                ProcessReply::Text(_)
                | ProcessReply::Current(_)
                | ProcessReply::Saved(_)
                | ProcessReply::Settings(_)
                | ProcessReply::Adoption(_)
                | ProcessReply::Files(_)
                | ProcessReply::Uncommitted(_)
                | ProcessReply::Committed(_)
                | ProcessReply::Proposal(_)
                | ProcessReply::AppSettings(_)
                | ProcessReply::Sources(_),
            ) => return,
            Err(code) if code == NOT_READY => Load::NotReady,
            Err(code) => Load::Failed(code.clone()),
        };
        open.top = 0;
    }

    pub(super) fn sync(&mut self, ctx: &mut Ctx) {
        let (rows, cols) = pane_size(ctx, self.master);
        let Some(attached) = self.attached.as_mut() else {
            return;
        };
        if attached.size == (rows, cols) {
            return;
        }
        attached.size = (rows, cols);
        if attached.closed.is_none() {
            ctx.request(Request::TerminalResize {
                agent: attached.agent.clone(),
                cols,
                rows,
            });
        }
    }

    /// Takes a key while the terminal has the keys: Ctrl-] gives them back to the team or
    /// master view, any other key goes to the open terminal. While the master's terminal
    /// starts, keys wait for it and Esc goes back; past the limit Esc still goes back, s tries
    /// again and every other key is dropped, so a typed stream never acts on the view; with
    /// no terminal coming the master view has the keys. A terminal that closed, or
    /// that the engine stopped before it opened, keeps the keys and drops them until Ctrl-],
    /// so keys typed then never act on the view, but for s after a stop with a line, which
    /// starts the master again.
    /// Inputs: the key and the context. Output: the flow.
    pub(super) fn terminal_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if *key == LEADER {
            self.focus = Focus::Tree;
            return Flow::Handled;
        }
        if self.master
            && ctx.shift_arrows == ShiftArrows::SwitchTabs
            && matches!(key, Key::ShiftLeft | Key::ShiftRight)
        {
            return self.step_tab(*key == Key::ShiftRight, ctx);
        }
        if self.starting() {
            if *key == Key::Esc {
                self.held.clear();
                self.detach(ctx);
                return Flow::Back;
            }
            self.hold(key, ctx);
            return Flow::Handled;
        }
        if self.overdue() {
            match key {
                Key::Esc => {
                    self.held.clear();
                    self.detach(ctx);
                    return Flow::Back;
                }
                Key::Char('s') => self.start_live(ctx),
                _ => {}
            }
            return Flow::Handled;
        }
        let stopped = self.stop_line().is_some();
        let again = self.idle_stopped() || self.try_again();
        let Some(attached) = self.attached.as_mut() else {
            self.focus = Focus::Tree;
            if self.master {
                return self.master_key(key, ctx);
            }
            return Flow::Handled;
        };
        if attached.closed.is_some() || (stopped && !attached.started) {
            if *key == Key::Char('s') && again {
                self.start_live(ctx);
            }
            return Flow::Handled;
        }
        let bytes = attached.pane.encode(key);
        if !bytes.is_empty() {
            ctx.request(Request::TerminalInput {
                agent: attached.agent.clone(),
                bytes,
            });
        }
        Flow::Handled
    }

    /// Leaves the master view for the project's previous or next tab, as the desktop tab
    /// strip beside its live terminal does; keys kept while the terminal started go with
    /// the view. Inputs: the direction and the context. Output: back to the project page.
    pub(super) fn step_tab(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        assert!(self.master);
        self.held.clear();
        self.detach(ctx);
        ctx.project_step = Some((self.project.clone(), forward));
        Flow::Back
    }

    pub(super) fn tree_lines(
        &mut self,
        limit: usize,
        height: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let notice = |lead: Line<'static>, rest: &str| {
            let mut lines = vec![join([Line::raw("  "), lead])];
            lines.extend(
                wrap(rest, limit.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|text| join([Line::raw("  "), styled(text, Role::Muted, theme)])),
            );
            lines
        };
        let graph = match &self.graph {
            Load::Idle | Load::Loading => {
                return vec![join([Line::raw("  "), ctx.loading("Loading the team")])];
            }
            Load::NotReady => {
                return notice(
                    Line::raw("Not ready yet."),
                    "This engine cannot show the team yet.",
                );
            }
            Load::Failed(code) => {
                return notice(
                    styled(
                        format!("{} Failed to load the team.", glyphs.status(Status::Fail)),
                        Role::Error,
                        theme,
                    ),
                    plain_problem(code).unwrap_or("Press r to try again."),
                );
            }
            Load::Ready(graph) => graph,
        };
        if graph.nodes.is_empty() {
            return notice(
                Line::raw("No agents yet."),
                "Agents show here once the master starts work.",
            );
        }
        let rows = tree(graph, glyphs, limit.saturating_sub(2));
        let selected = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.item.is_some())
            .nth(self.cursor)
            .map_or(0, |(index, _)| index);
        if selected < self.offset {
            self.offset = selected;
        }
        if selected >= self.offset + height {
            self.offset = selected + 1 - height;
        }
        self.offset = self.offset.min(rows.len().saturating_sub(height));
        let focused = self.focus == Focus::Tree;
        rows.iter()
            .enumerate()
            .skip(self.offset)
            .take(height)
            .map(|(index, row)| {
                let room = limit.saturating_sub(2).saturating_sub(width(&row.prefix));
                list_row(
                    join([Line::raw(row.prefix.clone()), body(row, graph, ctx, room)]),
                    limit,
                    row.item.is_some() && index == selected,
                    focused,
                    theme,
                    glyphs,
                )
            })
            .collect()
    }

    fn task_lines(&self, limit: usize, ctx: &Ctx) -> Option<Vec<Line<'static>>> {
        let theme = &ctx.theme;
        let glyphs = ctx.glyphs;
        let task = self.task?;
        let graph = self.graph.ready()?;
        let edge = graph.edges.iter().find(|edge| edge.task_id == task)?;
        let (kind, word) = edge_status(edge.state);
        let mut lines = vec![
            Line::raw(format!(
                "{} {} {}",
                label(graph, &edge.from),
                glyphs.right,
                ran_by(graph, edge)
            )),
            styled(
                format!("{} {word}", glyphs.status(kind)),
                kind.role(),
                theme,
            ),
            styled(
                match (&edge.lead, edge.source) {
                    (Some(lead), _) => format!("Handed out by {}.", label(graph, lead)),
                    (None, TaskSource::Master) => "Handed out by the master.".to_owned(),
                    (None, TaskSource::Owner) => "Handed off by you.".to_owned(),
                },
                Role::Muted,
                theme,
            ),
            Line::default(),
        ];
        lines.extend(wrap(&plain(&edge.text), limit).into_iter().map(Line::raw));
        lines.push(Line::default());
        lines.push(styled("Reply", Role::Muted, theme));
        match &edge.reply {
            Some(reply) => lines.extend(wrap(&plain(reply), limit).into_iter().map(Line::raw)),
            None => lines.push(styled("No reply yet.", Role::Muted, theme)),
        }
        Some(lines)
    }

    /// The master view's header: whether the live master is on, the harness it runs and a
    /// saved one that waits for its next start, where the live session's master harness
    /// last switched, last its time left and the rounds of the current goal while a live
    /// master runs (F01, F09), and the session keys at the right edge. The state is kept whole: when
    /// it does not fit beside both keys, only n New session shows and l Sessions stays in
    /// the hints. Inputs: the width and context. Output: the line.
    pub(super) fn master_head(&self, limit: usize, ctx: &Ctx) -> Line<'static> {
        let switch = self.switch.as_ref();
        let switched = switch.and_then(|switch| switch.switched.as_deref());
        let off = self.live_on() == Some(false);
        let lead = match (self.paused, off, switched) {
            (true, _, _) => MASTER_TEAM_STOPPED,
            (false, true, _) => MASTER_OFF,
            (false, false, _) => MASTER_ON_SWITCHED,
        };
        let rest = match (self.paused, off, switched) {
            (true, _, _) => None,
            (false, _, Some(switched)) => Some(switched),
            (false, true, None) => None,
            (false, false, None) => Some(MASTER_ON_WHEN),
        };
        let harness = switch
            .and_then(|switch| harness_words(switch.saved.as_deref(), switch.running.as_deref()));
        let limits = switch
            .filter(|switch| switch.running.is_some())
            .and_then(|switch| switch.limits)
            .filter(|_| !self.paused && !off)
            .map(|view| view.words(crate::team::now_ms()));
        let state = [Some(lead), harness.as_deref(), rest, limits.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let control = MASTER_CONTROLS
            .into_iter()
            .find(|control| width(&state) + 2 + width(control) <= limit)
            .unwrap_or(MASTER_CONTROLS[1])
            .to_owned();
        let room = limit.saturating_sub(width(&control));
        if room < width(MASTER_OFF) + 2 {
            return styled(
                cut(&control, limit, ctx.glyphs.ellipsis),
                Role::Muted,
                &ctx.theme,
            );
        }
        let state = cut(&state, room - 2, ctx.glyphs.ellipsis);
        join([
            Line::raw(format!("{state:<room$}")),
            styled(control, Role::Muted, &ctx.theme),
        ])
    }

    pub(super) fn draw_right(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &Ctx,
    ) -> Option<Position> {
        let theme = &ctx.theme;
        let inner = inside(area);
        let limit = usize::from(inner.width).max(1);
        if let Some(list) = &self.messages {
            let lines = list
                .lines(limit, usize::from(inner.height))
                .into_iter()
                .map(|line| match line.role {
                    Some(role) => styled(line.text, role, theme),
                    None => Line::raw(line.text),
                })
                .collect();
            boxed(frame, area, &list.title(), false, lines, ctx);
            return None;
        }
        if let Some((title, lines)) = self.cleanup.lines(limit, usize::from(inner.height), ctx) {
            boxed(frame, area, &title, false, lines, ctx);
            return None;
        }
        if let Some((title, lines)) = self.finished.lines(limit, usize::from(inner.height), ctx) {
            boxed(frame, area, &title, false, lines, ctx);
            return None;
        }
        if let Some((title, lines, cursor)) =
            self.questions.lines(limit, usize::from(inner.height), ctx)
        {
            let typing = cursor.is_some();
            boxed(frame, area, &title, typing, lines, ctx);
            return cursor.map(|(row, column)| Position {
                x: inner.x.saturating_add(
                    u16::try_from(column)
                        .unwrap_or(u16::MAX)
                        .min(inner.width.saturating_sub(1)),
                ),
                y: inner.y.saturating_add(
                    u16::try_from(row)
                        .unwrap_or(u16::MAX)
                        .min(inner.height.saturating_sub(1)),
                ),
            });
        }
        if let Some(open) = self.instructions.as_mut() {
            let lines = instruction_lines(&open.read, limit, ctx);
            open.top = open
                .top
                .min(lines.len().saturating_sub(usize::from(inner.height)));
            let title = format!("Instructions: {}", plain(&open.label));
            let shown = lines.into_iter().skip(open.top).collect();
            boxed(frame, area, &title, false, shown, ctx);
            return None;
        }
        if let Some(lines) = self.task_lines(limit, ctx) {
            boxed(frame, area, "Task", false, lines, ctx);
            return None;
        }
        let focused = self.focus == Focus::Terminal;
        if self.master && self.paused && self.attached.is_none() {
            let mut lines: Vec<Line<'static>> = wrap(MASTER_PAUSED, limit)
                .into_iter()
                .map(Line::raw)
                .collect();
            lines.extend(
                wrap(MASTER_PAUSED_START, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        if let Some(code) = self.refused.as_deref().filter(|_| self.attached.is_none()) {
            let mut lines = vec![Line::raw("The master terminal could not start.")];
            lines.extend(
                wrap(&refusal(code), limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        if self.master && self.live_on() == Some(false) && self.attached.is_none() {
            let (lead, rest) = match self.stop_line() {
                Some(stop) => stop,
                None if self.idle_stopped() => (MASTER_STOPPED, MASTER_START_AGAIN),
                None => (MASTER_STOPPED, MASTER_RESTART),
            };
            let mut lines: Vec<Line<'static>> =
                wrap(lead, limit).into_iter().map(Line::raw).collect();
            lines.extend(
                wrap(rest, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        if self.overdue() {
            let line = self
                .switch
                .as_ref()
                .and_then(|switch| switch.problem.as_deref())
                .map_or_else(
                    || MASTER_OVERDUE.to_owned(),
                    |line| format!("{MASTER_OVERDUE} {line}"),
                );
            let mut lines: Vec<Line<'static>> =
                wrap(&line, limit).into_iter().map(Line::raw).collect();
            lines.extend(
                wrap(MASTER_TRY_AGAIN, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, "Master", false, lines, ctx);
            return None;
        }
        let stop = self.stop_line().map(|(line, rest)| (line.to_owned(), rest));
        let Some(attached) = self.attached.as_mut() else {
            let waiting = if self.master {
                self.switch
                    .as_ref()
                    .and_then(|switch| switch.problem.as_deref())
                    .unwrap_or(MASTER_WAITING)
            } else {
                "Enter on an agent opens its live terminal here."
            };
            let lines = wrap(waiting, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme))
                .collect();
            let title = if self.master { "Master" } else { "Terminal" };
            boxed(frame, area, title, false, lines, ctx);
            return None;
        };
        if let Some((lead, rest)) = stop.as_ref().filter(|_| !attached.started) {
            let mut lines: Vec<Line<'static>> =
                wrap(lead, limit).into_iter().map(Line::raw).collect();
            lines.extend(
                wrap(rest, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            boxed(frame, area, &attached.label, false, lines, ctx);
            return None;
        }
        if let Some(waiting) = self
            .switch
            .as_ref()
            .and_then(|switch| switch.problem.as_deref())
            .filter(|_| self.master && !self.paused && !attached.started)
        {
            let lines = wrap(waiting, limit)
                .into_iter()
                .map(|text| styled(text, Role::Muted, theme))
                .collect();
            boxed(frame, area, &attached.label, false, lines, ctx);
            return None;
        }
        if let Some(code) = &attached.closed {
            let (lead, rest) = stop
                .as_ref()
                .map_or(closed_words(code), |(line, rest)| (line.as_str(), *rest));
            let mut lines: Vec<Line<'static>> =
                wrap(lead, limit).into_iter().map(Line::raw).collect();
            lines.extend(
                wrap(rest, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Muted, theme)),
            );
            if !attached.started {
                boxed(frame, area, &attached.label, false, lines, ctx);
                return None;
            }
            boxed(frame, area, &attached.label, false, Vec::new(), ctx);
            attached
                .pane
                .draw(frame.buffer_mut(), inner, ctx.depth, false);
            let rows = inner
                .height
                .min(u16::try_from(lines.len()).unwrap_or(u16::MAX));
            let banner = Rect::new(inner.x, inner.y + inner.height - rows, inner.width, rows);
            let lines: Vec<Line<'static>> = lines
                .into_iter()
                .take(usize::from(rows))
                .map(|line| pad(line, limit, ctx.glyphs))
                .collect();
            draw_lines(frame.buffer_mut(), banner, &lines);
            return None;
        }
        boxed(frame, area, &attached.label, focused, Vec::new(), ctx);
        attached
            .pane
            .draw(frame.buffer_mut(), inner, ctx.depth, focused)
    }
}
