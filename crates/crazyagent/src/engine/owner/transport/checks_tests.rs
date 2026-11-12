use super::*;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Every `checks_*` code quoted in one source text, single or double quoted as Rust strings
/// and SQL text quote them; a log line such as `checks_question_unsettled code=` is not a
/// code. Input: the text. Output: the codes.
fn quoted_codes(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for quote in ['"', '\''] {
        let opening = format!("{quote}checks_");
        let mut rest = text;
        while let Some(at) = rest.find(&opening) {
            let tail = &rest[at + 1..];
            let end = tail
                .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
                .unwrap_or(tail.len());
            if tail[end..].starts_with(quote) {
                found.insert(tail[..end].to_owned());
            }
            rest = &tail[end..];
        }
    }
    found
}

/// The engine sources a Checks request or a checks tool reaches: the checks store and every
/// module under `checks/` but its tests, this protocol and the agent tools.
fn sources() -> Vec<PathBuf> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        src.join("checks.rs"),
        src.join("engine/owner/transport/checks.rs"),
        src.join("team/tool/checks.rs"),
    ];
    for entry in std::fs::read_dir(src.join("checks")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    assert!(files.len() >= 12, "{files:?}");
    files
}

#[test]
fn checks_codes_cover_every_refusal() {
    assert!(
        CHECKS_CODES.windows(2).all(|pair| pair[0] < pair[1]),
        "CHECKS_CODES is sorted and has no repeats"
    );
    let listed: BTreeSet<&str> = CHECKS_CODES.iter().copied().collect();
    let mut quoted = BTreeSet::new();
    for file in sources() {
        quoted.extend(quoted_codes(&std::fs::read_to_string(&file).unwrap()));
    }
    for tool in crate::team::tool::CHECKS_TOOLS {
        assert!(
            quoted.remove(tool),
            "the agent tool names are quoted too: {tool}"
        );
    }
    assert!(quoted.len() >= 40, "{quoted:?}");
    let missing: Vec<&String> = quoted
        .iter()
        .filter(|code| !listed.contains(code.as_str()))
        .collect();
    assert!(missing.is_empty(), "not in CHECKS_CODES: {missing:?}");
    let unlisted: Vec<&&str> = super::super::questions::QUESTIONS_CODES
        .iter()
        .filter(|code| !listed.contains(**code))
        .collect();
    assert!(
        unlisted.is_empty(),
        "a proposal raises an owner question and travels on the owner connection: {unlisted:?}"
    );
    for code in [
        "changes_not_found",
        "changes_stale",
        "checks_not_run",
        "checks_failed",
    ] {
        assert!(listed.contains(code), "{code}");
    }
    #[cfg(unix)]
    for code in crate::checks::gate::CODES {
        assert!(
            listed.contains(code),
            "a gate gap is a refusal code too: {code}"
        );
    }

    let oversized = vec![b' '; REQUEST_BYTES_MAX + 1];
    let refused = [
        parse(&oversized).unwrap_err(),
        parse(b"{}").unwrap_err(),
        parse(br#"{"checks":{"op":"profile_show","args":{"project":"p","id":"x"}}}"#).unwrap_err(),
        parse(br#"{"checks":{"op":"approve_all","args":{}}}"#).unwrap_err(),
        parse(br#"{"checks":{"op":"cancel","args":{"project":"p","change_set":"00","x":1}}}"#)
            .unwrap_err(),
    ];
    assert_eq!(refused[0].0, "checks_request_bounds");
    for refusal in &refused {
        assert!(listed.contains(refusal.0.as_str()), "{refusal:?}");
    }
    assert!(
        refused[1..]
            .iter()
            .all(|refusal| refusal.0 == "checks_request_invalid")
    );
    let frame = refusal("checks_rate_limited").unwrap();
    let reply: Reply = serde_json::from_slice(&frame[4..]).unwrap();
    assert!(matches!(reply, Reply::Rejected { code } if code == "checks_rate_limited"));
}

#[test]
fn a_checks_request_reads_back_as_it_was_sent() {
    let project = ProjectId::new("project-1").unwrap();
    let request = ChecksRequest::SettingsSave {
        project: Some(project.clone()),
        values: ChecksChoice {
            agent_requests: Some(2),
            ..ChecksChoice::default()
        },
        version: 3,
    };
    let body = serde_json::to_vec(&Envelope {
        checks: request.clone(),
    })
    .unwrap();
    let text: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(text["checks"]["op"], "settings_save");
    assert_eq!(text["checks"]["args"]["values"]["agent_requests"], 2);
    assert_eq!(parse(&body).unwrap(), request);
    let app = br#"{"checks":{"op":"settings_show","args":{}}}"#;
    assert_eq!(
        parse(app).unwrap(),
        ChecksRequest::SettingsShow { project: None }
    );
}
