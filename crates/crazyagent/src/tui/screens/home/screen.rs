use super::*;

impl Home {
    /// A Home screen with the project list focused.
    /// Input: none. Output: the screen; data arrives from the store.
    pub fn new() -> Self {
        Self {
            panel: Panel::Projects,
            selected: 0,
            offset: 0,
            setup_selected: 0,
            filter: None,
            filtering: false,
            was_done: None,
            owner_moved: false,
            hiding: None,
            folder: Load::Idle,
            found: Load::Idle,
            sent: None,
            job_shown: false,
        }
    }

    fn folders(&self) -> Vec<FoundProject> {
        let mut folders: Vec<FoundProject> = self
            .found
            .ready()
            .map(|found| {
                found
                    .iter()
                    .filter(|folder| folder.state != FoundState::Ready)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        folders.sort_by_key(|folder| match folder.state {
            FoundState::Onboarding => 0,
            FoundState::Found => 1,
            FoundState::Missing | FoundState::Ready => 2,
        });
        folders
    }

    fn unpicked(&self) -> bool {
        matches!(&self.folder, Load::Ready(home) if home.path.is_none())
    }

    pub(super) fn setup(ctx: &Ctx) -> Option<Setup> {
        let tools_ready = ctx
            .store
            .setup
            .ready()
            .is_some_and(|status| missing_tools(status).is_empty());
        let accounts = match &ctx.store.accounts {
            Load::Ready(rows) => rows.as_slice(),
            Load::Failed(_) | Load::NotReady => &[],
            Load::Idle | Load::Loading => return None,
        };
        ctx.store
            .pools
            .ready()
            .map(|pools| Setup::of(pools, accounts, tools_ready))
    }

    /// Works out Home's side panel. Setup keeps its rule: "Get set up" shows while setup is
    /// not done. The learning steps join it as "Learn CrazyAgent", setup under Basics, while
    /// the engine reports a step not done and the owner has not hidden the guide; a hidden
    /// guide, every step done or an engine without the learning read leaves "Get set up".
    /// Nothing shows until the learning read settles. Input: the context. Output: the
    /// panel, or None while it does not show.
    fn guide(ctx: &Ctx) -> Option<Guide> {
        let setup = Self::setup(ctx)?;
        let basics: Vec<GuideItem> = Self::items(ctx)
            .into_iter()
            .map(|(done, title, body, place)| GuideItem {
                done,
                title: title.into(),
                body,
                action: title.into(),
                counted: place != Place::Settings(SettingsSection::Harnesses),
                target: Target::Place(place),
            })
            .collect();
        let Some(progress) = ctx
            .store
            .learning
            .ready()
            .filter(|read| learning_open(read))
        else {
            let settled = !matches!(ctx.store.learning, Load::Idle | Load::Loading);
            if !settled || setup.done() {
                return None;
            }
            return Some(Guide {
                learning: false,
                groups: vec![(None, basics)],
            });
        };
        let project = ctx.store.projects.ready().and_then(|cards| cards.first());
        let mut groups = Vec::with_capacity(TOPICS.len());
        let mut basics = Some(basics);
        for (topic, heading) in TOPICS {
            let mut items = if topic == Topic::Basics {
                basics.take().unwrap_or_default()
            } else {
                Vec::new()
            };
            for step in progress.steps.iter().filter(|step| step.topic == topic) {
                let (action, target) = step_target(step.id, project);
                items.push(GuideItem {
                    done: step.done(),
                    title: step.title.clone(),
                    body: wrap(&step.body, TOOLS_TEXT_CHARS),
                    action: action.into(),
                    target,
                    counted: true,
                });
            }
            groups.push((Some(heading), items));
        }
        assert!(basics.is_none(), "the setup items sit under Basics");
        Some(Guide {
            learning: true,
            groups,
        })
    }

    /// Keeps the picked panel row inside the panel, which grows to the learning steps and
    /// shrinks back to the setup items as reads arrive. Input: the context. Output: none.
    fn clamp(&mut self, ctx: &Ctx) {
        let rows = Self::guide(ctx).map_or(SETUP_ITEMS, |guide| guide.items().count());
        assert!(rows >= SETUP_ITEMS);
        self.setup_selected = self.setup_selected.min(rows - 1);
    }

    fn entries(&self, ctx: &Ctx) -> Vec<Entry> {
        let Some(projects) = ctx.store.projects.ready() else {
            return Vec::new();
        };
        let words: Vec<String> = self
            .filter
            .as_ref()
            .map(|field| {
                field
                    .value()
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default();
        let matches = |text: &str| {
            let text = text.to_lowercase();
            words.iter().all(|word| text.contains(word))
        };
        let draft = kept_draft()
            .filter(|name| matches(name.as_str()))
            .map(Entry::Draft);
        let folders = self
            .folders()
            .into_iter()
            .filter(|folder| matches(folder.name.as_str()))
            .map(Entry::Folder);
        let projects = projects
            .iter()
            .filter(|card| {
                matches(
                    format!(
                        "{} {} {}",
                        card.name,
                        card.about,
                        card.pool.clone().unwrap_or_default()
                    )
                    .as_str(),
                )
            })
            .cloned()
            .map(Entry::Project);
        let job = ctx.store.intake.clone().map(Entry::Job);
        job.into_iter()
            .chain(draft)
            .chain(folders)
            .chain(projects)
            .collect()
    }

    /// The intake job when its card is the picked one: it is always the first card and no
    /// filter hides it. Input: the context. Output: the job, or None.
    fn picked_job<'a>(&self, ctx: &'a Ctx) -> Option<&'a JobView> {
        let picked = self.panel == Panel::Projects && self.selected == 0 && !self.filtering;
        let job = ctx.store.intake.as_ref().filter(|_| picked);
        assert!(job.is_none() || ctx.store.intake.is_some());
        job.filter(|_| ctx.store.projects.ready().is_some())
    }

    /// Does what Enter or d asks of the engine's job: stop it, start it again as the same kind
    /// of job (an add of its folder, a move of its project) with its mode and overrides, open
    /// the project's remove ask for a delete, where the exact name is typed again (DEC-177
    /// add.1), open New project for a New Project, whose job keeps no answers (add.2 Q3 A),
    /// answer its soft refusal in Add folder, or forget it. The request is kept so a failure
    /// is said.
    /// Inputs: the job, an action it offers and the context. Output: the flow.
    fn job_action(&mut self, job: &JobView, action: JobAction, ctx: &mut Ctx) -> Flow {
        assert!(intake_job::actions(job).contains(&action));
        let query = match action {
            JobAction::Stop => IntakeQuery::JobStop { id: job.id.clone() },
            JobAction::Retry if job.kind == JobKind::Setup => return Flow::Open(Place::NewProject),
            JobAction::Retry if job.kind == JobKind::Delete => {
                let Some(id) = job.project_id.clone() else {
                    ctx.say(Message::error(
                        retry_failed(job.kind),
                        &copy("intake_failed"),
                    ));
                    return Flow::Handled;
                };
                return Flow::Open(Place::DeleteAsk {
                    id,
                    name: job.source.clone(),
                });
            }
            JobAction::Retry => match (start_id(), job.subject()) {
                (Ok(id), Some(subject)) => IntakeQuery::JobStart {
                    id,
                    subject,
                    mode: job.mode,
                    overrides: job.overrides.clone(),
                },
                (Err(code), _) => {
                    ctx.say(Message::error(retry_failed(job.kind), &copy(&code)));
                    return Flow::Handled;
                }
                (Ok(_), None) => {
                    ctx.say(Message::error(
                        retry_failed(job.kind),
                        &copy("intake_failed"),
                    ));
                    return Flow::Handled;
                }
            },
            JobAction::Dismiss => IntakeQuery::JobDismiss { id: job.id.clone() },
            JobAction::Answer => {
                return Flow::Open(Place::AnswerAdd {
                    job: job.id.clone(),
                });
            }
        };
        self.sent = Some(query.clone());
        ask(ctx, query);
        assert!(self.sent.is_some());
        Flow::Handled
    }

    /// d on the picked intake job forgets it once it ended. Input: the context. Output:
    /// handled when the job was asked to be dismissed, else ignored.
    fn dismiss_picked(&mut self, ctx: &mut Ctx) -> Flow {
        let Some(job) = self.picked_job(ctx).cloned() else {
            return Flow::Ignored;
        };
        if !intake_job::actions(&job).contains(&JobAction::Dismiss) {
            return Flow::Ignored;
        }
        self.job_action(&job, JobAction::Dismiss, ctx)
    }

    /// Keeps the same card picked when the job card comes first or goes, since it is always
    /// the first card. Input: the context with the intake job read. Output: none.
    fn follow_job_card(&mut self, ctx: &Ctx) {
        let card = ctx.store.intake.is_some();
        if card == self.job_shown {
            return;
        }
        self.job_shown = card;
        self.selected = if card {
            self.selected + 1
        } else {
            self.selected.saturating_sub(1)
        };
        self.keep_in_range(ctx);
        assert_eq!(self.job_shown, ctx.store.intake.is_some());
    }

    /// Keeps the picked card inside the list after it changed, and in view. Input: the
    /// context. Output: none.
    fn keep_in_range(&mut self, ctx: &Ctx) {
        let count = self.entries(ctx).len();
        if self.selected >= count {
            self.selected = count.saturating_sub(1);
        }
        self.follow(ctx);
        assert!(self.selected < count.max(1));
    }

    pub(super) fn items(ctx: &Ctx) -> [(bool, &'static str, Vec<String>, Place); 4] {
        let setup = Self::setup(ctx).unwrap_or_default();
        let line = ctx
            .store
            .setup
            .ready()
            .filter(|status| !checking(status))
            .map(tools_line)
            .unwrap_or_default();
        let tools = if line.is_empty() {
            vec!["Agents need tmux, uv and".into(), "Graphify.".into()]
        } else {
            wrap(&line, TOOLS_TEXT_CHARS)
        };
        let harness = match &ctx.store.harnesses {
            Load::Ready((ready, all)) => vec![
                format!("{ready} of {all} harnesses ready"),
                "on this Mac.".into(),
            ],
            _ => vec!["Harness checks are not".into(), "ready yet.".into()],
        };
        [
            (
                setup.account_ready(),
                "Add an account",
                wrap(&setup.account_line(ctx.glyphs.ellipsis), TOOLS_TEXT_CHARS),
                Place::Settings(SettingsSection::Accounts),
            ),
            (
                setup.pool_exists,
                "Create a pool",
                vec![
                    "Group up to 15 accounts".into(),
                    "for projects to use.".into(),
                ],
                Place::Settings(SettingsSection::Pools),
            ),
            (
                matches!(ctx.store.harnesses, Load::Ready((ready, _)) if ready > 0),
                "Check your harnesses",
                harness,
                Place::Settings(SettingsSection::Harnesses),
            ),
            (
                setup.tools_ready,
                "Install tools",
                tools,
                Place::Setup { first_run: false },
            ),
        ]
    }

    fn rows(ctx: &Ctx) -> usize {
        ctx.height.saturating_sub(4) as usize
    }

    fn page(ctx: &Ctx) -> usize {
        (Self::rows(ctx).saturating_sub(2) / CARD_ROWS).max(1)
    }

    fn follow(&mut self, ctx: &Ctx) {
        let page = Self::page(ctx);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + page {
            self.offset = self.selected + 1 - page;
        }
    }

    fn running(ctx: &Ctx) -> usize {
        ctx.store.projects.ready().map_or(0, |projects| {
            projects.iter().map(ProjectCard::working).sum()
        })
    }

    fn suggestions(ctx: &Ctx) -> usize {
        ctx.store.projects.ready().map_or(0, |projects| {
            projects.iter().map(|card| card.activity.suggestions).sum()
        })
    }

    fn gutter(selected: bool, ctx: &Ctx) -> Line<'static> {
        if selected {
            Line::from(vec![
                Span::styled(ctx.glyphs.bar, ctx.theme.accent),
                Span::raw(" "),
            ])
        } else {
            Line::raw("  ")
        }
    }

    fn draft_card(name: &str, limit: usize, selected: bool, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let name_role = if selected {
            Role::Accent
        } else {
            Role::Heading
        };
        vec![
            join([
                Self::gutter(selected, ctx),
                row(
                    limit - 2,
                    styled(name.to_owned(), name_role, theme),
                    styled("Draft", Role::Muted, theme),
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    styled("Not created yet.", Role::Muted, theme),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    Line::raw("Enter continues the draft."),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            Line::default(),
        ]
    }

    fn job_card(job: &JobView, limit: usize, selected: bool, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let (kind, word) = intake_job::badge(job);
        let badge = Line::from(Span::styled(
            format!("{} {word}", ctx.glyphs.status(kind)),
            theme.style(kind.role()),
        ));
        let name_role = if selected {
            Role::Accent
        } else {
            Role::Heading
        };
        let lines = vec![
            join([
                Self::gutter(selected, ctx),
                row(
                    limit - 2,
                    styled(intake_job::name(job), name_role, theme),
                    badge,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(Line::raw(intake_job::line(job)), limit - 2, ctx.glyphs),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    styled(intake_job::next(job), Role::Muted, theme),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            Line::default(),
        ];
        assert_eq!(lines.len(), CARD_ROWS);
        lines
    }

    fn folder_card(
        folder: &FoundProject,
        limit: usize,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let (state, next) = match folder.state {
            FoundState::Onboarding => (
                status("Setting up", theme, ctx.glyphs),
                "Enter shows the setup.",
            ),
            FoundState::Missing => (
                status("Folder missing", theme, ctx.glyphs),
                "The folder is gone from your projects folder.",
            ),
            FoundState::Found | FoundState::Ready => {
                (status("Not set up", theme, ctx.glyphs), "Enter sets it up.")
            }
        };
        let name_role = if selected {
            Role::Accent
        } else {
            Role::Heading
        };
        let home = ctx.home.as_deref().map(std::path::Path::new);
        vec![
            join([
                Self::gutter(selected, ctx),
                row(
                    limit - 2,
                    styled(folder.name.clone(), name_role, theme),
                    state,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(
                    styled(display_path(&folder.path, home), Role::Muted, theme),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                Self::gutter(selected, ctx),
                pad(Line::raw(next), limit - 2, ctx.glyphs),
            ]),
            Line::default(),
        ]
    }

    fn card(
        &self,
        card: &ProjectCard,
        limit: usize,
        selected: bool,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let gutter = || Self::gutter(selected, ctx);
        let state = if card.working() > 0 {
            let count = card.working();
            styled(
                format!("{count} agent{} running", if count == 1 { "" } else { "s" }),
                Role::Running,
                theme,
            )
        } else if card.research.needs_pool > 0 {
            status("Needs a pool", theme, ctx.glyphs)
        } else if card.research.rate_limited > 0 {
            status("Rate limited", theme, ctx.glyphs)
        } else {
            styled("Idle", Role::Muted, theme)
        };
        let platforms = card
            .platforms
            .iter()
            .map(|platform| platform.label())
            .collect::<Vec<_>>()
            .join(", ");
        let about = if card.about.is_empty() {
            "No description."
        } else {
            card.about.as_str()
        };
        let first_line = about.lines().next().unwrap_or_default().to_owned();
        let created = card
            .created_ms
            .map(|ms| {
                format!(
                    "Created {}",
                    clock::ago(ms, clock::now_ms(), clock::local_offset())
                )
            })
            .unwrap_or_default();
        let pool = format!(
            "Pool: {}",
            card.pool.clone().unwrap_or_else(|| "none".into())
        );
        let corner = match (card.activity.notifications, card.activity.suggestions) {
            (0, 0) => styled(created, Role::Muted, theme),
            (notifications, suggestions) => styled(
                activity_corner(notifications, suggestions, limit - 2),
                Role::Warning,
                theme,
            ),
        };
        vec![
            join([
                gutter(),
                row(
                    limit - 2,
                    styled(
                        card.name.clone(),
                        if selected {
                            Role::Accent
                        } else {
                            Role::Heading
                        },
                        theme,
                    ),
                    state,
                    ctx.glyphs,
                ),
            ]),
            join([
                gutter(),
                pad(
                    Line::from(vec![
                        Span::raw(first_line),
                        Span::raw("  "),
                        Span::styled(platforms, theme.muted),
                    ]),
                    limit - 2,
                    ctx.glyphs,
                ),
            ]),
            join([
                gutter(),
                row(
                    limit - 2,
                    styled(
                        format!("{}   {pool}", card.research.summary()),
                        Role::Muted,
                        theme,
                    ),
                    corner,
                    ctx.glyphs,
                ),
            ]),
            Line::default(),
        ]
    }

    fn projects_column(&self, limit: usize, ctx: &Ctx) -> (Vec<Line<'static>>, Option<Position>) {
        let theme = &ctx.theme;
        let focused = self.panel == Panel::Projects;
        let title = |count: Option<usize>| {
            let mut spans = vec![Span::styled(
                "Projects",
                if focused && count.is_some() {
                    theme.accent
                } else {
                    theme.heading
                },
            )];
            if let Some(count) = count.filter(|count| *count > 0) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(count.to_string(), theme.muted));
            }
            Line::from(spans)
        };
        let unpicked = self
            .unpicked()
            .then(|| styled("Pick your projects folder with f.", Role::Warning, theme));
        match &ctx.store.projects {
            Load::Ready(projects)
                if projects.is_empty()
                    && ctx.store.intake.is_none()
                    && kept_draft().is_none()
                    && self.folders().is_empty() =>
            {
                let mut lines = vec![title(None), Line::default(), Line::raw("No projects yet.")];
                lines.extend(
                    wrap(
                        "Start one with n. You can add accounts first, or start a project now and pick a pool later.",
                        limit,
                    )
                    .into_iter()
                    .map(Line::raw),
                );
                if let Some(line) = unpicked {
                    lines.push(Line::default());
                    lines.push(line);
                }
                (lines, None)
            }
            Load::Ready(projects) => {
                let entries = self.entries(ctx);
                let right = if ctx.link == Link::Reconnecting {
                    styled(
                        ctx.store
                            .projects_seen
                            .as_ref()
                            .map(|time| format!("Last updated {time}"))
                            .unwrap_or_default(),
                        Role::Muted,
                        theme,
                    )
                } else {
                    styled("Most recent first", Role::Muted, theme)
                };
                let mut lines = vec![row(limit, title(Some(projects.len())), right, ctx.glyphs)];
                let mut cursor = None;
                match &self.filter {
                    Some(field) => {
                        lines.push(pad(
                            Line::from(vec![
                                Span::styled("/", theme.accent),
                                Span::raw(format!(" {}", field.value())),
                            ]),
                            limit,
                            ctx.glyphs,
                        ));
                        if self.filtering {
                            let column = (2 + width(field.value())).min(limit.saturating_sub(1));
                            cursor = Some(Position::new(column as u16, 1));
                        }
                    }
                    None => lines.push(Line::default()),
                }
                if let Some(line) = unpicked {
                    lines.push(pad(line, limit, ctx.glyphs));
                }
                if entries.is_empty() {
                    lines.push(Line::raw(format!(
                        "No project matches. Showing 0 of {}.",
                        projects.len()
                    )));
                }
                for (index, entry) in entries
                    .iter()
                    .enumerate()
                    .skip(self.offset)
                    .take(Self::page(ctx))
                {
                    let selected = focused && index == self.selected && !self.filtering;
                    lines.extend(match entry {
                        Entry::Job(job) => Self::job_card(job, limit, selected, ctx),
                        Entry::Draft(name) => Self::draft_card(name, limit, selected, ctx),
                        Entry::Folder(folder) => Self::folder_card(folder, limit, selected, ctx),
                        Entry::Project(card) => self.card(card, limit, selected, ctx),
                    });
                }
                if ctx.link == Link::Reconnecting {
                    lines = lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| {
                            if index == 0 {
                                line
                            } else {
                                restyle(line, theme.backdrop)
                            }
                        })
                        .collect();
                }
                (lines, cursor)
            }
            Load::Failed(_) => (
                vec![
                    title(None),
                    Line::default(),
                    status("Failed to load projects.", theme, ctx.glyphs),
                    Line::raw("Press r to try again."),
                ],
                None,
            ),
            _ => (
                vec![
                    title(None),
                    Line::default(),
                    ctx.loading("Loading projects"),
                ],
                None,
            ),
        }
    }

    /// Draws Home's side panel. "Get set up" lists each item with its words and says Enter
    /// opens Settings. "Learn CrazyAgent" says how many rows are done, lists one line per row
    /// under its topic and shows the words of the picked row, scrolled to keep it in view.
    /// Inputs: the panel, the rows it may take and the context. Output: the lines.
    fn guide_column(&self, guide: &Guide, height: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let focused = self.panel == Panel::Setup;
        let title = if guide.learning {
            "Learn CrazyAgent"
        } else {
            "Get set up"
        };
        let mut head = vec![styled(
            title,
            if focused { Role::Accent } else { Role::Heading },
            theme,
        )];
        if guide.learning {
            let (done, total) = guide.progress();
            head.push(styled(
                format!("{done} of {total} done"),
                Role::Muted,
                theme,
            ));
        } else {
            head.push(Line::default());
        }
        let mut body = Vec::new();
        let mut picked = 0..0;
        let mut index = 0;
        for (heading, items) in &guide.groups {
            if let Some(heading) = heading {
                body.push(styled(*heading, Role::Heading, theme));
            }
            for item in items {
                let selected = focused && index == self.setup_selected;
                let start = body.len();
                let gutter = || {
                    if selected {
                        Span::styled(ctx.glyphs.bar, theme.accent)
                    } else {
                        Span::raw(" ")
                    }
                };
                body.push(Line::from(vec![
                    gutter(),
                    Span::raw(" "),
                    if item.done {
                        Span::styled("[x]", theme.success)
                    } else {
                        Span::raw("[ ]")
                    },
                    Span::raw(" "),
                    Span::styled(
                        item.title.clone(),
                        if selected { theme.accent } else { theme.text },
                    ),
                ]));
                if selected || !guide.learning {
                    for part in &item.body {
                        body.push(Line::from(vec![
                            gutter(),
                            Span::raw("     "),
                            Span::styled(part.clone(), theme.muted),
                        ]));
                    }
                }
                if !guide.learning {
                    body.push(Line::default());
                }
                if selected {
                    picked = start..body.len();
                }
                index += 1;
            }
        }
        if !guide.learning {
            body.push(styled("Enter opens Settings", Role::Muted, theme));
        }
        let room = height.saturating_sub(head.len());
        let skip = picked.end.saturating_sub(room).min(picked.start);
        head.extend(body.into_iter().skip(skip).take(room));
        head
    }

    /// True once the tools check has a final answer: every tool checked, or the
    /// read failed or is not in this build.
    /// Input: the context. Output: whether the tools item is settled.
    fn checked(ctx: &Ctx) -> bool {
        match &ctx.store.setup {
            Load::Ready(status) => !checking(status),
            Load::Failed(_) | Load::NotReady => true,
            Load::Idle | Load::Loading => false,
        }
    }

    /// Handles one key on Home: the filter while it is typed, else the shortcuts, the panel
    /// switch, the moves in the focused panel and Enter. Inputs: the key and the context.
    /// Output: the flow the key leads to.
    fn steer(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        self.clamp(ctx);
        if self.filtering {
            match key {
                Key::Esc => {
                    self.filter = None;
                    self.filtering = false;
                }
                Key::Enter | Key::Down => {
                    self.filtering = false;
                    if self.filter.as_ref().is_some_and(TextField::is_empty) {
                        self.filter = None;
                    }
                }
                other => {
                    if let Some(field) = &mut self.filter {
                        field.key(other);
                    }
                    self.selected = 0;
                    self.offset = 0;
                }
            }
            return Flow::Handled;
        }
        let guide = Self::guide(ctx);
        let panel_visible = guide.is_some();
        let learning = guide.as_ref().is_some_and(|guide| guide.learning);
        let rows = guide
            .as_ref()
            .map_or(SETUP_ITEMS, |guide| guide.items().count());
        if !panel_visible {
            self.panel = Panel::Projects;
        }
        match key {
            Key::Char('n') => Flow::Open(Place::NewProject),
            Key::Char('a') => Flow::Open(Place::AddProject),
            Key::Char('f') => Flow::Open(Place::HomeFolder { first_run: false }),
            Key::Char('s') => Flow::Open(Place::Settings(SettingsSection::Accounts)),
            Key::Char('m') => Flow::Open(Place::Settings(SettingsSection::Memory)),
            Key::Char('u') => Flow::Open(Place::Usage),
            Key::Char('r') if matches!(ctx.store.projects, Load::Failed(_)) => {
                ctx.request(Request::Projects);
                Flow::Handled
            }
            Key::Char('/') if self.panel == Panel::Projects => {
                self.filter
                    .get_or_insert_with(|| TextField::new("Filter", FILTER_CHARS_MAX));
                self.filtering = true;
                Flow::Handled
            }
            Key::Esc if self.filter.is_some() => {
                self.filter = None;
                Flow::Handled
            }
            Key::Tab | Key::BackTab if panel_visible => {
                self.panel = match self.panel {
                    Panel::Projects => Panel::Setup,
                    Panel::Setup => Panel::Projects,
                };
                Flow::Handled
            }
            Key::Char('h') if self.panel == Panel::Setup && learning => {
                self.hiding = Some(hide(ctx));
                Flow::Handled
            }
            Key::Up | Key::Down if self.panel == Panel::Setup => {
                self.setup_selected = if matches!(key, Key::Down) {
                    (self.setup_selected + 1).min(rows - 1)
                } else {
                    self.setup_selected.saturating_sub(1)
                };
                Flow::Handled
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End => {
                let count = self.entries(ctx).len();
                if count == 0 {
                    return Flow::Handled;
                }
                let page = Self::page(ctx);
                self.selected = match key {
                    Key::Up => self.selected.saturating_sub(1),
                    Key::Down => (self.selected + 1).min(count - 1),
                    Key::PageUp => self.selected.saturating_sub(page.saturating_sub(1).max(1)),
                    Key::PageDown => (self.selected + page.saturating_sub(1).max(1)).min(count - 1),
                    Key::Home => 0,
                    _ => count - 1,
                };
                self.follow(ctx);
                Flow::Handled
            }
            Key::Char('d') => self.dismiss_picked(ctx),
            Key::Enter => self.open_selected(ctx),
            _ => Flow::Ignored,
        }
    }

    /// Where the owner is on Home: the focused panel, the picked card, the picked setup item
    /// and whether the filter is typed. Input: none. Output: those four.
    fn focus(&self) -> (Panel, usize, usize, bool) {
        (
            self.panel,
            self.selected,
            self.setup_selected,
            self.filtering,
        )
    }

    /// Says "Setup done." with the accounts ready for agents when the last item
    /// gets done, and focuses the panel on the first answer when something is left,
    /// unless the owner already moved the focus or the selection on this Home.
    /// Input: the context, once pools, accounts and the tools check have answered.
    /// Output: none.
    fn notice(&mut self, ctx: &mut Ctx) {
        if !Self::checked(ctx) {
            return;
        }
        let Some(setup) = Self::setup(ctx) else {
            return;
        };
        if self.was_done == Some(false) && setup.done() {
            let line = setup.account_line(ctx.glyphs.ellipsis);
            ctx.say(Message::success("Setup done.", &line));
        }
        if self.was_done.is_none() && !setup.done() && !self.owner_moved {
            self.panel = Panel::Setup;
        }
        self.was_done = Some(setup.done());
    }

    fn open_selected(&mut self, ctx: &mut Ctx) -> Flow {
        if self.panel == Panel::Setup
            && let Some(guide) = Self::guide(ctx)
        {
            return match guide.items().nth(self.setup_selected) {
                Some(GuideItem {
                    target: Target::Place(place),
                    ..
                }) => Flow::Open(place.clone()),
                Some(GuideItem {
                    target: Target::Desktop,
                    ..
                }) => {
                    ctx.say(Message::warning("The IDE opens in the desktop app.", ""));
                    Flow::Handled
                }
                None => Flow::Handled,
            };
        }
        match self.entries(ctx).into_iter().nth(self.selected) {
            Some(Entry::Job(job)) => match intake_job::actions(&job).first() {
                Some(&action) => self.job_action(&job, action, ctx),
                None => Flow::Handled,
            },
            Some(Entry::Draft(_)) => Flow::Open(Place::NewProject),
            Some(Entry::Folder(folder)) => match folder.state {
                FoundState::Missing => Flow::Handled,
                FoundState::Onboarding => Flow::Open(Place::Onboarding {
                    path: folder.path,
                    name: folder.name,
                    project: folder.project_id,
                }),
                FoundState::Found | FoundState::Ready => Flow::Open(Place::Onboarding {
                    path: folder.path,
                    name: folder.name,
                    project: None,
                }),
            },
            Some(Entry::Project(card)) => Flow::Open(Place::Project {
                id: card.id,
                name: card.name,
                tab: ProjectTab::Overview,
            }),
            None => Flow::Handled,
        }
    }
}

impl Screen for Home {
    fn crumbs(&self) -> Vec<String> {
        vec!["Home".into()]
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if ctx.link == Link::Reconnecting {
            return styled("Reconnecting", Role::Error, &ctx.theme);
        }
        match Self::running(ctx) {
            0 => Line::default(),
            count => styled(
                format!("{count} agent{} running", if count == 1 { "" } else { "s" }),
                Role::Running,
                &ctx.theme,
            ),
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
        let arrows = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        if self.panel == Panel::Setup
            && let Some(guide) = Self::guide(ctx)
        {
            let mut hints = Vec::new();
            if let Some(item) = guide.items().nth(self.setup_selected)
                && item.target != Target::Desktop
            {
                hints.push(hint("Enter", &item.action));
            }
            if guide.learning {
                hints.push(hint("h", "Hide guide"));
            }
            hints.extend([
                hint("n", "New project"),
                hint("a", "Add project"),
                hint(&arrows, "Move"),
                hint("Tab", "Switch panel"),
                hint("s", "Settings"),
                hint("Ctrl-K", "Go to"),
                hint("Ctrl-Q", "Quit"),
            ]);
            return hints;
        }
        let mut hints = vec![hint("n", "New project"), hint("a", "Add project")];
        match self.picked_job(ctx) {
            Some(job) => {
                let offered = intake_job::actions(job);
                if let Some(&first) = offered.first() {
                    hints.push(hint("Enter", intake_job::label(job, first)));
                }
                if offered.len() > 1 && offered.contains(&JobAction::Dismiss) {
                    hints.push(hint("d", intake_job::label(job, JobAction::Dismiss)));
                }
            }
            None => hints.push(hint("Enter", "Open")),
        }
        if Self::suggestions(ctx) > 0 {
            hints.push(hint("m", "Review memories"));
        }
        hints.extend([
            hint("s", "Settings"),
            hint("/", "Filter"),
            hint("Ctrl-K", "Go to"),
            hint("Ctrl-Q", "Quit"),
        ]);
        hints
    }

    fn keys(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            hint("n", "New project"),
            hint("a", "Add project"),
            hint("s", "Settings"),
            hint("Enter", "Open project"),
            hint("f", "Projects folder"),
        ]
    }

    fn typing(&self) -> bool {
        self.filtering
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let before = self.focus();
        let flow = self.steer(key, ctx);
        if self.focus() != before {
            self.owner_moved = true;
        }
        let rows = Self::guide(ctx).map_or(SETUP_ITEMS, |guide| guide.items().count());
        assert!(self.setup_selected < rows);
        flow
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        if let Arrival::Owner(owner) = arrival
            && let OwnerEvent::Learning(reply) = owner.as_ref()
            && let LearningQuery::Mark { generation, .. } = reply.query
            && self.hiding == Some(generation)
        {
            self.hiding = None;
            if let Err(code) = &reply.result {
                ctx.say(Message::error(
                    "Your learning progress was not saved.",
                    plain(code),
                ));
            }
        }
        if let Some(reply) = event(arrival) {
            self.follow_job_card(ctx);
            if self.sent.as_ref() == Some(&reply.query) {
                self.sent = None;
                if let Err(code) = &reply.result {
                    let line = intake_job::refusal(code, ctx.store.intake.as_ref());
                    ctx.say(Message::error(job_failed(&reply.query), &line));
                }
            }
            match (&reply.query, &reply.result) {
                (IntakeQuery::Home, Ok(IntakeView::Home(home))) => {
                    self.folder = Load::Ready(home.clone());
                }
                (IntakeQuery::Home, Err(code)) => {
                    self.folder = if not_ready(code) {
                        Load::NotReady
                    } else {
                        Load::Failed(code.clone())
                    };
                }
                (IntakeQuery::Found, Ok(IntakeView::Found(found))) => {
                    self.found = Load::Ready(found.clone());
                    self.keep_in_range(ctx);
                }
                (IntakeQuery::JobStatus, Ok(IntakeView::Job(_))) => self.keep_in_range(ctx),
                (IntakeQuery::Found, Err(code)) => {
                    self.found = if not_ready(code) {
                        Load::NotReady
                    } else {
                        Load::Failed(code.clone())
                    };
                }
                _ => {}
            }
            return Flow::Handled;
        }
        match arrival {
            Arrival::Pools(Ok(_)) | Arrival::Owner(_) => {
                self.clamp(ctx);
                self.notice(ctx);
                Flow::Handled
            }
            Arrival::Projects(Ok(_)) => {
                self.keep_in_range(ctx);
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        self.job_shown = ctx.store.intake.is_some();
        ctx.request(Request::Projects);
        ctx.request(Request::Pools);
        let ready = ctx
            .store
            .accounts
            .ready()
            .is_some_and(|rows| rows.iter().any(ready_for_agents));
        if !ready {
            load_accounts(ctx);
        }
        ask(ctx, IntakeQuery::Home);
        ask(ctx, IntakeQuery::Found);
        if ctx.store.harnesses.ready().is_none() {
            send(ctx, SettingsJob::Harnesses);
        }
        if !Self::checked(ctx) || matches!(ctx.store.setup, Load::Failed(_)) {
            send(ctx, SettingsJob::AppSetup(AppSetupJob::Status));
        }
        let learning = match &ctx.store.learning {
            Load::Idle | Load::Failed(_) => true,
            Load::Ready(progress) => learning_open(progress),
            Load::Loading | Load::NotReady => false,
        };
        if learning {
            if matches!(ctx.store.learning, Load::Idle) {
                ctx.store.learning = Load::Loading;
            }
            ctx.request(Request::Learning(Box::new(LearningQuery::Progress)));
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let width = area.width as usize;
        let (lines, cursor) = if let Some(guide) = Self::guide(ctx) {
            let right = if ctx.wide() { PANEL_WIDE } else { PANEL_NARROW };
            let left = width - 4 - 3 - right;
            let (projects, cursor) = self.projects_column(left, ctx);
            let setup = self.guide_column(&guide, area.height as usize, ctx);
            let lines = (0..area.height as usize)
                .map(|index| {
                    join([
                        Line::raw("  "),
                        pad(
                            projects.get(index).cloned().unwrap_or_default(),
                            left,
                            ctx.glyphs,
                        ),
                        Line::from(Span::styled(
                            format!(" {} ", ctx.glyphs.vline),
                            ctx.theme.muted,
                        )),
                        pad(
                            setup.get(index).cloned().unwrap_or_default(),
                            right,
                            ctx.glyphs,
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            (lines, cursor)
        } else {
            let (projects, cursor) = self.projects_column(width - 4, ctx);
            (
                projects
                    .into_iter()
                    .map(|line| join([Line::raw("  "), line]))
                    .collect(),
                cursor,
            )
        };
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor.map(|at| Position::new(area.x + 2 + at.x, area.y + at.y))
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        matches!(ctx.store.projects, Load::Loading | Load::Idle)
    }

    fn place(&self) -> Option<Place> {
        Some(Place::Home)
    }
}
