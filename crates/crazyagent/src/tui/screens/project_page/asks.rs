use super::*;

impl ProjectPage {
    /// Opens the project's Settings with its remove ask on Delete everything, as Home's Finish
    /// delete or Try again for a delete job does; the exact name is typed again (DEC-177
    /// add.1). Takes the project, its name and the context. Returns the page.
    pub fn deleting(project: ProjectId, name: String, ctx: &Ctx) -> Self {
        let mut page = Self::new(project, name, ProjectTab::Settings, ctx);
        page.screen.ask_delete_again();
        assert!(page.screen.confirm_remove);
        page
    }

    /// Opens a project on one tab.
    /// Takes the project id and name from Home, the tab and the shell context.
    /// Returns the screen; engine reads start when it is shown.
    pub fn new(project: ProjectId, name: String, tab: ProjectTab, ctx: &Ctx) -> Self {
        let mut screen = ProjectScreen::new(project, name, ctx.home.as_ref().map(PathBuf::from));
        screen.open(tab_of(tab));
        let now = now_ms();
        screen.now_ms = now;
        screen.offset = local_offset(now);
        let mut page = Self {
            screen,
            view: View::default(),
            sent: Vec::new(),
            assigning: None,
            recording: None,
            access_wanted: None,
            live_wanted: false,
            created_access: None,
            master: MasterState::default(),
            master_sent: Vec::new(),
            waiting: None,
            starting: None,
            retried: false,
            renewed: false,
            schedule_asked: false,
            team_asked: None,
            files_asked: None,
            covered: false,
            from_master: false,
            reopen_master: false,
            master_entry: tab == ProjectTab::Conversation,
            import: None,
            import_asked: false,
            import_sent: None,
            moved: None,
            uncommitted: Vec::new(),
            uncommitted_branch: None,
            uncommitted_asked: false,
            job_sent: None,
            publish_sent: None,
            publish_pane: None,
            publish_asked: None,
            publish_later: false,
            publish_read_ms: None,
            publish_lost_ms: None,
            ask_pane: None,
            publish_offered: false,
            moving: None,
        };
        page.refresh_pool(ctx);
        page.select_master(ctx);
        page.screen.transcript = page.transcript();
        page.redraw(ctx);
        page
    }

    /// Saves the agent access level the owner picked in New project, right after the project
    /// was made: it reads the project's policy first and saves at the version read, since a
    /// project made again under an earlier id keeps that id's policy version. Until the save
    /// lands the project stays Sandboxed, and a refusal says so.
    /// Takes the picked level and the context. Returns nothing; the replies arrive later.
    pub(in crate::tui) fn save_created_access(&mut self, access: AgentAccess, ctx: &mut Ctx) {
        assert_ne!(access, AgentAccess::Sandboxed);
        self.access_wanted = Some(access);
        self.send(
            SetupQuery::AgentPolicy {
                project: self.screen.project.clone(),
            },
            ctx,
        );
    }

    /// Starts publishing the project the owner just made on GitHub with the plan from New
    /// project (DEC-182). The engine answers once its checks pass and drafts the .gitignore
    /// lines after, and the reply says the publish started or why not; a refused Start reads
    /// the publish again, so the Overview's question comes back.
    /// Takes the plan and the context. Returns nothing; the reply arrives later.
    pub(in crate::tui) fn start_publish(&mut self, plan: GitHubPublishPlan, ctx: &mut Ctx) {
        assert!(self.publish_sent.is_none());
        let query = IntakeQuery::GitHubPublishStart {
            project: self.screen.project.clone(),
            plan,
        };
        self.publish_sent = Some(query.clone());
        super::super::intake::ask(ctx, query);
    }

    /// Reads where the project's GitHub publish stands while the Overview shows and no
    /// publish request waits, so the pane opens on a publish the owner can act on or follow.
    /// Takes the context. Returns nothing; the reply arrives later.
    pub(super) fn ask_publish(&mut self, ctx: &mut Ctx) {
        if self.screen.tab != Tab::Overview
            || self.publish_asked.is_some()
            || self.publish_sent.is_some()
        {
            return;
        }
        let status = IntakeQuery::GitHubPublishStatus {
            project: self.screen.project.clone(),
        };
        self.publish_asked = Some(status.clone());
        self.publish_read_ms = Some(now_ms());
        super::super::intake::ask(ctx, status);
    }

    /// Asks the engine whether to offer publishing on GitHub, after a read that found no
    /// publish, while the Overview shows and no publish request waits (DEC-182 S5b).
    /// Takes the context. Returns nothing; the reply arrives later.
    fn ask_prompt(&mut self, ctx: &mut Ctx) {
        if self.screen.tab != Tab::Overview
            || self.publish_asked.is_some()
            || self.publish_sent.is_some()
        {
            return;
        }
        let prompt = IntakeQuery::GitHubPublishPrompt {
            project: self.screen.project.clone(),
        };
        self.publish_asked = Some(prompt.clone());
        super::super::intake::ask(ctx, prompt);
    }

    /// Reads the publish again, at most every `DRAFT_POLL_MS`, while its pane shows the draft
    /// running or while a Start whose reply was lost may still land (GSB-1). That wait ends
    /// when a publish shows, or after as long as the engine gives a Start, when it says no
    /// publish showed and asks the question again. Takes the context. Returns nothing.
    pub(super) fn poll_publish(&mut self, ctx: &mut Ctx) {
        if self
            .publish_lost_ms
            .is_some_and(|since| lost_start_over(since, now_ms()))
        {
            self.publish_lost_ms = None;
            ctx.say(Message::warning(PUBLISH_NOT_SEEN, PUBLISH_AGAIN));
            self.ask_prompt(ctx);
        }
        let drafting = self
            .publish_pane
            .as_ref()
            .is_some_and(|pane| pane.job().step == GitHubPublishStep::Drafting);
        let due = self
            .publish_read_ms
            .is_none_or(|at| now_ms() - at >= DRAFT_POLL_MS);
        if (drafting || self.publish_lost_ms.is_some()) && due && !self.publish_later {
            self.ask_publish(ctx);
        }
    }

    /// Takes the engine's answer to this page's publish read or one of the pane's answers:
    /// a read opens the pane on a publish the owner can act on or follow and keeps an open
    /// pane on where the publish stands, finished or not, an answer shows where the publish
    /// now stands, a discard says what it left on GitHub, and a refused answer says why and
    /// reads the publish again with the pane kept. A read that finds a publish closes the
    /// Overview's question or form and says why the form's last Start was refused. With no
    /// publish it asks whether to offer one, shows the question when the engine says to and
    /// drops it once Not now or Don't ask again is kept. A failed read stays quiet. Takes the
    /// reply and the context. Returns nothing.
    pub(super) fn publish_answered(&mut self, event: &IntakeEvent, ctx: &mut Ctx) {
        let read = matches!(
            event.query,
            IntakeQuery::GitHubPublishStatus { .. } | IntakeQuery::GitHubPublishPrompt { .. }
        );
        self.publish_asked = None;
        match &event.result {
            Ok(IntakeView::PublishStatus(job)) => {
                let shown = self.publish_shows();
                self.publish_pane = job
                    .as_deref()
                    .filter(|job| shown || opens(job))
                    .cloned()
                    .map(PublishPane::new);
                if job.is_some() {
                    self.publish_lost_ms = None;
                    self.screen.github_notice = false;
                    if let Some(said) = self.ask_pane.take().and_then(AskPane::into_refusal) {
                        ctx.say(said);
                    }
                } else if self.publish_lost_ms.is_none() {
                    self.ask_prompt(ctx);
                }
            }
            Ok(IntakeView::Prompt(ask)) => {
                self.screen.github_notice = *ask;
                if !ask && self.ask_pane.as_ref().is_some_and(AskPane::asking) {
                    self.ask_pane = None;
                }
            }
            Ok(IntakeView::PromptKept) => {
                self.screen.github_notice = false;
                self.ask_pane = None;
            }
            Ok(IntakeView::Publish(job)) => {
                self.publish_pane = Some(PublishPane::new((**job).clone()));
            }
            Ok(IntakeView::Discarded(left)) => {
                self.publish_pane = None;
                ctx.say(Message::success(
                    "The publish was discarded.",
                    &left_line(left.as_ref()),
                ));
                self.ask_prompt(ctx);
            }
            Err(_) if read => {}
            Err(code) => {
                ctx.say(Message::warning(
                    ANSWER_NOT_TAKEN,
                    &super::super::intake::copy(code),
                ));
                self.ask_publish(ctx);
            }
            Ok(_) => {
                ctx.say(Message::warning(
                    ANSWER_NOT_TAKEN,
                    &super::super::intake::copy("github_outcome_unknown"),
                ));
                self.ask_publish(ctx);
            }
        }
    }

    /// Handles a key while the publish pane shows: one answer goes to the engine at a time,
    /// and Esc closes the pane until the page shows again, or for good once it is finished.
    /// Takes the key and the context. Returns handled, so no key reaches the page meanwhile.
    pub(super) fn publish_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(pane) = self.publish_pane.as_mut() else {
            return Flow::Handled;
        };
        if self.publish_asked.is_some() {
            self.publish_later |= *key == Key::Esc;
            return Flow::Handled;
        }
        match pane.key(key) {
            PaneKey::Handled => {}
            PaneKey::Later => {
                self.publish_later = true;
                if !opens(pane.job()) {
                    self.publish_pane = None;
                }
            }
            PaneKey::Ask(ask) => {
                let sent = query(&self.screen.project, ask);
                self.publish_asked = Some(sent.clone());
                super::super::intake::ask(ctx, sent);
            }
        }
        Flow::Handled
    }

    /// Handles a key while the Overview's publish question or its form is open: one answer,
    /// read or Start goes to the engine at a time, Esc closes the pane meanwhile, Publish
    /// starts the publish with the form's plan and keeps the form until the engine answers,
    /// and the sign-in closes the pane, so G reads the sign-in again after it. Takes the key
    /// and the context. Returns the GitHub sign-in to open, or handled, so no key reaches
    /// the page meanwhile.
    pub(super) fn ask_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(pane) = self.ask_pane.as_mut() else {
            return Flow::Handled;
        };
        if self.publish_asked.is_some() || self.publish_sent.is_some() {
            if *key == Key::Esc {
                self.ask_pane = None;
            }
            return Flow::Handled;
        }
        let about = self
            .screen
            .setup
            .as_ref()
            .map_or("", |setup| setup.about.as_str());
        match pane.key(key, &self.screen.name, about) {
            AskKey::Handled => {}
            AskKey::Close => self.ask_pane = None,
            AskKey::Answer(answer) => {
                let sent = IntakeQuery::GitHubPublishPromptSet {
                    project: self.screen.project.clone(),
                    answer,
                };
                self.publish_asked = Some(sent.clone());
                super::super::intake::ask(ctx, sent);
            }
            AskKey::Read(read) => super::super::intake::ask(ctx, read),
            AskKey::SignIn => {
                self.ask_pane = None;
                self.covered = true;
                return Flow::Open(Place::CloneRepo);
            }
            AskKey::Start(plan) => {
                self.screen.github_notice = false;
                self.publish_offered = true;
                self.start_publish(plan, ctx);
                self.redraw(ctx);
            }
        }
        Flow::Handled
    }

    pub(super) fn publish_shows(&self) -> bool {
        self.publish_pane.is_some() && !self.publish_later
    }

    /// The Overview's publish question or its form as a modal, with the keys hidden and what
    /// the page waits on said while an answer, a read or the Start is on its way. Takes the
    /// context. Returns the modal and its keys, or none while the pane is closed.
    fn ask_modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let pane = self.ask_pane.as_ref()?;
        let about = self
            .screen
            .setup
            .as_ref()
            .map_or("", |setup| setup.about.as_str());
        let text = pane.text(&self.screen.name, about, &look(ctx));
        let mut body: Vec<Line<'static>> = text.body.into_iter().map(Row::line).collect();
        let waiting = match (&self.publish_sent, &self.publish_asked) {
            (Some(_), _) => Some(ASK_STARTING),
            (None, Some(IntakeQuery::GitHubPublishPromptSet { .. })) => Some(ASK_SENDING),
            (None, Some(_)) => Some(ASK_READING),
            (None, None) => None,
        };
        let hints = if let Some(words) = waiting {
            body.push(styled(words, Role::Muted, &ctx.theme));
            Vec::new()
        } else {
            text.hints
                .iter()
                .map(|(key, label)| hint(key, label))
                .collect()
        };
        Some((
            Modal {
                width: IMPORT_MODAL_WIDTH,
                title: text.title.into(),
                body,
                top: None,
            },
            hints,
        ))
    }

    /// The publish pane as a modal: where the publish stands and the keys for it, with the
    /// keys hidden while an answer is on its way. Takes the context. Returns the modal and
    /// its keys, or none while the pane is closed.
    pub(super) fn publish_modal(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        if !self.publish_shows() {
            return self.ask_modal(ctx);
        }
        let text = self.publish_pane.as_ref()?.text();
        let room = usize::from(IMPORT_MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = text
            .body
            .iter()
            .flat_map(|line| wrap(line, room))
            .map(Line::raw)
            .collect();
        let hints = if self.publish_asked.is_some() {
            body.push(styled("Sending your answer...", Role::Muted, &ctx.theme));
            Vec::new()
        } else {
            text.hints
                .iter()
                .map(|(key, label)| hint(key, label))
                .collect()
        };
        Some((
            Modal {
                width: IMPORT_MODAL_WIDTH,
                title: text.title.into(),
                body,
                top: None,
            },
            hints,
        ))
    }

    /// The write that saves the agent access picked in New project at the version the
    /// project's policy read found, for an open project page or a New Project carried in the
    /// background (DEC-177 add.3 decision 4). Inputs: the project, the picked level, the
    /// read's reply and the context, which numbers the write. Output: the write and its
    /// generation, or why the project stays Sandboxed.
    pub(in crate::tui) fn created_access_write(
        project: &ProjectId,
        access: AgentAccess,
        read: &Result<SetupView, String>,
        ctx: &mut Ctx,
    ) -> Result<(SetupQuery, u64), String> {
        assert_ne!(access, AgentAccess::Sandboxed);
        let version = match read {
            Ok(SetupView::AgentPolicy(view)) => view.version,
            Err(code) => return Err(plain_problem(code)),
            Ok(_) => return Err(plain_problem("change_outcome_unknown")),
        };
        let generation = ctx.next_generation();
        let write = SetupQuery::Change {
            action: SetupAction::SetAgentPolicy {
                project_id: project.clone(),
                version,
                policy: AgentPolicy {
                    access,
                    toolchains: Vec::new(),
                    ide_git_hooks: false,
                    tool_cache: Default::default(),
                    home_caches: Default::default(),
                    ide_agent_files: Default::default(),
                    shared_folders: Default::default(),
                    network: crate::sandbox::policy::AgentNetwork::default(),
                    apply_changes: Default::default(),
                    grants: Vec::new(),
                },
            },
            generation,
        };
        Ok((write, generation))
    }

    /// Why the write that saves New project's agent access did not land. Input: the write's
    /// reply. Output: none when it was applied, else the reason in plain words.
    pub(in crate::tui) fn created_access_unsaved(
        reply: &Result<SetupView, String>,
    ) -> Option<String> {
        match reply {
            Ok(SetupView::Changed(OwnerResult::Applied)) => None,
            Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                Some(plain_problem(code))
            }
            Ok(_) => Some(plain_problem("change_outcome_unknown")),
        }
    }

    /// The line a GitHub publish Start reply gives: the project is ready and publishing
    /// started, it may have started when the reply was lost after the engine ran it, or why
    /// it did not start. Takes the project's name and the reply. Returns the message.
    pub(in crate::tui) fn publish_start_said(
        name: &str,
        result: &Result<IntakeView, String>,
    ) -> Message {
        match result {
            Ok(IntakeView::Publish(_)) => {
                Message::success(&format!("{name} is ready."), PUBLISH_STARTED)
            }
            Err(code) if !start_reply_lost(result) => {
                Message::warning(PUBLISH_NOT_STARTED, &super::super::intake::copy(code))
            }
            _ => Message::warning(PUBLISH_MAY_HAVE_STARTED, PUBLISH_WHERE_IT_STANDS),
        }
    }

    /// Says a made project's picked agent access was not saved, so its agents stay
    /// Sandboxed. Inputs: the reason and the context. Output: none.
    pub(in crate::tui) fn stays_sandboxed(why: &str, ctx: &mut Ctx) {
        ctx.say(Message::warning(
            "The project is made, but its agents stay Sandboxed.",
            &format!("{why} Choose Agent access again on the Settings tab with c."),
        ));
    }

    /// Asks the engine, once per opening, which instruction files wait to be copied into the
    /// project's instructions.md (DEC-106). Input: the context. Output: none; the list arrives
    /// as a process event and shows as a prompt when it isn't empty.
    pub(super) fn ask_import(&mut self, ctx: &mut Ctx) {
        if self.import_asked || self.import_sent.is_some() {
            return;
        }
        let query = ProcessQuery::PendingImport {
            project: self.screen.project.clone(),
        };
        if ctx.try_request(Request::Process(Box::new(query.clone()))) {
            self.import_asked = true;
            self.import_sent = Some(query);
        }
    }

    /// Reads which moved instruction files still wait for a commit, for the banner shown
    /// while the move stays uncommitted (DEC-106 addendum). Input: the context. Output:
    /// none; the paths arrive as a process event.
    pub(super) fn ask_uncommitted(&mut self, ctx: &mut Ctx) {
        if self.uncommitted_asked {
            return;
        }
        let query = ProcessQuery::Uncommitted {
            project: self.screen.project.clone(),
        };
        self.uncommitted_asked = ctx.try_request(Request::Process(Box::new(query)));
    }

    /// Asks the engine to commit the instruction import's move: one local commit on the
    /// checkout's current branch holding only the moved files, never pushed. Only the owner's
    /// key calls this. Input: the context. Output: none; the commit arrives as a process event.
    pub(super) fn commit_move(&mut self, ctx: &mut Ctx) {
        if self.import_sent.is_some() {
            return;
        }
        let query = ProcessQuery::CommitImport {
            project: self.screen.project.clone(),
        };
        if ctx.try_request(Request::Process(Box::new(query.clone()))) {
            self.import_sent = Some(query);
        } else {
            ctx.say(Message::error(
                "The move wasn't committed: too many requests are waiting.",
                "",
            ));
        }
    }

    /// Applies the engine's answer to the pending-import read, the confirmed move, the
    /// uncommitted-move read or the commit. Inputs: the process event and the context.
    /// Output: none; a list shows the prompt, a move with uncommitted files offers Commit the
    /// move or Leave it, any other move closes it and names the moved files, a commit names
    /// its branch and files, and a refusal says why.
    pub(super) fn import_arrived(&mut self, event: &ProcessEvent, ctx: &mut Ctx) {
        if let (ProcessQuery::Uncommitted { project }, result) = (&event.query, &event.result)
            && *project == self.screen.project
        {
            self.uncommitted_asked = false;
            if let Ok(ProcessReply::Uncommitted(waiting)) = result {
                self.uncommitted = waiting.files.clone();
                self.uncommitted_branch = waiting.branch.clone();
            }
            return;
        }
        if self.import_sent.as_ref() != Some(&event.query) {
            return;
        }
        self.import_sent = None;
        match (&event.query, &event.result) {
            (ProcessQuery::PendingImport { .. }, Ok(ProcessReply::Files(files))) => {
                self.import = (!files.is_empty()).then(|| files.clone());
            }
            (ProcessQuery::PendingImport { .. }, _) => self.import_asked = false,
            (ProcessQuery::ConfirmImport { .. }, Ok(ProcessReply::Files(moved))) => {
                self.import = None;
                if moved.iter().any(|file| file.uncommitted) {
                    self.moved = Some(moved.clone());
                } else {
                    let names: Vec<String> =
                        moved.iter().map(|file| sanitize(&file.path)).collect();
                    ctx.say(Message::success(
                        "Moved to .crazyagent/imported/:",
                        &names.join(", "),
                    ));
                }
                self.ask_uncommitted(ctx);
            }
            (ProcessQuery::ConfirmImport { .. }, Err(code)) => {
                ctx.say(Message::error(&import_problem(code), ""));
            }
            (ProcessQuery::CommitImport { .. }, Ok(ProcessReply::Committed(commit))) => {
                self.moved = None;
                let files: Vec<String> = commit.files.iter().map(|path| sanitize(path)).collect();
                ctx.say(Message::success(
                    &format!("Committed the move on {}:", sanitize(&commit.branch)),
                    &format!("{}. Nothing was pushed.", files.join(", ")),
                ));
                self.ask_uncommitted(ctx);
            }
            (ProcessQuery::CommitImport { .. }, Err(code)) => {
                if code == "process_commit_none" {
                    self.moved = None;
                }
                ctx.say(Message::error(&super::super::process::message(code), ""));
                self.ask_uncommitted(ctx);
            }
            _ => {}
        }
    }

    /// Handles a key while the moved files still wait for a commit after the confirm: Enter
    /// or y commits the move, Esc or n leaves it for the project page banner. Inputs: the key
    /// and the context. Output: handled, so no key reaches the page meanwhile.
    pub(super) fn moved_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        match key {
            Key::Enter | Key::Char('y') => self.commit_move(ctx),
            Key::Esc | Key::Char('n') if self.import_sent.is_none() => {
                self.moved = None;
                self.ask_uncommitted(ctx);
            }
            _ => {}
        }
        Flow::Handled
    }

    /// The banner rows shown above the tabs while an instruction import's move stays
    /// uncommitted: the files, the branch merges into are refused and the Ctrl-G action.
    /// Inputs: the width and the context. Output: the rows, none when no move waits.
    pub(super) fn banner(&self, width: usize, ctx: &Ctx) -> Vec<Line<'static>> {
        if self.uncommitted.is_empty() || width < 8 {
            return Vec::new();
        }
        let text = move_banner(
            &self.uncommitted,
            self.uncommitted_branch.as_deref(),
            ctx.glyphs.ellipsis,
        );
        wrap(&text, width - 2)
            .into_iter()
            .take(BANNER_ROWS_MAX)
            .map(|line| styled(format!(" {line}"), Role::Warning, &ctx.theme))
            .collect()
    }

    /// Handles a key while the pending-import prompt shows: Enter or y copies the files into
    /// instructions.md and moves them aside, Esc or n closes it until the next opening.
    /// Inputs: the key and the context. Output: handled, so no key reaches the page meanwhile.
    pub(super) fn import_key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let movable = self
            .import
            .as_ref()
            .is_some_and(|files| files.iter().any(|file| file.refused.is_none()));
        match key {
            Key::Enter | Key::Char('y') if movable && self.import_sent.is_none() => {
                let query = ProcessQuery::ConfirmImport {
                    project: self.screen.project.clone(),
                };
                if ctx.try_request(Request::Process(Box::new(query.clone()))) {
                    self.import_sent = Some(query);
                } else {
                    ctx.say(Message::error(
                        "The files weren't moved: too many requests are waiting.",
                        "",
                    ));
                }
            }
            Key::Esc | Key::Char('n') if self.import_sent.is_none() => self.import = None,
            _ => {}
        }
        Flow::Handled
    }

    /// The step after a confirmed import whose moved files git still shows as deleted: the
    /// moved files, which of them wait for a commit, and Commit the move or Leave it.
    /// Inputs: the moved files and the context. Output: the modal and its keys.
    pub(super) fn moved_modal(&self, moved: &[ImportFile], ctx: &Ctx) -> (Modal, Vec<Hint>) {
        let theme = &ctx.theme;
        let room = usize::from(IMPORT_MODAL_WIDTH) - 4;
        let mut body: Vec<Line<'static>> = wrap(
            "Their text is in this project's .crazyagent/instructions.md. Moved:",
            room,
        )
        .into_iter()
        .map(Line::raw)
        .collect();
        for file in moved.iter().take(IMPORT_ROWS_MAX) {
            let path = sanitize(&file.path);
            let text = if file.uncommitted {
                format!("- {path} (git shows it as deleted)")
            } else {
                format!("- {path}")
            };
            body.extend(wrap(&text, room).into_iter().map(Line::raw));
        }
        body.push(Line::default());
        body.extend(
            wrap(
                "Merges into this branch are refused until the move is committed. Commit the move makes one local commit on your current branch holding only that move; it is never pushed. Leave it keeps a banner on the project page.",
                room,
            )
            .into_iter()
            .map(|text| styled(text, Role::Warning, theme)),
        );
        let hints = if self.import_sent.is_none() {
            vec![hint("Enter", "Commit the move"), hint("Esc", "Leave it")]
        } else {
            Vec::new()
        };
        (
            Modal {
                width: IMPORT_MODAL_WIDTH,
                title: "Moved to .crazyagent/imported/".into(),
                body,
                top: None,
            },
            hints,
        )
    }

    /// Reads the team graph while this page is on top: on the Conversation tab for the panel
    /// beside the master, on the Settings tab while folders are hidden to name the agents
    /// that can still open one (SA-VID14-A), and once on any tab while no read stands, so the
    /// next entry knows Live terminal. At most one read is out, at most one every
    /// `TEAM_READ_MS`, and a read lost for `TEAM_WAIT_MS` is asked again. Takes the context.
    pub(super) fn ask_team(&mut self, ctx: &mut Ctx) {
        let hides = self.screen.tab == Tab::Settings
            && self.screen.hidden_offered()
            && self
                .screen
                .hidden_paths
                .as_ref()
                .is_some_and(|saved| !saved.paths.is_empty());
        let wanted =
            self.screen.tab == Tab::Conversation || hides || matches!(self.screen.team, Load::Idle);
        if self.covered || !wanted || ctx.link != Link::Ready {
            return;
        }
        let now = now_ms();
        if let Some((at, waiting)) = self.team_asked
            && now - at < if waiting { TEAM_WAIT_MS } else { TEAM_READ_MS }
        {
            return;
        }
        self.team_asked = Some((now, true));
        ctx.request(Request::TeamGraph {
            project: self.screen.project.clone(),
        });
    }

    /// Lists the project files for the `@` word the owner is typing: one read out at a time,
    /// and only when the word differs from the last listed one. A read the full request
    /// queue drops is not counted as out, so the next key asks again. Takes the context.
    pub(super) fn ask_files(&mut self, ctx: &mut Ctx) {
        if self.covered || self.files_asked.is_some() || ctx.link != Link::Ready {
            return;
        }
        let Some(query) = self.screen.files_query() else {
            return;
        };
        if self
            .screen
            .files
            .as_ref()
            .is_some_and(|(listed, _)| *listed == query)
        {
            return;
        }
        let queued = ctx.queued();
        ctx.request(Request::ProjectFiles {
            project: self.screen.project.clone(),
            query: query.clone(),
        });
        if ctx.queued() > queued {
            self.files_asked = Some(query);
        }
    }
}
