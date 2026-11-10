use super::capture::{self, Attribution, Format, Tokens};
use super::storage;
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

pub(super) const BYTES_PER_STEP: u64 = 16 * 1024 * 1024;
const LISTING_COST: u64 = 4 * 1024 * 1024;
const CHUNK_COST_MIN: u64 = 64 * 1024;
const SEEDED_PER_STEP: i64 = 64_000;
const ATTEMPTS_MAX: i64 = 3;
const LINE_TIME_FLOOR_MS: i64 = 1_704_067_200_000;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_recount (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    state TEXT NOT NULL CHECK(state IN ('running','ready','used','kept','nothing')),
    started_ms INTEGER NOT NULL,
    decided_ms INTEGER,
    homes_partial INTEGER NOT NULL DEFAULT 0,
    seeded_to TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS usage_recount_homes (
    home TEXT PRIMARY KEY,
    format TEXT NOT NULL CHECK(format IN ('claude','codex')),
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    listed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_recount_homes_todo ON usage_recount_homes(listed) WHERE listed=0;
CREATE TABLE IF NOT EXISTS usage_recount_files (
    path TEXT PRIMARY KEY,
    home TEXT NOT NULL,
    session TEXT NOT NULL,
    fallback_ms INTEGER NOT NULL,
    progress TEXT NOT NULL DEFAULT '',
    done INTEGER NOT NULL DEFAULT 0,
    dev INTEGER NOT NULL DEFAULT 0,
    ino INTEGER NOT NULL DEFAULT 0,
    complete INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS usage_recount_files_todo ON usage_recount_files(done) WHERE done=0;
CREATE TABLE IF NOT EXISTS usage_recount_rows (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    project TEXT NOT NULL,
    pool TEXT NOT NULL,
    account TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    session TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    cost_nanos INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (hour_ms, kind, project, pool, account, agent, harness, session, model)
);
CREATE TABLE IF NOT EXISTS usage_recount_messages (
    id TEXT PRIMARY KEY,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    web INTEGER NOT NULL DEFAULT 0,
    last_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_recount_sessions (
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    session TEXT NOT NULL,
    PRIMARY KEY (project, agent, session)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_recount_live (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL
) WITHOUT ROWID;";

pub(super) struct Home {
    pub path: PathBuf,
    pub format: Format,
    pub attribution: Attribution,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecountStatus {
    pub old: Tokens,
    pub new: Tokens,
    pub new_cost_nanos: i64,
    pub sessions_recounted: u64,
    pub sessions_kept_old: u64,
    pub homes_partial: u64,
}

pub(super) fn state(db: &Connection) -> Result<Option<String>, EngineError> {
    db.query_row(
        "SELECT state FROM usage_recount WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(storage)
}

pub(super) fn started(db: &Connection) -> Result<bool, EngineError> {
    Ok(state(db)?.is_some())
}

pub(super) fn begin(_: &Connection, _: &[Home], _: i64) -> Result<(), EngineError> {
    Ok(())
}

pub(super) fn step(_: &Connection, _: u64, _: i64) -> Result<bool, EngineError> {
    Ok(false)
}

pub fn status(_: &Connection) -> Result<Option<RecountStatus>, EngineError> {
    Ok(None)
}
