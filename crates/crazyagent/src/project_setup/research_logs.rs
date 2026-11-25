use super::*;

/// Reads the last plain lines of one research job's agent output, with secrets hidden, from
/// its newest run by folder name: the legacy read for a job whose record names no run, as one
/// last run before the engine kept the name. Takes the engine database path, whose folder
/// holds the research runner's agent state, and the job id. Returns the newest lines within
/// RESEARCH_LOG_LINES_MAX and RESEARCH_LOG_BYTES_MAX; none before the first run.
#[cfg(unix)]
pub fn research_log(database: &Path, job: i64) -> Result<Vec<String>, EngineError> {
    assert!(job > 0);
    let Some(directory) = agents_folder(database)? else {
        return Ok(Vec::new());
    };
    let Some(run) = latest_run(&directory, job)? else {
        return Ok(Vec::new());
    };
    run_lines(&directory, &run)
}

/// Opens the research runner's agent state folder beside the engine database, without
/// following a link. Takes the database path. Returns the folder, none when it is not there,
/// or `runtime_parent_missing` for a path with no folder.
#[cfg(unix)]
fn agents_folder(database: &Path) -> Result<Option<Directory>, EngineError> {
    let agents = database
        .parent()
        .ok_or_else(|| failure("runtime_parent_missing"))?
        .join("research")
        .join("agents");
    Ok(rustix::fs::open(
        agents.as_path(),
        open_flags() | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .ok())
}

/// Reads the last plain lines of one run folder's logs, with secrets hidden. Takes the open
/// agents folder and the run folder name. Returns the newest lines within
/// RESEARCH_LOG_LINES_MAX and RESEARCH_LOG_BYTES_MAX, so the reply fits one owner frame;
/// none for a run folder that is not there.
#[cfg(unix)]
fn run_lines(directory: &Directory, run: &str) -> Result<Vec<String>, EngineError> {
    let Ok(run) = open_directory(directory, run) else {
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
        let plain: Vec<String> = rows.filter_map(plain_text).collect();
        if plain.is_empty() {
            continue;
        }
        let before = lines.len();
        lines.extend(redact(&plain.join("\n")).split('\n').map(clipped));
        assert_eq!(lines.len() - before, plain.len());
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

/// Reads the last plain lines of the run a research job's own record names, as
/// [`research_log`] reads a run: a name the runner gives this job is read, an empty name (a
/// start over) or any other value reads none, and no name at all, a record from before the
/// engine kept it, takes the legacy read by folder name. Takes the transaction that read the
/// job, the engine database path and the job id. Returns the lines.
#[cfg(unix)]
pub fn research_job_log(
    tx: &Transaction<'_>,
    database: &Path,
    job: i64,
) -> Result<Vec<String>, EngineError> {
    assert!(job > 0);
    let stored: Option<String> = tx
        .query_row(
            "SELECT run_name FROM research_jobs WHERE id=?1",
            [job],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?
        .flatten();
    let Some(run) = stored else {
        return research_log(database, job);
    };
    if !named_run(job, &run) {
        return Ok(Vec::new());
    }
    let Some(directory) = agents_folder(database)? else {
        return Ok(Vec::new());
    };
    run_lines(&directory, &run)
}

/// Whether a stored run name is one the runner gives this job: `job-<id>-<attempt>` with an
/// optional `-<launch>`, each in plain decimal within the runner's bounds, so the read stays
/// on one entry of the agents folder. Inputs: the job id and the name. Output: the answer,
/// false for the empty name a start over leaves.
#[cfg(unix)]
fn named_run(job: i64, name: &str) -> bool {
    let Some(rest) = name.strip_prefix(&format!("job-{job}-")) else {
        return false;
    };
    let mut parts = rest.split('-');
    let attempt = parts.next().and_then(|part| part.parse::<u32>().ok());
    let launch = parts.next().map(|part| part.parse::<u32>().ok());
    if parts.next().is_some() {
        return false;
    }
    let Some(attempt) =
        attempt.filter(|attempt| (1..=crate::research::jobs::ATTEMPTS_MAX).contains(attempt))
    else {
        return false;
    };
    let suffix = match launch {
        None => String::new(),
        Some(Some(launch))
            if (1..crate::research::runner::RUNS_PER_ATTEMPT_MAX).contains(&launch) =>
        {
            format!("-{launch}")
        }
        Some(_) => return false,
    };
    name == format!("job-{job}-{attempt}{suffix}")
}

/// Refuses research job output where no directory-relative no-follow read exists.
/// Takes the transaction, engine database path and job id, all unused.
/// Returns `research_log_unavailable`.
#[cfg(not(unix))]
pub fn research_job_log(
    _tx: &Transaction<'_>,
    _database: &Path,
    _job: i64,
) -> Result<Vec<String>, EngineError> {
    Err(failure("research_log_unavailable"))
}

/// Names the newest run folder of a research job in the runner's agent state.
/// Takes the open agents folder and the job id; run folders are named job-<id>-<attempt> with
/// an optional -<launch>, both bounded by the runner, so each name is probed newest first and
/// no other entry can hide it. Returns the highest attempt and launch, or none before any run.
#[cfg(unix)]
fn latest_run(agents: &Directory, job: i64) -> Result<Option<String>, EngineError> {
    for attempt in (1..=crate::research::jobs::ATTEMPTS_MAX).rev() {
        for launch in (0..crate::research::runner::RUNS_PER_ATTEMPT_MAX).rev() {
            let suffix = if launch == 0 {
                String::new()
            } else {
                format!("-{launch}")
            };
            let name = format!("job-{job}-{attempt}{suffix}");
            match rustix::fs::statat(agents, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => return Ok(Some(name)),
                Err(rustix::io::Errno::NOENT) => {}
                Err(problem) => return Err(error(problem)),
            }
        }
    }
    Ok(None)
}

/// Turns one raw agent log line into one plain line, secrets not yet hidden: control
/// characters, line breaks among them, become spaces. Takes the raw line; a JSON event gives
/// its message text and textless events are skipped. Returns the line, or none for an empty
/// or textless line.
#[cfg(unix)]
fn plain_text(raw: &str) -> Option<String> {
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
    assert!(!text.contains('\n'));
    Some(text.to_owned())
}

/// Bounds one redacted log line to RESEARCH_LOG_LINE_CHARS_MAX characters, marking a cut
/// with "...". Takes the line. Returns it whole when it fits, else its head and the mark.
#[cfg(unix)]
fn clipped(line: &str) -> String {
    if line.chars().count() <= RESEARCH_LOG_LINE_CHARS_MAX {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(RESEARCH_LOG_LINE_CHARS_MAX).collect();
    cut.push_str("...");
    cut
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

#[cfg(all(test, unix))]
mod job_log_tests;
#[cfg(all(test, unix))]
mod tests;
