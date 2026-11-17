use super::*;

impl TeamView {
    /// Opens the coordination editor on its first row. Output: none; a notice while the team
    /// is loading or when the engine sends no coordination settings.
    pub(super) fn open_coord(&mut self) {
        match &self.status {
            None => self.notice = Some(Notice::plain("The team is still loading.")),
            Some(status) if status.coordination.is_none() => {
                self.notice = Some(Notice::plain(
                    "This engine does not report coordination settings yet.",
                ));
            }
            Some(_) => {
                self.notice = None;
                self.coord = Some(CoordEdit {
                    row: 0,
                    input: None,
                });
            }
        }
    }

    /// Handles one key while the coordination editor is open: move, change the row's value,
    /// edit or reset the counted tools, switch a role between All approved skills and Chosen
    /// skills or type its skill names, or close. Takes the key. Output: the change to send,
    /// or Handled.
    pub(super) fn coord_key(&mut self, key: KeyEvent) -> TeamReply {
        let Some(edit) = self.coord.clone() else {
            return TeamReply::Ignored;
        };
        if edit.input.is_some() {
            return match key.code {
                KeyCode::Esc => {
                    self.coord = Some(CoordEdit {
                        input: None,
                        ..edit
                    });
                    TeamReply::Handled
                }
                KeyCode::Enter => match COORD_ROWS[edit.row] {
                    CoordRow::Skills(role) => self.save_skills(role),
                    CoordRow::Resources => self.save_resources(),
                    _ => self.save_counted(),
                },
                _ => {
                    if let Some(input) = self.coord.as_mut().and_then(|edit| edit.input.as_mut()) {
                        input.handle(key);
                    }
                    TeamReply::Handled
                }
            };
        }
        let Some(view) = self
            .status
            .as_ref()
            .and_then(|status| status.coordination.clone())
        else {
            self.coord = None;
            return TeamReply::Handled;
        };
        let row = COORD_ROWS[edit.row];
        match key.code {
            KeyCode::Esc => {
                self.coord = None;
                TeamReply::Handled
            }
            KeyCode::Up | KeyCode::Down => {
                let next = if key.code == KeyCode::Down {
                    (edit.row + 1).min(COORD_ROWS.len() - 1)
                } else {
                    edit.row.saturating_sub(1)
                };
                self.coord = Some(CoordEdit {
                    row: next,
                    input: None,
                });
                TeamReply::Handled
            }
            KeyCode::Enter | KeyCode::Char(' ') if row == CoordRow::Counted => {
                let mut input = TextInput::single(COUNTED_TEXT_BYTES_MAX);
                input.set(&view.settings.counted_tools.join(", "));
                self.coord = Some(CoordEdit {
                    row: edit.row,
                    input: Some(input),
                });
                TeamReply::Handled
            }
            KeyCode::Enter | KeyCode::Char(' ') if row == CoordRow::Resources => {
                let mut input = TextInput::single(RESOURCES_TEXT_BYTES_MAX);
                input.set(&resources_text(&view.settings.shared_resources));
                self.coord = Some(CoordEdit {
                    row: edit.row,
                    input: Some(input),
                });
                TeamReply::Handled
            }
            KeyCode::Char('r') if row == CoordRow::Counted => {
                let mut settings = view.settings;
                settings.counted_tools = COUNTED_TOOLS_DEFAULT.map(str::to_owned).to_vec();
                self.coord_change(
                    TeamAction::CoordSettings {
                        project_id: self.project.clone(),
                        settings,
                    },
                    "The counted tools are back to the defaults.".to_owned(),
                )
            }
            KeyCode::Enter if matches!(row, CoordRow::Skills(_)) => {
                let CoordRow::Skills(role) = row else {
                    return TeamReply::Handled;
                };
                self.type_skills(edit.row, view.settings.role_skills.of(role));
                TeamReply::Handled
            }
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Right => {
                self.coord_step(row, &view, true)
            }
            KeyCode::Left => self.coord_step(row, &view, false),
            _ => TeamReply::Handled,
        }
    }

    /// Moves one coordination row to its next or previous choice and sends the change.
    /// Native is offered only for a harness kind with native sub-agents.
    /// Takes the row, the current settings and the direction. Output: the change, or Handled.
    fn coord_step(&mut self, row: CoordRow, view: &CoordinationView, forward: bool) -> TeamReply {
        let project_id = self.project.clone();
        let harness = |kind: HarnessKind| view.harnesses.iter().find(|row| row.harness == kind);
        let mut settings = view.settings.clone();
        if let CoordRow::Mode(kind) | CoordRow::Delivery(kind) = row
            && let Some(reason) = crate::team::sub_agents_unavailable(kind)
        {
            let label = harness_row_label(matches!(row, CoordRow::Delivery(_)), kind);
            self.notice = Some(Notice::plain(format!("{label} - {reason}")));
            return TeamReply::Handled;
        }
        let (action, done) = match row {
            CoordRow::Mode(kind) => {
                let Some(current) = harness(kind) else {
                    return TeamReply::Handled;
                };
                let name = harness_name(kind.as_str(), None);
                if !current.native {
                    self.notice = Some(Notice::plain(native_unavailable(kind)));
                    return TeamReply::Handled;
                }
                let mode = match current.mode {
                    SubagentMode::AppSpawned => SubagentMode::Native,
                    SubagentMode::Native => SubagentMode::AppSpawned,
                };
                (
                    TeamAction::HarnessMode {
                        project_id,
                        harness: kind,
                        mode,
                    },
                    match (mode, native_scope(kind)) {
                        (SubagentMode::Native, Some(scope)) => {
                            format!(
                                "{name} sub-agents are now {}. {scope}",
                                mode_words(kind, mode)
                            )
                        }
                        _ => format!("{name} sub-agents are now {}.", mode_words(kind, mode)),
                    },
                )
            }
            CoordRow::Delivery(kind) => {
                let Some(current) = harness(kind) else {
                    return TeamReply::Handled;
                };
                let delivery = cycle(&DELIVERIES, current.delivery, forward);
                (
                    TeamAction::HarnessDelivery {
                        project_id,
                        harness: kind,
                        delivery,
                    },
                    format!(
                        "Messages to {} agents: {}. {}",
                        harness_name(kind.as_str(), None),
                        delivery_words(delivery),
                        delivery_scope(kind)
                    ),
                )
            }
            CoordRow::TerminalDisplay => {
                settings.terminal_display = match settings.terminal_display {
                    TerminalDisplay::Classic => TerminalDisplay::Fullscreen,
                    TerminalDisplay::Fullscreen => TerminalDisplay::Classic,
                };
                let done = format!(
                    "Terminals now open {}.{}",
                    display_words(settings.terminal_display),
                    display_kept(view, settings.terminal_display)
                );
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done,
                )
            }
            CoordRow::Questions => {
                settings.questions_to = match settings.questions_to {
                    QuestionsTo::Lead => QuestionsTo::Me,
                    QuestionsTo::Me => QuestionsTo::Lead,
                };
                let done = match settings.questions_to {
                    QuestionsTo::Lead => "Sub-agent questions now go to their lead.",
                    QuestionsTo::Me => "Sub-agent questions now come to you directly.",
                };
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done.to_owned(),
                )
            }
            CoordRow::Overlaps => {
                settings.overlaps = cycle(&OVERLAPS, settings.overlaps, forward);
                let done = format!(
                    "Overlapping edits: {}.",
                    overlaps_words(settings.overlaps).to_lowercase()
                );
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done,
                )
            }
            CoordRow::CatchUpMain => {
                let mode = match self.status.as_ref().map(|status| status.catch_up_main) {
                    Some(CatchUpMain::Off) => CatchUpMain::On,
                    Some(CatchUpMain::On) | None => CatchUpMain::Off,
                };
                let done = match mode {
                    CatchUpMain::On => {
                        "Catch up with main is on for agents started from now on: they may git merge crazyagent/main."
                    }
                    CatchUpMain::Off => {
                        "Catch up with main is off for agents started from now on: the git the app writes for an agent refuses git merge crazyagent/main, except a master at Full access. It is a guide, not a lock: landed work can still come in another way."
                    }
                };
                (
                    TeamAction::CatchUpMain { project_id, mode },
                    done.to_owned(),
                )
            }
            CoordRow::ReuseIdle => {
                let mode = match self.status.as_ref().map(|status| status.reuse_idle) {
                    Some(ReuseIdle::On) => ReuseIdle::Off,
                    Some(ReuseIdle::Off) | None => ReuseIdle::On,
                };
                let done = match mode {
                    ReuseIdle::On => {
                        "Reuse idle agents is on: an idle agent may take the next task, on the history it already has."
                    }
                    ReuseIdle::Off => {
                        "Reuse idle agents is off: each new task starts a new agent on the current branch."
                    }
                };
                (TeamAction::ReuseIdle { project_id, mode }, done.to_owned())
            }
            CoordRow::LandingMode => {
                let mode = match self.status.as_ref().map(|status| status.landing_mode) {
                    Some(LandingMode::Merge) => LandingMode::Squash,
                    Some(LandingMode::Squash) | None => LandingMode::Merge,
                };
                let done = match mode {
                    LandingMode::Merge => {
                        "Landing is Merge commit: accepted changes land as a merge commit that keeps every agent commit in your history."
                    }
                    LandingMode::Squash => {
                        "Landing is Squash: each accepted change set lands as one commit."
                    }
                };
                (
                    TeamAction::LandingMode { project_id, mode },
                    done.to_owned(),
                )
            }
            CoordRow::Nudges => {
                settings.nudges = cycle(&Nudges::ALL, settings.nudges, forward);
                let done = format!(
                    "Workflow nudges: {}.",
                    settings.nudges.words().to_lowercase()
                );
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done,
                )
            }
            CoordRow::TaskStart => {
                settings.task_start = cycle(&TaskStart::ALL, settings.task_start, forward);
                let done = format!(
                    "Task-start reminder: {}.",
                    settings.task_start.words().to_lowercase()
                );
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done,
                )
            }
            CoordRow::HeavyLane => {
                settings.heavy_lane = cycle(&HeavyLane::ALL, settings.heavy_lane, forward);
                let done = format!(
                    "Heavy lane: {}.",
                    settings.heavy_lane.words().to_lowercase()
                );
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    done,
                )
            }
            CoordRow::Counted | CoordRow::Resources => return TeamReply::Handled,
            CoordRow::Skills(role) => {
                if settings.role_skills.of(role).is_none() {
                    if let Some(at) = self.coord.as_ref().map(|edit| edit.row) {
                        self.type_skills(at, None);
                    }
                    return TeamReply::Handled;
                }
                settings.role_skills.set(role, None);
                (
                    TeamAction::CoordSettings {
                        project_id,
                        settings,
                    },
                    format!("{} skills: all approved skills.", role_label(role)),
                )
            }
        };
        self.coord_change(action, done)
    }

    /// Opens the input for one role's skill names on its row, holding the role's chosen
    /// names, empty while it gets All approved skills. Takes the row and the choice.
    fn type_skills(&mut self, at: usize, chosen: Option<&[String]>) {
        assert!(matches!(COORD_ROWS[at], CoordRow::Skills(_)));
        let mut input = TextInput::single(SKILL_NAMES_TEXT_BYTES_MAX);
        input.set(&chosen.unwrap_or_default().join(", "));
        self.coord = Some(CoordEdit {
            row: at,
            input: Some(input),
        });
    }

    /// Sends the skill names typed for one role as its Chosen skills: names split on commas
    /// or spaces, each kept once in order, checked with the engine's rule first; an empty
    /// list gives the role no skills. Takes the role. Output: the change, or Handled with
    /// the reason the names can't be saved.
    fn save_skills(&mut self, role: SkillRole) -> TeamReply {
        let Some(typed) = self
            .coord
            .as_ref()
            .and_then(|edit| edit.input.as_ref())
            .map(|input| input.value().to_owned())
        else {
            return TeamReply::Handled;
        };
        let Some(mut settings) = self
            .status
            .as_ref()
            .and_then(|status| status.coordination.as_ref())
            .map(|view| view.settings.clone())
        else {
            return TeamReply::Handled;
        };
        let mut names: Vec<String> = Vec::new();
        for name in typed
            .split([',', ' ', '\t'])
            .filter(|name| !name.is_empty())
        {
            if !names.iter().any(|kept| kept == name) {
                names.push(name.to_owned());
            }
        }
        let done = match names.is_empty() {
            true => format!("{} skills: none.", role_label(role)),
            false => format!("{} skills: {}.", role_label(role), names.join(", ")),
        };
        settings.role_skills.set(role, Some(names));
        if settings.validate().is_err() {
            self.notice = Some(Notice::problem(team_plain("coord_settings_invalid")));
            return TeamReply::Handled;
        }
        self.coord_change(
            TeamAction::CoordSettings {
                project_id: self.project.clone(),
                settings,
            },
            done,
        )
    }

    /// Sends the counted tools typed in the editor: names split on commas or spaces, each kept
    /// once in order, checked with the engine's own rule first. Output: the change, or Handled
    /// with the reason the list can't be saved.
    fn save_counted(&mut self) -> TeamReply {
        let Some(typed) = self
            .coord
            .as_ref()
            .and_then(|edit| edit.input.as_ref())
            .map(|input| input.value().to_owned())
        else {
            return TeamReply::Handled;
        };
        let Some(mut settings) = self
            .status
            .as_ref()
            .and_then(|status| status.coordination.as_ref())
            .map(|view| view.settings.clone())
        else {
            return TeamReply::Handled;
        };
        let mut tools: Vec<String> = Vec::new();
        for name in typed
            .split([',', ' ', '\t'])
            .filter(|name| !name.is_empty())
        {
            if !tools.iter().any(|kept| kept == name) {
                tools.push(name.to_owned());
            }
        }
        settings.counted_tools = tools;
        if settings.validate().is_err() {
            self.notice = Some(Notice::problem(team_plain("coord_settings_invalid")));
            return TeamReply::Handled;
        }
        self.coord_change(
            TeamAction::CoordSettings {
                project_id: self.project.clone(),
                settings,
            },
            "Counted tools saved.".to_owned(),
        )
    }

    /// Sends the shared resources typed on their row (F28): `name` or `name:slots` entries
    /// split on commas or spaces, one slot when none is given, checked with the engine's
    /// rule first; an empty line removes them all. Output: the change, or Handled with the
    /// reason they can't be saved.
    fn save_resources(&mut self) -> TeamReply {
        let Some(typed) = self
            .coord
            .as_ref()
            .and_then(|edit| edit.input.as_ref())
            .map(|input| input.value().to_owned())
        else {
            return TeamReply::Handled;
        };
        let Some(mut settings) = self
            .status
            .as_ref()
            .and_then(|status| status.coordination.as_ref())
            .map(|view| view.settings.clone())
        else {
            return TeamReply::Handled;
        };
        let Some(resources) = resources_typed(&typed) else {
            self.notice = Some(Notice::problem(team_plain("coord_settings_invalid")));
            return TeamReply::Handled;
        };
        settings.shared_resources = resources;
        if settings.validate().is_err() {
            self.notice = Some(Notice::problem(team_plain("coord_settings_invalid")));
            return TeamReply::Handled;
        }
        self.coord_change(
            TeamAction::CoordSettings {
                project_id: self.project.clone(),
                settings,
            },
            "Shared resources saved.".to_owned(),
        )
    }

    fn coord_change(&mut self, action: TeamAction, done: String) -> TeamReply {
        if self.still_saving() {
            return TeamReply::Handled;
        }
        self.changing = Some(done);
        TeamReply::Change(action)
    }

    /// Draws the coordination editor: one row per setting with its current choice, and the
    /// counted tools input while it is open. Takes the edit, the content width and the look.
    /// Returns the rows and, while typing, the cursor as a row and a column inside them.
    pub(super) fn coord_rows(
        &self,
        edit: &CoordEdit,
        content: usize,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>) {
        let Some(view) = self
            .status
            .as_ref()
            .and_then(|status| status.coordination.as_ref())
        else {
            return (Vec::new(), None);
        };
        let mut rows = vec![spread(
            content,
            Row::of("Coordination", look.heading),
            Row::of("This project", look.muted),
        )];
        for (index, row) in COORD_ROWS.into_iter().enumerate() {
            let found = |kind: HarnessKind| view.harnesses.iter().find(|row| row.harness == kind);
            let (label, value, note) = match row {
                CoordRow::Mode(kind) | CoordRow::Delivery(kind)
                    if crate::team::sub_agents_unavailable(kind).is_some() =>
                {
                    (
                        harness_row_label(matches!(row, CoordRow::Delivery(_)), kind),
                        crate::team::NO_SUB_AGENTS.to_owned(),
                        "",
                    )
                }
                CoordRow::Mode(kind) => (
                    harness_row_label(false, kind),
                    found(kind)
                        .map_or("", |row| mode_words(kind, row.mode))
                        .to_owned(),
                    match found(kind) {
                        Some(row) if !row.native => "  no native sub-agents",
                        Some(row)
                            if row.mode == SubagentMode::Native && native_scope(kind).is_some() =>
                        {
                            "  shown in the lead's terminal only"
                        }
                        _ => "",
                    },
                ),
                CoordRow::Delivery(kind) => (
                    harness_row_label(true, kind),
                    found(kind)
                        .map_or("", |row| delivery_words(row.delivery))
                        .to_owned(),
                    "",
                ),
                CoordRow::TerminalDisplay => {
                    let words = display_words(view.settings.terminal_display);
                    let kept = display_kept(view, view.settings.terminal_display);
                    (
                        "Terminal display".to_owned(),
                        if kept.is_empty() {
                            words.to_owned()
                        } else {
                            format!("{words}.{kept}")
                        },
                        "",
                    )
                }
                CoordRow::Questions => (
                    "Sub-agent questions go to".to_owned(),
                    match view.settings.questions_to {
                        QuestionsTo::Lead => "Their lead",
                        QuestionsTo::Me => "Me directly",
                    }
                    .to_owned(),
                    "",
                ),
                CoordRow::Overlaps => (
                    "Overlapping edits".to_owned(),
                    overlaps_words(view.settings.overlaps).to_owned(),
                    match view.settings.overlaps {
                        Overlaps::Block | Overlaps::Wait => OVERLAPS_HOLD_NOTE,
                        Overlaps::Warn | Overlaps::Off => "",
                    },
                ),
                CoordRow::CatchUpMain => (
                    "Catch up with main".to_owned(),
                    match self.status.as_ref().map(|status| status.catch_up_main) {
                        Some(CatchUpMain::Off) => "Off",
                        Some(CatchUpMain::On) | None => "On",
                    }
                    .to_owned(),
                    "",
                ),
                CoordRow::ReuseIdle => (
                    "Reuse idle agents".to_owned(),
                    match self.status.as_ref().map(|status| status.reuse_idle) {
                        Some(ReuseIdle::On) => "On",
                        Some(ReuseIdle::Off) | None => "Off",
                    }
                    .to_owned(),
                    "",
                ),
                CoordRow::LandingMode => (
                    "Landing".to_owned(),
                    match self.status.as_ref().map(|status| status.landing_mode) {
                        Some(LandingMode::Merge) => "Merge commit",
                        Some(LandingMode::Squash) | None => "Squash",
                    }
                    .to_owned(),
                    "",
                ),
                CoordRow::Nudges => (
                    "Workflow nudges".to_owned(),
                    view.settings.nudges.words().to_owned(),
                    "",
                ),
                CoordRow::TaskStart => (
                    "Task-start reminder".to_owned(),
                    view.settings.task_start.words().to_owned(),
                    "",
                ),
                CoordRow::Counted => (
                    "Counted tools".to_owned(),
                    view.settings.counted_tools.join(", "),
                    "",
                ),
                CoordRow::HeavyLane => (
                    "Heavy lane".to_owned(),
                    format!(
                        "{}  {} running, {} waiting",
                        view.settings.heavy_lane.words(),
                        view.lane.running.len(),
                        view.lane.waiting.len()
                    ),
                    "",
                ),
                CoordRow::Resources => (
                    "Shared resources".to_owned(),
                    if view.settings.shared_resources.is_empty() {
                        "None".to_owned()
                    } else {
                        resources_text(&view.settings.shared_resources)
                    },
                    "",
                ),
                CoordRow::Skills(role) => (
                    format!("{} skills", role_label(role)),
                    match view.settings.role_skills.of(role) {
                        None => "All approved skills".to_owned(),
                        Some([]) => "Chosen skills: none".to_owned(),
                        Some(names) => format!("Chosen skills: {}", names.join(", ")),
                    },
                    "",
                ),
            };
            rows.push(list_row(
                Row::of(label, look.fg)
                    .pad(32)
                    .push(value, look.heading)
                    .push(note, look.muted),
                content,
                edit.row == index,
                true,
                look,
            ));
        }
        let Some(input) = &edit.input else {
            return (rows, None);
        };
        let inner = content.saturating_sub(4).max(2);
        let (lines, (line, column)) = input.wrap(inner - 1);
        let text = if input.value().is_empty() {
            Row::of(
                match COORD_ROWS[edit.row] {
                    CoordRow::Skills(_) => "No skills: type names separated by commas",
                    CoordRow::Resources => {
                        "No shared resources: type name or name:slots, such as ollama:1"
                    }
                    _ => "No tools counted",
                },
                look.muted,
            )
        } else {
            Row::of(lines.get(line).cloned().unwrap_or_default(), look.fg)
        };
        let cursor = (rows.len(), 2 + column);
        rows.push(
            Row::of("[", look.accent)
                .push(" ", Style::new())
                .cat(text.pad(inner))
                .push(" ", Style::new())
                .push("]", look.accent),
        );
        assert!(cursor.0 < rows.len());
        (rows, Some(cursor))
    }
}
