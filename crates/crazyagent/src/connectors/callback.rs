//! The loopback address the owner's browser comes back to after a connector sign-in: one
//! listener on `127.0.0.1` at a port of its own, which takes exactly one answer carrying the
//! sign-in's `state`, refuses every other request without ending the wait, and closes after
//! `CALLBACK_WAIT`.

use std::io;
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The path the browser comes back to.
pub const CALLBACK_PATH: &str = "/callback";
/// How long a sign-in waits for the browser.
pub const CALLBACK_WAIT: Duration = Duration::from_secs(5 * 60);
/// How many refused requests close a callback before its answer came.
pub const REFUSED_MAX: usize = 32;
/// The page the browser shows once the answer came.
pub const DONE_PAGE: &str = "You can close this tab.";
/// The page the browser shows when the answer says the sign-in was refused.
pub const FAILED_PAGE: &str = "Sign-in did not finish. You can close this tab.";

/// How a wait for the browser ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// The answer carried the sign-in's state and a code, with the issuer when sent.
    Code { code: String, iss: Option<String> },
    /// The answer carried the sign-in's state and an `error`.
    Refused,
    /// No answer came within the wait.
    Expired,
    /// `REFUSED_MAX` requests were refused before the answer came.
    Flooded,
    /// The owner cancelled the sign-in.
    Stopped,
}

/// One sign-in's loopback listener and the address the browser is sent back to.
pub struct Callback {
    listener: TcpListener,
    redirect: url::Url,
}

impl Callback {
    /// Binds a listener of its own on `127.0.0.1` at a port the system picks. Output: the
    /// callback, or why it could not bind.
    pub fn open() -> io::Result<Self> {
        Err(io::Error::other("callback_not_built"))
    }

    /// The address the browser is sent back to: `http://127.0.0.1:<port>/callback`.
    pub fn redirect(&self) -> &url::Url {
        &self.redirect
    }

    /// Waits for the browser's answer carrying `state`, at most `limit`, until `stop` is set;
    /// the port closes when it returns. Inputs: the sign-in's state, the limit and the stop
    /// flag. Output: how the wait ended.
    pub fn wait(self, _state: &str, _limit: Duration, _stop: &AtomicBool) -> Waited {
        drop(self.listener);
        Waited::Expired
    }
}

#[cfg(test)]
#[path = "callback_tests.rs"]
mod tests;
