use super::app::UiEvent;
use super::model_access::Basis;
use crate::adapters::{claude, codex};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    ModelAccessRecord, NativeAuthAccount, NativeAuthAction, NativeAuthCancel, NativeAuthFact,
    NativeAuthLogout, NativeAuthMethod, NativeAuthPhase, NativeAuthQuery, NativeAuthReply,
    NativeAuthScope, NativeAuthStart, OwnerData, OwnerReadReply,
};
use crate::protocol::RequestId;

pub(super) mod claude_code;

const DISPLAY_BYTES_MAX: usize = 65536;
const FIELD_BYTES_MAX: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Context {
    pub basis: Basis,
    pub connection: ModelConnection,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Inspect,
    Start(NativeAuthStart),
    Read(NativeAuthQuery),
    Cancel(NativeAuthCancel),
    Logout(NativeAuthLogout),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Job {
    pub context: Context,
    pub command: Command,
}

pub(super) enum Output {
    Inspected(Box<OwnerReadReply>),
    Auth(Box<NativeAuthReply>),
}

pub(super) struct Event {
    pub job: Job,
    pub result: Result<Output, String>,
}

struct Retained {
    original: Job,
    fact: Option<NativeAuthFact>,
    uncertain: bool,
}

pub(super) struct State {
    claude_code: claude_code::Input,
    context: Option<Context>,
    retained: Option<Retained>,
    flight: Option<Job>,
    job: Option<Job>,
    presentation: Option<NativeAuthReply>,
    generation: u64,
    connected: bool,
    open: bool,
    inspected: bool,
    selected: usize,
    confirmation: Option<bool>,
    scroll: u16,
    notice: &'static str,
    polled: Option<std::time::Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            claude_code: claude_code::Input::default(),
            context: None,
            retained: None,
            flight: None,
            job: None,
            presentation: None,
            generation: 0,
            connected: false,
            open: false,
            inspected: false,
            selected: 0,
            confirmation: None,
            scroll: 0,
            notice: "Select a managed native connection",
            polled: None,
        }
    }
}

impl Command {
    fn request_id(&self) -> Option<&RequestId> {
        match self {
            Self::Inspect => None,
            Self::Start(request) => Some(&request.request_id),
            Self::Read(request) => Some(&request.request_id),
            Self::Cancel(request) => Some(&request.request_id),
            Self::Logout(request) => Some(&request.request_id),
        }
    }

    fn operation_id(&self) -> Option<&RequestId> {
        match self {
            Self::Inspect => None,
            Self::Start(request) => Some(&request.request_id),
            Self::Read(request) => Some(&request.operation_id),
            Self::Cancel(request) => Some(&request.operation_id),
            Self::Logout(request) => Some(&request.request_id),
        }
    }

    fn scope(&self) -> Option<&NativeAuthScope> {
        match self {
            Self::Inspect => None,
            Self::Start(request) => Some(&request.scope),
            Self::Read(request) => Some(&request.scope),
            Self::Cancel(request) => Some(&request.scope),
            Self::Logout(request) => Some(&request.scope),
        }
    }

    fn action(&self) -> Option<NativeAuthAction> {
        match self {
            Self::Start(request) => Some(NativeAuthAction::SignIn {
                method: request.method,
            }),
            Self::Logout(_) => Some(NativeAuthAction::Logout),
            Self::Inspect | Self::Read(_) | Self::Cancel(_) => None,
        }
    }
}

impl Context {
    pub(super) fn scope(&self) -> NativeAuthScope {
        NativeAuthScope {
            expected_database_instance: self.basis.identity.database_instance().into(),
            connection: self.connection.id.clone(),
            expected_config_id: self.basis.config_id.clone(),
            expected_source_hash: self.basis.source_hash.clone(),
        }
    }
}

impl State {
    pub(super) fn open(&mut self, basis: Basis, connection: ModelConnection) -> Result<(), String> {
        self.hide()?;
        self.flight = None;
        self.job = None;
        self.inspected = false;
        self.context = None;
        if !basis.writable || !supported(&connection) {
            return Err("Select a writable managed Codex or Claude native connection".into());
        }
        let context = Context {
            basis,
            connection,
            generation: self.generation,
        };
        self.context = Some(context.clone());
        self.connected = true;
        self.open = true;
        self.selected = 0;
        self.scroll = 0;
        self.notice = "Reading the exact selected connection; no sign-in requested";
        self.queue(Job {
            context,
            command: Command::Inspect,
        })
    }

    pub(super) fn access(&mut self, identity: Option<OwnerConnectionIdentity>, connected: bool) {
        self.flight = None;
        self.job = None;
        self.inspected = false;
        self.connected = connected && identity.is_some();
        if self.hide().is_err() {
            self.connected = false;
        }
        if !self.connected {
            self.notice = "Owner disconnected; original operation retained; Ctrl-R reconnects";
            return;
        }
        let (Some(identity), Some(retained)) = (identity, self.retained.as_ref()) else {
            return;
        };
        if retained.original.context.basis.identity.database_instance()
            != identity.database_instance()
        {
            self.notice = "Owner database changed; original operation cannot be rebound";
            return;
        }
        let mut context = retained.original.context.clone();
        context.basis.identity = identity;
        context.generation = self.generation;
        self.notice = match self
            .query(false)
            .and_then(|command| self.queue(Job { context, command }))
        {
            Ok(()) => "Reconnected; reading the exact original operation",
            Err(_) => "Original operation retained; owner reconciliation unavailable",
        };
    }

    pub(super) fn hide(&mut self) -> Result<(), String> {
        self.open = false;
        self.claude_code.clear();
        self.presentation = None;
        self.confirmation = None;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Native authentication view generation exhausted")?;
        Ok(())
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn operation_id(&self) -> Option<&RequestId> {
        self.retained
            .as_ref()
            .and_then(|r| r.original.command.operation_id())
    }

    pub(super) fn has_pending(&self) -> bool {
        self.retained.as_ref().is_some_and(|retained| {
            retained.uncertain
                || retained.fact.as_ref().is_none_or(|fact| {
                    matches!(
                        fact.phase,
                        NativeAuthPhase::Starting
                            | NativeAuthPhase::AwaitingOwner
                            | NativeAuthPhase::CheckingAccount
                            | NativeAuthPhase::Cancelling
                            | NativeAuthPhase::Uncertain
                    )
                })
        })
    }

    /// Re-reads the original operation every two seconds while sign-in is still moving, so
    /// the device code, browser link and final account appear without a manual read.
    /// Input: the current time. Output: whether a read was queued; never while a Claude
    /// code is being typed, a confirmation is open or another request is in flight.
    pub(super) fn poll_pending(&mut self, now: std::time::Instant) -> bool {
        if !self.open
            || !self.connected
            || !self.inspected
            || self.loading()
            || self.confirmation.is_some()
            || self.claude_code.accepts_code()
        {
            return false;
        }
        let moving = self
            .retained
            .as_ref()
            .and_then(|retained| retained.fact.as_ref())
            .is_some_and(|fact| {
                matches!(
                    fact.phase,
                    NativeAuthPhase::Starting
                        | NativeAuthPhase::AwaitingOwner
                        | NativeAuthPhase::CheckingAccount
                        | NativeAuthPhase::Cancelling
                )
            });
        if !moving
            || self
                .polled
                .is_some_and(|last| now < last + std::time::Duration::from_secs(2))
        {
            return false;
        }
        self.polled = Some(now);
        self.enqueue_query(false).is_ok()
    }

    pub(super) fn loading(&self) -> bool {
        self.flight.is_some() || self.job.is_some() || self.claude_code.loading()
    }

    pub(super) fn request(&mut self) -> Option<Job> {
        self.assert_consistent();
        if !self.connected {
            return None;
        }
        self.job.take()
    }

    fn queue(&mut self, job: Job) -> Result<(), String> {
        if !self.connected || self.loading() {
            return Err("Wait for the current owner request or reconnect".into());
        }
        self.flight = Some(job.clone());
        self.job = Some(job);
        self.assert_consistent();
        Ok(())
    }

    pub(super) fn reduce(&mut self, event: &UiEvent) -> Result<bool, String> {
        if !self.open {
            return Ok(false);
        }
        if self.selected == 0
            && self.confirmation.is_none()
            && matches!(
                event,
                UiEvent::Character(_) | UiEvent::Backspace | UiEvent::Submit
            )
            && self.connected
            && self.inspected
            && self.flight.is_none()
            && self.job.is_none()
            && self.claude_code.reduce(event)?
        {
            return Ok(true);
        }
        match event {
            UiEvent::Cancel | UiEvent::FocusNext | UiEvent::FocusPrevious => {
                self.hide()?;
                return Ok(matches!(event, UiEvent::Cancel));
            }
            UiEvent::Resize(..) => return Ok(false),
            UiEvent::Scroll(delta) => {
                self.scroll = if *delta < 0 {
                    self.scroll.saturating_sub(delta.unsigned_abs())
                } else {
                    self.scroll.saturating_add(*delta as u16)
                };
            }
            UiEvent::SelectionNext | UiEvent::SelectionPrevious => {
                if let Some(confirm) = &mut self.confirmation {
                    *confirm = !*confirm;
                } else {
                    let count = self.choices().len();
                    let step = if matches!(event, UiEvent::SelectionNext) {
                        1
                    } else {
                        count - 1
                    };
                    self.selected = (self.selected + step) % count;
                }
            }
            UiEvent::Submit => self.submit()?,
            _ => {}
        }
        self.assert_consistent();
        Ok(true)
    }

    fn choices(&self) -> &'static [&'static str] {
        if self
            .context
            .as_ref()
            .is_some_and(|c| c.connection.harness.as_str() == "claude")
        {
            if self.claude_code.accepts_code() {
                return &[
                    "Submit Claude fallback code",
                    "Read original operation",
                    "Cancel original operation",
                    "Logout",
                    "Back",
                ];
            }
            &[
                "Claude official sign-in",
                "Read original operation",
                "Cancel original operation",
                "Logout",
                "Back",
            ]
        } else {
            &[
                "Codex browser sign-in",
                "Codex device-code sign-in",
                "Read original operation",
                "Cancel original operation",
                "Logout",
                "Use the existing Codex sign-in (no new code)",
                "Back",
            ]
        }
    }

    fn submit(&mut self) -> Result<(), String> {
        if !self.connected || !self.inspected || self.loading() {
            return Err(
                "A fresh selected-connection inspection and idle owner are required".into(),
            );
        }
        if let Some(confirm) = self.confirmation.take() {
            return if confirm { self.begin(None) } else { Ok(()) };
        }
        let claude = self
            .context
            .as_ref()
            .is_some_and(|c| c.connection.harness.as_str() == "claude");
        match (claude, self.selected) {
            (true, 0) => self.begin(Some(NativeAuthMethod::ClaudeOfficial)),
            (false, 0) => self.begin(Some(NativeAuthMethod::CodexBrowser)),
            (false, 1) => self.begin(Some(NativeAuthMethod::CodexDeviceCode)),
            (true, 1) | (false, 2) => self.enqueue_query(false),
            (true, 2) | (false, 3) => self.enqueue_query(true),
            (true, 3) | (false, 4) => {
                self.confirmation = Some(false);
                Ok(())
            }
            (false, 5) => self.begin(Some(NativeAuthMethod::CodexExisting)),
            _ => self.hide(),
        }
    }

    fn begin(&mut self, method: Option<NativeAuthMethod>) -> Result<(), String> {
        if self.has_pending() {
            return Err("Original outcome unresolved; read or cancel that operation".into());
        }
        let context = self.context.clone().ok_or("Select a native connection")?;
        let scope = context.scope();
        let request_id = new_request_id()?;
        let command = match method {
            Some(method) => Command::Start(NativeAuthStart {
                request_id,
                scope,
                method,
            }),
            None => Command::Logout(NativeAuthLogout { request_id, scope }),
        };
        let job = Job { context, command };
        self.queue(job.clone())?;
        self.retained = Some(Retained {
            original: job,
            fact: None,
            uncertain: true,
        });
        self.presentation = None;
        self.notice = "Owner request pending; retain its original identity until reconciled";
        self.assert_consistent();
        Ok(())
    }

    fn query(&self, cancel: bool) -> Result<Command, String> {
        let retained = self
            .retained
            .as_ref()
            .ok_or("No original authentication operation")?;
        let scope = retained
            .original
            .command
            .scope()
            .ok_or("No original operation scope")?
            .clone();
        let operation_id = self
            .operation_id()
            .ok_or("No original operation identity")?
            .clone();
        let request_id = new_request_id()?;
        Ok(if cancel {
            Command::Cancel(NativeAuthCancel {
                request_id,
                scope,
                operation_id,
            })
        } else {
            Command::Read(NativeAuthQuery {
                request_id,
                scope,
                operation_id,
            })
        })
    }

    fn enqueue_query(&mut self, cancel: bool) -> Result<(), String> {
        let context = self
            .context
            .clone()
            .ok_or("Select the original native connection")?;
        let retained = self
            .retained
            .as_ref()
            .ok_or("No original authentication operation")?;
        if context.connection != retained.original.context.connection
            || context.scope() != retained.original.context.scope()
        {
            return Err("Select the original connection and source to read or cancel".into());
        }
        let command = self.query(cancel)?;
        self.queue(Job { context, command })?;
        if cancel {
            self.claude_code.clear();
        }
        Ok(())
    }

    pub(super) fn apply(&mut self, event: Event) -> Result<(), String> {
        if self.flight.as_ref() != Some(&event.job) {
            return Ok(());
        }
        match event.result {
            Ok(Output::Auth(reply)) if !matches!(event.job.command, Command::Inspect) => {
                if !self.matches_reply(&event.job, &reply) {
                    return Ok(());
                }
                self.accept_reply(&event.job, *reply);
            }
            Ok(Output::Inspected(reply)) if matches!(event.job.command, Command::Inspect) => {
                self.inspected = self.matches_inspection(&event.job, &reply);
                self.notice = if self.inspected {
                    "Choose an explicit authentication action"
                } else {
                    "Selected connection or source changed; close and inspect again"
                };
            }
            Err(problem) => {
                self.presentation = None;
                self.claude_code.clear();
                self.notice = public_problem(&problem);
                if problem != "native_auth_not_active"
                    && let Some(retained) = &mut self.retained
                {
                    retained.uncertain = true;
                }
                if matches!(event.job.command, Command::Inspect) {
                    self.inspected = false;
                }
            }
            _ => {
                self.presentation = None;
                self.claude_code.clear();
                self.inspected = false;
                self.notice = "Owner response context changed; close and inspect again";
            }
        }
        self.flight = None;
        self.job = None;
        self.assert_consistent();
        Ok(())
    }

    fn matches_inspection(&self, job: &Job, reply: &OwnerReadReply) -> bool {
        let context = &job.context;
        self.open
            && self.context.as_ref() == Some(context)
            && reply.config_id == context.basis.config_id
            && reply.revision.value() >= context.basis.revision.value()
            && matches!(&reply.data, OwnerData::ModelAccessRecord {
                source_hash, writable: true,
                record: ModelAccessRecord::Connection { connection },
            } if source_hash == &context.basis.source_hash && connection == &context.connection)
    }

    fn matches_reply(&self, job: &Job, reply: &NativeAuthReply) -> bool {
        let Some(retained) = &self.retained else {
            return false;
        };
        let Some(scope) = job.command.scope() else {
            return false;
        };
        let fact = &reply.fact;
        let original = &retained.original;
        job.command.request_id() == Some(&reply.request_id)
            && job.command.operation_id() == Some(&fact.operation_id)
            && original.command.operation_id() == Some(&fact.operation_id)
            && original.command.scope() == Some(scope)
            && original.command.action() == Some(fact.action.clone())
            && reply.database_instance == scope.expected_database_instance
            && reply.database_instance == job.context.basis.identity.database_instance()
            && reply.engine_epoch == job.context.basis.identity.engine_epoch()
            && fact.binding.database_instance == reply.database_instance
            && fact.binding.engine_epoch == original.context.basis.identity.engine_epoch()
            && fact.binding.config_id == scope.expected_config_id
            && fact.binding.source_hash == scope.expected_source_hash
            && fact.binding.connection == original.context.connection
            && fact.binding.at_revision.value() >= original.context.basis.revision.value()
            && fact.binding.custody_id.len() == 64
            && fact
                .binding
                .custody_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && fact.generation > 0
            && retained.fact.as_ref().is_none_or(|pinned| {
                pinned.generation == fact.generation && pinned.binding == fact.binding
            })
    }

    fn accept_reply(&mut self, job: &Job, reply: NativeAuthReply) {
        let retained = self
            .retained
            .as_mut()
            .expect("matching reply retains its original operation");
        retained.uncertain = reply.fact.phase == NativeAuthPhase::Uncertain;
        retained.fact = Some(reply.fact.clone());
        self.notice = if reply.fact.binding.engine_epoch == reply.engine_epoch {
            phase_text(reply.fact.phase)
        } else {
            "Historical operation observed; current account presentation unavailable"
        };
        let present = self.open
            && self.connected
            && self.context.as_ref() == Some(&job.context)
            && reply.fact.binding.engine_epoch == reply.engine_epoch;
        if present {
            self.observe_claude(&reply);
        } else {
            self.claude_code.clear();
        }
        self.presentation = if present { Some(reply) } else { None };
    }

    pub(super) fn last_authentication(&self, basis: Option<&Basis>) -> Vec<String> {
        let (Some(retained), Some(operation)) = (&self.retained, self.operation_id()) else {
            return Vec::new();
        };
        let original = &retained.original.context;
        let generation = retained
            .fact
            .as_ref()
            .map(|fact| fact.generation.to_string())
            .unwrap_or_else(|| "not observed".into());
        let phase = retained
            .fact
            .as_ref()
            .map(|fact| phase_text(fact.phase))
            .unwrap_or("not observed");
        let action = retained
            .original
            .command
            .action()
            .map(action_text)
            .unwrap_or_default();
        let current = self.connected
            && basis.is_some_and(|basis| {
                basis.identity.database_instance() == original.basis.identity.database_instance()
                    && basis.identity.engine_epoch() == original.basis.identity.engine_epoch()
                    && basis.config_id == original.basis.config_id
                    && basis.source_hash == original.basis.source_hash
            });
        let mut lines = vec![
            format!(
                "Last authentication | {} | account reference: {}",
                display_text(original.connection.id.as_str(), FIELD_BYTES_MAX),
                display_text(original.connection.account_ref.as_str(), FIELD_BYTES_MAX)
            ),
            format!(
                "Original operation: {} | generation: {generation}",
                display_text(operation.as_str(), 256)
            ),
            format!("Original action: {action} | Last owner phase: {phase}"),
            format!(
                "Original owner: {} | epoch: {}",
                display_text(original.basis.identity.database_instance(), 256),
                display_text(original.basis.identity.engine_epoch(), 256)
            ),
            format!(
                "Original config: {} | source: {}",
                display_text(original.basis.config_id.as_str(), 256),
                display_text(&original.basis.source_hash, 128)
            ),
        ];
        if !current {
            lines.push("Historical owner/source; read the exact original operation".into());
        }
        if retained.uncertain {
            lines.push("Current outcome unresolved; read or cancel the original operation".into());
        }
        lines.push("Last owner observation only; model delivery and quota remain separate".into());
        lines
    }

    pub(super) fn render(&self, compact: bool) -> String {
        if !self.open {
            return String::new();
        }
        let Some(context) = &self.context else {
            return String::new();
        };
        let header = display_text(context.connection.id.as_str(), FIELD_BYTES_MAX);
        let mut lines = vec![
            format!(
                "Native authentication | {}",
                if context.connection.harness.as_str() == "claude" {
                    "Claude"
                } else {
                    "Codex"
                }
            ),
            self.notice.into(),
            format!(
                "Selected account reference: {}",
                display_text(context.connection.account_ref.as_str(), FIELD_BYTES_MAX)
            ),
        ];
        let history = self.last_authentication(Some(&context.basis));
        lines.extend(history.iter().take(3).cloned());
        lines.extend(self.action_lines());
        let claude = self.claude_code.render();
        if !claude.is_empty() {
            lines.push(claude);
        }
        if let Some(reply) = &self.presentation {
            if let Some(code) = &reply.fact.code {
                lines.push(format!("Owner result: {}", public_problem(code)));
            }
            if let Some(challenge) = &reply.challenge {
                lines.push(render_challenge(challenge.codex()));
            }
            if let Some(account) = &reply.account {
                lines.push(render_account(account));
            }
        }
        lines.extend(history.into_iter().skip(3));
        if !compact {
            lines.push("Account observations do not qualify model delivery or quota".into());
        }
        lines.push("Enter action | Esc hide | Tab focus | PgUp/PgDn scroll".into());
        let body = lines.join("\n");
        let body = body
            .lines()
            .skip(usize::from(self.scroll))
            .collect::<Vec<_>>()
            .join("\n");
        let output = format!("{header}\n{body}");
        assert!(output.len() <= DISPLAY_BYTES_MAX);
        output
    }

    fn action_lines(&self) -> Vec<String> {
        if let Some(confirm) = self.confirmation {
            vec![
                "Request logout for this connection?".into(),
                format!(
                    "{} Back | {} Confirm logout",
                    if confirm { " " } else { ">" },
                    if confirm { ">" } else { " " }
                ),
            ]
        } else {
            self.choices()
                .iter()
                .enumerate()
                .map(|(index, choice)| {
                    format!(
                        "{} {choice}",
                        if self.selected == index { ">" } else { " " }
                    )
                })
                .collect()
        }
    }

    fn assert_consistent(&self) {
        assert!(
            self.job
                .as_ref()
                .is_none_or(|job| self.flight.as_ref() == Some(job))
        );
        assert!(!self.open || self.context.is_some());
        assert!(!self.open || self.selected < self.choices().len());
        assert!(self.presentation.is_none() || (self.open && self.connected));
        if let Some(retained) = &self.retained {
            assert!(retained.original.command.action().is_some());
            assert!(retained.fact.as_ref().is_none_or(|fact| {
                retained.original.command.operation_id() == Some(&fact.operation_id)
            }));
        }
    }
}

pub(super) fn supported(connection: &ModelConnection) -> bool {
    connection.protocol.as_str() == "native"
        && matches!(connection.harness.as_str(), "codex" | "claude")
        && matches!(connection.endpoint, ModelEndpoint::Native {})
        && matches!(connection.auth, ModelAuthentication::NativeManaged { .. })
}

fn new_request_id() -> Result<RequestId, String> {
    let id = crate::engine::owner::random_id()
        .map_err(|_| "Authentication request identity unavailable")?;
    RequestId::new(id).map_err(|_| "Authentication request identity invalid".into())
}

pub(super) fn public_problem(problem: &str) -> &'static str {
    match problem {
        "native_auth_not_active" => "native_auth_not_active",
        "native_auth_operation_missing" => "native_auth_operation_missing",
        "native_auth_recovery_required" => "native_auth_recovery_required",
        "native_auth_sign_in_expired" => {
            "Sign-in code expired before approval; start sign-in again"
        }
        "native_auth_operation_conflict" => "native_auth_operation_conflict",
        "native_auth_operation_scope_conflict" => "native_auth_operation_scope_conflict",
        "native_auth_operation_capacity" => "native_auth_operation_capacity",
        "native_auth_database_mismatch" => "native_auth_database_mismatch",
        "native_auth_readonly" => "native_auth_readonly",
        "native_auth_host_unavailable" => "native_auth_host_unavailable",
        "native_auth_method_mismatch" => "native_auth_method_mismatch",
        "native_profile_source_conflict" => "native_profile_source_conflict",
        "native_profile_in_use" => "native_profile_in_use",
        "native_profile_not_managed" => "native_profile_not_managed",
        "owner_grant_stale_or_revoked" => "owner_grant_stale_or_revoked",
        "native_auth_owner_identity_changed" => "native_auth_owner_identity_changed",
        "native_auth_inspection_unavailable" => "native_auth_inspection_unavailable",
        _ => "native_auth_transport_unavailable_read_original_operation",
    }
}

/// Names a native authentication action in owner-facing words.
/// Input: the retained original action. Output: a short plain label.
fn action_text(action: NativeAuthAction) -> &'static str {
    match action {
        NativeAuthAction::SignIn { method } => match method {
            NativeAuthMethod::CodexBrowser => "Codex browser sign-in",
            NativeAuthMethod::CodexDeviceCode => "Codex device-code sign-in",
            NativeAuthMethod::CodexExisting => "Existing Codex sign-in check",
            NativeAuthMethod::ClaudeOfficial => "Claude official sign-in",
            NativeAuthMethod::ClaudeExisting => "Existing Claude sign-in check",
        },
        NativeAuthAction::Logout => "Logout",
    }
}

fn phase_text(phase: NativeAuthPhase) -> &'static str {
    match phase {
        NativeAuthPhase::Starting => "Official sign-in starting",
        NativeAuthPhase::AwaitingOwner => "Official sign-in awaits owner interaction",
        NativeAuthPhase::CheckingAccount => "Official account observation pending",
        NativeAuthPhase::AuthenticatedObserved => "Authenticated account observed",
        NativeAuthPhase::UnauthenticatedObserved => "Signed-out account observed",
        NativeAuthPhase::Cancelling => "Cancellation requested; outcome pending",
        NativeAuthPhase::Failed => "Native authentication failed",
        NativeAuthPhase::Cancelled => "Native authentication cancelled",
        NativeAuthPhase::Unavailable => "Native authentication unavailable",
        NativeAuthPhase::Uncertain => "Outcome uncertain; read the original operation",
    }
}

fn terminal_control(character: char) -> bool {
    character.is_control()
        || matches!(character,
        '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
}

fn display_text(text: &str, limit: usize) -> String {
    assert!(limit >= 16);
    let mut output = String::new();
    for character in text.chars() {
        let visible = if terminal_control(character) {
            character.escape_default().to_string()
        } else {
            character.to_string()
        };
        if output.len() + visible.len() > limit.saturating_sub(16) {
            output.push_str(" [truncated]");
            break;
        }
        output.push_str(&visible);
    }
    assert!(output.len() <= limit);
    output
}

fn render_challenge(challenge: &codex::LoginChallenge) -> String {
    let (url, code) = match challenge {
        codex::LoginChallenge::Browser { auth_url, .. } => (auth_url.expose(), None),
        codex::LoginChallenge::DeviceCode {
            user_code,
            verification_url,
            ..
        } => (verification_url.expose(), Some(user_code.expose())),
    };
    if url.chars().any(terminal_control)
        || code.is_some_and(|code| code.chars().any(terminal_control))
    {
        return "Official challenge cannot display safely; retain the native operation".into();
    }
    let mut output = format!("Official verification URL: {url}");
    if let Some(code) = code {
        output.push_str(&format!("\nOfficial user code: {code}"));
    }
    assert!(output.len() <= DISPLAY_BYTES_MAX);
    output
}

fn render_account(account: &NativeAuthAccount) -> String {
    match account {
        NativeAuthAccount::Codex(codex::AuthAccount::ApiKey) => {
            "Observed Codex account: API key".into()
        }
        NativeAuthAccount::Codex(codex::AuthAccount::AmazonBedrock {
            uses_codex_managed_credentials,
        }) => format!(
            "Observed Codex account: Amazon Bedrock; managed credentials: {}",
            uses_codex_managed_credentials
        ),
        NativeAuthAccount::Codex(codex::AuthAccount::Chatgpt { email, plan_type }) => format!(
            "Observed Codex account: ChatGPT\nEmail: {}\nObserved plan: {}",
            email.as_ref().map_or_else(
                || "unobserved".into(),
                |email| display_text(email.expose(), FIELD_BYTES_MAX)
            ),
            display_text(plan_type, FIELD_BYTES_MAX)
        ),
        NativeAuthAccount::Claude(status) => render_claude_account(status),
    }
}

fn render_claude_account(status: &claude::AuthStatus) -> String {
    let method = match &status.auth_method {
        claude::AuthMethod::None => "none",
        claude::AuthMethod::ClaudeAi => "claude.ai",
        claude::AuthMethod::ApiKey => "api_key",
        claude::AuthMethod::OauthToken => "oauth_token",
        claude::AuthMethod::ApiKeyHelper => "api_key_helper",
        claude::AuthMethod::ThirdParty => "third_party",
        claude::AuthMethod::Unknown(text) => text.as_str(),
    };
    let mut lines = vec![
        format!("Observed Claude loggedIn: {}", status.logged_in),
        format!("Observed method: {}", display_text(method, FIELD_BYTES_MAX)),
        format!(
            "Observed provider: {}",
            display_text(status.api_provider.as_str(), FIELD_BYTES_MAX)
        ),
        format!(
            "Observed config directory: {}",
            display_text(status.config_directory.as_str(), FIELD_BYTES_MAX)
        ),
    ];
    for (label, field) in [
        ("Email", &status.email),
        ("Organization ID", &status.org_id),
        ("Organization", &status.org_name),
        ("Subscription", &status.subscription_type),
    ] {
        let value = match field {
            claude::AuthStatusField::Missing => "unobserved".into(),
            claude::AuthStatusField::Null => "null".into(),
            claude::AuthStatusField::Value(text) => display_text(text.as_str(), FIELD_BYTES_MAX),
        };
        lines.push(format!("{label}: {value}"));
    }
    let output = lines.join("\n");
    assert!(output.len() <= DISPLAY_BYTES_MAX);
    output
}

#[cfg(all(test, unix))]
mod tests;
