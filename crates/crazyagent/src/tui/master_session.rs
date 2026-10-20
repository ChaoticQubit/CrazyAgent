use super::app::UiEvent;
use super::client::MutationDelivery;
use crate::client_controller::OwnerController;
use crate::config::{AccountPool, ModelAccess, ModelConnection, ModelEndpoint};
use crate::conversation::{ConversationContent, ConversationEntry};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, OwnerData, OwnerProvenance, OwnerRead,
    OwnerReadReply, OwnerResponse, OwnerResult,
};
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionPhase, MasterSessionView,
    MasterTurnPhase, MasterTurnView,
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
        confirmation: Option<Confirmation>,
    },
    Mutation {
        submission: Submission,
        delivery: MutationDelivery,
    },
}

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

pub(super) struct Catalog {
    config_id: ConfigId,
    source_hash: String,
    revision: Cursor,
    models: ModelAccess,
    approved: Vec<CredentialRef>,
    targets: Vec<CredentialRef>,
    capacities: [ResourceBudget; 3],
}

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
}

struct Pending {
    submission: Submission,
    operation: Option<(RequestId, String)>,
    receipt: Option<OwnerProvenance>,
    retry_allowed: bool,
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
            notice: "Current master metadata unavailable; inspect Settings; Ctrl-R reconnects"
                .into(),
        }
    }
}

impl State {
    pub fn select(
        &mut self,
        identity: Option<OwnerConnectionIdentity>,
        project: Option<ProjectId>,
        ready: bool,
    ) -> Result<(), String> {
        if self.identity != identity || self.project != project {
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
            let budget = ResourceBudget::unit();
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
            });
            self.forms.len() - 1
        };
        self.forms[index].open = true;
        self.forms[index].field = 0;
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
        if !form.open {
            return None;
        }
        Some(match form.field {
            0 => backend_label(form.backend),
            1 => form.pool.as_ref().map_or("", CredentialRef::as_str),
            2 => form.account.as_ref().map_or("", CredentialRef::as_str),
            3 => form.target.as_ref().map_or("", CredentialRef::as_str),
            field => &form.values[field - 4],
        })
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
            form.target = catalog.targets.first().cloned();
        }
    }

    pub fn reduce(&mut self, event: &UiEvent) -> Result<bool, String> {
        if !self.is_open() {
            return Ok(false);
        }
        let index = self.form_index().unwrap();
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
                let form = &mut self.forms[index];
                match form.field {
                    0 => {
                        let backends = [
                            MasterBackend::Rcli,
                            MasterBackend::Codex,
                            MasterBackend::Claude,
                        ];
                        let current = backends
                            .iter()
                            .position(|backend| backend == &form.backend)
                            .unwrap();
                        form.backend = backends[(current + if forward { 1 } else { 2 }) % 3];
                        form.account = None;
                        if self.catalog.as_ref().is_some_and(|catalog| {
                            !catalog
                                .pools(form)
                                .iter()
                                .any(|pool| Some(&pool.id) == form.pool.as_ref())
                        }) {
                            form.pool = None;
                        }
                    }
                    1 => {
                        if let Some(catalog) = &self.catalog {
                            let choices: Vec<_> = catalog
                                .pools(form)
                                .iter()
                                .map(|pool| pool.id.clone())
                                .collect();
                            let selected = cycle(&choices, form.pool.as_ref(), forward);
                            if form.pool != selected {
                                form.account = None;
                            }
                            form.pool = selected;
                        }
                    }
                    2 => {
                        if let Some(catalog) = &self.catalog {
                            form.account =
                                cycle(&catalog.accounts(form), form.account.as_ref(), forward);
                        }
                    }
                    3 => {
                        if let Some(catalog) = &self.catalog {
                            form.target = cycle(&catalog.targets, form.target.as_ref(), forward);
                        }
                    }
                    _ => return Ok(true),
                }
                form.generation = form
                    .generation
                    .checked_add(1)
                    .ok_or("Master form generation exhausted")?;
                self.defaults();
            }
            UiEvent::Character(character) => {
                let form = &mut self.forms[index];
                if form.field >= 4 {
                    let value = &mut form.values[form.field - 4];
                    if !character.is_ascii_digit() || value.len() >= 13 {
                        return Err(
                            "Allowance uses at most 13 decimal digits; literal value retained"
                                .into(),
                        );
                    }
                    value.push(*character);
                    form.generation = form
                        .generation
                        .checked_add(1)
                        .ok_or("Master form generation exhausted")?;
                }
            }
            UiEvent::Backspace => {
                let form = &mut self.forms[index];
                if form.field >= 4 && form.values[form.field - 4].pop().is_some() {
                    form.generation = form
                        .generation
                        .checked_add(1)
                        .ok_or("Master form generation exhausted")?;
                }
            }
            UiEvent::Submit => {
                let selection = self.selection()?;
                let catalog = self.catalog.as_ref().unwrap();
                let action = MasterOwnerAction::Open {
                    project_id: self.project.clone().unwrap(),
                    expected_config_id: catalog.config_id.clone(),
                    expected_source_hash: catalog.source_hash.clone(),
                    selection,
                };
                self.enqueue(action, self.forms[index].generation)?;
                self.forms[index].explicit = true;
            }
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

    fn selection(&self) -> Result<MasterSelection, String> {
        if !self.ready {
            return Err("Open disabled: authenticated owner unavailable; inspect Settings; Ctrl-R reconnects".into());
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
        let mut values = [0; 7];
        for (index, value) in form.values.iter().enumerate() {
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
        if catalog
            .capacities
            .iter()
            .any(|capacity| !resources.fits(capacity))
        {
            return Err("Open disabled: allowance exceeds current owner policy; all seven values retained; inspect Settings".into());
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
            return Err("Master owner intent pending or unavailable; draft and exact identity retained; Ctrl-R recovers".into());
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
        if session.current_turn.is_some() {
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
            confirmation,
        })
    }

    pub fn apply(&mut self, event: Event) -> Result<Option<Submission>, String> {
        match event {
            Event::Catalog { context, result } => {
                if self.flight.as_ref() != Some(&context) {
                    return Ok(None);
                }
                self.flight = None;
                if !self.matches(&context) {
                    return Ok(None);
                }
                match result {
                    Ok(catalog) => {
                        self.catalog = Some(*catalog);
                        self.defaults();
                        self.notice = "Actual owner catalog and policy loaded; native delivery/authentication remain unsupported".into();
                    }
                    Err(problem) => {
                        self.catalog = None;
                        self.notice = format!(
                            "Metadata unavailable: {problem}; inspect Settings; Ctrl-R refreshes"
                        );
                    }
                }
            }
            Event::Read { context, result } => {
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
                                self.notice = format!(
                                    "{problem}; receipt and drafts retained; Ctrl-R refreshes"
                                );
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
                            self.pending = None;
                            if let MasterOwnerAction::Open { .. } = submission.action
                                && let Some(form) = self.forms.iter_mut().find(|form| {
                                    form.database.as_deref()
                                        == Some(submission.identity.database_instance())
                                        && &form.project == action_project(&submission.action)
                                })
                                && form.generation == submission.generation
                            {
                                form.open = false;
                            }
                            self.notice = "Actual master receipt and read agree; native delivery/authentication remain unsupported".into();
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
            }
            Event::Mutation {
                submission,
                operation,
                result,
                retained,
            } => {
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
                    return Err(
                        "Master owner operation identity changed; original intent retained".into(),
                    );
                }
                pending.operation = operation.clone();
                pending.retry_allowed = false;
                match result {
                    Ok(response) => match *response {
                        OwnerResponse::Applied { operation: applied }
                        | OwnerResponse::Committed { operation: applied } => {
                            if retained
                                || operation.as_ref()
                                    != Some(&(
                                        applied.provenance.operation_id.clone(),
                                        applied.provenance.request_hash.clone(),
                                    ))
                            {
                                return Err(
                                    "Master receipt provenance mismatch; intent retained".into()
                                );
                            }
                            match applied.result {
                                OwnerResult::Applied => {
                                    pending.receipt = Some(applied.provenance);
                                    self.next_read = Instant::now();
                                    self.notice = "Owner applied master intent; waiting for matching actual session/input/turn read".into();
                                }
                                OwnerResult::Rejected { code } => {
                                    self.pending = None;
                                    self.notice = format!(
                                        "Owner rejected master intent: {code}; fields and draft retained"
                                    );
                                }
                                _ => return Err("Unexpected master owner result".into()),
                            }
                        }
                        OwnerResponse::NotRecordedAtBarrier { .. } => {
                            if !retained {
                                return Err("Master barrier lost original intent".into());
                            }
                            pending.retry_allowed = true;
                            self.notice = "Original owner operation not recorded at actual barrier; Ctrl-R explicitly retries that exact request".into();
                        }
                        OwnerResponse::OutcomeUnknown { operation_id } => {
                            if !retained
                                || !operation
                                    .as_ref()
                                    .is_some_and(|operation| operation.0 == operation_id)
                            {
                                return Err("Master unknown outcome lost original identity".into());
                            }
                            self.notice = "Owner outcome unknown; original identity and drafts retained; Ctrl-R reconnects and reconciles".into();
                        }
                        OwnerResponse::Rejected { code } => {
                            if !retained {
                                self.pending = None;
                            }
                            self.notice = format!(
                                "Owner admission rejected: {code}; draft and any original intent retained"
                            );
                        }
                        OwnerResponse::Read { .. } => {
                            return Err("Unexpected read for master mutation".into());
                        }
                    },
                    Err(problem) => {
                        if !retained {
                            self.pending = None;
                        }
                        self.ready = false;
                        self.notice = if retained {
                            format!(
                                "Owner outcome unknown: {problem}; original identity and drafts retained; Ctrl-R reconnects and reconciles"
                            )
                        } else {
                            format!(
                                "Master intent unavailable: {problem}; fields and draft retained; Ctrl-R reconnects"
                            )
                        };
                    }
                }
            }
        }
        self.assert_consistent();
        Ok(None)
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
        if action_project(&pending.submission.action) != &context.project
            || pending.submission.identity.database_instance()
                != context.identity.database_instance()
            || view.revision.value() < receipt.revision_after.value()
        {
            return Ok(None);
        }
        let session = view
            .session
            .as_ref()
            .ok_or("Applied master session missing from actual read")?;
        let matches = match &pending.submission.action {
            MasterOwnerAction::Open {
                selection,
                expected_config_id,
                expected_source_hash,
                ..
            } => {
                session.selection == *selection
                    && session.config_id == *expected_config_id
                    && session.source_hash == *expected_source_hash
                    && session.phase == MasterSessionPhase::Configured
                    && session.owner == *receipt
            }
            MasterOwnerAction::Submit {
                session_id, text, ..
            } => {
                session.session_id == *session_id
                    && view.turns.iter().any(|turn| {
                        turn.id == receipt.operation_id
                            && turn.session_id == *session_id
                            && receipt.event_cursor.as_ref() == Some(&turn.input_cursor)
                    })
                    && view.input.as_ref().is_some_and(|input| {
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
                    && view
                        .turns
                        .iter()
                        .any(|turn| turn.id == *turn_id && turn.phase == MasterTurnPhase::Cancelled)
            }
            MasterOwnerAction::Close { session_id, .. } => {
                session.session_id == *session_id
                    && session.phase == MasterSessionPhase::Closed
                    && session.current_turn.is_none()
            }
        };
        if !matches {
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
            let form = &self.forms[self.form_index().unwrap()];
            let choices = [
                backend_label(form.backend).to_owned(),
                form.pool
                    .as_ref()
                    .map_or("Unavailable", CredentialRef::as_str)
                    .to_owned(),
                form.account
                    .as_ref()
                    .map_or("Unavailable", CredentialRef::as_str)
                    .to_owned(),
                form.target
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
                    if form.field == index { ">" } else { " " },
                    choices[index]
                ));
            }
            for (index, name) in DIMENSIONS.iter().enumerate() {
                lines.push(format!(
                    "{} {name}: {}",
                    if form.field == index + 4 { ">" } else { " " },
                    form.values[index]
                ));
            }
            if let Some(catalog) = &self.catalog {
                lines.push(format!(
                    "Owner catalog revision {} | config {} | source {}",
                    catalog.revision.value(),
                    catalog.config_id.as_str(),
                    catalog.source_hash
                ));
            }
            lines.push(match self.selection() { Ok(_) if !self.has_pending() => "Enter Open | Tab/Shift-Tab fields | arrows choices | Esc retains setup and conversation draft".into(), Ok(_) => "Open disabled: owner intent unresolved; fields retained".into(), Err(reason) => reason });
            lines.push(
                "Settings: Esc, Tab twice | execution sandbox is separate from credential profile"
                    .into(),
            );
        } else if let Some(view) = &self.view {
            if let Some(session) = &view.session {
                lines.push(format!(
                    "Master: {:?} | {} | {} | pool {}",
                    session.phase,
                    session.session_id.as_str(),
                    backend_label(session.selection.backend),
                    session.selection.pool_ref.as_str()
                ));
                for turn in &view.turns {
                    lines.push(format!(
                        "Turn: {:?} | {} | input {} | reason {:?}",
                        turn.phase,
                        turn.id.as_str(),
                        turn.input_cursor.value(),
                        turn.reason
                    ));
                    if let Some(reply) = &turn.reply {
                        lines.push(format!(
                            "Actual reply | attempt {} | runtime {} | capture {} sequence {}\n{}",
                            reply.provenance.attempt_id.as_str(),
                            reply.provenance.runtime_ref.as_str(),
                            reply.provenance.capture_ref.as_str(),
                            reply.provenance.capture_sequence,
                            super::views::safe_text(&reply.text, 8192)
                        ));
                    }
                }
            } else {
                lines.push(
                    "No master configured; Enter stores an owner note until explicit Open".into(),
                );
            }
        } else {
            lines.push("Waiting for actual master state; stored notes remain notes".into());
        }
        if let Some(pending) = &self.pending {
            lines.push(match &pending.operation { Some((id, _)) => format!("Owner operation {} unresolved; Ctrl-R reconciles or retries after an actual barrier", id.as_str()), None => "Master owner intent queued; fields and newer text retained".into() });
        }
        if self.is_open() || self.pending.is_some() || self.view.is_none() {
            lines.push(self.notice.clone());
        }
        if self.is_open()
            || self
                .view
                .as_ref()
                .is_some_and(|view| view.session.is_some())
        {
            lines.push("Configured/Queued are durable bootstrap states; native delivery/authentication remain unsupported".into());
        }
        lines.join("\n")
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
        }
    }
}

impl Catalog {
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

pub(super) fn action_project(action: &MasterOwnerAction) -> &ProjectId {
    match action {
        MasterOwnerAction::Open { project_id, .. }
        | MasterOwnerAction::Submit { project_id, .. }
        | MasterOwnerAction::Cancel { project_id, .. }
        | MasterOwnerAction::Close { project_id, .. } => project_id,
    }
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
            connection.harness.as_str() == "claude-code"
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
        let mut cursor = None;
        let mut complete = false;
        for _ in 0..64 {
            let state = controller
                .read(OwnerRead::ModelAccessPage {
                    collection,
                    cursor: cursor.clone(),
                    limit: 16,
                })
                .await
                .map_err(|error| error.to_string())?;
            if let Some((revision, config, _)) = &basis {
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
            if let Some((_, _, hash)) = &basis {
                if hash != &source_hash {
                    return Err("master_catalog_source_changed".into());
                }
            } else {
                basis = Some((state.revision, state.config_id, source_hash.clone()));
            }
            for item in items {
                match (collection, item) {
                    (
                        ModelAccessCollection::Accounts,
                        ModelAccessRecord::Account { account_ref },
                    ) => models.accounts.push(account_ref),
                    (
                        ModelAccessCollection::Connections,
                        ModelAccessRecord::Connection { connection },
                    ) => models.connections.push(connection),
                    (ModelAccessCollection::Pools, ModelAccessRecord::Pool { pool }) => {
                        models.pools.push(pool)
                    }
                    _ => return Err("master_catalog_record_kind".into()),
                }
            }
            if models.accounts.len() > 64
                || models.connections.len() > 64
                || models.pools.len() > 64
            {
                return Err("master_catalog_capacity".into());
            }
            if let Some(next) = &next {
                let (revision, config, _) = basis.as_ref().unwrap();
                if next.db_instance != controller.identity().database_instance()
                    || next.engine_epoch != controller.identity().engine_epoch()
                    || next.collection != collection
                    || next.at_revision != *revision
                    || next.config_id != *config
                    || next.source_hash != source_hash
                    || cursor.as_ref().is_some_and(
                        |previous: &crate::engine::owner::ModelAccessCursor| {
                            next.after_id.as_str() <= previous.after_id.as_str()
                        },
                    )
                {
                    return Err("master_catalog_cursor_context".into());
                }
            } else {
                complete = true;
            }
            cursor = next;
            if complete {
                break;
            }
        }
        if !complete {
            return Err("master_catalog_incomplete; inspect Settings".into());
        }
    }
    let (revision, config_id, source_hash) = basis.ok_or("master_catalog_basis_missing")?;
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
        same_basis(&reply, &revision, &config_id)?;
        let OwnerData::Setting { descriptor } = reply.data else {
            return Err("master_policy_descriptor_required".into());
        };
        if descriptor.name != name {
            return Err("master_policy_descriptor_context".into());
        }
        values.push(descriptor.value);
    }
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
        let mut cursor = None;
        let mut complete = false;
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
            same_basis(&reply, &view.revision, &state.config_id)?;
            let OwnerData::ProjectMasterTurns { page } = reply.data else {
                return Err("master_turn_page_required".into());
            };
            if page.project_id != context.project
                || page.session_id != session.session_id
                || page.turns.len() > 32
            {
                return Err("master_turn_page_context".into());
            }
            let mut after = cursor
                .as_ref()
                .map_or(0, |cursor: &crate::master_session::MasterCursor| {
                    cursor.after_ordinal
                });
            for turn in page.turns {
                if turn.session_id != session.session_id
                    || turn.ordinal <= after
                    || turn.input_cursor.value() > view.revision.value()
                {
                    return Err("master_turn_context".into());
                }
                after = turn.ordinal;
                view.turns.push(turn);
                if view.turns.len() > 32 {
                    view.turns.remove(0);
                }
            }
            if let Some(next) = &page.next {
                if next.project_id != context.project
                    || next.session_id != session.session_id
                    || next.version != view.revision
                    || next.config_id != state.config_id
                    || next.db_instance != context.identity.database_instance()
                    || next.engine_epoch != context.identity.engine_epoch()
                    || next.after_ordinal != after
                    || cursor
                        .as_ref()
                        .is_some_and(|previous| next.after_ordinal <= previous.after_ordinal)
                {
                    return Err("master_turn_cursor_context".into());
                }
            } else {
                complete = true;
            }
            cursor = page.next;
            if complete {
                break;
            }
        }
        if !complete {
            return Err("master_turn_pages_incomplete".into());
        }
    }
    if let Some(confirmation) = confirmation
        && let MasterOwnerAction::Submit { .. } = confirmation.submission.action
    {
        let receipt = &confirmation.receipt;
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
            same_basis(&reply, &view.revision, &state.config_id)?;
            let OwnerData::ConversationHistory { page } = reply.data else {
                return Err("master_owner_input_read_required".into());
            };
            if page.project_id != context.project || page.entries.len() > 32 {
                return Err("master_owner_input_page_context".into());
            }
            view.input = page
                .entries
                .into_iter()
                .find(|entry| receipt.event_cursor.as_ref() == Some(&entry.event_cursor));
            if view.input.is_some() || page.next.is_none() {
                break;
            }
            if page.next.as_ref().is_some_and(|next| {
                next.project_id != context.project
                    || next.db_instance != context.identity.database_instance()
                    || next.engine_epoch != context.identity.engine_epoch()
                    || cursor.as_ref().is_some_and(
                        |previous: &crate::conversation::ConversationCursor| {
                            next.after.value() <= previous.after.value()
                        },
                    )
            }) {
                return Err("master_owner_input_cursor_context".into());
            }
            cursor = page.next;
        }
    }
    Ok(view)
}
