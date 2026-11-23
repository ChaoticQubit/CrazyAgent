use super::request::{Capture, Captured};
use crate::error::StoreError;
use crate::secret::{SecretString, wipe};
use crate::secrets::Secrets;
use std::collections::BTreeMap;

/// The most reply fields one manifest row, or one request, names for capture.
pub const CAPTURES_MAX: usize = 4;
/// The most bytes of one capture field name.
pub const FIELD_BYTES_MAX: usize = 64;

/// The reply fields each plugin's manifest names as always captured, per host.
pub struct CaptureRules {
    by_plugin_host: BTreeMap<(&'static str, String), Vec<String>>,
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
        Ok(Self { by_plugin_host })
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

/// Takes the captured fields out of a reply body into handles. Inputs: the secrets
/// module, the plugin, the fields and the raw body, which is wiped. Output: the body the
/// plugin gets, re-serialized without the captured fields, and their handles; or
/// `net_capture_not_json` when a field is to be captured and the body is not one JSON
/// object or a present field is not a string, so no body reaches the plugin.
pub fn take(
    secrets: &Secrets<'_>,
    caller: &'static str,
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
    let mut captured = Vec::with_capacity(values.len());
    for (wanted, value) in values {
        let secret = match &wanted.keep_as {
            Some(name) => secrets.keep(caller, name, &value)?,
            None => secrets.hold(caller, value)?,
        };
        captured.push(Captured {
            field: wanted.field.clone(),
            secret,
        });
    }
    let reply =
        serde_json::to_vec(&object).map_err(|_| StoreError::code("net_capture_not_json"))?;
    Ok((reply, captured))
}
