use super::*;

impl TeamView {
    /// Opens the team of one project with nothing read yet.
    /// Takes the project.
    /// Returns the view; reads start with `next_query`.
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            status: None,
            focus: TeamFocus::Agents,
            agent_cursor: 0,
            task_cursor: 0,
            confirm: None,
            ask: None,
            draft: None,
            branch_edit: None,
            wake: None,
            coord: None,
            messages: None,
            message_row: None,
            master_pick: None,
            alerts: false,
            notice: None,
            now_ms: 0,
            offset: 0,
            read_ms: None,
            waiting: false,
            changing: None,
            reread: None,
            stored: None,
            setup_waiting: false,
            setup_failed_ms: None,
            saving: None,
            catalog: None,
            catalog_due: false,
            settings_scroll: 0,
            settings_scroll_max: std::cell::Cell::new(0),
            settings_window: std::cell::Cell::new(0),
        }
    }

    /// Gives the next read: the open Messages list's page first, then the stored setup while
    /// the master harness picker needs it, a failed one again after a refresh period, else the
    /// team, at most one team read at a time and one per refresh period.
    /// Takes the current time in milliseconds. Returns `SetupQuery::TeamMessages`,
    /// `SetupQuery::Setup` or `SetupQuery::Team`, or none while a read is out or nothing is due.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        if let Some(query) = self
            .messages
            .as_mut()
            .and_then(|list| list.next_query(now_ms))
        {
            return Some(query);
        }
        if self.master_pick.is_some()
            && self.stored.is_none()
            && !self.setup_waiting
            && self
                .setup_failed_ms
                .is_none_or(|failed| now_ms - failed >= REFRESH_MS)
        {
            self.setup_waiting = true;
            return Some(SetupQuery::Setup {
                project: self.project.clone(),
            });
        }
        if self.waiting || self.read_ms.is_some_and(|last| now_ms - last < REFRESH_MS) {
            return None;
        }
        self.waiting = true;
        if self.reread.is_some() {
            self.reread = Some(Reread::Sent);
        }
        Some(SetupQuery::Team {
            project: self.project.clone(),
        })
    }

    /// Forgets the read in flight after the engine connection dropped.
    /// Takes the view; the last team stays on screen.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        self.waiting = false;
        if let Some(list) = self.messages.as_mut() {
            list.forget_reads();
        }
        self.setup_waiting = false;
    }

    /// Says once that the open master harness picker needs the harness catalog, which the
    /// host reads with `SettingsJob::Harnesses`. Takes the view. Returns true at most once
    /// per open of the picker, and never once the catalog is read.
    pub fn wants_catalog(&mut self) -> bool {
        std::mem::take(&mut self.catalog_due)
    }

    /// Takes the engine's harness catalog for the master harness picker.
    /// Takes the read or its plain problem code. Returns nothing; a failed read is said
    /// while the picker is open and read again at its next open.
    pub fn take_catalog(&mut self, result: &Result<Vec<HarnessEntry>, String>) {
        match result {
            Ok(entries) => self.catalog = Some(harness_choices_from(entries)),
            Err(code) if self.master_pick.is_some() => {
                self.notice = Some(Notice::problem(team_plain(code)));
            }
            Err(_) => {}
        }
    }

    /// Applies one team read or team change result for this project.
    /// Takes the query that was sent and the engine's view or plain problem code.
    /// Returns nothing; other queries and other projects are left alone.
    pub fn apply(&mut self, query: &SetupQuery, result: Result<SetupView, String>) {
        if self
            .messages
            .as_mut()
            .is_some_and(|list| list.apply(query, &result))
        {
            return;
        }
        match query {
            SetupQuery::Team { project } if project == &self.project => {
                self.waiting = false;
                self.read_ms = (self.reread != Some(Reread::Due)).then_some(self.now_ms);
                match result {
                    Ok(SetupView::Team(status)) => {
                        assert_eq!(status.project_id, self.project);
                        self.status = Some(*status);
                        if self.reread == Some(Reread::Sent) {
                            self.reread = None;
                        }
                    }
                    Ok(_) => {}
                    Err(code) => self.notice = Some(Notice::problem(team_plain(&code))),
                }
            }
            SetupQuery::Setup { project } if project == &self.project => {
                self.setup_waiting = false;
                match result {
                    Ok(SetupView::Setup(Some(stored))) => {
                        assert_eq!(stored.project_id, self.project);
                        self.setup_failed_ms = None;
                        self.stored = Some(stored);
                    }
                    Ok(SetupView::Setup(None)) => {
                        self.master_pick = None;
                        self.notice = Some(Notice::problem(
                            "This project has no saved setup, so its master harness cannot change.",
                        ));
                    }
                    Ok(_) => {}
                    Err(code) => {
                        self.setup_failed_ms = Some(self.now_ms);
                        self.notice = Some(Notice::problem(team_plain(&code)));
                    }
                }
            }
            SetupQuery::Change { action, .. } if self.saving.as_ref() == Some(action) => {
                self.saving = None;
                let done = self.changing.take();
                self.stored = None;
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        self.master_pick = None;
                        self.notice = done.map(Notice::plain);
                        self.read_ms = None;
                    }
                    Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                        self.notice = Some(Notice::problem(team_plain(&code)));
                    }
                    Ok(_) => {}
                }
            }
            SetupQuery::TeamChange { action, .. }
                if action
                    .project()
                    .map_or(self.changing.is_some(), |project| project == &self.project) =>
            {
                let done = self.changing.take();
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        if matches!(action, TeamAction::Delegate { .. }) {
                            self.draft = None;
                        }
                        if matches!(
                            action,
                            TeamAction::MasterBranch { .. } | TeamAction::LandOn { .. }
                        ) {
                            self.branch_edit = None;
                        }
                        if matches!(action, TeamAction::CoordSettings { .. })
                            && let Some(edit) = &mut self.coord
                        {
                            edit.input = None;
                        }
                        self.notice = done.map(Notice::plain);
                        self.read_ms = None;
                        self.reread = Some(Reread::Due);
                    }
                    Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                        self.notice = Some(Notice::problem(team_plain(&code)));
                        self.read_ms = None;
                    }
                    Ok(_) => {}
                }
            }
            _ => {}
        }
        self.clamp();
    }

    /// Says whether a team change is waiting for the engine.
    /// Takes the view.
    /// Returns true until the change result arrives.
    pub fn busy(&self) -> bool {
        self.changing.is_some()
    }

    /// Refuses a change while the last one is with the engine or the team read sent after it
    /// has not landed, since a change built from an older read can undo the last save.
    /// Takes the view. Returns true, with the notice set, when the change must wait.
    pub(super) fn still_saving(&mut self) -> bool {
        let saving = self.changing.is_some() || self.reread.is_some();
        if saving {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
        }
        saving
    }

    /// Says whether the owner is writing a new task or a branch setting, so the host
    /// sends pastes here, keeps its own letter keys off and shows the cursor.
    /// Takes the view. Returns true while the task composer or the branch editor is open.
    pub fn typing(&self) -> bool {
        self.draft.is_some()
            || self.branch_edit.is_some()
            || self.coord.as_ref().is_some_and(|edit| edit.input.is_some())
    }

    /// Adds pasted text to the task or the branch setting being written.
    /// Takes untrusted pasted text; control characters are dropped.
    /// Returns true when the composer or the branch editor took it.
    pub fn paste(&mut self, text: &str) -> bool {
        if let Some(edit) = &mut self.branch_edit {
            edit.input.insert_str(text);
            return true;
        }
        if let Some(input) = self.coord.as_mut().and_then(|edit| edit.input.as_mut()) {
            input.insert_str(text);
            return true;
        }
        match &mut self.draft {
            Some(draft) => {
                draft.input.insert_str(text);
                true
            }
            None => false,
        }
    }

    /// Lists the agents in display order: live agents first with the master on
    /// top, then stopped agents, each oldest first. Finished agents wait in the team
    /// view's Finished box instead unless the owner chose Show (DEC-102).
    /// Takes the view. Returns the agents, empty before the first read.
    pub fn agents(&self) -> Vec<&TeamAgent> {
        self.agent_tree()
            .into_iter()
            .map(|(agent, _)| agent)
            .collect()
    }

    /// Lists the agents as `agents` does, each with its depth under its lead (N2), so a
    /// lead's workers show right below it. Takes the view. Returns the agents and depths.
    pub(super) fn agent_tree(&self) -> Vec<(&TeamAgent, usize)> {
        let mut agents: Vec<&TeamAgent> = self
            .status
            .iter()
            .flat_map(|status| {
                status
                    .agents
                    .iter()
                    .filter(|agent| status.show_finished || !agent.finished)
            })
            .collect();
        agents.sort_by_key(|agent| {
            (
                !agent.status.live(),
                agent.role != AgentRole::Master,
                agent.id,
            )
        });
        crate::team::lead_tree(&agents)
    }

    /// Finds one agent of the last read by id, a finished one the list leaves out too, for
    /// names. Takes the view and the agent id. Returns the agent, or none.
    pub(super) fn agent(&self, id: i64) -> Option<&TeamAgent> {
        self.status
            .iter()
            .flat_map(|status| status.agents.iter())
            .find(|agent| agent.id == id)
    }

    /// Counts the finished agents the list leaves for the Finished box: none under Show.
    /// Takes the view. Returns the count.
    pub(super) fn finished_hidden(&self) -> usize {
        self.status
            .iter()
            .filter(|status| !status.show_finished)
            .flat_map(|status| status.agents.iter())
            .filter(|agent| agent.finished)
            .count()
    }

    /// Names who handed a task out: the lead that started it, by its name, "A lead" for one
    /// the read does not list, else the master or the owner as the task's source says.
    /// Takes the view and the task. Returns the name.
    pub(super) fn task_from(&self, task: &TeamTask) -> String {
        match task.parent {
            Some(lead) => self
                .agent(lead)
                .map_or_else(|| "A lead".to_owned(), |lead| lead.label.clone()),
            None => source_word(task.source).to_owned(),
        }
    }

    /// Lists the tasks in display order: unfinished first, oldest first, then
    /// finished tasks, newest first.
    /// Takes the view. Returns the tasks, empty before the first read.
    pub fn tasks(&self) -> Vec<&TeamTask> {
        let mut tasks: Vec<&TeamTask> = self
            .status
            .iter()
            .flat_map(|status| status.tasks.iter())
            .collect();
        tasks.sort_by_key(|task| {
            let finished = task.status.finished();
            (finished, if finished { -task.id } else { task.id })
        });
        tasks
    }

    pub(super) fn task(&self, id: i64) -> Option<&TeamTask> {
        self.status
            .as_ref()?
            .tasks
            .iter()
            .find(|task| task.id == id)
    }

    pub(super) fn last_task(&self, agent: &TeamAgent) -> Option<&TeamTask> {
        self.status
            .as_ref()?
            .tasks
            .iter()
            .filter(|task| task.agent == Some(agent.id))
            .max_by_key(|task| task.id)
    }

    fn clamp(&mut self) {
        let agents = self.agents().len();
        let tasks = self.tasks().len();
        self.agent_cursor = self.agent_cursor.min(agents.saturating_sub(1));
        self.task_cursor = self.task_cursor.min(tasks.saturating_sub(1));
        if self
            .confirm
            .is_some_and(|id| self.task(id).is_none_or(|task| task.status.finished()))
        {
            self.confirm = None;
        }
    }

    /// Handles one key: move, switch list, cancel a task after asking, the
    /// keep-warm switch, or open the notifications and mark them seen.
    /// Takes the key event. Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> TeamReply {
        if let Some(list) = self.messages.as_mut() {
            if Key::from_event(key).is_some_and(|key| list.key(&key) == ListKey::Closed) {
                self.messages = None;
            }
            return TeamReply::Handled;
        }
        if self.draft.is_some() {
            return self.draft_key(key);
        }
        if self.branch_edit.is_some() {
            return self.branch_key(key);
        }
        if self.wake.is_some() {
            return self.wake_key(key);
        }
        if self.coord.is_some() {
            return self.coord_key(key);
        }
        if self.message_row.is_some() {
            return self.message_key(key);
        }
        if self.master_pick.is_some() {
            return self.pick_key(key);
        }
        if self.alerts {
            return match key.code {
                KeyCode::Char('s') => self.mark_seen(),
                KeyCode::Char('a') | KeyCode::Esc => {
                    self.alerts = false;
                    TeamReply::Handled
                }
                _ => TeamReply::Ignored,
            };
        }
        if let Some(ask) = self.ask {
            return self.ask_key(ask, key);
        }
        if let Some(task) = self.confirm {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm = None;
                    self.cancel(task)
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm = None;
                    TeamReply::Handled
                }
                _ => TeamReply::Handled,
            };
        }
        let reply = match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    TeamFocus::Agents => TeamFocus::Tasks,
                    TeamFocus::Tasks => TeamFocus::Agents,
                };
                TeamReply::Handled
            }
            KeyCode::Up => {
                self.step(false);
                TeamReply::Handled
            }
            KeyCode::Down => {
                self.step(true);
                TeamReply::Handled
            }
            KeyCode::Char('x') => {
                self.ask_cancel();
                TeamReply::Handled
            }
            KeyCode::Char('w') => self.keep_warm(),
            KeyCode::Char('t') => self.time_limit(Limit::Task),
            KeyCode::Char('l') => self.time_limit(Limit::Session),
            KeyCode::Char('i') => self.time_limit(Limit::Idle),
            KeyCode::Char('h') => self.harness_switch(),
            KeyCode::Char('H') => self.open_master_pick(),
            KeyCode::Char('A') => self.apply_switch(),
            KeyCode::Char('d') => self.sub_agent_depth(),
            KeyCode::Char('k') => self.child_landing(),
            KeyCode::Char('b') => self.agent_branches(),
            KeyCode::Char('S') => self.stop_all(),
            KeyCode::Char('z') => self.ask_stop_agent(),
            KeyCode::Char('p') => self.ask_teammate(),
            KeyCode::Char('v') => self.cap(Cap::Harnesses),
            KeyCode::Char('u') => self.cap(Cap::Teammates),
            KeyCode::Char('U') => self.master_rounds(),
            KeyCode::Char('N') => self.master_run_time(),
            KeyCode::Char('T') => self.token_budget(),
            KeyCode::Char('P') => self.spread_accounts(),
            KeyCode::Char('m') => {
                self.open_branch_edit(BranchSetting::Master);
                TeamReply::Handled
            }
            KeyCode::Char('L') => {
                self.open_branch_edit(BranchSetting::LandOn);
                TeamReply::Handled
            }
            KeyCode::Char('g') => self.ask_before_merge(),
            KeyCode::Char('E') => self.master_lands(),
            KeyCode::Char('R') => self.reask_empty_reply(),
            KeyCode::Char('D') => self.done_quiet(),
            KeyCode::Char('I') => self.master_idle_stop(),
            KeyCode::Char('V') => self.live_terminal(),
            KeyCode::Char('F') => self.new_folder_limit(),
            KeyCode::Char('W') => self.terminals_per_window(),
            KeyCode::Char('B') => self.background_at_end(),
            KeyCode::Char('O') => self.leftovers_setting(),
            KeyCode::Char('X') => self.settle_hold(true),
            KeyCode::Char('K') => self.settle_hold(false),
            KeyCode::Char('G') => self.grace_time(),
            KeyCode::Char('c') => {
                self.open_coord();
                TeamReply::Handled
            }
            KeyCode::Char('n') => {
                self.open_draft();
                TeamReply::Handled
            }
            KeyCode::Char('e') => {
                self.open_wake();
                TeamReply::Handled
            }
            KeyCode::Char('M') => {
                self.open_messages();
                TeamReply::Handled
            }
            KeyCode::Char('o') => {
                self.open_message_settings();
                TeamReply::Handled
            }
            KeyCode::Char('s') => self.mark_seen(),
            KeyCode::Char('a') => {
                self.alerts = true;
                TeamReply::Handled
            }
            KeyCode::PageDown => self.scroll_settings(true),
            KeyCode::PageUp => self.scroll_settings(false),
            _ => TeamReply::Ignored,
        };
        self.clamp();
        reply
    }

    /// Moves the settings rows after the first by the rows in view, down or up, after
    /// clamping the position to the range of the last frame drawn, so a position a resize
    /// left behind never eats a key press. Takes the direction. Returns Handled, or Ignored
    /// when nothing scrolls, so the screen can use the key.
    fn scroll_settings(&mut self, down: bool) -> TeamReply {
        let max = self.settings_scroll_max.get();
        self.settings_scroll = self.settings_scroll.min(max);
        if max == 0 {
            return TeamReply::Ignored;
        }
        let page = self.settings_window.get().max(1);
        self.settings_scroll = if down {
            (self.settings_scroll + page).min(max)
        } else {
            self.settings_scroll.saturating_sub(page)
        };
        assert!(self.settings_scroll <= max);
        TeamReply::Handled
    }

    /// Answers the stop that waits on a key: y sends Stop all or Stop harness, a number
    /// stops that running teammate, and n or Esc keeps everything running. Takes the stop
    /// and the key. Returns the change to send, or Handled.
    fn ask_key(&mut self, ask: Ask, key: KeyEvent) -> TeamReply {
        let reply = match (ask, key.code) {
            (_, KeyCode::Char('n') | KeyCode::Esc) => TeamReply::Handled,
            (Ask::StopAll, KeyCode::Char('y')) => self.change(
                "Stopping every agent of the project. Press S to start them again.",
                TeamAction::Pause {
                    project_id: self.project.clone(),
                },
            ),
            (Ask::StopAgent(agent), KeyCode::Char('y')) => self.change(
                "Stopping that agent and the agents it leads; a lead is told. Wake it from Finished to start it again.",
                TeamAction::StopAgent {
                    project_id: self.project.clone(),
                    agent,
                },
            ),
            (Ask::Teammate(agent), KeyCode::Char(digit @ '1'..='9')) => {
                let at = usize::from(digit as u8 - b'1');
                match self.running_teammates(agent).get(at) {
                    Some(pane) => {
                        let pane = pane.clone();
                        self.change(
                            "Stopping that teammate. Its harness is told.",
                            TeamAction::StopTeammate {
                                project_id: self.project.clone(),
                                agent,
                                pane,
                            },
                        )
                    }
                    None => return TeamReply::Handled,
                }
            }
            _ => return TeamReply::Handled,
        };
        self.ask = None;
        reply
    }

    /// Sends one team change with the line shown once it applied, unless the last change is
    /// still saving. Takes the line and the change. Returns the change, or Handled.
    pub(super) fn change(&mut self, done: &str, action: TeamAction) -> TeamReply {
        if self.still_saving() {
            return TeamReply::Handled;
        }
        self.changing = Some(done.to_owned());
        TeamReply::Change(action)
    }

    /// The pane ids of one harness's running teammates, oldest first, at most
    /// `TEAMMATE_KEYS_MAX`, numbered 1 up in the panel. Takes the agent. Returns the ids.
    pub(super) fn running_teammates(&self, agent: i64) -> Vec<String> {
        self.status
            .iter()
            .flat_map(|status| status.agents.iter())
            .filter(|item| item.id == agent)
            .flat_map(|item| item.teammates.iter())
            .filter(|teammate| !teammate.ended && !teammate.stopped)
            .map(|teammate| teammate.pane.clone())
            .take(TEAMMATE_KEYS_MAX)
            .collect()
    }

    /// Stop all: asks before stopping every agent of the project, and starts them again
    /// at once when the team is stopped. Returns the change, or Handled with a question or
    /// a notice.
    fn stop_all(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if status.paused {
            return self.change(
                "Starting the team again.",
                TeamAction::Resume {
                    project_id: self.project.clone(),
                },
            );
        }
        self.ask = Some(Ask::StopAll);
        TeamReply::Handled
    }

    /// Stop harness or Stop teammate: asks before stopping the selected harness with every
    /// agent it leads, or the selected sub-agent of either mode, app-spawned or a native
    /// teammate, whose lead is then told. The master stops only with Stop all. Returns
    /// Handled with the question or a notice.
    fn ask_stop_agent(&mut self) -> TeamReply {
        let Some(agent) = self.agents().get(self.agent_cursor).copied() else {
            self.notice = Some(Notice::plain("Pick an agent in the agents list first."));
            return TeamReply::Handled;
        };
        let note = match (agent.role, agent.status.live(), agent.owner_stopped) {
            (AgentRole::Master, ..) => Some("The master stops with Stop all (S)."),
            (_, false, _) | (_, _, true) => {
                Some("That agent is already stopped. Wake it from Finished to start it again.")
            }
            _ => None,
        };
        match note {
            Some(note) => self.notice = Some(Notice::plain(note)),
            None => self.ask = Some(Ask::StopAgent(agent.id)),
        }
        TeamReply::Handled
    }

    /// Stop teammate: asks which running teammate of the selected harness to stop.
    /// Returns Handled with the question or a notice.
    fn ask_teammate(&mut self) -> TeamReply {
        let Some(agent) = self.agents().get(self.agent_cursor).map(|agent| agent.id) else {
            self.notice = Some(Notice::plain("Pick a harness in the agents list first."));
            return TeamReply::Handled;
        };
        if self.running_teammates(agent).is_empty() {
            self.notice = Some(Notice::plain("That harness has no running teammates."));
        } else {
            self.ask = Some(Ask::Teammate(agent));
        }
        TeamReply::Handled
    }

    /// Moves Max harnesses per project or Max teammates per harness to its next choice:
    /// No limit, then each of `CAP_CHOICES`, then No limit again. Takes which cap. Returns
    /// the change, or Handled while the team loads or saves.
    fn cap(&mut self, which: Cap) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let current = match which {
            Cap::Harnesses => status.max_harnesses,
            Cap::Teammates => status.max_teammates,
        };
        let max = next_choice(&crate::team::CAP_CHOICES, current);
        let project_id = self.project.clone();
        let (done, action) = match which {
            Cap::Harnesses => (
                format!("Max harnesses is now {}.", cap_words(max)),
                TeamAction::MaxHarnesses { project_id, max },
            ),
            Cap::Teammates => (
                format!("Max teammates per harness is now {}.", cap_words(max)),
                TeamAction::MaxTeammates { project_id, max },
            ),
        };
        self.change(&done, action)
    }

    /// Moves Sub-agent rounds per message to its next choice: Until the goal is done, then each
    /// of `ROUND_CHOICES`, then Until the goal is done again (F09). Returns the change, or
    /// Handled while the team loads or saves.
    fn master_rounds(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let max = next_choice(
            &crate::team::master_limits::ROUND_CHOICES,
            status.master_rounds,
        );
        let project_id = self.project.clone();
        self.change(
            &format!(
                "Sub-agent rounds per message is now {}.",
                rounds_choice(max)
            ),
            TeamAction::MasterRounds { project_id, max },
        )
    }

    /// Moves Master run time to its next choice: each of `RUN_CHOICES_SECS`, then No limit,
    /// then the first again (F01). Returns the change, or Handled while the team loads or
    /// saves.
    fn master_run_time(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let secs = next_choice(
            &crate::team::master_limits::RUN_CHOICES_SECS,
            status.master_run_secs,
        );
        let project_id = self.project.clone();
        self.change(
            &format!(
                "Master run time is now {}.",
                crate::team::master_limits::run_words(secs)
            ),
            TeamAction::MasterRunTime { project_id, secs },
        )
    }

    /// Turns Spread harnesses across accounts on or off: on, a new or woken agent prefers a
    /// pool account no running agent of the project uses. Returns the change, or Handled
    /// while the team loads.
    fn spread_accounts(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let on = !status.spread_accounts;
        self.change(
            if on {
                "New agents now prefer a pool account no running agent of this project uses."
            } else {
                "New agents now take the account your account strategy picks."
            },
            TeamAction::SpreadAccounts {
                project_id: self.project.clone(),
                on,
            },
        )
    }

    /// Moves the Token budget to its next choice: Off, then each of `BUDGET_CHOICES`, then
    /// Off again. Returns the change, or Handled while the team loads or saves.
    fn token_budget(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let tokens = next_choice(&crate::team::BUDGET_CHOICES, status.token_budget);
        let done = match tokens {
            Some(tokens) => format!(
                "The team now stops after {} from now.",
                crate::team::token_words(tokens)
            ),
            None => "The token budget is off.".to_owned(),
        };
        self.change(
            &done,
            TeamAction::TokenBudget {
                project_id: self.project.clone(),
                tokens,
            },
        )
    }

    fn step(&mut self, down: bool) {
        let cursor = match self.focus {
            TeamFocus::Agents => &mut self.agent_cursor,
            TeamFocus::Tasks => &mut self.task_cursor,
        };
        *cursor = if down {
            cursor.saturating_add(1)
        } else {
            cursor.saturating_sub(1)
        };
    }

    fn ask_cancel(&mut self) {
        let target = match self.focus {
            TeamFocus::Tasks => self.tasks().get(self.task_cursor).map(|task| task.id),
            TeamFocus::Agents => self
                .agents()
                .get(self.agent_cursor)
                .and_then(|agent| agent.task),
        };
        let open = target
            .and_then(|id| self.task(id))
            .filter(|task| !task.status.finished())
            .map(|task| task.id);
        match open {
            Some(id) => self.confirm = Some(id),
            None => self.notice = Some(Notice::plain("There is no running or waiting task here.")),
        }
    }

    fn cancel(&mut self, id: i64) -> TeamReply {
        if self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(task) = self.task(id) else {
            return TeamReply::Handled;
        };
        let done = if task.status == TaskStatus::Running {
            format!("Stopping \"{}\".", title(&task.prompt))
        } else {
            format!("Cancelled \"{}\".", title(&task.prompt))
        };
        self.changing = Some(done);
        TeamReply::Change(TeamAction::Cancel {
            project_id: self.project.clone(),
            task: Some(id),
        })
    }

    fn sub_harnesses(&self) -> Vec<String> {
        self.status
            .iter()
            .flat_map(|status| status.harnesses.iter())
            .filter(|harness| harness.role == AgentRole::Sub && harness.runnable)
            .map(|harness| harness.harness.clone())
            .collect()
    }

    fn open_draft(&mut self) {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        }
        if self.sub_harnesses().is_empty() {
            self.notice = Some(Notice::plain(
                "This team has no sub-agent that can run yet.",
            ));
            return;
        }
        self.notice = None;
        self.draft = Some(Draft {
            input: TextInput::multi(TASK_PROMPT_BYTES_MAX),
            harness: None,
        });
    }

    fn draft_key(&mut self, key: KeyEvent) -> TeamReply {
        match key.code {
            KeyCode::Esc => {
                self.draft = None;
                TeamReply::Handled
            }
            KeyCode::Enter => self.send_draft(),
            KeyCode::Tab | KeyCode::BackTab => {
                self.pick_harness(key.code == KeyCode::Tab);
                TeamReply::Handled
            }
            _ => {
                if let Some(draft) = &mut self.draft {
                    draft.input.handle(key);
                }
                TeamReply::Handled
            }
        }
    }

    fn pick_harness(&mut self, forward: bool) {
        let choices: Vec<Option<String>> = std::iter::once(None)
            .chain(self.sub_harnesses().into_iter().map(Some))
            .collect();
        let Some(draft) = &mut self.draft else {
            return;
        };
        let at = choices
            .iter()
            .position(|choice| choice == &draft.harness)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % choices.len()
        } else {
            (at + choices.len() - 1) % choices.len()
        };
        draft.harness = choices[next].clone();
    }

    fn send_draft(&mut self) -> TeamReply {
        let Some(draft) = &self.draft else {
            return TeamReply::Ignored;
        };
        let prompt = draft.input.value().trim().to_owned();
        let harness = draft.harness.clone();
        if prompt.is_empty() {
            self.notice = Some(Notice::plain("Write what the sub-agent should do first."));
            return TeamReply::Handled;
        }
        if self.still_saving() {
            return TeamReply::Handled;
        }
        self.notice = None;
        self.changing = Some(format!(
            "Handed \"{}\" to {}.",
            title(&prompt),
            harness
                .as_deref()
                .map_or_else(|| "a sub-agent".to_owned(), |name| harness_name(name, None))
        ));
        TeamReply::Change(TeamAction::Delegate {
            project_id: self.project.clone(),
            harness,
            prompt,
        })
    }
}
