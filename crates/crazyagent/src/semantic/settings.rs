use super::{ModelState, SENDS, SearchChoice, SearchOption, SearchView};
use crate::app_plugins::manifest::Runs;
use crate::app_plugins::store::{self, Loaded};
use crate::app_plugins::{LOCAL_PLUGIN, PluginStatus, Problem, RunsView};
use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS search_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    version INTEGER NOT NULL CHECK(version >= 0),
    plugin TEXT CHECK(plugin IS NULL OR length(plugin) BETWEEN 1 AND 64),
    host TEXT CHECK(host IS NULL OR length(host) BETWEEN 1 AND 255),
    sha256 TEXT CHECK(sha256 IS NULL OR length(sha256) = 64),
    note TEXT CHECK(note IS NULL OR length(note) BETWEEN 1 AND 64)
)";

const REMOVED: &str = "search_choice_removed";

struct Saved {
    version: u64,
    plugin: Option<String>,
    host: Option<String>,
    sha256: Option<String>,
    note: Option<String>,
}

fn error(code: &'static str) -> EngineError {
    EngineError(code.into())
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    error("app_plugins_unavailable")
}

fn load(db: &Connection) -> Result<Saved, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let row = db
        .query_row(
            "SELECT version, plugin, host, sha256, note FROM search_settings WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite)?;
    Ok(match row {
        None => Saved {
            version: 0,
            plugin: Some(LOCAL_PLUGIN.to_owned()),
            host: None,
            sha256: None,
            note: None,
        },
        Some((version, plugin, host, sha256, note)) => Saved {
            version: u64::try_from(version).map_err(|_| error("app_plugins_unavailable"))?,
            plugin,
            host,
            sha256,
            note,
        },
    })
}

fn save(db: &Connection, current: u64, saved: &Saved) -> Result<bool, EngineError> {
    let current = i64::try_from(current).map_err(|_| error("app_plugins_unavailable"))?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| error("app_plugins_unavailable"))?;
    let changed = db
        .execute(
            "INSERT INTO search_settings (id, version, plugin, host, sha256, note)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET version=excluded.version, plugin=excluded.plugin,
             host=excluded.host, sha256=excluded.sha256, note=excluded.note
             WHERE search_settings.version=?6",
            params![
                next,
                saved.plugin,
                saved.host,
                saved.sha256,
                saved.note,
                current
            ],
        )
        .map_err(sqlite)?;
    Ok(changed == 1)
}

fn http_host(loaded: &Loaded) -> Option<&str> {
    match &loaded.manifest.runs {
        Runs::Local { .. } => None,
        Runs::Http(http) => http.url.host_str(),
    }
}

fn confirmed(saved: &Saved, loaded: &Loaded) -> bool {
    match http_host(loaded) {
        None => true,
        Some(host) => {
            saved.host.as_deref() == Some(host)
                && saved.sha256.as_deref() == Some(loaded.manifest.sha256.as_str())
        }
    }
}

/// The plugin agents' search embeds with now, if any: the saved choice, still ready to
/// run, and for an HTTP plugin still the exact file the owner confirmed at its host (its
/// trust hash, so a same-name replacement needs a new confirm). Read it before every request
/// so turning it off or choosing full text stops sends at once. Inputs: database and engine
/// root. Output: the loaded plugin, or none for full text or any refusal.
pub fn chosen(db: &Connection, root: &Path) -> Option<Loaded> {
    let saved = load(db).ok()?;
    let loaded = store::ready(db, root, saved.plugin.as_deref()?).ok()?;
    confirmed(&saved, &loaded).then_some(loaded)
}

fn reason(code: &'static str) -> Option<String> {
    Some(Problem::new(code, None, None).message)
}

fn model_reason(model: &ModelState) -> Option<String> {
    match model {
        ModelState::Ready { .. } => None,
        ModelState::NotDownloaded { .. } => reason("search_model_missing"),
        ModelState::Downloading { .. } => reason("search_model_downloading"),
        ModelState::Failed { .. } => reason("search_model_download_failed"),
    }
}

/// Turns Search back to full text when the plugin it names is removed, with a version bump
/// and a note for the page, so a later plugin of that name is never chosen without a new
/// confirm (rv-v3sec PL-1). Inputs: database and the removed plugin's name. Output: Ok,
/// including when the choice names another plugin.
pub fn forget(db: &Connection, name: &str) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    db.execute(
        "UPDATE search_settings SET version=version+1, plugin=NULL, host=NULL, sha256=NULL,
         note=?2 WHERE id=1 AND plugin=?1",
        params![name, REMOVED],
    )
    .map_err(sqlite)?;
    Ok(())
}

fn unlisted(name: &str, refused: Option<String>) -> SearchOption {
    let code = if refused.is_some() {
        "app_plugin_refused"
    } else {
        "app_plugin_not_found"
    };
    SearchOption {
        name: name.to_owned(),
        title: refused.unwrap_or_else(|| name.to_owned()),
        runs: None,
        usable: false,
        reason: reason(code),
        sends: Vec::new(),
    }
}

/// The Search settings page: the saved choice at its version, every enabled embedding
/// plugin plus the chosen one, each with whether it can embed now and the plain reason
/// when not (a chosen plugin that is gone or refused is listed with no runs), a note when
/// the choice fell back to full text, the local model's download state, and the embedding
/// progress. Inputs: database and engine root. Output: the page, or the folder's refusal.
pub fn view(db: &Connection, root: &Path) -> Result<SearchView, EngineError> {
    let saved = load(db)?;
    let listing = store::list(db, root)?;
    let model = super::local::state(root);
    let mut options = Vec::new();
    let mut chosen_refused = None;
    for plugin in listing.plugins {
        let is_chosen = saved.plugin.as_deref() == Some(plugin.name.as_str());
        let Some(runs) = plugin.runs else {
            if is_chosen {
                chosen_refused = Some(plugin.title);
            }
            continue;
        };
        if !plugin.enabled && !is_chosen {
            continue;
        }
        let unconfirmed = is_chosen
            && matches!(&runs, RunsView::Http { url, .. }
                if url::Url::parse(url).ok().as_ref().and_then(url::Url::host_str)
                    != saved.host.as_deref()
                    || saved.sha256.as_deref() != Some(plugin.sha256.as_str()));
        let why = if !plugin.enabled {
            reason("app_plugin_off")
        } else {
            match (&plugin.status, &runs) {
                (PluginStatus::NeedsReview, _) => reason("app_plugin_needs_review"),
                (PluginStatus::NeedsKey, _) => reason("app_plugin_needs_key"),
                (PluginStatus::Refused { .. }, _) => reason("app_plugin_refused"),
                (PluginStatus::Ready, _) if unconfirmed => reason("search_confirm_required"),
                (PluginStatus::Ready, RunsView::Local { .. }) => model_reason(&model),
                (PluginStatus::Ready, RunsView::Http { .. }) => None,
            }
        };
        let sends = match runs {
            RunsView::Local { .. } => Vec::new(),
            RunsView::Http { .. } => SENDS.iter().map(|send| (*send).to_owned()).collect(),
        };
        options.push(SearchOption {
            name: plugin.name,
            title: plugin.title,
            usable: why.is_none(),
            reason: why,
            runs: Some(runs),
            sends,
        });
    }
    if let Some(name) = saved.plugin.as_deref()
        && options.iter().all(|option| option.name != name)
    {
        options.push(unlisted(name, chosen_refused));
    }
    let note = match saved.note.as_deref() {
        Some(REMOVED) => reason(REMOVED),
        _ => None,
    };
    let choice = match saved.plugin {
        None => SearchChoice::FullText,
        Some(name) => SearchChoice::Plugin { name },
    };
    Ok(SearchView {
        version: saved.version,
        choice,
        options,
        note,
        model,
        progress: super::progress(db, root),
    })
}

/// Saves the Search choice. An HTTP plugin is chosen only with a confirm naming its URL's
/// host, and the confirm holds for that exact file (its trust hash); the local plugin may
/// be chosen before its model is downloaded. A save clears any fallback note. Inputs:
/// database, engine root, the choice, the host the owner confirmed and the version the page
/// showed. Output: the new page, the current version when the page was stale, or
/// `search_choice_unavailable` / `search_confirm_required`.
pub fn set(
    db: &Connection,
    root: &Path,
    choice: &SearchChoice,
    confirm_host: Option<&str>,
    version: u64,
) -> Result<Result<SearchView, u64>, EngineError> {
    let saved = load(db)?;
    if saved.version != version {
        return Ok(Err(saved.version));
    }
    let next = match choice {
        SearchChoice::FullText => Saved {
            version,
            plugin: None,
            host: None,
            sha256: None,
            note: None,
        },
        SearchChoice::Plugin { name } => {
            let loaded =
                store::ready(db, root, name).map_err(|_| error("search_choice_unavailable"))?;
            let host = http_host(&loaded).map(str::to_owned);
            if host.is_some() && host.as_deref() != confirm_host {
                return Err(error("search_confirm_required"));
            }
            Saved {
                version,
                sha256: host.is_some().then(|| loaded.manifest.sha256.clone()),
                plugin: Some(loaded.manifest.name),
                host,
                note: None,
            }
        }
    };
    if !save(db, version, &next)? {
        return Ok(Err(load(db)?.version));
    }
    view(db, root).map(Ok)
}

#[cfg(all(test, unix))]
#[path = "settings_tests.rs"]
mod tests;
