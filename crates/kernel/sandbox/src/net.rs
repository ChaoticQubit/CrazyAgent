use crate::error::{SandboxError, invalid};

/// Network reach of a sandboxed agent: open when the agent's tools are on, except that
/// it never binds or listens on an `ports::OWNER_PORTS` port, one `ProjectSandbox::keep_ports`
/// keeps or one of `ports::PROXY_PORTS` or `ports::RELAY_PORTS`, otherwise
/// only the listed remotes, each one port on loopback or on any host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNetwork {
    Open,
    Only(Vec<SandboxRemote>),
}

/// One outbound TCP destination a tools-off agent may still reach. Seatbelt filters by port only,
/// either on loopback or on any host, so `accounts::account_url` refuses an endpoint on another
/// machine on a port a cloud metadata service answers on when it is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxRemote {
    Loopback(u16),
    AnyHost(u16),
}

impl SandboxRemote {
    /// The destination a tools-off agent needs for one model endpoint: the URL's port on
    /// loopback for a local server, otherwise that port on any host. Input: an http or
    /// https URL. Output: the remote, or a refusal for another scheme, host or port.
    pub fn for_endpoint(url: &str) -> Result<Self, SandboxError> {
        let parsed = url::Url::parse(url).map_err(|_| invalid("endpoint URL is invalid"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid("endpoint URL must be http or https"));
        }
        let port = parsed
            .port_or_known_default()
            .filter(|port| *port != 0)
            .ok_or_else(|| invalid("endpoint port is missing"))?;
        let loopback = match parsed.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => return Err(invalid("endpoint host is missing")),
        };
        assert_ne!(port, 0);
        Ok(if loopback {
            Self::Loopback(port)
        } else {
            Self::AnyHost(port)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model endpoint's port reads as a loopback remote for this computer and as an
    /// any-host remote otherwise, with the scheme's default port when none is written; an
    /// endpoint that is not http or https, has no host or names port 0 is refused.
    #[test]
    fn an_endpoint_reads_as_a_loopback_or_any_host_port() {
        for (url, remote) in [
            ("http://127.0.0.1:11434/v1", SandboxRemote::Loopback(11434)),
            ("http://[::1]:8080", SandboxRemote::Loopback(8080)),
            ("http://LocalHost:9000", SandboxRemote::Loopback(9000)),
            ("https://api.example.com/v1", SandboxRemote::AnyHost(443)),
            ("http://models.example.com", SandboxRemote::AnyHost(80)),
        ] {
            assert_eq!(SandboxRemote::for_endpoint(url).unwrap(), remote, "{url}");
        }
        for url in [
            "ftp://127.0.0.1:21",
            "http://127.0.0.1:0",
            "unix:/tmp/socket",
            "not a url",
        ] {
            let refused = SandboxRemote::for_endpoint(url);
            assert!(
                matches!(refused, Err(SandboxError::Invalid(_))),
                "{url}: {refused:?}"
            );
        }
    }
}
