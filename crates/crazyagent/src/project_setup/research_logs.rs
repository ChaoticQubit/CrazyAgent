use super::*;

/// Reads the last plain lines of one research job's agent output, with secrets hidden.
/// Takes the engine database path, whose folder holds the research runner's agent state,
/// and the job id. Returns the newest lines within RESEARCH_LOG_LINES_MAX and
/// RESEARCH_LOG_BYTES_MAX, so the reply fits one owner frame; none before the first run.
#[cfg(unix)]
pub fn research_log(database: &Path, job: i64) -> Result<Vec<String>, EngineError> {
    assert!(job > 0);
    let agents = database
        .parent()
        .ok_or_else(|| failure("runtime_parent_missing"))?
        .join("research")
        .join("agents");
    let Ok(directory) = rustix::fs::open(
        agents.as_path(),
        open_flags() | OFlags::DIRECTORY,
        Mode::empty(),
    ) else {
        return Ok(Vec::new());
    };
    let Some(run) = latest_run(&agents, job)? else {
        return Ok(Vec::new());
    };
    let Ok(run) = open_directory(&directory, &run) else {
        return Ok(Vec::new());
    };
    let mut lines = Vec::new();
    for name in RESEARCH_LOG_FILES {
        let Ok(fd) = rustix::fs::openat(&run, name, open_flags(), Mode::empty()) else {
            continue;
        };
        let Ok((mut file, bytes)) = regular(fd, "research_log_unreadable") else {
            continue;
        };
        let start = bytes.saturating_sub(RESEARCH_LOG_TAIL_BYTES);
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start)).map_err(error)?;
        let mut tail = Vec::new();
        file.take(RESEARCH_LOG_TAIL_BYTES)
            .read_to_end(&mut tail)
            .map_err(error)?;
        let text = String::from_utf8_lossy(&tail);
        let mut rows = text.lines();
        if start > 0 {
            rows.next();
        }
        lines.extend(rows.filter_map(plain_log_line));
    }
    let mut kept = 0;
    let mut bytes = 0;
    for line in lines.iter().rev() {
        if kept == RESEARCH_LOG_LINES_MAX || bytes + line.len() > RESEARCH_LOG_BYTES_MAX {
            break;
        }
        kept += 1;
        bytes += line.len();
    }
    let lines = lines.split_off(lines.len() - kept);
    assert!(lines.len() <= RESEARCH_LOG_LINES_MAX);
    assert!(lines.iter().map(String::len).sum::<usize>() <= RESEARCH_LOG_BYTES_MAX);
    Ok(lines)
}

/// Refuses research job output where no directory-relative no-follow read exists.
/// Takes the engine database path and job id, both unused.
/// Returns `research_log_unavailable`.
#[cfg(not(unix))]
pub fn research_log(_database: &Path, _job: i64) -> Result<Vec<String>, EngineError> {
    Err(failure("research_log_unavailable"))
}

/// Names the newest run folder of a research job in the runner's agent state.
/// Takes the agents folder and the job id; run folders are named job-<id>-<attempt> with an
/// optional -<launch>. Returns the highest attempt and launch, or none before any run.
#[cfg(unix)]
fn latest_run(agents: &Path, job: i64) -> Result<Option<String>, EngineError> {
    let prefix = format!("job-{job}-");
    let mut latest: Option<((u32, u32), String)> = None;
    for entry in std::fs::read_dir(agents)
        .map_err(error)?
        .take(RESEARCH_RUNS_SCANNED_MAX)
    {
        let Ok(name) = entry.map_err(error)?.file_name().into_string() else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let mut parts = rest.split('-');
        let attempt = parts.next().and_then(|part| part.parse::<u32>().ok());
        let launch = parts
            .next()
            .map_or(Some(0), |part| part.parse::<u32>().ok());
        let (Some(attempt), Some(launch), None) = (attempt, launch, parts.next()) else {
            continue;
        };
        if latest
            .as_ref()
            .is_none_or(|(order, _)| (attempt, launch) > *order)
        {
            latest = Some(((attempt, launch), name));
        }
    }
    Ok(latest.map(|(_, name)| name))
}

/// Turns one raw agent log line into plain text with likely secrets hidden.
/// Takes the raw line; a JSON event gives its message text and textless events are skipped.
/// Returns the bounded plain line, or none for an empty or textless line.
#[cfg(unix)]
fn plain_log_line(raw: &str) -> Option<String> {
    let text = match serde_json::from_str::<serde_json::Value>(raw.trim()) {
        Ok(event) if event.is_object() => event_text(&event)?,
        _ => raw.to_owned(),
    };
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let line = redact(text);
    if line.chars().count() <= RESEARCH_LOG_LINE_CHARS_MAX {
        return Some(line);
    }
    let mut cut: String = line.chars().take(RESEARCH_LOG_LINE_CHARS_MAX).collect();
    cut.push_str("...");
    Some(cut)
}

#[cfg(unix)]
fn event_text(event: &serde_json::Value) -> Option<String> {
    [Some(event), event.get("item"), event.get("msg")]
        .into_iter()
        .flatten()
        .find_map(|scope| {
            ["text", "message", "command"]
                .into_iter()
                .find_map(|key| scope.get(key)?.as_str().map(str::to_owned))
        })
}
