use super::*;

fn kept(kind: &'static str, project_id: Option<&str>) -> PendingIntent {
    PendingIntent {
        kind,
        project_id: project_id.map(str::to_owned),
        baseline_id: None,
        generation: "1".into(),
        model_access: None,
        project_spec: None,
        operation_id: "operation-1".into(),
    }
}

/// DEC-178 addendum 1: a started onboarding's receipt names the project the engine gave,
/// `my-app-2` when `my-app` is held by a removal; a replayed plain Applied names none, so
/// the window finds the project by its folder (rv-secreview S2 F1), and no other change
/// may carry an engine-given project.
#[test]
fn a_started_onboarding_receipt_names_the_project_the_engine_gave() {
    let started = || OwnerResult::OnboardingStarted {
        project_id: ProjectId::new("my-app-2").unwrap(),
    };
    let mut intent = kept("start_onboarding", Some("my-app"));
    let (outcome, refused) = settled(&mut intent, started()).unwrap();
    assert_eq!(outcome, ActionOutcome::Applied);
    assert_eq!(refused, None);
    assert_eq!(intent.project_id.as_deref(), Some("my-app-2"));

    let mut replayed = kept("start_onboarding", None);
    let (outcome, _) = settled(&mut replayed, OwnerResult::Applied).unwrap();
    assert_eq!(outcome, ActionOutcome::Applied);
    assert_eq!(replayed.project_id, None);

    let mut other = kept("remove_project", Some("notes"));
    assert_eq!(
        settled(&mut other, started()).unwrap_err().code,
        "unexpected_reply"
    );
    assert_eq!(other.project_id.as_deref(), Some("notes"));

    let mut refused = kept("start_onboarding", Some("my-app"));
    let (outcome, code) = settled(
        &mut refused,
        OwnerResult::Rejected {
            code: "onboarding_project_ids_used".into(),
        },
    )
    .unwrap();
    assert_eq!(outcome, ActionOutcome::Rejected);
    assert_eq!(code.as_deref(), Some("onboarding_project_ids_used"));
    assert_eq!(refused.project_id.as_deref(), Some("my-app"));
}

#[test]
fn every_bridge_error_function_keeps_an_engine_version_mismatch() {
    const CODE: &str = "engine_version_mismatch";
    let engine = || crazyagent::engine::EngineError(CODE.to_owned());
    let mapped = [
        ("changes", changes::changes_error(engine())),
        ("usage", usage::usage_error(engine())),
        ("agents", agents::agent_error(engine())),
        ("process", process::process_error(engine())),
        ("questions", questions::questions_error(engine())),
        ("roadmap", roadmap::roadmap_error(engine())),
        ("recovery", recovery::recovery_error(engine())),
        ("cleanup", cleanup::cleanup_error(engine())),
        ("ide reveal", ide_reveal::ide_error(engine())),
        ("ide extensions", ide_extensions::extensions_error(engine())),
        ("ide tab", ide::ide_error(engine())),
        ("notify", notify::notify_error(engine())),
        ("native auth", native_auth::auth_error(engine())),
        ("account profile", account_profile::profile_error(engine())),
        ("accounts", accounts::account_error(engine())),
        ("plugins", plugins::plugin_failure(engine())),
        ("team", team::read_failure(CODE)),
        ("project setup", project_setup::read_failure(CODE)),
        (
            "intake and github",
            intake::known(CODE, &[], "intake_failed"),
        ),
        ("harness catalog", caused("owner_read_failed", CODE)),
        ("master open", master::open_failure(CODE)),
    ];
    let dropped: Vec<&str> = mapped
        .iter()
        .filter(|(_, error)| error.code != CODE || error.cause.is_some())
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(dropped, Vec::<&str>::new());
}

#[test]
fn a_project_list_stops_at_its_bound_says_so_and_fits_the_worst_rows() {
    let rows = |count: usize| -> Vec<ProjectRow> {
        (1..=count)
            .map(|at| ProjectRow {
                id: format!("p-{at}"),
                name: format!("Project {at}"),
                repository_count: 0,
            })
            .collect()
    };
    let all = cut_projects(rows(20), false).unwrap();
    assert_eq!((all.items.len(), all.more), (20, false));
    let full = cut_projects(rows(PROJECT_LIST_MAX), false).unwrap();
    assert_eq!((full.items.len(), full.more), (PROJECT_LIST_MAX, false));
    let over = cut_projects(rows(PROJECT_LIST_MAX + 5), false).unwrap();
    assert_eq!((over.items.len(), over.more), (PROJECT_LIST_MAX, true));
    assert_eq!(
        over.items.last().unwrap().id,
        format!("p-{PROJECT_LIST_MAX}")
    );
    let cut_short = cut_projects(rows(16), true).unwrap();
    assert_eq!((cut_short.items.len(), cut_short.more), (16, true));

    let worst = (0..PROJECT_LIST_MAX)
        .map(|at| ProjectRow {
            id: format!("{at:0>128}"),
            name: "\"".repeat(256),
            repository_count: usize::MAX,
        })
        .collect();
    let worst = cut_projects(worst, true).unwrap();
    assert!(serde_json::to_vec(&worst).unwrap().len() <= PROJECT_LIST_BYTES_MAX);
    assert!(serde_json::to_vec(&worst.items[0]).unwrap().len() <= PROJECT_ROW_BYTES_MAX);
}

#[test]
fn random_ids_are_64_hex_characters_and_differ() {
    let first = native_random_id().unwrap();
    let second = native_random_id().unwrap();
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(first, second);
    assert!(
        native_project_id()
            .unwrap()
            .as_str()
            .starts_with("project-")
    );
}

#[test]
fn every_code_the_engine_refuses_an_owner_change_with_reaches_the_window_by_name() {
    fn sources(folder: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tui" && name != "tests" {
                    sources(&path, found);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let production = text
                    .find("#[cfg(test)]\nmod tests {")
                    .or_else(|| text.find("#[cfg(all(test, unix))]\nmod tests {"))
                    .map_or(text.as_str(), |end| &text[..end]);
                found.push(production.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
    }
    fn quoted_after(text: &str, mark: &str, codes: &mut Vec<String>) {
        for (at, _) in text.match_indices(mark) {
            let rest = text[at + mark.len()..].trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                codes.push(rest[..rest.find('"').unwrap()].to_owned());
            }
        }
    }
    let engine =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/crazyagent/src");
    let mut files = Vec::new();
    sources(&engine, &mut files);
    let mut codes: Vec<String> = Vec::new();
    for text in &files {
        for mark in ["reject(", "rejected(", "Rejected { code:"] {
            quoted_after(text, mark, &mut codes);
        }
    }
    let selection = std::fs::read_to_string(engine.join("master_session.rs")).unwrap();
    let start = selection.find("pub(crate) fn validate_selection(").unwrap();
    let end = start + selection[start..].find("\n}\n").unwrap();
    quoted_after(&selection[start..end], "error(", &mut codes);
    let transport = std::fs::read_to_string(engine.join("engine/owner/transport.rs")).unwrap();
    let frame = transport.find("fn owner_reply_frame(").unwrap();
    let known = &transport[frame..frame + transport[frame..].find("let response").unwrap()];
    codes.extend(known.split('"').skip(1).step_by(2).map(str::to_owned));
    codes.sort();
    codes.dedup();
    assert!(codes.len() >= 100, "{codes:?}");
    for code in [
        "master_turn_outstanding",
        "owner_busy",
        "asset_source_missing",
        "knowledge_entry_exists",
    ] {
        assert!(codes.iter().any(|found| found == code), "{code}");
    }
    for code in &codes {
        assert_eq!(rejection_code(code), code.as_str(), "{code}");
    }
    assert_eq!(rejection_code("made_up_code"), "made_up_code");
    assert_eq!(
        rejection_code("open /Users/mira/.codex/auth.json: denied"),
        crazyagent::protocol::TEXT_WITHHELD
    );
}

#[cfg(target_os = "macos")]
#[test]
fn random_ids_need_no_dev_urandom_file_as_on_windows() {
    use std::process::{Command, Stdio};
    const NO_URANDOM: &str =
        "(version 1)(allow default)(deny file-read* (literal \"/dev/urandom\"))";
    let denied = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", NO_URANDOM, "/usr/bin/head", "-c", "1", "/dev/urandom"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        !denied.status.success(),
        "the sandbox let /dev/urandom be read"
    );
    let child = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", NO_URANDOM])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bridge::tests::random_ids_are_64_hex_characters_and_differ",
                "--test-threads=1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(
        child.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(stdout.contains("1 passed"), "{stdout}");
}
