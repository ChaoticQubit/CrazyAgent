use super::Scenario;
use crazyagent::engine::owner::{OwnerResponse, OwnerResult};
use crazyagent::harnesses::interactive::{self, InteractiveCommand, Run, Support, Transcript};
use crazyagent::harnesses::launch::{
    AccountBinding, GitIdentity, LaunchError, LaunchLimits, LaunchRequest, Secret, command, route,
};
use crazyagent::model_gateway::relay::Relay;
use crazyagent::protocol::ProjectId;
use crazyagent::sandbox::policy::{
    AgentAccess, AgentPolicy, PolicyView, ToolchainChoice, ToolchainView,
};
use crazyagent::sandbox::toolchains::{ToolHomes, Toolchain};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The harness every launch scenario starts: a native system program the resolver accepts,
/// so a capture names no installed harness and reads the same on any Mac.
const EXECUTABLE: &str = "/usr/bin/true";

/// The model every launch scenario asks for.
const MODEL: &str = "snapshot-model";

/// A key that is no real provider's and starts with no token prefix the golden writer
/// refuses; only its relay placeholder or key file reaches a launch.
const DUMMY_KEY: &str = "snapshot-dummy-provider-key-0001";

/// The session id every interactive scenario records under, in the UUID form Claude Code
/// requires, so a capture never depends on a random id.
const SESSION: &str = "4f7e2a91-3c5d-4b6e-8a1f-2d3c4b5a6e7f";

/// The most entries one home listing reads before it stops with an error.
const LISTED_MAX: usize = 4096;

/// The deepest folder one home listing reads.
const LISTED_DEPTH_MAX: usize = 16;

/// The URL a keyless endpoint account points at: no loopback address, whose port the runner
/// masks, and a host that never resolves; no launch here dials it.
const ENDPOINT_URL: &str = "https://models.example.invalid:8443/v1";

/// Lists the harness scenarios: the static catalog, the headless launch spec and the terminal
/// pane spec of every launch adapter, and their refusals. Output: the scenarios in order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "harness",
        name,
        capture,
    };
    vec![
        scenario("catalog-specs", catalog_specs),
        scenario("interactive-support", interactive_support),
        scenario(
            "headless-codex-subscription-tools-off",
            headless_codex_subscription_tools_off,
        ),
        scenario(
            "headless-codex-subscription-tools-on",
            headless_codex_subscription_tools_on,
        ),
        scenario("headless-codex-url-tools-off", headless_codex_url_tools_off),
        scenario(
            "headless-codex-routed-key-tools-off",
            headless_codex_routed_key_tools_off,
        ),
        scenario("headless-codex-git-identity", headless_codex_git_identity),
        scenario("headless-codex-full-access", headless_codex_full_access),
        scenario(
            "headless-claude-code-subscription-tools-off",
            headless_claude_code_subscription_tools_off,
        ),
        scenario(
            "headless-claude-code-subscription-tools-on",
            headless_claude_code_subscription_tools_on,
        ),
        scenario(
            "headless-claude-code-url-tools-off",
            headless_claude_code_url_tools_off,
        ),
        scenario(
            "headless-claude-code-routed-key-tools-off",
            headless_claude_code_routed_key_tools_off,
        ),
        scenario(
            "headless-claude-code-git-identity",
            headless_claude_code_git_identity,
        ),
        scenario(
            "headless-claude-code-full-access",
            headless_claude_code_full_access,
        ),
        scenario("headless-refusals", headless_refusals),
        scenario("interactive-codex-fresh", interactive_codex_fresh),
        scenario("interactive-codex-resume", interactive_codex_resume),
        scenario("interactive-codex-tools-on", interactive_codex_tools_on),
        scenario("interactive-codex-keyed", interactive_codex_keyed),
        scenario(
            "interactive-claude-code-fresh",
            interactive_claude_code_fresh,
        ),
        scenario(
            "interactive-claude-code-resume",
            interactive_claude_code_resume,
        ),
        scenario(
            "interactive-claude-code-keyed",
            interactive_claude_code_keyed,
        ),
        scenario(
            "interactive-claude-code-full-access",
            interactive_claude_code_full_access,
        ),
        scenario("interactive-plain-model-flag", interactive_plain_model_flag),
        scenario("interactive-plain-subcommand", interactive_plain_subcommand),
        scenario("interactive-refusals", interactive_refusals),
        scenario("agent-policy-sandboxed", agent_policy_sandboxed),
        scenario("agent-policy-own-branch", agent_policy_own_branch),
        scenario("agent-policy-full", agent_policy_full),
    ]
}

/// The account a launch scenario runs as.
#[derive(Clone, Copy)]
enum Account {
    Subscription,
    Url,
    RoutedKey,
}

/// What a launch scenario changes from a confined, tools-off launch with no git identity.
#[derive(Clone, Copy)]
struct Shape {
    account: Account,
    tools: bool,
    identity: bool,
    full_access: bool,
}

const CONFINED: Shape = Shape {
    account: Account::Subscription,
    tools: false,
    identity: false,
    full_access: false,
};

/// The folders one launch scenario runs in, all below the scratch folder: the account home
/// (0700, as a launch requires), the working folder and the fixture sandbox profile in it.
struct Folders {
    home: PathBuf,
    work: PathBuf,
    profile: PathBuf,
}

/// Makes the scenario's folders. Input: the scratch folder. Output: the folders, by their
/// real paths, or why one could not be made.
fn folders(scratch: &Path) -> Result<Folders, String> {
    let root = scratch.canonicalize().map_err(|error| error.to_string())?;
    let home = root.join("home");
    let work = root.join("work");
    for folder in [&home, &work] {
        std::fs::create_dir(folder).map_err(|error| error.to_string())?;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let profile = work.join("project.sb");
    std::fs::write(&profile, "(version 1)\n(allow default)\n")
        .map_err(|error| error.to_string())?;
    assert!(home.is_absolute() && work.is_absolute());
    Ok(Folders {
        home,
        work,
        profile,
    })
}

/// A launch error as the code it carries. Input: the error. Output: the code.
fn code(error: LaunchError) -> String {
    error.0
}

/// Wraps the dummy key. Output: the secret, or why it was refused.
fn dummy_key() -> Result<Secret, String> {
    Secret::new(DUMMY_KEY.to_owned()).map_err(code)
}

/// Builds the launch request one shape asks for, before any relay. Inputs: the harness, the
/// folders and the shape. Output: the request, or why its key or identity was refused.
fn request(harness: &str, folders: &Folders, shape: Shape) -> Result<LaunchRequest, String> {
    let home = folders.home.clone();
    let account = match shape.account {
        Account::Subscription => AccountBinding::Subscription { home },
        Account::Url => AccountBinding::Url {
            home,
            base_url: ENDPOINT_URL.to_owned(),
            key: None,
        },
        Account::RoutedKey => AccountBinding::ApiKey {
            home,
            key: dummy_key()?,
            base_url: None,
        },
    };
    let git_identity = if shape.identity {
        Some(
            GitIdentity::new(
                "Snapshot Owner".to_owned(),
                "owner@example.invalid".to_owned(),
            )
            .map_err(code)?,
        )
    } else {
        None
    };
    Ok(LaunchRequest {
        harness: harness.to_owned(),
        executable: PathBuf::from(EXECUTABLE),
        working_dir: folders.work.clone(),
        account,
        model: Some(MODEL.to_owned()),
        tools: shape.tools,
        sandbox_profile: (!shape.full_access).then(|| folders.profile.clone()),
        unconfined: shape.full_access,
        git_identity,
        limits: LaunchLimits::default(),
    })
}

/// Text of a path or argument, refusing one that is not UTF-8. Input: the OS text.
/// Output: the text.
fn utf8(text: &std::ffi::OsStr) -> Result<String, String> {
    text.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "a launch word is not UTF-8".to_owned())
}

/// Lists every file and folder below an account home, relative and sorted, folders with a
/// trailing `/`, so a capture shows what the launch wrote there and never what it holds.
/// Input: the home. Output: the names, or why the listing stopped.
pub(super) fn home_files(home: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut pending = vec![(home.to_path_buf(), 0_usize)];
    while let Some((folder, depth)) = pending.pop() {
        if depth > LISTED_DEPTH_MAX {
            return Err("an account home is deeper than a listing reads".to_owned());
        }
        for entry in std::fs::read_dir(&folder).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            let relative = path.strip_prefix(home).map_err(|error| error.to_string())?;
            let mut name = utf8(relative.as_os_str())?;
            let kind = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if kind.is_dir() {
                name.push('/');
                pending.push((path, depth + 1));
            }
            found.push(name);
            if found.len() > LISTED_MAX {
                return Err("an account home holds more entries than a listing reads".to_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The headless launch spec of one built command: program, arguments, variable names
/// without `SDKROOT` and whether it came as the engine gives it on this Mac, working folder
/// and the files below the account home. Inputs: the command, the home and whether tools
/// are on, the only headless runs given toolchain variables. Output: the surface as JSON.
fn headless_spec(built: &Command, home: &Path, tools: bool) -> Result<Value, String> {
    let argv = built
        .get_args()
        .map(utf8)
        .collect::<Result<Vec<String>, String>>()?;
    let mut env_names = built
        .get_envs()
        .map(|(name, _)| utf8(name))
        .collect::<Result<Vec<String>, String>>()?;
    env_names.sort();
    let sdkroot = super::launch::sdkroot_fact(&mut env_names, tools)?;
    let cwd = built
        .get_current_dir()
        .map(|dir| utf8(dir.as_os_str()))
        .transpose()?;
    Ok(json!({
        "program": utf8(built.get_program())?,
        "argv": argv,
        "env_names": env_names,
        "sdkroot": sdkroot,
        "cwd": cwd,
        "home_files": home_files(home)?,
    }))
}

/// Stable facts about what a launch hands over, since the runner masks the relay's loopback
/// port and the capture keeps variable names only: whether an argument or variable names the
/// relay's own address (none when no relay runs), and whether the dummy key is absent from
/// every argument, variable and the key file. Inputs: the arguments and variable values, the
/// relay and the account home. Output: the facts.
fn handover(words: &[String], relay: Option<&Relay>, home: &Path) -> Value {
    let names_the_relay = relay.map(|relay| {
        let address = format!("127.0.0.1:{}", relay.port());
        words.iter().any(|word| word.contains(&address))
    });
    let key_file = std::fs::read_to_string(home.join(interactive::KEY_FILE)).unwrap_or_default();
    let key_absent =
        !key_file.contains(DUMMY_KEY) && !words.iter().any(|word| word.contains(DUMMY_KEY));
    json!({"names_the_relay": names_the_relay, "key_absent": key_absent})
}

/// Captures the headless launch spec of one harness and shape, routing a keyed account
/// through the key relay first, as the engine does, with the hand-over facts. Inputs: the
/// scratch folder, the harness and the shape. Output: the surface as JSON.
fn headless(scratch: &Path, harness: &str, shape: Shape) -> Result<Value, String> {
    let folders = folders(scratch)?;
    let (routed, relay) = route(request(harness, &folders, shape)?).map_err(code)?;
    assert_eq!(relay.is_some(), matches!(shape.account, Account::RoutedKey));
    let built = command(&routed).map_err(code)?;
    let mut spec = headless_spec(&built, &folders.home, shape.tools)?;
    let words: Vec<String> = built
        .get_args()
        .chain(built.get_envs().filter_map(|(_, value)| value))
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    spec["handover"] = handover(&words, relay.as_ref(), &folders.home);
    drop(relay);
    Ok(spec)
}

fn headless_codex_subscription_tools_off(scratch: &Path) -> Result<Value, String> {
    headless(scratch, "codex", CONFINED)
}

fn headless_codex_subscription_tools_on(scratch: &Path) -> Result<Value, String> {
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    headless(scratch, "codex", tools)
}

fn headless_codex_url_tools_off(scratch: &Path) -> Result<Value, String> {
    let url = Shape {
        account: Account::Url,
        ..CONFINED
    };
    headless(scratch, "codex", url)
}

fn headless_codex_routed_key_tools_off(scratch: &Path) -> Result<Value, String> {
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    headless(scratch, "codex", keyed)
}

fn headless_codex_git_identity(scratch: &Path) -> Result<Value, String> {
    let identity = Shape {
        identity: true,
        ..CONFINED
    };
    headless(scratch, "codex", identity)
}

fn headless_codex_full_access(scratch: &Path) -> Result<Value, String> {
    let full = Shape {
        full_access: true,
        ..CONFINED
    };
    headless(scratch, "codex", full)
}

fn headless_claude_code_subscription_tools_off(scratch: &Path) -> Result<Value, String> {
    headless(scratch, "claude-code", CONFINED)
}

fn headless_claude_code_subscription_tools_on(scratch: &Path) -> Result<Value, String> {
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    headless(scratch, "claude-code", tools)
}

fn headless_claude_code_url_tools_off(scratch: &Path) -> Result<Value, String> {
    let url = Shape {
        account: Account::Url,
        ..CONFINED
    };
    headless(scratch, "claude-code", url)
}

fn headless_claude_code_routed_key_tools_off(scratch: &Path) -> Result<Value, String> {
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    headless(scratch, "claude-code", keyed)
}

fn headless_claude_code_git_identity(scratch: &Path) -> Result<Value, String> {
    let identity = Shape {
        identity: true,
        ..CONFINED
    };
    headless(scratch, "claude-code", identity)
}

fn headless_claude_code_full_access(scratch: &Path) -> Result<Value, String> {
    let full = Shape {
        full_access: true,
        ..CONFINED
    };
    headless(scratch, "claude-code", full)
}

/// The refusal code one headless case gets, or `built` when the command builds.
/// Input: the case's result. Output: the code.
fn outcome<T>(result: Result<T, LaunchError>) -> String {
    match result {
        Ok(_) => "built".to_owned(),
        Err(error) => code(error),
    }
}

/// Captures the code each unsafe or unsupported headless launch is refused with: a harness
/// with no headless adapter, no profile while confined, a profile while unconfined, a key
/// that is not a relay placeholder, a relay for a harness the relay does not carry, a bad
/// model, a home others can read and a URL with a query. Input: the scratch folder.
/// Output: the codes by case.
fn headless_refusals(scratch: &Path) -> Result<Value, String> {
    let folders = folders(scratch)?;
    let mut cases = serde_json::Map::new();
    let mut note = |case: &str, value: String| {
        cases.insert(case.to_owned(), Value::String(value));
    };
    note(
        "harness-without-headless-adapter",
        outcome(command(&request("gemini-cli", &folders, CONFINED)?)),
    );
    let mut bare = request("codex", &folders, CONFINED)?;
    bare.sandbox_profile = None;
    note("confined-without-profile", outcome(command(&bare)));
    let mut both = request("codex", &folders, CONFINED)?;
    both.unconfined = true;
    note("profile-and-unconfined", outcome(command(&both)));
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    note(
        "key-not-routed",
        outcome(command(&request("codex", &folders, keyed)?)),
    );
    note(
        "relay-for-harness-without-adapter",
        outcome(route(request("not-a-harness", &folders, keyed)?)),
    );
    let mut model = request("codex", &folders, CONFINED)?;
    model.model = Some("-starts-with-a-dash".to_owned());
    note("model-invalid", outcome(command(&model)));
    let mut query = request("codex", &folders, CONFINED)?;
    query.account = AccountBinding::Url {
        home: folders.home.clone(),
        base_url: format!("{ENDPOINT_URL}?region=1"),
        key: None,
    };
    note("url-with-query", outcome(command(&query)));
    std::fs::set_permissions(&folders.home, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| error.to_string())?;
    note(
        "home-others-can-read",
        outcome(command(&request("codex", &folders, CONFINED)?)),
    );
    Ok(Value::Object(cases))
}

/// The pane spec of one built interactive command: argv, working folder, variable names in
/// the order the pane gets them without `SDKROOT` and whether it came as the engine gives it, where the harness records its session (by kind, as a place
/// below the account home) and the files below the home. A manifest harness's record, which
/// no 2.4.2 harness has, reads as its own kind with its harness and record kind, so a 2.5
/// harness that gets one shows as a golden difference, not a failed capture. Inputs: the
/// command and the home. Output: the surface as JSON.
fn pane_spec(built: &InteractiveCommand, home: &Path) -> Result<Value, String> {
    let mut env_names: Vec<String> = built.env.iter().map(|(name, _)| name.clone()).collect();
    let sdkroot = super::launch::sdkroot_fact(&mut env_names, true)?;
    let transcript = match &built.transcript {
        Transcript::CodexRollouts { sessions } => json!({
            "kind": "codex_rollouts",
            "sessions": utf8(sessions.as_os_str())?,
        }),
        Transcript::ClaudeSession { file } => {
            let folder = interactive::claude_project_folder(&built.cwd)
                .ok_or_else(|| "a pane's folder has no Claude project name".to_owned())?;
            json!({
                "kind": "claude_session",
                "in_projects_folder_named_for_cwd": file
                    == &home.join("projects").join(folder).join(format!("{SESSION}.jsonl")),
            })
        }
        Transcript::Records(records) => json!({
            "kind": "records",
            "harness": records.harness,
            "record_kind": format!("{:?}", records.kind),
        }),
        Transcript::Unavailable => json!({"kind": "unavailable"}),
    };
    Ok(json!({
        "argv": built.argv,
        "cwd": utf8(built.cwd.as_os_str())?,
        "env_names": env_names,
        "sdkroot": sdkroot,
        "transcript": transcript,
        "home_files": home_files(home)?,
    }))
}

/// Captures the pane spec of one harness and shape with no installed project context, with
/// the hand-over facts: a keyed account is routed and its placeholder written to the key file
/// the run names, as the engine does. Inputs: the scratch folder, the harness, the shape and whether the run
/// resumes. Output: the surface as JSON.
fn pane(scratch: &Path, harness: &str, shape: Shape, resume: bool) -> Result<Value, String> {
    let folders = folders(scratch)?;
    let (routed, relay) = route(request(harness, &folders, shape)?).map_err(code)?;
    let key_file = match &routed.account {
        AccountBinding::Url { key: Some(key), .. } | AccountBinding::ApiKey { key, .. } => {
            Some(interactive::write_key(&folders.home, key).map_err(code)?)
        }
        AccountBinding::Url { key: None, .. } | AccountBinding::Subscription { .. } => None,
    };
    let run = Run {
        session_id: SESSION,
        key_file: key_file.as_deref(),
        resume,
    };
    let built = interactive::command(&routed, None, &run).map_err(code)?;
    let mut spec = pane_spec(&built, &folders.home)?;
    let words: Vec<String> = built
        .argv
        .iter()
        .chain(built.env.iter().map(|(_, value)| value))
        .cloned()
        .collect();
    spec["handover"] = handover(&words, relay.as_ref(), &folders.home);
    drop(relay);
    Ok(spec)
}

fn interactive_codex_fresh(scratch: &Path) -> Result<Value, String> {
    pane(scratch, "codex", CONFINED, false)
}

fn interactive_codex_resume(scratch: &Path) -> Result<Value, String> {
    pane(scratch, "codex", CONFINED, true)
}

fn interactive_codex_tools_on(scratch: &Path) -> Result<Value, String> {
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    pane(scratch, "codex", tools, false)
}

fn interactive_codex_keyed(scratch: &Path) -> Result<Value, String> {
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    pane(scratch, "codex", keyed, false)
}

fn interactive_claude_code_fresh(scratch: &Path) -> Result<Value, String> {
    pane(scratch, "claude-code", CONFINED, false)
}

fn interactive_claude_code_resume(scratch: &Path) -> Result<Value, String> {
    pane(scratch, "claude-code", CONFINED, true)
}

fn interactive_claude_code_keyed(scratch: &Path) -> Result<Value, String> {
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    pane(scratch, "claude-code", keyed, false)
}

fn interactive_claude_code_full_access(scratch: &Path) -> Result<Value, String> {
    let full = Shape {
        full_access: true,
        ..CONFINED
    };
    pane(scratch, "claude-code", full, false)
}

/// A harness with no launch adapter that takes a model flag, started with the tools on and a
/// subscription, the one shape such a pane accepts.
fn interactive_plain_model_flag(scratch: &Path) -> Result<Value, String> {
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    pane(scratch, "gemini-cli", tools, false)
}

/// A harness with no launch adapter that starts through a subcommand and takes no model flag,
/// so its request carries no model.
fn interactive_plain_subcommand(scratch: &Path) -> Result<Value, String> {
    let folders = folders(scratch)?;
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    let mut goose = request("goose", &folders, tools)?;
    goose.model = None;
    let run = Run {
        session_id: SESSION,
        key_file: None,
        resume: false,
    };
    let built = interactive::command(&goose, None, &run).map_err(code)?;
    pane_spec(&built, &folders.home)
}

/// Captures the code each refused pane gets: a key with no key file, a plain harness with
/// the tools off, with a URL account or with a model it has no flag for, a resume under an
/// id that is not a UUID and a name outside the catalog. Input: the scratch folder.
/// Output: the codes by case.
fn interactive_refusals(scratch: &Path) -> Result<Value, String> {
    let folders = folders(scratch)?;
    let fresh = Run {
        session_id: SESSION,
        key_file: None,
        resume: false,
    };
    let tools = Shape {
        tools: true,
        ..CONFINED
    };
    let mut cases = serde_json::Map::new();
    let mut note = |case: &str, value: String| {
        cases.insert(case.to_owned(), Value::String(value));
    };
    let keyed = Shape {
        account: Account::RoutedKey,
        ..CONFINED
    };
    let (routed, relay) = route(request("claude-code", &folders, keyed)?).map_err(code)?;
    note(
        "key-without-key-file",
        outcome(interactive::command(&routed, None, &fresh)),
    );
    drop(relay);
    note(
        "plain-tools-off",
        outcome(interactive::command(
            &request("gemini-cli", &folders, CONFINED)?,
            None,
            &fresh,
        )),
    );
    let url = Shape {
        account: Account::Url,
        ..tools
    };
    note(
        "plain-url-account",
        outcome(interactive::command(
            &request("gemini-cli", &folders, url)?,
            None,
            &fresh,
        )),
    );
    note(
        "plain-model-without-flag",
        outcome(interactive::command(
            &request("goose", &folders, tools)?,
            None,
            &fresh,
        )),
    );
    let bad_resume = Run {
        session_id: "not-a-session-id",
        key_file: None,
        resume: true,
    };
    note(
        "resume-id-not-uuid",
        outcome(interactive::command(
            &request("claude-code", &folders, CONFINED)?,
            None,
            &bad_resume,
        )),
    );
    note(
        "harness-outside-catalog",
        outcome(interactive::command(
            &request("not-a-harness", &folders, tools)?,
            None,
            &fresh,
        )),
    );
    Ok(Value::Object(cases))
}

/// Captures the static harness catalog: every spec's id, name, program names, install
/// places, headless and limited-tools notes, model choice, account kinds and launch adapter.
/// Input: the scratch folder, unused. Output: the specs as a list in catalog order.
fn catalog_specs(_scratch: &Path) -> Result<Value, String> {
    let specs: Vec<Value> = crazyagent::harnesses::specs()
        .iter()
        .map(|spec| {
            json!({
                "id": spec.id,
                "name": spec.name,
                "binaries": spec.binaries,
                "locations": spec.locations,
                "headless": spec.headless,
                "tools_limited": spec.tools_limited,
                "model_selection": spec.model_selection,
                "account_kinds": spec.account_kinds,
                "launch": spec.launch,
            })
        })
        .collect();
    assert!(!specs.is_empty());
    Ok(Value::Array(specs))
}

/// Captures whether each catalog harness, the `claude` connection name and one unknown name
/// runs as its own app in a terminal pane. Input: the scratch folder, unused. Output: the
/// answers as a list in catalog order.
fn interactive_support(_scratch: &Path) -> Result<Value, String> {
    let names = crazyagent::harnesses::specs()
        .iter()
        .map(|spec| spec.id)
        .chain(["claude", "not-a-harness"]);
    let answers: Vec<Value> = names
        .map(|name| {
            let (support, reason) = match interactive::support(name) {
                Support::Terminal => ("terminal", None),
                Support::HeadlessOnly { reason } => ("headless_only", Some(reason)),
                Support::Unavailable { reason } => ("unavailable", Some(reason)),
            };
            json!({"name": name, "support": support, "reason": reason})
        })
        .collect();
    Ok(Value::Array(answers))
}

/// The project the policy scenarios set up.
const POLICY_PROJECT: &str = "snapshot-p2-policy";

/// The toolchain choices each policy scenario saves, one per family, so the view shows a row
/// for every family whatever this Mac has installed; mixed, so a lost or flipped choice shows.
const TOOLCHAIN_CHOICES: [ToolchainChoice; 3] = [
    ToolchainChoice {
        toolchain: Toolchain::Rust,
        allowed: true,
    },
    ToolchainChoice {
        toolchain: Toolchain::Node,
        allowed: false,
    },
    ToolchainChoice {
        toolchain: Toolchain::Python,
        allowed: true,
    },
];

/// The policy a save of one view at another access level sends, keeping every other choice
/// the view shows and no toolchain. Inputs: the view and the level. Output: the policy.
fn at_level(view: &PolicyView, access: AgentAccess) -> AgentPolicy {
    AgentPolicy {
        access,
        toolchains: Vec::new(),
        ide_git_hooks: view.ide_git_hooks,
        network: view.network,
        tool_cache: view.tool_cache,
        home_caches: view.home_caches,
        ide_agent_files: view.ide_agent_files,
        apply_changes: view.apply_changes,
        grants: view
            .grants
            .iter()
            .map(|grant| grant.grant.clone())
            .collect(),
        shared_folders: view.shared_folders.clone(),
    }
}

/// The result an owner change was recorded with now, never an earlier change the engine
/// replays for a generation it already saw. Input: the engine's response. Output: the
/// result, or the response when it was not recorded now.
pub(super) fn recorded(response: OwnerResponse) -> Result<OwnerResult, String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }
            if !operation.historical_replay =>
        {
            Ok(operation.result)
        }
        other => Err(format!("the owner change was not recorded now: {other:?}")),
    }
}

/// Reads one project's agent policy as the owner's policy screen does. Inputs: the owner
/// and the project. Output: the view, or the engine's refusal code.
async fn policy_view(
    owner: &mut super::e2e_support::Owner,
    project: &ProjectId,
) -> Result<PolicyView, String> {
    owner
        .controller
        .project_agent_policy(project.clone())
        .await
        .map_err(|error| error.0)
}

/// Saves one project's agent policy at an access level, keeping every other choice the
/// owner's screen shows and no toolchain, as the owner's policy screen saves it; a project
/// already at that level is left as it is. Inputs: the owner, the project and the level.
/// Output: nothing, or why the save was not applied.
pub(super) async fn set_level(
    owner: &mut super::e2e_support::Owner,
    project: &ProjectId,
    access: AgentAccess,
) -> Result<(), String> {
    let before = policy_view(owner, project).await?;
    if before.access == access {
        return Ok(());
    }
    save_policy(owner, project, before.version, at_level(&before, access)).await
}

/// Saves one project's agent policy over the version a view read, as the owner's policy
/// screen saves it. Inputs: the owner, the project, that version and the policy. Output:
/// nothing, or why the save was not applied.
async fn save_policy(
    owner: &mut super::e2e_support::Owner,
    project: &ProjectId,
    version: u64,
    policy: AgentPolicy,
) -> Result<(), String> {
    let generation = owner.next();
    let saved = owner
        .controller
        .set_project_agent_policy(project.clone(), version, policy, generation)
        .await
        .map_err(|error| error.0)?;
    let result = recorded(saved)?;
    if result != OwnerResult::Applied {
        return Err(format!("set_project_agent_policy: {result:?}"));
    }
    Ok(())
}

/// Captures the agent policy a new project's agents get at one access level, saved with
/// [`TOOLCHAIN_CHOICES`], as the owner's policy screen reads it: the level, network, the
/// choices offered, the tool caches, the grants and their log, apply timing, shared and local
/// folders and the temp folder a grant would hand over, with whether that folder is the
/// owner's own private temp folder, since the runner masks it, and the toolchain rows as
/// [`toolchain_rows`] reads them on any Mac. Inputs: the scratch folder and the level.
/// Output: the view as JSON.
fn agent_policy(scratch: &Path, access: AgentAccess) -> Result<Value, String> {
    let projects = scratch
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join("projects");
    std::fs::create_dir(&projects).map_err(|error| error.to_string())?;
    let engine = super::provider::engine(scratch, None)?;
    let view = super::provider::with_owner(&engine, |mut owner| async move {
        let steps = async {
            let home = owner.set_home(&projects).await;
            if home != OwnerResult::Applied {
                return Err(format!("set_home: {home:?}"));
            }
            let created = owner
                .create_project(POLICY_PROJECT, "Snapshot policy")
                .await;
            if created != OwnerResult::Applied {
                return Err(format!("create_project: {created:?}"));
            }
            let project = ProjectId::new(POLICY_PROJECT).map_err(|error| error.to_string())?;
            let before = policy_view(&mut owner, &project).await?;
            let policy = AgentPolicy {
                toolchains: TOOLCHAIN_CHOICES.to_vec(),
                ..at_level(&before, access)
            };
            save_policy(&mut owner, &project, before.version, policy).await?;
            policy_view(&mut owner, &project).await
        };
        let view = steps.await;
        (owner, view)
    })?;
    assert_eq!(view.access, access);
    if view.toolchains.len() != TOOLCHAIN_CHOICES.len() {
        return Err(format!(
            "the policy view shows {} toolchain rows for {} saved choices",
            view.toolchains.len(),
            TOOLCHAIN_CHOICES.len()
        ));
    }
    let homes = ToolHomes::owner().map_err(|error| format!("{error:?}"))?;
    let toolchains = toolchain_rows(&view.toolchains, |toolchain| toolchain.installed(&homes));
    let owner_temp = crazyagent::sandbox::grants::owner_temp_folder().ok();
    let temp_folder_is_the_owners =
        view.temp_folder.path.as_deref() == owner_temp.as_deref().and_then(Path::to_str);
    let mut captured = serde_json::to_value(view).map_err(|error| error.to_string())?;
    let fields = captured
        .as_object_mut()
        .ok_or_else(|| "a policy view is not an object".to_owned())?;
    fields.insert("toolchains".to_owned(), toolchains);
    fields.insert(
        "temp_folder_is_the_owners".to_owned(),
        Value::Bool(temp_folder_is_the_owners),
    );
    Ok(captured)
}

/// The toolchain rows of a policy view as they read on any Mac: each row's family, label,
/// use and choice, and in place of whether this Mac has the family installed, whether the
/// row says so as the engine's own check finds it. Inputs: the rows and that check.
/// Output: the rows as JSON, in view order.
pub(super) fn toolchain_rows(
    rows: &[ToolchainView],
    installed: impl Fn(Toolchain) -> bool,
) -> Value {
    let read: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "toolchain": row.toolchain,
                "label": row.label,
                "used": row.used,
                "allowed": row.allowed,
                "installed_as_this_mac_has_it": row.installed == installed(row.toolchain),
            })
        })
        .collect();
    assert_eq!(read.len(), rows.len());
    Value::Array(read)
}

fn agent_policy_sandboxed(scratch: &Path) -> Result<Value, String> {
    agent_policy(scratch, AgentAccess::Sandboxed)
}

fn agent_policy_own_branch(scratch: &Path) -> Result<Value, String> {
    agent_policy(scratch, AgentAccess::OwnBranch)
}

fn agent_policy_full(scratch: &Path) -> Result<Value, String> {
    agent_policy(scratch, AgentAccess::Full)
}

#[test]
fn toolchain_rows_read_the_same_on_a_mac_with_and_without_the_toolchains() {
    let row = |toolchain: Toolchain, installed: bool, allowed: bool| ToolchainView {
        toolchain,
        label: toolchain.label().to_owned(),
        installed,
        used: false,
        allowed,
    };
    let here = [
        row(Toolchain::Rust, true, true),
        row(Toolchain::Node, true, false),
        row(Toolchain::Python, false, true),
    ];
    let there = [
        row(Toolchain::Rust, false, true),
        row(Toolchain::Node, false, false),
        row(Toolchain::Python, true, true),
    ];
    let read_here = toolchain_rows(&here, |toolchain| toolchain != Toolchain::Python);
    let read_there = toolchain_rows(&there, |toolchain| toolchain == Toolchain::Python);
    assert_eq!(read_here, read_there);
    let fact = |toolchain: Toolchain, allowed: bool| {
        json!({
            "toolchain": toolchain,
            "label": toolchain.label(),
            "used": false,
            "allowed": allowed,
            "installed_as_this_mac_has_it": true,
        })
    };
    assert_eq!(
        read_here,
        json!([
            fact(Toolchain::Rust, true),
            fact(Toolchain::Node, false),
            fact(Toolchain::Python, true),
        ])
    );
    let misread = toolchain_rows(&there, |_| true);
    assert_eq!(misread[0]["installed_as_this_mac_has_it"], json!(false));
}

#[test]
fn a_manifest_record_transcript_reads_as_its_own_kind_no_2_4_2_harness_has() {
    let home = super::e2e_support::scratch("ca-snap-records-");
    let text = include_str!("../fixtures/harness/standin-jsonl-append.yaml");
    let manifest = crazyagent::harnesses::manifest::parse(text.as_bytes()).unwrap();
    let settings = interactive::settings::TerminalSettings::read(&manifest).unwrap();
    let harness = settings.harness().to_owned();
    let records = settings.records(home.path(), "snapshot-session").unwrap();
    let built = InteractiveCommand {
        argv: vec!["standin".to_owned()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
        transcript: Transcript::Records(records),
    };
    let spec = pane_spec(&built, home.path()).unwrap();
    assert_eq!(
        spec["transcript"],
        json!({"kind": "records", "harness": harness, "record_kind": "JsonlAppend"})
    );
}
