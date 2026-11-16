use crate::credentials::SecretString;
use crate::engine::EngineError;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The ChatGPT backend Codex sends a ChatGPT sign-in's model turns to (openai/codex
/// `model-provider-info/src/lib.rs` `CHATGPT_CODEX_BASE_URL`). A ChatGPT relay forwards only
/// here.
pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// How long before the access token lapses the source renews it before forwarding.
pub const RENEW_MARGIN: Duration = Duration::from_secs(300);

/// The least time a token must have left after a renewal attempt to be forwarded at all.
pub const USABLE_MIN: Duration = Duration::from_secs(60);

/// How long the source waits after a renewal that left the token as it was, or failed
/// while the token still had time, before it tries again.
pub const RENEW_BACKOFF: Duration = Duration::from_secs(30);

/// The code a run gets once its ChatGPT sign-in can no longer be renewed.
pub const SIGN_IN_EXPIRED: &str = "chatgpt_sign_in_expired";

/// One reading of a ChatGPT sign-in, held only in engine memory: the access token, the
/// ChatGPT account id Codex sends beside it when the record has one, and when the access
/// token lapses, as time since the epoch.
pub struct ChatGptTokens {
    pub access: SecretString,
    pub account_id: Option<String>,
    pub expires: Duration,
}

/// Renews the sign-in through the engine's own renewal. Inputs: the expiry the source saw
/// and a test that tells the renewal its relay is stopping. Output: the sign-in as it is
/// after the attempt, or the renewal's refusal.
pub type Renew =
    Box<dyn Fn(Duration, &dyn Fn() -> bool) -> Result<ChatGptTokens, EngineError> + Send + Sync>;

struct State {
    tokens: ChatGptTokens,
    failed: Option<&'static str>,
    retry_at: Option<Instant>,
}

/// The ChatGPT sign-in a relay forwards with. It keeps the access token in memory and asks
/// the engine's renewal for a fresh one shortly before it lapses, one renewal at a time;
/// it never refreshes the sign-in itself, so only the engine ever uses the refresh token.
pub struct ChatGptSource {
    state: Mutex<State>,
    renew: Renew,
}

impl ChatGptSource {
    /// Holds a sign-in read from custody with the renewal that keeps it fresh.
    /// Inputs: the sign-in and the renewal. Output: the source.
    pub fn new(tokens: ChatGptTokens, renew: Renew) -> Self {
        Self {
            state: Mutex::new(State {
                tokens,
                failed: None,
                retry_at: None,
            }),
            renew,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The access token and account id one request is forwarded with.
    /// Input: the relay's stop test. Output: a copy of each, or `chatgpt_sign_in_expired`.
    pub fn current(
        &self,
        _halted: &dyn Fn() -> bool,
    ) -> Result<(SecretString, Option<String>), EngineError> {
        let state = self.state();
        let access = SecretString::bearer(state.tokens.access.expose().as_bytes().to_vec())
            .map_err(|_| EngineError(SIGN_IN_EXPIRED.to_owned()))?;
        Ok((access, state.tokens.account_id.clone()))
    }

    /// Why this source stopped forwarding, once a renewal left the sign-in lapsed.
    /// Output: `chatgpt_sign_in_expired`, or none while it still forwards.
    pub fn failed(&self) -> Option<&'static str> {
        let _ = &self.renew;
        None
    }
}
