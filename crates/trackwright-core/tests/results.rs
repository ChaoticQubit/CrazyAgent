use trackwright_core::results::{
    MESSAGE_ELIDED, ParseRefusal, ParsedResults, ResultFormat, ResultRow, Summary, parse,
};
use trackwright_core::tests_model::{
    ALLURE_RESULT_BYTES_MAX, JUNIT_BYTES_MAX, JUNIT_DEPTH_MAX, LIBTEST_BYTES_MAX,
    RESULT_MESSAGE_BYTES_MAX, RESULT_NAME_BYTES_MAX, RESULTS_PER_RUN_MAX, ResultOutcome,
};

const NODE_JUNIT: &[u8] = include_bytes!("fixtures/results/node-test.junit.xml");
const PYTEST_JUNIT: &[u8] = include_bytes!("fixtures/results/pytest.junit.xml");
const LIBTEST_FAILED: &str = include_str!("fixtures/results/libtest-failed.txt");
const LIBTEST_BUILD_FAILED: &str = include_str!("fixtures/results/libtest-build-failed.txt");
const ALLURE_BROKEN: &str = include_str!("fixtures/results/allure-broken-result.json");

fn row<'a>(parsed: &'a ParsedResults, name: &str) -> &'a ResultRow {
    parsed
        .rows
        .iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no row named {name}"))
}

fn junit_case(body: &str) -> String {
    format!(
        "<testsuites><testsuite name=\"s\"><testcase classname=\"c\" name=\"t\">{body}</testcase></testsuite></testsuites>"
    )
}

fn libtest_block(names: &[String]) -> String {
    let mut text = format!("running {} tests\n", names.len());
    for name in names {
        text.push_str(&format!("test {name} ... ok\n"));
    }
    text.push_str(&format!(
        "\ntest result: ok. {} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        names.len()
    ));
    text
}

#[test]
fn a_real_junit_file_records_one_result_per_case() {
    let parsed =
        parse(ResultFormat::Junit, NODE_JUNIT).expect("real node --test JUnit output parses");
    let outcomes: Vec<(&str, ResultOutcome)> = parsed
        .rows
        .iter()
        .map(|row| (row.name.as_str(), row.outcome))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("test.adds two numbers", ResultOutcome::Passed),
            ("test.reports a wrong sum", ResultOutcome::Failed),
            ("test.waits for a fixture", ResultOutcome::Skipped),
        ]
    );
    assert_eq!(
        parsed
            .rows
            .iter()
            .map(|row| row.duration_ms)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(1), Some(0)]
    );
    let failed = row(&parsed, "test.reports a wrong sum");
    assert!(
        failed.message.as_deref().is_some_and(|message| message
            .starts_with("[Error [ERR_TEST_FAILURE]: Expected values to be strictly equal:")
            && message.contains("2 !== 3")
            && message.contains("at TestContext.<anonymous> (file:///work/sample.test.mjs:9:10)")),
        "{failed:?}"
    );
    assert_eq!(
        row(&parsed, "test.waits for a fixture").message.as_deref(),
        Some("fixture not ready")
    );
    assert_eq!(parsed.summary, None);
    assert!(!parsed.build_failed);
}

#[test]
fn a_real_pytest_junit_file_records_one_result_per_case() {
    let parsed =
        parse(ResultFormat::Junit, PYTEST_JUNIT).expect("real pytest --junitxml output parses");
    assert_eq!(
        parsed,
        ParsedResults {
            rows: vec![
                ResultRow {
                    name: "test_sample.test_adds_two_numbers".to_string(),
                    outcome: ResultOutcome::Passed,
                    duration_ms: Some(0),
                    message: None,
                },
                ResultRow {
                    name: "test_sample.test_reports_a_wrong_sum".to_string(),
                    outcome: ResultOutcome::Failed,
                    duration_ms: Some(1),
                    message: Some(
                        "def test_reports_a_wrong_sum():\n>       assert 1 + 1 == 3\nE       assert (1 + 1) == 3\n\ntest_sample.py:9: AssertionError"
                            .to_string()
                    ),
                },
                ResultRow {
                    name: "test_sample.test_waits_for_a_fixture".to_string(),
                    outcome: ResultOutcome::Skipped,
                    duration_ms: Some(0),
                    message: Some("/work/test_sample.py:12: fixture not ready".to_string()),
                },
            ],
            summary: None,
            build_failed: false,
        }
    );
}

#[test]
fn a_junit_doctype_or_entity_is_refused() {
    let refused = [
        (
            "<?xml version=\"1.0\"?><!DOCTYPE testsuites [<!ENTITY boom \"x\">]><testsuites><testsuite name=\"s\"><testcase name=\"t\"><failure>&boom;</failure></testcase></testsuite></testsuites>".to_string(),
            ParseRefusal::Doctype,
        ),
        (
            "<?xml version=\"1.0\"?><!DOCTYPE testsuites SYSTEM \"file:///etc/passwd\"><testsuites/>".to_string(),
            ParseRefusal::Doctype,
        ),
        (junit_case("<failure>&boom;</failure>"), ParseRefusal::Entity),
        (junit_case("<system-out>&boom;</system-out>"), ParseRefusal::Entity),
        (junit_case("<failure message=\"&boom;\"/>"), ParseRefusal::Entity),
        (
            "<testsuites><testsuite name=\"s\"><testcase name=\"&boom;\"/></testsuite></testsuites>".to_string(),
            ParseRefusal::Entity,
        ),
    ];
    for (xml, refusal) in refused {
        assert_eq!(
            parse(ResultFormat::Junit, xml.as_bytes()),
            Err(refusal),
            "{xml}"
        );
    }

    let escaped = "<testsuites><testsuite name=\"s\"><testcase classname=\"a&amp;b\" name=\"x &lt; y\" time=\"0.25\"><failure message=\"short\">1 &#x3c; 2 &amp;&amp; <![CDATA[3 > 2]]></failure></testcase></testsuite></testsuites>";
    let parsed = parse(ResultFormat::Junit, escaped.as_bytes())
        .expect("predefined and character references are read");
    assert_eq!(
        parsed.rows,
        vec![ResultRow {
            name: "a&b.x < y".to_string(),
            outcome: ResultOutcome::Failed,
            duration_ms: Some(250),
            message: Some("1 < 2 && 3 > 2".to_string()),
        }]
    );
}

#[test]
fn a_truncated_or_foreign_junit_file_is_refused() {
    let truncated = "<testsuites><testsuite name=\"s\"><testcase name=\"t\">";
    assert_eq!(
        parse(ResultFormat::Junit, truncated.as_bytes()),
        Err(ParseRefusal::Malformed {
            reason: "the report ends inside an element"
        })
    );
    assert!(matches!(
        parse(ResultFormat::Junit, b"<testsuites><testsuite></testsuites>"),
        Err(ParseRefusal::XmlSyntax { .. })
    ));
    assert_eq!(
        parse(ResultFormat::Junit, b"<html><body/></html>"),
        Err(ParseRefusal::NotAReport {
            reason: "the root element is not testsuites or testsuite"
        })
    );
    assert_eq!(
        parse(ResultFormat::Junit, b"<testsuite/><testsuite/>"),
        Err(ParseRefusal::NotAReport {
            reason: "the report has more than one root element"
        })
    );
    assert_eq!(
        parse(
            ResultFormat::Junit,
            b"<testsuites><testsuite><testcase classname=\"c\"/></testsuite></testsuites>"
        ),
        Err(ParseRefusal::BadName {
            reason: "a testcase has no name"
        })
    );
    assert_eq!(
        parse(ResultFormat::Junit, b"<testsuites>\xff</testsuites>"),
        Err(ParseRefusal::NotUtf8)
    );
    let empty = parse(ResultFormat::Junit, b"<testsuites/>").expect("an empty report parses");
    assert!(empty.rows.is_empty());
}

#[test]
fn libtest_text_maps_ok_failed_and_ignored_and_checks_the_summary() {
    let parsed = parse(ResultFormat::Libtest, LIBTEST_FAILED.as_bytes())
        .expect("real libtest output parses");
    assert!(!parsed.build_failed);
    assert_eq!(parsed.rows.len(), 21);
    assert_eq!(
        parsed.summary,
        Some(Summary {
            passed: 19,
            failed: 1,
            skipped: 1,
            measured: 0,
            filtered_out: 0
        })
    );
    assert_eq!(
        row(&parsed, "two_claimers_never_claim_the_same_job").outcome,
        ResultOutcome::Passed
    );
    assert_eq!(
        row(
            &parsed,
            "queued_research_runs_codex_in_the_project_sandbox_and_writes_a_markdown_report"
        )
        .outcome,
        ResultOutcome::Passed
    );
    let failed = row(
        &parsed,
        "research_that_runs_out_of_time_is_stopped_and_retried_then_failed",
    );
    assert_eq!(failed.outcome, ResultOutcome::Failed);
    assert!(
        failed
            .message
            .as_deref()
            .is_some_and(|message| message.contains("assertion failed: !process_group_alive")),
        "{failed:?}"
    );
    let ignored = row(
        &parsed,
        "research_runs_on_a_signed_in_managed_codex_account",
    );
    assert_eq!(ignored.outcome, ResultOutcome::Skipped);
    assert!(
        ignored
            .message
            .as_deref()
            .is_some_and(|message| message
                .starts_with("BLOCKED until a signed-in managed Codex account exists")),
        "{ignored:?}"
    );
    assert!(parsed.rows.iter().all(|row| row.duration_ms.is_none()));

    let dropped = LIBTEST_FAILED.replace("test two_claimers_never_claim_the_same_job ... ok\n", "");
    assert_ne!(dropped, LIBTEST_FAILED);
    assert_eq!(
        parse(ResultFormat::Libtest, dropped.as_bytes()),
        Err(ParseRefusal::SummaryMismatch)
    );

    let crashed = &LIBTEST_FAILED[..LIBTEST_FAILED
        .find("test result: FAILED")
        .expect("second block")];
    assert_eq!(
        parse(ResultFormat::Libtest, crashed.as_bytes()),
        Err(ParseRefusal::SummaryMismatch)
    );

    let claims_ok =
        LIBTEST_FAILED.replace("test result: FAILED. 7 passed", "test result: ok. 7 passed");
    assert_ne!(claims_ok, LIBTEST_FAILED);
    assert_eq!(
        parse(ResultFormat::Libtest, claims_ok.as_bytes()),
        Err(ParseRefusal::SummaryMismatch)
    );

    let coloured = LIBTEST_FAILED.replace(
        "test two_claimers_never_claim_the_same_job ... ok",
        "test two_claimers_never_claim_the_same_job ... \u{1b}[32mok\u{1b}[0m",
    );
    assert_eq!(
        parse(ResultFormat::Libtest, coloured.as_bytes()),
        Ok(parsed)
    );
}

#[test]
fn libtest_with_error_e_is_build_failed_not_failed() {
    let parsed = parse(ResultFormat::Libtest, LIBTEST_BUILD_FAILED.as_bytes())
        .expect("compiler output parses");
    assert!(parsed.build_failed);
    assert!(parsed.rows.is_empty());
    assert_eq!(parsed.summary, None);

    let lint_only: String = LIBTEST_BUILD_FAILED
        .lines()
        .filter(|line| !line.starts_with("error[E"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(lint_only.contains("error: could not compile"));
    assert!(
        parse(ResultFormat::Libtest, lint_only.as_bytes())
            .expect("parses")
            .build_failed
    );

    assert!(
        !parse(ResultFormat::Libtest, LIBTEST_FAILED.as_bytes())
            .expect("parses")
            .build_failed
    );
    assert!(
        !parse(
            ResultFormat::Libtest,
            b"note: error[E0599] is quoted, not reported\n"
        )
        .expect("parses")
        .build_failed
    );
}

#[test]
fn an_allure_broken_result_is_errored() {
    let parsed =
        parse(ResultFormat::Allure, ALLURE_BROKEN.as_bytes()).expect("an Allure result parses");
    assert_eq!(
        parsed,
        ParsedResults {
            rows: vec![ResultRow {
                name: "tests.test_store#test_reads_back_a_written_row".to_string(),
                outcome: ResultOutcome::Errored,
                duration_ms: Some(42),
                message: Some("sqlite3.OperationalError: database is locked".to_string()),
            }],
            summary: None,
            build_failed: false,
        }
    );

    for (status, outcome) in [
        ("passed", ResultOutcome::Passed),
        ("failed", ResultOutcome::Failed),
        ("skipped", ResultOutcome::Skipped),
    ] {
        let json = ALLURE_BROKEN.replace(
            "\"status\": \"broken\"",
            &format!("\"status\": \"{status}\""),
        );
        assert_eq!(
            parse(ResultFormat::Allure, json.as_bytes())
                .expect(status)
                .rows[0]
                .outcome,
            outcome
        );
    }
    for status in [
        "\"status\": \"unknown\"",
        "\"status\": 3",
        "\"stage\": \"running\"",
    ] {
        let json = ALLURE_BROKEN.replace("\"status\": \"broken\"", status);
        assert_ne!(json, ALLURE_BROKEN);
        assert_eq!(
            parse(ResultFormat::Allure, json.as_bytes()),
            Err(ParseRefusal::UnknownStatus),
            "{status}"
        );
    }

    let no_full_name = ALLURE_BROKEN.replace(
        "\"fullName\": \"tests.test_store#test_reads_back_a_written_row\", ",
        "",
    );
    assert_ne!(no_full_name, ALLURE_BROKEN);
    assert_eq!(
        parse(ResultFormat::Allure, no_full_name.as_bytes())
            .expect("name is the fallback")
            .rows[0]
            .name,
        "test_reads_back_a_written_row"
    );
    assert_eq!(
        parse(ResultFormat::Allure, b"[1, 2]"),
        Err(ParseRefusal::Malformed {
            reason: "the Allure result is not a JSON result object"
        })
    );
}

#[test]
fn input_over_each_bound_is_refused_plainly() {
    let filler = |total: usize| {
        let frame = "<testsuites></testsuites>".len();
        format!("<testsuites>{}</testsuites>", " ".repeat(total - frame))
    };
    let at_limit = filler(JUNIT_BYTES_MAX);
    assert_eq!(at_limit.len(), JUNIT_BYTES_MAX);
    assert!(
        parse(ResultFormat::Junit, at_limit.as_bytes())
            .expect("at the bound")
            .rows
            .is_empty()
    );
    let over = filler(JUNIT_BYTES_MAX + 1);
    assert_eq!(
        parse(ResultFormat::Junit, over.as_bytes()),
        Err(ParseRefusal::TooLarge {
            limit: JUNIT_BYTES_MAX,
            actual: JUNIT_BYTES_MAX + 1
        })
    );

    let nested = |depth: usize| {
        format!(
            "<testsuites>{}{}</testsuites>",
            "<x>".repeat(depth - 1),
            "</x>".repeat(depth - 1)
        )
    };
    assert!(parse(ResultFormat::Junit, nested(JUNIT_DEPTH_MAX).as_bytes()).is_ok());
    assert_eq!(
        parse(ResultFormat::Junit, nested(JUNIT_DEPTH_MAX + 1).as_bytes()),
        Err(ParseRefusal::TooDeep {
            limit: JUNIT_DEPTH_MAX
        })
    );
    let empty_at_depth = format!(
        "<testsuites>{}<x/>{}</testsuites>",
        "<x>".repeat(JUNIT_DEPTH_MAX - 1),
        "</x>".repeat(JUNIT_DEPTH_MAX - 1)
    );
    assert_eq!(
        parse(ResultFormat::Junit, empty_at_depth.as_bytes()),
        Err(ParseRefusal::TooDeep {
            limit: JUNIT_DEPTH_MAX
        })
    );

    let allure = format!(
        "{{\"fullName\": \"a\", \"status\": \"passed\", \"description\": \"{}\"}}",
        "d".repeat(ALLURE_RESULT_BYTES_MAX)
    );
    assert_eq!(
        parse(ResultFormat::Allure, allure.as_bytes()),
        Err(ParseRefusal::TooLarge {
            limit: ALLURE_RESULT_BYTES_MAX,
            actual: allure.len()
        })
    );

    let text = "x".repeat(LIBTEST_BYTES_MAX + 1);
    assert_eq!(
        parse(ResultFormat::Libtest, text.as_bytes()),
        Err(ParseRefusal::TooLarge {
            limit: LIBTEST_BYTES_MAX,
            actual: LIBTEST_BYTES_MAX + 1
        })
    );

    let names = |count: usize| (0..count).map(|n| format!("t{n}")).collect::<Vec<_>>();
    let full = parse(
        ResultFormat::Libtest,
        libtest_block(&names(RESULTS_PER_RUN_MAX)).as_bytes(),
    )
    .expect("the most results a run holds");
    assert_eq!(full.rows.len(), RESULTS_PER_RUN_MAX);
    assert_eq!(
        parse(
            ResultFormat::Libtest,
            libtest_block(&names(RESULTS_PER_RUN_MAX + 1)).as_bytes()
        ),
        Err(ParseRefusal::TooManyResults {
            limit: RESULTS_PER_RUN_MAX
        })
    );
    let cases: String = (0..=RESULTS_PER_RUN_MAX)
        .map(|n| format!("<testcase name=\"t{n}\"/>"))
        .collect();
    assert_eq!(
        parse(
            ResultFormat::Junit,
            format!("<testsuite>{cases}</testsuite>").as_bytes()
        ),
        Err(ParseRefusal::TooManyResults {
            limit: RESULTS_PER_RUN_MAX
        })
    );

    let longest = "n".repeat(RESULT_NAME_BYTES_MAX);
    assert!(
        parse(
            ResultFormat::Libtest,
            libtest_block(std::slice::from_ref(&longest)).as_bytes()
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            ResultFormat::Libtest,
            libtest_block(&[format!("{longest}n")]).as_bytes()
        ),
        Err(ParseRefusal::NameTooLong {
            limit: RESULT_NAME_BYTES_MAX
        })
    );
    assert_eq!(
        parse(
            ResultFormat::Libtest,
            libtest_block(&["evil\u{202e}name".to_string()]).as_bytes()
        ),
        Err(ParseRefusal::BadName {
            reason: "a result name holds hidden characters"
        })
    );

    let tail = "the tail that names the failing assertion";
    let long = junit_case(&format!(
        "<failure>head {} {tail}</failure>",
        "m".repeat(RESULT_MESSAGE_BYTES_MAX * 2)
    ));
    let parsed =
        parse(ResultFormat::Junit, long.as_bytes()).expect("a long message is kept, not refused");
    let message = parsed.rows[0].message.as_deref().expect("message");
    assert!(
        message.len() <= RESULT_MESSAGE_BYTES_MAX,
        "{}",
        message.len()
    );
    assert!(message.starts_with(MESSAGE_ELIDED));
    assert!(message.ends_with(tail));
    assert!(!message.contains("head"));

    let hidden = junit_case("<failure>left\u{202e}right \u{1b}[31mred\u{1b}[0m\r\nnext</failure>");
    let parsed = parse(ResultFormat::Junit, hidden.as_bytes())
        .expect("hidden characters in a message are replaced");
    assert_eq!(
        parsed.rows[0].message.as_deref(),
        Some("left\u{fffd}right red\nnext")
    );
}

#[test]
fn a_repeated_name_is_suffixed_on_every_occurrence() {
    let names = [
        "works".to_string(),
        "works".to_string(),
        "works [2]".to_string(),
        "works".to_string(),
    ];
    let parsed = parse(ResultFormat::Libtest, libtest_block(&names).as_bytes()).expect("parses");
    let got: Vec<&str> = parsed.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        got,
        vec!["works [1]", "works [3]", "works [2]", "works [4]"]
    );

    let suffixed_twice = [
        "a [1]".to_string(),
        "a [1]".to_string(),
        "a".to_string(),
        "a".to_string(),
    ];
    let parsed = parse(
        ResultFormat::Libtest,
        libtest_block(&suffixed_twice).as_bytes(),
    )
    .expect("parses");
    let got: Vec<&str> = parsed.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(got, vec!["a [1] [1]", "a [1] [2]", "a [2]", "a [3]"]);

    let shadowed = format!(
        "{}running 1 test\ntest tests::it_works ... FAILED\n\nfailures:\n\nfailures:\n    tests::it_works\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        libtest_block(&["tests::it_works".to_string()])
    );
    let parsed = parse(ResultFormat::Libtest, shadowed.as_bytes()).expect("parses");
    assert!(parsed.rows.iter().all(|row| row.name != "tests::it_works"));
    assert_eq!(
        parsed
            .rows
            .iter()
            .map(|row| (row.name.as_str(), row.outcome))
            .collect::<Vec<_>>(),
        vec![
            ("tests::it_works [1]", ResultOutcome::Passed),
            ("tests::it_works [2]", ResultOutcome::Failed),
        ]
    );

    let joined = "<testsuite><testcase classname=\"a\" name=\"b.c\"/><testcase classname=\"a.b\" name=\"c\"/><testcase classname=\"a\" name=\"d\"/></testsuite>";
    let parsed = parse(ResultFormat::Junit, joined.as_bytes()).expect("parses");
    let got: Vec<&str> = parsed.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(got, vec!["a.b.c [1]", "a.b.c [2]", "a.d"]);
}

#[test]
fn test_output_inside_a_failure_section_never_changes_the_report_structure() {
    let original = parse(ResultFormat::Libtest, LIBTEST_FAILED.as_bytes()).expect("parses");
    let marker = "assertion failed: !process_group_alive(second.process_group.unwrap())\n";
    assert!(LIBTEST_FAILED.contains(marker));
    let printed = LIBTEST_FAILED.replace(
        marker,
        &format!(
            "{marker}error[E0001]: printed by the test\nerror: could not compile `printed`\nrunning 3 tests\ntest injected ... ok\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        ),
    );
    let parsed = parse(ResultFormat::Libtest, printed.as_bytes())
        .expect("printed output stays inside its section");
    assert!(!parsed.build_failed);
    assert_eq!(parsed.summary, original.summary);
    let outcomes = |parsed: &ParsedResults| {
        parsed
            .rows
            .iter()
            .map(|row| (row.name.clone(), row.outcome))
            .collect::<Vec<_>>()
    };
    assert_eq!(outcomes(&parsed), outcomes(&original));
    let failed = row(
        &parsed,
        "research_that_runs_out_of_time_is_stopped_and_retried_then_failed",
    );
    assert_eq!(failed.outcome, ResultOutcome::Failed);
    assert!(
        failed
            .message
            .as_deref()
            .is_some_and(|message| message.contains("error[E0001]: printed by the test")),
        "{failed:?}"
    );

    let uncaptured = LIBTEST_FAILED.replace(
        "running 9 tests\n",
        "running 9 tests\nerror[E0002]: printed with --nocapture\n",
    );
    assert_ne!(uncaptured, LIBTEST_FAILED);
    let parsed = parse(ResultFormat::Libtest, uncaptured.as_bytes()).expect("parses");
    assert!(!parsed.build_failed);
    assert_eq!(outcomes(&parsed), outcomes(&original));

    let last_ok = "test two_claimers_never_claim_the_same_job ... ok\n";
    assert!(LIBTEST_FAILED.contains(last_ok));
    let show_output = LIBTEST_FAILED.replace(
        last_ok,
        &format!(
            "{last_ok}\nsuccesses:\n\n---- two_claimers_never_claim_the_same_job stdout ----\nclaimed once\n\n\nsuccesses:\n    two_claimers_never_claim_the_same_job\n"
        ),
    );
    let parsed = parse(ResultFormat::Libtest, show_output.as_bytes())
        .expect("--show-output sections end at the successes list");
    assert_eq!(parsed.summary, original.summary);
    assert_eq!(outcomes(&parsed), outcomes(&original));
}
