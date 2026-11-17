use super::*;

impl ProjectPage {
    pub(super) fn select_master(&mut self, ctx: &Ctx) {
        let ready = ctx.link == Link::Ready && ctx.identity.is_some();
        self.master
            .select(
                ctx.identity.clone(),
                Some(self.screen.project.clone()),
                ready,
            )
            .expect("the master view generation never runs out");
    }

    pub(super) fn sync_master(&mut self, ctx: &mut Ctx) {
        self.select_master(ctx);
        if let Some(reason) = self.master.take_renewal_problem() {
            ctx.say(Message::warning(RENEWAL_NOT_STARTED, &reason));
        }
        self.master
            .show_sessions(self.screen.tab == Tab::Conversation);
        if self.master_sent.len() < MASTER_SENT_MAX
            && let Some(job) = self.master.request(self.screen.tab == Tab::Conversation)
        {
            self.master_sent.push(MasterSent::of_job(&job));
            ctx.request(Request::Master(Box::new(job)));
        }
        self.screen.transcript = self.transcript();
        if let Some(listed) = self.master.sessions() {
            self.screen
                .sessions
                .set(super::super::sessions::rows_of(listed));
        }
        let reading = self
            .screen
            .sessions
            .reading()
            .and_then(|session| crate::protocol::SessionId::new(session).ok());
        self.master.open_messages(reading);
        self.screen
            .sessions
            .set_messages(self.master.opened_messages());
    }

    pub(super) fn transcript(&self) -> Option<Transcript> {
        let (session, turns, _) = self.master.transcript()?;
        let read = MasterRead {
            session: session.map(|session| session.phase),
            turns,
            inputs: self.master.inputs(),
            messages: self.master.messages(),
        };
        let harness = match session {
            Some(session) => backend_name(session.selection.backend),
            None => self.screen.setup.as_ref().map_or_else(
                || "Master".to_owned(),
                |setup| harness_name(&setup.team.master, None),
            ),
        };
        let tools = self
            .screen
            .setup
            .as_ref()
            .is_some_and(|setup| setup.team.master_tools);
        let mut transcript = transcript_from(&read, &harness, None, tools, self.screen.offset);
        if session.is_none() {
            transcript.status = MasterStatus::Ready;
        }
        if let Some(waiting) = &self.waiting {
            transcript.messages.push(super::super::project::Message {
                speaker: Speaker::You,
                time: None,
                text: waiting.text.clone(),
                error: false,
            });
            transcript.status = MasterStatus::Thinking {
                since_ms: None,
                activity: Vec::new(),
            };
        }
        Some(transcript)
    }

    fn drop_waiting(&mut self, ctx: &mut Ctx, problem: &str) {
        let Some(waiting) = self.waiting.take() else {
            return;
        };
        if self.screen.input.value().is_empty() {
            self.screen.input.set(&waiting.text);
        }
        ctx.say(Message::error(
            problem,
            "What you typed is back in the box.",
        ));
    }

    fn send_master(&mut self, text: String, ctx: &mut Ctx) {
        if self.waiting.is_some() || self.master.has_pending() {
            self.screen.input.set(&text);
            ctx.say(Message::warning(
                "The master is still taking your last message.",
                "Wait a moment, then send again.",
            ));
            return;
        }
        self.renewed = false;
        let session = self.master.transcript().and_then(|(session, turns, _)| {
            session.map(|session| {
                (
                    session.phase,
                    crate::tui::master_session::turn_holds(session.current_turn.as_ref(), turns),
                )
            })
        });
        match session {
            Some((crate::master_session::MasterSessionPhase::Configured, true)) => {
                self.screen.input.set(&text);
                ctx.say(Message::warning(
                    "The master is still working.",
                    "Press Ctrl-X to stop it, or wait for its reply.",
                ));
            }
            Some((crate::master_session::MasterSessionPhase::Configured, false)) => {
                self.retried = false;
                if self.master.submit(&text, ctx.next_generation()) != Ok(true) {
                    self.screen.input.set(&text);
                    ctx.say(Message::warning(
                        "The master is not ready for a message yet.",
                        "Wait a moment, then send again.",
                    ));
                }
            }
            _ => self.open_master(text, ctx),
        }
    }

    fn open_master(&mut self, text: String, ctx: &mut Ctx) {
        self.waiting = Some(Waiting {
            text,
            opening: false,
        });
        self.retried = false;
        if self.master.open().is_err() {
            self.drop_waiting(ctx, "The master could not start.");
        }
    }

    pub(super) fn master_arrived(&mut self, event: &MasterEvent, ctx: &mut Ctx) {
        let sent = MasterSent::of_event(event);
        let Some(index) = self.master_sent.iter().position(|each| *each == sent) else {
            return;
        };
        self.master_sent.remove(index);
        let rejected = rejection(event);
        let barrier = not_recorded(event);
        let failed = matches!(event, MasterEvent::Mutation { result: Err(_), .. });
        let applied = self.master.apply(event.clone());
        let renewing = self.renew(rejected.as_deref(), submitted_text(event));
        if let Some(code) = rejected.as_deref().filter(|_| !renewing) {
            self.refused(event, code, ctx);
        }
        if (barrier || failed) && !self.retried {
            self.retried = true;
            if barrier {
                self.master.retry();
            } else {
                self.master.access();
            }
        } else if failed {
            ctx.say(Message::warning(
                "The engine did not confirm the master's last step.",
                "It is checked again when the connection is back.",
            ));
        }
        match applied {
            Err(_) => self.drop_waiting(ctx, "The master conversation stopped updating."),
            Ok(Some(confirmed)) => self.confirmed(&confirmed.action, ctx),
            Ok(None) if renewing => {}
            Ok(None) => self.advance_open(event, rejected.is_some() || failed, ctx),
        }
    }

    /// Ends a conversation the engine refused a message for because it opened before a
    /// settings change, so the message is sent once more in a new conversation.
    /// Takes the rejection code and the refused text.
    /// Returns true when the stale conversation is being ended.
    fn renew(&mut self, code: Option<&str>, text: Option<&str>) -> bool {
        let (Some("master_source_conflict"), Some(text)) = (code, text) else {
            return false;
        };
        if self.renewed || self.waiting.is_some() || self.master.control(true).is_err() {
            return false;
        }
        self.renewed = true;
        self.waiting = Some(Waiting {
            text: text.to_owned(),
            opening: false,
        });
        true
    }

    /// Tells the owner the engine refused a master request, in plain words with its code.
    /// Takes the event and the rejection code; a refused message goes back in the input box.
    fn refused(&mut self, event: &MasterEvent, code: &str, ctx: &mut Ctx) {
        let MasterEvent::Mutation { submission, .. } = event else {
            return;
        };
        if code == "master_turn_running"
            && self.screen.sessions.turn_was_running(&submission.action)
        {
            return;
        }
        if code == "master_session_unused" {
            ctx.say(Message::warning(
                "This session has no messages yet, so it already is a new session.",
                "",
            ));
            return;
        }
        let lead = match &submission.action {
            MasterOwnerAction::Submit { text, .. } => {
                if self.screen.input.value().is_empty() {
                    self.screen.input.set(text);
                }
                "The master did not take that message."
            }
            MasterOwnerAction::Open { .. }
            | MasterOwnerAction::NewSession { .. }
            | MasterOwnerAction::RegisterRcliProfile { .. } => "The master could not start.",
            MasterOwnerAction::Resume { .. } => "The session did not resume.",
            MasterOwnerAction::Cancel { .. } => "The master's turn did not stop.",
            MasterOwnerAction::Close { .. } => "The conversation did not end.",
            MasterOwnerAction::Resolve { .. } => "The master did not start resolving the update.",
            MasterOwnerAction::DeleteSession { .. } => "The session was not deleted.",
        };
        let problem = master_problem(code);
        if self.waiting.is_some() {
            self.drop_waiting(ctx, &format!("{lead} {problem}"));
        } else {
            ctx.say(Message::error(lead, &problem));
        }
    }

    /// Carries on after the engine confirmed a master request: an opened master takes the
    /// waiting message, a conversation ended for a renewal opens a new one, a session
    /// switch the owner started from the live master view opens that view again, and a new
    /// session of a master that runs live turns the live switch on, so the live master
    /// starts on the fresh session. A new session also clears the error that said it was
    /// already starting, while that error still shows. Takes the confirmed action and the
    /// context.
    fn confirmed(&mut self, action: &MasterOwnerAction, ctx: &mut Ctx) {
        match action {
            MasterOwnerAction::NewSession { .. } | MasterOwnerAction::Resume { .. } => {
                if self.from_master {
                    self.from_master = false;
                    self.reopen_master = true;
                }
                if matches!(action, MasterOwnerAction::NewSession { .. })
                    && let Some(said) = self.starting.take()
                {
                    ctx.unsay(&said);
                }
                if matches!(action, MasterOwnerAction::NewSession { .. })
                    && self.screen.live_master()
                {
                    self.turn_live_on(ctx);
                }
            }
            MasterOwnerAction::Open { .. } => {
                let Some(waiting) = self.waiting.take() else {
                    return;
                };
                self.retried = false;
                if self.master.submit(&waiting.text, ctx.next_generation()) != Ok(true) {
                    self.waiting = Some(waiting);
                    self.drop_waiting(ctx, "The master started but did not take the message.");
                } else if self.renewed {
                    ctx.say(Message::warning(
                        "Settings changed, so a new conversation started.",
                        "",
                    ));
                }
            }
            MasterOwnerAction::Close { .. }
                if self.waiting.is_some() && self.master.open().is_err() =>
            {
                self.drop_waiting(ctx, "The master could not start.");
            }
            _ => {}
        }
    }

    /// Turns the project's live master switch on, for a new session the owner asked for: it
    /// reads the team status first and switches at the team settings version read, since
    /// the switch is a versioned team setting (DEC-098). Takes the context; the replies
    /// arrive later.
    fn turn_live_on(&mut self, ctx: &mut Ctx) {
        self.live_wanted = true;
        self.send(
            SetupQuery::Team {
                project: self.screen.project.clone(),
            },
            ctx,
        );
    }

    fn send_live_on(&self, version: u64, ctx: &mut Ctx) {
        let generation = ctx.next_generation();
        ctx.request(Request::Setup(SetupQuery::TeamChange {
            action: crate::team::TeamAction::LiveMaster {
                project_id: self.screen.project.clone(),
                on: true,
                version,
            },
            generation,
        }));
    }

    fn advance_open(&mut self, event: &MasterEvent, stopped: bool, ctx: &mut Ctx) {
        let Some(waiting) = self.waiting.as_mut() else {
            return;
        };
        if stopped {
            self.drop_waiting(ctx, "The master could not start.");
            return;
        }
        if waiting.opening || !matches!(event, MasterEvent::Catalog { .. }) {
            return;
        }
        waiting.opening = true;
        if self.master.reduce(&UiEvent::Submit) != Ok(true) || !self.master.has_pending() {
            let _ = self.master.reduce(&UiEvent::Cancel);
            self.drop_waiting(
                ctx,
                "The master could not start. Check that the pool has a signed-in account.",
            );
        }
    }

    /// Starts the engine's job that moves this project into the projects folder or deletes it
    /// with the exact name typed (DEC-177 add.1); Home follows it and the reply says it started
    /// or why not. Takes the subject, the way and the context. Returns nothing; the reply
    /// arrives later.
    fn start_job(&mut self, subject: JobSubject, mode: AddMode, ctx: &mut Ctx) {
        let id = match start_id() {
            Ok(id) => id,
            Err(code) => {
                self.screen.job_refused(&code, ctx.store.intake.as_ref());
                return;
            }
        };
        let query = IntakeQuery::JobStart {
            id,
            subject,
            mode,
            overrides: Vec::new(),
        };
        self.job_sent = Some(query.clone());
        super::super::intake::ask(ctx, query);
        assert!(self.job_sent.is_some());
    }

    /// Takes the reply to this page's move or delete start, then follows the started move in
    /// the job the shell keeps until it ends or is gone, when the project's setup is read again
    /// for its new folder; a started delete is followed on Home, where the page goes. Takes the
    /// reply and the context. Returns whether the page changed.
    pub(super) fn intake_arrived(&mut self, event: &IntakeEvent, ctx: &mut Ctx) -> bool {
        if self.publish_asked.as_ref() == Some(&event.query) {
            self.publish_answered(event, ctx);
            return true;
        }
        if self.publish_sent.as_ref() == Some(&event.query) {
            self.publish_sent = None;
            let offered = std::mem::take(&mut self.publish_offered);
            let started = matches!(event.result, Ok(IntakeView::Publish(_)));
            if let Ok(IntakeView::Publish(job)) = &event.result {
                self.publish_pane = Some(PublishPane::new((**job).clone()));
                self.publish_read_ms = Some(now_ms());
                self.ask_pane = None;
            }
            let said = match &event.result {
                Ok(IntakeView::Publish(_)) if offered => Message::success(PUBLISH_STARTED, ""),
                result => Self::publish_start_said(&self.screen.name, result),
            };
            match self.ask_pane.as_mut().filter(|pane| !pane.asking()) {
                Some(form) => form.refused(said),
                None => ctx.say(said),
            }
            if start_reply_lost(&event.result) {
                self.publish_lost_ms = Some(now_ms());
            }
            if !started {
                self.ask_publish(ctx);
            }
            return true;
        }
        if self
            .ask_pane
            .as_ref()
            .is_some_and(|pane| pane.takes(&event.query))
        {
            let next = self.ask_pane.as_mut().and_then(|pane| pane.arrived(event));
            if let Some(next) = next {
                super::super::intake::ask(ctx, next);
            }
            return true;
        }
        self.poll_publish(ctx);
        let answered = self.job_sent.as_ref() == Some(&event.query);
        if answered {
            self.job_sent = None;
            match &event.result {
                Ok(IntakeView::Job(Some(job))) if job.kind == JobKind::Delete => {
                    ctx.say(Message::success(
                        &format!("Deleting {}.", self.screen.name),
                        "Follow it on Home; you can keep working.",
                    ));
                    self.screen.removed = true;
                }
                Ok(IntakeView::Job(Some(job))) => {
                    self.moving = Some(job.id.clone());
                    ctx.say(Message::success(
                        &format!("Moving {}.", self.screen.name),
                        "Follow it on Home; you can keep working.",
                    ));
                }
                Ok(_) => self
                    .screen
                    .job_refused("intake_change_outcome_unknown", ctx.store.intake.as_ref()),
                Err(code) => self.screen.job_refused(code, ctx.store.intake.as_ref()),
            }
        }
        let ended = self.moving.as_ref().is_some_and(|id| {
            ctx.store
                .intake
                .as_ref()
                .is_none_or(|job| &job.id != id || !intake_job::running(job))
        });
        if ended {
            self.moving = None;
            self.screen.setup_due = true;
        }
        assert!(!ended || self.moving.is_none());
        answered || ended
    }

    pub(super) fn setup_arrived(&mut self, event: &SetupEvent, ctx: &mut Ctx) {
        let Some(index) = self.sent.iter().position(|query| query == &event.query) else {
            return;
        };
        let query = self.sent.remove(index);
        self.screen.now_ms = now_ms();
        self.screen.apply(&query, event.result.clone());
        let applied = matches!(event.result, Ok(SetupView::Changed(OwnerResult::Applied)));
        if matches!(query, SetupQuery::AgentPolicy { .. })
            && let Some(access) = self.access_wanted.take()
        {
            match Self::created_access_write(&self.screen.project, access, &event.result, ctx) {
                Ok((write, generation)) => {
                    self.created_access = Some(generation);
                    self.send(write, ctx);
                }
                Err(why) => Self::stays_sandboxed(&why, ctx),
            }
            return;
        }
        if matches!(query, SetupQuery::Team { .. }) && std::mem::take(&mut self.live_wanted) {
            let why = match &event.result {
                Ok(SetupView::Team(status)) => {
                    self.send_live_on(status.settings_version, ctx);
                    return;
                }
                Err(code) => plain_problem(code),
                Ok(_) => plain_problem("change_outcome_unknown"),
            };
            ctx.say(Message::warning(
                "The live master was not turned on for the new session.",
                &why,
            ));
            return;
        }
        let SetupQuery::Change { action, generation } = &query else {
            return;
        };
        if self.recording == Some(*generation) {
            self.recording = None;
            if !applied {
                ctx.say(Message::warning(
                    "The pool is changed, but the project details still name the old one.",
                    "Pick the pool again to save it.",
                ));
            }
            return;
        }
        if self.created_access == Some(*generation) {
            self.created_access = None;
            if let Some(why) = Self::created_access_unsaved(&event.result) {
                Self::stays_sandboxed(&why, ctx);
            }
            return;
        }
        let name = |job: i64| {
            self.screen
                .jobs
                .iter()
                .flatten()
                .find(|known| known.id == job)
                .map_or_else(
                    || "The research".to_owned(),
                    |known| known.kind_label.clone(),
                )
        };
        match action {
            SetupAction::CancelResearch { job, .. } if applied => {
                ctx.say(Message::success(&format!("Stopping {}.", name(*job)), ""));
            }
            SetupAction::RetryResearch { job, .. } if applied => {
                ctx.say(Message::success(
                    &format!("{} will run again.", name(*job)),
                    "",
                ));
            }
            SetupAction::AddDocuments { .. } if applied => {
                ctx.say(Message::success("Document added.", ""));
            }
            SetupAction::Update { .. } if applied => {
                ctx.say(Message::success("Changes saved.", ""));
            }
            SetupAction::Remove { .. } if applied => {
                ctx.request(Request::Projects);
                ctx.say(Message::success(
                    &format!("{} was removed.", self.screen.name),
                    "Its folder stays on disk.",
                ));
            }
            _ => {}
        }
    }

    pub(super) fn say_problem(&mut self, ctx: &mut Ctx) {
        if let Some(problem) = self.screen.problem.take() {
            ctx.say(Message::error(&problem, ""));
        }
    }

    fn stop_master(&mut self, ctx: &mut Ctx) {
        match self.master.control(false) {
            Ok(()) => ctx.say(Message::success("Stopping the master's turn.", "")),
            Err(_) => ctx.say(Message::warning(
                "The master is not working on anything.",
                "",
            )),
        }
    }

    fn end_master(&mut self, ctx: &mut Ctx) {
        if self.waiting.is_some() || self.master.has_pending() {
            ctx.say(Message::warning(
                "The master is still taking your last message.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        match self.master.control(true) {
            Ok(()) => ctx.say(Message::success(
                "Ending the conversation.",
                "Your next message starts a new one.",
            )),
            Err(_) => ctx.say(Message::warning(
                "There is no open conversation to end.",
                "",
            )),
        }
    }

    fn retry_master(&mut self, ctx: &mut Ctx) {
        let last = self
            .master
            .transcript()
            .and_then(|(_, _, input)| input)
            .map(|entry| entry.text.clone());
        match last {
            Some(text) => self.send_master(text, ctx),
            None => ctx.say(Message::warning("There is no message to send again.", "")),
        }
    }

    pub(super) fn refresh_pool(&mut self, ctx: &Ctx) {
        let (Some(pools), Some(scopes)) =
            (ctx.store.pools.ready(), ctx.store.pool_projects.ready())
        else {
            self.screen.pool_ready = false;
            return;
        };
        let picked = self
            .screen
            .setup
            .as_ref()
            .and_then(|setup| setup.pool.as_ref());
        self.screen.pool = project_pool(pools, scopes, &self.screen.project, picked);
        self.screen.set_pool_choices(pool_choices_from(pools));
        self.screen.pool_ready = true;
    }

    fn assign(&mut self, pool: Option<CredentialRef>, ctx: &mut Ctx) {
        if self.assigning.is_some() {
            ctx.say(Message::warning(
                "Still assigning the pool.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        let name = pool.as_ref().map(|pool| {
            self.screen
                .pool_choices
                .iter()
                .find(|choice| &choice.id == pool)
                .map_or_else(|| "The pool".to_owned(), |choice| choice.name.clone())
        });
        self.assigning = Some((pool.clone(), name));
        ctx.request(Request::EditPool {
            change: PoolChange::Assign {
                pool,
                project: self.screen.project.clone(),
            },
        });
    }

    /// Records the pool the owner just gave the project in its details too, so a removed
    /// pool is not offered again as the picked one. Takes the pool, None for no pool.
    /// Sends nothing when the details already name it; its reply stays quiet unless it fails.
    fn record_pool(&mut self, pool: Option<CredentialRef>, ctx: &mut Ctx) {
        let Some(setup) = self.screen.setup.as_ref() else {
            return;
        };
        if setup.pool == pool {
            return;
        }
        let action = SetupAction::Update {
            project_id: setup.project_id.clone(),
            kept: None,
            expected_revision: setup.revision,
            name: None,
            setup: ProjectSetup {
                about: setup.about.clone(),
                tech_stack: setup.tech_stack.clone(),
                platforms: setup.platforms.clone(),
                research: setup.research.clone(),
                pool,
                team: Some(setup.team.clone()),
                documents: Vec::new(),
            },
        };
        let generation = ctx.next_generation();
        self.recording = Some(generation);
        self.send(SetupQuery::Change { action, generation }, ctx);
    }

    pub(super) fn assigned(&mut self, result: &Result<(), String>, ctx: &mut Ctx) {
        let Some((pool, name)) = self.assigning.take() else {
            return;
        };
        if result.is_ok() {
            self.record_pool(pool, ctx);
        }
        match (result, name) {
            (Ok(()), Some(name)) => ctx.say(Message::success(
                &format!("{name} is assigned."),
                "Work can start now.",
            )),
            (Ok(()), None) => ctx.say(Message::success(
                "This project has no pool now.",
                "Pick a pool to start.",
            )),
            (Err(code), _) => ctx.say(Message::error(
                "The pool was not changed.",
                &plain_problem(code),
            )),
        }
        ctx.request(Request::Pools);
        ctx.request(Request::PoolProjects);
    }

    pub(super) fn redraw(&mut self, ctx: &Ctx) {
        self.screen.width = ctx.width;
        self.view = project_screen(&mut self.screen, ctx.width, ctx.height, &look(ctx));
    }

    pub(super) fn pump(&mut self, ctx: &mut Ctx) {
        if let Some(query) = self.screen.recovery.next_query(now_ms()) {
            ctx.request(Request::Recovery(Box::new(query)));
        }
        if matches!(self.screen.tab, Tab::Overview | Tab::Settings)
            && let Some(query) = self.screen.roadmap.next_query(now_ms())
        {
            ctx.request(Request::Roadmap(Box::new(query)));
        }
        if self.screen.tab == Tab::Changes
            && let Some(query) = self.screen.changes.next_query(now_ms())
        {
            ctx.request(Request::Changes(Box::new(query)));
        }
        if self.screen.tab == Tab::Questions
            && let Some(query) = self.screen.questions.next_query(now_ms())
        {
            ctx.request(Request::Questions(Box::new(query)));
        }
        if self.screen.tab == Tab::Settings
            && let Some(job) = self.screen.plugins.next_job()
        {
            send_settings(ctx, job);
        }
        if self.screen.tab == Tab::Settings && !self.schedule_asked {
            self.schedule_asked = true;
            send_settings(ctx, SettingsJob::Schedule);
        }
        if self.sent.len() >= SENT_MAX {
            return;
        }
        if let Some(query) = self.screen.next_query(now_ms()) {
            self.sent.push(query.clone());
            ctx.request(Request::Setup(query));
        }
    }

    pub(super) fn send(&mut self, query: SetupQuery, ctx: &mut Ctx) {
        if self.sent.len() >= SENT_MAX {
            ctx.say(Message::warning(
                "Still working.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        self.sent.push(query.clone());
        ctx.request(Request::Setup(query));
    }

    /// Notes a key's move onto or off the Conversation tab: entering the tab offers the live
    /// master view, a key typed on the tab or leaving it withdraws the offer.
    /// Inputs: the tab before the key and the flow the key gave. Output: the flow to apply.
    pub(super) fn entered(&mut self, before: Tab, flow: Flow) -> Flow {
        self.master_entry = self.screen.tab == Tab::Conversation && before != Tab::Conversation;
        self.offer_master(flow)
    }

    /// Opens the master view on the Conversation tab just entered, once the stored plan and a
    /// team read are in, when its master runs live (Codex or Claude Code) and Live terminal is
    /// not Off, as the desktop shows that master's terminal there; another master keeps the
    /// console. Off keeps the entry, so a read showing On opens the view until a key withdraws
    /// it (DEC-117 Q4). Input: the flow so far. Output: that flow, or the master view.
    pub(super) fn offer_master(&mut self, flow: Flow) -> Flow {
        if !matches!(flow, Flow::Handled)
            || !self.master_entry
            || self.covered
            || self.screen.setup.is_none()
        {
            return flow;
        }
        if !self.screen.live_master() {
            self.master_entry = false;
            return flow;
        }
        let unread = matches!(self.screen.team, Load::Idle | Load::Loading);
        let off = (self.screen.team.ready()).is_some_and(|graph| !graph.master_live_terminal);
        if unread || off {
            return flow;
        }
        self.master_entry = false;
        self.covered = true;
        Flow::Open(Place::Master {
            id: self.screen.project.clone(),
            name: self.screen.name.clone(),
        })
    }

    pub(super) fn reply(&mut self, reply: ProjectReply, ctx: &mut Ctx) -> Flow {
        let flow = match reply {
            ProjectReply::Ignored => Flow::Ignored,
            ProjectReply::Handled => Flow::Handled,
            ProjectReply::Home => Flow::Back,
            ProjectReply::Change(action) => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::Change {
                        action: *action,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Review {
                entry,
                version,
                decision,
            } => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::ReviewMemory {
                        project: self.screen.project.clone(),
                        entry_id: entry,
                        version,
                        decision,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Open(request) => {
                match open_request(&request) {
                    Ok(()) => ctx.say(Message::success("Opened.", "")),
                    Err(problem) => ctx.say(Message::error(&problem, "")),
                }
                Flow::Handled
            }
            ProjectReply::Reveal(target) => {
                self.reveal(target, ctx);
                Flow::Handled
            }
            ProjectReply::Edit(request) => {
                match opening(&request) {
                    Ok(Opening::Open(file)) => editor::ask(ctx, file, None),
                    Ok(Opening::Reveal(_)) => unreachable!("a folder never goes to the editor"),
                    Err(problem) => ctx.say(Message::error(&problem, "")),
                }
                Flow::Handled
            }
            ProjectReply::Assign(pool) => {
                self.assign(pool, ctx);
                Flow::Handled
            }
            ProjectReply::PickPool => {
                ctx.say(Message::warning(
                    "There are no pools yet.",
                    "Create one here, then assign it in the project's Settings tab.",
                ));
                Flow::Open(Place::Settings(SettingsSection::Pools))
            }
            ProjectReply::Send(text) => {
                self.send_master(text, ctx);
                Flow::Handled
            }
            ProjectReply::StopTurn => {
                self.stop_master(ctx);
                Flow::Handled
            }
            ProjectReply::EndConversation => {
                self.end_master(ctx);
                Flow::Handled
            }
            ProjectReply::Session(action) => {
                let in_view = self
                    .master
                    .transcript()
                    .is_some_and(|(session, _, _)| session.is_some());
                if let Err(problem) = super::super::sessions::act(&mut self.master, &action) {
                    let said = Message::error("The session did not switch.", &problem);
                    if problem == NEW_SESSION_STARTING {
                        self.starting = Some(said.clone());
                    }
                    ctx.say(said);
                } else if matches!(action, super::super::sessions::SessionsAction::New { .. })
                    && !in_view
                    && self.screen.live_master()
                {
                    self.turn_live_on(ctx);
                }
                Flow::Handled
            }
            ProjectReply::Retry => {
                self.retry_master(ctx);
                Flow::Handled
            }
            ProjectReply::RefreshOverview => {
                let project = self.screen.project.clone();
                self.send(SetupQuery::RefreshOverview { project }, ctx);
                Flow::Handled
            }
            ProjectReply::Team => Flow::Open(Place::Team {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::TeamSettings => Flow::Open(Place::TeamSettings {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Master => Flow::Open(Place::Master {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Process => Flow::Open(Place::ProjectProcess {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::Memory => Flow::Open(Place::ProjectMemory {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::GitHubRules => Flow::Open(Place::ProjectGitHub {
                id: self.screen.project.clone(),
                name: self.screen.name.clone(),
            }),
            ProjectReply::TeamChange(action) => {
                let generation = ctx.next_generation();
                self.send(
                    SetupQuery::TeamChange {
                        action: *action,
                        generation,
                    },
                    ctx,
                );
                Flow::Handled
            }
            ProjectReply::Usage => Flow::Open(Place::Usage),
            ProjectReply::AccountOrder(strategy) => {
                self.set_order(strategy, ctx);
                Flow::Handled
            }
            ProjectReply::MoveHome(mode) => {
                let project_id = self.screen.project.clone();
                self.start_job(JobSubject::MoveProject { project_id }, mode, ctx);
                Flow::Handled
            }
            ProjectReply::DeleteEverything(name) => {
                let project_id = self.screen.project.clone();
                let subject = JobSubject::Delete { project_id, name };
                self.start_job(subject, AddMode::Move, ctx);
                Flow::Handled
            }
        };
        self.sync_master(ctx);
        self.pump(ctx);
        self.say_problem(ctx);
        self.redraw(ctx);
        flow
    }

    fn set_order(&mut self, strategy: Option<crate::scheduling::Strategy>, ctx: &mut Ctx) {
        let Ok(project) = CredentialRef::new(self.screen.project.as_str()) else {
            self.screen.problem = Some("This project can't keep its own account order.".into());
            return;
        };
        let generation = ctx.next_generation();
        send_settings(
            ctx,
            SettingsJob::SetSchedule {
                change: crate::scheduling::ScheduleChange::ProjectStrategy { project, strategy },
                version: self
                    .screen
                    .schedule
                    .as_ref()
                    .map_or(0, |schedule| schedule.version),
                generation,
            },
        );
    }

    pub(super) fn schedule_arrived(&mut self, event: &SettingsEvent, ctx: &mut Ctx) {
        let failed = |code: &str, fallback: &'static str| {
            crate::config::plain_problem(code)
                .unwrap_or(fallback)
                .to_owned()
        };
        match event {
            SettingsEvent::Schedule(Ok(schedule)) => self.screen.schedule = Some(schedule.clone()),
            SettingsEvent::Schedule(Err(code)) => {
                self.screen.problem = Some(failed(code, "Could not read the account order."));
            }
            SettingsEvent::ScheduleSet(_, result) => {
                if let Err(code) = result {
                    self.screen.problem = Some(failed(code, "Could not save the account order."));
                }
                send_settings(ctx, SettingsJob::Schedule);
            }
            _ => {}
        }
    }

    pub(super) fn reveal(&self, target: RevealTarget, ctx: &mut Ctx) {
        ctx.request(Request::Reveal(Box::new(RevealQuery {
            project: self.screen.project.clone(),
            target,
        })));
    }

    pub(super) fn edit_changed(&self, path: &str, ctx: &mut Ctx) {
        let Some(folder) = self.screen.project_folder() else {
            ctx.say(Message::error(
                "The project folder isn't loaded yet.",
                "Try again in a moment.",
            ));
            return;
        };
        match changed_file(&folder, path) {
            Ok(file) => editor::ask(ctx, file, Some(CHANGED_NOTE)),
            Err(problem) => ctx.say(Message::error(problem, "")),
        }
    }

    pub(super) fn recovery_has_keys(&self) -> bool {
        self.screen.edit.is_none()
            && self.screen.review.is_none()
            && self.screen.pool_picker.is_none()
            && self.screen.confirm_delete.is_none()
            && self.screen.confirm_stop.is_none()
            && self.screen.adding.is_none()
            && self.screen.folders.is_none()
            && self.screen.general.is_none()
            && !self.screen.confirm_remove
            && !self.screen.confirm_move
            && !self.screen.changes.editing()
            && !self.typing()
    }

    pub(super) fn changes_have_keys(&self) -> bool {
        self.screen.tab == Tab::Changes && !self.screen.boxed()
    }

    pub(super) fn questions_have_keys(&self) -> bool {
        self.screen.tab == Tab::Questions
            && self.screen.edit.is_none()
            && self.screen.review.is_none()
    }

    pub(super) fn plugins_have_keys(&self) -> bool {
        self.screen.tab == Tab::Settings && !self.screen.boxed()
    }

    pub(super) fn plugins_reply(&mut self, reply: PluginsReply, ctx: &mut Ctx) -> Flow {
        let project = self.screen.project.clone();
        let name = self.screen.name.clone();
        let flow = match reply {
            PluginsReply::Ignored => Flow::Ignored,
            PluginsReply::Handled => Flow::Handled,
            PluginsReply::Send(job) => {
                send_settings(ctx, job);
                Flow::Handled
            }
            PluginsReply::AddSkill => {
                return Flow::Replace(Box::new(AddPlugin::skill().for_project(project, name)));
            }
            PluginsReply::AddServer => {
                return Flow::Replace(Box::new(AddPlugin::server().for_project(project, name)));
            }
            PluginsReply::AddHooks => {
                return Flow::Replace(Box::new(AddPlugin::hooks().for_project(project, name)));
            }
            PluginsReply::OpenSkill { id, name: skill } => {
                return Flow::Replace(Box::new(
                    SkillDetailPage::new(id, skill).for_project(project, name),
                ));
            }
            PluginsReply::Edit(server) => {
                return Flow::Replace(Box::new(
                    EditServer::new(*server).for_project(project, name),
                ));
            }
            PluginsReply::Tools { servers, at } => {
                return Flow::Replace(Box::new(
                    ToolsPage::new(servers, at).for_project(project, name),
                ));
            }
        };
        self.pump(ctx);
        self.redraw(ctx);
        flow
    }
}
