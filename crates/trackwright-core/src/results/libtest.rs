use super::{ParseRefusal, ParsedResults, ResultRow, Summary, push_row, strip_ansi};
use crate::tests_model::ResultOutcome;
use std::collections::HashMap;

struct Block {
    first: usize,
    announced: u64,
    measured: u64,
    names: HashMap<String, usize>,
}

struct Stated {
    ok: bool,
    counts: Summary,
}

struct Section {
    row: Option<usize>,
    output: String,
}

/// Reads cargo and libtest text: `test <name> ... ok|FAILED|ignored` lines in `running N
/// tests` blocks, each checked against its `test result:` line, failure output from
/// `---- <name> stdout ----` sections (open until the next header, `failures:` or
/// `successes:`), and compiler errors outside any block as a build failure. Input: the
/// bytes, read lossily. Output: the rows and totals, or SummaryMismatch.
pub(super) fn parse(bytes: &[u8]) -> Result<ParsedResults, ParseRefusal> {
    let text = String::from_utf8_lossy(bytes);
    let mut rows: Vec<ResultRow> = Vec::new();
    let mut summary: Option<Summary> = None;
    let mut build_failed = false;
    let mut block: Option<Block> = None;
    let mut section: Option<Section> = None;
    for raw in text.lines() {
        let line = strip_ansi(raw);
        let line: &str = &line;
        if let Some(open) = section.as_mut()
            && section_name(line).is_none()
            && !list_heading(line)
        {
            open.output.push_str(line);
            open.output.push('\n');
            continue;
        }
        if block.is_none()
            && (line.starts_with("error[E") || line.starts_with("error: could not compile"))
        {
            build_failed = true;
        }
        if let Some(announced) = running(line) {
            if block.is_some() {
                return Err(ParseRefusal::SummaryMismatch);
            }
            block = Some(Block {
                first: rows.len(),
                announced,
                measured: 0,
                names: HashMap::new(),
            });
            continue;
        }
        if let Some(stated) = result_line(line)? {
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
            section = Some(Section {
                row: open.names.get(name).copied(),
                output: String::new(),
            });
            continue;
        }
        if list_heading(line) {
            settle(&mut rows, section.take());
            continue;
        }
        if let Some((row, measured)) = test_line(line) {
            if measured {
                open.measured += 1;
            }
            open.names.entry(row.name.clone()).or_insert(rows.len());
            push_row(&mut rows, row)?;
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

fn list_heading(line: &str) -> bool {
    line == "failures:" || line == "successes:"
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

fn settle(rows: &mut [ResultRow], section: Option<Section>) {
    let Some(Section {
        row: Some(at),
        output,
    }) = section
    else {
        return;
    };
    let row = &mut rows[at];
    if row.message.is_none() && !output.trim().is_empty() {
        row.message = Some(output);
    }
}
