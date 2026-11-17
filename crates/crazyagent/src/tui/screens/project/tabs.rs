use super::*;

impl ProjectScreen {
    /// Finds the suggested memory that came from the report being read.
    /// Takes the screen; only research reports have one.
    /// Returns the memory, when the report being read suggested one.
    pub fn report_suggestion(&self) -> Option<&KnowledgeEntry> {
        let kind = research_kind(&self.preview.as_ref()?.path)?;
        self.suggested()
            .into_iter()
            .find(|memory| memory_source(memory) == Some(kind))
    }

    fn decide(&mut self, memory: Option<(RequestId, u64, String)>, code: KeyCode) -> ProjectReply {
        let Some((entry, version, text)) = memory else {
            return ProjectReply::Handled;
        };
        match code {
            KeyCode::Char('n') => ProjectReply::Review {
                entry,
                version,
                decision: ReviewDecision::Reject,
            },
            _ => {
                self.review = Some(MemoryReview {
                    entry,
                    version,
                    text,
                    scroll: 0,
                });
                ProjectReply::Handled
            }
        }
    }

    fn edit_memory(&mut self, entry: RequestId, version: u64, text: &str) {
        let mut input = TextInput::multi(MEMORY_BYTES_MAX);
        input.set(&sanitize(text));
        self.edit = Some(MemoryEdit {
            entry,
            version,
            input,
        });
    }

    pub(super) fn review_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(review) = self.review.as_mut() else {
            return ProjectReply::Ignored;
        };
        let last = review.lines().len().saturating_sub(REVIEW_ROWS);
        match key.code {
            KeyCode::Esc => self.review = None,
            KeyCode::Up => review.scroll = review.scroll.saturating_sub(1),
            KeyCode::Down => review.scroll = (review.scroll + 1).min(last),
            KeyCode::PageUp => review.scroll = review.scroll.saturating_sub(REVIEW_ROWS),
            KeyCode::PageDown => review.scroll = (review.scroll + REVIEW_ROWS).min(last),
            KeyCode::Char('y') if review.at_end() => {
                let Some(review) = self.review.take() else {
                    return ProjectReply::Ignored;
                };
                let shown = sanitize(&review.text);
                let decision = if shown == review.text {
                    ReviewDecision::Keep
                } else {
                    ReviewDecision::Edit {
                        text: shown.trim().to_owned(),
                    }
                };
                return ProjectReply::Review {
                    entry: review.entry,
                    version: review.version,
                    decision,
                };
            }
            KeyCode::Char('n') => {
                let Some(review) = self.review.take() else {
                    return ProjectReply::Ignored;
                };
                return ProjectReply::Review {
                    entry: review.entry,
                    version: review.version,
                    decision: ReviewDecision::Reject,
                };
            }
            KeyCode::Char('e') if review.at_end() => {
                if let Some(review) = self.review.take() {
                    self.edit_memory(review.entry, review.version, &review.text);
                }
            }
            _ => {}
        }
        ProjectReply::Handled
    }

    pub(super) fn overview_key(&mut self, key: KeyEvent) -> ProjectReply {
        let suggested: Vec<(RequestId, u64, String)> = self
            .suggested()
            .iter()
            .map(|m| (m.id.clone(), m.version, m.text.clone()))
            .collect();
        let work = self.work_kinds().len();
        let rows = suggested.len() + work;
        let focused = suggested.get(self.overview_cursor).cloned();
        if let Some(reply) = self.planning_reply(key.code) {
            return reply;
        }
        let tabs = self.overview_tabs.is_some();
        match key.code {
            KeyCode::Left | KeyCode::Right if tabs => {
                let count = OVERVIEW_TAB_LABELS.len();
                let step = if key.code == KeyCode::Right {
                    1
                } else {
                    count - 1
                };
                self.overview_tab = (self.overview_tab + step) % count;
                self.overview_scroll = 0;
                self.overview_read_ms = None;
            }
            KeyCode::Char('R') if tabs => return self.refresh_overview(),
            KeyCode::PageDown if tabs => {
                let down = self.overview_scroll + OVERVIEW_PAGE_ROWS;
                self.overview_scroll = down.min(OVERVIEW_SCROLL_MAX);
            }
            KeyCode::PageUp if tabs => {
                self.overview_scroll = self.overview_scroll.saturating_sub(OVERVIEW_PAGE_ROWS);
            }
            KeyCode::Up => self.overview_cursor = self.overview_cursor.saturating_sub(1),
            KeyCode::Down => {
                self.overview_cursor = (self.overview_cursor + 1).min(rows.saturating_sub(1));
            }
            KeyCode::Char('y' | 'n' | 'e') => return self.decide(focused, key.code),
            KeyCode::Char('o') => {
                return self.setup.as_ref().map_or(ProjectReply::Handled, |s| {
                    ProjectReply::Open(OpenRequest::Folder(
                        s.project_folder().display().to_string(),
                    ))
                });
            }
            KeyCode::Enter
                if !self.pool.as_ref().is_some_and(|p| p.assigned) && suggested.is_empty() =>
            {
                return self.pool_reply();
            }
            KeyCode::Enter => {
                let index = self.overview_cursor.checked_sub(suggested.len());
                let kind = index.and_then(|i| self.work_kinds().get(i).copied());
                if let Some(kind) = kind {
                    self.read(&kind.report_path());
                }
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    pub(super) fn edit_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(edit) = self.edit.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.edit = None;
                ProjectReply::Handled
            }
            KeyCode::Enter => {
                let text = edit.input.value().trim().to_owned();
                if text.is_empty() {
                    return ProjectReply::Handled;
                }
                let reply = ProjectReply::Review {
                    entry: edit.entry.clone(),
                    version: edit.version,
                    decision: ReviewDecision::Edit { text },
                };
                self.edit = None;
                reply
            }
            _ => {
                edit.input.handle(key);
                ProjectReply::Handled
            }
        }
    }

    pub(super) fn agents_key(&mut self, key: KeyEvent) -> ProjectReply {
        let count = self.jobs.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Up => self.agents_cursor = self.agents_cursor.saturating_sub(1),
            KeyCode::Down => {
                self.agents_cursor = (self.agents_cursor + 1).min(count.saturating_sub(1))
            }
            KeyCode::Enter => {
                let path = self
                    .jobs
                    .as_ref()
                    .and_then(|jobs| jobs.get(self.agents_cursor))
                    .map(|job| job.report_path.clone());
                if let Some(path) = path {
                    self.read(&path);
                }
            }
            KeyCode::Char('x') => {
                let Some((job, status, stopping)) = self
                    .focused_job()
                    .map(|job| (job.id, job.status, job.cancel_requested))
                else {
                    return ProjectReply::Handled;
                };
                match status {
                    ResearchStatus::Running if stopping => {
                        self.problem = Some("Stop was already asked. The agent is ending.".into());
                    }
                    ResearchStatus::Queued
                    | ResearchStatus::NeedsPool
                    | ResearchStatus::Running => {
                        self.confirm_stop = Some(job);
                    }
                    ResearchStatus::Done | ResearchStatus::Failed | ResearchStatus::Cancelled => {
                        self.problem = Some("This research is not running.".into());
                    }
                }
            }
            KeyCode::Char('t') => return ProjectReply::Team,
            KeyCode::Char('s') => return ProjectReply::TeamSettings,
            KeyCode::Char('r') => {
                let Some((job, status)) = self.focused_job().map(|job| (job.id, job.status)) else {
                    return ProjectReply::Handled;
                };
                if matches!(status, ResearchStatus::Failed | ResearchStatus::Cancelled) {
                    return ProjectReply::Change(Box::new(SetupAction::RetryResearch {
                        project_id: self.project.clone(),
                        job,
                    }));
                }
                self.problem = Some("Only failed or stopped research can run again.".into());
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    /// Opens a context file in the reader on the Context tab.
    /// Takes the path inside the project folder.
    /// Returns nothing; the text loads with the next reads.
    pub fn read(&mut self, path: &str) {
        self.open(Tab::Context);
        if let Some(index) = self.tree().iter().position(|item| item.path == path) {
            self.tree_cursor = index;
        }
        if self
            .preview
            .as_ref()
            .is_none_or(|p| p.path != path || p.problem.is_some())
        {
            self.preview = Some(Preview {
                path: path.to_owned(),
                ..Preview::default()
            });
        }
        self.reading = true;
    }

    pub(super) fn context_key(&mut self, key: KeyEvent) -> ProjectReply {
        let items: Vec<ContextItem> = self.tree().into_iter().cloned().collect();
        if self.reading {
            let wide = self.width >= 100;
            match key.code {
                KeyCode::Esc => {
                    self.reading = false;
                    return ProjectReply::Handled;
                }
                KeyCode::PageDown | KeyCode::Down if !wide || key.code == KeyCode::PageDown => {
                    if let Some(preview) = self.preview.as_mut() {
                        preview.scroll += if key.code == KeyCode::PageDown { 10 } else { 1 };
                    }
                    return ProjectReply::Handled;
                }
                KeyCode::PageUp | KeyCode::Up if !wide || key.code == KeyCode::PageUp => {
                    if let Some(preview) = self.preview.as_mut() {
                        preview.scroll = preview
                            .scroll
                            .saturating_sub(if key.code == KeyCode::PageUp { 10 } else { 1 });
                    }
                    return ProjectReply::Handled;
                }
                KeyCode::Char('y' | 'n' | 'e') => {
                    let memory = self
                        .report_suggestion()
                        .map(|m| (m.id.clone(), m.version, m.text.clone()));
                    if memory.is_some() {
                        return self.decide(memory, key.code);
                    }
                }
                _ => {}
            }
        }
        let item = items.get(self.tree_cursor).cloned();
        match key.code {
            KeyCode::Up => {
                self.tree_cursor = self.tree_cursor.saturating_sub(1);
                self.follow_cursor();
            }
            KeyCode::Down => {
                self.tree_cursor = (self.tree_cursor + 1).min(items.len().saturating_sub(1));
                self.follow_cursor();
            }
            KeyCode::Enter => {
                if let Some(item) = item {
                    self.read(&item.path);
                }
            }
            KeyCode::Char('o') => {
                if let Some(request) = item.and_then(|item| self.context_request(&item.path)) {
                    return ProjectReply::Open(request);
                }
            }
            KeyCode::Char('v') => {
                if let Some(request) = item.and_then(|item| self.context_request(&item.path)) {
                    return ProjectReply::Edit(request);
                }
            }
            KeyCode::Char('O') => {
                if let Some(item) = item {
                    match self.ide_target(&item.path) {
                        Some(target) => return ProjectReply::Reveal(target),
                        None if self.setup.is_none() => {
                            self.problem = Some("Still loading the project. Try again.".into());
                        }
                        None => {
                            self.problem = Some(
                                "This file is outside the project folder, so the IDE can't open it."
                                    .into(),
                            );
                        }
                    }
                }
            }
            KeyCode::Char('a') => {
                self.adding = Some(AddFile {
                    input: TextInput::single(DOCUMENT_PATH_BYTES_MAX),
                    busy: false,
                    error: None,
                });
            }
            KeyCode::Char('d') => match item {
                Some(item) if item.linked => {
                    self.problem = Some(LINKED_KEPT.into());
                }
                Some(item) if item.kind == ContextKind::Plan => {
                    self.problem = Some("The roadmap and release process can't be deleted.".into());
                }
                Some(item) if item.kind != ContextKind::Brief => {
                    self.confirm_delete = Some(item.path);
                }
                Some(_) => self.problem = Some("The project brief can't be deleted.".into()),
                None => {}
            },
            KeyCode::Char('p') => return ProjectReply::Memory,
            KeyCode::Char('m') => {
                let research = item
                    .as_ref()
                    .filter(|item| item.kind == ContextKind::Research)
                    .and_then(|item| research_kind(&item.path));
                return match research {
                    Some(kind) => ProjectReply::Change(Box::new(SetupAction::SaveToMemory {
                        project_id: self.project.clone(),
                        research: kind,
                        section: None,
                    })),
                    None => {
                        self.problem = Some("Only research reports can be saved to memory.".into());
                        ProjectReply::Handled
                    }
                };
            }
            _ => return ProjectReply::Ignored,
        }
        ProjectReply::Handled
    }

    fn follow_cursor(&mut self) {
        if self.width < 100 {
            return;
        }
        let path = self
            .tree()
            .get(self.tree_cursor)
            .map(|item| item.path.clone());
        if let Some(path) = path
            && self.preview.as_ref().is_none_or(|p| p.path != path)
        {
            self.preview = Some(Preview {
                path,
                ..Preview::default()
            });
        }
    }

    fn pool_reply(&mut self) -> ProjectReply {
        if !self.pool_ready {
            return ProjectReply::Handled;
        }
        let picked = self.setup.as_ref().and_then(|setup| setup.pool.clone());
        match (&self.pool, picked) {
            (Some(pool), Some(id)) if !pool.assigned => ProjectReply::Assign(Some(id)),
            _ if self.pool_choices.is_empty() => ProjectReply::PickPool,
            (pool, _) => {
                let current = pool.as_ref().filter(|pool| pool.assigned).and_then(|pool| {
                    self.pool_choices
                        .iter()
                        .position(|choice| choice.id == pool.id)
                });
                self.pool_picker = Some(current.unwrap_or(0));
                ProjectReply::Handled
            }
        }
    }

    /// Answers one key in the remove ask. Remove from dashboard sends on y; Delete everything
    /// is offered only when the preview allows it and sends on Enter once the typed text is the
    /// project's exact name. Takes the key. Returns the change to send, or Handled.
    pub(super) fn remove_key(&mut self, key: KeyEvent) -> ProjectReply {
        let can_delete = self
            .removal
            .as_ref()
            .is_some_and(|removal| removal.can_delete);
        if key.code == KeyCode::Esc {
            self.close_remove();
            return ProjectReply::Handled;
        }
        if can_delete
            && matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab
            )
        {
            self.remove_choice = match self.remove_choice {
                RemoveChoice::Dashboard => RemoveChoice::Everything,
                RemoveChoice::Everything => RemoveChoice::Dashboard,
            };
            return ProjectReply::Handled;
        }
        match self.remove_choice {
            RemoveChoice::Dashboard => match key.code {
                KeyCode::Char('y') => {
                    self.close_remove();
                    ProjectReply::Change(Box::new(SetupAction::Remove {
                        project_id: self.project.clone(),
                    }))
                }
                KeyCode::Char('n') => {
                    self.close_remove();
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            },
            RemoveChoice::Everything => {
                match key.code {
                    KeyCode::Enter => {
                        let name = self
                            .removal
                            .as_ref()
                            .filter(|removal| {
                                removal.can_delete && removal.name == self.remove_typed
                            })
                            .map(|removal| removal.name.clone());
                        let Some(name) = name else {
                            self.problem =
                                Some("Type the project's exact name to delete everything.".into());
                            return ProjectReply::Handled;
                        };
                        self.close_remove();
                        return ProjectReply::DeleteEverything(name);
                    }
                    KeyCode::Backspace => {
                        self.remove_typed.pop();
                    }
                    KeyCode::Char(letter)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && self.remove_typed.len() + letter.len_utf8() <= NAME_BYTES_MAX =>
                    {
                        self.remove_typed.push(letter);
                    }
                    _ => {}
                }
                ProjectReply::Handled
            }
        }
    }

    /// Answers one key in the move ask: m moves the project into the projects home, c copies
    /// it there, and n or Esc keeps it where it is. Takes the key. Returns the move to send,
    /// or Handled.
    pub(super) fn move_key(&mut self, key: KeyEvent) -> ProjectReply {
        use crate::projects_home::AddMode;
        assert!(self.confirm_move);
        let mode = match key.code {
            KeyCode::Char('m') => AddMode::Move,
            KeyCode::Char('c') => AddMode::Copy,
            KeyCode::Char('n') | KeyCode::Esc => {
                self.confirm_move = false;
                return ProjectReply::Handled;
            }
            _ => return ProjectReply::Handled,
        };
        self.confirm_move = false;
        ProjectReply::MoveHome(mode)
    }

    /// Closes the remove ask and forgets its preview, choice and typed name.
    /// Takes the screen. Returns nothing.
    pub(super) fn close_remove(&mut self) {
        self.confirm_remove = false;
        self.removal = None;
        self.remove_choice = RemoveChoice::Dashboard;
        self.remove_everything = false;
        self.remove_typed.clear();
    }

    /// Shows why the engine refused to start the job that moves this project into the projects
    /// home or deletes it, as a plain line; a busy refusal names the job that runs, as last read.
    /// Takes the refusal code and the job read. Returns nothing.
    pub fn job_refused(&mut self, code: &str, held: Option<&crate::projects_home::job::JobView>) {
        assert!(!code.is_empty());
        self.problem = Some(match code {
            "job_busy" => super::super::intake_job::busy(held),
            code => plain_problem(code),
        });
        assert!(self.problem.is_some());
    }

    /// Opens the remove ask for a delete an engine stop cut short, as x does, on the Settings
    /// tab where that ask lives, as the desktop's Delete again opens Settings, with Delete
    /// everything picked once the engine's preview offers it; the exact name is still typed.
    /// Does nothing while another box is open, which keeps its keys. Takes the screen.
    /// Returns nothing.
    pub fn ask_delete_again(&mut self) {
        if self.boxed() {
            return;
        }
        self.open(Tab::Settings);
        self.confirm_remove = true;
        self.removal_read_ms = None;
        self.remove_everything = true;
        assert_eq!(self.tab, Tab::Settings);
        assert!(self.removal.is_none());
        assert_eq!(self.remove_choice, RemoveChoice::Dashboard);
    }

    /// Moves the pool picker or settles it. The rows are the pools, then "No pool" last.
    /// Takes the key and the cursor. Returns the chosen pool, None for "No pool", or Handled.
    pub(super) fn picker_key(&mut self, key: KeyEvent, cursor: usize) -> ProjectReply {
        let last = self.pool_choices.len();
        assert!(cursor <= last);
        match key.code {
            KeyCode::Up => self.pool_picker = Some(cursor.saturating_sub(1)),
            KeyCode::Down => self.pool_picker = Some((cursor + 1).min(last)),
            KeyCode::Esc => self.pool_picker = None,
            KeyCode::Enter => {
                self.pool_picker = None;
                return ProjectReply::Assign(
                    self.pool_choices
                        .get(cursor)
                        .map(|choice| choice.id.clone()),
                );
            }
            _ => {}
        }
        ProjectReply::Handled
    }

    /// Replaces the pools the picker lists.
    /// Takes the choices in the engine's order; an open picker keeps a cursor on a pool
    /// or on "No pool", and closes when no pool is left. Returns nothing.
    pub fn set_pool_choices(&mut self, choices: Vec<PoolChoice>) {
        self.pool_choices = choices;
        if let Some(cursor) = self.pool_picker {
            self.pool_picker = if self.pool_choices.is_empty() {
                None
            } else {
                Some(cursor.min(self.pool_choices.len()))
            };
        }
    }

    pub(super) fn settings_key(&mut self, key: KeyEvent) -> ProjectReply {
        if let Some(reply) = self.planning_reply(key.code) {
            return reply;
        }
        match key.code {
            KeyCode::Enter => self.pool_reply(),
            KeyCode::Char('o') => self.setup.as_ref().map_or(ProjectReply::Handled, |s| {
                ProjectReply::Open(OpenRequest::Folder(
                    s.project_folder().display().to_string(),
                ))
            }),
            KeyCode::Char('g') => {
                match self.setup.as_ref() {
                    Some(setup) => self.general = Some(GeneralEdit::of(setup)),
                    None if self.setup_missing => {
                        self.problem = Some(plain_problem("project_setup_missing"));
                    }
                    None => self.problem = Some("Still loading the project. Try again.".into()),
                }
                ProjectReply::Handled
            }
            KeyCode::Char('x') => {
                self.close_remove();
                self.confirm_remove = true;
                self.removal_read_ms = None;
                ProjectReply::Handled
            }
            KeyCode::Char('f') => {
                match &self.read_paths {
                    Some(saved) => {
                        let mut input = TextInput::single(FOLDERS_BYTES_MAX);
                        input.set(&saved.paths.join(":"));
                        self.folders_version = saved.version;
                        self.folders_stale = false;
                        self.folders = Some(AddFile {
                            input,
                            busy: false,
                            error: None,
                        });
                    }
                    None => {
                        self.problem =
                            Some("Still loading the read-only folders. Try again.".into());
                    }
                }
                ProjectReply::Handled
            }
            KeyCode::Char('R') if !self.hidden_seers().is_empty() => {
                ProjectReply::TeamChange(Box::new(crate::team::TeamAction::RestartHiddenSeers {
                    project_id: self.project.clone(),
                }))
            }
            KeyCode::Char('k') => {
                match &self.hidden_paths {
                    Some(saved) => {
                        let mut input = TextInput::single(FOLDERS_BYTES_MAX);
                        input.set(&saved.paths.join(":"));
                        self.hidden_version = saved.version;
                        self.hidden_stale = false;
                        self.hidden = Some(AddFile {
                            input,
                            busy: false,
                            error: None,
                        });
                    }
                    None => {
                        self.problem = Some("Still loading the hidden folders. Try again.".into());
                    }
                }
                ProjectReply::Handled
            }
            KeyCode::Char('c') => {
                match &self.agent_policy {
                    Some(view) => {
                        self.access = Some(AccessEdit::of(view));
                        self.access_stale = false;
                    }
                    None => {
                        self.problem = Some("Still loading the agent access. Try again.".into());
                    }
                }
                ProjectReply::Handled
            }
            KeyCode::Char('M') => {
                self.confirm_move = true;
                ProjectReply::Handled
            }
            KeyCode::Char('i') => ProjectReply::Process,
            KeyCode::Char('G') => ProjectReply::GitHubRules,
            KeyCode::Char('u') => match &self.schedule {
                Some(schedule) => ProjectReply::AccountOrder(
                    crate::tui::screens::settings::account_choice::cycle(
                        self.own_order(schedule),
                        true,
                        true,
                    ),
                ),
                None => {
                    self.problem = Some("Still loading the account order. Try again.".into());
                    ProjectReply::Handled
                }
            },
            _ => ProjectReply::Ignored,
        }
    }

    /// Whether a planning run can replace roadmap documents the owner may have edited: a plan
    /// ran before, the context lists one of them, or the context is not read yet. A roadmap
    /// linked from the repository is never written by planning, so it does not count.
    /// Takes the planning state. Returns true when the owner must confirm first.
    fn plan_replaces(&self, state: &Planning) -> bool {
        *state != Planning::Idle
            || self.context.as_ref().is_none_or(|context| {
                context
                    .items
                    .iter()
                    .any(|item| item.kind == ContextKind::Plan && !item.linked)
            })
    }

    /// The research the Overview lists as work rows: the setup's research without roadmap
    /// planning, which has its own Roadmap line. Takes the screen.
    /// Returns the kinds in setup order, none before the setup is read.
    pub(in crate::tui) fn work_kinds(&self) -> Vec<ResearchKind> {
        self.setup
            .iter()
            .flat_map(|setup| setup.research.iter().copied())
            .filter(|kind| *kind != ResearchKind::RoadmapPlanning)
            .collect()
    }

    /// Answers Plan roadmap now and Retry for the roadmap planning line once the jobs are
    /// read. Either one asks first when it can replace the roadmap documents.
    /// Takes the key. Returns the change, the handled reply with its confirm or plain line, or
    /// none for a key the planning line does not use.
    fn planning_reply(&mut self, key: KeyCode) -> Option<ProjectReply> {
        let state = planning(self.jobs.as_ref()?);
        if refreshes_status(&state, key) {
            if let Err(line) = self.roadmap.refresh() {
                self.problem = Some(line.into());
            }
            return Some(ProjectReply::Handled);
        }
        match planning_key(&state, &self.project, key)? {
            Ok(action) if self.plan_replaces(&state) => {
                self.confirm_plan = Some(action);
                Some(ProjectReply::Handled)
            }
            Ok(action) => Some(ProjectReply::Change(Box::new(action))),
            Err(line) => {
                self.problem = Some(line.into());
                Some(ProjectReply::Handled)
            }
        }
    }

    fn own_order(
        &self,
        schedule: &crate::scheduling::Schedule,
    ) -> Option<crate::scheduling::Strategy> {
        schedule
            .projects
            .iter()
            .find(|entry| entry.id.as_str() == self.project.as_str())
            .map(|entry| entry.strategy)
    }

    /// The running agents the team graph names as able to open a folder hidden after they
    /// started (SA-VID14-A), while the project hides any folder. Takes the screen. Returns
    /// their labels in graph order, none while nothing is hidden or the graph loads.
    pub fn hidden_seers(&self) -> Vec<&str> {
        let hides = self
            .hidden_paths
            .as_ref()
            .is_some_and(|saved| !saved.paths.is_empty());
        match self.team.ready() {
            Some(graph) if hides => graph
                .nodes
                .iter()
                .filter(|node| node.sees_hidden)
                .map(|node| node.label.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The project's account order for its Settings tab: its own strategy, or the one it
    /// follows from its pool or the app. Takes the screen. Returns the text, or none while
    /// the schedule loads.
    pub fn account_order(&self) -> Option<String> {
        let schedule = self.schedule.as_ref()?;
        if let Some(own) = self.own_order(schedule) {
            return Some(format!("{} (this project)", own.label()));
        }
        let pools: Vec<&CredentialRef> = self
            .pool
            .iter()
            .filter(|pool| pool.assigned)
            .map(|pool| &pool.id)
            .collect();
        Some(format!(
            "Default ({})",
            schedule.strategy_for(None, &pools).label()
        ))
    }

    /// Handles one key in the read-only folders box: Enter saves the folders typed between
    /// colons, an empty box clears them, Esc closes it.
    /// Takes the key event.
    /// Returns the change to send, or what the box did with the key.
    pub(super) fn folders_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(folders) = self.folders.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.folders = None;
                ProjectReply::Handled
            }
            _ if folders.busy => ProjectReply::Handled,
            KeyCode::Enter => {
                let paths: Vec<String> = folders
                    .input
                    .value()
                    .split(':')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect();
                let action = SetupAction::SetReadPaths {
                    project_id: self.project.clone(),
                    version: self.folders_version,
                    paths,
                };
                if let Err(error) = action.validate() {
                    folders.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                folders.busy = true;
                folders.error = None;
                ProjectReply::Change(Box::new(action))
            }
            _ => {
                if folders.input.handle(key) {
                    folders.error = None;
                }
                ProjectReply::Handled
            }
        }
    }

    /// Handles one key in the hidden folders box: Enter saves the folders typed between colons,
    /// an empty box clears them, Esc closes it.
    /// Takes the key event.
    /// Returns the change to send, or what the box did with the key.
    pub(super) fn hidden_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(hidden) = self.hidden.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.hidden = None;
                ProjectReply::Handled
            }
            _ if hidden.busy => ProjectReply::Handled,
            KeyCode::Enter => {
                let paths: Vec<String> = hidden
                    .input
                    .value()
                    .split(':')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect();
                let action = SetupAction::SetHiddenPaths {
                    project_id: self.project.clone(),
                    version: self.hidden_version,
                    paths,
                };
                if let Err(error) = action.validate() {
                    hidden.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                hidden.busy = true;
                hidden.error = None;
                ProjectReply::Change(Box::new(action))
            }
            _ => {
                if hidden.input.handle(key) {
                    hidden.error = None;
                }
                ProjectReply::Handled
            }
        }
    }

    /// Handles one key in the Agent access box (see [`AccessEdit::key`]): a save sends the
    /// policy and keeps the box busy until the engine answers.
    /// Takes the key event.
    /// Returns the change to send, or what the box did with the key.
    pub(super) fn access_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(access) = self.access.as_mut() else {
            return ProjectReply::Ignored;
        };
        match access.key(key) {
            AccessKey::Handled => ProjectReply::Handled,
            AccessKey::Close => {
                self.access = None;
                ProjectReply::Handled
            }
            AccessKey::Save(policy) => {
                let action = SetupAction::SetAgentPolicy {
                    project_id: self.project.clone(),
                    version: access.view.version,
                    policy,
                };
                if let Err(error) = action.validate() {
                    access.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                access.busy = true;
                access.error = None;
                ProjectReply::Change(Box::new(action))
            }
        }
    }

    pub(super) fn add_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(adding) = self.adding.as_mut() else {
            return ProjectReply::Ignored;
        };
        match key.code {
            KeyCode::Esc => {
                self.adding = None;
                ProjectReply::Handled
            }
            _ if adding.busy => ProjectReply::Handled,
            KeyCode::Enter => {
                let typed = adding.input.value().trim();
                if typed.is_empty() {
                    adding.error = Some("Type the full path of a file on this computer.".into());
                    return ProjectReply::Handled;
                }
                let path = match (typed.strip_prefix("~/"), &self.home) {
                    (Some(rest), Some(home)) => home.join(rest).display().to_string(),
                    _ => typed.to_owned(),
                };
                let action = SetupAction::AddDocuments {
                    project_id: self.project.clone(),
                    documents: vec![path],
                };
                if let Err(error) = action.validate() {
                    adding.error = Some(plain_problem(&error.0));
                    return ProjectReply::Handled;
                }
                adding.busy = true;
                adding.error = None;
                ProjectReply::Change(Box::new(action))
            }
            _ => {
                if adding.input.handle(key) {
                    adding.error = None;
                }
                ProjectReply::Handled
            }
        }
    }

    pub(super) fn general_key(&mut self, key: KeyEvent) -> ProjectReply {
        let Some(general) = self.general.as_mut() else {
            return ProjectReply::Ignored;
        };
        let last = GENERAL_FIELDS - 1;
        match key.code {
            KeyCode::Esc => {
                self.general = None;
                return ProjectReply::Handled;
            }
            _ if general.busy => return ProjectReply::Handled,
            KeyCode::Tab | KeyCode::Down => general.cursor = (general.cursor + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => general.cursor = general.cursor.saturating_sub(1),
            KeyCode::Char(' ') if !general.on_text() => general.toggle(),
            KeyCode::Enter => return self.save_general(),
            _ => {
                if let Some(field) = general.text() {
                    field.handle(key);
                }
            }
        }
        if let Some(general) = self.general.as_mut() {
            general.error = None;
        }
        ProjectReply::Handled
    }

    fn save_general(&mut self) -> ProjectReply {
        let action = match (&self.general, &self.setup) {
            (Some(general), Some(setup)) => general.action(setup),
            _ => None,
        };
        let Some(action) = action else {
            self.general = None;
            return ProjectReply::Handled;
        };
        let Some(general) = self.general.as_mut() else {
            return ProjectReply::Handled;
        };
        if let Err(error) = action.validate() {
            general.error = Some(plain_problem(&error.0));
            return ProjectReply::Handled;
        }
        general.busy = true;
        general.error = None;
        ProjectReply::Change(Box::new(action))
    }

    pub(super) fn assert_consistent(&self) {
        assert!(self.overview_tab < OVERVIEW_TAB_LABELS.len());
        assert!(self.overview_scroll <= OVERVIEW_SCROLL_MAX);
        if let Some(cursor) = self.pool_picker {
            assert!(!self.pool_choices.is_empty() && cursor <= self.pool_choices.len());
        }
        if let Some(jobs) = &self.jobs {
            assert!(self.agents_cursor <= jobs.len());
        }
        if let Some(context) = &self.context {
            assert!(self.tree_cursor <= context.items.len());
        }
        assert!(!self.reading || self.preview.is_some() || self.tab != Tab::Context);
        assert!(self.confirm_stop.is_none() || self.confirm_delete.is_none());
        let boxes = [
            self.edit.is_some(),
            self.review.is_some(),
            self.adding.is_some(),
            self.folders.is_some(),
            self.hidden.is_some(),
            self.access.is_some(),
            self.general.is_some(),
            self.confirm_remove,
            self.confirm_move,
        ];
        assert!(boxes.iter().filter(|open| **open).count() <= 1);
        if self.remove_choice == RemoveChoice::Everything {
            assert!(self.confirm_remove);
            assert!(
                self.removal
                    .as_ref()
                    .is_some_and(|removal| removal.can_delete)
            );
        }
        assert!(self.confirm_remove || (self.removal.is_none() && self.remove_typed.is_empty()));
        assert!(!self.remove_everything || (self.confirm_remove && self.removal.is_none()));
        assert!(self.remove_typed.len() <= NAME_BYTES_MAX);
        if let Some(review) = &self.review {
            assert!(review.scroll < review.lines().len().max(1));
        }
        if let Some(general) = &self.general {
            assert!(general.cursor < GENERAL_FIELDS);
        }
        if self.adding.is_some() {
            assert_eq!(self.tab, Tab::Context);
        }
        if self.general.is_some()
            || self.confirm_remove
            || self.confirm_move
            || self.folders.is_some()
            || self.hidden.is_some()
            || self.access.is_some()
        {
            assert_eq!(self.tab, Tab::Settings);
        }
        if let Some(job) = self.confirm_stop {
            assert_eq!(self.tab, Tab::Agents);
            assert!(job > 0);
        }
        if let Some(action) = &self.confirm_plan {
            assert!(matches!(self.tab, Tab::Overview | Tab::Settings));
            assert!(matches!(
                action,
                SetupAction::PlanRoadmap { .. } | SetupAction::RetryResearch { .. }
            ));
        }
        if let Some(log) = &self.log {
            assert!(log.job > 0);
            assert!(
                log.lines
                    .as_ref()
                    .is_none_or(|lines| lines.len() <= RESEARCH_LOG_LINES_MAX)
            );
        }
        if let Some(preview) = &self.preview {
            assert!(
                preview.path.starts_with(CONTEXT_DIRECTORY)
                    || preview
                        .path
                        .starts_with(crate::project_setup::LINKED_PREFIX)
            );
        }
    }
}
