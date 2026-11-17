#![cfg(target_os = "macos")]

use crazyagent::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crazyagent::harnesses::interactive::{self, Run, support};
use crazyagent::harnesses::launch::{
    AccountBinding, LaunchLimits, LaunchRequest, Secret, command, route,
};
use crazyagent::harnesses::{HarnessEntry, Search, catalog, specs};
use crazyagent::model_gateway::relay::TOKEN_PREFIX;
use crazyagent::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use crazyagent::rcli_host::{RcliMasterBinding, RcliMasterLaunch};
use crazyagent::sandbox::toolchains::{self, ToolHomes};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FIXTURES: &str = "tests/fixtures/harness_layer";
const CAPTURE: &str = "CRAZYAGENT_PARITY_CAPTURE";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const MODEL: &str = "unit-model";
const DUMMY_KEY: &str = "sk-parity-dummy-key-0001";
const HARNESSES: [&str; 2] = ["codex", "claude-code"];
const ACCOUNTS: [&str; 2] = ["subscription", "api_key"];
const GONE: [&str; 5] = ["antigravity", "cursor-agent", "amp", "kiro", "auggie"];
const ODD_IDS: [&str; 4] = ["", "claude", "Claude", "claude-codex"];
const FILE_BYTES_MAX: u64 = 64 * 1024;
const FILES_MAX: usize = 256;
const OWNER_HOME: &str = "<OWNER_HOME>";
const DEVELOPER_TOOLS: &str = "/Library/Developer/CommandLineTools";
const RUSTUP_BINS: [&str; 2] = ["/opt/homebrew/opt/rustup/bin", "/usr/local/opt/rustup/bin"];

/// One build's temporary folders: the account home, the project folder and the folder that
/// holds the stand-in programs. Each is private to the owner, as the engine requires.
struct Roots {
    home: tempfile::TempDir,
    work: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl Roots {
    /// Makes the three folders under the real temp folder. Input: a name prefix. Output:
    /// the folders.
    fn new(prefix: &str) -> Self {
        Self {
            home: private_dir(&format!("{prefix}-home-")),
            work: private_dir(&format!("{prefix}-work-")),
            bin: private_dir(&format!("{prefix}-bin-")),
        }
    }

    /// The placeholders this build's paths read as, longest path first, each folder both as
    /// made and as resolved. Output: the pairs of text and placeholder.
    fn placeholders(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for (path, name) in [
            (self.home.path(), "<HOME>"),
            (self.work.path(), "<WORK>"),
            (self.bin.path(), "<BIN>"),
        ] {
            pairs.push((path.to_str().unwrap().to_owned(), name.to_owned()));
            let resolved = std::fs::canonicalize(path).unwrap();
            pairs.push((resolved.to_str().unwrap().to_owned(), name.to_owned()));
        }
        for (path, name) in [
            (self.work.path(), "<WORK_AS_CLAUDE_PROJECT>"),
            (self.home.path(), "<HOME_AS_CLAUDE_PROJECT>"),
        ] {
            let resolved = std::fs::canonicalize(path).unwrap();
            if let Some(folder) = interactive::claude_project_folder(&resolved) {
                pairs.push((folder, name.to_owned()));
            }
        }
        if let Ok(owner) = std::env::var("HOME")
            && owner.len() > 1
        {
            pairs.push((owner, OWNER_HOME.to_owned()));
        }
        pairs.extend(tool_placeholders());
        pairs.sort_by(|left, right| right.0.len().cmp(&left.0.len()).then(left.0.cmp(&right.0)));
        pairs.dedup();
        pairs
    }
}

/// The owner's home and tool folders a tools-on launch names, each with the fixed name a
/// snapshot holds instead, so no snapshot holds the owner's home or what this machine has
/// installed. Takes nothing. Output: the pairs of path and placeholder, the home last.
fn tool_placeholders() -> Vec<(String, String)> {
    let homes = ToolHomes::owner().unwrap();
    let mut folders: Vec<(PathBuf, &str)> = vec![
        (homes.rustup().to_path_buf(), "<OWNER_RUSTUP>"),
        (homes.cargo().to_path_buf(), "<OWNER_CARGO>"),
        (PathBuf::from(DEVELOPER_TOOLS), "<DEVELOPER_TOOLS>"),
    ];
    folders.extend(RUSTUP_BINS.map(|bin| (PathBuf::from(bin), "<RUSTUP_BIN>")));
    folders.extend(
        homes
            .xcode()
            .map(|xcode| (xcode.to_path_buf(), "<XCODE_DEVELOPER>")),
    );
    folders.extend(
        toolchains::graphify(&homes)
            .into_iter()
            .zip(["<GRAPHIFY_TOOL>", "<GRAPHIFY_PYTHON>"]),
    );
    folders.push((homes.home().to_path_buf(), OWNER_HOME));
    let pairs: Vec<(String, String)> = folders
        .into_iter()
        .map(|(path, name)| (path.to_str().unwrap().to_owned(), name.to_owned()))
        .collect();
    assert!(
        pairs
            .iter()
            .all(|(path, _)| path.len() > 1 && path.starts_with('/'))
    );
    pairs
}

/// Makes a folder only the owner can open, under the resolved real temp folder. Input: the
/// name prefix. Output: the folder, removed when dropped.
fn private_dir(prefix: &str) -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// Writes a stand-in program the owner can run, which the launch builders name but never
/// start. Inputs: the folder and the program name. Output: its resolved path.
fn stand_in(directory: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::canonicalize(path).unwrap()
}

/// Writes the project sandbox profile every launch needs, as the harness launch rows do.
/// Input: the build's folders. Output: the profile's path.
fn project_profile(roots: &Roots) -> PathBuf {
    let profile = roots.work.path().join("project.sb");
    let rules = format!(
        "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\") (subpath \"{}\"))\n",
        std::fs::canonicalize(roots.work.path()).unwrap().display(),
        std::fs::canonicalize(roots.home.path()).unwrap().display()
    );
    std::fs::write(&profile, rules).unwrap();
    profile
}

/// The account a case runs on: a sign-in or a key, each with the build's home. Inputs: the
/// folders and the account kind. Output: the binding.
fn account(roots: &Roots, kind: &str) -> AccountBinding {
    let home = roots.home.path().to_path_buf();
    match kind {
        "subscription" => AccountBinding::Subscription { home },
        "api_key" => AccountBinding::ApiKey {
            home,
            key: Secret::new(DUMMY_KEY.into()).unwrap(),
            base_url: None,
        },
        other => panic!("no account kind {other}"),
    }
}

/// The launch request one case builds. Inputs: the folders, the harness id, the account
/// kind and whether tools are on. Output: the request.
fn request(roots: &Roots, harness: &str, kind: &str, tools: bool) -> LaunchRequest {
    let program = if harness == "codex" {
        "codex"
    } else {
        "claude"
    };
    LaunchRequest {
        harness: harness.into(),
        executable: stand_in(roots.bin.path(), program),
        working_dir: roots.work.path().to_path_buf(),
        account: account(roots, kind),
        model: Some(MODEL.into()),
        tools,
        sandbox_profile: Some(project_profile(roots)),
        unconfined: false,
        git_identity: None,
        limits: LaunchLimits {
            run_time: Duration::from_secs(420),
            ..LaunchLimits::default()
        },
    }
}

/// Every file under a folder with its mode and text, by path inside the folder. Input: the
/// folder. Output: the files, sorted.
fn files(root: &Path) -> BTreeMap<String, Value> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(&directory).unwrap() {
            let path = item.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let mode = format!("{:o}", metadata.permissions().mode() & 0o7777);
            if metadata.is_dir() {
                found.insert(format!("{name}/"), json!({ "mode": mode }));
                pending.push(path);
            } else if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                found.insert(name, json!({ "link": target.to_str().unwrap() }));
            } else {
                assert!(
                    metadata.len() <= FILE_BYTES_MAX,
                    "{name} is larger than a launch writes"
                );
                let text = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
                found.insert(name, json!({ "mode": mode, "text": text }));
            }
            assert!(
                found.len() <= FILES_MAX,
                "a launch writes no more than {FILES_MAX} files"
            );
        }
    }
    found
}

/// What a build wrote into its home and project folders, the stand-in programs left out.
/// Input: the folders. Output: both trees.
fn written(roots: &Roots) -> Value {
    json!({ "home": files(roots.home.path()), "work": files(roots.work.path()) })
}

/// The program, arguments, variables and folder of a piped launch command. Input: the
/// command. Output: the record.
fn piped_record(built: &std::process::Command) -> Value {
    let args: Vec<&str> = built
        .get_args()
        .map(|argument| argument.to_str().unwrap())
        .collect();
    let env: BTreeMap<&str, Option<&str>> = built
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_str().unwrap(),
                value.map(|value| value.to_str().unwrap()),
            )
        })
        .collect();
    json!({
        "program": built.get_program().to_str().unwrap(),
        "args": args,
        "env": env,
        "cwd": built.get_current_dir().map(|path| path.to_str().unwrap()),
    })
}

/// Replaces every build-specific text in a record with its placeholder. Inputs: the record
/// and the pairs of text and placeholder. Output: the record with placeholders.
fn normalized(value: Value, pairs: &[(String, String)]) -> Value {
    match value {
        Value::String(mut text) => {
            for (from, to) in pairs {
                text = text.replace(from.as_str(), to);
            }
            Value::String(text)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalized(item, pairs))
                .collect(),
        ),
        Value::Object(fields) => {
            let mut renamed = Map::new();
            for (mut key, item) in fields {
                for (from, to) in pairs {
                    key = key.replace(from.as_str(), to);
                }
                renamed.insert(key, normalized(item, pairs));
            }
            Value::Object(renamed)
        }
        other => other,
    }
}

/// Fails when a record still holds a temp folder, the owner's home, the dummy key or a relay
/// token, so a snapshot never pins one machine's or one run's text. Inputs: the case name and
/// the record.
fn assert_portable(name: &str, record: &Value) {
    let text = record.to_string();
    let mut leftovers = vec![
        "/private/tmp/".to_owned(),
        "/private/var/folders/".to_owned(),
        "/var/folders/".to_owned(),
        "/Users/".to_owned(),
        OWNER_HOME.to_owned(),
        DUMMY_KEY.to_owned(),
        TOKEN_PREFIX.to_owned(),
    ];
    if let Ok(owner) = std::env::var("HOME")
        && owner.len() > 1
    {
        leftovers.push(owner);
    }
    leftovers.extend(tool_placeholders().into_iter().map(|(path, _)| path));
    for leftover in leftovers {
        assert!(
            !text.contains(&leftover),
            "{name} still holds {leftover}: {text}"
        );
    }
}

/// Builds a case twice under fresh folders and returns the one record both builds give,
/// failing when they differ, so a snapshot holds only what the engine decides. Inputs: the
/// case name and the build. Output: the record.
fn twice(name: &str, build: impl Fn(&Roots) -> (Value, Vec<(String, String)>)) -> Value {
    let records: Vec<Value> = (0..2)
        .map(|round| {
            let roots = Roots::new(&format!("ca-parity-{round}"));
            let (record, extra) = build(&roots);
            let mut pairs = extra;
            pairs.extend(roots.placeholders());
            pairs
                .sort_by(|left, right| right.0.len().cmp(&left.0.len()).then(left.0.cmp(&right.0)));
            normalized(record, &pairs)
        })
        .collect();
    assert_eq!(records[0], records[1], "{name}: two builds differ");
    assert_portable(name, &records[0]);
    records.into_iter().next().unwrap()
}

/// Writes the snapshot when the capture variable names a folder, or compares it with the
/// committed one otherwise; a missing snapshot fails and is never written in place. Inputs:
/// the snapshot name and the record.
fn check(name: &str, record: &Value) {
    let shown = format!("{}\n", serde_json::to_string_pretty(record).unwrap());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(folder) = std::env::var_os(CAPTURE) {
        let folder = PathBuf::from(folder);
        assert!(folder.is_absolute(), "{CAPTURE} names an absolute folder");
        let checkout = manifest.ancestors().nth(2).unwrap();
        assert!(
            !folder.starts_with(checkout),
            "{CAPTURE} names a folder outside the checkout"
        );
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{name}.json"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, shown.as_bytes()).unwrap();
        return;
    }
    let path = manifest.join(FIXTURES).join(format!("{name}.json"));
    let kept = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no snapshot at {}", path.display()));
    assert_eq!(shown, kept, "{name} differs from its snapshot");
}

/// The relay's token and address as placeholders, so a keyed build's random port and token
/// read the same in every run. Input: the relay's token and port. Output: the pairs.
fn relay_placeholders(token: &str, port: u16) -> Vec<(String, String)> {
    vec![
        (token.to_owned(), "<RELAY_TOKEN>".to_owned()),
        (
            format!("127.0.0.1:{port}"),
            "127.0.0.1:<RELAY_PORT>".to_owned(),
        ),
    ]
}

/// The piped launch of one agent: its command, or the refusal code, and what it wrote. A key
/// account is routed through the relay first, as the engine does. Inputs: the folders and the
/// case. Output: the record and the relay placeholders.
fn agent_launch(
    roots: &Roots,
    harness: &str,
    kind: &str,
    tools: bool,
) -> (Value, Vec<(String, String)>) {
    let mut extra = Vec::new();
    let mut relay_kept = None;
    let built = if kind == "api_key" {
        let (routed, relay) = route(request(roots, harness, kind, tools)).unwrap();
        let relay = relay.unwrap();
        extra = relay_placeholders(relay.token(), relay.port());
        let built = command(&routed);
        relay_kept = Some(relay);
        built
    } else {
        command(&request(roots, harness, kind, tools))
    };
    let launch = match built {
        Ok(built) => piped_record(&built),
        Err(refused) => json!({ "refused": refused.0 }),
    };
    let record = json!({ "launch": launch, "written": written(roots) });
    drop(relay_kept);
    (record, extra)
}

/// The terminal launch of one agent as the terminal host starts it: a Claude Code home made
/// ready first, the key file written for a key account, then the command or the refusal
/// code, and what was written. Inputs: the folders and the case. Output: the record and the
/// relay placeholders.
fn terminal_launch(
    roots: &Roots,
    harness: &str,
    kind: &str,
    tools: bool,
) -> (Value, Vec<(String, String)>) {
    let mut extra = Vec::new();
    let mut relay_kept = None;
    let mut launch_request = request(roots, harness, kind, tools);
    let mut key_file = None;
    if kind == "api_key" {
        let (routed, relay) = route(launch_request).unwrap();
        let relay = relay.unwrap();
        extra = relay_placeholders(relay.token(), relay.port());
        key_file = Some(
            interactive::write_key(
                roots.home.path(),
                &Secret::new(relay.token().to_owned()).unwrap(),
            )
            .unwrap(),
        );
        launch_request = routed;
        relay_kept = Some(relay);
    }
    let prepared = if harness == "claude-code" {
        interactive::prepare_claude_home(roots.home.path(), roots.work.path())
            .err()
            .map(|refused| refused.0)
    } else {
        None
    };
    let run = Run {
        session_id: SESSION,
        key_file: key_file.as_deref(),
        resume: false,
    };
    let launch = match interactive::command(&launch_request, None, &run) {
        Ok(built) => json!({
            "argv": built.argv,
            "cwd": built.cwd.to_str().unwrap(),
            "env": built.env,
            "transcript": format!("{:?}", built.transcript),
        }),
        Err(refused) => json!({ "refused": refused.0 }),
    };
    let record =
        json!({ "prepare_refused": prepared, "launch": launch, "written": written(roots) });
    drop(relay_kept);
    (record, extra)
}

/// A launch kind's build for one case: the roots, the harness, the account kind and whether
/// tools are on, to the case's record and its extra files.
type BuildCase = fn(&Roots, &str, &str, bool) -> (Value, Vec<(String, String)>);

/// Runs one launch kind over every case: Codex and Claude Code, on a sign-in and on a key,
/// with tools off and on. Inputs: the snapshot name and the build. Output: the record of
/// every case by name.
fn every_case(name: &str, build: BuildCase) -> Value {
    let mut cases = Map::new();
    for harness in HARNESSES {
        for kind in ACCOUNTS {
            for tools in [false, true] {
                let case = format!(
                    "{harness}/{kind}/tools-{}",
                    if tools { "on" } else { "off" }
                );
                let record = twice(&format!("{name} {case}"), |roots| {
                    build(roots, harness, kind, tools)
                });
                cases.insert(case, record);
            }
        }
    }
    Value::Object(cases)
}

/// The catalog fields a row held at the Task 0 commit, one by one, so fields added since
/// do not move the snapshot. Input: the row. Output: the fields.
fn task0_fields(entry: &HarnessEntry) -> Value {
    json!({
        "id": entry.id,
        "name": entry.name,
        "status": serde_json::to_value(&entry.status).unwrap(),
        "status_label": entry.status_label,
        "executable": entry.executable.as_ref().map(|path| path.to_str().unwrap()),
        "headless": entry.headless,
        "tools": serde_json::to_value(&entry.tools).unwrap(),
        "model_selection": entry.model_selection,
        "account_kinds": serde_json::to_value(&entry.account_kinds).unwrap(),
        "team": serde_json::to_value(&entry.team).unwrap(),
        "aliases": entry.aliases,
    })
}

/// The catalog for a search with no folders and for one where every listed program name is
/// a stand-in on the search folder, as the Task 0 fields of each row. Input: the folders.
/// Output: both catalogs.
fn catalogs(roots: &Roots) -> (Value, Vec<(String, String)>) {
    let empty: Vec<Value> = catalog(&Search::new(Vec::new(), None, Vec::new()))
        .iter()
        .map(task0_fields)
        .collect();
    for spec in specs() {
        for name in spec.binaries {
            stand_in(roots.bin.path(), name);
        }
    }
    let search = Search::new(
        vec![std::fs::canonicalize(roots.bin.path()).unwrap()],
        Some(std::fs::canonicalize(roots.home.path()).unwrap()),
        Vec::new(),
    );
    let installed: Vec<Value> = catalog(&search).iter().map(task0_fields).collect();
    (
        json!({ "empty": empty, "installed": installed }),
        Vec::new(),
    )
}

/// Every piped agent launch for Codex and Claude Code, on a sign-in and on a key, tools off
/// and on, equals the snapshot taken at the base commit (Task 3 and Task 8).
#[test]
fn launch_agents_match_the_base_snapshot() {
    check("launch-agents", &every_case("launch-agents", agent_launch));
}

/// Every terminal launch for Codex and Claude Code, on a sign-in and on a key, tools off and
/// on, equals the snapshot taken at the base commit (Task 3 and Task 8).
#[test]
fn launch_terminals_match_the_base_snapshot() {
    check(
        "launch-terminals",
        &every_case("launch-terminals", terminal_launch),
    );
}

/// Writes a file only the owner can read and change. Inputs: the path, the bytes and the
/// mode. Output: the bytes' SHA-256 in hex.
fn owner_file(path: &Path, bytes: &[u8], mode: u32) -> String {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

/// An RCLI master's connection to a local endpoint with no credential, as its own profile
/// lists it. Takes nothing. Output: the connection.
fn rcli_connection() -> ModelConnection {
    let named = |text: &str| CredentialRef::new(text).unwrap();
    ModelConnection {
        id: named("connection-parity"),
        provider: named("local-parity"),
        harness: named("rcli"),
        protocol: named("openai-chat"),
        account_ref: named("account-parity"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: MODEL.into(),
            author: named("model-author-parity"),
        },
        auth: ModelAuthentication::None {},
    }
}

/// The RCLI master command for that connection's profile, without and with an
/// instructions file, and the files the case holds. Input: the folders. Output: the record.
fn rcli_master(roots: &Roots) -> (Value, Vec<(String, String)>) {
    let home = std::fs::canonicalize(roots.home.path()).unwrap();
    let work = std::fs::canonicalize(roots.work.path()).unwrap();
    let bin = std::fs::canonicalize(roots.bin.path()).unwrap();
    std::fs::create_dir(work.join(".git")).unwrap();
    std::fs::create_dir(home.join(".backboard")).unwrap();
    let executable = bin.join("rcli");
    let artifact = owner_file(&executable, b"#!/bin/sh\nexit 1\n", 0o700);
    let profile = json!({
        "model": {"provider": "local-parity", "model": MODEL},
        "memory": "off",
        "providers": [{
            "id": "local-parity", "name": "Parity local endpoint",
            "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
            "auth": {"type": "none"},
            "models": [{"id": MODEL}]
        }]
    });
    let profile_sha = owner_file(
        &home.join(".backboard/config.json"),
        &serde_json::to_vec(&profile).unwrap(),
        0o600,
    );
    let instructions = bin.join("instructions.md");
    owner_file(&instructions, b"Parity master instructions.\n", 0o600);
    let mut launch = RcliMasterLaunch {
        binding: RcliMasterBinding {
            project_id: ProjectId::new("project-parity").unwrap(),
            session_id: SessionId::new("session-parity").unwrap(),
            instance_id: RequestId::new("instance-parity").unwrap(),
            connection: rcli_connection(),
            target_profile_ref: CredentialRef::new("profile-parity").unwrap(),
        },
        executable,
        artifact_sha256: artifact,
        home_dir: home,
        cwd: work,
        profile_config_sha256: profile_sha,
        instructions: None,
    };
    let plain = piped_record(&launch.command().unwrap());
    launch.instructions = Some(instructions);
    let instructed = piped_record(&launch.command().unwrap());
    let record = json!({
        "plain": plain,
        "instructions": instructed,
        "written": written(roots),
    });
    (record, Vec::new())
}

/// The RCLI master command equals the snapshot taken at the base commit (Task 5 and
/// Task 8).
#[test]
fn launch_rcli_master_matches_the_base_snapshot() {
    check(
        "launch-rcli-master",
        &twice("launch-rcli-master", rcli_master),
    );
}

/// The catalog's Task 0 fields for an empty search and for a search finding every program
/// equal the snapshot taken at the Task 0 commit, the five removed harnesses already gone
/// (Task 1 and Task 8).
#[test]
fn catalog_matches_the_task0_snapshot() {
    check("catalog", &twice("catalog", catalogs));
}

/// What `support()` answers for every harness the Task 0 catalog lists, the five removed
/// ones and a few names that are no harness equals the snapshot taken at the Task 0 commit;
/// the ids come from that snapshot once it is kept, never from today's catalog (Task 3 and
/// Task 8).
#[test]
fn catalog_support_matches_the_task0_snapshot() {
    let kept = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join("catalog.json");
    let listed: Vec<String> = if std::env::var_os(CAPTURE).is_some() {
        specs().iter().map(|spec| spec.id.to_owned()).collect()
    } else {
        let snapshot: Value = serde_json::from_str(
            &std::fs::read_to_string(&kept)
                .unwrap_or_else(|_| panic!("no snapshot at {}", kept.display())),
        )
        .unwrap();
        snapshot["empty"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(listed.len(), 18, "the Task 0 catalog lists 18 harnesses");
    let mut answers = Map::new();
    for id in listed.iter().map(String::as_str).chain(GONE).chain(ODD_IDS) {
        answers.insert(id.to_owned(), Value::String(format!("{:?}", support(id))));
    }
    check("support", &Value::Object(answers));
}

const SOURCE_FILES_MAX: usize = 4096;
const SOURCE_BYTES_MAX: usize = 4 * 1024 * 1024;
const NAMES_COUNT: usize = 24;

/// The engine source places that may compare a harness id to a literal: the registry and
/// the manifest reader, the driver and record dispatch, and the code of the three native
/// drivers (Codex, Claude Code and RCLI: their adapters, managed sign-in, native profiles
/// and the RCLI host). Each is a path prefix under `src/`.
const NATIVE_PLACES: [&str; 15] = [
    "harnesses/registry.rs",
    "harnesses/manifest.rs",
    "harnesses/drivers",
    "harnesses/records",
    "adapters/",
    "rcli_host/",
    "engine/native_auth_host",
    "engine/owner/native_auth",
    "engine/owner/transport/native_auth_channel.rs",
    "engine/owner/transport/account_profile.rs",
    "engine/owner/transport/master_rcli",
    "engine/owner/account_profile.rs",
    "engine/bootstrap/native_profiles/",
    "storage/native_auth.rs",
    "tui/native_auth.rs",
];

/// Every other engine source file that still compares a harness id to a literal, with how
/// many lines do and why. The task named moves the lines to the registry; a change in any
/// count fails the row, so a new comparison is seen and a moved one leaves this list.
const LISTED: [(&str, usize, &str); 23] = [
    (
        "accounts.rs",
        2,
        "closed sign-in list: a Codex or Claude sign-in names its provider (Task 17)",
    ),
    ("config.rs", 1, "RCLI driver: its HTTP profile (Task 20)"),
    (
        "demo.rs",
        1,
        "RCLI driver: a demo pool of RCLI accounts only runs no live agents",
    ),
    (
        "harnesses.rs",
        1,
        "Claude Code base URL without /v1 for the key relay (Task 17)",
    ),
    (
        "harnesses/finding.rs",
        1,
        "closed list: the two harnesses with a managed sign-in whose file the owner chooses",
    ),
    (
        "harnesses/update.rs",
        1,
        "RCLI comes with CrazyAgent, so it is not updated as an installed harness",
    ),
    ("master_session.rs", 3, "native masters (Task 20)"),
    ("master_sessions.rs", 3, "native master names (Task 20)"),
    (
        "model_gateway.rs",
        2,
        "closed sign-in list: a managed Codex or Claude sign-in through the gateway",
    ),
    (
        "pools.rs",
        2,
        "closed sign-in list: a managed sign-in's account kind (Task 17)",
    ),
    (
        "project_setup.rs",
        3,
        "native masters a new project's lead can be, Codex by default (Task 20)",
    ),
    (
        "team/history.rs",
        5,
        "native record folders and lines of Codex and Claude Code (Tasks 14 and 25)",
    ),
    (
        "team/runtime/managed_agent.rs",
        2,
        "closed sign-in list: the account shape of a managed Codex or Claude sign-in",
    ),
    (
        "tools.rs",
        2,
        "install hints for the Codex and Claude programs the managed sign-in needs (Task 18)",
    ),
    ("tui/master_session.rs", 3, "native masters (Task 20)"),
    (
        "tui/model_access.rs",
        1,
        "RCLI driver: its HTTP profile (Task 20)",
    ),
    (
        "tui/screens/harness_setup.rs",
        1,
        "first run's Harnesses step: RCLI comes with CrazyAgent and stays picked",
    ),
    (
        "tui/screens/sessions.rs",
        3,
        "native master names (Task 20)",
    ),
    (
        "tui/screens/settings_add_account.rs",
        1,
        "closed sign-in list: the title of a managed Codex sign-in",
    ),
    (
        "tui/screens/settings_harnesses.rs",
        1,
        "the program name the owner looks for, claude for Claude Code (Task 2 left it)",
    ),
    (
        "usage.rs",
        2,
        "native usage readers of Codex and Claude Code (Task 19)",
    ),
    (
        "usage/recount.rs",
        2,
        "native usage formats of Codex and Claude Code (Task 19)",
    ),
    (
        "usage/report.rs",
        1,
        "RCLI reports usage from its host's frames (Task 19)",
    ),
];

/// The names a harness id literal can take: the catalog ids, their aliases and the ids of
/// the five removed harnesses. Takes nothing. Output: the names.
fn harness_names() -> Vec<String> {
    let mut names: Vec<String> = specs().iter().map(|spec| spec.id.to_owned()).collect();
    for entry in catalog(&Search::new(Vec::new(), None, Vec::new())) {
        names.extend(entry.aliases);
    }
    names.extend(GONE.iter().map(|id| (*id).to_owned()));
    assert_eq!(names.len(), NAMES_COUNT);
    names
}

/// Whether a character can be part of a Rust name. Input: the character. Output: the answer.
fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether an attribute line keeps the item after it to test builds: a `cfg` that names
/// `test` as a word and not `not(test`. Input: the trimmed line. Output: the answer.
fn test_only(line: &str) -> bool {
    let Some(inner) = line.strip_prefix("#[cfg(") else {
        return false;
    };
    !inner.contains("not(test")
        && inner.match_indices("test").any(|(at, _)| {
            !inner[..at].chars().next_back().is_some_and(name_char)
                && !inner[at + 4..].chars().next().is_some_and(name_char)
        })
}

/// The lines of a source file that build outside tests, each with its number: an item under
/// a test-only `cfg` is left out, a one-line item to its `;` and any other to the brace that
/// closes it. Input: the file text. Output: the kept lines.
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut kept = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if !test_only(lines[at].trim()) {
            kept.push((at + 1, lines[at]));
            at += 1;
            continue;
        }
        let mut item = at + 1;
        while item < lines.len() && lines[item].trim().starts_with("#[") {
            item += 1;
        }
        if item < lines.len() && lines[item].trim_end().ends_with(';') && !lines[item].contains('{')
        {
            at = item + 1;
            continue;
        }
        let (mut depth, mut opened, mut end) = (0_i64, false, item);
        while end < lines.len() {
            let line = lines[end];
            depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
            opened |= line.contains('{');
            end += 1;
            if opened && depth <= 0 {
                break;
            }
        }
        assert!(end > at);
        at = end;
    }
    kept
}

/// What follows the name of a constant or static a trimmed line declares, such as
/// `&str = "rcli";` for `const RCLI: &str = "rcli";`. Input: the line. Output: the rest,
/// or None when the line declares no upper-case constant or static.
fn declared(line: &str) -> Option<&str> {
    let visible = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|word| line.strip_prefix(*word))
        .unwrap_or(line);
    let rest = visible
        .strip_prefix("const ")
        .or_else(|| visible.strip_prefix("static "))?;
    let (name, kind) = rest.split_once(": ")?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    .then_some(kind)
}

/// Whether a trimmed line opens a constant or static list of strings over several lines,
/// such as `pub const NAMES: [&str; 3] = [`. Input: the line. Output: the answer.
fn list_opens(line: &str) -> bool {
    declared(line).is_some_and(|kind| {
        (kind.starts_with("[&str; ") || kind.starts_with("[&'static str; "))
            && kind.ends_with("] = [")
    })
}

/// Whether a trimmed line binds a quoted name to a constant or static string, such as
/// `const RCLI: &str = "rcli";`, which later lines compare by that name. Inputs: the line
/// and the quoted name. Output: the answer.
fn binds(line: &str, quoted: &str) -> bool {
    declared(line).is_some_and(|kind| {
        [
            format!("&str = {quoted};"),
            format!("&'static str = {quoted};"),
        ]
        .iter()
        .any(|bound| kind == bound)
    })
}

/// Whether a trimmed line compares a harness name to a literal: the quoted name next to
/// `==`, `!=`, `=>`, `|`, `[` or `Some(`, inside `matches!(`, in a match pattern such as
/// `("codex", ..) =>` with no `=>` before it, bound to a constant that `binds`, or alone on
/// a line of a list that `list_opens`. A name passed to a call, held in a field or a table,
/// given back by a match arm, or in a pattern over several lines is not seen. Inputs: the
/// line, the names and whether it sits in such a list. Output: the answer.
fn compares(line: &str, names: &[String], in_list: bool) -> bool {
    if line.starts_with("//") {
        return false;
    }
    names.iter().any(|name| {
        let quoted = format!("\"{name}\"");
        binds(line, &quoted)
            || line.match_indices(quoted.as_str()).any(|(at, _)| {
                let before = line[..at].trim_end();
                let after = line[at + quoted.len()..].trim_start();
                ["==", "!=", "|", "[", "Some("]
                    .iter()
                    .any(|end| before.ends_with(*end))
                    || ["==", "!=", "=>", "|"]
                        .iter()
                        .any(|start| after.starts_with(*start))
                    || before.contains("matches!(")
                    || (after.contains("=>") && !before.contains("=>"))
                    || (in_list
                        && (line == quoted || line.strip_suffix(',') == Some(quoted.as_str())))
            })
    })
}

/// The lines of one source file that compare a harness name to a literal. Inputs: the file
/// text and the names. Output: each line's number and trimmed text.
fn comparisons(text: &str, names: &[String]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut in_list = false;
    for (number, line) in code_lines(text) {
        let line = line.trim();
        if list_opens(line) {
            in_list = true;
            continue;
        }
        if in_list && line.starts_with("];") {
            in_list = false;
            continue;
        }
        if compares(line, names, in_list) {
            found.push((number, line.to_owned()));
        }
    }
    found
}

/// The engine's Rust source files outside tests, by path under `src/`: no `tests.rs`, no
/// `*_tests.rs` and nothing in a `tests` folder. Input: the `src` folder. Output: the paths
/// in order.
fn engine_sources(root: &Path) -> Vec<String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            assert!(files.len() + pending.len() <= SOURCE_FILES_MAX);
        }
    }
    files.retain(|file| {
        let (folders, name) = file.rsplit_once('/').unwrap_or(("", file.as_str()));
        name != "tests.rs"
            && !name.ends_with("_tests.rs")
            && !folders.split('/').any(|folder| folder == "tests")
    });
    files.sort();
    files
}

/// Outside the registry, the manifests and the three native drivers, no engine source line
/// outside tests compares a harness id to a literal, except the lines `LISTED` counts with
/// the reason and the task that moves them; every native place still exists (Task 8).
#[test]
fn only_listed_engine_lines_compare_a_harness_id_to_a_literal() {
    let names = harness_names();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = engine_sources(&root);
    for place in NATIVE_PLACES {
        assert!(
            files.iter().any(|file| file.starts_with(place)),
            "{place} names no engine source file"
        );
    }
    let expected: BTreeMap<&str, usize> = LISTED
        .iter()
        .map(|(file, count, _)| (*file, *count))
        .collect();
    assert_eq!(expected.len(), LISTED.len(), "a file is listed twice");
    assert!(
        LISTED
            .iter()
            .all(|(_, count, reason)| *count > 0 && !reason.is_empty())
    );
    let mut found: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| !NATIVE_PLACES.iter().any(|place| file.starts_with(*place)))
    {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        assert!(
            text.len() <= SOURCE_BYTES_MAX,
            "{file} is too large to scan"
        );
        let lines = comparisons(&text, &names);
        if !lines.is_empty() {
            found.insert(file.clone(), lines);
        }
    }
    let counted: BTreeMap<&str, usize> = found
        .iter()
        .map(|(file, lines)| (file.as_str(), lines.len()))
        .collect();
    let named: BTreeSet<&str> = expected.keys().chain(counted.keys()).copied().collect();
    let changed: Vec<String> = named
        .into_iter()
        .filter(|file| expected.get(file) != counted.get(file))
        .map(|file| {
            format!(
                "{file}: listed {}, found {:?}",
                expected.get(file).copied().unwrap_or(0),
                found.get(file).cloned().unwrap_or_default()
            )
        })
        .collect();
    assert!(
        changed.is_empty(),
        "harness id comparisons differ from LISTED:\n{}",
        changed.join("\n")
    );
}
