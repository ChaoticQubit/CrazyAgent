use crate::credentials::SecretString;
use crate::engine::EngineError;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// The plain sentence for [`SIGN_IN_EXPIRED`], shown to the owner and put in the error the
/// relay answers the harness with.
pub const SIGN_IN_EXPIRED_LINE: &str =
    "This ChatGPT sign-in has expired. Sign in again in Settings, Accounts.";

const _: () = assert!(USABLE_MIN.as_secs() < RENEW_MARGIN.as_secs());

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

    /// The access token and account id one request is forwarded with. A token that lapses
    /// within [`RENEW_MARGIN`] is first renewed, by one request at a time while the others
    /// wait for its outcome. A renewal that fails, or leaves the token as it was, while the
    /// token still has more than [`USABLE_MIN`] left forwards that token and is not tried
    /// again for [`RENEW_BACKOFF`]; one that leaves it lapsing within [`USABLE_MIN`] stops
    /// the source for good. Input: the relay's stop test, handed to the renewal.
    /// Output: a copy of each, or `chatgpt_sign_in_expired`.
    pub fn current(
        &self,
        halted: &dyn Fn() -> bool,
    ) -> Result<(SecretString, Option<String>), EngineError> {
        let expired = || EngineError(SIGN_IN_EXPIRED.to_owned());
        let mut state = self.state();
        if state.failed.is_some() {
            return Err(expired());
        }
        let now = since_epoch();
        let lapsing = state.tokens.expires <= now + RENEW_MARGIN;
        let waiting = state.retry_at.is_some_and(|at| Instant::now() < at)
            && state.tokens.expires > now + USABLE_MIN;
        if lapsing && !waiting {
            let renewed = (self.renew)(state.tokens.expires, halted);
            let now = since_epoch();
            match renewed {
                Ok(tokens) if tokens.expires > now + USABLE_MIN => {
                    state.retry_at = (tokens.expires <= now + RENEW_MARGIN)
                        .then(|| Instant::now() + RENEW_BACKOFF);
                    state.tokens = tokens;
                }
                Err(_) if state.tokens.expires > now + USABLE_MIN => {
                    state.retry_at = Some(Instant::now() + RENEW_BACKOFF);
                }
                _ => {
                    state.failed = Some(SIGN_IN_EXPIRED);
                    return Err(expired());
                }
            }
        }
        assert!(state.failed.is_none());
        let access = SecretString::bearer(state.tokens.access.expose().as_bytes().to_vec())
            .map_err(|_| expired())?;
        Ok((access, state.tokens.account_id.clone()))
    }

    /// Why this source stopped forwarding, once a renewal left the sign-in lapsed.
    /// Output: `chatgpt_sign_in_expired`, or none while it still forwards.
    pub fn failed(&self) -> Option<&'static str> {
        self.state().failed
    }
}

/// The time since the epoch now, the scale of a token's expiry. A clock set before the
/// epoch reads as the epoch, so every held token looks fresh and the backend decides.
fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}
