use super::request::{Capture, Captured, REPLY_BYTES_MAX, REPLY_LIST_MAX};
use crate::error::StoreError;
use crate::secret::{SecretString, wipe};
use crate::secrets::Secrets;
use crate::store::checked_name;
use std::collections::BTreeMap;

/// The most reply fields one manifest row, or one request, names for capture.
pub const CAPTURES_MAX: usize = 4;
/// The most bytes of one capture field name.
pub const FIELD_BYTES_MAX: usize = 64;
/// The most hosts one `[net.send]` row names.
pub const SEND_HOSTS_MAX: usize = 16;

/// One `[net.send]` row: the plugin, the capture field, the host keys its value may also
/// be sent to, and the name it is kept under, if any.
pub type SendRow<'a> = (&'static str, &'a str, &'a [&'a str], Option<&'a str>);

/// One plugin field's `[net.send]` row: the extra host keys and the `keep_as` name.
type Send = (Vec<String>, Option<String>);

/// The reply fields each plugin's manifest names as always captured, per host, and where
/// each captured field may be sent.
pub struct CaptureRules {
    by_plugin_host: BTreeMap<(&'static str, String), Vec<String>>,
    sends: BTreeMap<(&'static str, String), Send>,
}

impl CaptureRules {
    /// Builds the rules. Inputs: (plugin, host, fields) rows. Output: the rules, or
    /// `net_capture_rules_invalid` for a duplicate row or field, a field outside
    /// [`field_fits`], or more than `CAPTURES_MAX` fields per row.
    pub fn new(rows: &[(&'static str, &str, &[&str])]) -> Result<Self, StoreError> {
        let invalid = || StoreError::code("net_capture_rules_invalid");
        let mut by_plugin_host = BTreeMap::new();
        for (plugin, host, fields) in rows {
            if fields.len() > CAPTURES_MAX || !fields.iter().all(|field| field_fits(field)) {
                return Err(invalid());
            }
            let mut kept: Vec<String> = Vec::with_capacity(fields.len());
            for field in *fields {
                if kept.iter().any(|found| found == field) {
                    return Err(invalid());
                }
                kept.push((*field).to_owned());
            }
            if by_plugin_host
                .insert((*plugin, (*host).to_owned()), kept)
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            by_plugin_host,
            sends: BTreeMap::new(),
        })
    }

    /// The same rules with `[net.send]` rows. Input: the rows. Output: the rules, or
    /// `net_capture_rules_invalid` for a field no capture row of the plugin names, a
    /// duplicate row, 0 or more than `SEND_HOSTS_MAX` hosts or a duplicate host, a refused
    /// `keep_as` name, or one `keep_as` twice in a plugin.
    pub fn with_sends(mut self, rows: &[SendRow<'_>]) -> Result<Self, StoreError> {
        let invalid = || StoreError::code("net_capture_rules_invalid");
        for (plugin, field, hosts, keep_as) in rows {
            let captured = self.by_plugin_host.iter().any(|((owner, _), fields)| {
                owner == plugin && fields.iter().any(|known| known == field)
            });
            let distinct = hosts
                .iter()
                .enumerate()
                .all(|(index, host)| !hosts[..index].contains(host));
            let name_fits = keep_as.is_none_or(|name| checked_name(name).is_ok());
            let name_taken = keep_as.is_some_and(|name| {
                self.sends
                    .iter()
                    .any(|((owner, _), (_, kept))| owner == plugin && kept.as_deref() == Some(name))
            });
            if !captured
                || hosts.is_empty()
                || hosts.len() > SEND_HOSTS_MAX
                || !distinct
                || !name_fits
                || name_taken
            {
                return Err(invalid());
            }
            let row = (
                hosts.iter().map(|host| (*host).to_owned()).collect(),
                keep_as.map(str::to_owned),
            );
            if self
                .sends
                .insert((*plugin, (*field).to_owned()), row)
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(self)
    }

    /// The host keys a value captured from one field at one host may be sent to: that host,
    /// then its `[net.send]` hosts. Inputs: the plugin, the field and the capture host key.
    /// Output: the host keys.
    pub fn send_hosts(&self, caller: &'static str, field: &str, host: &str) -> Vec<String> {
        let mut hosts = vec![host.to_owned()];
        if let Some((more, _)) = self.sends.get(&(caller, field.to_owned())) {
            hosts.extend(more.iter().filter(|more| *more != host).cloned());
        }
        assert!(hosts.len() <= SEND_HOSTS_MAX + 1);
        hosts
    }

    /// The fields always captured for one plugin and host. Inputs: the plugin and the host
    /// key. Output: the fields, empty when the manifest names none.
    pub fn fields(&self, caller: &'static str, host: &str) -> &[String] {
        self.by_plugin_host
            .get(&(caller, host.to_owned()))
            .map_or(&[], Vec::as_slice)
    }
}

/// Whether a capture field name fits: 1 to `FIELD_BYTES_MAX` bytes of ASCII letters,
/// digits, `_` or `-`. Input: the name. Output: the answer.
pub fn field_fits(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= FIELD_BYTES_MAX
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// The key a manifest names a host by: the host, with `:port` only for a port other than
/// the scheme's default. Input: the parsed URL. Output: the key.
pub fn host_key(url: &url::Url) -> String {
    let host = url.host_str().expect("a target always has a host");
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// The fields to capture for one request: the manifest's for the host, which a request
/// can give a `keep_as` but never remove, then the request's own. Inputs: the rules, the
/// plugin, the host key and the request's captures. Output: the fields, or
/// `net_request_invalid` for a field outside [`field_fits`], a duplicate or more than
/// `CAPTURES_MAX` request captures.
pub fn fields(
    rules: &CaptureRules,
    caller: &'static str,
    host: &str,
    asked: &[Capture],
) -> Result<Vec<Capture>, StoreError> {
    let invalid = || StoreError::code("net_request_invalid");
    if asked.len() > CAPTURES_MAX {
        return Err(invalid());
    }
    let mut wanted: Vec<Capture> = rules
        .fields(caller, host)
        .iter()
        .map(|field| Capture {
            field: field.clone(),
            keep_as: None,
        })
        .collect();
    let from_manifest = wanted.len();
    for (index, capture) in asked.iter().enumerate() {
        if !field_fits(&capture.field)
            || asked[..index]
                .iter()
                .any(|seen| seen.field == capture.field)
        {
            return Err(invalid());
        }
        match wanted[..from_manifest]
            .iter_mut()
            .find(|found| found.field == capture.field)
        {
            Some(found) => found.keep_as.clone_from(&capture.keep_as),
            None => wanted.push(capture.clone()),
        }
    }
    assert!(wanted.len() <= CAPTURES_MAX * 2);
    Ok(wanted)
}

/// Takes the captured fields out of a reply body into handles, each sendable to the
/// capture host and its `[net.send]` hosts. Inputs: the secrets module, the plugin, the
/// rules with the capture host key, the fields and the raw body, which is wiped. Output: the body the
/// plugin gets, re-serialized without the captured fields, and their handles; or
/// `net_capture_not_json` when a field is to be captured and the body is not one JSON
/// object or a present field is not a string, so no body reaches the plugin;
/// `cap_output_too_large` when the body left over is past `REPLY_BYTES_MAX`, checked before
/// any handle is minted. A failed save frees the handles this call minted.
pub fn take(
    secrets: &Secrets<'_>,
    caller: &'static str,
    (rules, host): (&CaptureRules, &str),
    fields: &[Capture],
    mut body: Vec<u8>,
) -> Result<(Vec<u8>, Vec<Captured>), StoreError> {
    if fields.is_empty() {
        return Ok((body, Vec::new()));
    }
    let parsed = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body);
    wipe(&mut body);
    let mut object = parsed.map_err(|_| StoreError::code("net_capture_not_json"))?;
    let mut values = Vec::with_capacity(fields.len());
    for wanted in fields {
        match object.remove(&wanted.field) {
            None => {}
            Some(serde_json::Value::String(text)) => {
                let bytes = text.into_bytes();
                let value = match wanted.keep_as {
                    Some(_) => SecretString::new(bytes)?,
                    None => SecretString::bearer(bytes)?,
                };
                values.push((wanted, value));
            }
            Some(_) => return Err(StoreError::code("net_capture_not_json")),
        }
    }
    let reply =
        serde_json::to_vec(&object).map_err(|_| StoreError::code("net_capture_not_json"))?;
    if reply.len() > REPLY_BYTES_MAX {
        return Err(StoreError::code("cap_output_too_large"));
    }
    let mut captured = Vec::with_capacity(values.len());
    let mut minted = Vec::with_capacity(values.len());
    for (wanted, value) in values {
        let hosts = rules.send_hosts(caller, &wanted.field, host);
        let saved = match &wanted.keep_as {
            Some(name) => secrets.keep(caller, name, &value, &hosts),
            None => secrets.hold(caller, value, &hosts).map(|id| (id, true)),
        };
        let (secret, fresh) = match saved {
            Ok(saved) => saved,
            Err(error) => {
                for id in minted {
                    secrets.release(caller, id);
                }
                return Err(error);
            }
        };
        if fresh {
            minted.push(secret);
        }
        captured.push(Captured {
            field: wanted.field.clone(),
            secret,
        });
    }
    assert!(captured.len() <= REPLY_LIST_MAX);
    Ok((reply, captured))
}
