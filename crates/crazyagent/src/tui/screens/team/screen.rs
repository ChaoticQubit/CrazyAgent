use super::*;

impl Screen for Team {
    fn crumbs(&self) -> Vec<String> {
        let page = if self.master { "Master" } else { "Team" };
        vec![self.name.clone(), page.into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.terminal_keys() && self.master {
            let overdue = self.overdue();
            let mut hints = vec![hint("Ctrl-]", "Leave terminal")];
            if !overdue {
                hints.push(hint("Ctrl-J", "New line"));
            }
            if self.idle_stopped() {
                hints.insert(0, hint("s", "Start again"));
            } else if self.try_again() {
                hints.insert(0, hint("s", "Try again"));
            }
            if self.starting() || overdue {
                hints.push(hint("Esc", "Back"));
            }
            if ctx.shift_arrows == ShiftArrows::SwitchTabs {
                hints.push(hint(
                    &format!("Shift-{}{}", ctx.glyphs.left, ctx.glyphs.right),
                    "Tabs",
                ));
            }
            return hints;
        }
        if self.terminal_keys() {
            return vec![hint("Ctrl-]", "Back to the team")];
        }
        if self.master {
            let mut hints = Vec::new();
            if self.paused {
                hints.push(hint("S", "Start all"));
            }
            if self.idle_stopped() {
                hints.push(hint("s", "Start again"));
            } else if self.try_again() {
                hints.push(hint("s", "Try again"));
            }
            if !self.overdue()
                && self
                    .attached
                    .as_ref()
                    .is_some_and(|attached| attached.closed.is_none())
            {
                hints.push(hint("Enter", "Terminal"));
            }
            hints.push(hint("n", "New session"));
            hints.push(hint("l", "Sessions"));
            hints.push(hint("Esc", "Back"));
            hints.push(hint("r", "Refresh"));
            return hints;
        }
        if let Some(view) = &self.settings {
            return view
                .help()
                .into_iter()
                .map(|(key, label)| hint(key, label))
                .collect();
        }
        if let Some(list) = &self.messages {
            return list
                .help()
                .into_iter()
                .map(|(key, label)| hint(key, label))
                .collect();
        }
        if self.cleanup.open() {
            return self.cleanup.hints();
        }
        if self.finished.open() {
            return self.finished.hints();
        }
        if self.questions.open() {
            return self.questions.hints(ctx);
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = vec![
            hint(&vertical, "Move"),
            hint("Enter", "Open"),
            hint("i", "Instructions"),
            hint("m", "Messages"),
        ];
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.closed.is_none())
        {
            hints.push(hint("Tab", "Terminal"));
        }
        hints.push(hint("r", "Refresh"));
        match self.instructions {
            Some(_) => {
                hints.push(hint("PgUp/PgDn", "Scroll"));
                hints.push(hint("Esc", "Close"));
            }
            None => {
                let questions = match self.questions.counts() {
                    Some((asked, waiting)) if asked + waiting > 0 => {
                        format!("Questions ({})", asked + waiting)
                    }
                    _ => "Questions".to_owned(),
                };
                hints.push(hint("q", &questions));
                hints.push(hint("f", "Finished"));
                hints.push(hint("c", "Clean up"));
                hints.push(hint("Esc", "Back"));
            }
        }
        hints.push(hint("s", "Settings"));
        hints
    }

    fn raw(&self) -> bool {
        self.terminal_keys()
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        if !self.master {
            return Flow::Ignored;
        }
        self.step_tab(forward, ctx)
    }

    fn typing(&self) -> bool {
        self.settings.as_ref().is_some_and(TeamView::typing) || self.questions.typing()
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.focus == Focus::Terminal {
            return self.terminal_key(key, ctx);
        }
        if self.master {
            return self.master_key(key, ctx);
        }
        if self.settings.is_some() {
            return self.settings_key(key, ctx);
        }
        if let Some(list) = self.messages.as_mut() {
            if list.key(key) == ListKey::Closed {
                self.messages = None;
            }
            self.ask_messages(ctx);
            return Flow::Handled;
        }
        if !self.cleanup.open()
            && !self.finished.open()
            && let Some(took) = self.questions.key(key, &self.project, ctx)
        {
            if let questions::Took::Open(agent) = took {
                self.open_agent(&agent, ctx);
            }
            return Flow::Handled;
        }
        if !self.cleanup.open()
            && let Some(flow) = self.finished.key(key, &self.project, ctx)
        {
            return flow;
        }
        if let Some(flow) = self.cleanup.key(key, &self.project, ctx) {
            return flow;
        }
        let items = self.items(ctx.glyphs);
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(items.len().saturating_sub(1)),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = items.len().saturating_sub(1),
            Key::Enter => match items.get(self.cursor) {
                Some(Item::Node(node)) => self.open(*node, ctx),
                Some(Item::Edge(edge)) => self.show_task(*edge),
                None => {}
            },
            Key::Tab => {
                if self
                    .attached
                    .as_ref()
                    .is_some_and(|attached| attached.closed.is_none())
                {
                    self.task = None;
                    self.instructions = None;
                    self.focus = Focus::Terminal;
                }
            }
            Key::Char('i') => {
                if let Some(Item::Node(node)) = items.get(self.cursor) {
                    self.read_instructions(*node, ctx);
                }
            }
            Key::Char('m') => {
                if let Some(Item::Node(node)) = items.get(self.cursor) {
                    self.open_messages(*node, ctx);
                }
            }
            Key::PageDown if self.instructions.is_some() => {
                if let Some(open) = self.instructions.as_mut() {
                    open.top = open.top.saturating_add(PAGE_STEP);
                }
            }
            Key::PageUp if self.instructions.is_some() => {
                if let Some(open) = self.instructions.as_mut() {
                    open.top = open.top.saturating_sub(PAGE_STEP);
                }
            }
            Key::Char('r') => ctx.request(Request::TeamGraph {
                project: self.project.clone(),
            }),
            Key::Char('s') => self.open_settings(ctx),
            Key::Esc if self.instructions.is_some() => self.instructions = None,
            Key::Esc => {
                self.detach(ctx);
                return Flow::Back;
            }
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        self.cleanup.arrive(arrival, &self.project, ctx);
        self.finished.arrive(arrival, &self.project, ctx);
        self.questions.arrive(arrival, &self.project, ctx);
        if let Some(list) = self.messages.as_mut() {
            match arrival {
                Arrival::Owner(event) => {
                    if let OwnerEvent::Setup(event) = event.as_ref() {
                        list.apply(&event.query, &event.result);
                    }
                }
                Arrival::Disconnected(_) => list.forget_reads(),
                _ => {}
            }
        }
        match arrival {
            Arrival::TeamGraph { project, result } if *project == self.project => {
                self.take_graph(result, ctx.glyphs);
                self.end_stopped(ctx);
                self.note_master_agent(ctx);
                self.note_master_stopped(ctx);
                self.follow_master(ctx);
                if !self.master {
                    questions::Questions::read(&self.project, ctx);
                }
            }
            Arrival::Terminal { agent, frame } => self.stream(agent, frame, ctx),
            Arrival::Owner(event) => match event.as_ref() {
                OwnerEvent::Process(event) => self.take_instructions(event),
                OwnerEvent::Setup(event) => {
                    if self.live_terminal_off(event) {
                        self.detach(ctx);
                        ctx.say(Message::warning(LIVE_TERMINAL_OFF, LIVE_TERMINAL_HOW));
                        return Flow::Back;
                    }
                    if self.no_live_terminal(event) {
                        self.detach(ctx);
                        ctx.say(Message::warning(super::project_team::NO_LIVE_TERMINAL, ""));
                        return Flow::Back;
                    }
                    self.took_setup(&event.query, &event.result, ctx);
                    if let Some(view) = self.settings.as_mut() {
                        view.apply(&event.query, event.result.clone());
                    }
                }
                OwnerEvent::Settings(event) => {
                    if let (SettingsEvent::Harnesses(result), Some(view)) =
                        (event.as_ref(), self.settings.as_mut())
                    {
                        view.take_catalog(result);
                    }
                }
                _ => {}
            },
            Arrival::Disconnected(_) => {
                if let Some(view) = self.settings.as_mut() {
                    view.forget_reads();
                }
            }
            _ => {}
        }
        self.ask_messages(ctx);
        self.time_wait();
        if !self.starting() {
            self.drop_held(ctx);
        }
        self.pump_settings(ctx);
        self.sync(ctx);
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if self.master {
            self.read_switch(ctx);
        }
        self.pump_settings(ctx);
        ctx.request(Request::TeamGraph {
            project: self.project.clone(),
        });
        self.reattach(ctx);
        self.time_wait();
        self.sync(ctx);
    }

    fn resized(&mut self, ctx: &mut Ctx) {
        self.sync(ctx);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        if self.master {
            let (head, body) = master_split(whole(area));
            let line = self.master_head(usize::from(head.width), ctx);
            draw_lines(frame.buffer_mut(), head, &[line]);
            return self.draw_right(frame, body, ctx);
        }
        if let Some(view) = &self.settings {
            let area = whole(area);
            let inner = inside(area);
            let accounts = ctx.store.accounts.ready().map_or(&[][..], Vec::as_slice);
            let (rows, _, caret) = view.draw(
                usize::from(inner.width),
                usize::from(inner.height),
                accounts,
                &look(ctx),
            );
            let lines = rows.into_iter().map(Row::line).collect();
            boxed(frame, area, "Team settings", true, lines, ctx);
            let (row, column) = caret?;
            let (row, column) = (u16::try_from(row).ok()?, u16::try_from(column).ok()?);
            return (row < inner.height && column < inner.width)
                .then(|| Position::new(inner.x + column, inner.y + row));
        }
        let (left, right) = split(area);
        let inner = inside(left);
        let lines = self.tree_lines(usize::from(inner.width), usize::from(inner.height), ctx);
        boxed(frame, left, "Team", self.focus == Focus::Tree, lines, ctx);
        self.draw_right(frame, right, ctx)
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        matches!(self.graph, Load::Idle | Load::Loading)
            || self.settings.as_ref().is_some_and(TeamView::busy)
    }

    fn draft(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|view| view.draft.is_some())
    }

    fn loss(&self) -> &'static str {
        "The task you are writing will be lost."
    }

    fn place(&self) -> Option<Place> {
        let (id, name) = (self.project.clone(), self.name.clone());
        Some(if self.master {
            Place::Master { id, name }
        } else {
            Place::Team { id, name }
        })
    }
}
