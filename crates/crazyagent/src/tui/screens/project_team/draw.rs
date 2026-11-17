use super::*;

impl TeamView {
    /// Draws the Team view like `draw` without the typing cursor.
    /// Takes the content width, the rows available, the accounts used to name
    /// agents' accounts and the look. Returns the body rows and the help keys.
    #[cfg(all(test, unix))]
    pub fn rows(
        &self,
        content: usize,
        room: usize,
        accounts: &[AccountRow],
        look: &Look,
    ) -> (Vec<Row>, Help) {
        let (lines, help, _) = self.draw(content, room, accounts, look);
        (lines, help)
    }

    /// Draws the Team view for the Team screen's settings box and the Team settings screen,
    /// and places the typing cursor.
    /// When the window is too short for the lists, the panel and any footer question, the
    /// blank rows before the panel and before the tasks go first, then the focused agent's
    /// Tools and Working copy rows, then the settings rows after the first scroll behind a
    /// marker row (PgUp and PgDn), then the plan line while every harness can run, once the
    /// settings can scroll no further. The counts row and the hold rows never scroll.
    /// Takes the content width, the rows available, the accounts and the look.
    /// Returns the body rows, the help keys and, while the owner writes a task,
    /// the cursor as a body row and a column inside that row.
    pub fn draw(
        &self,
        content: usize,
        room: usize,
        accounts: &[AccountRow],
        look: &Look,
    ) -> (Vec<Row>, Help, Option<(usize, usize)>) {
        let wide = content >= WIDE;
        let (mut lines, packed) = self.summary(content, look);
        let summary_rows = lines.len();
        assert!(packed >= 1 && packed <= summary_rows);
        let Some(status) = &self.status else {
            lines.push(Row::of("Loading...", look.muted));
            lines.extend(
                self.notice
                    .iter()
                    .flat_map(|notice| notice.rows(look, content)),
            );
            return (lines, self.help(), None);
        };
        lines.push(Row::of(plan_line(status), look.muted));
        if let Some(label) = status
            .harnesses
            .iter()
            .find(|harness| {
                harness.role == AgentRole::Master
                    && harness.delegation == Some(DelegationPath::Unavailable)
            })
            .and_then(|harness| harness.delegation_label.clone())
        {
            lines.push(Row::of(label, look.muted));
        }
        if self.alerts {
            let notice: Vec<Row> = self
                .notice
                .iter()
                .flat_map(|notice| notice.rows(look, content))
                .collect();
            let room = room.saturating_sub(lines.len() + notice.len());
            lines.extend(self.alert_rows(content, room, look));
            lines.extend(notice);
            return (lines, self.help(), None);
        }
        if let Some(list) = &self.messages {
            lines.push(Row::of(list.title(), look.heading));
            let footer = self.footer(content, look);
            let room = room.saturating_sub(lines.len() + footer.len());
            lines.extend(
                list.lines(content, room)
                    .into_iter()
                    .map(|line| shown_row(line, look)),
            );
            lines.extend(footer);
            return (lines, self.help(), None);
        }
        if wide {
            lines.push(Row::default());
        }
        let tree = self.agent_tree();
        let agents: Vec<&TeamAgent> = tree.iter().map(|(agent, _)| *agent).collect();
        let tasks = self.tasks();
        let picking = self.master_pick.is_some();
        let mut detail = false;
        let (panel, caret) = match (&self.draft, &self.branch_edit, self.wake, self.message_row) {
            _ if picking => (self.pick_rows(content, look), None),
            (Some(draft), _, _, _) => {
                let (rows, at) = draft_rows(draft, content, look);
                (rows, Some(at))
            }
            (None, Some(edit), _, _) => {
                let (rows, at) = branch_rows(edit, content, look);
                (rows, Some(at))
            }
            (None, None, Some(edit), _) => (self.wake_rows(edit, content, look), None),
            (None, None, None, Some(row)) => (self.message_setting_rows(row, content, look), None),
            (None, None, None, None) => match &self.coord {
                Some(edit) => self.coord_rows(edit, content, look),
                None => {
                    detail = true;
                    (self.panel(content, accounts, false, look), None)
                }
            },
        };
        let footer = self.footer(content, look);
        let hidden = self.finished_hidden();
        let fixed = lines.len() + 5 + usize::from(hidden > 0) + panel.len() + footer.len();
        let over = (fixed + 2).saturating_sub(room);
        let spacers = over.min(2);
        let whole = panel.len();
        let panel = if detail && over > spacers {
            self.panel(content, accounts, true, look)
        } else {
            panel
        };
        let briefed = whole - panel.len();
        let left = (over - spacers).saturating_sub(briefed);
        let plan_can_go = status.harnesses.iter().all(|harness| harness.runnable)
            && status
                .harnesses
                .iter()
                .any(|harness| harness.role == AgentRole::Master);
        let scrolled = scrolled_rows(packed, left);
        let won = scrolled.saturating_sub(1);
        let plan_gives_way = usize::from(plan_can_go && left > won);
        let in_view = packed - 1 - scrolled;
        assert!(
            scrolled == 0 || in_view >= 1,
            "one scrolled row stays in view"
        );
        assert!(
            scrolled == 0 || spacers == 2,
            "blank rows go before settings"
        );
        assert!(
            plan_gives_way == 0 || scrolled == 0 || in_view == 1,
            "the settings scroll before the plan line goes"
        );
        self.settings_scroll_max.set(scrolled);
        self.settings_window.set(in_view);
        if plan_gives_way == 1 {
            lines.remove(summary_rows);
        }
        if scrolled > 0 {
            let offset = self.settings_scroll.min(scrolled);
            lines.drain(1 + offset + in_view..packed);
            lines.drain(1..1 + offset);
            lines.insert(
                1 + in_view,
                scroll_marker(offset > 0, offset < scrolled, content, look),
            );
        }
        let fixed = fixed - spacers - briefed - plan_gives_way - won;
        let free = room.saturating_sub(fixed).max(2);
        let wanted_tasks = tasks.len().max(1).min(free / 2);
        let agent_rows = agents
            .len()
            .max(1)
            .min(free.saturating_sub(wanted_tasks))
            .max(1);
        let task_rows = free.saturating_sub(agent_rows).max(1);

        let agent_widths = if wide {
            vec![16, 13, 22, content.saturating_sub(2 + 16 + 13 + 22 + 6)]
        } else {
            vec![16, 13, content.saturating_sub(2 + 16 + 13 + 4)]
        };
        let agent_heads: &[&str] = if wide {
            &["Agent", "Harness", "Account", "Status"]
        } else {
            &["Agent", "Harness", "Status"]
        };
        lines.push(heads(&agent_widths, agent_heads, look));
        if agents.is_empty() {
            lines.push(Row::of(
                "  No agents are running. They start when there is work.",
                look.muted,
            ));
        }
        let first = window(self.agent_cursor, agents.len(), agent_rows);
        for (index, agent) in agents.iter().enumerate().skip(first).take(agent_rows) {
            let current = agent.task.and_then(|id| self.task(id));
            let last = current.is_none().then(|| self.last_task(agent)).flatten();
            let depth = tree[index].1;
            let shown = if depth == 0 {
                agent.label.clone()
            } else {
                format!("{}\u{21b3} {}", "  ".repeat(depth - 1), agent.label)
            };
            let mut cells = vec![
                Row::of(shown, look.fg),
                Row::of(harness_name(&agent.harness, None), look.fg),
            ];
            if wide {
                cells.push(Row::of(
                    account_name(agent.account.as_deref(), accounts),
                    look.fg,
                ));
            }
            cells.push(agent_status(agent, current, last, look));
            lines.push(list_row(
                columns(&agent_widths, cells),
                content,
                index == self.agent_cursor,
                self.focus == TeamFocus::Agents,
                look,
            ));
        }
        if hidden > 0 {
            let finished = if hidden == 1 {
                "1 finished agent is".to_owned()
            } else {
                format!("{hidden} finished agents are")
            };
            lines.push(Row::of(
                format!("  {finished} in Finished: press Esc, then f."),
                look.muted,
            ));
        }
        if spacers < 2 {
            lines.push(Row::default());
        }

        lines.push(Row::of("Tasks", look.heading).push(format!("  {}", tasks.len()), look.muted));
        let task_widths = if wide {
            vec![28, 13, 16, content.saturating_sub(2 + 28 + 13 + 16 + 6)]
        } else {
            vec![30, 13, content.saturating_sub(2 + 30 + 13 + 4)]
        };
        let task_heads: &[&str] = if wide {
            &["Task", "Harness", "From", "Status"]
        } else {
            &["Task", "Harness", "Status"]
        };
        if tasks.is_empty() {
            lines.push(Row::of(
                "  No tasks yet. The master hands work to sub-agents here.",
                look.muted,
            ));
        } else {
            lines.push(heads(&task_widths, task_heads, look));
        }
        let first = window(self.task_cursor, tasks.len(), task_rows);
        for (index, task) in tasks.iter().enumerate().skip(first).take(task_rows) {
            let mut cells = vec![
                Row::of(title(&task.prompt), look.fg),
                Row::of(harness_name(&task.harness, None), look.fg),
            ];
            if wide {
                cells.push(Row::of(self.task_from(task), look.fg));
            }
            cells.push(task_status(task, self.now_ms, self.offset, look));
            lines.push(list_row(
                columns(&task_widths, cells),
                content,
                index == self.task_cursor,
                self.focus == TeamFocus::Tasks,
                look,
            ));
        }
        if spacers == 0 {
            lines.push(Row::default());
        }
        let cursor = caret.map(|(row, column)| (lines.len() + row, column));
        lines.extend(panel);
        lines.extend(footer);
        (lines, self.help(), cursor)
    }

    /// The Team rows: the agent counts, a stopped team and alerts, then every team setting whole,
    /// right-aligned, then one row per Left running hold. Settings wrap in order only, so a
    /// resize or a wider value never moves a setting ahead of an earlier one; a short window
    /// scrolls them instead. Counts wider than the content are cut like any row. Inputs: the
    /// content width and the look. Output: the rows and how many of them hold the counts and
    /// settings, the hold rows after them.
    fn summary(&self, content: usize, look: &Look) -> (Vec<Row>, usize) {
        let Some(status) = &self.status else {
            return (vec![Row::of("Team", look.accent).pad(content)], 1);
        };
        let count = |wanted: &[AgentStatus]| {
            status
                .agents
                .iter()
                .filter(|agent| wanted.contains(&agent.status))
                .count()
        };
        let mut left = Row::of("Team", look.accent);
        for (number, word, style) in [
            (count(&[AgentStatus::Working]), "working", look.running),
            (
                count(&[AgentStatus::Starting, AgentStatus::Idle]),
                "waiting",
                look.muted,
            ),
            (
                count(&[AgentStatus::Failed]),
                "stopped with a problem",
                look.error,
            ),
        ] {
            if number > 0 {
                left = left
                    .push("  ", Style::new())
                    .push(format!("{number} {word}"), style);
            }
        }
        let asking = status.events.waiting.len();
        if asking > 0 {
            left = left
                .push("  ", Style::new())
                .push(format!("{asking} waiting for an answer"), look.warning);
        }
        if status.paused {
            left = left.push("  ", Style::new()).push(
                if status.budget_reached {
                    "stopped: token budget used"
                } else {
                    "stopped by you"
                },
                look.error,
            );
        }
        if status.events.paused {
            left = left
                .push("  ", Style::new())
                .push("auto-wake paused", look.warning);
        }
        if status.partial {
            left = left
                .push("  ", Style::new())
                .push("older history hidden", look.muted);
        }
        let setting = |label: &str, value: String| {
            Row::of(format!("{label}  "), look.muted).push(value, look.fg)
        };
        let settings = [
            setting(
                "Keep warm",
                if status.keep_warm { "On" } else { "Off" }.to_owned(),
            ),
            setting("Task", limit_label(status.turn_limit_secs)),
            setting("Session", limit_label(status.session_limit_secs)),
            setting("Idle stop", limit_label(status.idle_stop_secs)),
            setting(
                "On harness switch",
                format!(
                    "{}, {}",
                    switch_words(status.on_harness_switch),
                    apply_words(status.apply_harness_switch)
                ),
            ),
            setting("Sub-agent depth", status.sub_agent_depth.to_string()),
            setting(
                "Worker lands",
                landing_words(status.child_landing).to_owned(),
            ),
            setting(
                "Agent branches",
                branch_words(status.agent_branches).to_owned(),
            ),
            setting("Max harnesses", cap_words(status.max_harnesses)),
        ];
        let live_terminal = master_runs_live(status).then(|| {
            setting(
                "Live terminal",
                if status.master_live_terminal {
                    "On"
                } else {
                    "Off"
                }
                .to_owned(),
            )
        });
        let middle = [
            setting("Max teammates", cap_words(status.max_teammates)),
            setting("Sub-agent rounds", rounds_choice(status.master_rounds)),
            setting(
                "Master run time",
                crate::team::master_limits::run_words(status.master_run_secs),
            ),
            setting(
                "Spread accounts",
                if status.spread_accounts { "On" } else { "Off" }.to_owned(),
            ),
            setting(
                "Token budget",
                budget_words(status.token_budget, status.budget_used),
            ),
            setting(
                "Master's branch",
                status
                    .master_branch
                    .clone()
                    .unwrap_or_else(|| "Repository default".to_owned()),
            ),
            setting(
                "Land on",
                status
                    .land_on
                    .clone()
                    .unwrap_or_else(|| "Master's branch".to_owned()),
            ),
            setting(
                "Master accepts change sets",
                if status.ask_before_merge {
                    "Only when I click or say accept all"
                } else {
                    "Itself"
                }
                .to_owned(),
            ),
        ];
        let master_lands = status.master_lands.map(|lands| {
            setting(
                "When the master resolves conflicts",
                match lands {
                    MasterLands::Merges => "Merge into main itself",
                    MasterLands::Waits => "Wait for my Accept",
                }
                .to_owned(),
            )
        });
        let later = [
            setting(
                "Re-ask empty reply",
                if status.reask_empty_reply {
                    "On"
                } else {
                    "Off"
                }
                .to_owned(),
            ),
            setting(
                "End a turn after done",
                done_quiet_words(status.done_quiet_secs),
            ),
            setting(
                "New folder limit",
                format!("{} files", status.new_folder_files_max),
            ),
            setting(
                "Master idle stop",
                status.master_idle_stop.label().to_owned(),
            ),
            setting(
                "Terminals per window",
                u8::from(status.terminals_per_window).to_string(),
            ),
            setting(
                "Background commands",
                background_words(status.background_at_end),
            ),
            setting("Left running", leftover_words(status.leftovers).to_owned()),
        ];
        let items: Vec<Row> = settings
            .into_iter()
            .chain(live_terminal)
            .chain(middle)
            .chain(master_lands)
            .chain(later)
            .collect();
        let mut rows: Vec<Row> = pack(&items, left, content, look.muted)
            .into_iter()
            .map(|(lead, right)| spread(content, lead, right))
            .collect();
        let packed = rows.len();
        for hold in status.leftover_holds.iter().take(LEFTOVER_ROWS_MAX) {
            rows.push(
                Row::of("Left running  ", look.warning)
                    .push(format!("{}: {}", hold.who, hold.names), look.fg)
                    .push(format!("  {}", hold_words(hold.state)), look.muted)
                    .pad(content),
            );
        }
        assert!(packed >= 1 && rows.len() - packed <= LEFTOVER_ROWS_MAX);
        (rows, packed)
    }

    /// The panel under the lists for the focused agent or task. When `brief`, for a short
    /// window, an agent's Tools and Working copy rows give way; a task's panel stays whole.
    /// Takes the content width, the accounts, whether to be brief and the look. Returns the
    /// rows.
    fn panel(&self, content: usize, accounts: &[AccountRow], brief: bool, look: &Look) -> Vec<Row> {
        let entry =
            |label: &str, value: String| Row::of(label, look.muted).pad(14).push(value, look.fg);
        let mut rows = Vec::new();
        match self.focus {
            TeamFocus::Agents => {
                let Some(agent) = self.agents().get(self.agent_cursor).copied() else {
                    return rows;
                };
                let when = match agent.stopped_ms {
                    Some(at) => format!("Stopped {}", clock(at, self.offset)),
                    None => format!(
                        "Started {}, up {}",
                        clock(agent.created_ms, self.offset),
                        duration(self.now_ms - agent.created_ms)
                    ),
                };
                rows.push(spread(
                    content,
                    Row::of(
                        crate::team::names::line(&agent.label, &agent.harness, &agent.status_label),
                        look.heading,
                    ),
                    Row::of(when, look.muted),
                ));
                rows.push(entry(
                    "Account",
                    account_name(agent.account.as_deref(), accounts),
                ));
                if !brief {
                    rows.push(entry("Tools", tools_word(agent.tools).into()));
                }
                if agent.owner_stopped {
                    rows.push(Row::of(
                        "Stopped by you. Wake it from Finished to start it again.",
                        look.warning,
                    ));
                }
                let running = self.running_teammates(agent.id);
                for teammate in &agent.teammates {
                    let state = match (teammate.ended, teammate.stopped) {
                        (_, true) => "stopped".to_owned(),
                        (true, false) => "finished".to_owned(),
                        (false, false) => running
                            .iter()
                            .position(|pane| pane == &teammate.pane)
                            .map_or_else(
                                || "running".to_owned(),
                                |at| format!("running, p then {}", at + 1),
                            ),
                    };
                    rows.push(entry("Teammate", format!("{}  {state}", teammate.pane)));
                }
                if let Some(waiting) = self
                    .status
                    .iter()
                    .flat_map(|status| status.events.waiting.iter())
                    .find(|item| item.agent == agent.id)
                {
                    rows.extend(
                        wrap_words(&format!("Asks: {}", waiting.text), content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.warning)),
                    );
                }
                match agent.task.and_then(|id| self.task(id)) {
                    Some(task) => rows.push(entry("Task", title(&task.prompt))),
                    None => {
                        if let Some(last) = self.last_task(agent) {
                            rows.push(entry("Last task", title(&last.prompt)));
                        }
                        if let Some(path) = &agent.worktree
                            && !brief
                        {
                            rows.push(entry("Working copy", short_path(path)));
                        }
                    }
                }
                if let Some(problem) = &agent.problem {
                    rows.extend(
                        wrap_words(problem, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.error)),
                    );
                }
            }
            TeamFocus::Tasks => {
                let Some(task) = self.tasks().get(self.task_cursor).copied() else {
                    return rows;
                };
                let when = match (task.started_ms, task.finished_ms) {
                    (_, Some(at)) => format!("Finished {}", clock(at, self.offset)),
                    (Some(at), None) => format!(
                        "Started {}, running {}",
                        clock(at, self.offset),
                        duration(self.now_ms - at)
                    ),
                    (None, None) => format!("Added {}", clock(task.created_ms, self.offset)),
                };
                let heading = match (task.parent, task.source) {
                    (Some(lead), _) => self.agent(lead).map_or_else(
                        || "Task from a lead".to_owned(),
                        |lead| format!("Task from {}", lead.label),
                    ),
                    (None, TaskSource::Master) => "Task from the master".to_owned(),
                    (None, TaskSource::Owner) => "Task from you".to_owned(),
                };
                rows.push(spread(
                    content,
                    Row::of(heading, look.heading),
                    Row::of(when, look.muted),
                ));
                rows.extend(
                    wrap_words(task.prompt.trim(), content)
                        .into_iter()
                        .take(PANEL_TEXT_LINES)
                        .map(|line| Row::of(line, look.fg)),
                );
                if let Some(agent) = task.agent.and_then(|id| self.agent(id)) {
                    rows.push(entry("Agent", agent.label.clone()));
                }
                if task.attempts > 1 {
                    rows.push(entry("Tries", task.attempts.to_string()));
                }
                if let Some(wait) = crate::team::wait_line(task.status, task.wait.as_deref()) {
                    rows.extend(
                        wrap_words(&wait, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.muted)),
                    );
                }
                match (&task.result, &task.problem) {
                    (Some(result), _) if task.status == TaskStatus::Done => {
                        rows.push(entry("Result", title(result)));
                    }
                    (_, Some(problem)) => rows.extend(
                        wrap_words(problem, content)
                            .into_iter()
                            .take(PANEL_TEXT_LINES)
                            .map(|line| Row::of(line, look.error)),
                    ),
                    _ => {}
                }
            }
        }
        rows
    }

    fn footer(&self, content: usize, look: &Look) -> Vec<Row> {
        if let Some(ask) = self.ask {
            let label = |id: i64| {
                self.agent(id)
                    .map_or_else(|| "that harness".to_owned(), |agent| agent.label.clone())
            };
            let question = match ask {
                Ask::StopAll => {
                    "Stop the master, every harness and every teammate of this project?".to_owned()
                }
                Ask::StopAgent(id) => {
                    let led = self.status.as_ref().is_some_and(|status| {
                        status
                            .agents
                            .iter()
                            .find(|agent| agent.id == id)
                            .is_some_and(|agent| agent.parent.is_some_and(|lead| lead != id))
                    });
                    if led {
                        format!("Stop {}? Its lead is told.", label(id))
                    } else {
                        format!("Stop {} and every agent it leads?", label(id))
                    }
                }
                Ask::Teammate(id) => {
                    let count = self.running_teammates(id).len();
                    return vec![
                        Row::of(
                            format!("Stop which teammate of {}?", label(id)),
                            look.warning,
                        )
                        .push(format!("   1-{count}"), look.heading)
                        .push(" Teammate   ", look.fg)
                        .push("Esc", look.heading)
                        .push(" Keep them", look.fg),
                    ];
                }
            };
            return vec![
                Row::of(question, look.warning)
                    .push("   y", look.heading)
                    .push(" Stop   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep running", look.fg),
            ];
        }
        if let Some(task) = self.confirm.and_then(|id| self.task(id)) {
            return vec![
                Row::of(format!("Cancel \"{}\"?", title(&task.prompt)), look.warning)
                    .push("   y", look.heading)
                    .push(" Cancel task   ", look.fg)
                    .push("n", look.heading)
                    .push(" Keep", look.fg),
            ];
        }
        if let Some(notice) = &self.notice {
            return notice.rows(look, content);
        }
        self.status
            .iter()
            .flat_map(|status| status.events.notifications.iter())
            .find(|item| item.seen_ms.is_none())
            .map(|item| Row::of(title(&item.text), look.warning))
            .into_iter()
            .collect()
    }

    /// The help keys of the view in its current state, most used first.
    /// Takes the view. Returns key and label pairs; Esc goes back to the Team screen
    /// once nothing inside the view is open.
    pub fn help(&self) -> Help {
        if let Some(list) = &self.messages {
            return list.help();
        }
        if self.message_row.is_some() {
            return vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Space", "Next choice"),
                ("\u{2190}\u{2192}", "Choice"),
                ("Esc", "Close"),
            ];
        }
        if self.draft.is_some() {
            return vec![
                ("Enter", "Hand over"),
                ("Ctrl-J", "New line"),
                ("Tab", "Sub-agent"),
                ("Esc", "Close"),
            ];
        }
        if let Some(ask) = self.ask {
            return match ask {
                Ask::Teammate(_) => vec![("1-9", "Stop teammate"), ("Esc", "Keep")],
                Ask::StopAll | Ask::StopAgent(_) => vec![("y", "Stop"), ("n", "Keep running")],
            };
        }
        if self.branch_edit.is_some() {
            return vec![("Enter", "Save"), ("Esc", "Close")];
        }
        if self.master_pick.is_some() {
            return vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Enter", "Save"),
                ("Esc", "Close"),
            ];
        }
        if self.confirm.is_some() {
            return vec![("y", "Cancel task"), ("n", "Keep")];
        }
        if let Some(edit) = &self.coord {
            if edit.input.is_some() {
                return vec![("Enter", "Save"), ("Esc", "Close")];
            }
            let mut help = vec![("\u{2191}\u{2193}", "Move")];
            match COORD_ROWS[edit.row] {
                CoordRow::Counted => {
                    help.extend([("Enter", "Edit tools"), ("r", "Default tools")]);
                }
                CoordRow::Skills(_) => {
                    help.extend([("Space", "All or chosen"), ("Enter", "Edit skills")]);
                }
                CoordRow::Resources => help.push(("Enter", "Edit resources")),
                _ => help.extend([("Space", "Change"), ("\u{2190}\u{2192}", "Change")]),
            }
            help.push(("Esc", "Close"));
            return help;
        }
        if self.wake.is_some() {
            return vec![
                ("\u{2191}\u{2193}", "Move"),
                ("Space", "Switch"),
                ("\u{2190}\u{2192}", "Limit"),
                ("Tab", "Level"),
                ("r", "Reset"),
                ("Esc", "Close"),
            ];
        }
        let unseen = self.status.as_ref().is_some_and(|status| {
            status
                .events
                .notifications
                .iter()
                .any(|item| item.seen_ms.is_none())
        });
        if self.alerts {
            let mut help = Vec::new();
            if unseen {
                help.push(("s", "Seen"));
            }
            help.push(("Esc", "Close"));
            return help;
        }
        let mut help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Tab", "Agents or tasks"),
            ("n", "New task"),
            ("x", "Cancel task"),
            ("w", "Keep warm"),
            ("t", "Task limit"),
            ("l", "Session limit"),
            ("i", "Idle stop"),
            ("h", "On harness switch"),
            ("H", "Master harness"),
            ("A", "Apply master change"),
            ("d", "Sub-agent depth"),
            ("k", "Worker lands"),
            ("b", "Agent branches"),
            ("v", "Max harnesses"),
            ("u", "Max teammates"),
            ("U", "Sub-agent rounds"),
            ("N", "Master run time"),
            ("T", "Token budget"),
            ("P", "Spread accounts"),
            ("z", "Stop agent"),
            ("p", "Stop teammate"),
            (
                "S",
                if self.status.as_ref().is_some_and(|status| status.paused) {
                    "Start all"
                } else {
                    "Stop all"
                },
            ),
            ("m", "Master's branch"),
            ("L", "Land on"),
            ("g", "Master accepts change sets"),
            ("E", "Master merges"),
            ("R", "Re-ask empty reply"),
            ("D", "End a turn after done"),
            ("I", "Stop the master when idle after"),
            ("V", "Live terminal"),
            ("F", "New folder limit"),
            ("W", "Terminals per window"),
            ("B", "Background commands"),
            ("O", "Left running after a stop"),
            ("G", "Grace time"),
            ("c", "Coordination"),
            ("e", "Wake rules"),
            ("M", "Messages"),
            ("o", "Messages settings"),
        ];
        if self
            .status
            .as_ref()
            .is_some_and(|status| !status.events.notifications.is_empty())
        {
            help.push(("a", "Notifications"));
        }
        if unseen {
            help.push(("s", "Seen"));
        }
        if self
            .status
            .as_ref()
            .is_some_and(|status| !master_runs_live(status))
        {
            help.retain(|(key, _)| *key != "V");
        }
        help.push(("Esc", "Back"));
        help
    }
}
