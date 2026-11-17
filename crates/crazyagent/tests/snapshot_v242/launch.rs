use super::Scenario;
use super::e2e_support::{Engine, Owner, stop_tmux};
use super::harness::{home_files, recorded, set_level};
use super::provider::{Recorder, as_owner, engine, with_owner};
use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerData, OwnerMutation, OwnerRead, OwnerResult,
};
use crazyagent::project_setup::{HarnessSlot, ProjectSetup, TeamPlan};
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec};
use crazyagent::sandbox::policy::AgentAccess;
use crazyagent::sandbox::toolchains::{self, ToolHomes};
use crazyagent::team::{AgentRole, TeamAction, TeamStatus};
use crazyagent::terminal::FOLDER;
use crazyagent::terminal::tmux::{SOCKET, installed};
use serde_json::{Value, json};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Lists the engine launch scenarios: one Codex sub agent, tools off, delegated by the owner
/// at each access level, captured as the engine itself launched it, and the pool list of
/// the team those scenarios seed.
pub(super) fn scenarios() -> Vec<Scenario> {
    let scenario = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "harness",
        name,
        capture,
    };
    vec![
        scenario("engine-codex-sub-sandboxed", engine_codex_sub_sandboxed),
        scenario("engine-codex-sub-own-branch", engine_codex_sub_own_branch),
        scenario("engine-codex-sub-full-access", engine_codex_sub_full_access),
        Scenario {
            kind: "provider",
            name: "pool-summaries",
            capture: pool_summaries,
        },
    ]
}

/// Captures the pool list the owner's pools screen reads for a pool of one keyless
/// OpenAI-compatible account assigned to one project. Input: the scratch folder. Output:
/// the summaries.
fn pool_summaries(scratch: &Path) -> Result<Value, String> {
    let team = Team::start(scratch, AgentAccess::Sandboxed, false)?;
    let pools = as_owner(&team.engine, |mut owner| async move {
        let pools = owner
            .controller
            .pool_summaries()
            .await
            .map_err(|error| error.0);
        (owner, pools)
    })?;
    serde_json::to_value(pools).map_err(|error| error.to_string())
}

/// Captures the launch of the Codex sub agent the owner delegates one task to, tools off,
/// at one access level. Inputs: the scratch folder and the level. Output: the capture.
fn engine_codex_sub(scratch: &Path, access: AgentAccess) -> Result<Value, String> {
    let team = Team::start(scratch, access, false)?;
    team.act(TeamAction::Delegate {
        project_id: team.project.clone(),
        harness: Some(HARNESS.to_owned()),
        prompt: "Reply with the single word done.".to_owned(),
    })?;
    team.launched(AgentRole::Sub)
}

fn engine_codex_sub_sandboxed(scratch: &Path) -> Result<Value, String> {
    engine_codex_sub(scratch, AgentAccess::Sandboxed)
}

fn engine_codex_sub_own_branch(scratch: &Path) -> Result<Value, String> {
    engine_codex_sub(scratch, AgentAccess::OwnBranch)
}

fn engine_codex_sub_full_access(scratch: &Path) -> Result<Value, String> {
    engine_codex_sub(scratch, AgentAccess::Full)
}

/// The project every engine launch scenario sets up.
const PROJECT: &str = "snapshot-p2-team";

/// The pool that hands the project its one account.
const POOL: &str = "snapshot-p2-pool";

/// The model the project's account names.
const MODEL: &str = "snapshot-model";

/// The harness every launch here runs; its stand-in answers in its place.
const HARNESS: &str = "codex";

/// What the stand-in's folder reads as in every capture: the one literal replaced.
const HARNESS_TOKEN: &str = "<harness>";

/// The folder the stand-in leaves its record in, next to the agent's own home.
const RECORD: &str = "snapshot-v242-launch";

/// How long one launch may take to leave its record.
const LAUNCH_WAIT: Duration = Duration::from_secs(60);

/// How often the team status is read while a launch is awaited.
const LAUNCH_POLL: Duration = Duration::from_millis(200);

/// The largest profile or record file one capture reads.
const READ_BYTES_MAX: u64 = 1024 * 1024;

/// The variable the engine gives an agent only when this Mac has developer tools with a
/// macOS SDK (toolchains.rs `agent_env`).
const SDKROOT: &str = "SDKROOT";

/// The rule kinds that name one path on a line of their own in a Seatbelt profile.
const PATH_RULES: [&str; 3] = ["subpath", "literal", "path-ancestors"];

/// The Codex stand-in: it answers a version probe, and on a launch records its arguments
/// (NUL separated) and its variable names, never their values, in [`RECORD`] beside its
/// home, moved into place whole, then reads its input until the pane closes.
const STAND_IN: &str = r#"#!/bin/sh
case "$1" in
  --version) echo 'codex-cli 0.167.1'; exit 0 ;;
esac
r="${CODEX_HOME:-$HOME}/../tmp/snapshot-v242-launch"
/bin/mkdir -p "$r.part" || exit 1
for a in "$@"; do printf '%s\000' "$a"; done > "$r.part/argv"
/usr/bin/awk 'BEGIN { for (name in ENVIRON) print name }' > "$r.part/env"
/bin/mv "$r.part" "$r" || exit 1
while IFS= read -r line; do :; done
"#;

/// Writes the Codex stand-in at its one fixed place below cargo's per-target temp folder:
/// outside every temp folder the engine refuses a harness in, and the one literal every
/// capture replaces with [`HARNESS_TOKEN`]. Output: the canonical folder holding `codex`.
pub(super) fn stand_in() -> Result<PathBuf, String> {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join("snapshot-v242");
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let folder = folder.canonicalize().map_err(|error| error.to_string())?;
    let program = folder.join(HARNESS);
    if std::fs::read_to_string(&program).ok().as_deref() != Some(STAND_IN) {
        std::fs::write(&program, STAND_IN).map_err(|error| error.to_string())?;
    }
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    assert!(folder.is_absolute() && program.is_file());
    Ok(folder)
}

/// A real engine started with the Codex stand-in pinned, holding one project with a team
/// plan of Codex agents whose pool has one keyless OpenAI-compatible account on a loopback
/// recorder, all seeded through owner requests. Dropping it ends the engine's tmux server
/// and every pane in it, then the engine.
pub(super) struct Team {
    pub(super) engine: Engine,
    pub(super) project: ProjectId,
    stand_in: String,
    projects: PathBuf,
    tmux: Option<PathBuf>,
    recorder: Recorder,
}

impl Drop for Team {
    fn drop(&mut self) {
        if let Some(tmux) = &self.tmux {
            stop_tmux(tmux, &self.engine.path().join(FOLDER).join(SOCKET));
        }
    }
}

/// Fails unless an owner change was applied. Inputs: what was changed, for the message,
/// and the result. Output: nothing, or the message.
fn applied(what: &str, result: OwnerResult) -> Result<(), String> {
    match result {
        OwnerResult::Applied => Ok(()),
        other => Err(format!("{what}: {other:?}")),
    }
}

/// Creates the pool holding one account for one project, as the owner's pool screen
/// saves it against a fresh read of the pools. Inputs: the owner, the account and the
/// project. Output: nothing, or why the pool was not created.
async fn create_pool(
    owner: &mut Owner,
    member: &CredentialRef,
    project: &ProjectId,
) -> Result<(), String> {
    let page = owner
        .controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        })
        .await
        .map_err(|error| error.0)?;
    let OwnerData::ModelAccessPage { source_hash, .. } = &page.data else {
        return Err("the pools page did not come back".to_owned());
    };
    let reference = |value: &str| CredentialRef::new(value).map_err(|error| error.to_string());
    let pool = AccountPool {
        id: reference(POOL)?,
        name: None,
        members: vec![member.clone()],
        projects: vec![reference(project.as_str())?],
        roles: vec![reference("project_leader")?],
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::default(),
    };
    let generation = owner.next();
    owner
        .controller
        .begin_mutation(
            OwnerMutation::ModelAccess {
                expected_config_id: page.config_id.clone(),
                expected_source_hash: source_hash.clone(),
                edit: ModelAccessEdit::CreatePool { pool },
            },
            generation,
        )
        .map_err(|error| error.0)?;
    let response = owner
        .controller
        .send_pending()
        .await
        .map_err(|error| error.0)?;
    applied("create the pool", recorded(response)?)
}

/// The team plan every launch scenario saves: a Codex master and one Codex agent row.
/// Input: whether the agents run with tools. Output: the plan.
fn plan(tools: bool) -> TeamPlan {
    TeamPlan {
        master: HARNESS.to_owned(),
        master_tools: tools,
        sub_harnesses: vec![HarnessSlot {
            harness: HARNESS.to_owned(),
            agents: 1,
            tools,
        }],
        min_agents: 1,
        max_agents: 2,
    }
}

impl Team {
    /// Starts the engine with the stand-in pinned and seeds the project through owner
    /// requests: the projects home, the account, its pool, the project with its team plan
    /// and its agent access level. Inputs: the scratch folder, the level and whether the
    /// agents run with tools. Output: the team, or the first step that was refused.
    pub(super) fn start(scratch: &Path, access: AgentAccess, tools: bool) -> Result<Self, String> {
        let folder = stand_in()?;
        let stand_in = folder
            .to_str()
            .ok_or_else(|| "the stand-in folder is not UTF-8".to_owned())?
            .to_owned();
        let projects = scratch
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("projects");
        std::fs::create_dir(&projects).map_err(|error| error.to_string())?;
        let projects_home = projects.clone();
        let recorder = Recorder::start(false)?;
        let engine = engine(scratch, Some(("--codex-executable", folder.join(HARNESS))))?;
        let tmux = installed(engine.path());
        let project = ProjectId::new(PROJECT).map_err(|error| error.to_string())?;
        let url = format!("http://127.0.0.1:{}/v1", recorder.port);
        let seeded = project.clone();
        with_owner(&engine, |mut owner| async move {
            let steps = async {
                applied("set the projects home", owner.set_home(&projects).await)?;
                let draft = AccountDraft {
                    provider: Provider::OpenaiCompatible,
                    method: SignInMethod::Url,
                    name: "Snapshot team".to_owned(),
                    url: Some(url),
                    model: Some(MODEL.to_owned()),
                };
                let account = owner
                    .controller
                    .add_account(draft, None)
                    .await
                    .map_err(|error| format!("add_account: {}", error.0))?;
                create_pool(&mut owner, &account.id, &seeded).await?;
                let setup = ProjectSetup {
                    about: "One team launch, captured.".to_owned(),
                    tech_stack: Vec::new(),
                    platforms: Vec::new(),
                    research: Vec::new(),
                    pool: Some(CredentialRef::new(POOL).map_err(|error| error.to_string())?),
                    team: Some(plan(tools)),
                    documents: Vec::new(),
                };
                let spec = ProjectSpec {
                    name: "Snapshot team".to_owned(),
                    repositories: Vec::new(),
                };
                let generation = owner.next();
                let response = owner
                    .controller
                    .setup_project(seeded.clone(), spec, setup, generation)
                    .await
                    .map_err(|error| format!("setup_project: {}", error.0))?;
                applied("set up the project", recorded(response)?)?;
                set_level(&mut owner, &seeded, access).await
            };
            let result = steps.await;
            (owner, result)
        })?;
        Ok(Self {
            engine,
            project,
            stand_in,
            projects: projects_home,
            tmux,
            recorder,
        })
    }

    /// Sends one team action as the owner. Input: the action. Output: nothing, or why it
    /// was not applied.
    pub(super) fn act(&self, action: TeamAction) -> Result<(), String> {
        as_owner(&self.engine, |mut owner| async move {
            let generation = owner.next();
            let result = match owner.controller.team_action(action, generation).await {
                Ok(response) => {
                    recorded(response).and_then(|result| applied("team action", result))
                }
                Err(error) => Err(format!("team action: {}", error.0)),
            };
            (owner, result)
        })
    }

    /// Reads the project's team status as the owner. Output: the status.
    fn status(&self) -> Result<TeamStatus, String> {
        let project = self.project.clone();
        as_owner(&self.engine, |mut owner| async move {
            let status = owner
                .controller
                .team_status(project)
                .await
                .map_err(|error| error.0);
            (owner, status)
        })
    }

    /// The engine's team state folder, where agent homes and profiles live.
    fn state(&self) -> PathBuf {
        self.engine.path().join("team")
    }

    /// Waits for the first agent of one role whose stand-in left its record and captures
    /// that launch. Input: the role. Output: the capture, or the agents seen when none
    /// launched within [`LAUNCH_WAIT`].
    pub(super) fn launched(&self, role: AgentRole) -> Result<Value, String> {
        let start = Instant::now();
        loop {
            let status = self.status()?;
            let found = status
                .agents
                .iter()
                .filter(|agent| agent.role == role)
                .map(|agent| agent.id)
                .find(|id| self.record(*id).is_dir());
            if let Some(id) = found {
                return self.capture(id);
            }
            if start.elapsed() > LAUNCH_WAIT {
                let seen: Vec<_> = status
                    .agents
                    .iter()
                    .map(|agent| {
                        (
                            agent.role,
                            agent.status_label.clone(),
                            agent.problem.clone(),
                        )
                    })
                    .collect();
                return Err(format!("no {role:?} agent launched within 60 s: {seen:?}"));
            }
            std::thread::sleep(LAUNCH_POLL);
        }
    }

    /// The record one agent's stand-in leaves. Input: the agent id. Output: its folder.
    fn record(&self, id: i64) -> PathBuf {
        self.state()
            .join("agents")
            .join(format!("agent-{id}"))
            .join("tmp")
            .join(RECORD)
    }

    /// Captures one launch: the arguments and variable names the stand-in was started
    /// with, the files in the agent's own home and the Seatbelt profile the engine
    /// installed for it (none when it installed none), with the stand-in's folder read as
    /// [`HARNESS_TOKEN`], the rules naming this Mac's own tool installs taken out and stable
    /// facts in their place, facts about its loopback remotes, and the engine root,
    /// projects home and agent home the masked paths in it stand for.
    /// Input: the agent id. Output: the capture.
    fn capture(&self, id: i64) -> Result<Value, String> {
        let record = self.record(id);
        let argv: Vec<String> = bounded(&record.join("argv"))?
            .split_terminator('\0')
            .map(|word| word.replace(&self.stand_in, HARNESS_TOKEN))
            .collect();
        let mut env_names: Vec<String> = bounded(&record.join("env"))?
            .lines()
            .map(str::to_owned)
            .collect();
        env_names.sort();
        env_names.dedup();
        let sdkroot = sdkroot_fact(&mut env_names, true)?;
        let state = self.state();
        let home = crazyagent::team::runtime::agent_home(&state, id);
        let installed = crazyagent::team::runtime::agent_profile(&state, id);
        let (profile, machine_reads, network) = if installed.exists() {
            let text = bounded(&installed)?.replace(&self.stand_in, HARNESS_TOKEN);
            let (text, facts) = without_machine_reads(&text, &machine_folders()?);
            let network = loopback_facts(&text, self.recorder.port);
            (Some(text), facts, network)
        } else {
            (None, Value::Null, Value::Null)
        };
        let canonical = |folder: &Path| {
            folder
                .canonicalize()
                .map_err(|error| format!("{}: {error}", folder.display()))
        };
        let anchors = json!({
            "engine_root": canonical(self.engine.path())?,
            "projects_home": canonical(&self.projects)?,
            "agent_home": canonical(&home)?,
        });
        Ok(json!({
            "argv": argv,
            "env_names": env_names,
            "sdkroot": sdkroot,
            "home_files": home_files(&home)?,
            "profile": profile.as_deref().map(|text| text.lines().collect::<Vec<&str>>()),
            "machine_reads": machine_reads,
            "network": network,
            "anchors": anchors,
        }))
    }
}

/// Stable facts about the loopback remotes a profile lets the agent reach, since the runner
/// masks loopback ports: whether the account's own endpoint port is allowed and how many
/// other loopback ports are. Inputs: the profile and the endpoint port. Output: the facts.
pub(super) fn loopback_facts(profile: &str, endpoint: u16) -> Value {
    let ports: Vec<&str> = profile
        .split("\"localhost:")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    let own = endpoint.to_string();
    json!({
        "endpoint_allowed": ports.contains(&own.as_str()),
        "other_loopback_remotes": ports.iter().filter(|port| **port != own).count(),
    })
}

/// Reads a whole text file of at most [`READ_BYTES_MAX`] bytes. Input: the file. Output:
/// its text, or why it could not be read whole.
fn bounded(file: &Path) -> Result<String, String> {
    let mut text = String::new();
    std::fs::File::open(file)
        .map_err(|error| format!("{}: {error}", file.display()))?
        .take(READ_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > READ_BYTES_MAX {
        return Err("a captured file is larger than 1 MiB".to_owned());
    }
    Ok(text)
}

/// The folders the engine grants every agent read access to only because this Mac has
/// them installed: the Xcode app's developer folder `xcode-select` names and graphify's
/// uv tool with its Python, as the engine's own `ToolHomes::owner` and
/// `toolchains::graphify` find them. Output: the canonical folders, none on a Mac with
/// neither.
fn machine_folders() -> Result<Vec<String>, String> {
    let homes = ToolHomes::owner().map_err(|error| format!("{error:?}"))?;
    homes
        .xcode()
        .map(Path::to_path_buf)
        .into_iter()
        .chain(toolchains::graphify(&homes))
        .map(|folder| {
            folder
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "a machine folder is not UTF-8".to_owned())
        })
        .collect()
}

/// Takes `SDKROOT` out of a launch's variable names and states whether it was there
/// exactly when the engine gives it on this Mac, so the names read the same on a Mac
/// without developer tools. Inputs: the names and whether this launch path gives the
/// toolchain variables at all (a pane always, a headless run only with tools on).
/// Output: `as_the_engine_gives_it`, `unexpected` or `missing`.
pub(super) fn sdkroot_fact(
    env_names: &mut Vec<String>,
    tool_variables: bool,
) -> Result<&'static str, String> {
    let homes = ToolHomes::owner().map_err(|error| format!("{error:?}"))?;
    let expected = tool_variables
        && toolchains::agent_env(Path::new("/"), &homes)
            .iter()
            .any(|(name, _)| *name == SDKROOT);
    let given = env_names.iter().any(|name| name == SDKROOT);
    env_names.retain(|name| name != SDKROOT);
    Ok(match (given, expected) {
        (true, true) | (false, false) => "as_the_engine_gives_it",
        (true, false) => "unexpected",
        (false, true) => "missing",
    })
}

/// Takes out of a Seatbelt profile only the path lines that name exactly one of the given
/// folders, so a rule widened to a parent or a child path stays and shows as a change; a
/// closing parenthesis on a removed line moves to the line before it. Inputs: the profile
/// and the folders. Output: the profile left, and whether every removed line sat in a rule
/// that grants no write and whether any folder is still named anywhere, both reading
/// `true` and `false` when there is nothing to remove.
pub(super) fn without_machine_reads(profile: &str, folders: &[String]) -> (String, Value) {
    let mut kept = String::with_capacity(profile.len());
    let mut rule = "";
    let mut read_only = true;
    for line in profile.split_inclusive('\n') {
        if line.starts_with('(') {
            rule = line;
        }
        let body = line.trim();
        let open = body.trim_end_matches(')');
        let closing = &body[open.len()..];
        let named = folders.iter().any(|folder| {
            PATH_RULES
                .iter()
                .any(|kind| open == format!("({kind} \"{folder}\"") && !closing.is_empty())
        });
        if !named || line.starts_with('(') {
            kept.push_str(line);
            continue;
        }
        read_only &= !rule.contains("file-write");
        let extra = &closing[1..];
        if !extra.is_empty() {
            let had_newline = kept.ends_with('\n');
            if had_newline {
                kept.pop();
            }
            kept.push_str(extra);
            if had_newline {
                kept.push('\n');
            }
        }
    }
    let elsewhere = folders.iter().any(|folder| {
        kept.contains(&format!("\"{folder}\"")) || kept.contains(&format!("\"{folder}/"))
    });
    (
        kept,
        json!({"read_only": read_only, "named_elsewhere": elsewhere}),
    )
}

#[test]
fn only_lines_naming_a_machine_folder_exactly_are_taken_out_and_a_parent_rule_stays() {
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let profile = "(allow file-read-metadata\n  (path-ancestors \"/opt/x\")\n  (path-ancestors \"/Applications/Xcode.app/Contents/Developer\"))\n(allow file-read* file-map-executable\n  (subpath \"/Applications/Xcode.app\")\n  (subpath \"/Applications/Xcode.app/Contents/Developer\")\n  (subpath \"/Applications/Xcode.app/Contents/Developer/usr\"))\n(allow file-write*\n  (subpath \"/opt/w\"))\n";
    let (kept, facts) = without_machine_reads(profile, &[developer]);
    assert_eq!(
        kept,
        "(allow file-read-metadata\n  (path-ancestors \"/opt/x\"))\n(allow file-read* file-map-executable\n  (subpath \"/Applications/Xcode.app\")\n  (subpath \"/Applications/Xcode.app/Contents/Developer/usr\"))\n(allow file-write*\n  (subpath \"/opt/w\"))\n"
    );
    assert_eq!(facts, json!({"read_only": true, "named_elsewhere": true}));
}

#[test]
fn machine_read_facts_read_the_same_on_a_mac_with_no_machine_folders() {
    let profile = "(allow file-read*\n  (subpath \"/opt/x\"))\n";
    let (kept, facts) = without_machine_reads(profile, &[]);
    assert_eq!(kept, profile);
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let with_xcode =
        format!("(allow file-read*\n  (subpath \"/opt/x\")\n  (subpath \"{developer}\"))\n");
    let (kept_there, facts_there) = without_machine_reads(&with_xcode, &[developer]);
    assert_eq!(kept_there, kept);
    assert_eq!(facts_there, facts);
    assert_eq!(facts, json!({"read_only": true, "named_elsewhere": false}));
}

#[test]
fn a_machine_folder_in_a_write_rule_turns_the_read_only_fact() {
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let profile =
        format!("(allow file-write*\n  (subpath \"/opt/w\")\n  (subpath \"{developer}\"))\n");
    let (kept, facts) = without_machine_reads(&profile, &[developer]);
    assert_eq!(kept, "(allow file-write*\n  (subpath \"/opt/w\"))\n");
    assert_eq!(facts, json!({"read_only": false, "named_elsewhere": false}));
}

#[test]
fn loopback_facts_name_the_endpoint_port_and_count_the_others() {
    let profile = "(allow network-outbound (remote ip \"localhost:41234\"))\n(allow network-outbound (remote ip \"localhost:5000\"))\n";
    assert_eq!(
        loopback_facts(profile, 41234),
        json!({"endpoint_allowed": true, "other_loopback_remotes": 1})
    );
    assert_eq!(
        loopback_facts(profile, 41235),
        json!({"endpoint_allowed": false, "other_loopback_remotes": 2})
    );
}
