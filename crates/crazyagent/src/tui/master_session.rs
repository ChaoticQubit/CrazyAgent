use super::app::UiEvent;
use super::client::MutationDelivery;
use super::model_access::{HomeSelection, rcli_home_supported};
use crate::client_controller::OwnerController;
use crate::config::{AccountPool, ModelAccess, ModelConnection, ModelEndpoint};
use crate::conversation::{ConversationContent, ConversationCursor, ConversationEntry};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    ModelAccessCollection, ModelAccessCursor, ModelAccessRecord, OwnerApplied, OwnerData,
    OwnerProvenance, OwnerRead, OwnerReadReply, OwnerResponse, OwnerResult,
};
use crate::master_session::{
    MasterBackend, MasterCursor, MasterOwnerAction, MasterReason, MasterSelection,
    MasterSessionPhase, MasterSessionView, MasterTurnPhase, MasterTurnView,
    RcliProfileRegistration, RcliProfileRegistrationMetadata,
};
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId};
use crate::routing::{ResourceBudget, RouteRequest};
use std::time::{Duration, Instant};

const DIMENSIONS: [&str; 7] = [
    "slots",
    "cpu_millis",
    "memory_mib",
    "processes",
    "output_bytes",
    "wall_time_ms",
    "spend_microunits",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Context {
    pub identity: OwnerConnectionIdentity,
    pub project: ProjectId,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub identity: OwnerConnectionIdentity,
    pub generation: u64,
    pub action: MasterOwnerAction,
}

#[derive(Clone)]
pub(super) struct Confirmation {
    pub submission: Submission,
    pub receipt: OwnerProvenance,
}

#[derive(Clone)]
pub(super) enum Job {
    Catalog {
        context: Context,
    },
    Read {
        context: Context,
        confirmation: Option<Box<Confirmation>>,
    },
    Mutation {
        submission: Submission,
        delivery: MutationDelivery,
    },
}

#[derive(Clone)]
pub(super) enum Event {
    Catalog {
        context: Context,
        result: Result<Box<Catalog>, String>,
    },
    Read {
        context: Context,
        result: Result<Box<View>, String>,
    },
    Mutation {
        submission: Submission,
        operation: Option<(RequestId, String)>,
        result: Result<Box<OwnerResponse>, String>,
        retained: bool,
    },
}

#[derive(Clone)]
pub(super) struct Catalog {
    config_id: ConfigId,
    source_hash: String,
    revision: Cursor,
    models: ModelAccess,
    approved: Vec<CredentialRef>,
    targets: Vec<CredentialRef>,
    capacities: [ResourceBudget; 3],
}

#[derive(Clone)]
pub(super) struct View {
    session: Option<MasterSessionView>,
    turns: Vec<MasterTurnView>,
    revision: Cursor,
    input: Option<ConversationEntry>,
}

struct Form {
    database: Option<String>,
    project: ProjectId,
    backend: MasterBackend,
    pool: Option<CredentialRef>,
    account: Option<CredentialRef>,
    target: Option<CredentialRef>,
    values: [String; 7],
    field: usize,
    generation: u64,
    open: bool,
    explicit: bool,
    edited: bool,
}

struct Pending {
    submission: Submission,
    operation: Option<(RequestId, String)>,
    receipt: Option<OwnerProvenance>,
    retry_allowed: bool,
    registration: Option<RegisteredHome>,
}

struct RegisteredHome {
    identity: OwnerConnectionIdentity,
    metadata: RcliProfileRegistrationMetadata,
    provenance: OwnerProvenance,
    historical: bool,
}

pub(super) struct State {
    identity: Option<OwnerConnectionIdentity>,
    project: Option<ProjectId>,
    ready: bool,
    generation: u64,
    forms: Vec<Form>,
    catalog: Option<Catalog>,
    catalog_attempted: bool,
    view: Option<View>,
    flight: Option<Context>,
    next_read: Instant,
    pending: Option<Pending>,
    job: Option<Job>,
    home_confirmation: Option<RcliProfileRegistration>,
    registered_home: Option<RegisteredHome>,
    pub notice: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            identity: None,
            project: None,
            ready: false,
            generation: 1,
            forms: Vec::new(),
            catalog: None,
            catalog_attempted: false,
            view: None,
            flight: None,
            next_read: Instant::now(),
            pending: None,
            job: None,
            home_confirmation: None,
            registered_home: None,
            notice: "Current master metadata unavailable; inspect Settings; Ctrl-R reconnects"
                .into(),
        }
    }
}

impl State {
    /// The current master transcript, read-only, for the Project screen.
    /// Input: none. Output: the session, its turns and the latest owner input
    /// once a read arrived; None before the first read.
    pub fn transcript(
        &self,
    ) -> Option<(
        Option<&MasterSessionView>,
        &[MasterTurnView],
        Option<&ConversationEntry>,
    )> {
        self.view.as_ref().map(|view| {
            (
                view.session.as_ref(),
                view.turns.as_slice(),
                view.input.as_ref(),
            )
        })
    }

    pub fn select(
        &mut self,
        identity: Option<OwnerConnectionIdentity>,
        project: Option<ProjectId>,
        ready: bool,
    ) -> Result<(), String> {
        if self.identity != identity || self.project != project {
            self.home_confirmation = None;
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or("Master view generation exhausted")?;
            self.view = None;
            self.flight = None;
            self.next_read = Instant::now();
            if self.identity != identity {
                self.catalog = None;
                self.catalog_attempted = false;
            }
            self.identity = identity;
            self.project = project;
        }
        self.ready = ready;
        Ok(())
    }

    pub fn access(&mut self) {
        self.home_confirmation = None;
        self.catalog = None;
        self.catalog_attempted = false;
        self.view = None;
        self.flight = None;
        self.next_read = Instant::now();
        if self.ready
            && let Some(pending) = &self.pending
            && pending.receipt.is_none()
            && pending.operation.is_some()
        {
            self.job = Some(Job::Mutation {
                submission: pending.submission.clone(),
                delivery: MutationDelivery::Reconcile,
            });
        }
    }

    fn form_index(&self) -> Option<usize> {
        let project = self.project.as_ref()?;
        let database = self.identity.as_ref().map(|id| id.database_instance());
        self.forms
            .iter()
            .position(|form| &form.project == project && form.database.as_deref() == database)
    }

    pub fn open(&mut self) -> Result<(), String> {
        self.home_confirmation = None;
        let project = self
            .project
            .clone()
            .ok_or("Select a project before Configure master (F2)")?;
        let index = if let Some(index) = self.form_index() {
            index
        } else {
            if self.forms.len() >= 128 {
                return Err("Master setup draft capacity reached; existing fields retained".into());
            }
            let policy = crate::routing::RoutingPolicy::default();
            let budget = default_master_budget(&[
                policy.global_capacity,
                policy.project_capacity,
                policy.task_capacity,
            ]);
            self.forms.push(Form {
                database: self
                    .identity
                    .as_ref()
                    .map(|id| id.database_instance().to_owned()),
                project,
                backend: MasterBackend::Rcli,
                pool: None,
                account: None,
                target: None,
                values: budget_values(&budget).map(|value| value.to_string()),
                field: 0,
                generation: 1,
                open: false,
                explicit: false,
                edited: false,
            });
            self.forms.len() - 1
        };
        if self.pending.is_none() {
            self.catalog = None;
            self.catalog_attempted = false;
        }
        self.forms[index].open = true;
        self.forms[index].field = 0;
        self.prefer_assigned_backend();
        self.defaults();
        self.assert_consistent();
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.form_index()
            .is_some_and(|index| self.forms[index].open)
    }

    pub fn draft(&self) -> Option<&str> {
        let form = &self.forms[self.form_index()?];
        if !form.open || form.field < 4 {
            return None;
        }
        Some(&form.values[form.field - 4])
    }

    pub fn draft_label(&self) -> &'static str {
        if self.home_confirmation.is_some() {
            "Register configured SDK home | Enter confirms | Esc retains conversation draft"
        } else {
            "Master setup | Enter Open | Esc retains conversation draft"
        }
    }

    pub fn configure_home<'a>(&mut self, homes: impl Iterator<Item = HomeSelection<'a>>) {
        self.home_confirmation = None;
        match self.home_intent(homes) {
            Ok(intent) => {
                self.home_confirmation = Some(intent);
                self.notice =
                    "Review actual session, connection, sandbox and selected SDK home".into();
            }
            Err(reason) => self.notice = format!("SDK home registration disabled: {reason}"),
        }
    }

    fn home_intent<'a>(
        &self,
        mut homes: impl Iterator<Item = HomeSelection<'a>>,
    ) -> Result<RcliProfileRegistration, String> {
        if !self.is_open() || self.has_pending() || !self.ready {
            return Err("an idle actual owner and open master setup are required".into());
        }
        let session = self
            .view
            .as_ref()
            .and_then(|view| view.session.as_ref())
            .filter(|session| {
                session.phase == MasterSessionPhase::Configured
                    && session.selection.backend == MasterBackend::Rcli
                    && session.current_turn.is_none()
            })
            .ok_or("Open an RCLI master and wait for its actual Configured read first")?;
        if self.selection()? != session.selection {
            return Err("setup differs from the actual master; review and Open it first".into());
        }
        let catalog = self
            .catalog
            .as_ref()
            .ok_or("current owner catalog unavailable")?;
        if catalog.config_id != session.config_id || catalog.source_hash != session.source_hash {
            return Err("catalog and session basis differ; refresh before registration".into());
        }
        let connection = catalog.rcli_connection(session)?;
        let home = homes
            .find(|home| {
                Some(&home.basis.identity) == self.identity.as_ref()
                    && home.basis.writable
                    && home.basis.config_id == catalog.config_id
                    && home.basis.source_hash == catalog.source_hash
                    && home.connection == connection
            })
            .ok_or("select this connection's SDK home in Settings with h first")?;
        Ok(RcliProfileRegistration {
            expected_session: session.clone(),
            expected_connection: connection.clone(),
            configured_home: home.configured_home.clone(),
        })
    }

    fn submit_home(&mut self, index: usize) -> Result<(), String> {
        let intent = self.home_confirmation.as_ref().unwrap();
        if self.view.as_ref().and_then(|view| view.session.as_ref())
            != Some(&intent.expected_session)
            || self.selection()? != intent.expected_session.selection
        {
            return Err("SDK home confirmation is stale; draft retained; reopen with h".into());
        }
        let action = MasterOwnerAction::RegisterRcliProfile {
            registration: Box::new(intent.clone()),
        };
        self.enqueue(action, self.forms[index].generation)
    }

    /// Drops the setup picks the current catalog no longer offers: a pool that no longer
    /// leads this project, an account no longer in it, or a sandbox no longer allowed.
    /// Input: none. Output: none; a dropped pick is chosen again from current settings.
    fn forget_stale_picks(&mut self) {
        let (Some(index), Some(catalog)) = (self.form_index(), self.catalog.as_ref()) else {
            return;
        };
        let form = &self.forms[index];
        let pool = form.pool.as_ref().is_some_and(|pool| {
            !catalog
                .pools(form)
                .iter()
                .any(|offered| &offered.id == pool)
        });
        let account = form
            .account
            .as_ref()
            .is_some_and(|account| !catalog.accounts(form).contains(account));
        let target = form
            .target
            .as_ref()
            .is_some_and(|target| !catalog.target_choices(form).contains(target));
        let form = &mut self.forms[index];
        if pool {
            form.pool = None;
        }
        if pool || account {
            form.account = None;
        }
        if pool || account || target {
            form.target = None;
        }
    }

    fn prefer_assigned_backend(&mut self) {
        let (Some(index), Some(catalog)) = (self.form_index(), self.catalog.as_ref()) else {
            return;
        };
        let form = &mut self.forms[index];
        if form.pool.is_some() || !catalog.pools(form).is_empty() {
            return;
        }
        if let Some(backend) = catalog.assigned_backend(form) {
            form.backend = backend;
            form.account = None;
            form.target = None;
        }
    }

    fn defaults(&mut self) {
        let (Some(index), Some(catalog)) = (self.form_index(), self.catalog.as_ref()) else {
            return;
        };
        let form = &mut self.forms[index];
        if form.pool.is_none() {
            form.pool = catalog.pools(form).first().map(|pool| pool.id.clone());
        }
        if form.account.is_none() {
            form.account = catalog.accounts(form).first().cloned();
        }
        if form.target.is_none() {
            form.target = catalog.target(form);
        }
        if !form.edited {
            form.values = budget_values(&default_master_budget(&catalog.capacities))
                .map(|value| value.to_string());
        }
    }

    pub fn reduce(&mut self, event: &UiEvent) -> Result<bool, String> {
        if !self.is_open() {
            return Ok(false);
        }
        let index = self.form_index().unwrap();
        if self.home_confirmation.is_some() {
            match event {
                UiEvent::Resize(..) | UiEvent::Scroll(_) => return Ok(false),
                UiEvent::CancelMaster | UiEvent::CloseMaster => {
                    self.home_confirmation = None;
                    return Ok(false);
                }
                UiEvent::ToggleBrief
                | UiEvent::ToggleUnassigned
                | UiEvent::HistoryNext
                | UiEvent::HistoryFirst => {
                    self.home_confirmation = None;
                    self.forms[index].open = false;
                    return Ok(false);
                }
                UiEvent::Submit => self.submit_home(index)?,
                UiEvent::Cancel => {
                    self.home_confirmation = None;
                    self.forms[index].open = false;
                }
                _ => {}
            }
            return Ok(true);
        }
        match event {
            UiEvent::FocusNext | UiEvent::FocusPrevious => {
                let delta = if matches!(event, UiEvent::FocusNext) {
                    1
                } else {
                    10
                };
                self.forms[index].field = (self.forms[index].field + delta) % 11;
            }
            UiEvent::SelectionNext | UiEvent::SelectionPrevious => {
                let forward = matches!(event, UiEvent::SelectionNext);
                if !self.forms[index].select(self.catalog.as_ref(), forward)? {
                    return Ok(true);
                }
                self.defaults();
            }
            UiEvent::Character(character) => {
                self.forms[index].edit_allowance(Some(*character))?;
            }
            UiEvent::Backspace => self.forms[index].edit_allowance(None)?,
            UiEvent::Submit => self.open_selection(index)?,
            UiEvent::Cancel => self.forms[index].open = false,
            UiEvent::ToggleBrief
            | UiEvent::ToggleUnassigned
            | UiEvent::HistoryNext
            | UiEvent::HistoryFirst => {
                self.forms[index].open = false;
                return Ok(false);
            }
            _ => return Ok(false),
        }
        self.assert_consistent();
        Ok(true)
    }

    fn open_selection(&mut self, index: usize) -> Result<(), String> {
        let selection = self.selection()?;
        let catalog = self.catalog.as_ref().unwrap();
        let action = MasterOwnerAction::Open {
            project_id: self.project.clone().unwrap(),
            expected_config_id: catalog.config_id.clone(),
            expected_source_hash: catalog.source_hash.clone(),
            selection: Box::new(selection),
        };
        self.enqueue(action, self.forms[index].generation)?;
        self.forms[index].explicit = true;
        Ok(())
    }

    fn selection(&self) -> Result<MasterSelection, String> {
        if !self.ready {
            return Err(concat!(
                "Open disabled: authenticated owner unavailable; ",
                "inspect Settings; Ctrl-R reconnects"
            )
            .into());
        }
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| format!("Open disabled: {}; inspect Settings", self.notice))?;
        let form = &self.forms[self.form_index().ok_or("Select a project")?];
        let pool = catalog
            .pools(form)
            .into_iter()
            .find(|pool| Some(&pool.id) == form.pool.as_ref())
            .ok_or(
                "Open disabled: no approved project_leader pool for this backend; inspect Settings",
            )?;
        let account = form
            .account
            .as_ref()
            .filter(|account| catalog.accounts(form).contains(account))
            .ok_or(
                "Open disabled: no approved backend account in the selected pool; inspect Settings",
            )?;
        let target = form
            .target
            .as_ref()
            .filter(|target| catalog.targets.contains(target))
            .ok_or("Open disabled: no allowed execution sandbox; inspect Settings")?;
        let resources = form.resources()?;
        if catalog
            .capacities
            .iter()
            .any(|capacity| !resources.fits(capacity))
        {
            return Err(concat!(
                "Open disabled: allowance exceeds current owner policy; ",
                "all seven values retained; inspect Settings"
            )
            .into());
        }
        Ok(MasterSelection {
            backend: form.backend,
            pool_ref: pool.id.clone(),
            target_profile_ref: target.clone(),
            route: RouteRequest {
                preferred_target: Some(target.clone()),
                required_target: Some(target.clone()),
                required_capabilities: Vec::new(),
                account_ref: account.clone(),
                budget_ref: pool.id.clone(),
                resources,
            },
        })
    }

    fn enqueue(&mut self, action: MasterOwnerAction, generation: u64) -> Result<(), String> {
        if !self.ready || self.pending.is_some() || self.job.is_some() {
            return Err(concat!(
                "Master owner intent pending or unavailable; ",
                "draft and exact identity retained; Ctrl-R recovers"
            )
            .into());
        }
        let submission = Submission {
            identity: self
                .identity
                .clone()
                .ok_or("Master owner identity unavailable")?,
            generation,
            action,
        };
        assert!(submission.generation > 0);
        self.job = Some(Job::Mutation {
            submission: submission.clone(),
            delivery: MutationDelivery::Submit,
        });
        self.pending = Some(Pending {
            submission,
            operation: None,
            receipt: None,
            retry_allowed: false,
            registration: None,
        });
        self.notice = "Master intent queued for the actual owner; drafts retained".into();
        Ok(())
    }

    pub fn submit(&mut self, text: &str, generation: u64) -> Result<bool, String> {
        let Some(view) = self.view.as_ref().filter(|_| self.ready) else {
            return Err(
                "Waiting for actual master state; input retained; Ctrl-R reconnects".into(),
            );
        };
        let Some(session) = &view.session else {
            if self
                .form_index()
                .is_some_and(|index| self.forms[index].explicit)
            {
                return Err("Explicit master intent unresolved; input retained".into());
            }
            return Ok(false);
        };
        if session.phase != MasterSessionPhase::Configured {
            return Err(
                "Master is closed; F2 Configure master before submitting a turn; draft retained"
                    .into(),
            );
        }
        if turn_holds(session.current_turn.as_ref(), &view.turns) {
            return Err(
                "A master turn is outstanding; F3 cancels that exact turn; draft retained".into(),
            );
        }
        let action = MasterOwnerAction::Submit {
            project_id: session.project_id.clone(),
            session_id: session.session_id.clone(),
            text: text.to_owned(),
        };
        self.enqueue(action, generation)?;
        Ok(true)
    }

    pub fn control(&mut self, close: bool) -> Result<(), String> {
        let session = self
            .view
            .as_ref()
            .filter(|_| self.ready)
            .and_then(|view| view.session.as_ref())
            .ok_or("No current actual master session; F2 configures one")?;
        if session.phase != MasterSessionPhase::Configured {
            return Err("Master session is already closed or closing".into());
        }
        let action = if close {
            MasterOwnerAction::Close {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
            }
        } else {
            MasterOwnerAction::Cancel {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                turn_id: session
                    .current_turn
                    .clone()
                    .ok_or("No outstanding actual master turn to cancel")?,
            }
        };
        self.enqueue(action, self.generation)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn retry(&mut self) -> bool {
        let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.retry_allowed)
        else {
            return false;
        };
        if !self.ready || self.job.is_some() {
            return false;
        }
        pending.retry_allowed = false;
        self.job = Some(Job::Mutation {
            submission: pending.submission.clone(),
            delivery: MutationDelivery::Retry,
        });
        self.notice =
            "Retrying the exact original owner operation after an actual not-recorded barrier"
                .into();
        true
    }

    pub fn request(&mut self, active: bool) -> Option<Job> {
        if !self.ready {
            return None;
        }
        if let Some(job) = self.job.take() {
            return Some(job);
        }
        if self.flight.is_some() {
            return None;
        }
        let confirmation = self.pending.as_ref().and_then(|pending| {
            pending.receipt.as_ref().map(|receipt| Confirmation {
                submission: pending.submission.clone(),
                receipt: receipt.clone(),
            })
        });
        let project = confirmation
            .as_ref()
            .map(|confirmation| action_project(&confirmation.submission.action).clone())
            .or_else(|| self.project.clone())?;
        if !active && confirmation.is_none() {
            return None;
        }
        let context = Context {
            identity: self.identity.clone()?,
            project,
            generation: self.generation,
        };
        if self.is_open() && !self.catalog_attempted && self.pending.is_none() {
            self.catalog_attempted = true;
            self.flight = Some(context.clone());
            return Some(Job::Catalog { context });
        }
        if Instant::now() < self.next_read {
            return None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.receipt.is_none())
        {
            return None;
        }
        self.flight = Some(context.clone());
        Some(Job::Read {
            context,
            confirmation: confirmation.map(Box::new),
        })
    }

    pub fn apply(&mut self, event: Event) -> Result<Option<Submission>, String> {
        match event {
            Event::Catalog { context, result } => {
                self.apply_catalog(context, result);
                Ok(None)
            }
            Event::Read { context, result } => self.apply_read(context, result),
            Event::Mutation {
                submission,
                operation,
                result,
                retained,
            } => {
                self.apply_mutation(submission, operation, result, retained)?;
                Ok(None)
            }
        }
    }

    fn apply_catalog(&mut self, context: Context, result: Result<Box<Catalog>, String>) {
        if self.flight.as_ref() != Some(&context) {
            return;
        }
        self.flight = None;
        if !self.matches(&context) {
            return;
        }
        match result {
            Ok(catalog) => {
                self.catalog = Some(*catalog);
                self.forget_stale_picks();
                self.prefer_assigned_backend();
                self.defaults();
                self.notice = concat!(
                    "Actual owner catalog and policy loaded; ",
                    "native delivery/authentication remain unsupported"
                )
                .into();
            }
            Err(problem) => {
                self.catalog = None;
                self.notice =
                    format!("Metadata unavailable: {problem}; inspect Settings; Ctrl-R refreshes");
            }
        }
        self.assert_consistent();
    }

    fn apply_read(
        &mut self,
        context: Context,
        result: Result<Box<View>, String>,
    ) -> Result<Option<Submission>, String> {
        if self.flight.as_ref() == Some(&context) {
            self.flight = None;
        }
        let current = self.matches(&context);
        if self.identity.as_ref() != Some(&context.identity) {
            return Ok(None);
        }
        self.next_read = Instant::now() + Duration::from_secs(1);
        match result {
            Ok(view) => {
                let completed = match self.confirm(&context, &view) {
                    Ok(completed) => completed,
                    Err(problem) => {
                        self.notice =
                            format!("{problem}; receipt and drafts retained; Ctrl-R refreshes");
                        if current {
                            self.view = None;
                        }
                        return Ok(None);
                    }
                };
                if current {
                    self.view = Some(*view);
                }
                if let Some(submission) = completed {
                    self.finish_read(&submission);
                    return Ok(Some(submission));
                }
            }
            Err(problem) => {
                if current {
                    self.view = None;
                }
                self.ready = false;
                self.notice = format!(
                    "Master read unavailable: {problem}; drafts retained; Ctrl-R reconnects"
                );
            }
        }
        self.assert_consistent();
        Ok(None)
    }

    fn finish_read(&mut self, submission: &Submission) {
        let pending = self
            .pending
            .take()
            .expect("a confirmed read retains its intent");
        let registered = pending.registration.is_some();
        if registered {
            self.registered_home = pending.registration;
            self.home_confirmation = None;
        }
        if matches!(
            submission.action,
            MasterOwnerAction::Open { .. } | MasterOwnerAction::RegisterRcliProfile { .. }
        ) && let Some(form) = self.forms.iter_mut().find(|form| {
            form.database.as_deref() == Some(submission.identity.database_instance())
                && &form.project == action_project(&submission.action)
        }) && form.generation == submission.generation
        {
            form.open = false;
        }
        self.notice = if registered {
            concat!(
                "Registered configured profile; owner receipt and session read agree; ",
                "no sign-in observed"
            )
            .into()
        } else {
            concat!(
                "Actual master receipt and read agree; ",
                "native delivery/authentication remain unsupported"
            )
            .into()
        };
    }

    fn apply_mutation(
        &mut self,
        submission: Submission,
        operation: Option<(RequestId, String)>,
        result: Result<Box<OwnerResponse>, String>,
        retained: bool,
    ) -> Result<(), String> {
        let pending = self
            .pending
            .as_mut()
            .ok_or("Master completion has no retained submission")?;
        assert_eq!(pending.submission, submission);
        if pending
            .operation
            .as_ref()
            .is_some_and(|original| Some(original) != operation.as_ref())
        {
            return Err("Master owner operation identity changed; original intent retained".into());
        }
        pending.operation = operation.clone();
        pending.retry_allowed = false;
        match result {
            Ok(response) => self.apply_response(*response, operation.as_ref(), retained)?,
            Err(problem) => self.mutation_unavailable(problem, retained),
        }
        self.assert_consistent();
        Ok(())
    }

    fn apply_response(
        &mut self,
        response: OwnerResponse,
        operation: Option<&(RequestId, String)>,
        retained: bool,
    ) -> Result<(), String> {
        match response {
            OwnerResponse::Applied { operation: applied }
            | OwnerResponse::Committed { operation: applied } => {
                self.apply_receipt(applied, operation, retained)?;
            }
            OwnerResponse::NotRecordedAtBarrier { .. } => {
                if !retained {
                    return Err("Master barrier lost original intent".into());
                }
                self.pending.as_mut().unwrap().retry_allowed = true;
                self.notice = concat!(
                    "Original owner operation not recorded at actual barrier; ",
                    "Ctrl-R explicitly retries that exact request"
                )
                .into();
            }
            OwnerResponse::OutcomeUnknown { operation_id } => {
                if !retained || !operation.is_some_and(|operation| operation.0 == operation_id) {
                    return Err("Master unknown outcome lost original identity".into());
                }
                self.notice = concat!(
                    "Owner outcome unknown; original identity and drafts retained; ",
                    "Ctrl-R reconnects and reconciles"
                )
                .into();
            }
            OwnerResponse::Rejected { code } => {
                if !retained {
                    self.pending = None;
                }
                self.notice = format!(
                    concat!(
                        "Owner admission rejected: {code}; ",
                        "draft and any original intent retained"
                    ),
                    code = code
                );
            }
            OwnerResponse::Read { .. } => {
                return Err("Unexpected read for master mutation".into());
            }
        }
        Ok(())
    }

    fn apply_receipt(
        &mut self,
        applied: OwnerApplied,
        operation: Option<&(RequestId, String)>,
        retained: bool,
    ) -> Result<(), String> {
        if retained
            || operation
                != Some(&(
                    applied.provenance.operation_id.clone(),
                    applied.provenance.request_hash.clone(),
                ))
        {
            return Err("Master receipt provenance mismatch; intent retained".into());
        }
        match applied.result {
            OwnerResult::Applied => {
                if matches!(
                    self.pending.as_ref().unwrap().submission.action,
                    MasterOwnerAction::RegisterRcliProfile { .. }
                ) {
                    return Err(
                        "Registration requires the actual configured-profile receipt".into(),
                    );
                }
                self.pending.as_mut().unwrap().receipt = Some(applied.provenance);
                self.next_read = Instant::now();
                self.notice = concat!(
                    "Owner applied master intent; ",
                    "waiting for matching actual session/input/turn read"
                )
                .into();
            }
            OwnerResult::RcliProfileRegistered { registration } => {
                self.accept_registration(
                    *registration,
                    applied.provenance,
                    applied.historical_replay,
                )?;
            }
            OwnerResult::Rejected { code } => {
                self.pending = None;
                self.notice =
                    format!("Owner rejected master intent: {code}; fields and draft retained");
            }
            _ => return Err("Unexpected master owner result".into()),
        }
        Ok(())
    }

    fn accept_registration(
        &mut self,
        metadata: RcliProfileRegistrationMetadata,
        provenance: OwnerProvenance,
        historical: bool,
    ) -> Result<(), String> {
        let pending = self.pending.as_mut().unwrap();
        let MasterOwnerAction::RegisterRcliProfile { registration } = &pending.submission.action
        else {
            return Err("Unexpected configured-profile receipt for another master action".into());
        };
        let session = &registration.expected_session;
        if metadata.project_id != session.project_id
            || metadata.session_id != session.session_id
            || metadata.profile_ref != session.selection.target_profile_ref
            || metadata.generation == 0
        {
            return Err(
                "Configured-profile receipt scope mismatch; original intent retained".into(),
            );
        }
        pending.registration = Some(RegisteredHome {
            identity: pending.submission.identity.clone(),
            metadata,
            provenance: provenance.clone(),
            historical,
        });
        pending.receipt = Some(provenance);
        self.next_read = Instant::now();
        self.notice = concat!(
            "Owner recorded configured profile; ",
            "waiting for the matching actual session read"
        )
        .into();
        Ok(())
    }

    fn mutation_unavailable(&mut self, problem: String, retained: bool) {
        if !retained {
            self.pending = None;
        }
        self.ready = false;
        self.notice = if retained {
            format!(
                concat!(
                    "Owner outcome unknown: {problem}; ",
                    "original identity and drafts retained; Ctrl-R reconnects and reconciles"
                ),
                problem = problem
            )
        } else {
            format!(
                concat!(
                    "Master intent unavailable: {problem}; ",
                    "fields and draft retained; Ctrl-R reconnects"
                ),
                problem = problem
            )
        };
    }

    fn matches(&self, context: &Context) -> bool {
        self.ready
            && self.identity.as_ref() == Some(&context.identity)
            && self.project.as_ref() == Some(&context.project)
            && self.generation == context.generation
    }

    fn confirm(&self, context: &Context, view: &View) -> Result<Option<Submission>, String> {
        let Some(pending) = &self.pending else {
            return Ok(None);
        };
        let Some(receipt) = &pending.receipt else {
            return Ok(None);
        };
        if matches!(
            pending.submission.action,
            MasterOwnerAction::RegisterRcliProfile { .. }
        ) && pending.registration.is_none()
        {
            return Err("Configured-profile receipt metadata missing".into());
        }
        if action_project(&pending.submission.action) != &context.project
            || pending.submission.identity.database_instance()
                != context.identity.database_instance()
            || view.revision.value() < receipt.revision_after.value()
        {
            return Ok(None);
        }
        if !view.confirms(&pending.submission.action, receipt)? {
            return Err(
                "Master receipt and actual read do not agree; draft and receipt retained".into(),
            );
        }
        Ok(Some(pending.submission.clone()))
    }

    pub fn disconnected(&self) -> bool {
        !self.ready
    }

    pub fn lines(&self) -> String {
        let mut lines = vec!["F2 Configure master | F3 Cancel turn | F4 Close master".to_owned()];
        if self.project.is_none() {
            lines.push("Select a project before Configure master".into());
        }
        if self.is_open() {
            if let Some(intent) = &self.home_confirmation {
                registration_lines(intent, &mut lines);
            } else {
                self.setup_lines(&mut lines);
            }
        } else if let Some(view) = &self.view {
            view.lines(&mut lines);
        } else {
            lines.push("Waiting for actual master state; stored notes remain notes".into());
        }
        if let Some(pending) = &self.pending {
            lines.push(pending.line());
        }
        self.registration_lines(&mut lines);
        if self.is_open() || self.pending.is_some() || self.view.is_none() {
            lines.push(self.notice.clone());
        }
        if self.is_open()
            || self
                .view
                .as_ref()
                .is_some_and(|view| view.session.is_some())
        {
            lines.push(
                concat!(
                    "Configured/Queued are durable bootstrap states; ",
                    "native delivery/authentication remain unsupported"
                )
                .into(),
            );
        }
        lines.join("\n")
    }

    fn registration_lines(&self, lines: &mut Vec<String>) {
        let Some(registered) = self.registered_home.as_ref().filter(|registered| {
            self.project.as_ref() == Some(&registered.metadata.project_id)
                && self.identity.as_ref().is_some_and(|identity| {
                    identity.database_instance() == registered.identity.database_instance()
                })
        }) else {
            return;
        };
        let current = self.ready
            && self.identity.as_ref() == Some(&registered.identity)
            && self
                .view
                .as_ref()
                .and_then(|view| view.session.as_ref())
                .is_some_and(|session| {
                    session.session_id == registered.metadata.session_id
                        && session.phase == MasterSessionPhase::Configured
                });
        lines.push(format!(
            "{}Registered configured profile | session {} | sandbox {} | generation {}",
            if current && !registered.historical {
                ""
            } else {
                "Historical receipt: "
            },
            registered.metadata.session_id.as_str(),
            registered.metadata.profile_ref.as_str(),
            registered.metadata.generation
        ));
        lines.push(format!(
            "Registration {} | original owner operation {} | no sign-in observation",
            registered.metadata.registration_id.as_str(),
            registered.provenance.operation_id.as_str()
        ));
    }

    fn setup_lines(&self, lines: &mut Vec<String>) {
        self.forms[self.form_index().unwrap()].lines(lines);
        lines.push(
            "h Register SDK home | requires actual Open/read and selected connection home".into(),
        );
        if let Some(catalog) = &self.catalog {
            if catalog
                .models
                .pools
                .iter()
                .any(|pool| pool.projects.is_empty() || pool.roles.is_empty())
            {
                lines.push(
                    "Unassigned pools do not route; Settings > Pools > a Assign to project".into(),
                );
            }
            lines.push(format!(
                "Owner catalog revision {} | config {} | source {}",
                catalog.revision.value(),
                catalog.config_id.as_str(),
                catalog.source_hash
            ));
        }
        lines.push(match self.selection() {
            Ok(_) if !self.has_pending() => concat!(
                "Enter Open | Tab/Shift-Tab fields | arrows choices | ",
                "Esc retains setup and conversation draft"
            )
            .into(),
            Ok(_) => "Open disabled: owner intent unresolved; fields retained".into(),
            Err(reason) => reason,
        });
        lines.push(
            "Settings: Esc, Tab twice | execution sandbox is separate from credential profile"
                .into(),
        );
    }

    fn assert_consistent(&self) {
        assert!(self.forms.len() <= 128);
        assert!(self.generation > 0);
        for form in &self.forms {
            assert!(form.field < 11 && form.generation > 0);
            assert!(
                form.values
                    .iter()
                    .all(|value| value.len() <= 13
                        && value.bytes().all(|byte| byte.is_ascii_digit()))
            );
        }
        if let Some(pending) = &self.pending {
            assert!(pending.submission.generation > 0);
            assert!(pending.receipt.is_none() || pending.operation.is_some());
            assert!(pending.registration.is_none() || pending.receipt.is_some());
        }
    }
}

impl View {
    fn lines(&self, lines: &mut Vec<String>) {
        if let Some(session) = &self.session {
            lines.push(format!(
                "Master: {:?} | {} | {} | pool {}",
                session.phase,
                session.session_id.as_str(),
                backend_label(session.selection.backend),
                session.selection.pool_ref.as_str()
            ));
            for turn in &self.turns {
                turn_lines(turn, lines);
            }
        } else {
            lines.push(
                "No master configured; Enter stores an owner note until explicit Open".into(),
            );
        }
    }

    fn confirms(
        &self,
        action: &MasterOwnerAction,
        receipt: &OwnerProvenance,
    ) -> Result<bool, String> {
        let session = self
            .session
            .as_ref()
            .ok_or("Applied master session missing from actual read")?;
        Ok(match action {
            MasterOwnerAction::Open {
                selection,
                expected_config_id,
                expected_source_hash,
                ..
            } => {
                session.selection == **selection
                    && session.config_id == *expected_config_id
                    && session.source_hash == *expected_source_hash
                    && session.phase == MasterSessionPhase::Configured
                    && session.owner == *receipt
            }
            MasterOwnerAction::Submit {
                session_id, text, ..
            } => {
                session.session_id == *session_id
                    && self.turns.iter().any(|turn| {
                        turn.id == receipt.operation_id
                            && turn.session_id == *session_id
                            && receipt.event_cursor.as_ref() == Some(&turn.input_cursor)
                    })
                    && self.input.as_ref().is_some_and(|input| {
                        input.text == *text
                            && input.content == ConversationContent::Message
                            && input.provenance == *receipt
                            && receipt.event_cursor.as_ref() == Some(&input.event_cursor)
                    })
            }
            MasterOwnerAction::Cancel {
                session_id,
                turn_id,
                ..
            } => {
                session.session_id == *session_id
                    && session.current_turn.is_none()
                    && self
                        .turns
                        .iter()
                        .any(|turn| turn.id == *turn_id && turn.phase == MasterTurnPhase::Cancelled)
            }
            MasterOwnerAction::Close { session_id, .. } => {
                session.session_id == *session_id
                    && session.phase == MasterSessionPhase::Closed
                    && session.current_turn.is_none()
            }
            MasterOwnerAction::Resolve {
                session_id,
                change_set,
                ..
            } => {
                session.session_id == *session_id
                    && self.turns.iter().any(|turn| {
                        turn.id == receipt.operation_id
                            && turn
                                .engine_request
                                .as_ref()
                                .is_some_and(|request| request.change_set == *change_set)
                    })
            }
            MasterOwnerAction::RegisterRcliProfile { registration } => {
                session == &registration.expected_session
                    && session.phase == MasterSessionPhase::Configured
                    && session.current_turn.is_none()
            }
        })
    }
}

impl Pending {
    fn line(&self) -> String {
        match &self.operation {
            Some((id, _)) => format!(
                concat!(
                    "Owner operation {} unresolved; ",
                    "Ctrl-R reconciles or retries after an actual barrier"
                ),
                id.as_str()
            ),
            None => "Master owner intent queued; fields and newer text retained".into(),
        }
    }
}

impl Form {
    fn lines(&self, lines: &mut Vec<String>) {
        let choices = [
            backend_label(self.backend).to_owned(),
            self.pool
                .as_ref()
                .map_or("Unavailable", CredentialRef::as_str)
                .to_owned(),
            self.account
                .as_ref()
                .map_or("Unavailable", CredentialRef::as_str)
                .to_owned(),
            self.target
                .as_ref()
                .map_or("Unavailable", CredentialRef::as_str)
                .to_owned(),
        ];
        for (index, label) in ["Backend", "Pool", "Account", "Execution sandbox"]
            .iter()
            .enumerate()
        {
            lines.push(format!(
                "{} {label}: {}",
                if self.field == index { ">" } else { " " },
                choices[index]
            ));
        }
        for (index, name) in DIMENSIONS.iter().enumerate() {
            lines.push(format!(
                "{} {name}: {}",
                if self.field == index + 4 { ">" } else { " " },
                self.values[index]
            ));
        }
    }

    fn select(&mut self, catalog: Option<&Catalog>, forward: bool) -> Result<bool, String> {
        match self.field {
            0 => self.cycle_backend(catalog, forward),
            1 => {
                if let Some(catalog) = catalog {
                    let choices: Vec<_> = catalog
                        .pools(self)
                        .iter()
                        .map(|pool| pool.id.clone())
                        .collect();
                    let selected = cycle(&choices, self.pool.as_ref(), forward);
                    if self.pool != selected {
                        self.account = None;
                        self.target = None;
                    }
                    self.pool = selected;
                }
            }
            2 => {
                if let Some(catalog) = catalog {
                    self.account = cycle(&catalog.accounts(self), self.account.as_ref(), forward);
                    self.target = None;
                }
            }
            3 => {
                if let Some(catalog) = catalog {
                    self.target =
                        cycle(&catalog.target_choices(self), self.target.as_ref(), forward);
                }
            }
            _ => return Ok(false),
        }
        self.advance_generation()?;
        Ok(true)
    }

    fn cycle_backend(&mut self, catalog: Option<&Catalog>, forward: bool) {
        let backends = [
            MasterBackend::Rcli,
            MasterBackend::Codex,
            MasterBackend::Claude,
        ];
        let current = backends
            .iter()
            .position(|backend| backend == &self.backend)
            .unwrap();
        self.backend = backends[(current + if forward { 1 } else { 2 }) % 3];
        self.account = None;
        self.target = None;
        if catalog.is_some_and(|catalog| {
            !catalog
                .pools(self)
                .iter()
                .any(|pool| Some(&pool.id) == self.pool.as_ref())
        }) {
            self.pool = None;
        }
    }

    fn edit_allowance(&mut self, character: Option<char>) -> Result<(), String> {
        if self.field < 4 {
            return Ok(());
        }
        let value = &mut self.values[self.field - 4];
        if let Some(character) = character {
            if !character.is_ascii_digit() || value.len() >= 13 {
                return Err(
                    "Allowance uses at most 13 decimal digits; literal value retained".into(),
                );
            }
            value.push(character);
        } else if value.pop().is_none() {
            return Ok(());
        }
        self.edited = true;
        self.advance_generation()
    }

    fn advance_generation(&mut self) -> Result<(), String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Master form generation exhausted")?;
        Ok(())
    }

    fn resources(&self) -> Result<ResourceBudget, String> {
        let mut values = [0; 7];
        for (index, value) in self.values.iter().enumerate() {
            values[index] = value.parse().map_err(|_| {
                format!(
                    "Open disabled: {} requires a decimal allowance; value retained",
                    DIMENSIONS[index]
                )
            })?;
        }
        let resources = ResourceBudget {
            slots: values[0],
            cpu_millis: values[1],
            memory_mib: values[2],
            processes: values[3],
            output_bytes: values[4],
            wall_time_ms: values[5],
            spend_microunits: values[6],
        };
        resources.validate()?;
        Ok(resources)
    }
}

impl Catalog {
    fn rcli_connection(&self, session: &MasterSessionView) -> Result<&ModelConnection, String> {
        let pool = self
            .models
            .pools
            .iter()
            .find(|pool| pool.id == session.selection.pool_ref)
            .ok_or("actual session pool missing from current owner catalog")?;
        let mut connections = self.models.connections.iter().filter(|connection| {
            pool.members.contains(&connection.id)
                && connection.account_ref == session.selection.route.account_ref
                && rcli_home_supported(connection)
        });
        let connection = connections.next().ok_or("RCLI connection unavailable")?;
        if connections.next().is_some() {
            return Err(
                "RCLI connection is ambiguous; inspect the selected pool in Settings".into(),
            );
        }
        Ok(connection)
    }

    fn pools(&self, form: &Form) -> Vec<&AccountPool> {
        self.models
            .pools
            .iter()
            .filter(|pool| {
                pool.projects
                    .iter()
                    .any(|project| project.as_str() == form.project.as_str())
                    && pool
                        .roles
                        .iter()
                        .any(|role| role.as_str() == "project_leader")
                    && self.models.connections.iter().any(|connection| {
                        pool.members.contains(&connection.id)
                            && self.approved.contains(&connection.account_ref)
                            && self.models.accounts.contains(&connection.account_ref)
                            && backend_matches(form.backend, connection)
                    })
            })
            .collect()
    }

    /// Names the first backend, in RCLI, Codex, Claude order, with a pool assigned to the
    /// form's project as master. Input: the form, whose backend is ignored. Output: that
    /// backend, or None when no pool is assigned to the project.
    fn assigned_backend(&self, form: &Form) -> Option<MasterBackend> {
        [
            MasterBackend::Rcli,
            MasterBackend::Codex,
            MasterBackend::Claude,
        ]
        .into_iter()
        .find(|backend| {
            let probe = Form {
                database: None,
                project: form.project.clone(),
                backend: *backend,
                pool: None,
                account: None,
                target: None,
                values: form.values.clone(),
                field: 0,
                generation: form.generation,
                open: false,
                explicit: false,
                edited: form.edited,
            };
            !self.pools(&probe).is_empty()
        })
    }

    /// Picks the execution target a new master of this form would use under DEC-076.
    /// Input: the form's backend, pool and account. Output: the first of its target
    /// choices; None when policy allows no such target.
    fn target(&self, form: &Form) -> Option<CredentialRef> {
        self.target_choices(form).into_iter().next()
    }

    /// Lists the execution targets a master of this form may use under DEC-076.
    /// Input: the form's backend, pool and account. Output: for a signed-in backend, the
    /// allowed connections of that account in the pool; for RCLI, the account's own allowed
    /// profiles first, then every allowed target that is not a signed-in connection.
    fn target_choices(&self, form: &Form) -> Vec<CredentialRef> {
        let pool = self
            .models
            .pools
            .iter()
            .find(|pool| Some(&pool.id) == form.pool.as_ref());
        let members = self.models.connections.iter().filter(|connection| {
            pool.is_some_and(|pool| pool.members.contains(&connection.id))
                && Some(&connection.account_ref) == form.account.as_ref()
                && backend_matches(form.backend, connection)
        });
        if form.backend != MasterBackend::Rcli {
            return members
                .filter(|connection| self.targets.contains(&connection.id))
                .map(|connection| connection.id.clone())
                .collect();
        }
        let mut choices: Vec<CredentialRef> = members
            .filter_map(crate::config::configured_profile_ref)
            .filter(|profile| self.targets.contains(profile))
            .collect();
        for target in &self.targets {
            let native = self.models.connections.iter().any(|connection| {
                &connection.id == target && matches!(connection.endpoint, ModelEndpoint::Native {})
            });
            if !native && !choices.contains(target) {
                choices.push(target.clone());
            }
        }
        assert!(choices.len() <= self.targets.len() + self.models.connections.len());
        choices
    }

    fn accounts(&self, form: &Form) -> Vec<CredentialRef> {
        let Some(pool) = self
            .models
            .pools
            .iter()
            .find(|pool| Some(&pool.id) == form.pool.as_ref())
        else {
            return Vec::new();
        };
        let mut accounts = Vec::new();
        for connection in &self.models.connections {
            if pool.members.contains(&connection.id)
                && self.approved.contains(&connection.account_ref)
                && self.models.accounts.contains(&connection.account_ref)
                && backend_matches(form.backend, connection)
                && !accounts.contains(&connection.account_ref)
            {
                accounts.push(connection.account_ref.clone());
            }
        }
        accounts
    }
}

/// Tells whether the session's current turn still holds it, so a new message must wait.
/// Inputs: the current turn id and the turns read with it. Output: false with no current
/// turn or when it ended uncertain, which the engine lets the next message supersede.
pub(crate) fn turn_holds(current: Option<&RequestId>, turns: &[MasterTurnView]) -> bool {
    current.is_some_and(|id| {
        turns
            .iter()
            .find(|turn| &turn.id == id)
            .is_none_or(|turn| turn.phase != MasterTurnPhase::Uncertain)
    })
}

fn turn_lines(turn: &MasterTurnView, lines: &mut Vec<String>) {
    let phase = match turn.phase {
        MasterTurnPhase::Queued => "Queued",
        MasterTurnPhase::Blocked => "Blocked",
        MasterTurnPhase::Prepared | MasterTurnPhase::Delivering => "Starting",
        MasterTurnPhase::Running => "Running",
        MasterTurnPhase::WaitingApproval => "Waiting for approval",
        MasterTurnPhase::Succeeded => "Replied",
        MasterTurnPhase::Failed => "Failed",
        MasterTurnPhase::Cancelled => "Cancelled",
        MasterTurnPhase::Uncertain => "Outcome unclear",
    };
    let mut line = match &turn.reason {
        Some(_) if !turn.accounts.is_empty() => format!(
            "Turn {}: {phase} | No account in this master's pool can run it now:",
            turn.ordinal
        ),
        Some(reason) => format!("Turn {}: {phase} | {}", turn.ordinal, reason_text(reason)),
        None => format!("Turn {}: {phase}", turn.ordinal),
    };
    if let Some(cause) = &turn.cause {
        line.push_str(&format!(" ({cause})"));
    }
    if let Some(attempt) = &turn.attempt {
        line.push_str(" | on account ");
        line.push_str(attempt.connection.account_ref.as_str());
    }
    lines.push(line);
    for account in &turn.accounts {
        lines.push(match &account.reason {
            Some(reason) => format!(
                "  {}: {}",
                account.account_ref.as_str(),
                reason_text(reason)
            ),
            None => format!(
                "  {} is ready on {}: open a {} master to use it",
                account.account_ref.as_str(),
                account.harness.as_str(),
                account.harness.as_str()
            ),
        });
    }
    if let Some(detail) = &turn.detail {
        lines.push(super::views::safe_text(detail, 8192));
    }
    if let Some(reply) = &turn.reply {
        lines.push(format!(
            "Actual reply | attempt {} | runtime {} | capture {} sequence {}\n{}",
            reply.provenance.attempt_id.as_str(),
            reply.provenance.runtime_ref.as_str(),
            reply.provenance.capture_ref.as_str(),
            reply.provenance.capture_sequence,
            super::views::safe_text(&reply.text, crate::master_session::REPLY_BYTES_MAX)
        ));
    }
}

pub(super) fn action_project(action: &MasterOwnerAction) -> &ProjectId {
    match action {
        MasterOwnerAction::Open { project_id, .. }
        | MasterOwnerAction::Submit { project_id, .. }
        | MasterOwnerAction::Cancel { project_id, .. }
        | MasterOwnerAction::Close { project_id, .. }
        | MasterOwnerAction::Resolve { project_id, .. } => project_id,
        MasterOwnerAction::RegisterRcliProfile { registration } => {
            &registration.expected_session.project_id
        }
    }
}

fn registration_lines(intent: &RcliProfileRegistration, lines: &mut Vec<String>) {
    let session = &intent.expected_session;
    lines.push("Register configured SDK home".into());
    lines.push(format!("SDK home: {}", intent.configured_home.display()));
    lines.push(format!(
        "Project {} | session {}",
        session.project_id.as_str(),
        session.session_id.as_str()
    ));
    lines.push(format!(
        "Connection {} | account {} | pool {}",
        intent.expected_connection.id.as_str(),
        intent.expected_connection.account_ref.as_str(),
        session.selection.pool_ref.as_str()
    ));
    lines.push(format!(
        "Execution sandbox {} | credential profile is separate",
        session.selection.target_profile_ref.as_str()
    ));
    lines.push("Enter confirms registration only | Esc retains conversation draft".into());
    lines.push("No login, prompt or authentication observation is performed".into());
}

fn backend_label(backend: MasterBackend) -> &'static str {
    match backend {
        MasterBackend::Rcli => "RCLI",
        MasterBackend::Codex => "Native Codex",
        MasterBackend::Claude => "Native Claude",
    }
}

fn backend_matches(backend: MasterBackend, connection: &ModelConnection) -> bool {
    match backend {
        MasterBackend::Rcli => {
            connection.harness.as_str() == "rcli"
                && matches!(connection.endpoint, ModelEndpoint::Http { .. })
        }
        MasterBackend::Codex => {
            connection.harness.as_str() == "codex"
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
        MasterBackend::Claude => {
            matches!(connection.harness.as_str(), "claude" | "claude-code")
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
    }
}

fn budget_values(budget: &ResourceBudget) -> [u64; 7] {
    [
        budget.slots,
        budget.cpu_millis,
        budget.memory_mib,
        budget.processes,
        budget.output_bytes,
        budget.wall_time_ms,
        budget.spend_microunits,
    ]
}

fn cycle(
    choices: &[CredentialRef],
    current: Option<&CredentialRef>,
    forward: bool,
) -> Option<CredentialRef> {
    if choices.is_empty() {
        return None;
    }
    let index = current.and_then(|current| choices.iter().position(|choice| choice == current));
    Some(
        choices[index.map_or(0, |index| {
            (index + if forward { 1 } else { choices.len() - 1 }) % choices.len()
        })]
        .clone(),
    )
}

fn same_basis(reply: &OwnerReadReply, revision: &Cursor, config: &ConfigId) -> Result<(), String> {
    if &reply.revision != revision
        || &reply.config_id != config
        || matches!(reply.data, OwnerData::ResyncRequired)
    {
        return Err("master_snapshot_changed; refresh actual metadata/state".into());
    }
    Ok(())
}

pub(super) async fn read_catalog(controller: &mut OwnerController) -> Result<Catalog, String> {
    let mut models = ModelAccess::default();
    let mut basis = None;
    for collection in [
        ModelAccessCollection::Accounts,
        ModelAccessCollection::Connections,
        ModelAccessCollection::Pools,
    ] {
        read_collection(controller, collection, &mut models, &mut basis).await?;
    }
    let (revision, config_id, source_hash) = basis.ok_or("master_catalog_basis_missing")?;
    let values = read_policy(controller, &revision, &config_id).await?;
    catalog_from_policy(models, revision, config_id, source_hash, values)
}

async fn read_collection(
    controller: &mut OwnerController,
    collection: ModelAccessCollection,
    models: &mut ModelAccess,
    basis: &mut Option<(Cursor, ConfigId, String)>,
) -> Result<(), String> {
    let mut cursor = None;
    for _ in 0..=crate::config::ACCOUNTS_MAX {
        let state = controller
            .read(OwnerRead::ModelAccessPage {
                collection,
                cursor: cursor.clone(),
                limit: 16,
            })
            .await
            .map_err(|error| error.to_string())?;
        let next = catalog_page(state, collection, models, basis)?;
        if let Some(next) = &next {
            catalog_cursor(
                controller.identity(),
                collection,
                basis.as_ref().unwrap(),
                cursor.as_ref(),
                next,
            )?;
        } else {
            return Ok(());
        }
        cursor = next;
    }
    Err("master_catalog_incomplete; inspect Settings".into())
}

fn catalog_page(
    state: OwnerReadReply,
    collection: ModelAccessCollection,
    models: &mut ModelAccess,
    basis: &mut Option<(Cursor, ConfigId, String)>,
) -> Result<Option<ModelAccessCursor>, String> {
    if let Some((revision, config, _)) = basis.as_ref() {
        same_basis(&state, revision, config)?;
    }
    let OwnerData::ModelAccessPage {
        collection: actual,
        source_hash,
        items,
        next,
        ..
    } = state.data
    else {
        return Err("master_catalog_page_required".into());
    };
    if actual != collection || items.len() > 16 {
        return Err("master_catalog_page_context".into());
    }
    if let Some((_, _, hash)) = basis.as_ref() {
        if hash != &source_hash {
            return Err("master_catalog_source_changed".into());
        }
    } else {
        *basis = Some((state.revision, state.config_id, source_hash));
    }
    catalog_records(models, collection, items)?;
    Ok(next)
}

fn catalog_records(
    models: &mut ModelAccess,
    collection: ModelAccessCollection,
    items: Vec<ModelAccessRecord>,
) -> Result<(), String> {
    for item in items {
        match (collection, item) {
            (ModelAccessCollection::Accounts, ModelAccessRecord::Account { account_ref }) => {
                models.accounts.push(account_ref);
            }
            (ModelAccessCollection::Connections, ModelAccessRecord::Connection { connection }) => {
                models.connections.push(connection);
            }
            (ModelAccessCollection::Pools, ModelAccessRecord::Pool { pool }) => {
                models.pools.push(pool);
            }
            _ => return Err("master_catalog_record_kind".into()),
        }
    }
    if models.accounts.len() > crate::config::ACCOUNTS_MAX
        || models.connections.len() > crate::config::ACCOUNTS_MAX
        || models.pools.len() > 64
    {
        return Err("master_catalog_capacity".into());
    }
    Ok(())
}

fn catalog_cursor(
    identity: &OwnerConnectionIdentity,
    collection: ModelAccessCollection,
    basis: &(Cursor, ConfigId, String),
    previous: Option<&ModelAccessCursor>,
    next: &ModelAccessCursor,
) -> Result<(), String> {
    if next.db_instance != identity.database_instance()
        || next.engine_epoch != identity.engine_epoch()
        || next.collection != collection
        || next.at_revision != basis.0
        || next.config_id != basis.1
        || next.source_hash != basis.2
        || previous.is_some_and(|previous| next.after_id.as_str() <= previous.after_id.as_str())
    {
        return Err("master_catalog_cursor_context".into());
    }
    Ok(())
}

async fn read_policy(
    controller: &mut OwnerController,
    revision: &Cursor,
    config: &ConfigId,
) -> Result<Vec<serde_json::Value>, String> {
    let mut values = Vec::new();
    for name in [
        "allowed_destinations",
        "revoked_destinations",
        "routing.approved_accounts",
        "routing.global_capacity",
        "routing.project_capacity",
        "routing.task_capacity",
    ] {
        let reply = controller
            .read(OwnerRead::Setting {
                name: name.to_owned(),
            })
            .await
            .map_err(|error| error.to_string())?;
        same_basis(&reply, revision, config)?;
        let OwnerData::Setting { descriptor } = reply.data else {
            return Err("master_policy_descriptor_required".into());
        };
        if descriptor.name != name {
            return Err("master_policy_descriptor_context".into());
        }
        values.push(descriptor.value);
    }
    Ok(values)
}

fn catalog_from_policy(
    models: ModelAccess,
    revision: Cursor,
    config_id: ConfigId,
    source_hash: String,
    values: Vec<serde_json::Value>,
) -> Result<Catalog, String> {
    let allowed: Vec<CredentialRef> =
        serde_json::from_value(values[0].clone()).map_err(|_| "master_allowed_targets_shape")?;
    let revoked: Vec<CredentialRef> =
        serde_json::from_value(values[1].clone()).map_err(|_| "master_revoked_targets_shape")?;
    let approved =
        serde_json::from_value(values[2].clone()).map_err(|_| "master_approved_accounts_shape")?;
    let capacities: Vec<ResourceBudget> = values[3..]
        .iter()
        .map(|value| serde_json::from_value(value.clone()))
        .collect::<Result<_, _>>()
        .map_err(|_| "master_capacity_shape")?;
    let capacities: [ResourceBudget; 3] =
        capacities.try_into().map_err(|_| "master_capacity_count")?;
    for capacity in &capacities {
        capacity.validate()?;
    }
    models.validate().map_err(|error| error.to_string())?;
    Ok(Catalog {
        config_id,
        source_hash,
        revision,
        models,
        approved,
        targets: allowed
            .into_iter()
            .filter(|target| !revoked.contains(target))
            .collect(),
        capacities,
    })
}

pub(super) async fn read_view(
    controller: &mut OwnerController,
    context: &Context,
    confirmation: Option<&Confirmation>,
) -> Result<View, String> {
    let state = controller
        .read(OwnerRead::ProjectMaster {
            project_id: context.project.clone(),
        })
        .await
        .map_err(|error| error.to_string())?;
    let OwnerData::ProjectMaster { session } = state.data else {
        return Err("master_session_read_required".into());
    };
    let session = session.map(|session| *session);
    let mut view = View {
        session,
        turns: Vec::new(),
        revision: state.revision,
        input: None,
    };
    if let Some(session) = &view.session {
        if session.project_id != context.project
            || session.version.value() > view.revision.value()
            || session.owner.event_cursor.is_none()
        {
            return Err("master_session_read_context".into());
        }
        view.turns = read_turns(
            controller,
            context,
            session,
            &view.revision,
            &state.config_id,
        )
        .await?;
    }
    if let Some(confirmation) = confirmation
        && let MasterOwnerAction::Submit { .. } = confirmation.submission.action
    {
        view.input = read_input(
            controller,
            context,
            &view.revision,
            &state.config_id,
            &confirmation.receipt,
        )
        .await?;
    }
    Ok(view)
}

async fn read_turns(
    controller: &mut OwnerController,
    context: &Context,
    session: &MasterSessionView,
    revision: &Cursor,
    config: &ConfigId,
) -> Result<Vec<MasterTurnView>, String> {
    let mut turns = Vec::new();
    let mut cursor = None;
    for _ in 0..1024 {
        let reply = controller
            .read(OwnerRead::ProjectMasterTurns {
                project_id: context.project.clone(),
                session_id: session.session_id.clone(),
                cursor: cursor.clone(),
                limit: 32,
            })
            .await
            .map_err(|error| error.to_string())?;
        same_basis(&reply, revision, config)?;
        let OwnerData::ProjectMasterTurns { page } = reply.data else {
            return Err("master_turn_page_required".into());
        };
        if page.project_id != context.project
            || page.session_id != session.session_id
            || page.turns.len() > 32
        {
            return Err("master_turn_page_context".into());
        }
        let after = cursor
            .as_ref()
            .map_or(0, |cursor: &MasterCursor| cursor.after_ordinal);
        let after = append_turns(&mut turns, page.turns, session, revision, after)?;
        if let Some(next) = &page.next {
            turn_cursor(
                context,
                session,
                revision,
                config,
                cursor.as_ref(),
                after,
                next,
            )?;
        } else {
            return Ok(turns);
        }
        cursor = page.next;
    }
    Err("master_turn_pages_incomplete".into())
}

fn append_turns(
    turns: &mut Vec<MasterTurnView>,
    items: Vec<MasterTurnView>,
    session: &MasterSessionView,
    revision: &Cursor,
    mut after: u64,
) -> Result<u64, String> {
    for turn in items {
        if turn.session_id != session.session_id
            || turn.ordinal <= after
            || turn.input_cursor.value() > revision.value()
        {
            return Err("master_turn_context".into());
        }
        after = turn.ordinal;
        turns.push(turn);
        if turns.len() > 32 {
            turns.remove(0);
        }
    }
    Ok(after)
}

fn turn_cursor(
    context: &Context,
    session: &MasterSessionView,
    revision: &Cursor,
    config: &ConfigId,
    previous: Option<&MasterCursor>,
    after: u64,
    next: &MasterCursor,
) -> Result<(), String> {
    if next.project_id != context.project
        || next.session_id != session.session_id
        || next.version != *revision
        || next.config_id != *config
        || next.db_instance != context.identity.database_instance()
        || next.engine_epoch != context.identity.engine_epoch()
        || next.after_ordinal != after
        || previous.is_some_and(|previous| next.after_ordinal <= previous.after_ordinal)
    {
        return Err("master_turn_cursor_context".into());
    }
    Ok(())
}

async fn read_input(
    controller: &mut OwnerController,
    context: &Context,
    revision: &Cursor,
    config: &ConfigId,
    receipt: &OwnerProvenance,
) -> Result<Option<ConversationEntry>, String> {
    let mut cursor = None;
    for _ in 0..crate::conversation::ENTRIES_MAX {
        let reply = controller
            .read(OwnerRead::ConversationHistory {
                project_id: context.project.clone(),
                cursor: cursor.clone(),
                limit: 32,
            })
            .await
            .map_err(|error| error.to_string())?;
        same_basis(&reply, revision, config)?;
        let OwnerData::ConversationHistory { page } = reply.data else {
            return Err("master_owner_input_read_required".into());
        };
        if page.project_id != context.project || page.entries.len() > 32 {
            return Err("master_owner_input_page_context".into());
        }
        let input = page
            .entries
            .into_iter()
            .find(|entry| receipt.event_cursor.as_ref() == Some(&entry.event_cursor));
        if input.is_some() || page.next.is_none() {
            return Ok(input);
        }
        if let Some(next) = &page.next {
            input_cursor(context, cursor.as_ref(), next)?;
        }
        cursor = page.next;
    }
    Ok(None)
}

fn input_cursor(
    context: &Context,
    previous: Option<&ConversationCursor>,
    next: &ConversationCursor,
) -> Result<(), String> {
    if next.project_id != context.project
        || next.db_instance != context.identity.database_instance()
        || next.engine_epoch != context.identity.engine_epoch()
        || previous.is_some_and(|previous| next.after.value() <= previous.after.value())
    {
        return Err("master_owner_input_cursor_context".into());
    }
    Ok(())
}

/// Wall time a default master turn asks for: ten minutes, before capacity clipping.
const MASTER_TURN_WALL_MS: u64 = 600_000;
/// The harness output one master turn may read in all. Codex keeps about 1 MiB of a
/// command's output and sends it twice, as deltas and in the finished item, so 64 MiB, the
/// default capacity, holds a turn of about thirty such commands.
const MASTER_TURN_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Default allowance for one master turn: the unit budget with ten minutes of wall time and
/// 64 MiB of harness output, each clipped to the tightest configured capacity so a slow local
/// model or a turn of large command results can finish a reply.
/// Input: the global, project and task capacities. Output: a budget that fits all three.
fn default_master_budget(capacities: &[ResourceBudget; 3]) -> ResourceBudget {
    let ceiling =
        |dimension: fn(&ResourceBudget) -> u64| capacities.iter().map(dimension).min().unwrap_or(0);
    let budget = ResourceBudget {
        wall_time_ms: MASTER_TURN_WALL_MS
            .min(ceiling(|capacity| capacity.wall_time_ms))
            .max(1),
        output_bytes: MASTER_TURN_OUTPUT_BYTES
            .min(ceiling(|capacity| capacity.output_bytes))
            .max(1),
        ..ResourceBudget::unit()
    };
    assert!(budget.wall_time_ms <= MASTER_TURN_WALL_MS);
    assert!(budget.output_bytes <= MASTER_TURN_OUTPUT_BYTES);
    budget
}

/// Builds the master Open action the TUI submits with its default choices for a project.
/// Inputs: a connected owner controller and the project. Output: an Open action using the
/// first assigned, approved project_leader pool (RCLI, then Codex, then Claude), its first
/// account as the one tried first (the engine runs each turn on a ready account of that
/// pool), the first allowed sandbox and the unit allowance; or a plain-language reason.
pub async fn default_master_open(
    controller: &mut OwnerController,
    project_id: &ProjectId,
) -> Result<MasterOwnerAction, String> {
    let catalog = read_catalog(controller).await?;
    let budget = default_master_budget(&catalog.capacities);
    for backend in [
        MasterBackend::Rcli,
        MasterBackend::Codex,
        MasterBackend::Claude,
    ] {
        let mut form = Form {
            database: None,
            project: project_id.clone(),
            backend,
            pool: None,
            account: None,
            target: None,
            values: budget_values(&budget).map(|value| value.to_string()),
            field: 0,
            generation: 1,
            open: false,
            explicit: false,
            edited: false,
        };
        form.pool = catalog.pools(&form).first().map(|pool| pool.id.clone());
        let Some(pool) = form.pool.clone() else {
            continue;
        };
        let account = catalog
            .accounts(&form)
            .first()
            .cloned()
            .ok_or("No approved account in the assigned pool")?;
        form.account = Some(account.clone());
        let target = catalog
            .target(&form)
            .ok_or("No allowed execution sandbox in settings")?;
        if catalog
            .capacities
            .iter()
            .any(|capacity| !budget.fits(capacity))
        {
            return Err("The default allowance exceeds current owner policy".into());
        }
        return Ok(MasterOwnerAction::Open {
            project_id: project_id.clone(),
            expected_config_id: catalog.config_id.clone(),
            expected_source_hash: catalog.source_hash.clone(),
            selection: Box::new(MasterSelection {
                backend,
                pool_ref: pool.clone(),
                target_profile_ref: target.clone(),
                route: RouteRequest {
                    preferred_target: Some(target.clone()),
                    required_target: Some(target),
                    required_capabilities: Vec::new(),
                    account_ref: account,
                    budget_ref: pool,
                    resources: budget,
                },
            }),
        });
    }
    Err("No account pool is assigned to this project as its master".into())
}

#[cfg(test)]
mod default_budget_tests {
    use super::*;

    #[test]
    fn an_unedited_allowance_follows_the_owner_policy_once_it_is_read() {
        let capacity = |wall_time_ms| ResourceBudget {
            slots: 2,
            cpu_millis: 2000,
            memory_mib: 1024,
            processes: 8,
            output_bytes: 2_097_152,
            wall_time_ms,
            spend_microunits: 10,
        };
        let catalog = |wall_time_ms| Catalog {
            config_id: ConfigId::new("0".repeat(64)).unwrap(),
            source_hash: "0".repeat(64),
            revision: Cursor::new(1),
            models: ModelAccess::default(),
            approved: Vec::new(),
            targets: Vec::new(),
            capacities: [
                capacity(wall_time_ms),
                capacity(wall_time_ms),
                capacity(wall_time_ms),
            ],
        };
        let mut state = State::default();
        state
            .select(None, Some(ProjectId::new("demo").unwrap()), true)
            .unwrap();
        state.open().unwrap();
        let index = state.form_index().unwrap();
        state.catalog = Some(catalog(120_000));
        state.defaults();
        let mut expected = ResourceBudget::unit();
        expected.wall_time_ms = 120_000;
        expected.output_bytes = 2_097_152;
        assert_eq!(
            state.forms[index].values,
            budget_values(&expected).map(|value| value.to_string()),
            "the default allowance fits the owner's actual capacity"
        );

        state.forms[index].field = 4 + 5;
        state.forms[index].edit_allowance(None).unwrap();
        state.forms[index].edit_allowance(Some('9')).unwrap();
        assert_eq!(state.forms[index].values[5], "120009");
        state.catalog = Some(catalog(90_000));
        state.defaults();
        assert_eq!(
            state.forms[index].values[5], "120009",
            "a value the owner typed is kept for them to fix"
        );
    }

    #[test]
    fn signed_in_master_targets_its_own_connection_and_rcli_keeps_the_sandbox() {
        let reference = |value: &str| CredentialRef::new(value).unwrap();
        let models: ModelAccess = toml::from_str(r#"accounts = ["codex-account", "local-account"]
connections = [
  { id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "codex-pool", members = ["codex-connection"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false },
  { id = "local-pool", members = ["local-connection"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false },
]"#).unwrap();
        let catalog = Catalog {
            config_id: ConfigId::new("0".repeat(64)).unwrap(),
            source_hash: "0".repeat(64),
            revision: Cursor::new(1),
            models,
            approved: vec![reference("codex-account"), reference("local-account")],
            targets: vec![reference("codex-connection"), reference("local")],
            capacities: [
                ResourceBudget::unit(),
                ResourceBudget::unit(),
                ResourceBudget::unit(),
            ],
        };
        let mut form = Form {
            database: None,
            project: ProjectId::new("demo").unwrap(),
            backend: MasterBackend::Codex,
            pool: Some(reference("codex-pool")),
            account: Some(reference("codex-account")),
            target: None,
            values: budget_values(&ResourceBudget::unit()).map(|value| value.to_string()),
            field: 0,
            generation: 1,
            open: false,
            explicit: false,
            edited: false,
        };
        assert_eq!(catalog.target(&form), Some(reference("codex-connection")));
        assert_eq!(
            catalog.target_choices(&form),
            vec![reference("codex-connection")]
        );
        form.backend = MasterBackend::Rcli;
        form.pool = Some(reference("local-pool"));
        form.account = Some(reference("local-account"));
        assert_eq!(catalog.target(&form), Some(reference("local")));
        assert_eq!(catalog.target_choices(&form), vec![reference("local")]);
        form.target = Some(reference("local"));
        form.field = 3;
        assert!(form.select(Some(&catalog), true).unwrap());
        assert_eq!(
            form.target,
            Some(reference("local")),
            "an RCLI master never cycles onto a signed-in connection"
        );
    }

    #[test]
    fn each_rcli_account_targets_its_own_approved_profile() {
        let reference = |value: &str| CredentialRef::new(value).unwrap();
        let models: ModelAccess = toml::from_str(r#"accounts = ["key-account", "local-account"]
connections = [
  { id = "key-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "key-account", endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }, model = { id = "m", author = "openai-compatible" }, auth = { method = "api_key", credential_ref = "key-ref" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "key-pool", members = ["key-connection"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false },
  { id = "local-pool", members = ["local-connection"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false },
]"#).unwrap();
        let local_profile = crate::config::configured_profile_ref(&models.connections[1]).unwrap();
        let catalog = Catalog {
            config_id: ConfigId::new("0".repeat(64)).unwrap(),
            source_hash: "0".repeat(64),
            revision: Cursor::new(1),
            models,
            approved: vec![reference("key-account"), reference("local-account")],
            targets: vec![
                reference("local"),
                reference("key-ref"),
                local_profile.clone(),
            ],
            capacities: [
                ResourceBudget::unit(),
                ResourceBudget::unit(),
                ResourceBudget::unit(),
            ],
        };
        let mut form = Form {
            database: None,
            project: ProjectId::new("demo").unwrap(),
            backend: MasterBackend::Rcli,
            pool: Some(reference("key-pool")),
            account: Some(reference("key-account")),
            target: None,
            values: budget_values(&ResourceBudget::unit()).map(|value| value.to_string()),
            field: 0,
            generation: 1,
            open: false,
            explicit: false,
            edited: false,
        };
        assert_eq!(catalog.target(&form), Some(reference("key-ref")));
        form.pool = Some(reference("local-pool"));
        form.account = Some(reference("local-account"));
        assert_eq!(catalog.target(&form), Some(local_profile));
    }

    #[test]
    fn master_setup_opens_on_the_backend_whose_pool_is_assigned() {
        let reference = |value: &str| CredentialRef::new(value).unwrap();
        let models = |local_projects: &str| -> ModelAccess {
            toml::from_str(&format!(r#"accounts = ["codex-account", "local-account"]
connections = [
  {{ id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = {{ kind = "native" }}, model = {{ id = "native-default", author = "openai" }}, auth = {{ method = "native_managed", profile_ref = "codex-profile" }} }},
  {{ id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:11434/v1" }}, model = {{ id = "qwen", author = "openai-compatible" }}, auth = {{ method = "none" }} }},
]
pools = [
  {{ id = "codex-pool", members = ["codex-connection"], projects = ["demo"], roles = ["project_leader"], fallback_allowed = false }},
  {{ id = "local-pool", members = ["local-connection"], projects = [{local_projects}], roles = ["project_leader"], fallback_allowed = false }},
]"#)).unwrap()
        };
        let catalog = |models| Catalog {
            config_id: ConfigId::new("0".repeat(64)).unwrap(),
            source_hash: "0".repeat(64),
            revision: Cursor::new(1),
            models,
            approved: vec![reference("codex-account"), reference("local-account")],
            targets: vec![reference("codex-connection"), reference("local")],
            capacities: [
                ResourceBudget::unit(),
                ResourceBudget::unit(),
                ResourceBudget::unit(),
            ],
        };
        let mut form = Form {
            database: None,
            project: ProjectId::new("demo").unwrap(),
            backend: MasterBackend::Rcli,
            pool: None,
            account: None,
            target: None,
            values: budget_values(&ResourceBudget::unit()).map(|value| value.to_string()),
            field: 0,
            generation: 1,
            open: false,
            explicit: false,
            edited: false,
        };
        let codex_only = catalog(models(""));
        assert_eq!(
            codex_only.assigned_backend(&form),
            Some(MasterBackend::Codex)
        );
        let both = catalog(models("\"demo\""));
        assert_eq!(both.assigned_backend(&form), Some(MasterBackend::Rcli));
        form.project = ProjectId::new("other").unwrap();
        assert_eq!(both.assigned_backend(&form), None);
    }

    #[test]
    fn default_master_budget_allows_a_slow_local_turn_within_policy() {
        let roomy = ResourceBudget {
            wall_time_ms: 86_400_000,
            ..ResourceBudget::unit()
        };
        let budget = default_master_budget(&[roomy.clone(), roomy.clone(), roomy]);
        assert_eq!(budget.wall_time_ms, MASTER_TURN_WALL_MS);
        assert!(budget.wall_time_ms >= 600_000);
        assert_eq!(budget.validate(), Ok(()));
        let tight = ResourceBudget {
            wall_time_ms: 90_000,
            ..ResourceBudget::unit()
        };
        let roomy = ResourceBudget {
            wall_time_ms: 86_400_000,
            ..ResourceBudget::unit()
        };
        let clipped = default_master_budget(&[roomy.clone(), tight.clone(), roomy]);
        assert_eq!(clipped.wall_time_ms, 90_000);
        assert!(clipped.fits(&tight));
    }

    #[test]
    fn default_master_budget_holds_a_turn_of_large_command_results_within_policy() {
        let policy = crate::routing::RoutingPolicy::default();
        let budget = default_master_budget(&[
            policy.global_capacity.clone(),
            policy.project_capacity.clone(),
            policy.task_capacity.clone(),
        ]);
        let command = 2 * 1_049_335;
        assert!(budget.output_bytes >= 16 * command);
        assert_eq!(budget.output_bytes, MASTER_TURN_OUTPUT_BYTES);
        assert!(budget.fits(&policy.task_capacity));
        assert_eq!(budget.validate(), Ok(()));
        let tight = ResourceBudget {
            output_bytes: 4_194_304,
            wall_time_ms: 86_400_000,
            ..ResourceBudget::unit()
        };
        let roomy = ResourceBudget {
            output_bytes: 1 << 40,
            wall_time_ms: 86_400_000,
            ..ResourceBudget::unit()
        };
        let clipped = default_master_budget(&[roomy.clone(), roomy, tight.clone()]);
        assert_eq!(clipped.output_bytes, 4_194_304);
        assert!(clipped.fits(&tight));
    }
}

/// Explains why a master turn stopped, in words the owner can act on.
/// Input: the engine's turn reason. Output: a short plain sentence.
fn reason_text(reason: &MasterReason) -> &'static str {
    match reason {
        MasterReason::ReadinessUnknown => {
            "Not signed in: sign in to this account (Settings, 2 Connections, F2), then send again"
        }
        MasterReason::ReadinessStale => {
            "Sign-in is out of date: sign in again (Settings, 2 Connections, F2), then send again"
        }
        MasterReason::InvalidAuthentication => {
            "Sign-in was rejected: sign in again, then send again"
        }
        MasterReason::QuotaUnknown => "Usage limit is unknown for this account",
        MasterReason::QuotaExhausted => "Usage limit reached for this account",
        MasterReason::RateLimited => "Rate limited; try again shortly",
        MasterReason::CooldownActive => "Account is cooling down; try again shortly",
        MasterReason::TransientFailure => "Account is unavailable for a moment; try again shortly",
        MasterReason::ConnectionChanged => concat!(
            "Settings or engine changed since this master opened: ",
            "close it (F4), open it again (F2, Enter), then send again"
        ),
        MasterReason::ScopeRevoked => "Access to this project was revoked",
        MasterReason::CapacityUnavailable => "Another turn is still running; wait for it",
        MasterReason::BudgetExhausted => "Budget exhausted",
        MasterReason::ProfileBusy => "Account is busy with a sign-in or sign-out",
        MasterReason::ApprovalRequired => "Needs your approval",
        MasterReason::RuntimeUnavailable => "Model runtime unavailable",
        MasterReason::CaptureIncomplete => "Reply was incomplete",
        MasterReason::OutputLimit => "Reply was too long",
        MasterReason::UnsupportedCapability => "This backend cannot run the master yet",
        MasterReason::RepositoryMissing => "This project has no repository yet",
        MasterReason::RepositoryConfigRefused => {
            "A Git setting in this project's repository runs commands or redirects Git"
        }
        MasterReason::Cancelled => "Cancelled",
        MasterReason::Unknown => "Something went wrong",
    }
}

#[cfg(test)]
mod turn_line_tests {
    use super::{turn_holds, turn_lines};
    use crate::master_session::{
        MasterAccountState, MasterReason, MasterReply, MasterReplyProvenance, MasterTurnPhase,
        MasterTurnView,
    };
    use crate::protocol::{CredentialRef, Cursor, RequestId, SessionId};

    #[test]
    fn a_long_reply_shows_whole_in_its_turn() {
        let text = "The README now covers setup, tests and the release steps.\n".repeat(190);
        assert!(text.len() > 8192);
        let turn = MasterTurnView {
            id: RequestId::new("turn-1").unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal: 1,
            input_cursor: Cursor::new(1),
            phase: MasterTurnPhase::Succeeded,
            reason: None,
            attempt: None,
            runtime: None,
            reply: Some(MasterReply {
                text: text.clone(),
                provenance: MasterReplyProvenance {
                    attempt_id: RequestId::new("attempt-1").unwrap(),
                    runtime_ref: RequestId::new("runtime-1").unwrap(),
                    capture_ref: RequestId::new("capture-1").unwrap(),
                    capture_sequence: 1,
                },
            }),
            accounts: Vec::new(),
            tasks: Vec::new(),
            detail: None,
            cause: None,
            changes: None,
        };
        let mut lines = Vec::new();
        turn_lines(&turn, &mut lines);
        assert!(lines.last().unwrap().ends_with(&text), "{lines:?}");
    }

    #[test]
    fn only_a_live_current_turn_holds_the_conversation() {
        let turn = |id: &str, phase| MasterTurnView {
            id: RequestId::new(id).unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal: 1,
            input_cursor: Cursor::new(1),
            phase,
            reason: None,
            detail: None,
            cause: None,
            attempt: None,
            runtime: None,
            reply: None,
            accounts: Vec::new(),
            tasks: Vec::new(),
            changes: None,
            engine_request: None,
        };
        let current = RequestId::new("turn-1").unwrap();
        assert!(!turn_holds(None, &[]));
        assert!(
            !turn_holds(
                Some(&current),
                &[turn("turn-1", MasterTurnPhase::Uncertain)]
            ),
            "a turn the harness left uncertain does not hold the next message (LB-19)"
        );
        for phase in [
            MasterTurnPhase::Queued,
            MasterTurnPhase::Prepared,
            MasterTurnPhase::Delivering,
            MasterTurnPhase::Running,
            MasterTurnPhase::WaitingApproval,
        ] {
            assert!(
                turn_holds(Some(&current), &[turn("turn-1", phase)]),
                "{phase:?}"
            );
        }
        assert!(
            turn_holds(
                Some(&current),
                &[turn("turn-0", MasterTurnPhase::Uncertain)]
            ),
            "a current turn missing from the page is treated as live"
        );
    }

    #[test]
    fn a_stopped_turn_line_ends_with_the_cause_the_engine_stored() {
        let turn = |reason, cause: &str| MasterTurnView {
            id: RequestId::new("turn-1").unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal: 1,
            input_cursor: Cursor::new(1),
            phase: MasterTurnPhase::Blocked,
            reason: Some(reason),
            detail: None,
            cause: Some(cause.to_owned()),
            attempt: None,
            runtime: None,
            reply: None,
            accounts: Vec::new(),
            tasks: Vec::new(),
            changes: None,
            engine_request: None,
        };
        for (reason, cause, expected) in [
            (
                MasterReason::RuntimeUnavailable,
                "rcli_profile_configuration_required",
                "Turn 1: Blocked | Model runtime unavailable (rcli_profile_configuration_required)",
            ),
            (
                MasterReason::Unknown,
                "master_runtime_binding",
                "Turn 1: Blocked | Something went wrong (master_runtime_binding)",
            ),
        ] {
            let mut lines = Vec::new();
            turn_lines(&turn(reason, cause), &mut lines);
            assert_eq!(lines, vec![expected.to_owned()]);
        }
    }

    #[test]
    fn a_blocked_turn_without_a_sign_in_tells_the_owner_what_to_do() {
        let turn = MasterTurnView {
            id: RequestId::new("turn-1").unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal: 1,
            input_cursor: Cursor::new(1),
            phase: MasterTurnPhase::Blocked,
            reason: Some(MasterReason::ReadinessUnknown),
            attempt: None,
            runtime: None,
            reply: None,
            accounts: Vec::new(),
            tasks: Vec::new(),
            changes: None,
            engine_request: None,
            detail: None,
            cause: None,
        };
        let mut lines = Vec::new();
        turn_lines(&turn, &mut lines);
        assert_eq!(
            lines,
            vec![
                "Turn 1: Blocked | Not signed in: sign in to this account (Settings, 2 Connections, F2), then send again"
                    .to_string()
            ]
        );
        let changed = MasterTurnView {
            reason: Some(MasterReason::ConnectionChanged),
            ..turn
        };
        let mut lines = Vec::new();
        turn_lines(&changed, &mut lines);
        assert_eq!(
            lines,
            vec![
                "Turn 1: Blocked | Settings or engine changed since this master opened: close it (F4), open it again (F2, Enter), then send again"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_turn_shows_its_account_and_a_full_block_names_each_account_and_why() {
        let attempt: crate::model_gateway::Attempt = serde_json::from_value(serde_json::json!({
            "request": {
                "id": "attempt-1",
                "task_id": "master-turn-1",
                "project_id": "project-1",
                "role": "ProjectLeader",
                "pool_ref": "shared-pool",
                "target_profile_ref": "claude-b",
                "route": {
                    "preferred_target": "claude-b",
                    "required_target": "claude-b",
                    "required_capabilities": [],
                    "account_ref": "claude-account-b",
                    "budget_ref": "conversation-budget",
                    "resources": crate::routing::ResourceBudget::unit()
                },
                "previous_attempt": null
            },
            "config_id": "a".repeat(64),
            "connection": {
                "id": "claude-b",
                "provider": "anthropic",
                "harness": "claude",
                "protocol": "native",
                "account_ref": "claude-account-b",
                "endpoint": { "kind": "native" },
                "model": { "id": "configured-claude-model", "author": "anthropic" },
                "auth": { "method": "native_managed", "profile_ref": "claude-profile-b" }
            },
            "connection_revision": "b".repeat(64),
            "selected_at_ms": 1,
            "reasons": [],
            "completion": null
        }))
        .unwrap();
        let running = MasterTurnView {
            id: RequestId::new("turn-2").unwrap(),
            session_id: SessionId::new("session-1").unwrap(),
            ordinal: 2,
            input_cursor: Cursor::new(2),
            phase: MasterTurnPhase::Running,
            reason: None,
            attempt: Some(attempt),
            runtime: None,
            reply: None,
            accounts: Vec::new(),
            tasks: Vec::new(),
            changes: None,
            engine_request: None,
            detail: None,
            cause: None,
        };
        let mut lines = Vec::new();
        turn_lines(&running, &mut lines);
        assert_eq!(
            lines,
            vec!["Turn 2: Running | on account claude-account-b".to_string()]
        );
        let state = |account: &str, harness: &str, reason| MasterAccountState {
            account_ref: CredentialRef::new(account).unwrap(),
            connection: CredentialRef::new(account).unwrap(),
            harness: CredentialRef::new(harness).unwrap(),
            reason,
        };
        let blocked = MasterTurnView {
            phase: MasterTurnPhase::Blocked,
            reason: Some(MasterReason::ReadinessUnknown),
            attempt: None,
            accounts: vec![
                state(
                    "claude-account-a",
                    "claude",
                    Some(MasterReason::ReadinessUnknown),
                ),
                state(
                    "claude-account-b",
                    "claude",
                    Some(MasterReason::RateLimited),
                ),
                state("codex-account-c", "codex", None),
            ],
            ..running
        };
        let mut lines = Vec::new();
        turn_lines(&blocked, &mut lines);
        assert_eq!(
            lines,
            vec![
                "Turn 2: Blocked | No account in this master's pool can run it now:".to_string(),
                "  claude-account-a: Not signed in: sign in to this account (Settings, 2 Connections, F2), then send again".to_string(),
                "  claude-account-b: Rate limited; try again shortly".to_string(),
                "  codex-account-c is ready on codex: open a codex master to use it".to_string(),
            ]
        );
    }
}
