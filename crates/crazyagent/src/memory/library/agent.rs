use super::read::scoped;
use super::*;

/// Reads the owner's Where agents save memories setting, This project only until the owner
/// chose. Input: the engine connection. Output: the choice.
pub(crate) fn agent_scope(connection: &rusqlite::Connection) -> Result<AgentScope, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT scope FROM memory_agent_scope WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(match stored.as_deref() {
        Some("agent") => AgentScope::Agent,
        _ => AgentScope::Project,
    })
}

/// Reads the owner's Agent memory writes setting, Save directly until the owner chose.
/// Input: the engine connection. Output: the choice.
pub(crate) fn agent_writes(connection: &rusqlite::Connection) -> Result<AgentWrites, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT agent_writes FROM memory_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(match stored.as_deref() {
        Some("ask") => AgentWrites::Ask,
        _ => AgentWrites::Save,
    })
}

/// Saves one memory an agent gave with its `remember` tool, with the agent named as its
/// origin: held for the owner's approval under Ask me first, else saved at once, and an
/// app-wide ask kept in the agent's project under This project only. The text is untrusted
/// and checked like a hand-written note. Inputs: the engine connection, the agent's project,
/// its label, whether it asked for app-wide and the text. Output: what was stored, or a
/// refusal code `plain_problem` explains.
pub(crate) fn agent_remember(
    connection: &mut rusqlite::Connection,
    project: &ProjectId,
    agent: &str,
    app: bool,
    text: &str,
) -> Result<Result<AgentSaved, String>, EngineError> {
    let text = text.trim();
    if let Err(problem) = validate_note(text) {
        return Ok(Err(problem.0));
    }
    let agent = agent.trim();
    if agent.is_empty()
        || agent.len() > AGENT_LABEL_BYTES_MAX
        || agent.chars().any(char::is_control)
    {
        return Ok(Err("memory_agent_invalid".into()));
    }
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    if !project_exists(&tx, project)? {
        return Ok(Err("memory_project_missing".into()));
    }
    let narrowed = app && agent_scope(&tx)? == AgentScope::Project;
    let scope = if app && !narrowed {
        MemoryScope::Global
    } else {
        MemoryScope::Project {
            project_id: project.clone(),
        }
    };
    let digest = hash(format!("agent\n{}\n{text}", scope.project().unwrap_or("")).as_bytes());
    let id = RequestId::new(format!("agent-{}", &digest[..32])).map_err(error)?;
    if let Some(code) = blocked(&tx, &id, &scope, text, "memory_duplicate")? {
        return Ok(Err(code.into()));
    }
    let ask = agent_writes(&tx)? == AgentWrites::Ask;
    let origin = if ask {
        MemoryOrigin::Requested {
            agent: agent.into(),
        }
    } else {
        MemoryOrigin::Agent {
            agent: agent.into(),
        }
    };
    insert(&tx, &id, &scope, &origin, text)?;
    tx.commit().map_err(error)?;
    assert!(!narrowed || scope.project().is_some());
    Ok(Ok(AgentSaved {
        waiting: ask,
        narrowed,
    }))
}

/// Finds the saved memories an agent of a project may read for its `recall` tool: the
/// project's and the app-wide ones not waiting for the owner's approval, whose text holds
/// the query, newest first, at most `RECALL_ITEMS_MAX`. Inputs: the engine connection, the
/// project and the untrusted query, blank for the newest. Output: the memories, or a refusal
/// code for a query that is not one line of up to 100 bytes.
pub(crate) fn recall(
    connection: &mut rusqlite::Connection,
    project: &ProjectId,
    query: &str,
) -> Result<Result<Vec<SavedMemory>, String>, EngineError> {
    let query = query.trim();
    let search = (!query.is_empty()).then_some(query);
    if let Err(problem) = validate_page(search, PAGE_ITEMS_MAX) {
        return Ok(Err(problem.0));
    }
    let tx = connection.transaction().map_err(error)?;
    let mut found = Vec::new();
    for scope in [
        MemoryScope::Project {
            project_id: project.clone(),
        },
        MemoryScope::Global,
    ] {
        found.extend(
            scoped(&tx, &scope, search, None, SCOPE_ITEMS_MAX)?
                .into_iter()
                .filter(|memory| {
                    !memory.origin.waiting() && !crate::memory::hidden_text(&memory.text)
                }),
        );
    }
    found.sort_by_key(|memory| std::cmp::Reverse(memory.ordinal));
    found.truncate(RECALL_ITEMS_MAX);
    assert!(found.iter().all(|memory| !memory.origin.waiting()));
    Ok(Ok(found))
}
