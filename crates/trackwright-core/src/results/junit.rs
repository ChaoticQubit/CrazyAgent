use super::{ParseRefusal, ParsedResults, ResultRow, push_row};
use crate::tests_model::{JUNIT_DEPTH_MAX, ResultOutcome};
use quick_xml::XmlVersion;
use quick_xml::errors::Error as XmlError;
use quick_xml::escape::{EscapeError, resolve_xml_entity};
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

struct OpenCase {
    depth: usize,
    row: ResultRow,
    severity: u8,
}

struct OpenOutcome {
    depth: usize,
    outcome: ResultOutcome,
    attribute: Option<String>,
    body: String,
}

struct Walk {
    version: XmlVersion,
    depth: usize,
    roots: usize,
    case: Option<OpenCase>,
    outcome: Option<OpenOutcome>,
    rows: Vec<ResultRow>,
}

/// Reads a JUnit XML report: one row per `testcase`, named `classname.name`, with its
/// `failure`, `error` or `skipped` child mapped to failed, errored or skipped. Input: the
/// report bytes. Output: the rows, or the refusal for a DOCTYPE, an entity other than the
/// predefined five, nesting over the depth bound, bad UTF-8 or malformed XML.
pub(super) fn parse(bytes: &[u8]) -> Result<ParsedResults, ParseRefusal> {
    let text = std::str::from_utf8(bytes).map_err(|_| ParseRefusal::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    reader.config_mut().enable_all_checks(true);
    let mut walk = Walk {
        version: XmlVersion::Implicit1_0,
        depth: 0,
        roots: 0,
        case: None,
        outcome: None,
        rows: Vec::new(),
    };
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(error) => return Err(refusal(&error, reader.error_position())),
        };
        match event {
            Event::DocType(_) => return Err(ParseRefusal::Doctype),
            Event::Decl(declaration) => {
                walk.version = declaration
                    .xml_version()
                    .map_err(|error| refusal(&error, reader.buffer_position()))?;
            }
            Event::Start(element) => walk.open(&element, false)?,
            Event::Empty(element) => walk.open(&element, true)?,
            Event::End(_) => walk.close()?,
            Event::Text(content) => {
                let content = content.xml_content(walk.version);
                if walk.depth == 0 && !content.trim().is_empty() {
                    return Err(ParseRefusal::NotAReport {
                        reason: "the report holds text outside its root element",
                    });
                }
                walk.text(&content);
            }
            Event::CData(content) => walk.text(&content.xml_content(walk.version)),
            Event::GeneralRef(reference) => {
                let resolved = resolve(&reference)?;
                walk.text(&resolved);
            }
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => break,
        }
    }
    if walk.depth != 0 {
        return Err(ParseRefusal::Malformed {
            reason: "the report ends inside an element",
        });
    }
    if walk.roots == 0 {
        return Err(ParseRefusal::NotAReport {
            reason: "the report has no root element",
        });
    }
    Ok(ParsedResults {
        rows: walk.rows,
        summary: None,
        build_failed: false,
    })
}

impl Walk {
    fn open(&mut self, element: &BytesStart<'_>, empty: bool) -> Result<(), ParseRefusal> {
        let depth = self.depth + 1;
        if depth > JUNIT_DEPTH_MAX {
            return Err(ParseRefusal::TooDeep {
                limit: JUNIT_DEPTH_MAX,
            });
        }
        let name = element.name();
        let name = name.as_ref();
        if depth == 1 {
            self.root(name)?;
        }
        let attributes = Attributes::read(element, self.version)?;
        match name {
            "testcase" => {
                if self.case.is_some() {
                    return Err(ParseRefusal::NotAReport {
                        reason: "a testcase sits inside another testcase",
                    });
                }
                let row = attributes.row()?;
                if empty {
                    push_row(&mut self.rows, row)?;
                } else {
                    self.case = Some(OpenCase {
                        depth,
                        row,
                        severity: 0,
                    });
                }
            }
            "failure" | "error" | "skipped"
                if self
                    .case
                    .as_ref()
                    .is_some_and(|case| case.depth + 1 == depth) =>
            {
                let outcome = match name {
                    "failure" => ResultOutcome::Failed,
                    "error" => ResultOutcome::Errored,
                    _ => ResultOutcome::Skipped,
                };
                let open = OpenOutcome {
                    depth,
                    outcome,
                    attribute: attributes.message,
                    body: String::new(),
                };
                if empty {
                    self.settle(open);
                } else {
                    self.outcome = Some(open);
                }
            }
            _ => {}
        }
        if !empty {
            self.depth = depth;
        }
        Ok(())
    }

    fn root(&mut self, name: &str) -> Result<(), ParseRefusal> {
        self.roots += 1;
        if self.roots > 1 {
            return Err(ParseRefusal::NotAReport {
                reason: "the report has more than one root element",
            });
        }
        if name != "testsuites" && name != "testsuite" {
            return Err(ParseRefusal::NotAReport {
                reason: "the root element is not testsuites or testsuite",
            });
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), ParseRefusal> {
        assert!(self.depth > 0, "an end tag closes no open element");
        if self
            .outcome
            .as_ref()
            .is_some_and(|open| open.depth == self.depth)
        {
            let open = self.outcome.take().expect("the open outcome was just seen");
            self.settle(open);
        }
        if self
            .case
            .as_ref()
            .is_some_and(|case| case.depth == self.depth)
        {
            let case = self.case.take().expect("the open case was just seen");
            push_row(&mut self.rows, case.row)?;
        }
        self.depth -= 1;
        Ok(())
    }

    fn text(&mut self, content: &str) {
        if let Some(open) = self.outcome.as_mut() {
            open.body.push_str(content);
        }
    }

    fn settle(&mut self, open: OpenOutcome) {
        let case = self
            .case
            .as_mut()
            .expect("an outcome element is read only inside a testcase");
        let severity = match open.outcome {
            ResultOutcome::Errored => 3,
            ResultOutcome::Failed => 2,
            _ => 1,
        };
        if severity <= case.severity {
            return;
        }
        case.severity = severity;
        case.row.outcome = open.outcome;
        case.row.message = if open.body.trim().is_empty() {
            open.attribute
        } else {
            Some(open.body)
        };
    }
}

struct Attributes {
    name: Option<String>,
    classname: Option<String>,
    time: Option<String>,
    message: Option<String>,
}

impl Attributes {
    fn read(element: &BytesStart<'_>, version: XmlVersion) -> Result<Self, ParseRefusal> {
        let mut read = Attributes {
            name: None,
            classname: None,
            time: None,
            message: None,
        };
        for attribute in element.attributes() {
            let attribute = attribute.map_err(|_| ParseRefusal::Malformed {
                reason: "an element holds a malformed attribute",
            })?;
            let value = attribute
                .normalized_value(version)
                .map_err(|error| refusal(&error, 0))?
                .into_owned();
            match attribute.key.as_ref() {
                "name" => read.name = Some(value),
                "classname" => read.classname = Some(value),
                "time" => read.time = Some(value),
                "message" => read.message = Some(value),
                _ => {}
            }
        }
        Ok(read)
    }

    fn row(self) -> Result<ResultRow, ParseRefusal> {
        let name =
            self.name
                .filter(|name| !name.trim().is_empty())
                .ok_or(ParseRefusal::BadName {
                    reason: "a testcase has no name",
                })?;
        let name = match self.classname.filter(|classname| !classname.is_empty()) {
            Some(classname) => format!("{classname}.{name}"),
            None => name,
        };
        Ok(ResultRow {
            name,
            outcome: ResultOutcome::Passed,
            duration_ms: self.time.as_deref().and_then(milliseconds),
            message: None,
        })
    }
}

fn milliseconds(seconds: &str) -> Option<u64> {
    let seconds: f64 = seconds.trim().parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some((seconds * 1000.0).round() as u64)
}

fn resolve(reference: &BytesRef<'_>) -> Result<String, ParseRefusal> {
    if reference.is_char_ref() {
        return match reference.resolve_char_ref() {
            Ok(Some(c)) => Ok(c.to_string()),
            _ => Err(ParseRefusal::Entity),
        };
    }
    resolve_xml_entity(reference)
        .map(str::to_string)
        .ok_or(ParseRefusal::Entity)
}

fn refusal(error: &XmlError, position: u64) -> ParseRefusal {
    match error {
        XmlError::Escape(EscapeError::UnrecognizedEntity(..)) => ParseRefusal::Entity,
        _ => ParseRefusal::XmlSyntax { position },
    }
}
