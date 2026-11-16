//! The owner's sign-in to an OAuth connector through the browser: the engine opens a
//! loopback callback, starts the sign-in with its address, answers the authorization
//! address for the owner's app to open, and waits on a thread of its own for the browser to
//! come back. At most one sign-in waits per connector, and the pages read where it stands.

use crate::connectors::callback::{CALLBACK_WAIT, Callback, Waited};
use crate::connectors::oauth::{self, OAuthError, Pending};
use crate::connectors::settings::{self, WritePolicy};
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How many sign-ins wait at once per engine root: as many as the OAuth client keeps, so
/// none waits on a sign-in the client already forgot.
pub const WAITING_MAX: usize = 8;
/// How long an ended sign-in's state stays readable.
pub const ENDED_KEEP: Duration = Duration::from_secs(10 * 60);

/// Where one connector's sign-in through the browser stands, as the pages read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignInState {
    /// No sign-in was started, or it ended long enough ago to be forgotten.
    #[serde(rename = "none")]
    Idle,
    /// The browser has not come back yet, or its answer is being redeemed.
    Waiting,
    /// The tokens are saved.
    SignedIn,
    /// The sign-in ended without tokens, for the reason `code` names.
    Failed { code: String },
    /// The owner cancelled it, or a later sign-in replaced it.
    Cancelled,
}

/// Where one sign-in stands, written by its own thread and by a cancel: whether the
/// browser's answer was claimed for redeeming, and when the sign-in ended.
struct Cell {
    state: SignInState,
    claimed: bool,
    ended: Option<Instant>,
}

/// One sign-in the engine holds: whose it is, the flag that closes its callback, and its
/// cell, which only it and a cancel write.
struct Flow {
    root: PathBuf,
    name: String,
    stop: Arc<AtomicBool>,
    cell: Arc<Mutex<Cell>>,
}

static FLOWS: Mutex<Vec<Flow>> = Mutex::new(Vec::new());

fn flows() -> MutexGuard<'static, Vec<Flow>> {
    FLOWS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn locked(cell: &Mutex<Cell>) -> MutexGuard<'_, Cell> {
    cell.lock().unwrap_or_else(PoisonError::into_inner)
}

fn failed(code: &str) -> SignInState {
    SignInState::Failed {
        code: code.to_owned(),
    }
}

fn unavailable() -> EngineError {
    EngineError("connector_oauth_callback_unavailable".into())
}

const FINISHING: &str = "connector_oauth_sign_in_finishing";

/// Whether the owner's app may open an authorization address: https, or in rows only the
/// loopback stand-in vendor's http address.
fn openable(address: &url::Url) -> bool {
    match (address.scheme(), address.host()) {
        ("https", _) => true,
        ("http", Some(url::Host::Ipv4(ip))) => cfg!(test) && ip == std::net::Ipv4Addr::LOCALHOST,
        _ => false,
    }
}

/// Whether a connector's sign-in has its browser answer claimed and still being redeemed.
/// Inputs: the held sign-ins, the engine root and the connector.
fn finishing(flows: &[Flow], root: &Path, name: &str) -> bool {
    flows
        .iter()
        .filter(|flow| flow.root == root && flow.name == name)
        .any(|flow| {
            let held = locked(&flow.cell);
            held.claimed && held.state == SignInState::Waiting
        })
}

/// Starts a sign-in to a trusted connector through the owner's browser, asking for more
/// scope when its server last refused a call for want of it. Inputs: the engine database,
/// the engine root and the connector. Output: the address the owner's app opens.
pub fn start(db: &Connection, root: &Path, name: &str) -> Result<url::Url, EngineError> {
    let (writes, challenge) = asked(db, name)?;
    let begin = |redirect: &url::Url| match challenge.as_deref() {
        Some(challenge) => oauth::step_up(db, root, name, challenge, redirect, writes),
        None => oauth::begin(db, root, name, redirect, writes),
    };
    let owned = root.to_path_buf();
    let finish = move |state: &str, code: &str, iss: Option<&str>| {
        let db = crate::questions::open(&owned);
        let db = db.map_err(|_| OAuthError("connectors_unavailable"))?;
        oauth::finish(&db, &owned, state, code, iss)
    };
    start_with(root, name, CALLBACK_WAIT, begin, finish)
}

/// How a sign-in of a connector begins now: whether the owner's setting allows writes, and
/// the scope its server last asked for, when one did. Inputs: the engine database and the
/// connector. Output: the two, or the refusal.
fn asked(db: &Connection, name: &str) -> Result<(bool, Option<String>), EngineError> {
    let (saved, _) = settings::load(db)?;
    Ok((saved.write != WritePolicy::ReadOnly, oauth::wanted(db, name)?))
}

/// Starts a sign-in with the given steps: opens the callback, runs `begin` with its address
/// and waits on a thread that redeems the answer with `finish` unless a cancel came first.
/// It replaces the connector's earlier sign-in, or is refused while that one's answer is
/// being redeemed. Inputs: root, connector, wait, steps. Output: the address, or the refusal.
pub(crate) fn start_with<B, F>(
    root: &Path,
    name: &str,
    limit: Duration,
    begin: B,
    finish: F,
) -> Result<url::Url, EngineError>
where
    B: FnOnce(&url::Url) -> Result<Pending, OAuthError>,
    F: FnOnce(&str, &str, Option<&str>) -> Result<(), OAuthError> + Send + 'static,
{
    assert!(!name.is_empty());
    if finishing(&flows(), root, name) {
        return Err(EngineError(FINISHING.into()));
    }
    let callback = Callback::open().map_err(|_| unavailable())?;
    let begun = begin(callback.redirect());
    let Pending {
        authorize_url,
        state,
    } = begun.map_err(|refusal| EngineError(refusal.0.into()))?;
    if !openable(&authorize_url) {
        return Err(EngineError("connector_address_refused".into()));
    }
    assert!(!state.is_empty(), "a sign-in has a state");
    let stop = Arc::new(AtomicBool::new(false));
    let cell = Arc::new(Mutex::new(Cell {
        state: SignInState::Waiting,
        claimed: false,
        ended: None,
    }));
    let (told, written) = (stop.clone(), cell.clone());
    std::thread::Builder::new()
        .name("connector-sign-in".into())
        .spawn(move || {
            let waited = callback.wait(&state, limit, &told);
            settle(&written, waited, |code, iss| finish(&state, code, iss));
        })
        .map_err(|_| unavailable())?;
    keep(Flow {
        root: root.to_path_buf(),
        name: name.to_owned(),
        stop,
        cell,
    })?;
    Ok(authorize_url)
}

/// Ends a sign-in's cell with how its wait ended. An answer is claimed first, unless a
/// cancel came before it, and then redeemed; any other end is written only while the
/// sign-in still waits. Inputs: the cell, how the wait ended and the redeem step.
fn settle(
    cell: &Mutex<Cell>,
    waited: Waited,
    finish: impl FnOnce(&str, Option<&str>) -> Result<(), OAuthError>,
) {
    let end = match waited {
        Waited::Code { code, iss } => {
            {
                let mut held = locked(cell);
                if held.state != SignInState::Waiting {
                    return;
                }
                held.claimed = true;
            }
            match finish(&code, iss.as_deref()) {
                Ok(()) => SignInState::SignedIn,
                Err(refusal) => failed(refusal.0),
            }
        }
        Waited::Refused => failed("connector_oauth_refused"),
        Waited::Expired => failed("connector_oauth_callback_expired"),
        Waited::Flooded => failed("connector_oauth_callback_flooded"),
        Waited::Stopped => SignInState::Cancelled,
    };
    let mut held = locked(cell);
    if held.claimed || held.state == SignInState::Waiting {
        held.state = end;
        held.ended = Some(Instant::now());
    }
    assert!(held.state != SignInState::Waiting && held.ended.is_some());
}

/// Cancels one sign-in unless its answer is being redeemed; its stop flag closes the
/// callback. Input: the sign-in. Output: where it stands after.
fn stopped(flow: &Flow) -> SignInState {
    let mut held = locked(&flow.cell);
    if held.state == SignInState::Waiting && !held.claimed {
        held.state = SignInState::Cancelled;
        held.ended = Some(Instant::now());
        flow.stop.store(true, Ordering::SeqCst);
    }
    held.state.clone()
}

/// Whether a sign-in still waits for the browser, with no answer claimed.
fn waits(flow: &Flow) -> bool {
    let held = locked(&flow.cell);
    held.state == SignInState::Waiting && !held.claimed
}

/// Drops the sign-ins that ended more than `ENDED_KEEP` ago. Input: the held sign-ins.
fn prune(flows: &mut Vec<Flow>) {
    flows.retain(|flow| {
        let ended = locked(&flow.cell).ended;
        ended.is_none_or(|ended| ended.elapsed() < ENDED_KEEP)
    });
}

/// Holds a new sign-in: it replaces and cancels the connector's earlier one, and the oldest
/// waiting one of its root is cancelled once `WAITING_MAX` already wait. While the earlier
/// one's answer is being redeemed, the new one is refused and its callback closes. Input:
/// the sign-in. Output: Ok, or `connector_oauth_sign_in_finishing`.
fn keep(flow: Flow) -> Result<(), EngineError> {
    let mut flows = flows();
    prune(&mut flows);
    if finishing(&flows, &flow.root, &flow.name) {
        flow.stop.store(true, Ordering::SeqCst);
        return Err(EngineError(FINISHING.into()));
    }
    let same = |kept: &Flow| kept.root == flow.root && kept.name == flow.name;
    if let Some(at) = flows.iter().position(same) {
        stopped(&flows.remove(at));
    }
    let waiting: Vec<&Flow> = flows
        .iter()
        .filter(|kept| kept.root == flow.root && waits(kept))
        .collect();
    if waiting.len() >= WAITING_MAX {
        stopped(waiting[0]);
    }
    let root = flow.root.clone();
    flows.push(flow);
    let count = flows
        .iter()
        .filter(|kept| kept.root == root && waits(kept))
        .count();
    assert!(count <= WAITING_MAX, "{count}");
    Ok(())
}

/// Where a connector's sign-in stands. Inputs: the engine root and the connector. Output:
/// its state.
pub fn state_of(root: &Path, name: &str) -> SignInState {
    let mut flows = flows();
    prune(&mut flows);
    flows
        .iter()
        .find(|flow| flow.root == root && flow.name == name)
        .map_or(SignInState::Idle, |flow| locked(&flow.cell).state.clone())
}

/// Clears a connector's sign-in once its record or sign-in identity changed: a waiting one
/// is cancelled and reads cancelled, one whose answer is being redeemed is left to finish,
/// and one that already ended is forgotten, so it never reads signed in after. Inputs: the
/// engine root and the connector.
fn replaced(root: &Path, name: &str) {
    let mut flows = flows();
    prune(&mut flows);
    let same = |flow: &Flow| flow.root == root && flow.name == name;
    let Some(at) = flows.iter().position(same) else {
        return;
    };
    if locked(&flows[at].cell).state == SignInState::Waiting {
        stopped(&flows[at]);
    } else {
        flows.remove(at);
    }
}

/// Cancels a connector's waiting sign-in and closes its callback, unless the browser's
/// answer already came and is being redeemed. Inputs: the engine root and the connector.
/// Output: where it stands after.
pub fn cancel(root: &Path, name: &str) -> SignInState {
    let mut flows = flows();
    prune(&mut flows);
    flows
        .iter()
        .find(|flow| flow.root == root && flow.name == name)
        .map_or(SignInState::Idle, stopped)
}

fn refusal(error: OAuthError) -> EngineError {
    EngineError(error.0.into())
}

/// Saves the owner's own OAuth app for a connector that allows one, which then signs in
/// with that app alone; a waiting sign-in is cancelled first, since it began with another
/// client, and an ended one forgotten. Inputs: the engine database and root, the connector,
/// the client id and the client secret of a confidential app. Output: Ok, or the refusal.
pub fn save_own_app(
    db: &Connection,
    root: &Path,
    name: &str,
    client_id: &str,
    secret: Option<&[u8]>,
) -> Result<(), EngineError> {
    let secret = secret
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| EngineError("connector_oauth_client_invalid".into()))?;
    replaced(root, name);
    oauth::set_own_client(db, root, name, client_id, secret)
        .map_err(refusal)
}

/// Removes the owner's own OAuth app for a connector, which then signs in the automatic
/// way; a waiting sign-in is cancelled first and an ended one forgotten. Inputs: the engine
/// database and root and the connector. Output: Ok, or the refusal.
pub fn clear_own_app(db: &Connection, root: &Path, name: &str) -> Result<(), EngineError> {
    replaced(root, name);
    oauth::clear_own_client(db, root, name).map_err(refusal)
}

/// Signs a connector out at the owner's ask: a waiting sign-in is cancelled first and an
/// ended one forgotten, then the sign-in ends with no "Sign in again" after. Inputs: the
/// engine database and root and the connector. Output: Ok, or the refusal.
pub fn sign_out(db: &Connection, root: &Path, name: &str) -> Result<(), EngineError> {
    replaced(root, name);
    oauth::sign_out(db, root, name).map_err(refusal)
}

#[cfg(test)]
#[path = "sign_in_tests.rs"]
mod tests;
