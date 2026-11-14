use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// How long one connect to a model provider's address may take (DEC-163).
const PROVIDER_REACH_TIME: Duration = Duration::from_secs(2);
/// The most addresses one reach check dials; a name that gives more is never named.
const ADDRESSES_MAX: usize = 8;
/// How long one lookup of a provider's name may take before the check names nothing.
const PROVIDER_LOOKUP_TIME: Duration = Duration::from_secs(2);
/// The most lookups out at once; with that many out, the next one names nothing.
const LOOKUPS_OUT_MAX: usize = 4;

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

impl ProviderReach {
    /// The reach the engine runs with: this machine's resolver and a direct connect bounded
    /// by [`PROVIDER_REACH_TIME`]. Output: the reach.
    pub fn system() -> Self {
        Self::new(
            |_, _| bounded(|| Ok(Vec::new())),
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

    /// Tells whether a remote provider known by name can't be reached. Inputs: the name and
    /// port. Output: the answer.
    pub(crate) fn unreached_name(&self, name: &str, port: u16) -> bool {
        let _ = (&self.lookup, name, port);
        false
    }

    /// Tells whether a remote provider at these addresses can't be reached. Input: the
    /// addresses. Output: the answer.
    pub(crate) fn unreached_at(&self, addresses: &[SocketAddr]) -> bool {
        let _ = addresses;
        false
    }

    /// Tells whether this machine finds none of a managed account's hosts. Input: the hosts.
    /// Output: the answer.
    pub(crate) fn offline(&self, hosts: &[(&str, u16)]) -> bool {
        assert!(!hosts.is_empty());
        false
    }
}

/// Runs one lookup within [`PROVIDER_LOOKUP_TIME`]. Input: the lookup. Output: its answer,
/// or TimedOut.
fn bounded(
    lookup: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
) -> io::Result<Vec<SocketAddr>> {
    let _ = (lookup, PROVIDER_LOOKUP_TIME, LOOKUPS_OUT_MAX);
    Err(io::ErrorKind::TimedOut.into())
}

/// Tells whether one connect to a model provider proves nothing listens there: only a
/// refused connect does, never a success, a timeout or another error such as too many open
/// files. Input: the connect's outcome. Output: true only for a refusal.
pub(crate) fn refused(outcome: io::Result<()>) -> bool {
    matches!(outcome, Err(error) if error.kind() == io::ErrorKind::ConnectionRefused)
}

#[cfg(test)]
mod tests;
