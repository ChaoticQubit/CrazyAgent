use super::{GitHub, failure, publish, state};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// How long a Not now keeps the Overview from asking to publish a project: 7 days, in ms.
pub const SNOOZE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS github_publish_prompt(
    project TEXT PRIMARY KEY,
    snoozed_until INTEGER,
    never INTEGER
)";

/// The owner's answer to the Overview's "This project is not on GitHub. Publish it?":
/// Not now asks again after SNOOZE_MS, Don't ask again never asks for that project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubPromptAnswer {
    NotNow,
    Never,
}

fn unavailable<E>(_: E) -> EngineError {
    failure("github_store_unavailable")
}

/// Opens the engine database with the prompt table, made when missing. Input: the engine
/// root. Output: the connection, or `github_store_unavailable`.
fn open(root: &Path) -> Result<Connection, EngineError> {
    if !root.is_absolute() {
        return Err(failure("github_store_unavailable"));
    }
    let db = Connection::open(root.join("engine.db")).map_err(unavailable)?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(unavailable)?;
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    Ok(db)
}

impl GitHub {
    /// Tells whether a project's Overview asks to publish it on GitHub (DEC-182 plan 2h):
    /// the owner did not say Don't ask again, a Not now has run out, no publish is
    /// recorded, and the folder names no git remote. The kept answer and the record are
    /// read first, so only a project that may be asked has its git config read.
    /// Inputs: the project's id, its folder and the time now in ms. Output: true to ask,
    /// or `github_store_unavailable`.
    pub fn publish_prompt(
        &self,
        project: &ProjectId,
        folder: &Path,
        now_ms: i64,
    ) -> Result<bool, EngineError> {
        let kept: Option<(Option<i64>, Option<i64>)> = open(&self.root)?
            .query_row(
                "SELECT snoozed_until, never FROM github_publish_prompt WHERE project = ?1",
                params![project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(unavailable)?;
        let (snoozed_until, never) = kept.unwrap_or((None, None));
        if never.is_some_and(|never| never != 0)
            || snoozed_until.is_some_and(|until| now_ms < until)
        {
            return Ok(false);
        }
        if state::load_job(&self.root, folder)?.is_some() {
            return Ok(false);
        }
        Ok(publish::no_remote(
            &self.root,
            folder,
            self.endpoints.web_host(),
        ))
    }

    /// Keeps the owner's answer to a project's publish prompt: Not now asks again
    /// SNOOZE_MS from now, and Don't ask again stays whatever comes after it.
    /// Inputs: the project's id, the answer and the time now in ms. Output: nothing, or
    /// `github_store_unavailable`.
    pub fn publish_prompt_set(
        &self,
        project: &ProjectId,
        answer: GitHubPromptAnswer,
        now_ms: i64,
    ) -> Result<(), EngineError> {
        let db = open(&self.root)?;
        match answer {
            GitHubPromptAnswer::NotNow => db.execute(
                "INSERT INTO github_publish_prompt(project, snoozed_until, never) VALUES (?1, ?2, 0)
                 ON CONFLICT(project) DO UPDATE SET snoozed_until = excluded.snoozed_until",
                params![project.as_str(), now_ms.saturating_add(SNOOZE_MS)],
            ),
            GitHubPromptAnswer::Never => db.execute(
                "INSERT INTO github_publish_prompt(project, snoozed_until, never) VALUES (?1, NULL, 1)
                 ON CONFLICT(project) DO UPDATE SET never = 1",
                params![project.as_str()],
            ),
        }
        .map_err(unavailable)?;
        Ok(())
    }
}

/// Carries a moved project's publish job, if any, from its old folder to its new one, as
/// both moves rewrite the project's folder, so the moved project keeps its publish and a
/// recorded publish is still never asked. The engine root is the folder of the database
/// the move writes. A carry that fails is logged and the move still finishes; the job then
/// stays under the old folder. Inputs: that database and both folders. Output: none.
pub(crate) fn carry_publish(db: &Connection, from: &Path, to: &Path) {
    let _ = (db, from, to);
}

/// Saves a project's publish job where the engine keeps it, for rows outside the GitHub
/// module. Inputs: the engine root and the job. Output: none; a failed save panics.
#[cfg(test)]
pub(crate) fn keep_job(root: &Path, job: &super::GitHubPublishJob) {
    state::save_job(root, job).unwrap();
}

/// Reads a project's publish job from where the engine keeps it, for rows outside the
/// GitHub module. Inputs: the engine root and the project folder. Output: the job, if
/// any; a failed read panics.
#[cfg(test)]
pub(crate) fn kept_job(root: &Path, project: &Path) -> Option<super::GitHubPublishJob> {
    state::load_job(root, project).unwrap()
}
