use rusqlite::{Connection, OptionalExtension, params};

use crate::Refused;
use crate::embedding::Manifest;
use crate::registry::{self, Engine};
use crate::store::{binding_rows, drop_binding, refused, sqlite};
use crate::views::{Origin, TrustView};

const HOSTS_KEY_BYTES_MAX: usize = 4096;

/// Every host a plugin reaches, sorted, as the trust record keeps it: a bound key stays
/// only while this is unchanged. Input: the manifest. Output: the hosts, one per line.
pub(crate) fn hosts_key(manifest: &Manifest) -> String {
    let mut hosts = manifest.http.hosts.clone();
    hosts.sort();
    let key = hosts.join("\n");
    assert!(key.len() <= HOSTS_KEY_BYTES_MAX);
    key
}

/// Whether the owner trusted this exact text: a bundled plugin by shipping, a user plugin
/// while its trust row holds the text's SHA-256. Inputs: the database, where it is from and
/// the manifest. Output: the answer, or `app_plugins_unavailable`.
pub(crate) fn reviewed(
    db: &Connection,
    origin: Origin,
    manifest: &Manifest,
) -> Result<bool, Refused> {
    if origin == Origin::Bundled {
        return Ok(true);
    }
    let trusted: Option<String> = db
        .query_row(
            "SELECT sha256 FROM app_plugin_trust WHERE name=?1",
            [manifest.name.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    Ok(trusted.as_deref() == Some(manifest.sha256.as_str()))
}

/// The trust screen for a re-review (owner O1): whether the text changed since the trust
/// row, and the hosts it reaches that the row does not hold, listed first. A plugin with no
/// trust row reads as a first review: unchanged, nothing grew. Inputs: the database, the
/// manifest and whether it replaces a user plugin. Output: the trust screen, or
/// `app_plugins_unavailable`.
pub(crate) fn trust_view(
    db: &Connection,
    manifest: &Manifest,
    replaces: bool,
) -> Result<TrustView, Refused> {
    let mut view = manifest.trust_view(replaces);
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT sha256, hosts FROM app_plugin_trust WHERE name=?1",
            [manifest.name.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite)?;
    if let Some((sha256, hosts)) = row {
        let trusted: Vec<&str> = hosts.split('\n').filter(|host| !host.is_empty()).collect();
        let grew: Vec<String> = manifest
            .http
            .hosts
            .iter()
            .filter(|host| !trusted.contains(&host.as_str()))
            .cloned()
            .collect();
        view.changed = sha256 != manifest.sha256;
        if let crate::views::RunsView::Http { hosts, .. } = &mut view.runs {
            hosts.sort_by_key(|host| !grew.contains(host));
        }
        view.grew_hosts = grew;
    }
    assert!(view.grew_hosts.len() <= manifest.http.hosts.len());
    Ok(view)
}

/// Writes the trust row for the text the owner confirmed and turns the plugin on. Keys bound
/// to an older version stay only while its hosts are unchanged and the secret is still
/// declared. Inputs: database, engine facts, the manifest and the time. Output: Ok, or
/// `app_plugins_unavailable`, `app_plugin_key_store_unavailable`.
pub(crate) fn settle(
    db: &Connection,
    engine: &Engine<'_>,
    manifest: &Manifest,
    now_ms: i64,
) -> Result<(), Refused> {
    let hosts = hosts_key(manifest);
    let trusted_hosts: Option<String> = db
        .query_row(
            "SELECT hosts FROM app_plugin_trust WHERE name=?1",
            [manifest.name.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    let moved = trusted_hosts.as_deref() != Some(hosts.as_str());
    for (secret, source, reference) in binding_rows(db, &manifest.name)? {
        if moved || !manifest.secrets().contains(&secret) {
            drop_binding(db, engine, &manifest.name, &secret, &source, &reference)?;
        }
    }
    db.execute(
        "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET sha256=excluded.sha256, hosts=excluded.hosts,
         trusted_ms=excluded.trusted_ms",
        params![manifest.name, manifest.sha256, hosts, now_ms],
    )
    .map_err(sqlite)?;
    registry::set_flag(db, &manifest.name, true)
}

/// Trusts a user plugin folder as the owner reviewed it and turns it on. Inputs: database,
/// engine facts, plugin name, the SHA-256 its trust screen showed, and the time. Output: Ok,
/// or `app_plugin_changed` when the plugin.toml changed since, `_refused`, `_bundled`,
/// `_not_found`.
pub fn trust_file(
    db: &Connection,
    engine: &Engine<'_>,
    name: &str,
    sha256: &str,
    now_ms: i64,
) -> Result<(), Refused> {
    registry::prepare(db, engine)?;
    if registry::is_bundled(name) {
        return Err(refused("app_plugin_bundled"));
    }
    let manifest = registry::found(engine, name)?
        .parsed
        .map_err(|_| refused("app_plugin_refused"))?;
    if manifest.sha256 != sha256 {
        return Err(refused("app_plugin_changed"));
    }
    settle(db, engine, &manifest, now_ms)
}
