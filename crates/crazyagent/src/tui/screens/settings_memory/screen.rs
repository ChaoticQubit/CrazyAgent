use super::*;

impl Screen for Memory {
    fn wraps_hints(&self) -> bool {
        true
    }

    fn crumbs(&self) -> Vec<String> {
        match &self.project {
            Some((_, name)) => vec![name.clone(), "Project memory".into()],
            None => vec!["Settings".into(), "Memory".into()],
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if self.filtering {
            return vec![
                hint("Enter", "Keep filter"),
                hint(ctx.glyphs.down, "To the list"),
                hint("Esc", "Clear filter"),
            ];
        }
        if matches!(self.mode, Mode::Add { .. }) {
            return vec![
                hint("Enter", "Save"),
                hint("Ctrl-J", "New line"),
                hint("Tab", "Projects"),
                hint("Esc", "Cancel"),
            ];
        }
        if matches!(self.mode, Mode::Import { .. }) {
            return vec![
                hint("Enter", "Import"),
                hint("Tab", "Projects"),
                hint("Esc", "Cancel"),
            ];
        }
        let app = self.project.is_none();
        if !matches!(self.load, Load::Ready(())) {
            return if app {
                vec![sections_hint(ctx.glyphs), hint("Esc", "Home")]
            } else {
                vec![hint("s", "Memory settings"), hint("Esc", "Back")]
            };
        }
        let mut hints = match self.picked(ctx) {
            Some(Picked::Suggested(item)) if item.kept => vec![hint("Enter", "Use everywhere")],
            Some(Picked::Suggested(_)) => vec![hint("Enter", "Save or dismiss")],
            Some(Picked::Saved(memory)) if memory.origin.waiting() => vec![
                hint("y", "Approve"),
                hint("Enter", "View"),
                hint("d", "Delete"),
            ],
            Some(Picked::Saved(_)) => vec![hint("Enter", "View"), hint("d", "Delete")],
            None => Vec::new(),
        };
        if app {
            if self
                .focused(ctx)
                .is_some_and(|memory| Self::movable(&memory))
            {
                hints.push(hint("m", "Move to a project"));
            }
            hints.push(hint("a", "Add"));
            hints.push(hint("w", "Agent memory writes"));
            hints.push(hint("i", "Import"));
        } else {
            hints.push(hint("s", "Memory settings"));
        }
        if !self.memories.is_empty() || self.open().next().is_some() {
            hints.push(hint("/", "Filter"));
        }
        if app {
            hints.push(sections_hint(ctx.glyphs));
            hints.push(hint("Esc", "Home"));
            hints.push(hint("p", "Where agents save"));
        } else {
            hints.push(hint("Esc", "Back"));
        }
        hints
    }

    fn typing(&self) -> bool {
        self.filtering || matches!(self.mode, Mode::Add { .. } | Mode::Import { .. })
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match self.mode {
            Mode::Delete { .. } => return self.delete_key(key, ctx),
            Mode::Move { .. } => return self.move_key(key, ctx),
            Mode::Add { .. } => return self.add_key(key, ctx),
            Mode::Import { .. } => return self.import_key(key, ctx),
            Mode::Suggestion { .. } => return self.suggestion_key(key, ctx),
            Mode::View => {
                match key {
                    Key::Char('d') if !self.deleting => {
                        self.mode = Mode::Delete {
                            delete_focused: false,
                        }
                    }
                    Key::Char('y') => {
                        self.approve(ctx);
                        self.mode = Mode::List;
                    }
                    Key::Esc | Key::Enter => self.mode = Mode::List,
                    _ => {}
                }
                return Flow::Handled;
            }
            Mode::List => {}
        }
        if self.filtering {
            return self.filter_key(key, ctx);
        }
        let (suggested, saved) = match self.picked(ctx) {
            Some(Picked::Suggested(_)) => (true, false),
            Some(Picked::Saved(_)) => (false, true),
            None => (false, false),
        };
        let ready = matches!(self.load, Load::Ready(()));
        let app = self.project.is_none();
        let page = Self::window(ctx).saturating_sub(1).max(1) as isize;
        match key {
            Key::Char('r') if matches!(self.load, Load::Failed(_)) => self.reload(ctx),
            Key::Char('a') if app && ready && !self.adding => self.open_composer(),
            Key::Char('w') if app && ready => self.toggle_writes(ctx),
            Key::Char('p') if app && ready => self.toggle_scope(ctx),
            Key::Char('m') if app && saved => self.open_mover(ctx),
            Key::Char('y') if saved => self.approve(ctx),
            Key::Char('i') if app && ready && !self.importing => self.open_importer(ctx),
            Key::Char('s') if !app => {
                return Flow::Open(Place::Settings(SettingsSection::Memory));
            }
            Key::Char('/') if !self.memories.is_empty() || self.open().next().is_some() => {
                self.filtering = true
            }
            Key::Enter if suggested && self.deciding.is_none() => {
                self.mode = Mode::Suggestion { save_focused: true }
            }
            Key::Enter if saved => self.mode = Mode::View,
            Key::Char('d') if saved && !self.deleting => {
                self.mode = Mode::Delete {
                    delete_focused: false,
                }
            }
            Key::Esc if !self.filter.is_empty() => self.filter.clear(),
            Key::Up => self.move_by(-1, ctx),
            Key::Down => self.move_by(1, ctx),
            Key::PageUp => self.move_by(-page, ctx),
            Key::PageDown => self.move_by(page, ctx),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.count(ctx).saturating_sub(1),
            _ => return Flow::Ignored,
        }
        Flow::Handled
    }

    fn step(&mut self, forward: bool, ctx: &mut Ctx) -> Flow {
        match self.project {
            Some(_) => Flow::Ignored,
            None => step(SettingsSection::Memory, forward, ctx),
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Arrival::Projects(Ok(_)) = arrival {
            if !matches!(self.load, Load::Idle) && self.wanted(ctx) != self.scoped {
                self.reload(ctx);
            }
            return Flow::Handled;
        }
        match settings_event(arrival) {
            Some(SettingsEvent::Memories(scopes, result))
                if self.waiting && *scopes == self.scoped =>
            {
                self.waiting = false;
                match result {
                    Ok(memories) => {
                        self.memories = memories
                            .iter()
                            .filter(|memory| self.scoped.contains(&memory.scope))
                            .cloned()
                            .collect();
                        self.load = Load::Ready(());
                        self.clamp(ctx);
                        if self.project.is_none() {
                            ctx.store.memory = Load::Ready(self.memories.len());
                        }
                    }
                    Err(code) if self.memories.is_empty() => self.load = Load::Failed(code.clone()),
                    Err(code) => {
                        ctx.say(Message::error("Could not refresh memories.", plain(code)))
                    }
                }
            }
            Some(SettingsEvent::MemoryDeleted(result)) if self.deleting => {
                self.deleting = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success("Memory deleted.", ""));
                        self.reload(ctx);
                    }
                    Err(code) => {
                        ctx.say(Message::error("Could not delete this memory.", plain(code)))
                    }
                }
            }
            Some(SettingsEvent::AgentWrites(result)) => {
                let asked = std::mem::take(&mut self.setting);
                match result {
                    Ok(writes) => {
                        self.writes = Some(*writes);
                        if asked {
                            let lead = match writes {
                                AgentWrites::Save => "Agents now save memories directly.",
                                AgentWrites::Ask => "Agents now ask you before a memory is saved.",
                            };
                            ctx.say(Message::success(lead, ""));
                        }
                    }
                    Err(code) if asked => ctx.say(Message::error(
                        "Could not change Agent memory writes.",
                        memory_plain(code),
                    )),
                    Err(_) => {}
                }
            }
            Some(SettingsEvent::MemoryMoved(result)) if self.moving => {
                self.moving = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success(
                            "Memory moved. Only agents in that project get it now.",
                            "",
                        ));
                        self.reload(ctx);
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not move this memory.",
                        memory_plain(code),
                    )),
                }
            }
            Some(SettingsEvent::AgentScope(result)) => {
                let asked = std::mem::take(&mut self.scoping);
                match result {
                    Ok(reach) => {
                        self.reach = Some(*reach);
                        if asked {
                            let lead = match reach {
                                AgentScope::Project => {
                                    "Agents now save memories in their own project only."
                                }
                                AgentScope::Agent => {
                                    "Agents now choose where a memory is saved, app-wide included."
                                }
                            };
                            ctx.say(Message::success(lead, ""));
                        }
                    }
                    Err(code) if asked => ctx.say(Message::error(
                        "Could not change where agents save memories.",
                        memory_plain(code),
                    )),
                    Err(_) => {}
                }
            }
            Some(SettingsEvent::MemoryApproved(result)) if self.approving => {
                self.approving = false;
                match result {
                    Ok(()) => {
                        ctx.say(Message::success(
                            "Memory approved. Agents get it from their next launch.",
                            "",
                        ));
                        self.reload(ctx);
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not approve this memory.",
                        memory_plain(code),
                    )),
                }
            }
            Some(SettingsEvent::MemoryAdded(result)) if self.adding => {
                self.adding = false;
                match result {
                    Ok(()) => {
                        self.mode = Mode::List;
                        ctx.say(Message::success("Memory saved.", ""));
                        self.reload(ctx);
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not save this memory.",
                        memory_plain(code),
                    )),
                }
            }
            Some(SettingsEvent::MemoryImported(result)) if self.importing => {
                self.importing = false;
                match result {
                    Ok(report) => {
                        self.mode = Mode::List;
                        ctx.say(Message::success(
                            &format!("Imported {}, skipped {}.", report.imported, report.skipped),
                            "",
                        ));
                    }
                    Err(code) => ctx.say(Message::error(
                        "Could not import memories.",
                        memory_plain(code),
                    )),
                }
                self.reload(ctx);
            }
            Some(SettingsEvent::Suggestions(result)) if self.suggesting => {
                self.suggesting = false;
                match result {
                    Ok(found) => {
                        self.suggestions = found.clone();
                        self.clamp(ctx);
                    }
                    Err(code) if code == NOT_READY => {}
                    Err(code) => ctx.say(Message::warning(
                        "Could not read research suggestions.",
                        plain(code),
                    )),
                }
            }
            Some(SettingsEvent::SuggestionDone(done, result))
                if self
                    .deciding
                    .as_ref()
                    .is_some_and(|(asked, _)| asked == done) =>
            {
                let keep = self.deciding.take().is_some_and(|(_, keep)| keep);
                match result {
                    Ok(()) => {
                        let lead = match (done.kept, keep) {
                            (true, _) => "Now used in all projects.",
                            (false, true) => "Saved to memory.",
                            (false, false) => "Suggestion dismissed.",
                        };
                        ctx.say(Message::success(lead, ""));
                        self.suggestions.retain(|item| item != done);
                        self.clamp(ctx);
                        self.reload(ctx);
                    }
                    Err(code) => {
                        ctx.say(Message::error(
                            "Could not finish with this suggestion.",
                            memory_plain(code),
                        ));
                        self.reload(ctx);
                    }
                }
            }
            _ => {}
        }
        Flow::Handled
    }

    fn modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if let (Mode::Suggestion { save_focused }, Some(Picked::Suggested(item))) =
            (&self.mode, self.picked(ctx))
        {
            return Some(Self::suggestion_modal(item, *save_focused, ctx));
        }
        let memory = self.focused(ctx)?;
        let theme = &ctx.theme;
        let room = usize::from(MODAL_WIDTH) - 4;
        match self.mode {
            Mode::List | Mode::Add { .. } | Mode::Import { .. } | Mode::Suggestion { .. } => None,
            Mode::Move { project } => {
                let choices = Self::project_choices(ctx);
                let name = choices
                    .get(project.min(choices.len().saturating_sub(1)))
                    .map_or_else(
                        || "A project".to_owned(),
                        |(_, name)| printable(name.clone()),
                    );
                let mut body = vec![
                    Line::raw("Move this app-wide memory into one project?"),
                    Line::default(),
                ];
                body.push(styled(
                    cut(&printable(memory.text.clone()), room, ctx.glyphs.ellipsis),
                    Role::Muted,
                    theme,
                ));
                body.push(Line::default());
                body.push(Line::raw(cut(
                    &format!("Project  {} {name} {}", ctx.glyphs.left, ctx.glyphs.right),
                    room,
                    ctx.glyphs.ellipsis,
                )));
                body.extend(
                    wrap("Only agents in this project get it from then on.", room)
                        .into_iter()
                        .map(Line::raw),
                );
                let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Move memory".into(),
                        body,
                        top: None,
                    },
                    vec![
                        hint(&arrows, "Project"),
                        hint("Enter", "Move"),
                        hint("Esc", "Cancel"),
                    ],
                ))
            }
            Mode::View => {
                let mut body: Vec<Line<'static>> = wrap(&sanitize(&memory.text), room)
                    .into_iter()
                    .map(Line::raw)
                    .collect();
                body.push(Line::default());
                body.push(status(&saved_line(&memory, ctx), theme, ctx.glyphs));
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Memory".into(),
                        body,
                        top: None,
                    },
                    if memory.origin.waiting() {
                        vec![
                            hint("y", "Approve"),
                            hint("d", "Delete"),
                            hint("Esc", "Close"),
                        ]
                    } else {
                        vec![hint("d", "Delete"), hint("Esc", "Close")]
                    },
                ))
            }
            Mode::Delete { delete_focused } => {
                let reach = match &memory.scope {
                    MemoryScope::Global => "Agents in every project stop getting it.".to_owned(),
                    MemoryScope::Project { project_id } => {
                        format!(
                            "Agents in {} stop getting it.",
                            project_name(project_id, ctx)
                        )
                    }
                };
                let mut body = vec![Line::raw("Delete this memory for good?"), Line::default()];
                body.push(styled(
                    cut(&printable(memory.text.clone()), room, ctx.glyphs.ellipsis),
                    Role::Muted,
                    theme,
                ));
                body.extend(wrap(&reach, room).into_iter().map(Line::raw));
                body.push(Line::default());
                body.push(buttons(
                    &["Cancel", "Delete"],
                    Some(usize::from(delete_focused)),
                    theme,
                ));
                let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
                Some((
                    Modal {
                        width: MODAL_WIDTH,
                        title: "Delete memory".into(),
                        body,
                        top: None,
                    },
                    vec![
                        hint(&arrows, "Choose"),
                        hint("Enter", "Confirm"),
                        hint("Esc", "Cancel"),
                    ],
                ))
            }
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ctx.request(Request::Projects);
        if matches!(self.load, Load::Idle) || self.project.is_some() {
            self.reload(ctx);
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let limit = (area.width as usize).saturating_sub(4).max(8);
        let (content, cursor) = self.body(limit, ctx);
        match self.project {
            Some(_) => {
                draw_lines(frame.buffer_mut(), area, &indent(content, 2));
                cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
            }
            None => draw_page(frame, area, SettingsSection::Memory, content, cursor, ctx),
        }
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.waiting
            || self.setting
            || self.scoping
            || self.moving
            || self.approving
            || self.deleting
            || self.adding
            || self.importing
            || self.suggesting
            || self.deciding.is_some()
    }

    fn place(&self) -> Option<Place> {
        Some(match &self.project {
            Some((id, name)) => Place::ProjectMemory {
                id: id.clone(),
                name: name.clone(),
            },
            None => Place::Settings(SettingsSection::Memory),
        })
    }
}
