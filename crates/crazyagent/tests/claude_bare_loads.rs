#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RUN_TIME_MAX: Duration = Duration::from_secs(120);
const PLACEHOLDER_KEY: &str = "crazyagent-offline-placeholder";
const PLUGIN: &str = "bare-check";
const PLUGIN_SKILL: &str = "bare-check:bare-check";
const PLUGIN_SERVER: &str = "plugin:bare-check:bare-mcp";
const HOOK_MARKER: &str = "session-start-ran";

/// What one offline `claude -p` run showed. Fields: its stream-json events, and whether
/// the plugin's `SessionStart` hook wrote its marker into the plugin folder.
struct Run {
    events: Vec<serde_json::Value>,
    hook_ran: bool,
}

impl Run {
    /// The run's `system`/`init` event. Output: the event; panics when the run wrote none.
    fn init(&self) -> &serde_json::Value {
        self.events
            .iter()
            .find(|event| event["type"] == "system" && event["subtype"] == "init")
            .unwrap_or_else(|| panic!("claude wrote no init event: {:?}", self.events))
    }

    /// The names the init event lists under `field`: plain strings, or each item's `name`.
    /// Input: the init field. Output: the names; panics when the field is not a list.
    fn listed(&self, field: &str) -> Vec<&str> {
        self.init()[field]
            .as_array()
            .unwrap_or_else(|| panic!("the init event has no {field} list"))
            .iter()
            .map(|item| {
                item.as_str()
                    .or_else(|| item["name"].as_str())
                    .unwrap_or_else(|| panic!("an init {field} item has no name: {item}"))
            })
            .collect()
    }

    /// Tells whether any event reports a hook starting. Output: the answer.
    fn hook_started(&self) -> bool {
        self.events
            .iter()
            .any(|event| event["type"] == "system" && event["subtype"] == "hook_started")
    }
}

fn claude() -> PathBuf {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("claude"))
        .find(|candidate| candidate.is_file())
        .expect("UNAVAILABLE: no claude executable on PATH")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn closed_port() -> u16 {
    let listener = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn run(bare: bool) -> Run {
    let root = tempfile::Builder::new()
        .prefix("ca-bare-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    let plugin = base.join("plugin");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bare-plugin"),
        &plugin,
    );
    let stream = base.join("stream.jsonl");

    let mut command = Command::new(claude());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &home)
        .env("CLAUDE_CODE_TMPDIR", &home)
        .env("TERM", "dumb")
        .env("DISABLE_TELEMETRY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("CLAUDE_CODE_MAX_RETRIES", "0")
        .env("ANTHROPIC_API_KEY", PLACEHOLDER_KEY)
        .env(
            "ANTHROPIC_BASE_URL",
            format!("http://127.0.0.1:{}", closed_port()),
        )
        .current_dir(&work)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stream).unwrap())
        .stderr(Stdio::null());
    if bare {
        command.arg("--bare");
    }
    command
        .args(["-p", "check", "--plugin-dir"])
        .arg(&plugin)
        .args(["--output-format", "stream-json", "--verbose"]);
    let mut child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
    let deadline = Instant::now() + RUN_TIME_MAX;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("claude did not finish within {RUN_TIME_MAX:?} with the provider port closed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let events = fs::read_to_string(&stream)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("claude wrote a line that is not JSON: {line}"))
        })
        .collect();
    Run {
        events,
        hook_ran: plugin.join(HOOK_MARKER).exists(),
    }
}

#[test]
fn claude_bare_loads_a_plugin_dir_s_skills_but_runs_none_of_its_hooks_or_servers() {
    let run = run(true);

    assert!(
        run.listed("plugins").contains(&PLUGIN),
        "--bare loads the plugin dir: {:?}",
        run.listed("plugins")
    );
    assert!(
        run.listed("skills").contains(&PLUGIN_SKILL),
        "--bare lists the plugin's skill: {:?}",
        run.listed("skills")
    );
    assert!(!run.hook_ran, "--bare runs no plugin SessionStart hook");
    assert!(!run.hook_started(), "--bare starts no hook at all");
    assert_eq!(
        run.listed("mcp_servers"),
        Vec::<&str>::new(),
        "--bare starts no server, so none from the plugin's .mcp.json"
    );
}

#[test]
fn without_bare_the_same_plugin_dir_runs_its_session_start_hook_and_lists_its_server() {
    let run = run(false);

    assert!(
        run.listed("skills").contains(&PLUGIN_SKILL),
        "the plugin's skill is listed: {:?}",
        run.listed("skills")
    );
    assert!(run.hook_ran, "the plugin's SessionStart hook runs");
    assert!(run.hook_started(), "the run reports the hook starting");
    assert_eq!(
        run.listed("mcp_servers"),
        [PLUGIN_SERVER],
        "the plugin's .mcp.json server is the only server listed"
    );
}
