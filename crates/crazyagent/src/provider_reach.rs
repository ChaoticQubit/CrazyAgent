use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How long one connect to a model provider's address may take (DEC-163).
const PROVIDER_REACH_TIME: Duration = Duration::from_secs(2);
/// The most addresses one reach check dials; a name that gives more is never named.
const ADDRESSES_MAX: usize = 8;
/// How long one lookup of a provider's name may take before the check names nothing.
const PROVIDER_LOOKUP_TIME: Duration = Duration::from_secs(2);
/// The most lookups out at once; with that many out, the next one names nothing.
const LOOKUPS_OUT_MAX: usize = 4;

/// The system lookups whose thread has not ended, at most [`LOOKUPS_OUT_MAX`].
static LOOKUPS_OUT: AtomicUsize = AtomicUsize::new(0);

type Lookup = dyn Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync;
type Connect = dyn Fn(SocketAddr) -> io::Result<()> + Send + Sync;

/// How the reach check finds a model provider's addresses and dials one before a run
/// (DEC-163): this machine's resolver and network, or two stand-ins a row gives so no test
/// does real DNS. A lookup answers the addresses of a name and port, TimedOut when its bound
/// ran out; a connect answers one dial.
#[derive(Clone)]
pub struct ProviderReach {
    lookup: Arc<Lookup>,
    connect: Arc<Connect>,
}

/// What one lookup of a provider's name told: its addresses, that it has none, or nothing
/// sure, when the lookup ran out of its bound or gave more addresses than a check dials.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Addresses(Vec<SocketAddr>),
    Nowhere,
    Unsure,
}

impl ProviderReach {
    /// The reach the engine runs with: this machine's resolver, bounded by
    /// [`PROVIDER_LOOKUP_TIME`] and [`LOOKUPS_OUT_MAX`], and a direct connect bounded by
    /// [`PROVIDER_REACH_TIME`], as the run's own traffic dials. Output: the reach.
    pub fn system() -> Self {
        Self::new(
            |name, port| {
                let name = name.to_owned();
                bounded(move || {
                    (name.as_str(), port)
                        .to_socket_addrs()
                        .map(|addresses| addresses.take(ADDRESSES_MAX + 1).collect())
                })
            },
            |address| {
                crate::spawn_gate::connect_tcp_std_timeout(&address, PROVIDER_REACH_TIME)
                    .map(drop)
            },
        )
    }

    /// A reach that looks names up and dials through the two functions given, for rows that
    /// must not touch the network. Inputs: the lookup and the connect. Output: the reach.
    pub fn new(
        lookup: impl Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
        connect: impl Fn(SocketAddr) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Arc::new(lookup),
            connect: Arc::new(connect),
        }
    }

    /// Tells whether every one of a loopback provider's addresses refuses a connect, so
    /// nothing listens there. Input: the addresses, at least one and at most
    /// [`ADDRESSES_MAX`]. Output: true only when each one refused.
    pub(crate) fn refused_everywhere(&self, addresses: &[SocketAddr]) -> bool {
        assert!(!addresses.is_empty() && addresses.len() <= ADDRESSES_MAX);
        addresses
            .iter()
            .all(|address| refused((self.connect)(*address)))
    }

    /// Tells whether a remote provider known by name can't be reached: the name has no
    /// address within the lookup's bound, or every address it gives that the engine may dial
    /// refuses ([`Self::unreached_at`]). A lookup that ran out of time or failed on this
    /// machine names nothing. Inputs: the name and port. Output: the answer.
    pub(crate) fn unreached_name(&self, name: &str, port: u16) -> bool {
        match self.find(name, port) {
            Found::Nowhere => true,
            Found::Unsure => false,
            Found::Addresses(addresses) => self.unreached_at(&addresses),
        }
    }

    /// Tells whether a remote provider at these addresses can't be reached: of the at most
    /// [`ADDRESSES_MAX`] addresses, those the engine may dial
    /// ([`crate::sandbox::model_proxy::may_dial`]) all refuse a connect, at least one of
    /// them. An address on this machine is never dialed, and any other connect error, a
    /// timeout included, names nothing (DEC-163 Q4). Input: the addresses. Output: the
    /// answer.
    pub(crate) fn unreached_at(&self, addresses: &[SocketAddr]) -> bool {
        if addresses.len() > ADDRESSES_MAX {
            return false;
        }
        let dialed: Vec<SocketAddr> = addresses
            .iter()
            .copied()
            .filter(|address| crate::sandbox::model_proxy::may_dial(address.ip()))
            .collect();
        assert!(dialed.len() <= ADDRESSES_MAX);
        !dialed.is_empty()
            && dialed
                .iter()
                .all(|address| refused((self.connect)(*address)))
    }

    /// Tells whether this machine finds none of a managed account's hosts: every lookup
    /// ended within its bound with no address. Hosts are only looked up, never dialed
    /// (DEC-163 Q5). Input: the hosts, at least one. Output: the answer.
    pub(crate) fn offline(&self, hosts: &[(&str, u16)]) -> bool {
        assert!(!hosts.is_empty());
        hosts
            .iter()
            .all(|(name, port)| self.find(name, *port) == Found::Nowhere)
    }

    /// Looks one name up: no address for an empty answer or the resolver's own error, which
    /// carries no system error number and which the lib cannot tell from a transient one
    /// without unsafe code; nothing sure for TimedOut, an error with a system error number,
    /// such as too many open files on this machine (r1 F1), or more than [`ADDRESSES_MAX`]
    /// addresses. Inputs: the name and port. Output: what the lookup told.
    fn find(&self, name: &str, port: u16) -> Found {
        match (self.lookup)(name, port) {
            Ok(addresses) if addresses.is_empty() => Found::Nowhere,
            Ok(addresses) if addresses.len() > ADDRESSES_MAX => Found::Unsure,
            Ok(addresses) => Found::Addresses(addresses),
            Err(error)
                if error.kind() == io::ErrorKind::TimedOut || error.raw_os_error().is_some() =>
            {
                Found::Unsure
            }
            Err(_) => Found::Nowhere,
        }
    }
}

/// Runs one lookup on its own thread and waits at most [`PROVIDER_LOOKUP_TIME`] for it, so
/// a stuck resolver holds that thread, not the caller, and no tokio runtime is started on a
/// caller that may run on one. With [`LOOKUPS_OUT_MAX`] lookups still out, or when no thread
/// starts, it answers TimedOut at once without running the lookup. Input: the lookup.
/// Output: its answer, or TimedOut.
fn bounded(
    lookup: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
) -> io::Result<Vec<SocketAddr>> {
    let timed_out = || io::Error::from(io::ErrorKind::TimedOut);
    let out = LOOKUPS_OUT.fetch_add(1, Ordering::SeqCst);
    if out >= LOOKUPS_OUT_MAX {
        LOOKUPS_OUT.fetch_sub(1, Ordering::SeqCst);
        return Err(timed_out());
    }
    assert!(out < LOOKUPS_OUT_MAX);
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    let started = std::thread::Builder::new()
        .name("provider-lookup".into())
        .spawn(move || {
            let found = lookup();
            LOOKUPS_OUT.fetch_sub(1, Ordering::SeqCst);
            let _ = answer.send(found);
        });
    if started.is_err() {
        LOOKUPS_OUT.fetch_sub(1, Ordering::SeqCst);
        return Err(timed_out());
    }
    answered
        .recv_timeout(PROVIDER_LOOKUP_TIME)
        .unwrap_or_else(|_| Err(timed_out()))
}

/// Tells whether one connect to a model provider proves nothing listens there: only a
/// refused connect does, never a success, a timeout or another error such as too many open
/// files. Input: the connect's outcome. Output: true only for a refusal.
pub(crate) fn refused(outcome: io::Result<()>) -> bool {
    matches!(outcome, Err(error) if error.kind() == io::ErrorKind::ConnectionRefused)
}

#[cfg(test)]
mod tests;
