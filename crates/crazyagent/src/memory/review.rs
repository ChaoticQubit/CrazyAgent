use super::*;

pub(super) fn owner(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    action: &HumanReview,
    revision: u64,
) -> Result<Outcome, EngineError> {
    match action {
        HumanReview::GlobalInbox { project_id, after } => {
            global_inbox(tx, config, project_id, after.as_ref())
        }
        HumanReview::Review {
            entry_id,
            version,
            decision,
        } => review(tx, config, entry_id, *version, decision, revision),
        HumanReview::ApproveGlobal {
            candidate_id,
            version,
            content_hash,
        } => approve(tx, candidate_id, *version, content_hash, revision),
        HumanReview::RevokeSource {
            project_id,
            source_id,
        } => revoke_source(tx, config, project_id, source_id, revision),
        HumanReview::DeleteActivity {
            project_id,
            activity_id,
        } => delete_activity(tx, config, project_id, activity_id, revision),
        HumanReview::Inbox { project_id, after } => inbox(tx, config, project_id, after.as_ref()),
        HumanReview::History {
            entry_id,
            after_version,
        } => history(tx, config, entry_id, *after_version),
    }
}

pub(super) fn nominations(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    p: &Principal,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT body FROM knowledge_nominations WHERE project=?1 AND id>?2 ORDER BY id LIMIT 4096").map_err(error)?;
    let rows = statement
        .query_map(
            params![p.project_id.as_str(), after.map_or("", RequestId::as_str)],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let mut nominations = Vec::new();
    let mut bytes = 0;
    for body in rows {
        let body = body.map_err(error)?;
        let n: Nomination = serde_json::from_str(&body).map_err(error)?;
        let mut live = true;
        for source in &n.source_ids {
            live &= source_live(tx, &p.project_id, source)?;
        }
        if !live {
            continue;
        }
        if bytes + body.len() > FRAME_BYTES_MAX - 2048 {
            break;
        }
        bytes += body.len();
        assert_eq!(n.principal.project_id, p.project_id);
        nominations.push(n);
        if nominations.len() >= config.settings.knowledge.batch_size {
            break;
        }
    }
    assert!(nominations.len() <= config.settings.knowledge.batch_size);
    assert!(bytes <= FRAME_BYTES_MAX - 2048);
    Ok(Outcome::Nominations { nominations })
}

fn global_inbox(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT g.body FROM knowledge_globals g JOIN knowledge_heads h ON g.entry=h.id
        WHERE h.project=?1 AND g.id>?2 ORDER BY g.id LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                after.map_or("", RequestId::as_str),
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let candidates = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<GlobalCandidate>, _>>()?;
    for candidate in &candidates {
        assert_eq!(&candidate.project_id, project);
        assert!(candidate.version > 0);
    }
    Ok(Outcome::GlobalCandidates { candidates })
}

fn delete_activity(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    activity_id: &RequestId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let source: Option<String> = tx
        .query_row(
            "SELECT source FROM knowledge_activity WHERE id=?1 AND project=?2",
            params![activity_id.as_str(), project.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(source) = source else {
        return reject("knowledge_source_denied");
    };
    let source = RequestId::new(source).map_err(error)?;
    if source_live(tx, project, &source)? {
        revoke_source(tx, config, project, &source, revision)?;
    }
    let count = tx
        .execute(
            "DELETE FROM knowledge_activity WHERE project=?1 AND source=?2",
            params![project.as_str(), source.as_str()],
        )
        .map_err(error)?;
    assert!(count > 0);
    assert!(!source_live(tx, project, &source)?);
    Ok(Outcome::Applied)
}

pub(super) fn review(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    version: u64,
    decision: &ReviewDecision,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let Some(mut entry) = load_entry(tx, id)? else {
        return reject("knowledge_entry_missing");
    };
    if entry.version != version {
        return reject("knowledge_version_conflict");
    }
    if !live_entry(tx, &entry)? {
        return reject("knowledge_source_revoked");
    }
    entry.status = match decision {
        ReviewDecision::Keep => KnowledgeStatus::Confirmed,
        ReviewDecision::Edit { text } => {
            if !memory_text_valid(text, config.settings.knowledge.text_bytes_max) {
                return reject("invalid_knowledge_text");
            }
            entry.text = text.clone();
            entry.content_hash = hash(text.as_bytes());
            KnowledgeStatus::Confirmed
        }
        ReviewDecision::Reject => KnowledgeStatus::Rejected,
        ReviewDecision::Remove => KnowledgeStatus::Removed,
    };
    entry.version += 1;
    entry.human_reviewed = true;
    entry.revision = Cursor::new(revision + 1);
    let mut reserved = config.settings.knowledge.clone();
    reserved.records_max = OWNER_RECORDS_MAX;
    save_entry(tx, &reserved, &entry)
}

fn approve(
    tx: &Transaction<'_>,
    id: &RequestId,
    version: u64,
    content_hash: &str,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM knowledge_globals WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(body) = body else {
        return reject("knowledge_candidate_missing");
    };
    let mut candidate: GlobalCandidate = serde_json::from_str(&body).map_err(error)?;
    let Some(entry) = load_entry(tx, &candidate.entry_id)? else {
        return reject("knowledge_entry_missing");
    };
    if candidate.version != version
        || candidate.content_hash != content_hash
        || entry.version != version
        || entry.content_hash != content_hash
    {
        return reject("knowledge_version_conflict");
    }
    if !live_entry(tx, &entry)? {
        return reject("knowledge_source_revoked");
    }
    candidate.approved_revision = Some(Cursor::new(revision + 1));
    tx.execute(
        "UPDATE knowledge_globals SET body=?1,approved=1 WHERE id=?2",
        params![
            serde_json::to_string(&candidate).map_err(error)?,
            id.as_str()
        ],
    )
    .map_err(error)?;
    assert_eq!(candidate.version, entry.version);
    assert!(candidate.approved_revision.is_some());
    Ok(Outcome::Applied)
}

fn revoke_source(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    source: &RequestId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    if !source_live(tx, project, source)? {
        return reject("knowledge_source_denied");
    }
    let mut statement = tx.prepare("SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h ON e.id=h.id AND e.version=h.version
        WHERE h.project=?1 ORDER BY h.id LIMIT 4096").map_err(error)?;
    let rows = statement
        .query_map([project.as_str()], |r| r.get::<_, String>(0))
        .map_err(error)?;
    for body in rows {
        let mut entry: KnowledgeEntry =
            serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        if entry.source_ids.contains(source)
            && matches!(
                entry.status,
                KnowledgeStatus::Provisional | KnowledgeStatus::Confirmed
            )
        {
            entry.version += 1;
            entry.status = KnowledgeStatus::Retracted;
            entry.revision = Cursor::new(revision + 1);
            let mut reserved = config.settings.knowledge.clone();
            reserved.records_max = OWNER_RECORDS_MAX;
            save_entry(tx, &reserved, &entry)?;
        }
    }
    tx.execute(
        "UPDATE knowledge_sources SET revoked=1 WHERE project=?1 AND id=?2",
        params![project.as_str(), source.as_str()],
    )
    .map_err(error)?;
    tx.execute(
        "DELETE FROM knowledge_jobs WHERE project=?1",
        [project.as_str()],
    )
    .map_err(error)?;
    assert!(!source_live(tx, project, source)?);
    assert!(!tx.is_autocommit());
    Ok(Outcome::Applied)
}

fn inbox(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    project: &ProjectId,
    after: Option<&RequestId>,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT e.body FROM knowledge_entries e JOIN knowledge_heads h ON e.id=h.id AND e.version=h.version
        WHERE h.project=?1 AND h.id>?2 ORDER BY h.id LIMIT ?3").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                after.map_or("", RequestId::as_str),
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let entries = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<KnowledgeEntry>, _>>()?;
    for entry in &entries {
        entry.assert_consistent();
        assert_eq!(&entry.project_id, project);
    }
    Ok(Outcome::Knowledge {
        entries: bounded_entries(entries)?,
    })
}

fn history(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    id: &RequestId,
    after: u64,
) -> Result<Outcome, EngineError> {
    let mut statement = tx.prepare("SELECT body FROM knowledge_entries WHERE id=?1 AND version>?2 ORDER BY version LIMIT ?3").map_err(error)?;
    let rows = statement
        .query_map(
            params![
                id.as_str(),
                after.min(i64::MAX as u64) as i64,
                config.settings.knowledge.batch_size as i64
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(error)?;
    let entries = rows
        .map(|body| serde_json::from_str(&body.map_err(error)?).map_err(error))
        .collect::<Result<Vec<KnowledgeEntry>, _>>()?;
    for entry in &entries {
        entry.assert_consistent();
        assert_eq!(&entry.id, id);
    }
    Ok(Outcome::Knowledge {
        entries: bounded_entries(entries)?,
    })
}
