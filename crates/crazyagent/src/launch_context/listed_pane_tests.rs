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
/// handed its keyless account as its piped launch is: the account's address in its
/// variables, and the local placeholder under the names its manifest's accounts entry lists
/// through the pane's bridge, never its variables. A hand-over for another address is refused.
#[test]
fn a_listed_pane_is_handed_its_keyless_account_and_refuses_one_for_another_address() {
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
    assert_eq!(value("OPENAI_API_KEY"), None);
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

/// DEC-180 T20 M8 (a): a live master's pane on a terminal-only listed harness on a keyed
/// account is handed the engine relay's loopback address, and its bridge reads the relay's
/// token from the key file into the name its manifest lists; the token is in no argument or
/// pane variable, the saved key in none and in no file of the pane's home. Without a
/// hand-over a key is refused.
#[test]
fn a_keyed_listed_pane_is_handed_its_relay_and_token_and_never_the_key() {
    use crate::credentials::SecretString;
    use crate::harnesses::launch::Secret;
    use crate::model_gateway::relay::Relay;
    use std::ffi::OsStr;
    let (_folder, home) = harness_home("standin-jsonl-append");
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().canonicalize().unwrap();
    let profile = work.join("project.sb");
    fs::write(&profile, "(version 1)\n(allow default)\n").unwrap();
    let saved = SecretString::new(SAVED_KEY.as_bytes().to_vec()).unwrap();
    let relay = Relay::start(KEYED_URL, saved).unwrap();
    let registry = crate::harnesses::registry::registry();
    let harness = registry.get("standin-jsonl-append").unwrap();
    let connection = openai_chat(KEYED_URL, true);
    let handover = crate::harnesses::accounts::handover(harness, &connection, Some(&relay));
    assert!(handover.is_ok(), "{:?}", handover.as_ref().err());
    let appended = parse(APPEND.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&appended).unwrap();
    let context = context().with_handover(handover.unwrap());
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let token = Secret::new(relay.token().to_owned()).unwrap();
    let key_file = crate::harnesses::interactive::write_key(&home, &token).unwrap();
    let mut request = pane_request("standin-jsonl-append", &home, &work, &profile);
    request.account = AccountBinding::Url {
        home: home.clone(),
        base_url: relay.base_url(),
        key: Some(token),
    };
    let run = Run {
        session_id: "standin-1",
        key_file: Some(&key_file),
        resume: false,
    };

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let built = built.unwrap();
    let address = built.env.iter().find(|pair| pair.0 == "OPENAI_BASE_URL");
    assert_eq!(address.map(|pair| &pair.1), Some(&relay.base_url()));
    for (name, value) in &built.env {
        assert!(!value.contains(relay.token()), "{name}");
        let (name, value) = (OsStr::new(name), OsStr::new(value));
        let allowed = crate::sandbox::agent_variable_allowed(name, value);
        assert!(allowed, "{name:?}");
    }
    let at = built.argv.iter().position(|word| word == "-c").unwrap();
    let bridge = &built.argv[at + 1..at + 6];
    let file = key_file.to_str().unwrap();
    assert_eq!(bridge[2..], ["file", file, "OPENAI_API_KEY"]);
    let shown = std::process::Command::new("/bin/sh")
        .arg("-c")
        .args(bridge)
        .args(["/usr/bin/printenv", "OPENAI_API_KEY"])
        .env_clear()
        .output()
        .unwrap();
    assert!(shown.status.success(), "{shown:?}");
    let printed = String::from_utf8_lossy(&shown.stdout);
    assert_eq!(printed.trim(), relay.token());
    let values = built.env.iter().flat_map(|(name, value)| [name, value]);
    for part in built.argv.iter().chain(values) {
        assert!(!part.contains(SAVED_KEY), "{part}");
        assert!(!part.contains(KEYED_URL), "{part}");
    }
    assert!(built.argv.iter().all(|part| !part.contains(relay.token())));
    let mut folders = vec![home.clone()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() {
                let bytes = fs::read(entry.path()).unwrap();
                let shown = String::from_utf8_lossy(&bytes);
                assert!(!shown.contains(SAVED_KEY), "{}", entry.path().display());
            }
        }
    }
    let (_other, bare) = harness_home("standin-jsonl-append");
    let plain = super::context()
        .install_listed(&appended, &bare, true)
        .unwrap();
    let refused = command_with(&request, Some(&plain), &run, Some(&settings));
    assert_eq!(refused.err(), Some(failure("launch_key_relay_unsupported")));
}

/// rv-ds241 N1 (T20 M8): a pane is given its account hand-over only inside the project
/// sandbox, as the piped launch is, so a hand-over to a pane at Full access is refused as
/// the piped launch refuses it, before any other check of the pane.
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
#[test]
#[should_panic(expected = "a listed context written for another harness")]
fn a_listed_context_of_one_harness_under_another_s_settings_trips_its_assert() {
    crossed(APPEND, REWRITE, "standin-json-rewrite");
}

/// rv-sa241 N2 (T20 M8): a pane's settings name the request's own harness, so a request
/// for another harness than its settings and installed context trips the assert.
#[test]
#[should_panic(expected = "a pane request for another harness than its settings")]
fn a_listed_pane_for_another_harness_than_its_settings_trips_its_assert() {
    crossed(APPEND, APPEND, "standin-json-rewrite");
}

/// DEC-180 T20 M8: every variable a listed pane with an account hand-over is built with
/// passes the terminal host's variable guard, which refuses a key name even holding the
/// local placeholder, so the names the hand-over lists reach the harness only through the
/// bridge its sandboxed command starts with, which exports them as the harness starts.
#[test]
fn a_handed_over_pane_passes_the_host_guard_and_its_bridge_exports_the_names() {
    use crate::model_gateway::relay::LOCAL_PLACEHOLDER;
    use std::ffi::OsStr;
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
    let context = context().with_handover(handover.unwrap());
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let request = pane_request("standin-jsonl-append", &home, &work, &profile);

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let built = built.unwrap();
    for (name, value) in &built.env {
        let (name, value) = (OsStr::new(name), OsStr::new(value));
        let allowed = crate::sandbox::agent_variable_allowed(name, value);
        assert!(allowed, "{name:?}");
    }
    let (sandbox, place) = (crate::sandbox::SANDBOX_EXEC, profile.to_str().unwrap());
    assert_eq!(built.argv[..3], [sandbox, "-f", place]);
    assert_eq!(built.argv[3..5], ["/bin/sh", "-c"]);
    let words = ["handed", "word", LOCAL_PLACEHOLDER, "OPENAI_API_KEY"];
    assert_eq!(built.argv[6..10], words);
    assert_eq!(built.argv[10..12], ["/usr/bin/true", "--home"]);
    let bridge = &built.argv[5..10];
    let shown = std::process::Command::new("/bin/sh")
        .arg("-c")
        .args(bridge)
        .args(["/usr/bin/printenv", "OPENAI_API_KEY"])
        .env_clear()
        .output()
        .unwrap();
    assert!(shown.status.success(), "{shown:?}");
    let printed = String::from_utf8_lossy(&shown.stdout);
    assert_eq!(printed.trim(), LOCAL_PLACEHOLDER);
}

/// rv-ds241 t20keyed nit: a pane's bridge takes out of its variables only the names the
/// account hand-over lists, so a home block variable that holds the same value stays one
/// of the pane's own variables.
#[test]
fn a_pane_s_bridge_takes_only_the_names_its_hand_over_lists() {
    use crate::model_gateway::relay::LOCAL_PLACEHOLDER;
    let line = "    STANDIN_APPEND_CONFIG: \"{home}/config\"\n";
    let both = format!("{line}    STANDIN_APPEND_MODE: \"{LOCAL_PLACEHOLDER}\"\n");
    let text = APPEND.replacen(line, &both, 1);
    assert_ne!(text, APPEND);
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
    let appended = parse(text.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&appended).unwrap();
    let registry = crate::harnesses::registry::registry();
    let harness = registry.get("standin-jsonl-append").unwrap();
    let connection = openai_chat(KEYLESS_URL, false);
    let handover = crate::harnesses::accounts::handover(harness, &connection, None);
    let context = context().with_handover(handover.unwrap());
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let request = pane_request("standin-jsonl-append", &home, &work, &profile);

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let built = built.unwrap();
    let name = "STANDIN_APPEND_MODE";
    let mode = built.env.iter().find(|pair| pair.0 == name);
    assert_eq!(mode.map(|pair| pair.1.as_str()), Some(LOCAL_PLACEHOLDER));
    assert!(built.argv.iter().all(|word| !word.contains(name)));
}

/// fix9 T19b F7 for panes: a live master's pane on a listed harness takes its temp folder,
/// agent `getconf` and cargo home from the agent home its context names as tool home, as
/// its piped launch does, while its HOME stays the private home.
#[test]
fn a_listed_pane_takes_its_temp_folder_and_tools_from_its_tool_home() {
    struct TmpRemoved(PathBuf);
    impl Drop for TmpRemoved {
        fn drop(&mut self) {
            let _ = crate::sandbox::toolchains::remove_tmp(&self.0);
        }
    }
    let (folder, home) = harness_home("standin-jsonl-append");
    let base = folder.path().canonicalize().unwrap();
    let _removed = TmpRemoved(base.clone());
    let made = crate::sandbox::toolchains::make_tmp(&base).unwrap();
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
    let context = context().with_tool_home(&base);
    let installed = context.install_listed(&appended, &home, true).unwrap();
    let request = pane_request("standin-jsonl-append", &home, &work, &profile);

    let built = command_with(&request, Some(&installed), &run, Some(&settings));

    assert!(built.is_ok(), "{:?}", built.as_ref().err());
    let env = built.unwrap().env;
    let value = |name: &str| {
        let found = env.iter().find(|pair| pair.0 == name);
        found.map(|pair| PathBuf::from(&pair.1))
    };
    assert_eq!(value("HOME"), Some(home.clone()));
    assert_eq!(value("TMPDIR"), Some(made));
    assert_eq!(value("CARGO_HOME"), Some(base.join(".cargo")));
    let getconf = base.join(STATE).join("bin");
    let path = env.iter().find(|pair| pair.0 == "PATH").unwrap();
    let lead = format!("{}:", getconf.display());
    assert!(path.1.starts_with(&lead), "{path:?}");
}
