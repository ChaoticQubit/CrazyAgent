use super::*;

impl Screen for Harnesses {
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        vec!["Settings".into(), "Harnesses".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let again = if self.blocked_selected() {
            TRY_AGAIN
        } else {
            "Check again"
        };
        let mut hints = vec![hint(&arrows, "Move"), hint("r", again)];
        let mode = self.mode();
        let missing = self
            .entries
            .get(self.selected)
            .filter(|entry| entry.executable.is_none());
        if let Some(entry) = missing {
            let offered = self
                .versions
                .get(&entry.id)
                .is_none_or(|versions| versions.install.is_some());
            if offered {
                hints.push(hint("i", "Install"));
            }
        }
        if let Some(id) = self.choosable() {
            hints.push(hint("f", "Choose file"));
            hints.push(hint("w", "Where it looked"));
            if matches!(self.found.get(&id), Some(Found::Refused { .. })) {
                hints.push(hint("x", "Forget the chosen file"));
            }
        }
        if self.chosen().is_some() && mode != UpdateMode::Off {
            hints.push(hint("c", "Check for updates"));
            if update_blocked(mode).is_none() {
                hints.push(hint("u", "Update"));
            }
        }
        let offered = matches!(
            self.node_offer(),
            NodeOffer::Offer | NodeOffer::Failed { .. }
        );
        if offered && !self.sending_node {
            hints.push(hint("n", INSTALL_NODE));
        }
        hints.extend([sections_hint(ctx.glyphs), hint("Esc", "Home")]);
        hints
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.confirm.is_some() {
            self.confirmed(key, ctx);
            return Flow::Handled;
        }
        if self.choosing.is_some() {
            self.chose(key, ctx);
            return Flow::Handled;
        }
        if self.looked.take().is_some() {
            return Flow::Handled;
        }
        let page = self.window(ctx).saturating_sub(1).max(1) as isize;
        let choosable = self.choosable().is_some();
        match key {
            Key::Char('r') => {
                self.found.clear();
                self.reload(ctx);
                self.read_where(
                    |entry, versions| needs_line(entry, Some(versions)).is_some(),
                    ctx,
                );
                self.reread_node(ctx);
            }
            Key::Char('n') => self.install_node(ctx),
            Key::Char('c') => self.check(ctx),
            Key::Char('u') => self.update(ctx),
            Key::Char('i') => self.install(ctx),
            Key::Char('f') if choosable => self.open_choose(),
            Key::Char('w') if choosable => self.find(ctx),
            Key::Char('x') if choosable => self.forget(ctx),
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-page),
            Key::PageDown => self.move_by(page),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.entries.len().saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        step(SettingsSection::Harnesses, forward, ctx)
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let result = match settings_event(arrival) {
            Some(SettingsEvent::Harnesses(result)) => result,
            Some(SettingsEvent::HarnessVersions(id, result)) => {
                self.arrive_versions(id, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessUpdated(id, result)) => {
                self.arrive_started(id, Job::Update, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessInstalled(id, result)) => {
                self.arrive_started(id, Job::Install, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessFound(id, result)) => {
                self.arrive_found(id, None, result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessChosen(id, path, result)) => {
                self.arrive_found(id, Some(path), result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::AppSetup(AppSetupEvent::Status(result))) => {
                self.arrive_setup(result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::AppSetup(AppSetupEvent::Installed(result))) => {
                self.arrive_node(result, ctx);
                return Flow::Handled;
            }
            Some(SettingsEvent::HarnessUpdateSettings(result)) => {
                self.settings = match result {
                    Ok(settings) => Load::Ready(*settings),
                    Err(code) => Load::Failed(code.clone()),
                };
                self.open_versions(ctx);
                return Flow::Handled;
            }
            _ => return Flow::Handled,
        };
        let asked = std::mem::take(&mut self.waiting);
        match result {
            Ok(entries) => {
                self.entries = entries.clone();
                self.versions
                    .retain(|id, _| entries.iter().any(|entry| entry.id == *id));
                self.load = Load::Ready(());
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
                let ready = self
                    .entries
                    .iter()
                    .filter(|entry| entry.status == HarnessStatus::Ready)
                    .count();
                ctx.store.harnesses = Load::Ready((ready, self.entries.len()));
                self.open_versions(ctx);
            }
            Err(code) if self.entries.is_empty() => self.load = Load::Failed(code.clone()),
            Err(code) if asked => {
                ctx.say(Message::error("Could not check harnesses.", plain(code)))
            }
            Err(_) => {}
        }
        Flow::Handled
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        if matches!(self.load, Load::Idle) {
            self.reload(ctx);
        }
        if matches!(self.settings, Load::Idle) {
            self.settings = Load::Loading;
            send(ctx, SettingsJob::HarnessUpdateSettings);
        }
        self.poll(ctx);
        let needed = self.versions.values().any(node_needed);
        let waiting = node_reads_again(needed, self.node.as_ref(), self.setup_busy);
        if waiting && !self.setup_asked && self.node_polls < NODE_POLLS_MAX {
            self.node_polls += 1;
            self.read_setup(ctx);
        }
        assert!(self.node_polls <= NODE_POLLS_MAX);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let content = self.body(limit, ctx);
        draw_page(frame, area, SettingsSection::Harnesses, content, None, ctx)
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let Some((id, field)) = &self.choosing {
            return Some(self.choose_modal(id, field, ctx));
        }
        if let Some(id) = &self.looked {
            return Some(self.looked_modal(id, ctx));
        }
        let Confirm { id, job, line } = self.confirm.as_ref()?;
        let name = self.name_of(id);
        let how = self.versions.get(id).map_or_else(String::new, |versions| {
            format!("{} ", installed_with(&versions.method))
        });
        let (after, verb) = match job {
            Job::Update => (
                vec![format!(
                    "{how}When it finishes, new agents use the new version with no restart. Agents that are running keep their version until they start again."
                )],
                "update",
            ),
            Job::Install => (
                vec![
                    "It never uses sudo and never runs a script from a website.".to_owned(),
                    format!("When it finishes, new agents can use {name} with no engine restart."),
                ],
                "install",
            ),
        };
        let room = usize::from(MODAL_WIDTH) - 4;
        let title = match job {
            Job::Update => format!("Update {name}?"),
            Job::Install => format!("Install {name}?"),
        };
        let mut body = vec![Line::raw("CrazyAgent runs exactly this line:")];
        body.extend(
            wrap(&sanitize(line), room)
                .into_iter()
                .map(|part| styled(part, Role::Accent, &ctx.theme)),
        );
        body.push(Line::default());
        for sentence in &after {
            body.extend(wrap(sentence, room).into_iter().map(Line::raw));
        }
        body.push(Line::default());
        body.extend(
            wrap(
                &format!("{title} y to {verb}, any other key to cancel."),
                room,
            )
            .into_iter()
            .map(Line::raw),
        );
        let label = match job {
            Job::Update => "Update",
            Job::Install => "Install",
        };
        Some((
            Modal {
                width: MODAL_WIDTH,
                title,
                body,
                top: None,
            },
            vec![hint("y", label), hint("Any key", "Cancel")],
        ))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
            || !self.asking.is_empty()
            || self.running.is_some()
            || !self.finding.is_empty()
            || self.sending_node
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Settings(SettingsSection::Harnesses))
    }
}
