use super::*;
use rusqlite::{OptionalExtension, params};

const COOLDOWN_HISTORY_MAX_ROWS: i64 = 256;

pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS model_gateway_observations (
            id TEXT PRIMARY KEY NOT NULL,
            connection_revision TEXT NOT NULL,
            sequence INTEGER NOT NULL CHECK(sequence > 0),
            body TEXT NOT NULL CHECK(length(body) <= 16384),
            UNIQUE(connection_revision, sequence)
        );
        CREATE TABLE IF NOT EXISTS model_gateway_attempts (
            id TEXT PRIMARY KEY NOT NULL,
            task_id TEXT NOT NULL,
            previous_id TEXT UNIQUE,
            body TEXT NOT NULL CHECK(length(body) <= 32768),
            completion TEXT CHECK(length(completion) <= 4096),
            FOREIGN KEY(previous_id) REFERENCES model_gateway_attempts(id)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS model_gateway_active_task
            ON model_gateway_attempts(task_id) WHERE completion IS NULL;",
        )
        .map_err(error)?;
    Ok(())
}

fn encode(value: &impl Serialize, limit: usize) -> Result<String, EngineError> {
    let body = serde_json::to_string(value).map_err(error)?;
    if body.len() > limit {
        return Err(error("gateway_record_bounds"));
    }
    assert!(body.len() <= limit);
    assert!(!body.is_empty());
    Ok(body)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyObservation {
    id: RequestId,
    connection: ModelConnection,
    sequence: u64,
    observed_at_ms: u64,
    valid_until_ms: u64,
    kind: LegacyObservationKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum LegacyObservationKind {
    Ready {
        #[serde(rename = "remaining")]
        _remaining: Option<ResourceBudget>,
        capabilities: Vec<String>,
    },
}

fn decode_observation(body: &str) -> Result<Observation, EngineError> {
    if body.len() > 16384 {
        return Err(error("gateway_record_bounds"));
    }
    if let Ok(observation) = serde_json::from_str::<Observation>(body) {
        return Ok(observation);
    }
    let legacy: LegacyObservation = serde_json::from_str(body).map_err(error)?;
    let LegacyObservationKind::Ready { capabilities, .. } = legacy.kind;
    let observation = Observation {
        id: legacy.id,
        connection: legacy.connection,
        sequence: legacy.sequence,
        observed_at_ms: legacy.observed_at_ms,
        valid_until_ms: legacy.valid_until_ms,
        kind: ObservationKind::Ready {
            remaining: None,
            capabilities,
        },
    };
    assert!(body.len() <= 16384);
    assert!(matches!(
        observation.kind,
        ObservationKind::Ready {
            remaining: None,
            ..
        }
    ));
    Ok(observation)
}

fn decode_observation_row(
    id: &str,
    sequence: i64,
    body: &str,
    revision: &str,
) -> Result<Observation, EngineError> {
    let observation = decode_observation(body)?;
    if observation.id.as_str() != id
        || i64::try_from(observation.sequence).ok() != Some(sequence)
        || connection_revision(&observation.connection)? != revision
    {
        return Err(error("gateway_observation_identity"));
    }
    Ok(observation)
}

pub(super) fn latest_observation(
    connection: &Connection,
    revision: &str,
) -> Result<Option<Observation>, EngineError> {
    let row: Option<(String, i64, String)> = connection
        .query_row(
            "SELECT id, sequence, body FROM model_gateway_observations
         WHERE connection_revision = ?1 ORDER BY sequence DESC LIMIT 1",
            [revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    row.map(|(id, sequence, body)| decode_observation_row(&id, sequence, &body, revision))
        .transpose()
}

pub(super) fn active_cooldown(
    connection: &Connection,
    revision: &str,
    latest: &Observation,
    now_ms: u64,
) -> Result<Option<u64>, EngineError> {
    let sequence =
        i64::try_from(latest.sequence).map_err(|_| error("gateway_observation_bounds"))?;
    let mut statement = connection
        .prepare(
            "SELECT id, sequence, body FROM model_gateway_observations
             WHERE connection_revision = ?1 AND sequence < ?2
             ORDER BY sequence DESC LIMIT ?3",
        )
        .map_err(error)?;
    let mut rows = statement
        .query(params![revision, sequence, COOLDOWN_HISTORY_MAX_ROWS + 1])
        .map_err(error)?;
    let mut examined = 0;
    let mut newer_observed_at_ms = latest.observed_at_ms;
    let expired_before = now_ms.checked_sub(OBSERVATION_LIFETIME_MAX_MS);
    let mut cooldown = retry_at(&latest.kind).filter(|until| now_ms < *until);
    while let Some(row) = rows.next().map_err(error)? {
        if examined >= COOLDOWN_HISTORY_MAX_ROWS {
            return Err(error("gateway_cooldown_history_limit"));
        }
        let id: String = row.get(0).map_err(error)?;
        let sequence: i64 = row.get(1).map_err(error)?;
        let body: String = row.get(2).map_err(error)?;
        let observation = decode_observation_row(&id, sequence, &body, revision)?;
        validate_observation(&observation)?;
        if observation.observed_at_ms > newer_observed_at_ms {
            return Err(error("gateway_observation_stale"));
        }
        examined += 1;
        newer_observed_at_ms = observation.observed_at_ms;
        cooldown = cooldown.max(retry_at(&observation.kind).filter(|until| now_ms < *until));
        if expired_before.is_some_and(|cutoff| observation.observed_at_ms <= cutoff) {
            return Ok(cooldown);
        }
    }
    Ok(cooldown)
}

pub(super) fn insert_observation(
    tx: &Transaction<'_>,
    observation: &Observation,
) -> Result<(), EngineError> {
    let sequence =
        i64::try_from(observation.sequence).map_err(|_| error("gateway_observation_bounds"))?;
    if sequence <= 0 {
        return Err(error("gateway_observation_bounds"));
    }
    let body = encode(observation, 16384)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT body FROM model_gateway_observations WHERE id = ?1",
            [observation.id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(existing) = existing {
        return if body == existing {
            Ok(())
        } else {
            Err(error("gateway_observation_conflict"))
        };
    }
    let revision = connection_revision(&observation.connection)?;
    if let Some(previous) = latest_observation(tx, &revision)?
        && (observation.sequence <= previous.sequence
            || observation.observed_at_ms < previous.observed_at_ms)
    {
        return Err(error("gateway_observation_stale"));
    }
    let inserted = tx
        .execute(
            "INSERT INTO model_gateway_observations(id, connection_revision, sequence, body)
         VALUES(?1, ?2, ?3, ?4)",
            params![observation.id.as_str(), revision, sequence, body],
        )
        .map_err(error)?;
    assert_eq!(inserted, 1);
    assert_eq!(
        latest_observation(tx, &revision)?.as_ref(),
        Some(observation)
    );
    Ok(())
}

pub(super) fn validate_attempt_chain(
    tx: &Transaction<'_>,
    request: &AttemptRequest,
) -> Result<(), EngineError> {
    let active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM model_gateway_attempts WHERE task_id = ?1 AND completion IS NULL)",
        [request.task_id.as_str()], |row| row.get(0),
    ).map_err(error)?;
    if active {
        return Err(error("gateway_attempt_active"));
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM model_gateway_attempts WHERE task_id = ?1",
            [request.task_id.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    let count = u64::try_from(count).map_err(|_| error("gateway_attempt_count_bounds"))?;
    if request.previous_attempt.is_none() && count != 0 {
        return Err(error("gateway_previous_attempt_required"));
    }
    if count >= ATTEMPTS_MAX as u64 {
        return Err(error("gateway_attempt_limit"));
    }
    if let Some(previous) = &request.previous_attempt {
        let has_child: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM model_gateway_attempts WHERE previous_id = ?1)",
                [previous.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if has_child {
            return Err(error("gateway_attempt_already_continued"));
        }
    }
    assert!(!active);
    assert!(count < ATTEMPTS_MAX as u64);
    Ok(())
}

pub(super) fn insert_attempt(tx: &Transaction<'_>, attempt: &Attempt) -> Result<(), EngineError> {
    let body = encode(attempt, 32768)?;
    let inserted = tx.execute(
        "INSERT INTO model_gateway_attempts(id, task_id, previous_id, body) VALUES(?1, ?2, ?3, ?4)",
        params![attempt.request.id.as_str(), attempt.request.task_id.as_str(), attempt.request.previous_attempt.as_ref().map(RequestId::as_str), body],
    ).map_err(error)?;
    assert_eq!(inserted, 1);
    assert_eq!(
        read_attempt(tx, &attempt.request.id)?.as_ref(),
        Some(attempt)
    );
    Ok(())
}

pub(super) fn complete_attempt(
    tx: &Transaction<'_>,
    id: &RequestId,
    completion: &AttemptCompletion,
) -> Result<(), EngineError> {
    let body = encode(completion, 4096)?;
    let updated = tx.execute(
        "UPDATE model_gateway_attempts SET completion = ?1 WHERE id = ?2 AND completion IS NULL",
        params![body, id.as_str()],
    ).map_err(error)?;
    if updated != 1 {
        return Err(error("gateway_completion_conflict"));
    }
    assert_eq!(updated, 1);
    assert_eq!(
        read_attempt(tx, id)?.unwrap().completion.as_ref(),
        Some(completion)
    );
    Ok(())
}

pub(super) fn read_attempt(
    connection: &Connection,
    id: &RequestId,
) -> Result<Option<Attempt>, EngineError> {
    let row: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT body, completion FROM model_gateway_attempts WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    row.map(|(body, completion)| {
        let mut attempt: Attempt = serde_json::from_str(&body).map_err(error)?;
        if attempt.request.id != *id
            || attempt.connection_revision != connection_revision(&attempt.connection)?
            || attempt.completion.is_some()
        {
            return Err(error("gateway_attempt_corrupt"));
        }
        attempt.completion = completion
            .map(|body| serde_json::from_str(&body).map_err(error))
            .transpose()?;
        assert_eq!(&attempt.request.id, id);
        assert_eq!(
            attempt.connection_revision,
            connection_revision(&attempt.connection)?
        );
        Ok(attempt)
    })
    .transpose()
}
