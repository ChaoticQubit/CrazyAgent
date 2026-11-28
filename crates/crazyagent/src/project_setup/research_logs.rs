#[cfg(unix)]
use super::redaction::Block;
use super::*;

#[cfg(unix)]
const OPENED_BLOCK: &str = "-----BEGIN PRIVATE KEY-----";

/// Reads the last plain lines of one research job's agent output, with secrets hidden, also
/// the rows of a key block opened in the RESEARCH_LOG_TAIL_BYTES before them. Takes the
/// engine database path, whose folder holds the research runner's agent state, and the job
/// id. Returns the newest lines within RESEARCH_LOG_LINES_MAX and RESEARCH_LOG_BYTES_MAX, so
/// the reply fits one owner frame; none before the first run.
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
    let Some(run) = latest_run(&directory, job)? else {
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
        let read_from = start.saturating_sub(RESEARCH_LOG_TAIL_BYTES);
        let before = read_from.saturating_sub(1);
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(before)).map_err(error)?;
        let mut earlier = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(start - before)
            .read_to_end(&mut earlier)
            .map_err(error)?;
        let mut tail = Vec::new();
        file.take(RESEARCH_LOG_TAIL_BYTES)
            .read_to_end(&mut tail)
            .map_err(error)?;
        let (cut, earlier) = match earlier.split_first() {
            Some((byte, rest)) if read_from > 0 => (*byte != b'\n', rest),
            _ => (false, earlier.as_slice()),
        };
        let opened = start > 0 && opens_block(earlier, &tail, cut);
        let text = String::from_utf8_lossy(&tail);
        let mut rows = text.lines();
        if start > 0 {
            rows.next();
        }
        let plain: Vec<String> = rows.filter_map(plain_text).collect();
        if plain.is_empty() {
            continue;
        }
        let joined = plain.join("\n");
        let shown = if opened {
            redact(&format!("{OPENED_BLOCK}\n{joined}"))
        } else {
            redact(&joined)
        };
        let before = lines.len();
        lines.extend(shown.split('\n').skip(usize::from(opened)).map(clipped));
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

/// Whether the rows before a tail window end inside a key block, tracked as redact tracks
/// one but opened only by a PEM-shaped BEGIN line. Inputs: the bytes before the window, the
/// window, whose first row they complete, and whether their own first row is cut. Output:
/// the answer.
#[cfg(unix)]
fn opens_block(earlier: &[u8], tail: &[u8], cut: bool) -> bool {
    let completed = tail
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(tail.len());
    let mut bytes = earlier.to_vec();
    bytes.extend_from_slice(&tail[..completed]);
    let text = String::from_utf8_lossy(&bytes);
    let mut rows = text.lines();
    if cut {
        rows.next();
    }
    let block = rows
        .filter_map(plain_text)
        .fold(Block::Closed, |block, row| {
            block.opened_by(&row, false).closed_by(&row)
        });
    assert_ne!(
        block,
        Block::Loose,
        "only a PEM-shaped BEGIN opens a block here"
    );
    block == Block::Strict
}

/// Refuses research job output where no directory-relative no-follow read exists.
/// Takes the engine database path and job id, both unused.
/// Returns `research_log_unavailable`.
#[cfg(not(unix))]
pub fn research_log(_database: &Path, _job: i64) -> Result<Vec<String>, EngineError> {
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
mod tests;
