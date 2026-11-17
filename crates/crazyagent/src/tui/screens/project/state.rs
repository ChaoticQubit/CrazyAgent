use super::*;

impl ProjectScreen {
    /// Opens a project on its Overview tab.
    /// Takes the project, its name from the engine snapshot and the owner's home folder.
    /// Returns the screen; engine reads start with `next_query`.
    pub fn new(project: ProjectId, name: String, home: Option<PathBuf>) -> Self {
        Self {
            plugins: ProjectPlugins::new(project.clone()),
            changes: ProjectChanges::new(project.clone()),
            questions: ProjectQuestions::new(project.clone()),
            recovery: ProjectRecovery::new(project.clone()),
            roadmap: StatusLine::new(project.clone()),
            github_notice: false,
            sessions: Sessions::default(),
            project,
            name,
            tab: Tab::Overview,
            width: 80,
            home,
            setup: None,
            setup_missing: false,
            jobs: None,
            context: None,
            memories: None,
            pool: None,
            pool_ready: true,
            pool_choices: Vec::new(),
            pool_picker: None,
            transcript: None,
            team: Load::Idle,
            input: TextInput::multi(MESSAGE_BYTES_MAX),
            typing: false,
            history: console::History::default(),
            menu_cursor: 0,
            console_note: Vec::new(),
            cleared: None,
            files: None,
            transcript_scroll: 0,
            activity_open: false,
            overview_cursor: 0,
            overview_tabs: None,
            overview_tab: 0,
            overview_scroll: 0,
            agents_cursor: 0,
            tree_cursor: 0,
            preview: None,
            reading: false,
            edit: None,
            review: None,
            confirm_delete: None,
            confirm_stop: None,
            confirm_remove: false,
            confirm_move: false,
            removal: None,
            remove_choice: RemoveChoice::Dashboard,
            remove_everything: false,
            remove_typed: String::new(),
            confirm_plan: None,
            adding: None,
            folders: None,
            read_paths: None,
            hidden: None,
            hidden_paths: None,
            access: None,
            agent_policy: None,
            schedule: None,
            general: None,
            removed: false,
            log: None,
            problem: None,
            now_ms: 0,
            offset: 0,
            jobs_read_ms: None,
            context_read_ms: None,
            read_paths_read_ms: None,
            folders_version: 0,
            folders_stale: false,
            hidden_paths_read_ms: None,
            hidden_version: 0,
            hidden_stale: false,
            agent_policy_read_ms: None,
            access_stale: false,
            memories_read_ms: None,
            overview_read_ms: None,
            removal_read_ms: None,
            setup_failed_ms: None,
            setup_due: false,
            waiting: None,
        }
    }

    /// Opens one tab.
    /// Takes the tab; the Conversation tab puts focus in the message input, and the Overview
    /// reads its tabs again, since a tab file can change outside CrazyAgent.
    /// Returns nothing.
    pub fn open(&mut self, tab: Tab) {
        self.tab = tab;
        if tab == Tab::Overview {
            self.overview_read_ms = None;
        }
        self.reading = false;
        self.confirm_delete = None;
        self.confirm_stop = None;
        self.close_remove();
        self.confirm_plan = None;
        self.adding = None;
        self.folders = None;
        self.access = None;
        self.general = None;
        self.typing = tab == Tab::Conversation;
        if tab == Tab::Changes {
            self.changes.refresh();
        }
        if tab == Tab::Questions {
            self.questions.refresh();
        }
    }

    /// Gives the next engine read this screen needs, at most one at a time.
    /// Takes the screen and the current time in milliseconds.
    /// Returns the query to send, or none while one is waiting or nothing is due.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        if self.waiting.is_some() {
            return None;
        }
        let project = self.project.clone();
        let due = |last: Option<i64>| last.is_none_or(|last| now_ms - last >= REFRESH_MS);
        let setup_wanted = (self.setup.is_none() || self.setup_due) && !self.setup_missing;
        let query = if self.confirm_remove && self.removal.is_none() && due(self.removal_read_ms) {
            SetupQuery::Removal { project }
        } else if setup_wanted && due(self.setup_failed_ms) {
            SetupQuery::Setup { project }
        } else if self.setup_missing || self.setup.is_none() {
            return None;
        } else if self.jobs_read_ms.is_none() || (due(self.jobs_read_ms) && self.jobs_live()) {
            SetupQuery::Jobs { project }
        } else if self.tab == Tab::Overview && self.overview_due(now_ms) {
            SetupQuery::Overview { project }
        } else if let Some(job) = self.log_due(now_ms) {
            SetupQuery::Log { project, job }
        } else if due(self.context_read_ms)
            && (self.context.is_none() || matches!(self.tab, Tab::Context))
        {
            SetupQuery::Context { project }
        } else if self.read_paths.is_none()
            && self.tab == Tab::Settings
            && due(self.read_paths_read_ms)
        {
            SetupQuery::ReadPaths { project }
        } else if self
            .hidden_paths
            .as_ref()
            .is_none_or(|saved| !saved.runs.is_empty())
            && self.tab == Tab::Settings
            && due(self.hidden_paths_read_ms)
        {
            SetupQuery::HiddenPaths { project }
        } else if self.agent_policy.is_none()
            && self.tab == Tab::Settings
            && due(self.agent_policy_read_ms)
        {
            SetupQuery::AgentPolicy { project }
        } else if due(self.memories_read_ms)
            && (self.memories.is_none() || matches!(self.tab, Tab::Overview))
        {
            SetupQuery::Memories {
                project,
                after: None,
            }
        } else if let Some(preview) = self
            .preview
            .as_ref()
            .filter(|p| p.text.is_empty() && !p.binary && !p.loaded && p.problem.is_none())
        {
            SetupQuery::Item {
                project,
                path: preview.path.clone(),
                offset: 0,
            }
        } else {
            let preview = self
                .preview
                .as_ref()
                .filter(|p| p.next.is_some() && p.text.len() < PREVIEW_BYTES_MAX)?;
            SetupQuery::Item {
                project,
                path: preview.path.clone(),
                offset: preview.next.unwrap_or(0),
            }
        };
        self.waiting = Some(query.clone());
        Some(query)
    }

    /// Says whether the Overview tabs are read now: at once before the first read, after a
    /// refresh was asked and when the owner opens the Overview or picks one of its tabs, then
    /// every `REFRESH_MS` only until they are read once, while a run writes them, or while a
    /// landed change set waits for a run that can start.
    /// Takes the screen and the current time in milliseconds. Returns whether to read.
    fn overview_due(&self, now_ms: i64) -> bool {
        let Some(read_ms) = self.overview_read_ms else {
            return true;
        };
        if now_ms.saturating_sub(read_ms) < REFRESH_MS {
            return false;
        }
        self.overview_tabs.as_ref().is_none_or(overview_pending)
    }

    /// Answers Shift+R on the Overview: asks for one writing run of the tabs, or says why
    /// none can start now. Takes the screen. Returns the ask, or handled with the reason
    /// shown; ignored before the tabs were read.
    pub(super) fn refresh_overview(&mut self) -> ProjectReply {
        let Some(overview) = &self.overview_tabs else {
            return ProjectReply::Ignored;
        };
        let reason = if overview.needs_home {
            Some(plain_problem(crate::project_overview::NEEDS_HOME))
        } else if overview.refresh == crate::roadmap_status::Refresh::Off {
            Some(plain_problem("overview_off"))
        } else if overview.running {
            Some("A run is writing the tabs now. They fill in when it ends.".to_owned())
        } else {
            None
        };
        match reason {
            Some(reason) => {
                assert!(!reason.is_empty());
                self.problem = Some(reason);
                ProjectReply::Handled
            }
            None => ProjectReply::RefreshOverview,
        }
    }

    /// Forgets the read in flight after the engine connection dropped, so the next read is sent again.
    /// Takes the screen; loaded data stays on screen.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        self.waiting = None;
    }

    /// Gives the research job focused on the Agents tab.
    /// Takes the screen.
    /// Returns the job under the cursor, or none before the jobs are read.
    pub fn focused_job(&self) -> Option<&ResearchJob> {
        self.jobs.as_ref()?.get(self.agents_cursor)
    }

    fn log_due(&self, now_ms: i64) -> Option<i64> {
        if self.tab != Tab::Agents {
            return None;
        }
        let job = self.focused_job()?;
        let fresh = self.log.as_ref().is_some_and(|log| {
            log.job == job.id
                && log.status == job.status
                && (job.status != ResearchStatus::Running || now_ms - log.read_ms < REFRESH_MS)
        });
        (!fresh).then_some(job.id)
    }

    fn log_read(&mut self, job: i64, lines: Option<Vec<String>>) {
        let Some(status) = self
            .jobs
            .iter()
            .flatten()
            .find(|known| known.id == job)
            .map(|known| known.status)
        else {
            return;
        };
        let lines = match (lines, self.log.take()) {
            (None, Some(old)) if old.job == job => old.lines,
            (Some(mut lines), _) => {
                let start = lines.len().saturating_sub(RESEARCH_LOG_LINES_MAX);
                Some(lines.split_off(start))
            }
            (None, _) => None,
        };
        self.log = Some(JobLog {
            job,
            status,
            lines,
            read_ms: self.now_ms,
        });
    }

    fn jobs_live(&self) -> bool {
        self.jobs.as_ref().is_none_or(|jobs| {
            jobs.iter().any(|job| {
                matches!(
                    job.status,
                    ResearchStatus::Queued | ResearchStatus::Running | ResearchStatus::NeedsPool
                )
            })
        })
    }

    /// Applies one engine read or change result.
    /// Takes the query that was sent and the engine's view or plain problem.
    /// Returns nothing; a failed read keeps the last good data and shows the problem.
    pub fn apply(&mut self, query: &SetupQuery, result: Result<SetupView, String>) {
        if self.waiting.as_ref() == Some(query) {
            self.waiting = None;
        }
        let view = match result {
            Ok(view) => view,
            Err(problem) if problem == "project_setup_missing" => {
                self.setup_missing = true;
                return;
            }
            Err(problem) => {
                if self.refused(query, &problem) {
                    return;
                }
                match query {
                    SetupQuery::Setup { .. } => {
                        self.setup_due = false;
                        self.setup_failed_ms = Some(self.now_ms);
                    }
                    SetupQuery::Jobs { .. } => self.jobs_read_ms = Some(self.now_ms),
                    SetupQuery::Context { .. } => self.context_read_ms = Some(self.now_ms),
                    SetupQuery::ReadPaths { .. } => self.read_paths_read_ms = Some(self.now_ms),
                    SetupQuery::HiddenPaths { .. } => self.hidden_paths_read_ms = Some(self.now_ms),
                    SetupQuery::AgentPolicy { .. } => {
                        self.agent_policy_read_ms = Some(self.now_ms);
                    }
                    SetupQuery::Memories { .. } => self.memories_read_ms = Some(self.now_ms),
                    SetupQuery::Overview { .. } => self.overview_read_ms = Some(self.now_ms),
                    SetupQuery::RefreshOverview { .. } => self.overview_read_ms = None,
                    SetupQuery::Removal { .. } => self.removal_read_ms = Some(self.now_ms),
                    SetupQuery::Item { path, offset, .. } => {
                        if let Some(preview) = self
                            .preview
                            .as_mut()
                            .filter(|p| &p.path == path && p.wanted() == Some(*offset))
                        {
                            preview.next = None;
                            preview.problem = Some(plain_problem(&problem));
                            preview.cache = None;
                        }
                    }
                    SetupQuery::Log { job, .. } => self.log_read(*job, None),
                    SetupQuery::Change {
                        action:
                            SetupAction::CancelResearch { .. }
                            | SetupAction::RetryResearch { .. }
                            | SetupAction::PlanRoadmap { .. },
                        ..
                    } => self.jobs_read_ms = None,
                    _ => {}
                }
                self.problem = Some(plain_problem(&problem));
                return;
            }
        };
        match view {
            SetupView::Setup(Some(setup)) => {
                if let Some(general) = self.general.as_mut().filter(|general| general.stale) {
                    let now = GeneralSeen::of(&setup);
                    let changed = general.seen.changed(&now);
                    general.seen = now;
                    general.revision = setup.revision;
                    general.stale = false;
                    general.error = Some(if changed.is_empty() {
                        "This project changed elsewhere, but not on this form. Enter saves your edit; Esc keeps the saved one.".to_owned()
                    } else {
                        format!("Changed elsewhere: {}. {KEEP_MINE}", changed.join(", "))
                    });
                }
                self.name = setup.name.clone();
                self.setup = Some(*setup);
                self.setup_due = false;
                self.setup_failed_ms = None;
            }
            SetupView::Setup(None) => self.setup_missing = true,
            SetupView::Jobs(jobs) => {
                self.agents_cursor = self.agents_cursor.min(jobs.len().saturating_sub(1));
                self.jobs = Some(jobs);
                self.jobs_read_ms = Some(self.now_ms);
            }
            SetupView::Context(context) => {
                self.tree_cursor = self.tree_cursor.min(context.items.len().saturating_sub(1));
                self.context = Some(context);
                self.context_read_ms = Some(self.now_ms);
            }
            SetupView::Memories(memories) => {
                self.memories = Some(memories);
                self.memories_read_ms = Some(self.now_ms);
            }
            SetupView::ReadPaths(saved) => {
                if std::mem::take(&mut self.folders_stale)
                    && let Some(folders) = self.folders.as_mut()
                {
                    self.folders_version = saved.version;
                    let now = if saved.paths.is_empty() {
                        "no extra folders".to_owned()
                    } else {
                        saved.paths.join(":")
                    };
                    folders.error = Some(format!("Saved now: {now}. {KEEP_MINE}"));
                }
                self.read_paths = Some(saved);
                self.read_paths_read_ms = Some(self.now_ms);
            }
            SetupView::HiddenPaths(saved) => {
                if std::mem::take(&mut self.hidden_stale)
                    && let Some(hidden) = self.hidden.as_mut()
                {
                    self.hidden_version = saved.version;
                    let now = if saved.paths.is_empty() {
                        "no hidden folders".to_owned()
                    } else {
                        saved.paths.join(":")
                    };
                    hidden.error = Some(format!("Saved now: {now}. {KEEP_MINE}"));
                }
                self.hidden_paths = Some(saved);
                self.hidden_paths_read_ms = Some(self.now_ms);
            }
            SetupView::AgentPolicy(view) => {
                if std::mem::take(&mut self.access_stale)
                    && let Some(access) = self.access.as_mut()
                {
                    access.view.version = view.version;
                    access.error = Some(format!(
                        "Saved now: {}. {KEEP_MINE}",
                        super::super::project_access::summary(Some(&view))
                    ));
                }
                self.agent_policy = Some(view);
                self.agent_policy_read_ms = Some(self.now_ms);
            }
            SetupView::Item(chunk) => {
                if let Some(preview) = self
                    .preview
                    .as_mut()
                    .filter(|p| p.path == chunk.path && p.wanted() == Some(chunk.offset))
                {
                    preview.loaded = true;
                    match chunk.text {
                        Some(text) => {
                            preview.text.push_str(&text);
                            preview.next = chunk.next_offset;
                        }
                        None => {
                            preview.binary = true;
                            preview.next = None;
                        }
                    }
                    preview.cache = None;
                }
            }
            SetupView::Log(lines) => {
                if let SetupQuery::Log { job, .. } = query {
                    self.log_read(*job, Some(lines));
                }
            }
            SetupView::Changed(result) => self.changed(query, result),
            SetupView::Overview(overview) => {
                self.overview_tabs = Some(*overview);
                self.overview_read_ms = Some(self.now_ms);
            }
            SetupView::OverviewAsked => self.overview_read_ms = None,
            SetupView::LiveWithoutSession(why) => {
                self.changed(query, crate::engine::owner::OwnerResult::Applied);
                self.problem = Some(format!(
                    "{} {}",
                    crate::tui::screens::team::MASTER_NO_SESSION,
                    crate::tui::screens::team::unopened(&why)
                ));
            }
            SetupView::Removal(preview) => {
                self.removal_read_ms = Some(self.now_ms);
                if self.confirm_remove {
                    if !preview.can_delete {
                        self.remove_choice = RemoveChoice::Dashboard;
                    } else if self.remove_everything {
                        self.remove_choice = RemoveChoice::Everything;
                    }
                    self.remove_everything = false;
                    self.removal = Some(*preview);
                }
            }
            SetupView::Catalog(_)
            | SetupView::Team(_)
            | SetupView::TeamMessages(_)
            | SetupView::CoordQuestions(_)
            | SetupView::Library(_) => {}
        }
        self.assert_consistent();
    }

    /// Puts a refused add, edit or remove back in its box with the reason. A General,
    /// read-only folders, hidden folders or Agent access edit refused as stale keeps its draft
    /// and reads the saved value again.
    /// Takes the change query and the plain problem code.
    /// Returns true when an open box took the refusal.
    fn refused(&mut self, query: &SetupQuery, code: &str) -> bool {
        let SetupQuery::Change { action, .. } = query else {
            return false;
        };
        let stale = code == crate::storage::versioned::STALE_CODE;
        let reason = Some(if stale {
            STALE_READING.to_owned()
        } else {
            plain_problem(code)
        });
        match action {
            SetupAction::AddDocuments { .. } if self.adding.is_some() => {
                if let Some(adding) = self.adding.as_mut() {
                    adding.busy = false;
                    adding.error = reason;
                }
                true
            }
            SetupAction::SetReadPaths { .. } if self.folders.is_some() => {
                if let Some(folders) = self.folders.as_mut() {
                    folders.busy = false;
                    folders.error = reason;
                }
                if stale {
                    self.folders_stale = true;
                    self.read_paths = None;
                    self.read_paths_read_ms = None;
                }
                true
            }
            SetupAction::SetHiddenPaths { .. } if self.hidden.is_some() => {
                if let Some(hidden) = self.hidden.as_mut() {
                    hidden.busy = false;
                    hidden.error = reason;
                }
                if stale {
                    self.hidden_stale = true;
                    self.hidden_paths = None;
                    self.hidden_paths_read_ms = None;
                }
                true
            }
            SetupAction::SetAgentPolicy { .. } if self.access.is_some() => {
                if let Some(access) = self.access.as_mut() {
                    access.busy = false;
                    access.error = reason;
                }
                if stale {
                    self.access_stale = true;
                    self.agent_policy = None;
                    self.agent_policy_read_ms = None;
                }
                true
            }
            SetupAction::Update { .. } if self.general.is_some() => {
                if let Some(general) = self.general.as_mut() {
                    general.busy = false;
                    general.error = reason;
                    general.stale |= stale;
                }
                self.setup_due |= stale;
                true
            }
            _ => false,
        }
    }

    fn changed(&mut self, query: &SetupQuery, result: crate::engine::owner::OwnerResult) {
        use crate::engine::owner::OwnerResult;
        let applied = matches!(result, OwnerResult::Applied);
        match &result {
            OwnerResult::Applied => {}
            OwnerResult::Rejected { code } => {
                if !self.refused(query, code) {
                    self.problem = Some(plain_problem(code));
                }
            }
            _ => {
                self.refused(query, "change_outcome_unknown");
            }
        }
        match query {
            SetupQuery::Change {
                action: SetupAction::AddDocuments { .. },
                ..
            } => {
                self.context_read_ms = None;
                if applied {
                    self.adding = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::Update { .. },
                ..
            } => {
                self.setup_due = true;
                if applied {
                    self.general = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::Remove { .. } | SetupAction::Delete { .. },
                ..
            } => self.removed = applied,
            SetupQuery::Change {
                action: SetupAction::SetReadPaths { .. },
                ..
            } => {
                self.read_paths = None;
                self.read_paths_read_ms = None;
                if applied {
                    self.folders = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::SetHiddenPaths { .. },
                ..
            } => {
                self.hidden_paths = None;
                self.hidden_paths_read_ms = None;
                if applied {
                    self.hidden = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::SetAgentPolicy { .. },
                ..
            } => {
                self.agent_policy = None;
                self.agent_policy_read_ms = None;
                if applied {
                    self.access = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::DeleteContext { path, .. },
                ..
            } if applied => {
                if self.preview.as_ref().is_some_and(|p| &p.path == path) {
                    self.preview = None;
                    self.reading = false;
                }
                self.context_read_ms = None;
                self.context = None;
            }
            SetupQuery::Change {
                action:
                    action @ (SetupAction::CancelResearch { .. } | SetupAction::RetryResearch { .. }),
                ..
            } => {
                self.jobs_read_ms = None;
                self.setup_due |= applied && matches!(action, SetupAction::RetryResearch { .. });
                if applied {
                    self.log = None;
                }
            }
            SetupQuery::Change {
                action: SetupAction::PlanRoadmap { .. },
                ..
            } => {
                self.jobs_read_ms = None;
                self.setup_due = true;
            }
            SetupQuery::Change { .. } | SetupQuery::ReviewMemory { .. } => {
                self.memories_read_ms = None;
                self.memories = None;
                self.context_read_ms = None;
            }
            _ => {}
        }
    }

    /// Lists the memories waiting for the owner's approval.
    /// Takes the screen.
    /// Returns suggested memories, oldest first.
    pub fn suggested(&self) -> Vec<&KnowledgeEntry> {
        self.memories
            .as_ref()
            .map(|memories| {
                memories
                    .iter()
                    .filter(|m| m.status == KnowledgeStatus::Provisional && !m.human_reviewed)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lists the context files in tree order: brief, research, then documents.
    /// Takes the screen.
    /// Returns the items, or an empty list before the first read.
    pub fn tree(&self) -> Vec<&ContextItem> {
        let mut items: Vec<&ContextItem> = self
            .context
            .as_ref()
            .map(|context| context.items.iter().collect())
            .unwrap_or_default();
        items.sort_by_key(|item| {
            (
                match item.kind {
                    ContextKind::Brief | ContextKind::Plan => 0,
                    ContextKind::Research => 1,
                    ContextKind::Document => 2,
                },
                item.path.clone(),
            )
        });
        items
    }

    /// Finds the research job writing one report.
    /// Takes the context path of the report.
    /// Returns the job, when one exists.
    pub fn job_for(&self, path: &str) -> Option<&ResearchJob> {
        self.jobs
            .as_ref()?
            .iter()
            .filter(|job| job.report_path == path)
            .max_by_key(|job| job.id)
    }

    /// Counts agents by state for the header and the Agents title.
    /// Takes the screen.
    /// Returns running, waiting, failed and done counts.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for job in self.jobs.iter().flatten() {
            match job.status {
                ResearchStatus::Running => counts.0 += 1,
                ResearchStatus::Queued | ResearchStatus::NeedsPool => counts.1 += 1,
                ResearchStatus::Failed => counts.2 += 1,
                ResearchStatus::Done => counts.3 += 1,
                ResearchStatus::Cancelled => {}
            }
        }
        counts
    }

    /// Names one context file for the desktop IDE by its path inside the project folder.
    /// Takes the path the context tree lists. Returns the target, or none before the
    /// project folder is read or when the context folder is outside it.
    pub fn ide_target(&self, path: &str) -> Option<RevealTarget> {
        let setup = self.setup.as_ref()?;
        let Some(OpenRequest::Context { folder, path }) = self.context_request(path) else {
            return None;
        };
        let inside = Path::new(&folder)
            .strip_prefix(setup.project_folder())
            .ok()?
            .join(path);
        Some(RevealTarget::Project {
            path: inside.to_str()?.to_owned(),
        })
    }

    /// Names one context file to open: the folder its context paths start from, as the
    /// engine listed them, and the path inside it; a linked repository file, listed as
    /// `repo/<path>`, is named by the project folder and its path below it. Takes the path
    /// the context tree lists. Returns the request, or none before the folder is known.
    pub fn context_request(&self, path: &str) -> Option<OpenRequest> {
        if let Some(linked) = path.strip_prefix(crate::project_setup::LINKED_PREFIX) {
            if linked.is_empty() {
                return None;
            }
            let folder = self.project_folder()?;
            return Some(OpenRequest::Context {
                folder: folder.to_string_lossy().into_owned(),
                path: linked.to_owned(),
            });
        }
        let folder = self
            .context
            .as_ref()
            .map(|context| context.folder.clone())
            .or_else(|| {
                self.setup
                    .as_ref()
                    .map(|setup| setup.state_folder().to_string_lossy().into_owned())
            })?;
        Some(OpenRequest::Context {
            folder,
            path: path.to_owned(),
        })
    }

    /// The project's own folder, where the owner's copies of changed files live.
    /// Takes the screen. Returns the folder, or none before the setup is read.
    pub fn project_folder(&self) -> Option<PathBuf> {
        self.setup.as_ref().map(StoredSetup::project_folder)
    }

    /// True while a box or question is open: `key` gives it every key before any tab or
    /// panel, so a panel must offer keys only while this is false.
    /// Output: whether an edit, review, add, folders or General box, or the remove, pool,
    /// delete or stop question, is open.
    pub fn boxed(&self) -> bool {
        self.edit.is_some()
            || self.review.is_some()
            || self.adding.is_some()
            || self.folders.is_some()
            || self.hidden.is_some()
            || self.access.is_some()
            || self.general.is_some()
            || self.confirm_remove
            || self.confirm_move
            || self.pool_picker.is_some()
            || self.confirm_delete.is_some()
            || self.confirm_stop.is_some()
    }

    /// Handles one key for the open tab, the edit box or the delete question.
    /// Takes the key event.
    /// Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> ProjectReply {
        self.problem = None;
        if self.edit.is_some() {
            return self.edit_key(key);
        }
        if self.review.is_some() {
            return self.review_key(key);
        }
        if self.adding.is_some() {
            return self.add_key(key);
        }
        if self.folders.is_some() {
            return self.folders_key(key);
        }
        if self.hidden.is_some() {
            return self.hidden_key(key);
        }
        if self.access.is_some() {
            return self.access_key(key);
        }
        if self.general.is_some() {
            return self.general_key(key);
        }
        if self.confirm_remove {
            return self.remove_key(key);
        }
        if self.confirm_move {
            return self.move_key(key);
        }
        if let Some(cursor) = self.pool_picker {
            return self.picker_key(key, cursor);
        }
        if let Some(path) = self.confirm_delete.clone() {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_delete = None;
                    ProjectReply::Change(Box::new(SetupAction::DeleteContext {
                        project_id: self.project.clone(),
                        path,
                    }))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = None;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        if let Some(action) = self.confirm_plan.clone() {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_plan = None;
                    ProjectReply::Change(Box::new(action))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_plan = None;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        if let Some(job) = self.confirm_stop {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_stop = None;
                    ProjectReply::Change(Box::new(SetupAction::CancelResearch {
                        project_id: self.project.clone(),
                        job,
                    }))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_stop = None;
                    ProjectReply::Handled
                }
                _ => ProjectReply::Handled,
            };
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Left if shift => return self.tab_by(-1),
            KeyCode::Right if shift => return self.tab_by(1),
            KeyCode::Tab
                if self.tab == Tab::Conversation && !(self.typing && self.picks().len() > 0) =>
            {
                self.typing = !self.typing;
                return ProjectReply::Handled;
            }
            _ => {}
        }
        if self.tab == Tab::Conversation && self.typing {
            return self.input_key(key);
        }
        if !control {
            match key.code {
                KeyCode::Char('[') => return self.tab_by(-1),
                KeyCode::Char(']') => return self.tab_by(1),
                _ => {}
            }
        }
        let reply = match self.tab {
            Tab::Overview => self.overview_key(key),
            Tab::Conversation => self.conversation_key(key),
            Tab::Agents => self.agents_key(key),
            Tab::Changes => ProjectReply::Ignored,
            Tab::Questions => ProjectReply::Ignored,
            Tab::Context => self.context_key(key),
            Tab::Settings => self.settings_key(key),
        };
        if reply == ProjectReply::Ignored && key.code == KeyCode::Esc {
            return ProjectReply::Home;
        }
        self.assert_consistent();
        reply
    }

    /// Inserts pasted text into the message input or the memory edit box.
    /// Takes the pasted text.
    /// Returns Handled when a field took it.
    pub fn paste(&mut self, text: &str) -> ProjectReply {
        if let Some(edit) = self.edit.as_mut() {
            edit.input.insert_str(text);
            return ProjectReply::Handled;
        }
        if self.review.is_some() {
            return ProjectReply::Handled;
        }
        if let Some(adding) = self.adding.as_mut().filter(|adding| !adding.busy) {
            adding.input.insert_str(text);
            adding.error = None;
            return ProjectReply::Handled;
        }
        if let Some(folders) = self.folders.as_mut().filter(|folders| !folders.busy) {
            folders.input.insert_str(text);
            folders.error = None;
            return ProjectReply::Handled;
        }
        if let Some(hidden) = self.hidden.as_mut().filter(|hidden| !hidden.busy) {
            hidden.input.insert_str(text);
            hidden.error = None;
            return ProjectReply::Handled;
        }
        if let Some(general) = self.general.as_mut().filter(|general| !general.busy) {
            if let Some(field) = general.text() {
                field.insert_str(text);
                general.error = None;
            }
            return ProjectReply::Handled;
        }
        if self.tab == Tab::Conversation && self.typing {
            self.input.insert_str(text);
            return ProjectReply::Handled;
        }
        if self.tab == Tab::Changes && self.changes.paste(text) {
            return ProjectReply::Handled;
        }
        if self.tab == Tab::Questions && self.questions.paste(text) {
            return ProjectReply::Handled;
        }
        ProjectReply::Ignored
    }

    fn tab_by(&mut self, offset: isize) -> ProjectReply {
        let count = Tab::ALL.len() as isize;
        let next = (self.tab.index() as isize + offset).rem_euclid(count);
        self.open(Tab::ALL[next as usize]);
        ProjectReply::Handled
    }

    fn input_key(&mut self, key: KeyEvent) -> ProjectReply {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let picks = self.picks();
        let shown = picks.len();
        match key.code {
            KeyCode::Esc if !self.input.value().is_empty() => {
                self.input.set("");
                self.menu_cursor = 0;
                self.console_note.clear();
                ProjectReply::Handled
            }
            KeyCode::Esc => {
                self.typing = false;
                self.console_note.clear();
                ProjectReply::Handled
            }
            KeyCode::Up if shown > 0 => {
                self.menu_cursor = self.menu_cursor.min(shown - 1).saturating_sub(1);
                ProjectReply::Handled
            }
            KeyCode::Down if shown > 0 => {
                self.menu_cursor = (self.menu_cursor + 1).min(shown - 1);
                ProjectReply::Handled
            }
            KeyCode::Up | KeyCode::Down => {
                let older = key.code == KeyCode::Up;
                if let Some(text) = self.history.step(older, self.input.value()) {
                    self.input.set(&text);
                }
                ProjectReply::Handled
            }
            KeyCode::Tab if shown > 0 => self.pick(picks),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert_str("\n");
                ProjectReply::Handled
            }
            KeyCode::Enter if matches!(picks, Picks::Tags { .. }) => self.pick(picks),
            KeyCode::Enter => {
                if let Picks::Commands(commands) = &picks
                    && let Some(command) =
                        commands.get(self.menu_cursor.min(shown.saturating_sub(1)))
                {
                    return self.run(Ok(command.action));
                }
                let text = self.input.value().trim().to_owned();
                if let Some(found) = console::command(&text) {
                    return self.run(found);
                }
                if text.is_empty() || !self.master_usable() {
                    return ProjectReply::Handled;
                }
                self.history.push(&text);
                self.input.set("");
                self.console_note.clear();
                self.transcript_scroll = 0;
                self.activity_open = false;
                ProjectReply::Send(text)
            }
            KeyCode::Char('o') if control && self.more_activity() => self.toggle_activity(),
            KeyCode::Char('x') if control => ProjectReply::StopTurn,
            KeyCode::Char('t') if control => ProjectReply::Team,
            KeyCode::PageUp => self.scroll_transcript(5),
            KeyCode::PageDown => self.scroll_transcript(-5),
            _ => {
                if self.input.handle(key) {
                    self.menu_cursor = 0;
                    ProjectReply::Handled
                } else if matches!(key.code, KeyCode::Char(_)) && !control {
                    ProjectReply::Handled
                } else {
                    ProjectReply::Ignored
                }
            }
        }
    }

    /// Gives the prompt's inline menu at the cursor while the owner types to the master.
    /// Takes the screen; tags come from the team graph and the project context as read.
    /// Returns the matching commands for a `/` word, the completing tags for an `@` word, or
    /// None.
    pub fn picks(&self) -> Picks {
        if self.tab != Tab::Conversation || !self.typing {
            return Picks::None;
        }
        if let Some(commands) = console::menu(self.input.value()) {
            return Picks::Commands(commands);
        }
        let head = self.input.head();
        let Some(query) = console::tag_query(head) else {
            return Picks::None;
        };
        let graph = self.team.ready().map(|graph| graph.as_ref());
        let files = self.files.as_ref().map(|(_, paths)| paths.as_slice());
        let candidates = console::tags(graph, files, self.context.as_ref());
        let tags: Vec<String> = console::completions(&candidates, query)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if tags.is_empty() {
            return Picks::None;
        }
        Picks::Tags {
            typed: query.chars().count() + 1,
            tags,
        }
    }

    /// The project file query for the `@` word the owner is typing to the master.
    /// Takes the screen. Returns the query, or None while no tag is typed.
    pub fn files_query(&self) -> Option<String> {
        if self.tab != Tab::Conversation || !self.typing {
            return None;
        }
        console::tag_query(self.input.head()).map(console::files_query)
    }

    fn pick(&mut self, picks: Picks) -> ProjectReply {
        let at = self.menu_cursor.min(picks.len().saturating_sub(1));
        match picks {
            Picks::None => {}
            Picks::Commands(commands) => self.input.set(&format!("/{}", commands[at].name)),
            Picks::Tags { typed, tags } => {
                self.input.replace_back(typed, &format!("{} ", tags[at]))
            }
        }
        self.menu_cursor = 0;
        ProjectReply::Handled
    }

    fn run(&mut self, found: Result<console::Action, String>) -> ProjectReply {
        let typed = self.input.value().trim().to_owned();
        self.history.push(&typed);
        self.input.set("");
        self.menu_cursor = 0;
        self.console_note.clear();
        let action = match found {
            Ok(action) => action,
            Err(word) => {
                self.console_note = vec![format!(
                    "Unknown command {word}. Type /help for the commands."
                )];
                return ProjectReply::Handled;
            }
        };
        match action {
            console::Action::Help => {
                self.console_note = console::COMMANDS
                    .iter()
                    .map(|command| format!("/{:<8} {}", command.name, command.about))
                    .collect();
            }
            console::Action::Clear => {
                self.cleared = self.transcript.as_ref().and_then(|transcript| {
                    let last = transcript.messages.last()?;
                    Some((transcript.messages.len(), last.clone()))
                });
                self.transcript_scroll = 0;
            }
            console::Action::Master if self.live_master() => return ProjectReply::Master,
            console::Action::Master => {
                self.console_note = vec![
                    "This master has no live terminal; its conversation is this console.".into(),
                ];
            }
            console::Action::Agents => self.open(Tab::Agents),
            console::Action::Changes => self.open(Tab::Changes),
            console::Action::Usage => return ProjectReply::Usage,
            console::Action::Process => return ProjectReply::Process,
            console::Action::Stop => return ProjectReply::StopTurn,
        }
        ProjectReply::Handled
    }

    /// Gives the transcript messages the console shows: those after a `/clear`, or all of
    /// them once the conversation no longer holds the message the clear ended on.
    /// Takes the transcript.
    /// Returns the messages to draw.
    pub fn shown_messages<'a>(&self, transcript: &'a Transcript) -> &'a [Message] {
        let skip = self
            .cleared
            .as_ref()
            .filter(|(count, last)| transcript.messages.get(count.wrapping_sub(1)) == Some(last))
            .map_or(0, |(count, _)| *count);
        &transcript.messages[skip..]
    }

    /// Says whether the master can take a message now.
    /// Takes the screen.
    /// Returns false without a pool or while the master is not ready.
    pub fn master_usable(&self) -> bool {
        self.pool.as_ref().is_some_and(|pool| pool.assigned)
            && self.transcript.as_ref().is_some_and(|t| {
                !matches!(
                    t.status,
                    MasterStatus::NeedsPool | MasterStatus::NotReady | MasterStatus::NotSignedIn
                )
            })
    }

    /// Says whether the running turn has more activity lines than the console lists at once.
    /// Takes the screen.
    /// Returns true while Ctrl-O has a longer list to open or close.
    pub fn more_activity(&self) -> bool {
        self.transcript
            .as_ref()
            .is_some_and(|transcript| transcript.status.activity_len() > ACTIVITY_SHOWN)
    }

    fn toggle_activity(&mut self) -> ProjectReply {
        assert!(self.more_activity());
        self.activity_open = !self.activity_open;
        ProjectReply::Handled
    }

    fn scroll_transcript(&mut self, by: isize) -> ProjectReply {
        self.transcript_scroll = self.transcript_scroll.saturating_add_signed(by);
        ProjectReply::Handled
    }

    /// Says whether the project's master runs as its own app in a live terminal, which
    /// `/master` opens from the console.
    /// Takes the screen.
    /// Returns true for a Codex or Claude Code master in the stored team plan.
    pub fn live_master(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| {
            crate::harnesses::interactive::support(&setup.team.master)
                == crate::harnesses::interactive::Support::Terminal
        })
    }

    fn conversation_key(&mut self, key: KeyEvent) -> ProjectReply {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let running = matches!(
            self.transcript.as_ref().map(|t| &t.status),
            Some(MasterStatus::Thinking { .. })
        );
        match self.sessions.key(key, self.master_usable(), running) {
            SessionsReply::Ignored => {}
            SessionsReply::Handled => return ProjectReply::Handled,
            SessionsReply::Act(action) => return ProjectReply::Session(action),
        }
        match key.code {
            KeyCode::Char('s') if !self.pool.as_ref().is_some_and(|p| p.assigned) => {
                self.open(Tab::Settings);
                ProjectReply::Handled
            }
            KeyCode::Char('r')
                if self
                    .transcript
                    .as_ref()
                    .is_some_and(|t| t.status.retryable()) =>
            {
                ProjectReply::Retry
            }
            KeyCode::Char('o') if control && self.more_activity() => self.toggle_activity(),
            KeyCode::Char('x') if control => ProjectReply::StopTurn,
            KeyCode::Char('t') if !control => ProjectReply::Team,
            KeyCode::Char('e') if !control && self.transcript.as_ref().is_some_and(|t| t.open) => {
                ProjectReply::EndConversation
            }
            KeyCode::PageUp => self.scroll_transcript(5),
            KeyCode::PageDown => self.scroll_transcript(-5),
            KeyCode::Enter => {
                self.typing = true;
                ProjectReply::Handled
            }
            _ => ProjectReply::Ignored,
        }
    }
}
