use super::*;
use crate::harnesses::interactive::settings::TerminalSettings;
use crate::harnesses::interactive::{Run, command_with};
use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest};

const APPEND: &str = include_str!("../../tests/fixtures/harness/standin-jsonl-append.yaml");
const REWRITE: &str = include_str!("../../tests/fixtures/harness/standin-json-rewrite.yaml");
const CONFIG: &str = "STANDIN_APPEND_CONFIG";

/// The request a live master's pane on a listed harness starts with: its private home on a
/// keyless endpoint account, tools on, inside the sandbox profile. Inputs: the harness id,
/// the home, the working folder and the profile. Output: the request.
fn pane_request(harness: &str, home: &Path, work: &Path, profile: &Path) -> LaunchRequest {
    LaunchRequest {
        harness: harness.into(),
        executable: PathBuf::from("/usr/bin/true"),
        working_dir: work.to_path_buf(),
        account: AccountBinding::Url {
            home: home.to_path_buf(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            key: None,
        },
        model: None,
        tools: true,
        sandbox_profile: Some(profile.to_path_buf()),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits::default(),
    }
}

/// DEC-180 T20 M8 (Q4): a live master's pane on a listed harness takes its installed team
/// context when its manifest renders MCP servers. The pane gets the variables the home
/// block sets, and the servers are only in the file the manifest names, so no secret is in
/// its arguments or environment. A manifest without an `mcp` render keeps the refusal.
#[cfg(target_os = "macos")]
#[test]
fn a_listed_pane_takes_its_team_context_only_when_its_manifest_renders_mcp_servers() {
    let (_folder, home) = harness_home("standin-jsonl-append");
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().canonicalize().unwrap();
    let profile = work.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let run = Run {
        session_id: "standin-1",
        key_file: None,
        resume: false,
    };
    let appended = parse(APPEND.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&appended).unwrap();
    let installed = context().install_listed(&appended, &home, true).unwrap();
    let request = pane_request("standin-jsonl-append", &home, &work, &profile);

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let built = built.unwrap();
    let config = home.join("config");
    let set = built.env.iter().find(|pair| pair.0 == CONFIG);
    assert_eq!(set.map(|(_, value)| value.as_str()), config.to_str());
    let servers = servers_in(&config.join("mcp.json")).unwrap();
    assert!(servers["mcpServers"].get("files").is_some(), "{servers}");
    let values = built.env.iter().map(|(_, value)| value);
    let mut parts = built.argv.iter().chain(values);
    assert!(parts.all(|part| !part.contains("s3cret")));

    let (_other, plain) = harness_home("standin-json-rewrite");
    let rewrite = parse(REWRITE.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&rewrite).unwrap();
    let installed = context().install_listed(&rewrite, &plain, true).unwrap();
    let request = pane_request("standin-json-rewrite", &plain, &work, &profile);
    let refused = command_with(&request, Some(&installed), &run, Some(&settings));
    assert_eq!(refused.err(), Some(failure("launch_context_unsupported")));
}

/// DEC-180 T20 M8 (T19b 19b-2): a live master's pane on a terminal-only listed harness is
/// handed its keyless account as its piped launch is: the account's address and the local
/// placeholder under the names its manifest's accounts entry lists, the placeholder the one
/// exception to the variable guard. A hand-over made for another address is refused.
#[cfg(target_os = "macos")]
#[test]
fn a_listed_pane_is_handed_its_keyless_account_and_refuses_one_for_another_address() {
    use crate::model_gateway::relay::LOCAL_PLACEHOLDER;
    let (_folder, home) = harness_home("standin-jsonl-append");
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().canonicalize().unwrap();
    let profile = work.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let run = Run {
        session_id: "standin-1",
        key_file: None,
        resume: false,
    };
    let appended = parse(APPEND.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&appended).unwrap();
    let registry = crate::harnesses::registry::registry();
    let harness = registry.get("standin-jsonl-append").unwrap();
    let connection = openai_chat(KEYLESS_URL, false);
    let handover = crate::harnesses::accounts::handover(harness, &connection, None);
    assert!(handover.is_ok(), "{:?}", handover.as_ref().err());
    let context = context().with_handover(handover.unwrap());
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let request = pane_request("standin-jsonl-append", &home, &work, &profile);

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let env = built.unwrap().env;
    let value = |name: &str| {
        let found = env.iter().find(|pair| pair.0 == name);
        found.map(|pair| pair.1.as_str())
    };
    assert_eq!(value("OPENAI_BASE_URL"), Some(KEYLESS_URL));
    assert_eq!(value("OPENAI_API_KEY"), Some(LOCAL_PLACEHOLDER));
    let mut other = pane_request("standin-jsonl-append", &home, &work, &profile);
    other.account = AccountBinding::Url {
        home: home.clone(),
        base_url: "http://127.0.0.1:9/v1".into(),
        key: None,
    };
    let refused = command_with(&other, Some(&installed), &run, Some(&settings));
    let mismatch = failure("launch_account_handover_mismatch");
    assert_eq!(refused.err(), Some(mismatch));
}

/// rv-ds241 N1 (T20 M8): a pane is given its account hand-over only inside the project
/// sandbox, as the piped launch is, so a hand-over to a pane at Full access is refused as
/// the piped launch refuses it, before any other check of the pane.
#[cfg(target_os = "macos")]
#[test]
fn a_listed_pane_at_full_access_is_refused_its_hand_over() {
    let (_folder, home) = harness_home("standin-jsonl-append");
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().canonicalize().unwrap();
    let run = Run {
        session_id: "standin-1",
        key_file: None,
        resume: false,
    };
    let appended = parse(APPEND.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&appended).unwrap();
    let registry = crate::harnesses::registry::registry();
    let harness = registry.get("standin-jsonl-append").unwrap();
    let connection = openai_chat(KEYLESS_URL, false);
    let handover = crate::harnesses::accounts::handover(harness, &connection, None);
    let context = context().with_handover(handover.unwrap());
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let unused = work.join("project.sb");
    let mut request = pane_request("standin-jsonl-append", &home, &work, &unused);
    request.sandbox_profile = None;
    request.unconfined = true;

    let refused = command_with(&request, Some(&installed), &run, Some(&settings));

    assert_eq!(refused.err(), Some(failure("launch_environment_refused")));
}

/// Builds a pane on crossed harnesses: a context installed for one stand-in's manifest in
/// that stand-in's own home, the terminal settings of a manifest and a request naming a
/// harness, and drops what [`command_with`] gives. Inputs: the installed manifest, the
/// settings' manifest and the request's harness.
#[cfg(target_os = "macos")]
fn crossed(installed: &str, settings: &str, harness: &str) {
    let written = parse(installed.as_bytes()).unwrap();
    let (_folder, home) = harness_home(&written.id);
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().canonicalize().unwrap();
    let profile = work.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let run = Run {
        session_id: "standin-1",
        key_file: None,
        resume: false,
    };
    let context = context().install_listed(&written, &home, true).unwrap();
    let read = parse(settings.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&read).unwrap();
    let request = pane_request(harness, &home, &work, &profile);
    let _ = command_with(&request, Some(&context), &run, Some(&settings));
}

/// rv-sa241 N2 (T20 M8): a listed context installed for one harness is never used under
/// another harness's settings, so one harness's MCP file never reaches another's pane.
#[cfg(target_os = "macos")]
#[test]
#[should_panic(expected = "a listed context written for another harness")]
fn a_listed_context_of_one_harness_under_another_s_settings_trips_its_assert() {
    crossed(APPEND, REWRITE, "standin-json-rewrite");
}

/// rv-sa241 N2 (T20 M8): a pane's settings name the request's own harness, so a request
/// for another harness than its settings and installed context trips the assert.
#[cfg(target_os = "macos")]
#[test]
#[should_panic(expected = "a pane request for another harness than its settings")]
fn a_listed_pane_for_another_harness_than_its_settings_trips_its_assert() {
    crossed(APPEND, APPEND, "standin-json-rewrite");
}
