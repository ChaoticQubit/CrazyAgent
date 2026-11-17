use super::*;

impl TeamView {
    pub(super) fn open_wake(&mut self) {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        }
        let agent = match self.focus {
            TeamFocus::Agents => self
                .agents()
                .get(self.agent_cursor)
                .filter(|agent| agent.role == AgentRole::Sub)
                .map(|agent| agent.id),
            TeamFocus::Tasks => None,
        };
        self.notice = None;
        self.wake = Some(WakeEdit {
            level: agent.map_or(WakeLevel::Project, WakeLevel::Agent),
            agent,
            row: 0,
        });
    }

    /// Opens the Messages list of the focused sub-agent or teammate. On the master it says
    /// where the master's own conversation is; on the tasks list it asks for an agent.
    /// Takes the view. Returns nothing; the list's first read comes from `next_query`.
    pub(super) fn open_messages(&mut self) {
        let picked = match self.focus {
            TeamFocus::Agents => self
                .agents()
                .get(self.agent_cursor)
                .map(|agent| (agent.id, agent.role, agent.label.clone())),
            TeamFocus::Tasks => None,
        };
        match picked {
            None => {
                self.notice = Some(Notice::plain(
                    "Pick an agent in the agents list to read its messages.",
                ));
            }
            Some((_, AgentRole::Master, _)) => self.notice = Some(Notice::plain(MASTER_ELSEWHERE)),
            Some((agent, AgentRole::Sub | AgentRole::Teammate, label)) => {
                self.notice = None;
                self.messages = Some(Messages::new(
                    self.project.clone(),
                    agent,
                    &label,
                    self.offset,
                ));
            }
        }
    }

    pub(super) fn open_message_settings(&mut self) {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        }
        self.notice = None;
        self.message_row = Some(0);
    }

    /// Handles a key in the Messages settings editor: the arrows up and down pick a setting,
    /// Space, Enter or the right arrow moves it to its next choice, the left arrow to the one
    /// before, Esc closes. Takes the key. Returns the change, or Handled.
    pub(super) fn message_key(&mut self, key: KeyEvent) -> TeamReply {
        let (Some(row), Some(status)) = (self.message_row, &self.status) else {
            self.message_row = None;
            return TeamReply::Handled;
        };
        let shown = status.message_settings;
        let forward = match key.code {
            KeyCode::Esc => {
                self.message_row = None;
                return TeamReply::Handled;
            }
            KeyCode::Up | KeyCode::Down => {
                self.message_row = Some(if key.code == KeyCode::Down {
                    (row + 1).min(MESSAGE_ROWS - 1)
                } else {
                    row.saturating_sub(1)
                });
                return TeamReply::Handled;
            }
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right => true,
            KeyCode::Left => false,
            _ => return TeamReply::Handled,
        };
        if self.still_saving() {
            return TeamReply::Handled;
        }
        let settings = next_message_setting(shown, row, forward);
        let (label, value) = message_setting(&settings, row);
        self.changing = Some(format!("{label}: {value}."));
        TeamReply::Change(TeamAction::MessageSettings {
            project_id: self.project.clone(),
            settings,
        })
    }

    pub(super) fn message_setting_rows(&self, row: usize, content: usize, look: &Look) -> Vec<Row> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let mut rows = vec![spread(
            content,
            Row::of("Messages settings", look.heading),
            Row::of("For every sub-agent's Messages list here", look.muted),
        )];
        for at in 0..MESSAGE_ROWS {
            let (label, value) = message_setting(&status.message_settings, at);
            rows.push(list_row(
                Row::of(label, look.fg)
                    .pad(MESSAGE_LABEL)
                    .push(value, look.heading),
                content,
                at == row,
                true,
                look,
            ));
        }
        rows
    }

    /// Gives the rule a level sets itself, if any, and the rule shown for it: its own, else
    /// the one it falls back to. Takes the level. Returns none before the first read.
    pub fn wake_rule(&self, level: WakeLevel) -> Option<(Option<WakeRule>, WakeRule)> {
        let wake = &self.status.as_ref()?.events.wake;
        let (own, fallback) = match level {
            WakeLevel::App => (wake.app, WakeRule::default()),
            WakeLevel::Project => (wake.project, wake.app.unwrap_or_default()),
            WakeLevel::Agent(id) => (
                wake.agents
                    .iter()
                    .find(|rule| rule.agent == id)
                    .map(|rule| rule.rule),
                wake.effective,
            ),
        };
        Some((own, own.unwrap_or(fallback)))
    }

    /// Gives the version of a level the last read showed: its own rule's, else for an agent
    /// level the version its cleared rule left, else 0 for a level never set. Takes the level.
    /// Returns the version a change of that level sends.
    fn wake_version(&self, level: WakeLevel) -> u64 {
        let Some(status) = self.status.as_ref() else {
            return 0;
        };
        let wake = &status.events.wake;
        match level {
            WakeLevel::App => wake.app_version,
            WakeLevel::Project => wake.project_version,
            WakeLevel::Agent(id) => wake
                .agents
                .iter()
                .find(|rule| rule.agent == id)
                .map(|rule| rule.version)
                .or_else(|| {
                    wake.cleared
                        .iter()
                        .find(|level| level.agent == id)
                        .map(|level| level.version)
                })
                .unwrap_or(0),
        }
    }

    pub(super) fn wake_key(&mut self, key: KeyEvent) -> TeamReply {
        let Some(edit) = self.wake else {
            return TeamReply::Ignored;
        };
        let Some((own, shown)) = self.wake_rule(edit.level) else {
            self.wake = None;
            return TeamReply::Handled;
        };
        let mut next = shown;
        match key.code {
            KeyCode::Esc => {
                self.wake = None;
                return TeamReply::Handled;
            }
            KeyCode::Up | KeyCode::Down => {
                let row = if key.code == KeyCode::Down {
                    (edit.row + 1).min(WAKE_ROWS - 1)
                } else {
                    edit.row.saturating_sub(1)
                };
                self.wake = Some(WakeEdit { row, ..edit });
                return TeamReply::Handled;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let levels: Vec<WakeLevel> = [WakeLevel::App, WakeLevel::Project]
                    .into_iter()
                    .chain(edit.agent.map(WakeLevel::Agent))
                    .collect();
                let at = levels
                    .iter()
                    .position(|level| *level == edit.level)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Tab {
                    (at + 1) % levels.len()
                } else {
                    (at + levels.len() - 1) % levels.len()
                };
                self.wake = Some(WakeEdit {
                    level: levels[next],
                    ..edit
                });
                return TeamReply::Handled;
            }
            KeyCode::Char(' ') | KeyCode::Enter if edit.row < EventKind::ALL.len() => {
                let kind = EventKind::ALL[edit.row];
                let on = !shown.wakes(kind);
                match kind {
                    EventKind::Question => next.question = on,
                    EventKind::Finished => next.finished = on,
                    EventKind::Failed => next.failed = on,
                    EventKind::ChangeSetReady => next.change_set_ready = on,
                    EventKind::Progress => next.progress = on,
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::PageDown | KeyCode::PageUp
                if edit.row == EventKind::ALL.len() =>
            {
                next.hourly_cap = match key.code {
                    KeyCode::Left => shown.hourly_cap.saturating_sub(1),
                    KeyCode::Right => shown.hourly_cap.saturating_add(1),
                    KeyCode::PageDown => shown.hourly_cap.saturating_sub(10),
                    _ => shown.hourly_cap.saturating_add(10),
                }
                .min(HOURLY_CAP_MAX);
                if next.hourly_cap == shown.hourly_cap {
                    return TeamReply::Handled;
                }
            }
            KeyCode::Char('r') => {
                if own.is_none() {
                    self.notice = Some(Notice::plain("This level already uses the rule above it."));
                    return TeamReply::Handled;
                }
                return self.wake_change(edit.level, None);
            }
            _ => return TeamReply::Handled,
        }
        self.wake_change(edit.level, Some(next))
    }

    fn wake_change(&mut self, level: WakeLevel, rule: Option<WakeRule>) -> TeamReply {
        if self.still_saving() {
            return TeamReply::Handled;
        }
        let target = match level {
            WakeLevel::App => RuleTarget::App,
            WakeLevel::Project => RuleTarget::Project {
                project_id: self.project.clone(),
            },
            WakeLevel::Agent(agent) => RuleTarget::Agent {
                project_id: self.project.clone(),
                agent,
            },
        };
        let version = self.wake_version(level);
        self.changing = Some(match rule {
            Some(_) => "Wake rule saved.".to_owned(),
            None => "Wake rule reset. The rule above it applies.".to_owned(),
        });
        TeamReply::Change(TeamAction::Wake {
            target,
            rule,
            version,
        })
    }

    pub(super) fn mark_seen(&mut self) -> TeamReply {
        let Some(upto) = self.status.as_ref().and_then(|status| {
            status
                .events
                .notifications
                .iter()
                .filter(|item| item.seen_ms.is_none())
                .map(|item| item.id)
                .max()
        }) else {
            self.notice = Some(Notice::plain("There are no new notifications."));
            return TeamReply::Handled;
        };
        if self.still_saving() {
            return TeamReply::Handled;
        }
        self.changing = Some("Notifications marked as seen.".to_owned());
        TeamReply::Change(TeamAction::Seen {
            project_id: self.project.clone(),
            upto,
        })
    }

    pub(super) fn alert_rows(&self, content: usize, room: usize, look: &Look) -> Vec<Row> {
        let mut notifications: Vec<_> = self
            .status
            .iter()
            .flat_map(|status| status.events.notifications.iter())
            .collect();
        notifications.sort_by_key(|item| std::cmp::Reverse(item.id));
        let unseen = notifications
            .iter()
            .filter(|item| item.seen_ms.is_none())
            .count();
        let mut rows = vec![
            Row::default(),
            Row::of("Notifications", look.heading).push(format!("  {unseen} new"), look.muted),
        ];
        if notifications.is_empty() {
            rows.push(Row::of(
                "  No notifications yet. Sub-agents that need you show here.",
                look.muted,
            ));
            return rows;
        }
        let fits = room.saturating_sub(rows.len());
        let shown = if notifications.len() > fits {
            fits.saturating_sub(1)
        } else {
            notifications.len()
        };
        for item in &notifications[..shown] {
            let (word, style) = match item.seen_ms {
                None => ("New", look.warning),
                Some(_) => ("Seen", look.muted),
            };
            rows.push(spread(
                content,
                Row::of(format!("  {word}"), style)
                    .pad(8)
                    .push(title(&item.text), look.fg),
                Row::of(
                    format!("  {} ago", duration(self.now_ms - item.created_ms)),
                    look.muted,
                ),
            ));
        }
        let hidden = notifications.len() - shown;
        if hidden > 0 {
            rows.push(Row::of(format!("  {hidden} older not shown"), look.muted));
        }
        rows
    }

    pub(super) fn wake_rows(&self, edit: WakeEdit, content: usize, look: &Look) -> Vec<Row> {
        let Some((own, shown)) = self.wake_rule(edit.level) else {
            return Vec::new();
        };
        let whose = match edit.level {
            WakeLevel::App => "every project".to_owned(),
            WakeLevel::Project => "this project".to_owned(),
            WakeLevel::Agent(id) => self
                .agent(id)
                .map_or_else(|| "this sub-agent".to_owned(), |agent| agent.label.clone()),
        };
        let source = match (own.is_some(), edit.level) {
            (true, _) => "Set here",
            (false, WakeLevel::App) => "Uses the default",
            (false, WakeLevel::Project) => "Uses the app rule",
            (false, WakeLevel::Agent(_)) => "Uses the project rule",
        };
        let mut rows = vec![spread(
            content,
            Row::of(format!("Wake the master for {whose}"), look.heading),
            Row::of(source, look.muted),
        )];
        for (row, kind) in EventKind::ALL.into_iter().enumerate() {
            let mark = if shown.wakes(kind) { "[x] " } else { "[ ] " };
            rows.push(list_row(
                Row::of(mark, look.fg).push(wake_kind_label(kind), look.fg),
                content,
                edit.row == row,
                true,
                look,
            ));
        }
        rows.push(list_row(
            Row::of("Wakes an hour before pausing  ", look.fg)
                .push(shown.hourly_cap.to_string(), look.heading)
                .push("  (0 never wakes)", look.muted),
            content,
            edit.row == EventKind::ALL.len(),
            true,
            look,
        ));
        rows
    }

    pub(super) fn keep_warm(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let on = !status.keep_warm;
        let done = match (on, status.min_agents) {
            (false, _) => "Agents now stop when they run out of work.".to_owned(),
            (true, 0) => {
                "Keep warm is on, but this team keeps no agents ready. Raise the minimum in Settings."
                    .to_owned()
            }
            (true, count) => format!(
                "Keeping {} ready between tasks.",
                agents_word(usize::from(count))
            ),
        };
        self.changing = Some(done);
        TeamReply::Change(TeamAction::KeepWarm {
            project_id: self.project.clone(),
            on,
            version: status.settings_version,
        })
    }

    /// Moves one time limit to its next choice. The task and session limits go Off, then
    /// 30 min up to 8 h, then Off again; when both are set, the other follows so a session is
    /// never shorter than a task, and Off on either bounds nothing. The idle stop goes Off,
    /// 2 min, 10 min, 1 h. Takes which limit. Returns the
    /// change, or Handled while the team loads or saves.
    pub(super) fn time_limit(&mut self, which: Limit) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let below = |a: Option<u32>, b: Option<u32>| matches!((a, b), (Some(a), Some(b)) if a < b);
        let limits = &crate::team::LIMIT_CHOICES_SECS;
        let (turn, kept, idle) = (
            status.turn_limit_secs,
            status.session_limit_secs,
            status.idle_stop_secs,
        );
        let version = status.settings_version;
        let (turn, kept, idle) = match which {
            Limit::Task => {
                let next = next_limit(limits, turn);
                (next, if below(kept, next) { next } else { kept }, idle)
            }
            Limit::Session => {
                let next = next_limit(limits, kept);
                (if below(next, turn) { next } else { turn }, next, idle)
            }
            Limit::Idle => (
                turn,
                kept,
                next_limit(&crate::team::IDLE_CHOICES_SECS, idle),
            ),
        };
        self.changing = Some(match which {
            Limit::Idle => format!(
                "{} {}",
                idle.map_or_else(
                    || "Idle sub-agents now keep running until the team stops.".to_owned(),
                    |secs| {
                        format!(
                            "An idle sub-agent now stops after {}.",
                            crate::team::span_words(secs.into())
                        )
                    },
                ),
                status.master_idle_stop.sentence()
            ),
            Limit::Task | Limit::Session => format!(
                "Each task may now run {}, each agent session {}.",
                limit_words(turn),
                limit_words(kept)
            ),
        });
        TeamReply::Change(TeamAction::TimeLimits {
            project_id: self.project.clone(),
            turn_secs: turn,
            session_secs: kept,
            idle_secs: idle,
            version,
        })
    }

    /// Flips the On harness switch setting between Carry over and Start fresh.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn harness_switch(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let version = status.settings_version;
        let mode = match status.on_harness_switch {
            SwitchMode::Carry => SwitchMode::Fresh,
            SwitchMode::Fresh => SwitchMode::Carry,
        };
        self.changing = Some(
            match mode {
                SwitchMode::Carry => {
                    "When the master harness changes, the new harness gets the earlier conversation."
                }
                SwitchMode::Fresh => {
                    "When the master harness changes, the new harness starts fresh."
                }
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::OnHarnessSwitch {
            project_id: self.project.clone(),
            mode,
            version,
        })
    }

    /// Flips Apply a master harness change between When the master is idle and At the next
    /// start (N-D26), naming the team settings version the view read so the engine refuses it
    /// as stale after a change made elsewhere. Output: the change to send, or Handled with a
    /// notice while loading or saving.
    pub(super) fn apply_switch(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let version = status.settings_version;
        let mode = match status.apply_harness_switch {
            SwitchApply::WhenIdle => SwitchApply::NextStart,
            SwitchApply::NextStart => SwitchApply::WhenIdle,
        };
        self.changing = Some(
            match mode {
                SwitchApply::WhenIdle => {
                    "A saved master harness change now reaches the running master once it is idle."
                }
                SwitchApply::NextStart => {
                    "A saved master harness change now waits for the master's next start."
                }
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::ApplyHarnessSwitch {
            project_id: self.project.clone(),
            mode,
            version,
        })
    }

    /// The saved master harness: the team's master harness row. Takes the view. Returns its
    /// engine id, none before the first read.
    fn saved_master(&self) -> Option<&str> {
        self.status
            .as_ref()?
            .harnesses
            .iter()
            .find(|harness| harness.role == AgentRole::Master)
            .map(|harness| harness.harness.as_str())
    }

    /// The harness the running master uses, else the saved one. Takes the view. Returns its
    /// engine id, none before the first read.
    fn running_master(&self) -> Option<&str> {
        self.status
            .as_ref()?
            .agents
            .iter()
            .find(|agent| agent.role == AgentRole::Master && agent.status.live())
            .map(|agent| agent.harness.as_str())
            .or_else(|| self.saved_master())
    }

    /// The harnesses the master harness picker offers: the catalog's that can run the
    /// master, in its order. Takes the view. Returns them, none before the catalog is read.
    fn master_choices(&self) -> Option<Vec<&HarnessChoice>> {
        self.catalog.as_ref().map(|catalog| {
            catalog
                .iter()
                .filter(|choice| {
                    matches!(
                        choice.state,
                        HarnessState::Ready | HarnessState::MasterOnly | HarnessState::Unknown
                    )
                })
                .collect()
        })
    }

    /// The picked row of the master harness picker: the owner's, else the saved harness,
    /// else the first. Takes the view and the offered harnesses. Returns a row index.
    fn pick_cursor(&self, choices: &[&HarnessChoice]) -> usize {
        let saved = || {
            self.saved_master().and_then(|saved| {
                choices
                    .iter()
                    .position(|choice| same_harness(&choice.id, saved))
            })
        };
        self.master_pick
            .and_then(|pick| pick.cursor)
            .or_else(saved)
            .unwrap_or(0)
            .min(choices.len().saturating_sub(1))
    }

    /// Opens the master harness picker; the host reads the catalog and the stored setup
    /// for it. Output: Handled, with a notice while the team is loading.
    pub(super) fn open_master_pick(&mut self) -> TeamReply {
        if self.status.is_none() {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        }
        self.master_pick = Some(MasterPick { cursor: None });
        self.catalog_due = self.catalog.is_none();
        TeamReply::Handled
    }

    /// Handles one key while the master harness picker is open: arrows move, Enter saves
    /// the picked harness, Esc closes. Takes the key. Output: the save to send, or Handled.
    pub(super) fn pick_key(&mut self, key: KeyEvent) -> TeamReply {
        match key.code {
            KeyCode::Esc => {
                self.master_pick = None;
                TeamReply::Handled
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(choices) = self.master_choices().filter(|list| !list.is_empty()) {
                    let at = self.pick_cursor(&choices);
                    let last = choices.len() - 1;
                    let next = if key.code == KeyCode::Down {
                        (at + 1).min(last)
                    } else {
                        at.saturating_sub(1)
                    };
                    self.master_pick = Some(MasterPick { cursor: Some(next) });
                }
                TeamReply::Handled
            }
            KeyCode::Enter => self.save_master(),
            _ => TeamReply::Handled,
        }
    }

    /// Saves the picked harness as the team's master through the project setup at the
    /// revision read, the rest of the setup as stored; the saved one closes the picker
    /// with nothing sent. Output: the save, or Handled while reading or saving.
    fn save_master(&mut self) -> TeamReply {
        let picked = self.master_choices().and_then(|choices| {
            let choice = choices.get(self.pick_cursor(&choices))?;
            Some((choice.id.clone(), choice.name.clone()))
        });
        let (Some((id, name)), Some(stored), Some(status)) =
            (picked, self.stored.as_deref(), self.status.as_ref())
        else {
            return TeamReply::Handled;
        };
        if same_harness(&id, &stored.team.master) {
            self.master_pick = None;
            self.notice = Some(Notice::plain(format!("The master already uses {name}.")));
            return TeamReply::Handled;
        }
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let action = SetupAction::Update {
            project_id: stored.project_id.clone(),
            kept: None,
            expected_revision: stored.revision,
            name: None,
            setup: ProjectSetup {
                about: stored.about.clone(),
                tech_stack: stored.tech_stack.clone(),
                platforms: stored.platforms.clone(),
                research: stored.research.clone(),
                pool: stored.pool.clone(),
                team: Some(TeamPlan {
                    master: id,
                    ..stored.team.clone()
                }),
                documents: Vec::new(),
            },
        };
        self.changing = Some(match status.apply_harness_switch {
            SwitchApply::WhenIdle => format!("The master switches to {name} once it is idle."),
            SwitchApply::NextStart => format!("The master switches to {name} at its next start."),
        });
        self.saving = Some(action.clone());
        TeamReply::Save(action)
    }

    /// The master harness picker's rows: its title, each harness that can run the master
    /// with the running one marked now, and when a saved change reaches the running master.
    /// Takes the content width and the look. Returns the rows.
    pub(super) fn pick_rows(&self, content: usize, look: &Look) -> Vec<Row> {
        let mut rows = vec![Row::of("Harness for the master", look.heading)];
        let (Some(choices), Some(_)) = (self.master_choices(), self.stored.as_ref()) else {
            rows.push(Row::of("Reading the harnesses...", look.muted));
            return rows;
        };
        if choices.is_empty() {
            rows.push(Row::of(
                "No harness can run the master. Install one in Settings, Harnesses.",
                look.muted,
            ));
            return rows;
        }
        let at = self.pick_cursor(&choices);
        let running = self.running_master();
        for (index, choice) in choices.iter().enumerate() {
            let mut row = Row::of(choice.name.clone(), look.fg);
            if running.is_some_and(|id| same_harness(&choice.id, id)) {
                row = row.push("  now", look.muted);
            }
            rows.push(list_row(row, content, index == at, true, look));
        }
        let when = match self
            .status
            .as_ref()
            .map(|status| status.apply_harness_switch)
        {
            Some(SwitchApply::NextStart) => {
                "The running master keeps its harness until it starts again."
            }
            Some(SwitchApply::WhenIdle) | None => "The running master switches once it is idle.",
        };
        rows.extend(
            wrap_words(&format!("Enter saves it. {when}"), content)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        rows
    }

    /// Moves the Sub-agent depth setting to its next choice, 1 after 3.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn sub_agent_depth(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let choices = crate::team::DEPTH_CHOICES;
        let depth = choices
            .iter()
            .position(|choice| *choice == status.sub_agent_depth)
            .map_or(choices[0], |at| choices[(at + 1) % choices.len()]);
        self.changing = Some(depth_note(depth).to_owned());
        TeamReply::Change(TeamAction::SubAgentDepth {
            project_id: self.project.clone(),
            depth,
        })
    }

    /// Flips where a lead's worker lands its change set between the lead's Accept and the
    /// owner's own. Output: the change to send, or Handled with a notice while loading or
    /// saving.
    pub(super) fn child_landing(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let mode = match status.child_landing {
            ChildLanding::Lead => ChildLanding::Owner,
            ChildLanding::Owner => ChildLanding::Lead,
        };
        self.changing = Some(
            match mode {
                ChildLanding::Lead => {
                    "A lead's worker now lands on the lead's branch when the lead accepts it."
                }
                ChildLanding::Owner => "A lead's worker now lands only when you accept it.",
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::ChildLanding {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Flips the Agent branches setting between Named branch and Detached.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn agent_branches(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let mode = match status.agent_branches {
            BranchMode::Named => BranchMode::Detached,
            BranchMode::Detached => BranchMode::Named,
        };
        self.changing = Some(
            match mode {
                BranchMode::Named => "Each new agent now works on its own branch.",
                BranchMode::Detached => "New agents now work on a detached HEAD.",
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::AgentBranches {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Opens the editor of one branch setting filled with the current choice, empty for
    /// its default: the repository's default branch for the Master's branch, the Master's
    /// branch for Land on. Takes the setting. Output: none; a notice while the team is
    /// loading.
    pub(super) fn open_branch_edit(&mut self, setting: BranchSetting) {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return;
        };
        let mut input = TextInput::single(MASTER_BRANCH_BYTES_MAX);
        input.set(
            match setting {
                BranchSetting::Master => status.master_branch.as_deref(),
                BranchSetting::LandOn => status.land_on.as_deref(),
            }
            .unwrap_or(""),
        );
        self.branch_edit = Some(BranchEdit { setting, input });
    }

    /// Handles one key while a branch editor is open: Enter saves, Esc closes, other keys
    /// edit. Takes the key. Output: the change to send, or Handled.
    pub(super) fn branch_key(&mut self, key: KeyEvent) -> TeamReply {
        match key.code {
            KeyCode::Esc => {
                self.branch_edit = None;
                TeamReply::Handled
            }
            KeyCode::Enter => self.save_branch(),
            _ => {
                if let Some(edit) = &mut self.branch_edit {
                    edit.input.handle(key);
                }
                TeamReply::Handled
            }
        }
    }

    /// Sends the branch typed in the open editor to its setting; an empty name goes back to
    /// the setting's default. Output: the change to send, or Handled with a notice while
    /// saving.
    fn save_branch(&mut self) -> TeamReply {
        if self.branch_edit.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(edit) = &self.branch_edit else {
            return TeamReply::Handled;
        };
        let typed = edit.input.value().trim();
        let branch = (!typed.is_empty()).then(|| typed.to_owned());
        let setting = edit.setting;
        self.changing = Some(match (setting, &branch) {
            (BranchSetting::Master, Some(name)) => format!("The master now works on {name}."),
            (BranchSetting::Master, None) => {
                "The master now works on the repository's default branch.".to_owned()
            }
            (BranchSetting::LandOn, Some(name)) => format!("Work now lands on {name}."),
            (BranchSetting::LandOn, None) => "Work now lands on the master's branch.".to_owned(),
        });
        let project_id = self.project.clone();
        TeamReply::Change(match setting {
            BranchSetting::Master => TeamAction::MasterBranch { project_id, branch },
            BranchSetting::LandOn => TeamAction::LandOn { project_id, branch },
        })
    }

    /// Flips "Master accepts change sets" between Itself and Only when I click or say accept
    /// all (DEC-147.7).
    /// Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn ask_before_merge(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let on = !status.ask_before_merge;
        self.changing = Some(
            if on {
                "The master now merges a harness only after you click Accept or send accept all to the team, and waits for your Accept on its own conflict fixes."
            } else {
                "The master now accepts change sets itself."
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::AskBeforeMerge {
            project_id: self.project.clone(),
            on,
        })
    }

    /// Flips "When the master resolves conflicts" between Merge into main itself and Wait
    /// for my Accept (DEC-117 Q2), offered only at Full access. Output: the change to send,
    /// or Handled with a notice while loading, saving or below Full access.
    pub(super) fn master_lands(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let Some(current) = status.master_lands else {
            self.notice = Some(Notice::plain(if status.access_unreadable {
                "This project's agent access can't be read, so the master's conflict fixes wait for your Accept."
            } else {
                "The master's own change sets land by themselves only at Full access."
            }));
            return TeamReply::Handled;
        };
        let mode = match current {
            MasterLands::Merges => MasterLands::Waits,
            MasterLands::Waits => MasterLands::Merges,
        };
        self.changing = Some(
            match mode {
                MasterLands::Merges => "The master now merges its conflict fixes into main itself.",
                MasterLands::Waits => "The master now waits for your Accept on its conflict fixes.",
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::MasterLands {
            project_id: self.project.clone(),
            mode,
        })
    }

    /// Flips "Re-ask empty reply" between On and Off: whether a sub-agent that ends a task
    /// with no reply text is asked once more for its reply (DEC-111). Output: the change to
    /// send, or Handled with a notice while loading or saving.
    pub(super) fn reask_empty_reply(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let on = !status.reask_empty_reply;
        self.changing = Some(
            if on {
                "A sub-agent that ends a task with no reply text is now asked once more for its reply."
            } else {
                "A sub-agent's task with no reply text is now reported without asking again."
            }
            .to_owned(),
        );
        TeamReply::Change(TeamAction::ReaskEmptyReply {
            project_id: self.project.clone(),
            on,
        })
    }

    /// Moves "End a turn after done" to its next choice: 2 min, 10 min, 1 h, Off and back to
    /// 2 min (F15): how long a task turn may stay quiet after its agent reported done before
    /// the engine ends it. Output: the change to send, or Handled with a notice while loading
    /// or saving.
    pub(super) fn done_quiet(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let secs = done_quiet_next(status.done_quiet_secs);
        self.changing = Some(match secs {
            Some(secs) => format!(
                "A task turn now ends {} after its agent says done and goes quiet.",
                crate::team::span_words(secs.into())
            ),
            None => "A task turn now ends only when its agent ends it.".to_owned(),
        });
        TeamReply::Change(TeamAction::DoneQuiet {
            project_id: self.project.clone(),
            secs,
        })
    }

    /// Moves "Stop the master when idle after" to its next choice: 15 min, 1 h, Off and back
    /// to 15 min (DEC-117 Q4). Output: the change to send, or Handled with a notice while
    /// loading or saving.
    pub(super) fn master_idle_stop(&mut self) -> TeamReply {
        use crate::team::MasterIdleStop::{FifteenMinutes, Off, OneHour};
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let (choice, words) = match status.master_idle_stop {
            FifteenMinutes => (OneHour, "An idle master now stops after 1 h."),
            OneHour => (
                Off,
                "An idle master now keeps running until the team stops.",
            ),
            Off => (FifteenMinutes, "An idle master now stops after 15 min."),
        };
        self.changing = Some(words.to_owned());
        TeamReply::Change(TeamAction::MasterIdleStop {
            project_id: self.project.clone(),
            choice,
        })
    }

    /// Turns "Live terminal" off or on, each way with its plain notice (DEC-117 Q4). Output:
    /// the change to send, or Handled with a notice while loading or saving.
    pub(super) fn live_terminal(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        if !master_runs_live(status) {
            self.notice = Some(Notice::plain(NO_LIVE_TERMINAL));
            return TeamReply::Handled;
        }
        if self.changing.is_some() {
            self.notice = Some(Notice::problem(
                "Still saving the last change. Try again in a moment.",
            ));
            return TeamReply::Handled;
        }
        let on = !status.master_live_terminal;
        let words = if on {
            "Live terminal is on: the master runs in a terminal you see and type into."
        } else {
            "Live terminal is off: the master ends at its next stop and its conversation is the console."
        };
        self.changing = Some(words.to_owned());
        TeamReply::Change(TeamAction::MasterLiveTerminal {
            project_id: self.project.clone(),
            on,
        })
    }

    /// Moves "Terminals per window", how many terminals each terminal wall window holds,
    /// to its next choice: 4, 6, 9 and back to 4. Output: the change to send, or Handled
    /// with a notice while loading or saving.
    pub(super) fn terminals_per_window(&mut self) -> TeamReply {
        use crate::team::TerminalsPerWindow::{Four, Nine, Six};
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let per_window = match status.terminals_per_window {
            Four => Six,
            Six => Nine,
            Nine => Four,
        };
        self.changing = Some(format!(
            "Open all terminals now puts {} terminals in each window.",
            u8::from(per_window)
        ));
        TeamReply::Change(TeamAction::TerminalsPerWindow {
            project_id: self.project.clone(),
            per_window,
        })
    }

    /// Moves "New folder limit", the most files one new top-level folder may gain in a change
    /// set (DEC-107), to its next choice. Output: the change to send, or Handled with a
    /// notice while loading or saving.
    pub(super) fn new_folder_limit(&mut self) -> TeamReply {
        if self.status.is_some() && self.still_saving() {
            return TeamReply::Handled;
        }
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let choices = crate::team::NEW_FOLDER_FILES_CHOICES;
        let files = choices
            .iter()
            .position(|choice| *choice == status.new_folder_files_max)
            .map_or(choices[0], |at| choices[(at + 1) % choices.len()]);
        self.changing = Some(format!(
            "A change set adding more than {files} files in one new folder is now refused."
        ));
        TeamReply::Change(TeamAction::NewFolderLimit {
            project_id: self.project.clone(),
            files,
        })
    }

    /// Moves "Background commands", what a task does at its turn end while commands its
    /// agent started in the background still run (DEC-111 Q13), from Wait to a grace time of
    /// [`crate::team::BACKGROUND_GRACE_SECS_DEFAULT`], to Stop and back to Wait. Output: the
    /// change to send, or Handled with a notice while loading or saving.
    pub(super) fn background_at_end(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let choice = match status.background_at_end {
            BackgroundAtEnd::Wait => BackgroundAtEnd::Grace {
                secs: crate::team::BACKGROUND_GRACE_SECS_DEFAULT,
            },
            BackgroundAtEnd::Grace { .. } => BackgroundAtEnd::Stop,
            BackgroundAtEnd::Stop => BackgroundAtEnd::Wait,
        };
        self.save_background(choice)
    }

    /// Moves the grace time of "Background commands" to the next of
    /// [`crate::team::BACKGROUND_GRACE_CHOICES_SECS`] above it, the first after the last, or
    /// starts a grace time of [`crate::team::BACKGROUND_GRACE_SECS_DEFAULT`] from Wait or
    /// Stop. Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn grace_time(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let choices = crate::team::BACKGROUND_GRACE_CHOICES_SECS;
        let secs = match status.background_at_end {
            BackgroundAtEnd::Grace { secs } => choices
                .iter()
                .copied()
                .find(|choice| *choice > secs)
                .unwrap_or(choices[0]),
            BackgroundAtEnd::Wait | BackgroundAtEnd::Stop => {
                crate::team::BACKGROUND_GRACE_SECS_DEFAULT
            }
        };
        assert!(crate::team::BACKGROUND_GRACE_SECS.contains(&secs));
        self.save_background(BackgroundAtEnd::Grace { secs })
    }

    /// Sends one Background commands choice with the notice that says what it now does and
    /// which agents it acts on. Takes the choice. Output: the change to send, or Handled
    /// while saving.
    fn save_background(&mut self, choice: BackgroundAtEnd) -> TeamReply {
        let done = match choice {
            BackgroundAtEnd::Wait => "A task now waits for the commands its agent started in the background, up to its task limit.".to_owned(),
            BackgroundAtEnd::Grace { secs } => format!(
                "A task's background commands now get {} after its turn ends, then stop.",
                crate::team::span_words(secs.into())
            ),
            BackgroundAtEnd::Stop => "A task's background commands now stop when its turn ends, and its agent is told so.".to_owned(),
        };
        let done = format!("{done} {BACKGROUND_SCOPE}");
        let action = TeamAction::BackgroundAtEnd {
            project_id: self.project.clone(),
            choice,
        };
        self.change(&done, action)
    }

    /// Moves "Left running after a stop", what the engine does with processes an agent leaves
    /// running after its stop (DEC-147.4), from End them to Keep running to Ask me and back.
    /// Output: the change to send, or Handled with a notice while loading or saving.
    pub(super) fn leftovers_setting(&mut self) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let (mode, done) = match status.leftovers {
            LeftoverMode::End => (
                LeftoverMode::Keep,
                "Processes an agent leaves running after its stop now keep running, and its account is let go at once.",
            ),
            LeftoverMode::Keep => (
                LeftoverMode::Ask,
                "Processes an agent leaves running after its stop now keep its account until you press X to end them or K to keep them.",
            ),
            LeftoverMode::Ask => (
                LeftoverMode::End,
                "Processes an agent leaves running after its stop are now ended after 10 seconds, then its account is let go.",
            ),
        };
        let action = TeamAction::LeftoversAtEnd {
            project_id: self.project.clone(),
            mode,
        };
        self.change(done, action)
    }

    /// Ends (X) or keeps (K) the processes of the oldest hold the owner's key still changes:
    /// End skips a hold already ending, Keep one already kept. Input: whether to end them.
    /// Output: the change to send, or Handled with a notice when none is listed.
    pub(super) fn settle_hold(&mut self, end: bool) -> TeamReply {
        let Some(status) = &self.status else {
            self.notice = Some(Notice::plain("The team is still loading."));
            return TeamReply::Handled;
        };
        let skip = if end {
            HoldState::Ending
        } else {
            HoldState::Kept
        };
        let Some(hold) = status.leftover_holds.iter().find(|hold| hold.state != skip) else {
            self.notice = Some(Notice::plain(
                "Nothing an agent left running waits for you.",
            ));
            return TeamReply::Handled;
        };
        let (done, action) = if end {
            (
                format!("Ending what {} left running.", hold.who),
                TeamAction::EndLeftovers {
                    project_id: self.project.clone(),
                    key: hold.key,
                },
            )
        } else {
            (
                format!(
                    "Keeping what {} left running; its account is let go.",
                    hold.who
                ),
                TeamAction::KeepLeftovers {
                    project_id: self.project.clone(),
                    key: hold.key,
                },
            )
        };
        self.change(&done, action)
    }
}
