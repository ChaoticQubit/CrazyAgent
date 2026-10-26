use super::*;
use crate::engine::owner::{
    AccountAuthCapabilitiesReply, AccountProfileOutcome, AccountProfileReceipt,
    AccountProfileReply, AccountSecret, NativeAuthScope, ProducerCapability,
};
use crate::tui::text::plain_code;
use std::fmt;

const SECRET_BYTES_MAX: usize = 8192;

struct SecretBuffer(Vec<u8>);

impl Default for SecretBuffer {
    fn default() -> Self {
        Self(Vec::with_capacity(SECRET_BYTES_MAX))
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBuffer([REDACTED])")
    }
}

impl SecretBuffer {
    fn character(&mut self, character: char) -> Result<(), String> {
        if character.is_control() || self.0.len() + character.len_utf8() > SECRET_BYTES_MAX {
            return Err("API key accepts at most 8192 literal UTF-8 bytes".into());
        }
        let mut encoded = [0; 4];
        self.0
            .extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        encoded.fill(0);
        assert!(self.0.len() <= SECRET_BYTES_MAX);
        Ok(())
    }

    fn backspace(&mut self) {
        let text = std::str::from_utf8(&self.0).expect("key input contains only UTF-8 characters");
        if let Some((start, _)) = text.char_indices().next_back() {
            self.0[start..].fill(0);
            self.0.truncate(start);
        }
    }

    fn take(&mut self) -> Result<AccountSecret, String> {
        if self.0.is_empty() {
            return Err("Enter an API key in the hidden buffer".into());
        }
        let bytes = std::mem::replace(&mut self.0, Vec::with_capacity(SECRET_BYTES_MAX));
        AccountSecret::new(bytes).map_err(|_| "API key input is invalid".into())
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

struct Panel {
    submission: ProfileSubmission,
    secret: SecretBuffer,
    capability_requested: bool,
    capability: Option<ProducerCapability>,
    attempted: bool,
    receipt: Option<AccountProfileReceipt>,
}

#[derive(Default)]
pub(super) struct Profile {
    panel: Option<Panel>,
    pending: Option<ProfileSubmission>,
    flight: Option<ProfileSubmission>,
}

impl Profile {
    pub(super) fn is_open(&self) -> bool {
        self.panel.is_some()
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn loading(&self) -> bool {
        self.flight.is_some()
    }
}

impl State {
    pub(super) fn open_profile(&mut self) -> Result<(), String> {
        if self.pinned || self.editing || self.confirmation.is_some() || self.loading() {
            return Err("Select an idle inspected RCLI connection for private setup".into());
        }
        let basis = self.writable()?;
        let Some(ModelAccessRecord::Connection { connection }) = &self.record else {
            return Err("Enter reads the selected connection before k opens private setup".into());
        };
        let connection = connection.clone();
        if self.items.get(self.selected)
            != Some(&ModelAccessRecord::Connection {
                connection: connection.clone(),
            })
        {
            return Err("Read the selected connection again before private setup".into());
        }
        validate_connection(&connection)?;
        let id = crate::engine::owner::random_id()
            .map_err(|_| "account_profile_request_identity_unavailable")?;
        let request = crate::engine::owner::AccountProfileRequest {
            request_id: RequestId::new(id)
                .map_err(|_| "account_profile_request_identity_invalid")?,
            scope: NativeAuthScope {
                expected_database_instance: basis.identity.database_instance().into(),
                connection: connection.id.clone(),
                expected_config_id: basis.config_id.clone(),
                expected_source_hash: basis.source_hash.clone(),
            },
        };
        let submission = ProfileSubmission {
            basis,
            connection,
            generation: self.advance()?,
            request,
        };
        self.profile.panel = Some(Panel {
            submission,
            secret: SecretBuffer::default(),
            capability_requested: false,
            capability: None,
            attempted: false,
            receipt: None,
        });
        self.explicit = true;
        self.notice = "Private profile setup; Enter explicitly configures locally".into();
        Ok(())
    }

    pub(super) fn reduce_profile(&mut self, event: &UiEvent) -> Result<bool, String> {
        if matches!(event, UiEvent::Cancel) {
            self.discard_unsent_profile();
            self.profile.panel = None;
            self.notice = if self.profile.has_pending() {
                "Panel closed; original outcome retained; Ctrl-R reads it without a key"
            } else {
                "Private setup closed; hidden buffer discarded"
            }
            .into();
            return Ok(true);
        }
        if self.profile.has_pending() {
            self.notice = "Profile outcome pending; Ctrl-R reads the original request".into();
            return Ok(true);
        }
        if matches!(event, UiEvent::FocusNext | UiEvent::FocusPrevious)
            && self
                .profile
                .panel
                .as_ref()
                .is_some_and(|panel| panel.receipt.is_some())
        {
            self.profile.panel = None;
            self.notice =
                "Account ready. Next: create a project with its folder (Projects, N)".into();
            return Ok(false);
        }
        let result = match event {
            UiEvent::Submit => self.confirm_profile(),
            UiEvent::Character(character) => self.profile_character(*character),
            UiEvent::TaskNext => self.profile_character(']'),
            UiEvent::Backspace => {
                self.profile.panel.as_mut().unwrap().secret.backspace();
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(problem) = &result {
            self.notice = problem.clone();
        }
        result.map(|()| true)
    }

    fn profile_character(&mut self, character: char) -> Result<(), String> {
        let panel = self.profile.panel.as_mut().unwrap();
        if !panel.attempted
            && matches!(
                panel.submission.connection.auth,
                ModelAuthentication::ApiKey { .. }
            )
        {
            panel.secret.character(character)
        } else {
            Ok(())
        }
    }

    pub(super) fn discard_unsent_profile(&mut self) {
        if matches!(&self.job, Some(Job::AccountProfileConfigure { .. })) {
            self.job = None;
            self.profile.pending = None;
        }
    }

    fn confirm_profile(&mut self) -> Result<(), String> {
        if self.job.is_some() {
            self.notice = "Checking the owner; hidden input retained for explicit Enter".into();
            return Ok(());
        }
        let panel = self.profile.panel.as_mut().unwrap();
        if panel.attempted {
            return Ok(());
        }
        if !self.ready || self.identity.as_ref() != Some(&panel.submission.basis.identity) {
            return Err("account_profile_owner_changed".into());
        }
        let secret = match panel.submission.connection.auth {
            ModelAuthentication::ApiKey { .. } => Some(panel.secret.take()?),
            ModelAuthentication::None {} => None,
            _ => return Err("account_profile_auth_unsupported".into()),
        };
        panel.attempted = true;
        panel.capability_requested = true;
        let submission = panel.submission.clone();
        self.profile.pending = Some(submission.clone());
        self.job = Some(Job::AccountProfileConfigure { submission, secret });
        self.notice = "Configuring private profile; no authentication or model request".into();
        Ok(())
    }

    pub(super) fn request_profile(&mut self) -> Option<Job> {
        if !self.ready || self.job.is_some() || self.has_pending() || self.loading() {
            return None;
        }
        let panel = self.profile.panel.as_mut()?;
        if panel.capability_requested {
            return None;
        }
        panel.capability_requested = true;
        let submission = panel.submission.clone();
        self.profile.flight = Some(submission.clone());
        Some(Job::AccountProfileCapabilities { submission })
    }

    pub(super) fn retry_profile(&mut self) -> bool {
        let Some(submission) = &self.profile.pending else {
            return false;
        };
        self.job = Some(Job::AccountProfileRead {
            submission: submission.clone(),
        });
        self.notice = "Reading the original profile request; no key is retained or replayed".into();
        true
    }

    pub(super) fn access_profile(&mut self) {
        self.profile.panel = None;
        self.profile.flight = None;
        if self.ready && self.profile.has_pending() {
            self.retry_profile();
        }
    }

    pub(super) fn apply_profile_capabilities(
        &mut self,
        submission: ProfileSubmission,
        result: Result<Box<AccountAuthCapabilitiesReply>, String>,
    ) {
        if self.profile.flight.as_ref() != Some(&submission) {
            return;
        }
        self.profile.flight = None;
        let Some(panel) = &mut self.profile.panel else {
            return;
        };
        if panel.submission != submission {
            return;
        }
        match result {
            Ok(reply) if capability_matches(&submission, &reply) => {
                if let ProducerCapability::Unavailable { code } = &reply.configured
                    && self.profile.pending.is_none()
                {
                    self.notice = public_problem(code);
                }
                panel.capability = Some(reply.configured);
            }
            Ok(_) => self.notice = "account_profile_capability_identity_mismatch".into(),
            Err(problem) => self.notice = public_problem(&problem),
        }
    }

    pub(super) fn apply_profile(
        &mut self,
        submission: ProfileSubmission,
        result: Result<Box<AccountProfileReply>, String>,
        retained: bool,
    ) -> Result<(), String> {
        if self.profile.pending.as_ref() != Some(&submission) {
            return Ok(());
        }
        match result {
            Ok(reply) => {
                if !profile_matches(&submission, &reply, self.identity.as_ref()) {
                    self.notice =
                        "account_profile_reply_identity_mismatch; Ctrl-R reads original".into();
                    return Ok(());
                }
                match &reply.outcome {
                    AccountProfileOutcome::ConfiguredOnly { receipt, .. } => {
                        if let Some(panel) = &mut self.profile.panel {
                            panel.receipt = Some((**receipt).clone());
                        }
                        self.notice =
                            "ConfiguredOnly; authentication and model availability unknown".into();
                        self.profile.pending = None;
                    }
                    AccountProfileOutcome::NotRecorded => {
                        self.notice = if retained {
                            "Profile outcome pending; Ctrl-R reads original without replaying a key"
                        } else {
                            "NotRecorded; close and reopen for a new explicit private setup"
                        }
                        .into();
                        if !retained {
                            self.profile.pending = None;
                        }
                    }
                }
            }
            Err(problem) => {
                self.notice = public_problem(&problem);
                if retained {
                    self.ready = false;
                } else {
                    self.profile.pending = None;
                }
            }
        }
        if !self.profile.has_pending() {
            self.basis = None;
            self.needs_page = true;
        }
        Ok(())
    }

    pub(super) fn profile_render(&self) -> String {
        let panel = self.profile.panel.as_ref().unwrap();
        let connection = &panel.submission.connection;
        let ModelEndpoint::Http { url } = &connection.endpoint else {
            unreachable!("private setup validates an HTTP endpoint");
        };
        let mut lines = vec![
            "Private profile setup".into(),
            format!("Provider: {}", visible(connection.provider.as_str())),
            format!("Endpoint: {}", visible(url)),
            format!("Model: {}", visible(&connection.model.id)),
            self.notice.clone(),
        ];
        if matches!(connection.auth, ModelAuthentication::ApiKey { .. }) {
            lines.push("API key (hidden)".into());
            lines.push(
                if panel.secret.0.is_empty() {
                    "Key buffer: empty"
                } else {
                    "Key buffer: hidden"
                }
                .into(),
            );
        } else {
            lines.push("No API key".into());
        }
        if let Some(capability) = &panel.capability {
            lines.push(match capability {
                ProducerCapability::Available => "Owner supports private configuration".into(),
                ProducerCapability::Unavailable { code } => public_problem(code),
            });
        }
        if let Some(receipt) = &panel.receipt {
            lines.push(format!(
                "ConfiguredOnly | generation {}",
                receipt.profile_generation
            ));
        }
        if self.profile.has_pending() {
            lines.push(format!(
                "Original request: {}",
                panel.submission.request.request_id.as_str()
            ));
            lines.push("Ctrl-R reads original outcome | Esc closes | key replay disabled".into());
        } else if panel.attempted {
            lines.push("Esc closes | reopen k for a new explicit attempt".into());
        } else {
            lines.push(
                "Enter configures locally | Esc cancels | no sign-in or model request".into(),
            );
        }
        lines.join("\n")
    }
}

pub(in crate::tui) fn validate_connection(connection: &ModelConnection) -> Result<(), String> {
    if !rcli_home_supported(connection) {
        return Err("account_profile_connection_unsupported".into());
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        return Err("account_profile_endpoint_unsupported".into());
    };
    let endpoint = url::Url::parse(url).map_err(|_| "account_profile_endpoint_invalid")?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || !endpoint.has_host()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err("account_profile_endpoint_invalid".into());
    }
    if matches!(connection.auth, ModelAuthentication::None {})
        && (endpoint.scheme() != "http"
            || !endpoint.host().is_some_and(|host| match host {
                url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
                url::Host::Ipv4(address) => address.is_loopback(),
                url::Host::Ipv6(address) => address.is_loopback(),
            }))
    {
        return Err("account_profile_local_none_requires_http_loopback".into());
    }
    Ok(())
}

fn capability_matches(
    submission: &ProfileSubmission,
    reply: &AccountAuthCapabilitiesReply,
) -> bool {
    reply.request_id == submission.request.request_id
        && reply.database_instance == submission.basis.identity.database_instance()
        && reply.engine_epoch == submission.basis.identity.engine_epoch()
        && reply.config_id == submission.basis.config_id
        && reply.source_hash == submission.basis.source_hash
        && reply.connection == submission.connection
}

fn profile_matches(
    submission: &ProfileSubmission,
    reply: &AccountProfileReply,
    current: Option<&OwnerConnectionIdentity>,
) -> bool {
    if reply.request_id != submission.request.request_id
        || reply.database_instance != submission.basis.identity.database_instance()
        || current.is_none_or(|identity| {
            reply.engine_epoch != identity.engine_epoch()
                || reply.database_instance != identity.database_instance()
        })
    {
        return false;
    }
    match &reply.outcome {
        AccountProfileOutcome::ConfiguredOnly { receipt, .. } => {
            receipt.config_id == submission.basis.config_id
                && receipt.source_hash == submission.basis.source_hash
                && receipt.connection == submission.connection
                && receipt.profile_generation > 0
                && !receipt.custody_id.is_empty()
                && receipt.profile_config_sha256.len() == 64
                && receipt
                    .profile_config_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        }
        AccountProfileOutcome::NotRecorded => true,
    }
}

fn visible(value: &str) -> String {
    value
        .chars()
        .take(4096)
        .flat_map(char::escape_default)
        .collect()
}

/// Keeps an account profile problem for the owner without raw error text.
/// Input: the engine's problem. Output: a plain code as it is, so a code with no
/// sentence still names itself, or a fixed code that says to read the original
/// request again when the problem is not a plain code.
pub(in crate::tui) fn public_problem(problem: &str) -> String {
    if plain_code(problem) {
        problem.to_owned()
    } else {
        "account_profile_unavailable_read_original_request".to_owned()
    }
}
