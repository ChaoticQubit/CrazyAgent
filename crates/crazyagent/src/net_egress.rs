use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};

use crate::github::http::exchange;
use kernel_security::error::StoreError;
use kernel_security::net::{CaptureRules, Egress, RawReply, Sealed, SendRow};

/// The hosts each bundled plugin's `plugin.toml` `[net].hosts` lists; equal to the
/// manifests, which no runtime code reads. Empty while no bundled plugin asks for
/// `net.request`.
pub const NET_HOSTS: &[(&str, &[&str])] = &[];

/// The reply fields each bundled plugin's `[net.capture]` names, per host; equal to the
/// manifests.
pub const NET_CAPTURES: &[(&str, &str, &[&str])] = &[];

/// The `[net.send]` rows of each bundled plugin's manifest: the field, the hosts its value
/// may also go to, and its `keep_as` name; equal to the manifests.
pub const NET_SENDS: &[SendRow<'static>] = &[];

/// The one reply header today's curl exchange reads back.
const SCOPES_HEADER: &str = "x-oauth-scopes";

/// The most bytes curl may download, as the GitHub exchange bounds it.
const BODY_BYTES_MAX: usize = 4 << 20;

/// Egress through the system curl and today's GitHub exchange, with a static per-plugin
/// host allowlist, until the kernel sandbox owns egress.
pub struct CurlEgress {
    hosts: &'static [(&'static str, &'static [&'static str])],
}

impl CurlEgress {
    /// The egress of the bundled plugins, over [`NET_HOSTS`]. Output: the egress.
    pub fn bundled() -> Self {
        Self { hosts: NET_HOSTS }
    }

    /// Egress over one host table. Input: (plugin, hosts) rows. Output: the egress.
    pub fn with_hosts(hosts: &'static [(&'static str, &'static [&'static str])]) -> Self {
        Self { hosts }
    }
}

/// The capture rules of the bundled plugins, over [`NET_CAPTURES`]. Output: the rules,
/// or the refusal of a malformed table.
pub fn bundled_captures() -> Result<CaptureRules, StoreError> {
    CaptureRules::new(NET_CAPTURES)?.with_sends(NET_SENDS)
}

/// The key a `[net].hosts` entry names an origin by: the host, with `:port` only for a
/// port other than the scheme's default. Inputs: the scheme, host and port. Output: the key.
fn host_key(scheme: &str, host: &str, port: u16) -> String {
    let default = match scheme {
        "https" => 443,
        "http" => 80,
        _ => 0,
    };
    if port == default {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    }
}

impl Egress for CurlEgress {
    fn allows(
        &self,
        caller: &'static str,
        scheme: &str,
        host: &str,
        port: u16,
    ) -> Result<(), StoreError> {
        let key = host_key(scheme, host, port);
        let listed = self
            .hosts
            .iter()
            .any(|(plugin, hosts)| *plugin == caller && hosts.contains(&key.as_str()));
        if listed {
            Ok(())
        } else {
            Err(StoreError::code("net_host_refused"))
        }
    }

    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, StoreError> {
        let found = (host, port)
            .to_socket_addrs()
            .map_err(|_| StoreError::code("net_host_unresolved"))?;
        Ok(found.map(|address| address.ip()).collect())
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        if sealed
            .reply_headers()
            .iter()
            .any(|name| !name.eq_ignore_ascii_case(SCOPES_HEADER))
        {
            return Err(StoreError::code("net_reply_header_unsupported"));
        }
        let loopback = matches!(sealed.host(), "127.0.0.1" | "[::1]" | "localhost");
        let reply = exchange(curl(loopback), sealed.config().to_vec()).map_err(|error| {
            StoreError::code(if error.0 == "github_reply_invalid" {
                "net_reply_invalid"
            } else {
                "net_unreachable"
            })
        })?;
        drop(sealed);
        let headers = match reply.scopes {
            Some(scopes) => vec![(SCOPES_HEADER.to_owned(), scopes.join(", "))],
            None => Vec::new(),
        };
        Ok(RawReply {
            status: reply.status,
            headers,
            body: reply.body,
        })
    }
}

/// The curl command of one request, with today's GitHub exchange flags: no `.curlrc`, no
/// redirects, one allowed scheme, bounded time and size, the config only on stdin and an
/// empty environment. Input: whether the host is loopback, which allows plain http only.
/// Output: the command, whose arguments and environment hold no URL, header or secret.
fn curl(loopback: bool) -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command
        .arg("-q")
        .args(["--silent", "--show-error", "--globoff"])
        .args(["--proto", if loopback { "=http,https" } else { "=https" }])
        .args(["--max-redirs", "0"])
        .args(["--connect-timeout", "10"])
        .args(["--max-time", "30"])
        .args(["--max-filesize", &BODY_BYTES_MAX.to_string()])
        .args(["--write-out", "\n%header{x-oauth-scopes}\n%{http_code}"])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests;
