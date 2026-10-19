use super::app::UiEvent;
use super::client::MutationDelivery;
use crate::config::{
    AccountPool, ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint,
    ModelReference,
};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    ModelAccessCollection, ModelAccessCursor, ModelAccessRecord, OwnerData, OwnerRead,
    OwnerReadReply, OwnerResponse, OwnerResult,
};
use crate::protocol::{ConfigId, CredentialRef, Cursor, RequestId};

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

#[derive(Clone)]
pub(super) enum Job {
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
    pending: Option<Submission>,
    operation: Option<(RequestId, String)>,
    retry_allowed: bool,
    job: Option<Job>,
    pub notice: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
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
        self.assert_consistent();
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn loading(&self) -> bool {
        self.flight.is_some() || self.job.is_some()
    }

    pub fn disconnected(&self) -> bool {
        !self.ready
    }

    pub fn request(&mut self, active: bool) -> Result<Option<Job>, String> {
        if !self.ready {
            return Ok(None);
        }
        if self.job.is_none()
            && active
            && !self.pinned
            && self.needs_page
            && !self.loading()
            && self.pending.is_none()
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
                if matches!(event, UiEvent::Character('a'))
                    != (self.collection == ModelAccessCollection::Accounts)
                {
                    self.notice = "Accounts use a; connections and pools use n".into();
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
                                    self.notice = "Metadata saved; authentication and execution remain unavailable".into();
                                    if !matches!(
                                        submission.edit,
                                        ModelAccessEdit::RemoveAccount { .. }
                                            | ModelAccessEdit::RemoveConnection { .. }
                                            | ModelAccessEdit::RemovePool { .. }
                                    ) {
                                        self.form = None;
                                        self.editing = false;
                                    }
                                    self.confirmation = None;
                                }
                                OwnerResult::Rejected { code } => {
                                    self.notice = format!(
                                        "Engine rejected metadata: {code}; values retained; Ctrl-P refreshes"
                                    );
                                    self.confirmation = None;
                                }
                                OwnerResult::VerificationUnavailable { .. } => {
                                    return Err(
                                        "Unexpected verification result for metadata".into()
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
                    self.form = Some(Form::replace(record, basis.clone())?);
                    self.editing = true;
                    self.notice =
                        "Replace form loaded from the actual record; identity is fixed".into();
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

    pub fn render(&self, compact: bool) -> String {
        let authority = if self.ready && self.basis.as_ref().is_some_and(|basis| basis.writable) {
            "Writable: host-granted metadata sources"
        } else {
            "Read-only: no current writable grant"
        };
        let mut lines = vec![
            "Model access".into(),
            if compact {
                "Metadata only; authentication unavailable".into()
            } else {
                "Metadata only; authentication and native execution unavailable".into()
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
        if let Some(label) = self.pending_label() {
            lines.push(label);
        }
        if let Some((record, _)) = &self.confirmation {
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
                lines.push(format!(
                    "{} {}",
                    if index == self.selected { ">" } else { " " },
                    record.id().as_str()
                ));
            }
            if let Some(record) = &self.record {
                lines.extend(record_lines(record));
            }
            lines.push(if self.collection == ModelAccessCollection::Accounts {
                "a Add account | d Remove".into()
            } else {
                "n Create | e Replace | d Remove".into()
            });
            lines.push("Enter details | Ctrl-N next | Ctrl-P refresh | x discard form".into());
        }
        lines.join("\n")
    }

    fn assert_consistent(&self) {
        assert!(self.items.len() <= 16);
        assert!(if self.items.is_empty() {
            self.selected == 0
        } else {
            self.selected < self.items.len()
        });
        assert!(!self.editing || self.form.is_some());
        assert!(self.operation.is_none() || self.pending.is_some());
        assert!(!self.retry_allowed || self.pending.is_some());
        if let Some(form) = &self.form {
            form.assert_consistent();
        }
    }
}

#[cfg(test)]
#[path = "model_access_tests.rs"]
mod tests;

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
        ],
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
                    members: references(1)?,
                    projects: references(2)?,
                    roles: references(3)?,
                    fallback_allowed: value(4) == "true",
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
