use super::*;

fn row(
    ordinal: i64,
    id: String,
    project: Option<String>,
    origin: String,
    text: String,
) -> Result<SavedMemory, EngineError> {
    let scope = match project {
        None => MemoryScope::Global,
        Some(project) => MemoryScope::Project {
            project_id: ProjectId::new(project).map_err(error)?,
        },
    };
    let memory = SavedMemory {
        id: RequestId::new(id).map_err(error)?,
        ordinal: u64::try_from(ordinal).map_err(error)?,
        scope,
        origin: serde_json::from_str(&origin).map_err(error)?,
        text,
    };
    assert!(memory.ordinal > 0);
    assert!(!memory.text.is_empty() && memory.text.len() <= TEXT_BYTES_MAX);
    Ok(memory)
}

/// Reads one saved memory by id for the owner's detail view.
/// Input: the transaction and memory id. Output: the memory, or None once deleted.
pub(crate) fn get(
    tx: &Transaction<'_>,
    id: &RequestId,
) -> Result<Option<SavedMemory>, EngineError> {
    tx.query_row(
        "SELECT ordinal,id,project,origin,text FROM memory_library WHERE id=?1",
        [id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
    .map_err(error)?
    .map(|(ordinal, id, project, origin, text)| row(ordinal, id, project, origin, text))
    .transpose()
}

pub(super) fn scoped(
    tx: &Transaction<'_>,
    scope: &MemoryScope,
    search: Option<&str>,
    before: Option<u64>,
    limit: usize,
) -> Result<Vec<SavedMemory>, EngineError> {
    assert!(limit <= SCOPE_ITEMS_MAX + 1);
    let before = before.map_or(i64::MAX, |n| i64::try_from(n).unwrap_or(i64::MAX));
    let mut statement = tx
        .prepare(
            "SELECT ordinal,id,project,origin,text FROM memory_library
            WHERE project IS ?1 AND ordinal<?2 AND (?3 IS NULL OR instr(lower(text),lower(?3))>0)
            ORDER BY ordinal DESC LIMIT ?4",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![scope.project(), before, search, limit as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(error)?;
    let mut items = Vec::new();
    for found in rows {
        let (ordinal, id, project, origin, text) = found.map_err(error)?;
        items.push(row(ordinal, id, project, origin, text)?);
    }
    assert!(items.len() <= limit);
    assert!(
        items
            .windows(2)
            .all(|pair| pair[0].ordinal > pair[1].ordinal)
    );
    Ok(items)
}

/// Lists saved memories in one scope, newest first, with optional search.
/// Input: scope, case-insensitive search text, the `next` cursor of the previous
/// page and the page size. Output: owner data holding one bounded page.
pub(crate) fn page(
    tx: &Transaction<'_>,
    scope: &MemoryScope,
    search: Option<&str>,
    before: Option<u64>,
    limit: u8,
) -> Result<OwnerData, EngineError> {
    validate_page(search, limit)?;
    if let MemoryScope::Project { project_id } = scope
        && !project_exists(tx, project_id)?
    {
        return Err(error("memory_project_missing"));
    }
    let found = scoped(tx, scope, search, before, usize::from(limit) + 1)?;
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut more = found.len() > usize::from(limit);
    for memory in found.into_iter().take(usize::from(limit)) {
        let size = serde_json::to_vec(&memory).map_err(error)?.len() + 1;
        if bytes + size > PAGE_BYTES_MAX {
            more = true;
            break;
        }
        bytes += size;
        items.push(memory);
    }
    let next = if more {
        items.last().map(|memory| memory.ordinal)
    } else {
        None
    };
    assert!(items.len() <= usize::from(limit));
    assert!(bytes <= PAGE_BYTES_MAX);
    Ok(OwnerData::Memories {
        page: MemoryPage {
            items,
            next,
            agent_writes: agent_writes(tx)?,
            agent_scope: agent_scope(tx)?,
        },
    })
}

fn kept_research(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<Vec<KnowledgeEntry>, EngineError> {
    let mut statement = tx
        .prepare(
            "SELECT e.body FROM knowledge_heads h JOIN knowledge_entries e ON h.id=e.id AND h.version=e.version
            WHERE h.project=?1 ORDER BY h.id LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), RESEARCH_ROWS_MAX], |r| {
            r.get::<_, String>(0)
        })
        .map_err(error)?;
    let mut entries = Vec::new();
    for body in rows {
        let entry: KnowledgeEntry = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
        entry.assert_consistent();
        if entry.status == KnowledgeStatus::Confirmed && live_entry(tx, &entry)? {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|a| std::cmp::Reverse(a.revision.value()));
    assert!(entries.iter().all(|e| &e.project_id == project));
    Ok(entries)
}

fn kind_of(origin: &MemoryOrigin) -> ApprovedKind {
    match origin {
        MemoryOrigin::Written
        | MemoryOrigin::Imported { .. }
        | MemoryOrigin::Agent { .. }
        | MemoryOrigin::Requested { .. } => ApprovedKind::Note,
        MemoryOrigin::Promoted { .. } => ApprovedKind::Promoted,
        MemoryOrigin::Selected { .. } => ApprovedKind::Selection,
    }
}

/// Returns the bounded owner-approved memories an agent in this project may read.
/// Input: the engine transaction and project id. Output: project notes and kept
/// project research first, then global memory, newest first, within the launch bounds.
/// A memory saved before the hidden-text check that still holds hidden characters is
/// never delivered.
pub(crate) fn approved_for_project(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<ApprovedMemories, EngineError> {
    let mut collector = Collector {
        items: Vec::new(),
        seen: BTreeSet::new(),
        bytes: 0,
        complete: true,
    };
    let local = MemoryScope::Project {
        project_id: project.clone(),
    };
    for memory in scoped(tx, &local, None, None, SCOPE_ITEMS_MAX)?
        .into_iter()
        .filter(|memory| !memory.origin.waiting())
    {
        collector.push(
            ApprovedScope::Project,
            kind_of(&memory.origin),
            &memory.text,
        );
    }
    for entry in kept_research(tx, project)? {
        collector.push(ApprovedScope::Project, ApprovedKind::Research, &entry.text);
    }
    for memory in scoped(tx, &MemoryScope::Global, None, None, SCOPE_ITEMS_MAX)?
        .into_iter()
        .filter(|memory| !memory.origin.waiting())
    {
        collector.push(ApprovedScope::Global, kind_of(&memory.origin), &memory.text);
    }
    let memories = ApprovedMemories {
        items: collector.items,
        complete: collector.complete,
    };
    assert!(memories.items.len() <= LAUNCH_ITEMS_MAX);
    assert!(memories.render().len() <= LAUNCH_BYTES_MAX + RENDER_HEADER_BYTES_MAX);
    Ok(memories)
}
