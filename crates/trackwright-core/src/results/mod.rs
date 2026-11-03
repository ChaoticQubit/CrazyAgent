mod allure;
mod junit;
mod libtest;

use crate::tests_model::{
    ALLURE_RESULT_BYTES_MAX, JUNIT_BYTES_MAX, LIBTEST_BYTES_MAX, RESULT_MESSAGE_BYTES_MAX,
    RESULT_NAME_BYTES_MAX, RESULTS_PER_RUN_MAX, ResultOutcome,
};
use crate::text;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// The prefix of a message whose head was cut so the message fits its byte bound.
pub const MESSAGE_ELIDED: &str = "[elided] ";

const _: () = assert!(MESSAGE_ELIDED.len() < RESULT_MESSAGE_BYTES_MAX);

/// A test report format `parse` reads: JUnit XML, one Allure result JSON file, or libtest text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultFormat {
    Junit,
    Allure,
    Libtest,
}

impl ResultFormat {
    /// The largest input of this format `parse` reads. Output: the bound in bytes.
    pub fn bytes_max(self) -> usize {
        match self {
            ResultFormat::Junit => JUNIT_BYTES_MAX,
            ResultFormat::Allure => ALLURE_RESULT_BYTES_MAX,
            ResultFormat::Libtest => LIBTEST_BYTES_MAX,
        }
    }
}

/// One test result read from a report: its stable name, outcome, duration and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRow {
    pub name: String,
    pub outcome: ResultOutcome,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
}

/// The totals a report states about itself, summed over every libtest `test result:` line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub measured: u64,
    pub filtered_out: u64,
}

/// What one report holds: its rows in report order, the totals it stated, and whether the
/// build failed before any test ran.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedResults {
    pub rows: Vec<ResultRow>,
    pub summary: Option<Summary>,
    pub build_failed: bool,
}

/// Why a report was refused. A host records a refused report as a run with verdict `unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseRefusal {
    TooLarge { limit: usize, actual: usize },
    TooDeep { limit: usize },
    TooManyResults { limit: usize },
    NameTooLong { limit: usize },
    BadName { reason: &'static str },
    NotUtf8,
    Doctype,
    Entity,
    XmlSyntax { position: u64 },
    NotAReport { reason: &'static str },
    Malformed { reason: &'static str },
    SummaryMismatch,
    UnknownStatus,
}

impl fmt::Display for ParseRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseRefusal::TooLarge { limit, actual } => {
                write!(
                    f,
                    "the report is {actual} bytes, over the {limit} byte bound"
                )
            }
            ParseRefusal::TooDeep { limit } => {
                write!(f, "the report nests elements deeper than {limit}")
            }
            ParseRefusal::TooManyResults { limit } => {
                write!(f, "the report holds more than {limit} results")
            }
            ParseRefusal::NameTooLong { limit } => {
                write!(f, "a result name is over the {limit} byte bound")
            }
            ParseRefusal::BadName { reason } | ParseRefusal::NotAReport { reason } => {
                f.write_str(reason)
            }
            ParseRefusal::Malformed { reason } => f.write_str(reason),
            ParseRefusal::NotUtf8 => f.write_str("the report is not UTF-8 text"),
            ParseRefusal::Doctype => f.write_str("the report holds a DOCTYPE, which is refused"),
            ParseRefusal::Entity => {
                f.write_str("the report references an entity other than the predefined five")
            }
            ParseRefusal::XmlSyntax { position } => {
                write!(f, "the report is not well-formed XML near byte {position}")
            }
            ParseRefusal::SummaryMismatch => {
                f.write_str("the test result line does not match the tests the report lists")
            }
            ParseRefusal::UnknownStatus => {
                f.write_str("the result has no passed, failed, broken or skipped status")
            }
        }
    }
}

impl std::error::Error for ParseRefusal {}

/// Reads one test report into result rows. Pure: no I/O, no process, no network.
/// Inputs: the format and the report bytes. Output: the rows with unique names, bounded
/// messages and the stated totals, or the refusal when the input is over its byte bound,
/// unsafe or malformed, holds more than the run's result bound, or names a result badly.
pub fn parse(format: ResultFormat, bytes: &[u8]) -> Result<ParsedResults, ParseRefusal> {
    let limit = format.bytes_max();
    if bytes.len() > limit {
        return Err(ParseRefusal::TooLarge {
            limit,
            actual: bytes.len(),
        });
    }
    let mut parsed = match format {
        ResultFormat::Junit => junit::parse(bytes)?,
        ResultFormat::Allure => allure::parse(bytes)?,
        ResultFormat::Libtest => libtest::parse(bytes)?,
    };
    assert!(
        parsed.rows.len() <= RESULTS_PER_RUN_MAX,
        "a format reader returned more rows than a run holds"
    );
    for row in &mut parsed.rows {
        row.message = row
            .message
            .take()
            .and_then(|message| bound_message(&message));
    }
    unique_names(&mut parsed.rows)?;
    Ok(parsed)
}

/// Adds a row unless the report already holds as many rows as a run may.
/// Inputs: the rows read so far and the next row. Output: TooManyResults at the cap.
pub(crate) fn push_row(rows: &mut Vec<ResultRow>, row: ResultRow) -> Result<(), ParseRefusal> {
    if rows.len() >= RESULTS_PER_RUN_MAX {
        return Err(ParseRefusal::TooManyResults {
            limit: RESULTS_PER_RUN_MAX,
        });
    }
    rows.push(row);
    Ok(())
}

fn unique_names(rows: &mut [ResultRow]) -> Result<(), ParseRefusal> {
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        check_name(&row.name)?;
        *seen.entry(row.name.clone()).or_insert(0) += 1;
    }
    let mut taken: HashSet<String> = seen.keys().cloned().collect();
    let mut next: HashMap<String, u64> = HashMap::new();
    for row in rows.iter_mut() {
        if seen[&row.name] == 1 {
            continue;
        }
        let counter = next.entry(row.name.clone()).or_insert(1);
        let mut candidate = format!("{} [{counter}]", row.name);
        while taken.contains(&candidate) {
            *counter += 1;
            candidate = format!("{} [{counter}]", row.name);
        }
        *counter += 1;
        check_name(&candidate)?;
        taken.insert(candidate.clone());
        row.name = candidate;
    }
    let distinct: HashSet<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        distinct.len(),
        rows.len(),
        "a result name is taken twice in one report"
    );
    Ok(())
}

fn check_name(name: &str) -> Result<(), ParseRefusal> {
    if name.trim().is_empty() {
        return Err(ParseRefusal::BadName {
            reason: "a result name is empty",
        });
    }
    if name.len() > RESULT_NAME_BYTES_MAX {
        return Err(ParseRefusal::NameTooLong {
            limit: RESULT_NAME_BYTES_MAX,
        });
    }
    if text::hidden(name) {
        return Err(ParseRefusal::BadName {
            reason: "a result name holds hidden characters",
        });
    }
    Ok(())
}

/// Removes ANSI CSI sequences (colours, cursor moves) a runner wrote around its text.
/// Input: one line or message. Output: the text without them, borrowed when it held none.
pub(crate) fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    Cow::Owned(out)
}

fn bound_message(message: &str) -> Option<String> {
    let mut clean = String::with_capacity(message.len());
    let mut buffer = [0u8; 4];
    for c in strip_ansi(message).chars() {
        if c == '\r' {
            continue;
        }
        if text::hidden(c.encode_utf8(&mut buffer)) {
            clean.push('\u{fffd}');
        } else {
            clean.push(c);
        }
    }
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    if clean.len() <= RESULT_MESSAGE_BYTES_MAX {
        return Some(clean.to_string());
    }
    let mut start = clean.len() - (RESULT_MESSAGE_BYTES_MAX - MESSAGE_ELIDED.len());
    while !clean.is_char_boundary(start) {
        start += 1;
    }
    let bounded = format!("{MESSAGE_ELIDED}{}", &clean[start..]);
    assert!(
        bounded.len() <= RESULT_MESSAGE_BYTES_MAX,
        "an elided message is over its bound"
    );
    Some(bounded)
}
