use super::{ParseRefusal, ParsedResults, ResultRow, Summary, strip_ansi};
use crate::tests_model::ResultOutcome;

struct Block {
    first: usize,
    announced: u64,
    measured: u64,
}

struct Stated {
    ok: bool,
    counts: Summary,
}

/// Reads cargo and libtest text: `test <name> ... ok|FAILED|ignored` lines grouped by
/// `running N tests` blocks, failure output from `---- <name> stdout ----` sections, and
/// compiler `error[E` or `error: could not compile` lines as a build failure. Each block's
/// `test result:` line must match the lines it closes. Input: the text bytes, read lossily.
/// Output: the rows and summed totals, or SummaryMismatch for a block that disagrees with
/// its summary or ends without one.
pub(super) fn parse(bytes: &[u8]) -> Result<ParsedResults, ParseRefusal> {
    let text = String::from_utf8_lossy(bytes);
    let mut rows: Vec<ResultRow> = Vec::new();
    let mut summary: Option<Summary> = None;
    let mut build_failed = false;
    let mut block: Option<Block> = None;
    let mut section: Option<(usize, String)> = None;
    for raw in text.lines() {
        let line = strip_ansi(raw);
        let line = line.as_ref();
        if line.starts_with("error[E") || line.starts_with("error: could not compile") {
            build_failed = true;
        }
        if let Some(announced) = running(line) {
            if block.is_some() {
                return Err(ParseRefusal::SummaryMismatch);
            }
            settle(&mut rows, section.take());
            block = Some(Block {
                first: rows.len(),
                announced,
                measured: 0,
            });
            continue;
        }
        if let Some(stated) = result_line(line)? {
            settle(&mut rows, section.take());
            let open = block.take().ok_or(ParseRefusal::SummaryMismatch)?;
            let counts = check(&rows[open.first..], &open, &stated)?;
            let total = summary.get_or_insert_with(Summary::default);
            total.passed += counts.passed;
            total.failed += counts.failed;
            total.skipped += counts.skipped;
            total.measured += counts.measured;
            total.filtered_out = total.filtered_out.saturating_add(counts.filtered_out);
            continue;
        }
        let Some(open) = block.as_mut() else {
            continue;
        };
        if let Some(name) = section_name(line) {
            settle(&mut rows, section.take());
            section = rows[open.first..]
                .iter()
                .position(|row| row.name == name)
                .map(|at| (open.first + at, String::new()));
            continue;
        }
        if line == "failures:" {
            settle(&mut rows, section.take());
            continue;
        }
        if let Some((_, output)) = section.as_mut() {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        if let Some((row, measured)) = test_line(line) {
            if measured {
                open.measured += 1;
            }
            rows.push(row);
        }
    }
    if block.is_some() {
        return Err(ParseRefusal::SummaryMismatch);
    }
    Ok(ParsedResults {
        rows,
        summary,
        build_failed,
    })
}

fn running(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("running ")?;
    let count = rest
        .strip_suffix(" tests")
        .or_else(|| rest.strip_suffix(" test"))?;
    count.parse().ok()
}

fn test_line(line: &str) -> Option<(ResultRow, bool)> {
    let rest = line.strip_prefix("test ")?;
    let at = rest.rfind(" ... ")?;
    let name = &rest[..at];
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    let result = &rest[at + " ... ".len()..];
    let (outcome, message, measured) = match result {
        "ok" => (ResultOutcome::Passed, None, false),
        "FAILED" => (ResultOutcome::Failed, None, false),
        "ignored" => (ResultOutcome::Skipped, None, false),
        _ => {
            if let Some(reason) = result.strip_prefix("ignored, ") {
                (ResultOutcome::Skipped, Some(reason.to_string()), false)
            } else if result.starts_with("bench:") {
                (ResultOutcome::Passed, None, true)
            } else {
                return None;
            }
        }
    };
    Some((
        ResultRow {
            name: name.to_string(),
            outcome,
            duration_ms: None,
            message,
        },
        measured,
    ))
}

fn section_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("---- ")?;
    rest.strip_suffix(" stdout ----")
        .or_else(|| rest.strip_suffix(" stderr ----"))
}

fn result_line(line: &str) -> Result<Option<Stated>, ParseRefusal> {
    let Some(rest) = line.strip_prefix("test result: ") else {
        return Ok(None);
    };
    let unreadable = ParseRefusal::Malformed {
        reason: "a test result line cannot be read",
    };
    let (ok, rest) = if let Some(rest) = rest.strip_prefix("ok. ") {
        (true, rest)
    } else if let Some(rest) = rest.strip_prefix("FAILED. ") {
        (false, rest)
    } else {
        return Err(unreadable);
    };
    let mut counts = Summary::default();
    let mut seen = 0;
    for part in rest.split("; ") {
        let Some((number, label)) = part.split_once(' ') else {
            continue;
        };
        let Ok(number) = number.parse::<u64>() else {
            continue;
        };
        let slot = match label {
            "passed" => &mut counts.passed,
            "failed" => &mut counts.failed,
            "ignored" => &mut counts.skipped,
            "measured" => &mut counts.measured,
            "filtered out" => &mut counts.filtered_out,
            _ => continue,
        };
        *slot = number;
        seen += 1;
    }
    if seen != 5 {
        return Err(unreadable);
    }
    Ok(Some(Stated { ok, counts }))
}

fn check(rows: &[ResultRow], block: &Block, stated: &Stated) -> Result<Summary, ParseRefusal> {
    let count =
        |outcome: ResultOutcome| rows.iter().filter(|row| row.outcome == outcome).count() as u64;
    let passed = count(ResultOutcome::Passed);
    assert!(
        passed >= block.measured,
        "a bench row is counted outside the passed rows"
    );
    let found = Summary {
        passed: passed - block.measured,
        failed: count(ResultOutcome::Failed),
        skipped: count(ResultOutcome::Skipped),
        measured: block.measured,
        filtered_out: stated.counts.filtered_out,
    };
    let consistent = found == stated.counts
        && rows.len() as u64 == block.announced
        && stated.ok == (found.failed == 0);
    if !consistent {
        return Err(ParseRefusal::SummaryMismatch);
    }
    Ok(found)
}

fn settle(rows: &mut [ResultRow], section: Option<(usize, String)>) {
    if let Some((at, output)) = section {
        let row = &mut rows[at];
        if row.message.is_none() && !output.trim().is_empty() {
            row.message = Some(output);
        }
    }
}
