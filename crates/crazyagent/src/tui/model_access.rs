use super::app::UiEvent;
use super::client::MutationDelivery;
use super::native_auth;
use crate::config::{
    AccountPool, ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint,
    ModelReference, ProviderQuotaPolicy,
};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    AccountAuthCapabilitiesReply, AccountProfileReply, AccountProfileRequest, AccountSecret,
    ModelAccessCollection, ModelAccessCursor, ModelAccessRecord, OwnerData, OwnerRead,
    OwnerReadReply, OwnerResponse, OwnerResult,
};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};
use std::path::PathBuf;

pub(super) mod account_profile;
mod managed_setup;
mod pool_assignment;
mod pool_setup;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Basis {
    pub identity: OwnerConnectionIdentity,
    pub revision: Cursor,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub basis: Basis,
    pub generation: u64,
    pub edit: ModelAccessEdit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProfileSubmission {
    pub basis: Basis,
    pub connection: ModelConnection,
    pub generation: u64,
    pub request: AccountProfileRequest,
}

pub(super) struct HomeSelection<'a> {
    pub basis: &'a Basis,
    pub connection: &'a ModelConnection,
    pub configured_home: &'a PathBuf,
}

struct HomeDraft {
    basis: Basis,
    connection: ModelConnection,
    value: String,
    selected: Option<PathBuf>,
}

pub(super) enum Job {
    ClaudeCode(Box<native_auth::claude_code::Job>),
    NativeAuth(Box<native_auth::Job>),
    AccountProfileConfigure {
        submission: ProfileSubmission,
        secret: Option<AccountSecret>,
    },
    AccountProfileRead {
        submission: ProfileSubmission,
    },
    AccountProfileCapabilities {
        submission: ProfileSubmission,
    },
    Read {
        identity: OwnerConnectionIdentity,
        generation: u64,
        query: OwnerRead,
    },
    Mutation {
        submission: Submission,
        delivery: MutationDelivery,
    },
}

pub(super) enum Event {
    ClaudeCode(Box<native_auth::claude_code::Event>),
    NativeAuth(Box<native_auth::Event>),
    AccountProfile {
        submission: ProfileSubmission,
        result: Result<Box<AccountProfileReply>, String>,
        retained: bool,
    },
    AccountProfileCapabilities {
        submission: ProfileSubmission,
        result: Result<Box<AccountAuthCapabilitiesReply>, String>,
    },
    Read {
        identity: OwnerConnectionIdentity,
        generation: u64,
        query: OwnerRead,
        result: Result<Box<OwnerReadReply>, String>,
    },
    Mutation {
        submission: Submission,
        operation: Option<(RequestId, String)>,
        result: Result<Box<OwnerResponse>, String>,
        retained: bool,
    },
}

#[derive(Clone, Copy)]
enum RecordAction {
    View,
    Replace,
    Remove,
}

pub(super) struct State {
    native_auth: native_auth::State,
    profile: account_profile::Profile,
    managed_setup: Option<managed_setup::Setup>,
    pool_setup: Option<pool_setup::Setup>,
    assignment: Option<pool_assignment::Assignment>,
    assignment_project: Option<(crate::protocol::ProjectId, String)>,
    identity: Option<OwnerConnectionIdentity>,
    ready: bool,
    pub pinned: bool,
    pub explicit: bool,
    collection: ModelAccessCollection,
    basis: Option<Basis>,
    items: Vec<ModelAccessRecord>,
    next: Option<ModelAccessCursor>,
    selected: usize,
    generation: u64,
    flight: Option<(OwnerConnectionIdentity, u64, OwnerRead)>,
    action: RecordAction,
    needs_page: bool,
    rebase: bool,
    form: Option<Form>,
    editing: bool,
    confirmation: Option<(ModelAccessRecord, Basis)>,
    record: Option<ModelAccessRecord>,
    homes: Vec<HomeDraft>,
    home_editor: Option<usize>,
    pending: Option<Submission>,
    operation: Option<(RequestId, String)>,
    retry_allowed: bool,
    job: Option<Job>,
    pub notice: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            native_auth: native_auth::State::default(),
            profile: account_profile::Profile::default(),
            managed_setup: None,
            pool_setup: None,
            assignment: None,
            assignment_project: None,
            identity: None,
            ready: false,
            pinned: false,
            explicit: false,
            collection: ModelAccessCollection::Accounts,
            basis: None,
            items: Vec::new(),
            next: None,
            selected: 0,
            generation: 0,
            flight: None,
            action: RecordAction::View,
            needs_page: true,
            rebase: false,
            form: None,
            editing: false,
            confirmation: None,
            record: None,
            homes: Vec::new(),
            home_editor: None,
            pending: None,
            operation: None,
            retry_allowed: false,
            job: None,
            notice: "Waiting for an actual owner metadata read".into(),
        }
    }
}

impl State {
    pub fn access(&mut self, identity: Option<OwnerConnectionIdentity>, ready: bool) {
        self.discard_unsent_profile();
        self.native_auth.access(identity.clone(), ready);
        self.ready = ready;
        if let Some(identity) = identity {
            self.identity = Some(identity);
        }
        self.basis = None;
        self.items.clear();
        self.selected = 0;
        self.next = None;
        self.flight = None;
        self.confirmation = None;
        self.record = None;
        self.home_editor = None;
        self.needs_page = true;
        self.rebase = false;
        self.job = None;
        if ready && let Some(submission) = &self.pending {
            self.job = Some(Job::Mutation {
                submission: submission.clone(),
                delivery: MutationDelivery::Reconcile,
            });
            self.notice = "Reconnected; reconciling the exact original request".into();
        }
        self.access_profile();
        self.assert_consistent();
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some() || self.native_auth.has_pending() || self.profile.has_pending()
    }

    pub fn loading(&self) -> bool {
        self.flight.is_some()
            || self.job.is_some()
            || self.native_auth.loading()
            || self.profile.loading()
    }

    pub fn disconnected(&self) -> bool {
        !self.ready
    }

    pub fn request(&mut self, active: bool) -> Result<Option<Job>, String> {
        if !self.ready {
            return Ok(None);
        }
        if let Some(job) = self.native_auth.code_request() {
            return Ok(Some(Job::ClaudeCode(Box::new(job))));
        }
        self.native_auth.poll_pending(std::time::Instant::now());
        if let Some(job) = self.native_auth.request() {
            return Ok(Some(Job::NativeAuth(Box::new(job))));
        }
        if let Some(job) = self.request_profile() {
            return Ok(Some(job));
        }
        self.request_managed_setup()?;
        self.request_pool_setup()?;
        self.request_assignment()?;
        if self.job.is_none()
            && self.managed_setup.is_none()
            && self.pool_setup.is_none()
            && self.assignment.is_none()
            && !self.profile.is_open()
            && active
            && !self.pinned
            && self.needs_page
            && !self.loading()
            && !self.has_pending()
        {
            self.read(OwnerRead::ModelAccessPage {
                collection: self.collection,
                cursor: None,
                limit: 16,
            })?;
        }
        Ok(self.job.take())
    }

    pub fn retry(&mut self) -> bool {
        if !self.ready || self.job.is_some() {
            return false;
        }
        if self.retry_profile() {
            return true;
        }
        let Some(submission) = &self.pending else {
            return false;
        };
        self.job = Some(Job::Mutation {
            submission: submission.clone(),
            delivery: if self.retry_allowed {
                MutationDelivery::Retry
            } else {
                MutationDelivery::Reconcile
            },
        });
        self.retry_allowed = false;
        true
    }

    fn advance(&mut self) -> Result<u64, String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Model-access generation exhausted")?;
        Ok(self.generation)
    }

    fn read(&mut self, query: OwnerRead) -> Result<(), String> {
        if !self.ready || self.job.is_some() || self.flight.is_some() || self.has_pending() {
            return Err("Owner metadata request is unavailable or pending".into());
        }
        let identity = self.identity.clone().ok_or("Owner identity unavailable")?;
        let generation = self.advance()?;
        self.flight = Some((identity.clone(), generation, query.clone()));
        self.job = Some(Job::Read {
            identity,
            generation,
            query,
        });
        self.needs_page = false;
        if self.notice.starts_with("Waiting") {
            self.notice = "Reading current metadata from the owner engine".into();
        }
        Ok(())
    }

    fn writable(&self) -> Result<Basis, String> {
        self.basis
            .as_ref()
            .filter(|basis| basis.writable && self.ready)
            .cloned()
            .ok_or_else(|| {
                "Read-only: a current host-granted writable owner read is required".into()
            })
    }

    pub fn reduce(&mut self, event: &UiEvent) -> Result<bool, String> {
        if self.profile.is_open() {
            return self.reduce_profile(event);
        }
        if self.native_auth.is_open() {
            return self.native_auth.reduce(event);
        }
        if self.home_editor.is_some() {
            return self.reduce_home(event);
        }
        if self.managed_setup.is_some() {
            return self.reduce_managed_setup(event);
        }
        if self.pool_setup.is_some() {
            return self.reduce_pool_setup(event);
        }
        if self.assignment.is_some() {
            return self.reduce_assignment(event);
        }
        if matches!(event, UiEvent::ConfigureMaster) {
            return self.open_native_auth();
        }
        if !self.pinned
            && matches!(
                event,
                UiEvent::Character('a' | 'n' | 'e' | 'd')
                    | UiEvent::Submit
                    | UiEvent::HistoryFirst
                    | UiEvent::HistoryNext
            )
        {
            self.explicit = true;
        }
        if self.pinned
            && matches!(
                event,
                UiEvent::SelectionNext | UiEvent::SelectionPrevious | UiEvent::TaskNext
            )
        {
            return Ok(false);
        }
        if matches!(event, UiEvent::Resize(..) | UiEvent::Scroll(_)) {
            return Ok(false);
        }
        if self.editing && matches!(event, UiEvent::FocusNext | UiEvent::FocusPrevious) {
            let form = self.form.as_mut().expect("editing requires a form");
            form.move_field(matches!(event, UiEvent::FocusNext));
            return Ok(true);
        }
        if matches!(event, UiEvent::FocusNext | UiEvent::FocusPrevious) {
            return Ok(false);
        }
        if self.has_pending() {
            self.notice =
                "Outcome pending; fields retained. Ctrl-R reconciles the original request".into();
            return Ok(true);
        }
        let result = self.reduce_idle(event);
        if let Err(error) = &result {
            self.notice = error.clone();
        }
        self.assert_consistent();
        result.map(|()| true)
    }

    fn open_native_auth(&mut self) -> Result<bool, String> {
        if self.editing || self.confirmation.is_some() || self.pending.is_some() {
            return Ok(true);
        }
        if !self.ready
            || self.pinned
            || self.collection != ModelAccessCollection::Connections
            || self.loading()
        {
            self.notice =
                "Select an idle writable managed native Connection for authentication".into();
            return Ok(true);
        }
        let Some(ModelAccessRecord::Connection { connection }) = self.items.get(self.selected)
        else {
            self.notice = "Select an actual Connection record for authentication".into();
            return Ok(true);
        };
        let basis = self
            .basis
            .clone()
            .ok_or("A fresh owner connection read is required")?;
        match self.native_auth.open(basis, connection.clone()) {
            Ok(()) => self.notice = "Inspecting the selected native connection".into(),
            Err(_) => {
                self.notice =
                    "Authentication requires a managed Codex or Claude native connection".into()
            }
        }
        Ok(true)
    }

    fn reduce_idle(&mut self, event: &UiEvent) -> Result<(), String> {
        if matches!(event, UiEvent::HistoryFirst | UiEvent::HistoryNext) {
            let cursor = if matches!(event, UiEvent::HistoryNext) {
                if self.editing {
                    return Err("Close the form before changing pages".into());
                }
                Some(self.next.clone().ok_or("No next page")?)
            } else {
                self.rebase = self.form.is_some();
                None
            };
            self.confirmation = None;
            return self.read(OwnerRead::ModelAccessPage {
                collection: self.collection,
                cursor,
                limit: 16,
            });
        }
        if self.confirmation.is_some() {
            return match event {
                UiEvent::Cancel => {
                    self.confirmation = None;
                    self.notice = "Removal cancelled; metadata unchanged".into();
                    Ok(())
                }
                UiEvent::Submit => {
                    let (record, basis) = self.confirmation.clone().unwrap();
                    let edit = match record {
                        ModelAccessRecord::Account { account_ref } => {
                            ModelAccessEdit::RemoveAccount { account_ref }
                        }
                        ModelAccessRecord::Connection { connection } => {
                            ModelAccessEdit::RemoveConnection { id: connection.id }
                        }
                        ModelAccessRecord::Pool { pool } => {
                            ModelAccessEdit::RemovePool { id: pool.id }
                        }
                    };
                    self.submit(basis, edit)
                }
                _ => Ok(()),
            };
        }
        if self.editing {
            let form = self.form.as_mut().expect("editing requires a form");
            return match event {
                UiEvent::Character(character) => form.character(*character),
                UiEvent::Backspace => {
                    form.backspace();
                    Ok(())
                }
                UiEvent::SelectionNext | UiEvent::SelectionPrevious => {
                    form.choose(matches!(event, UiEvent::SelectionNext));
                    Ok(())
                }
                UiEvent::Cancel => {
                    self.editing = false;
                    self.notice =
                        "Form closed; entered values retained. a/n/e resumes; x discards".into();
                    Ok(())
                }
                UiEvent::Submit => {
                    let edit = form.edit()?;
                    let basis = form.basis.clone();
                    self.submit(basis, edit)
                }
                _ => Ok(()),
            };
        }
        match event {
            UiEvent::Character('a')
                if !self.pinned && self.collection == ModelAccessCollection::Pools =>
            {
                self.begin_assignment()?;
            }
            UiEvent::Character('k') if !self.pinned => self.open_profile()?,
            UiEvent::Character('h') if !self.pinned => self.open_home()?,
            UiEvent::Character('n')
                if !self.pinned && self.collection == ModelAccessCollection::Pools =>
            {
                self.begin_pool_setup(self.writable()?, None)?;
            }
            UiEvent::Character('a')
                if !self.pinned && self.collection == ModelAccessCollection::Accounts =>
            {
                self.begin_managed_setup()?;
            }
            UiEvent::Character(character @ ('1' | '2' | '3' | '4')) => {
                self.explicit = true;
                self.pinned = *character == '4';
                if !self.pinned {
                    self.flight = None;
                    self.job = None;
                    self.collection = match character {
                        '1' => ModelAccessCollection::Accounts,
                        '2' => ModelAccessCollection::Connections,
                        _ => ModelAccessCollection::Pools,
                    };
                    self.basis = None;
                    self.items.clear();
                    self.selected = 0;
                    self.next = None;
                    self.record = None;
                    self.needs_page = true;
                }
            }
            UiEvent::Character('x') => {
                self.form = None;
                self.notice = "Retained form explicitly discarded".into();
            }
            UiEvent::Character('a' | 'n') if !self.pinned => {
                if matches!(event, UiEvent::Character('a')) {
                    self.notice = "Connections use n to create".into();
                    return Ok(());
                }
                let basis = match self.writable() {
                    Ok(basis) => basis,
                    Err(error) => {
                        self.notice = error;
                        return Ok(());
                    }
                };
                if self.form.is_none() {
                    self.form = Some(Form::new(self.collection, basis));
                }
                self.editing = true;
                self.notice =
                    "Reference-only form; Tab selects fields, arrows select choices".into();
            }
            UiEvent::Character('e' | 'd') | UiEvent::Submit if !self.pinned => {
                if matches!(event, UiEvent::Character('e'))
                    && self.collection == ModelAccessCollection::Accounts
                {
                    self.notice = "Account references have Add and Remove operations".into();
                    return Ok(());
                }
                if matches!(event, UiEvent::Character(_)) {
                    if let Err(error) = self.writable() {
                        self.notice = error;
                        return Ok(());
                    }
                    if self.form.is_some() && matches!(event, UiEvent::Character('e')) {
                        self.editing = true;
                        self.notice =
                            "Retained form reopened; close and x to discard explicitly".into();
                        return Ok(());
                    }
                }
                let id = self
                    .items
                    .get(self.selected)
                    .ok_or("Select a metadata record")?
                    .id()
                    .clone();
                self.action = match event {
                    UiEvent::Character('e') => RecordAction::Replace,
                    UiEvent::Character('d') => RecordAction::Remove,
                    _ => RecordAction::View,
                };
                self.read(OwnerRead::ModelAccessRecord {
                    collection: self.collection,
                    id,
                })?;
            }
            UiEvent::SelectionNext | UiEvent::SelectionPrevious if !self.items.is_empty() => {
                let count = self.items.len();
                self.selected = (self.selected
                    + if matches!(event, UiEvent::SelectionNext) {
                        1
                    } else {
                        count - 1
                    })
                    % count;
                self.record = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn open_home(&mut self) -> Result<(), String> {
        if self.loading() || self.has_pending() {
            return Err("Wait for the current owner operation before selecting an SDK home".into());
        }
        let basis = self.writable()?;
        let Some(ModelAccessRecord::Connection { connection }) = &self.record else {
            return Err("Inspect an actual RCLI connection before selecting its SDK home".into());
        };
        if !rcli_home_supported(connection) || self.items.get(self.selected) != self.record.as_ref()
        {
            return Err("SDK home requires an inspected RCLI API-key or local connection".into());
        }
        let index = self.homes.iter().position(|home| {
            home.basis.identity == basis.identity && home.connection == *connection
        });
        let index = if let Some(index) = index {
            let home = &mut self.homes[index];
            if home.basis.config_id != basis.config_id
                || home.basis.source_hash != basis.source_hash
            {
                home.selected = None;
            }
            home.basis = basis;
            index
        } else {
            if self.homes.len() >= 128 {
                return Err("SDK home draft capacity reached; existing input retained".into());
            }
            self.homes.push(HomeDraft {
                basis,
                connection: connection.clone(),
                value: String::new(),
                selected: None,
            });
            self.homes.len() - 1
        };
        self.home_editor = Some(index);
        self.notice = "SDK home is transient intent; Enter selects, Esc retains input".into();
        Ok(())
    }

    fn reduce_home(&mut self, event: &UiEvent) -> Result<bool, String> {
        let home = &mut self.homes[self.home_editor.unwrap()];
        match event {
            UiEvent::Resize(..) | UiEvent::Scroll(_) => return Ok(false),
            UiEvent::Character(character) => {
                if character.is_control() || home.value.len() + character.len_utf8() > 4096 {
                    return Err("SDK home requires at most 4096 literal UTF-8 bytes".into());
                }
                home.value.push(*character);
            }
            UiEvent::Backspace => {
                home.value.pop();
            }
            UiEvent::Submit => {
                let path = PathBuf::from(&home.value);
                if !path.is_absolute() {
                    return Err("Select a literal absolute SDK home; input retained".into());
                }
                home.selected = Some(path);
                self.home_editor = None;
                self.notice = concat!(
                    "Home selected; no registration or sign-in performed. ",
                    "Open a repository master, then F2 and h to review registration"
                )
                .into();
            }
            UiEvent::Cancel | UiEvent::FocusNext | UiEvent::FocusPrevious => {
                self.home_editor = None;
                self.notice = "SDK home input retained; h resumes; no owner operation sent".into();
                return Ok(matches!(event, UiEvent::Cancel));
            }
            _ => {}
        }
        self.assert_consistent();
        Ok(true)
    }

    pub fn selected_homes(&self) -> impl Iterator<Item = HomeSelection<'_>> {
        self.homes.iter().filter_map(|home| {
            (self.ready && self.identity.as_ref() == Some(&home.basis.identity))
                .then_some(home)
                .and_then(|home| {
                    home.selected.as_ref().map(|configured_home| HomeSelection {
                        basis: &home.basis,
                        connection: &home.connection,
                        configured_home,
                    })
                })
        })
    }

    pub fn home_draft(&self) -> Option<&str> {
        self.home_editor
            .map(|index| self.homes[index].value.as_str())
    }

    fn submit(&mut self, basis: Basis, edit: ModelAccessEdit) -> Result<(), String> {
        if self.writable()? != basis {
            return Err(
                "Form is stale; Ctrl-P refreshes its basis, then review retained values".into(),
            );
        }
        if self.job.is_some() || self.flight.is_some() {
            return Err("Wait for the current metadata read".into());
        }
        edit.validate().map_err(|error| error.to_string())?;
        let generation = self.advance()?;
        let submission = Submission {
            basis,
            generation,
            edit,
        };
        self.job = Some(Job::Mutation {
            submission: submission.clone(),
            delivery: MutationDelivery::Submit,
        });
        self.pending = Some(submission);
        self.retry_allowed = false;
        self.operation = None;
        self.notice = "Submitting the exact metadata edit; entered values retained".into();
        Ok(())
    }

    pub fn apply(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::ClaudeCode(event) => self.native_auth.apply_code(*event)?,
            Event::AccountProfile { submission, result, retained } => {
                self.apply_profile(submission, result, retained)?;
            }
            Event::AccountProfileCapabilities { submission, result } => {
                self.apply_profile_capabilities(submission, result);
            }
            Event::NativeAuth(event) => {
                self.native_auth.apply(*event)?;
                self.notice = "Native authentication observation updated; metadata retained".into();
            }
            Event::Read {
                identity,
                generation,
                query,
                result,
            } => {
                if self.flight.as_ref() != Some(&(identity.clone(), generation, query.clone())) {
                    return Ok(());
                }
                self.flight = None;
                match result {
                    Ok(reply) => self.accept_read(identity, query, *reply)?,
                    Err(problem) => {
                        self.ready = false;
                        self.basis = None;
                        self.next = None;
                        self.notice = format!(
                            "Metadata read failed: {problem}; values retained; Ctrl-R reconnects"
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
                if self.pending.as_ref() != Some(&submission) {
                    return Ok(());
                }
                self.operation = operation;
                self.retry_allowed = false;
                let mut applied = false;
                match result {
                    Ok(response) => match *response {
                        OwnerResponse::Applied { operation }
                        | OwnerResponse::Committed { operation } => {
                            if self.operation.as_ref()
                                != Some(&(
                                    operation.provenance.operation_id,
                                    operation.provenance.request_hash,
                                ))
                            {
                                return Err("Model-access receipt identity mismatch".into());
                            }
                            match operation.result {
                                OwnerResult::Applied => {
                                    applied = true;
                                    self.notice = "Metadata saved; authentication and execution remain unavailable".into();
                                    if self.managed_setup.is_none()
                                        && !matches!(
                                            submission.edit,
                                            ModelAccessEdit::RemoveAccount { .. }
                                                | ModelAccessEdit::RemoveConnection { .. }
                                                | ModelAccessEdit::RemovePool { .. }
                                        )
                                    {
                                        self.form = None;
                                        self.editing = false;
                                    }
                                    self.confirmation = None;
                                }
                                OwnerResult::Rejected { code } => {
                                    self.notice = rejection_notice(&code);
                                    self.confirmation = None;
                                }
                                OwnerResult::VerificationUnavailable { .. } => {
                                    return Err(
                                        "Unexpected verification result for metadata".into()
                                    );
                                }
                                OwnerResult::DispatchPrepared { .. } => {
                                    return Err("Unexpected dispatch result for metadata".into());
                                }
                                OwnerResult::RcliProfileRegistered { .. } => {
                                    return Err(
                                        "Unexpected profile registration result for metadata"
                                            .into(),
                                    );
                                }
                            }
                        }
                        OwnerResponse::NotRecordedAtBarrier { .. } => {
                            self.retry_allowed = retained;
                            self.notice = "Not recorded at the engine barrier; Ctrl-R may retry the exact retained request".into();
                        }
                        OwnerResponse::OutcomeUnknown { .. } => {
                            self.notice = "Outcome unknown; entered values and original request retained; Ctrl-R reconciles".into();
                        }
                        OwnerResponse::Rejected { code } => {
                            self.notice = format!(
                                "Owner rejected request: {code}; original intent retained for reconciliation"
                            );
                        }
                        _ => return Err("Unexpected model-access mutation response".into()),
                    },
                    Err(problem) => {
                        self.notice = format!(
                            "Metadata edit failed: {problem}; values retained; Ctrl-P refreshes or Ctrl-R reconnects"
                        );
                        if retained {
                            self.ready = false;
                        }
                    }
                }
                if !retained {
                    self.pending = None;
                    self.operation = None;
                    self.basis = None;
                    self.needs_page = true;
                    self.settle_managed_setup(applied);
                    self.settle_pool_setup(applied);
                    self.settle_assignment(applied);
                }
            }
        }
        self.assert_consistent();
        Ok(())
    }

    fn accept_read(
        &mut self,
        identity: OwnerConnectionIdentity,
        query: OwnerRead,
        reply: OwnerReadReply,
    ) -> Result<(), String> {
        if matches!(reply.data, OwnerData::ResyncRequired) {
            self.basis = None;
            self.next = None;
            self.items.clear();
            self.selected = 0;
            self.notice =
                "Metadata page is stale; Ctrl-P refreshes the first page; values retained".into();
            return Ok(());
        }
        let (source_hash, writable) = match &reply.data {
            OwnerData::ModelAccessPage {
                source_hash,
                writable,
                ..
            }
            | OwnerData::ModelAccessRecord {
                source_hash,
                writable,
                ..
            } => (source_hash.clone(), *writable),
            _ => return Err("Unexpected metadata read response".into()),
        };
        let basis = Basis {
            identity,
            revision: reply.revision,
            config_id: reply.config_id,
            source_hash,
            writable,
        };
        if self.accept_assignment_read(&query, &reply.data, &basis)? {
            return Ok(());
        }
        if self.accept_pool_read(&query, &reply.data, &basis)? {
            return Ok(());
        }
        if self.accept_managed_read(&query, &reply.data, &basis)? {
            return Ok(());
        }
        match (query, reply.data) {
            (
                OwnerRead::ModelAccessPage { collection, .. },
                OwnerData::ModelAccessPage {
                    collection: actual,
                    items,
                    next,
                    ..
                },
            ) if collection == actual && collection == self.collection => {
                if items.len() > 16
                    || items.iter().any(|item| !record_in(item, collection))
                    || !items.windows(2).all(|pair| pair[0].id() < pair[1].id())
                    || next.as_ref().is_some_and(|cursor| {
                        cursor.collection != collection
                            || cursor.source_hash != basis.source_hash
                            || cursor.config_id != basis.config_id
                            || cursor.at_revision != basis.revision
                            || items
                                .last()
                                .is_none_or(|last| last.id() != &cursor.after_id)
                    })
                {
                    return Err("Invalid bounded metadata page".into());
                }
                self.items = items;
                self.next = next;
                self.selected = 0;
                if self.rebase
                    && let Some(form) = &mut self.form
                {
                    form.basis = basis.clone();
                    self.notice =
                        "Current source loaded; review retained values before submitting".into();
                }
                self.rebase = false;
            }
            (
                OwnerRead::ModelAccessRecord { collection, id },
                OwnerData::ModelAccessRecord { record, .. },
            ) if record.id() == &id && record_in(&record, collection) => match self.action {
                RecordAction::View => {
                    self.record = Some(record);
                }
                RecordAction::Replace => {
                    if !basis.writable {
                        return Err("Read-only metadata record".into());
                    }
                    if let ModelAccessRecord::Pool { pool } = record {
                        self.begin_pool_setup(basis.clone(), Some(pool))?;
                    } else {
                        self.form = Some(Form::replace(record, basis.clone())?);
                        self.editing = true;
                        self.notice =
                            "Replace form loaded from the actual record; identity is fixed".into();
                    }
                }
                RecordAction::Remove => {
                    if !basis.writable {
                        return Err("Read-only metadata record".into());
                    }
                    self.confirmation = Some((record, basis.clone()));
                    self.notice = "Confirm removal with Enter; Esc cancels. Dependencies are enforced by the engine".into();
                }
            },
            _ => return Err("Metadata response context mismatch".into()),
        }
        self.basis = Some(basis);
        Ok(())
    }

    pub fn pending_label(&self) -> Option<String> {
        self.operation
            .as_ref()
            .map(|(id, hash)| format!("Original request: {} {hash}", id.as_str()))
    }

    fn setup_lines(&self) -> Vec<String> {
        if self.pinned || self.editing || self.confirmation.is_some() || self.pending.is_some() {
            return Vec::new();
        }
        if self.collection == ModelAccessCollection::Accounts {
            return vec![
                "Account setup | a Sign in to a provider | 3 Account pools".into(),
                "Advanced: n Create reference-only account | 2 Connections | existing managed connection: F2".into(),
            ];
        }
        if self.collection == ModelAccessCollection::Connections
            && self.ready
            && self.basis.as_ref().is_some_and(|basis| basis.writable)
            && matches!(self.items.get(self.selected),
                Some(ModelAccessRecord::Connection { connection })
                    if native_auth::supported(connection))
        {
            return vec!["F2 Sign in | inspect the selected native connection first".into()];
        }
        if matches!(&self.record, Some(ModelAccessRecord::Connection { connection })
            if rcli_home_supported(connection))
        {
            return vec![
                "k Private profile setup | h Advanced SDK home selection".into(),
            ];
        }
        Vec::new()
    }

    pub fn render(&self, compact: bool) -> String {
        if self.profile.is_open() {
            return self.profile_render();
        }
        if self.native_auth.is_open() {
            return self.native_auth.render(compact);
        }
        if let Some(setup) = &self.managed_setup {
            return setup.render(&self.notice, self.pending_label(), compact);
        }
        if let Some(setup) = &self.pool_setup {
            return setup.render(&self.notice, self.pending_label());
        }
        if let Some(assignment) = &self.assignment {
            return assignment.render(&self.notice, self.pending_label());
        }
        let authority = if self.ready && self.basis.as_ref().is_some_and(|basis| basis.writable) {
            "Writable: host-granted metadata sources"
        } else {
            "Read-only: no current writable grant"
        };
        let mut lines = vec![
            "Model access".into(),
            if compact {
                "Metadata only; account references are not sign-in observations".into()
            } else {
                "Metadata only; authentication is a separate explicit owner operation".into()
            },
            authority.into(),
            if compact {
                "1 Accounts | 2 Connections | 3 Pools | 4 Pinned".into()
            } else {
                "1 Accounts | 2 Connections | 3 Pools | 4 Pinned settings".into()
            },
            self.notice
                .chars()
                .take(if compact { 56 } else { 160 })
                .collect(),
        ];
        lines.extend(self.setup_lines());
        lines.extend(self.native_auth.last_authentication(self.basis.as_ref()));
        if let Some(label) = self.pending_label() {
            lines.push(label);
        }
        if let Some(index) = self.home_editor {
            let home = &self.homes[index];
            lines.push(format!(
                "SDK home | connection {}",
                home.connection.id.as_str()
            ));
            lines.push(home.value.clone());
            lines.push("Enter selects home only | Esc retains input | no credential reads".into());
        } else if let Some((record, _)) = &self.confirmation {
            lines.push(format!(
                "Remove {:?} {}?",
                self.collection,
                record.id().as_str()
            ));
            lines.push("Enter confirms removal | Esc cancels".into());
        } else if self.editing {
            lines.extend(
                self.form
                    .as_ref()
                    .expect("editing requires a form")
                    .render(compact),
            );
        } else {
            lines.push(format!(
                "{:?}: {} records on this page",
                self.collection,
                self.items.len()
            ));
            let start = self.selected.saturating_sub(2);
            for (index, record) in self.items.iter().enumerate().skip(start).take(5) {
                let unassigned = matches!(record, ModelAccessRecord::Pool { pool }
                    if pool.projects.is_empty() || pool.roles.is_empty());
                lines.push(format!(
                    "{} {}{}",
                    if index == self.selected { ">" } else { " " },
                    record.id().as_str(),
                    if unassigned { " | Unassigned" } else { "" }
                ));
            }
            if let Some(record) = &self.record {
                lines.extend(record_lines(record));
            }
            lines.push(if self.collection == ModelAccessCollection::Accounts {
                "a Sign in to a provider | n Create reference | d Remove".into()
            } else if self.collection == ModelAccessCollection::Pools {
                "a Assign | n Create | e Replace | d Remove".into()
            } else {
                "n Create | e Replace | d Remove".into()
            });
            lines.push("Enter details | Ctrl-N next | Ctrl-P refresh | x discard form".into());
        }
        lines.join("\n")
    }

    fn assert_consistent(&self) {
        if let Some(assignment) = &self.assignment {
            assignment.assert_consistent(self.pending.as_ref());
            assert!(self.pool_setup.is_none() && self.managed_setup.is_none());
        }
        if let Some(setup) = &self.pool_setup {
            setup.assert_consistent();
        }
        assert!(self.pool_setup.is_none() || self.managed_setup.is_none());
        if let Some(setup) = &self.managed_setup {
            setup.assert_consistent();
        }
        assert!(self.homes.len() <= 128);
        assert!(
            self.home_editor
                .is_none_or(|index| index < self.homes.len())
        );
        assert!(self.homes.iter().all(|home| {
            home.value.len() <= 4096
                && !home.value.chars().any(char::is_control)
                && home.selected.as_ref().is_none_or(|path| path.is_absolute())
        }));
        assert!(self.items.len() <= 16);
        assert!(if self.items.is_empty() {
            self.selected == 0
        } else {
            self.selected < self.items.len()
        });
        assert!(!self.editing || self.form.is_some());
        assert!(self.operation.is_none() || self.pending.is_some());
        assert!(!self.retry_allowed || self.pending.is_some());
        assert!(self.pending.is_none() || !self.profile.has_pending());
        if let Some(form) = &self.form {
            form.assert_consistent();
        }
    }
}

pub(super) fn rcli_home_supported(connection: &ModelConnection) -> bool {
    connection.harness.as_str() == "rcli"
        && matches!(connection.endpoint, ModelEndpoint::Http { .. })
        && matches!(
            connection.auth,
            ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}
        )
}

#[cfg(test)]
#[path = "model_access_tests.rs"]
mod tests;

/// Words the notice for a refused metadata edit.
/// Input: the engine rejection code. Output: the plain sentence for a known pool
/// refusal, else the code itself, followed by the retained-values hint.
fn rejection_notice(code: &str) -> String {
    match crate::config::plain_problem(code) {
        Some(text) => format!(
            "{}; values retained; Ctrl-P refreshes",
            text.trim_end_matches('.')
        ),
        None => format!("Engine rejected metadata: {code}; values retained; Ctrl-P refreshes"),
    }
}

fn record_in(record: &ModelAccessRecord, collection: ModelAccessCollection) -> bool {
    matches!(
        (record, collection),
        (
            ModelAccessRecord::Account { .. },
            ModelAccessCollection::Accounts
        ) | (
            ModelAccessRecord::Connection { .. },
            ModelAccessCollection::Connections
        ) | (ModelAccessRecord::Pool { .. }, ModelAccessCollection::Pools)
    )
}

fn record_lines(record: &ModelAccessRecord) -> Vec<String> {
    match record {
        ModelAccessRecord::Account { account_ref } => {
            vec![format!("Account reference: {}", account_ref.as_str())]
        }
        ModelAccessRecord::Connection { connection } => vec![
            format!(
                "Provider: {} | Harness: {} | Protocol: {}",
                connection.provider.as_str(),
                connection.harness.as_str(),
                connection.protocol.as_str()
            ),
            format!(
                "Account: {} | Model: {} | Author: {}",
                connection.account_ref.as_str(),
                connection.model.id,
                connection.model.author.as_str()
            ),
            format!(
                "Endpoint: {:?} | Auth method/reference: {:?}",
                connection.endpoint, connection.auth
            ),
        ],
        ModelAccessRecord::Pool { pool } => vec![
            format!("Member connections: {}", refs(&pool.members)),
            format!(
                "Projects: {} | Roles: {} | Fallback policy: {}",
                refs(&pool.projects),
                refs(&pool.roles),
                pool.fallback_allowed
            ),
            format!(
                "Provider quota policy: {}",
                provider_quota_policy_name(pool.provider_quota_policy)
            ),
        ],
    }
}

fn provider_quota_policy_name(policy: ProviderQuotaPolicy) -> &'static str {
    match policy {
        ProviderQuotaPolicy::BoundedAttempt => "bounded_attempt",
        ProviderQuotaPolicy::Strict => "strict",
    }
}

fn refs(values: &[CredentialRef]) -> String {
    values
        .iter()
        .map(CredentialRef::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

struct Field {
    label: &'static str,
    value: String,
    choices: &'static [&'static str],
    locked: bool,
    limit: usize,
}

struct Form {
    collection: ModelAccessCollection,
    replace: bool,
    basis: Basis,
    fields: Vec<Field>,
    selected: usize,
}

impl Form {
    fn new(collection: ModelAccessCollection, basis: Basis) -> Self {
        let definitions: Vec<(&str, &[&str], usize)> = match collection {
            ModelAccessCollection::Accounts => vec![("Account reference", &[], 256)],
            ModelAccessCollection::Connections => vec![
                ("Connection ID", &[], 256),
                ("Provider", &[], 256),
                ("Harness", &[], 256),
                ("Protocol", &[], 256),
                ("Account reference", &[], 256),
                ("Endpoint kind", &["native", "http"], 256),
                ("HTTP URL", &[], 2048),
                ("Model ID", &[], 256),
                ("Model author", &[], 256),
                (
                    "Auth method",
                    &["none", "api_key", "oauth", "native_managed"],
                    256,
                ),
                ("Credential/profile reference", &[], 256),
            ],
            ModelAccessCollection::Pools => vec![
                ("Pool ID", &[], 256),
                ("Member connection IDs", &[], 16384),
                ("Project IDs", &[], 16384),
                ("Role IDs", &[], 16384),
                ("Fallback allowed", &["false", "true"], 256),
                ("Provider quota policy", &["bounded_attempt", "strict"], 256),
                ("Pool name (optional)", &[], 256),
            ],
        };
        Self {
            collection,
            replace: false,
            basis,
            selected: 0,
            fields: definitions
                .into_iter()
                .map(|(label, choices, limit)| Field {
                    label,
                    choices,
                    limit,
                    locked: false,
                    value: choices.first().unwrap_or(&"").to_string(),
                })
                .collect(),
        }
    }

    fn replace(record: ModelAccessRecord, basis: Basis) -> Result<Self, String> {
        let (collection, values) = match record {
            ModelAccessRecord::Account { .. } => {
                return Err(
                    "Accounts have Add and Remove operations; no account replacement".into(),
                );
            }
            ModelAccessRecord::Connection { connection: c } => {
                let (kind, url) = match c.endpoint {
                    ModelEndpoint::Native {} => ("native", String::new()),
                    ModelEndpoint::Http { url } => ("http", url),
                };
                let (auth, reference) = match c.auth {
                    ModelAuthentication::None {} => ("none", String::new()),
                    ModelAuthentication::ApiKey { credential_ref } => {
                        ("api_key", credential_ref.as_str().to_string())
                    }
                    ModelAuthentication::Oauth { credential_ref } => {
                        ("oauth", credential_ref.as_str().to_string())
                    }
                    ModelAuthentication::NativeManaged { profile_ref } => {
                        ("native_managed", profile_ref.as_str().to_string())
                    }
                };
                (
                    ModelAccessCollection::Connections,
                    vec![
                        c.id.as_str().into(),
                        c.provider.as_str().into(),
                        c.harness.as_str().into(),
                        c.protocol.as_str().into(),
                        c.account_ref.as_str().into(),
                        kind.into(),
                        url,
                        c.model.id,
                        c.model.author.as_str().into(),
                        auth.into(),
                        reference,
                    ],
                )
            }
            ModelAccessRecord::Pool { pool } => (
                ModelAccessCollection::Pools,
                vec![
                    pool.id.as_str().into(),
                    refs(&pool.members),
                    refs(&pool.projects),
                    refs(&pool.roles),
                    pool.fallback_allowed.to_string(),
                    provider_quota_policy_name(pool.provider_quota_policy).into(),
                    pool.name.unwrap_or_default(),
                ],
            ),
        };
        let mut form = Self::new(collection, basis);
        assert_eq!(values.len(), form.fields.len());
        for (field, value) in form.fields.iter_mut().zip(values) {
            field.value = value;
        }
        form.fields[0].locked = true;
        form.replace = true;
        form.assert_consistent();
        Ok(form)
    }

    fn move_field(&mut self, forward: bool) {
        self.selected =
            (self.selected + if forward { 1 } else { self.fields.len() - 1 }) % self.fields.len();
    }

    fn character(&mut self, character: char) -> Result<(), String> {
        let total: usize = self.fields.iter().map(|field| field.value.len()).sum();
        let field = &mut self.fields[self.selected];
        if !field.locked && field.choices.is_empty() && !character.is_control() {
            if field.value.len() + character.len_utf8() > field.limit
                || total + character.len_utf8() > 32768
            {
                return Err("Metadata form input limit reached; values retained".into());
            }
            field.value.push(character);
        }
        Ok(())
    }

    fn backspace(&mut self) {
        let field = &mut self.fields[self.selected];
        if !field.locked && field.choices.is_empty() {
            field.value.pop();
        }
    }

    fn choose(&mut self, forward: bool) {
        let field = &mut self.fields[self.selected];
        if !field.choices.is_empty() {
            let index = field
                .choices
                .iter()
                .position(|value| *value == field.value)
                .expect("choice field must contain a declared value");
            let count = field.choices.len();
            field.value =
                field.choices[(index + if forward { 1 } else { count - 1 }) % count].to_string();
        }
    }

    fn edit(&self) -> Result<ModelAccessEdit, String> {
        let value = |index: usize| self.fields[index].value.trim().to_owned();
        let reference = |index: usize| {
            CredentialRef::new(value(index))
                .map_err(|error| format!("{}: {error}", self.fields[index].label))
        };
        let references = |index: usize| -> Result<Vec<CredentialRef>, String> {
            let values = value(index);
            let parts: Vec<_> = values.split(',').collect();
            if parts.len() > 64 {
                return Err("At most 64 references per list".into());
            }
            parts
                .into_iter()
                .map(|part| {
                    CredentialRef::new(part.trim().to_string())
                        .map_err(|error| format!("{}: {error}", self.fields[index].label))
                })
                .collect()
        };
        let edit = match self.collection {
            ModelAccessCollection::Accounts => ModelAccessEdit::AddAccount {
                account_ref: reference(0)?,
            },
            ModelAccessCollection::Connections => {
                let connection = ModelConnection {
                    id: reference(0)?,
                    provider: reference(1)?,
                    harness: reference(2)?,
                    protocol: reference(3)?,
                    account_ref: reference(4)?,
                    endpoint: if value(5) == "native" {
                        ModelEndpoint::Native {}
                    } else {
                        ModelEndpoint::Http { url: value(6) }
                    },
                    model: ModelReference {
                        id: value(7),
                        author: reference(8)?,
                    },
                    auth: match value(9).as_str() {
                        "none" => ModelAuthentication::None {},
                        "api_key" => ModelAuthentication::ApiKey {
                            credential_ref: reference(10)?,
                        },
                        "oauth" => ModelAuthentication::Oauth {
                            credential_ref: reference(10)?,
                        },
                        "native_managed" => ModelAuthentication::NativeManaged {
                            profile_ref: reference(10)?,
                        },
                        _ => unreachable!(),
                    },
                };
                if self.replace {
                    ModelAccessEdit::ReplaceConnection { connection }
                } else {
                    ModelAccessEdit::CreateConnection { connection }
                }
            }
            ModelAccessCollection::Pools => {
                let pool = AccountPool {
                    id: reference(0)?,
                    name: Some(value(6)).filter(|name| !name.is_empty()),
                    members: references(1)?,
                    projects: references(2)?,
                    roles: references(3)?,
                    fallback_allowed: value(4) == "true",
                    provider_quota_policy: match value(5).as_str() {
                        "bounded_attempt" => ProviderQuotaPolicy::BoundedAttempt,
                        "strict" => ProviderQuotaPolicy::Strict,
                        _ => return Err("Unsupported provider quota policy".into()),
                    },
                };
                if self.replace {
                    ModelAccessEdit::ReplacePool { pool }
                } else {
                    ModelAccessEdit::CreatePool { pool }
                }
            }
        };
        edit.validate().map_err(|error| error.to_string())?;
        Ok(edit)
    }

    fn render(&self, compact: bool) -> Vec<String> {
        let mut lines = vec![format!(
            "{} {:?} | field {}/{}",
            if self.replace {
                "Replace"
            } else {
                "Add/Create"
            },
            self.collection,
            self.selected + 1,
            self.fields.len()
        )];
        for (index, field) in self
            .fields
            .iter()
            .enumerate()
            .skip(if compact {
                self.selected
            } else {
                self.selected.saturating_sub(2)
            })
            .take(if compact { 2 } else { 5 })
        {
            let tail = field
                .value
                .chars()
                .rev()
                .take(if compact { 16 } else { 64 })
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let displayed = if tail.len() < field.value.len() {
                format!("…{tail}")
            } else {
                tail
            };
            lines.push(format!(
                "{} {}{}: {}",
                if index == self.selected { ">" } else { " " },
                field.label,
                if field.locked { " (fixed)" } else { "" },
                displayed
            ));
        }
        if self.collection == ModelAccessCollection::Pools {
            lines.push(
                "Strict blocks unknown provider quota; numeric quota is currently unavailable."
                    .into(),
            );
            lines.push(
                "Bounded attempt still requires fresh readiness and execution admission.".into(),
            );
        }
        lines.push("Reference IDs only; never paste keys/tokens".into());
        lines.push("Tab fields | arrows choices | Enter save | Esc retain".into());
        lines
    }

    fn assert_consistent(&self) {
        assert!(!self.fields.is_empty() && self.fields.len() <= 11);
        assert!(self.selected < self.fields.len());
        assert!(
            self.fields
                .iter()
                .map(|field| field.value.len())
                .sum::<usize>()
                <= 32768
        );
        for field in &self.fields {
            assert!(field.value.len() <= field.limit);
            assert!(!field.value.chars().any(char::is_control));
            assert!(field.choices.is_empty() || field.choices.contains(&field.value.as_str()));
        }
    }
}

#[cfg(test)]
mod quota_policy_tests {
    use super::*;
    use crate::client_controller::OwnerController;
    use crate::engine::Client;
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::{OwnerClientKind, OwnerMutation};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    async fn owner_basis() -> (
        tempfile::TempDir,
        EngineService,
        OwnerController,
        tokio::sync::watch::Sender<bool>,
        Basis,
    ) {
        let root = tempfile::Builder::new()
            .prefix("ca-tui563-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        std::fs::write(&global, "version = 1\n[model_access]\naccounts = []\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "engine.sock",
            &global,
            &project,
            &stopped,
        )
        .await
        .unwrap();
        let (_, mut owner) = OwnerController::attach_configured(
            Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap(),
            OwnerClientKind::Tui,
        )
        .await
        .unwrap();
        let page = owner
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 16,
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessPage {
            source_hash,
            writable,
            ..
        } = page.data
        else {
            panic!("actual pool page required")
        };
        let basis = Basis {
            identity: owner.identity().clone(),
            revision: page.revision,
            config_id: page.config_id,
            source_hash,
            writable,
        };
        assert!(basis.writable);
        (root, service, owner, stop, basis)
    }

    fn pool_value(policy: Option<&str>) -> Value {
        let mut pool = json!({
            "id": "quota-pool",
            "members": ["connection-1"],
            "projects": ["project-1"],
            "roles": ["leader"],
            "fallback_allowed": false
        });
        if let Some(policy) = policy {
            pool["provider_quota_policy"] = json!(policy);
        }
        pool
    }

    fn pool_form(basis: Basis) -> Form {
        let mut form = Form::new(ModelAccessCollection::Pools, basis);
        for (label, value) in [
            ("Pool ID", "quota-pool"),
            ("Member connection IDs", "connection-1"),
            ("Project IDs", "project-1"),
            ("Role IDs", "leader"),
        ] {
            form.selected = form
                .fields
                .iter()
                .position(|field| field.label == label)
                .unwrap();
            for character in value.chars() {
                form.character(character).unwrap();
            }
        }
        form.assert_consistent();
        form
    }

    fn policy_index(form: &Form) -> usize {
        let index = form
            .fields
            .iter()
            .position(|field| field.choices.contains(&"bounded_attempt"))
            .expect("pool form must expose provider_quota_policy bounded_attempt/strict choices");
        assert_eq!(form.fields[index].choices, &["bounded_attempt", "strict"]);
        assert!(!form.fields[index].locked);
        index
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_quota_default_is_visible_and_omitted_from_create() {
        let (_root, mut service, owner, _stop, basis) = owner_basis().await;
        let mut form = pool_form(basis);
        let index = policy_index(&form);
        assert_eq!(form.fields[index].value, "bounded_attempt");
        form.selected = index;
        assert!(form.render(false).join("\n").contains("bounded_attempt"));
        let ModelAccessEdit::CreatePool { pool } = form.edit().unwrap() else {
            panic!("actual create-pool edit required")
        };
        assert_eq!(serde_json::to_value(pool).unwrap(), pool_value(None));
        drop(owner);
        service.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_quota_choices_are_closed_and_invalid_edits_are_rejected() {
        let (_root, mut service, owner, _stop, basis) = owner_basis().await;
        let mut form = pool_form(basis);
        let index = policy_index(&form);
        form.selected = index;
        form.character('x').unwrap();
        form.backspace();
        assert_eq!(form.fields[index].value, "bounded_attempt");
        for (forward, expected) in [
            (true, "strict"),
            (true, "bounded_attempt"),
            (false, "strict"),
            (false, "bounded_attempt"),
        ] {
            form.choose(forward);
            assert_eq!(form.fields[index].value, expected);
            let ModelAccessEdit::CreatePool { pool } = form.edit().unwrap() else {
                panic!("actual create-pool edit required")
            };
            let policy = if expected == "strict" {
                Some("strict")
            } else {
                None
            };
            assert_eq!(serde_json::to_value(pool).unwrap(), pool_value(policy));
        }
        for unsupported in ["", "unlimited", "available", "Strict"] {
            form.fields[index].value = unsupported.into();
            assert!(
                form.edit().is_err(),
                "unsupported quota policy {unsupported:?}"
            );
        }
        drop(owner);
        service.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_quota_inspect_and_replace_preserve_policy_and_other_fields() {
        let (_root, mut service, owner, _stop, basis) = owner_basis().await;
        for (encoded, effective) in [
            (None, "bounded_attempt"),
            (Some("bounded_attempt"), "bounded_attempt"),
            (Some("strict"), "strict"),
        ] {
            let record: ModelAccessRecord = serde_json::from_value(json!({
                "kind": "pool",
                "pool": pool_value(encoded)
            }))
            .unwrap();
            assert!(record_lines(&record).join("\n").contains(effective));
            let mut form = Form::replace(record, basis.clone()).unwrap();
            let index = policy_index(&form);
            assert!(form.fields[0].locked);
            assert_eq!(form.fields[index].value, effective);
            form.selected = index;
            let rendered = form.render(false).join("\n").to_lowercase();
            assert!(rendered.contains(effective));
            assert!(rendered.contains("strict") && rendered.contains("quota"));
            assert!(rendered.contains("unknown") || rendered.contains("unavailable"));
            let ModelAccessEdit::ReplacePool { pool } = form.edit().unwrap() else {
                panic!("actual replace-pool edit required")
            };
            let policy = if effective == "strict" {
                Some("strict")
            } else {
                None
            };
            assert_eq!(serde_json::to_value(pool).unwrap(), pool_value(policy));
        }
        drop(owner);
        service.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_quota_rebase_retains_unsaved_choices_after_actual_owner_refresh() {
        let (_root, mut service, mut owner, _stop, basis) = owner_basis().await;
        let mut drafts = Vec::new();
        for policy in ["bounded_attempt", "strict"] {
            let mut form = pool_form(basis.clone());
            let index = policy_index(&form);
            form.selected = index;
            if policy == "strict" {
                form.choose(true);
            }
            assert_eq!(form.fields[index].value, policy);
            drafts.push((policy, form));
        }
        owner
            .begin_mutation(
                OwnerMutation::ModelAccess {
                    expected_config_id: basis.config_id.clone(),
                    expected_source_hash: basis.source_hash.clone(),
                    edit: ModelAccessEdit::AddAccount {
                        account_ref: CredentialRef::new("quota-rebase-account").unwrap(),
                    },
                },
                1,
            )
            .unwrap();
        assert!(matches!(
            owner.send_pending().await.unwrap(),
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ));
        for (policy, form) in drafts {
            let retained_fields: Vec<_> = form
                .fields
                .iter()
                .map(|field| (field.label, field.value.clone()))
                .collect();
            let selected = form.selected;
            let mut state = State::default();
            state.access(Some(owner.identity().clone()), true);
            state.collection = ModelAccessCollection::Pools;
            state.basis = Some(basis.clone());
            state.needs_page = false;
            state.form = Some(form);
            state.reduce(&UiEvent::HistoryFirst).unwrap();
            assert!(state.rebase);
            let Job::Read {
                identity,
                generation,
                query,
            } = state.request(true).unwrap().unwrap()
            else {
                panic!("actual first-page refresh required")
            };
            let reply = owner.read(query.clone()).await.unwrap();
            assert!(reply.revision.value() > basis.revision.value());
            assert_ne!(reply.config_id, basis.config_id);
            let OwnerData::ModelAccessPage {
                source_hash,
                writable,
                items,
                ..
            } = &reply.data
            else {
                panic!("actual refreshed pool page required")
            };
            assert!(items.is_empty());
            assert_ne!(source_hash, &basis.source_hash);
            let current = Basis {
                identity: identity.clone(),
                revision: reply.revision.clone(),
                config_id: reply.config_id.clone(),
                source_hash: source_hash.clone(),
                writable: *writable,
            };
            state
                .apply(Event::Read {
                    identity,
                    generation,
                    query,
                    result: Ok(Box::new(reply)),
                })
                .unwrap();
            assert!(!state.rebase);
            assert!(state.pending.is_none());
            let form = state.form.as_ref().unwrap();
            assert_eq!(form.basis, current);
            assert_eq!(form.selected, selected);
            assert_eq!(
                form.fields
                    .iter()
                    .map(|field| (field.label, field.value.clone()))
                    .collect::<Vec<_>>(),
                retained_fields
            );
            let ModelAccessEdit::CreatePool { pool } = form.edit().unwrap() else {
                panic!("retained actual create-pool edit required")
            };
            let encoded = if policy == "strict" {
                Some("strict")
            } else {
                None
            };
            assert_eq!(serde_json::to_value(pool).unwrap(), pool_value(encoded));
        }
        drop(owner);
        service.shutdown().await.unwrap();
    }
}
