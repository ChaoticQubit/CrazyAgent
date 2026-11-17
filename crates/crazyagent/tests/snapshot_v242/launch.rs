use super::Scenario;
use super::e2e_support::{Engine, Owner, TMUX, stop_tmux};
use super::harness::{home_files, recorded, set_level};
use super::provider::{Recorder, as_owner, engine, with_owner};
use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::config::{
    AccountPool, ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint,
    ModelReference, ProviderQuotaPolicy,
};
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
use std::collections::BTreeSet;
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
        scenario(
            "engine-claude-code-sub-sandboxed",
            engine_claude_sub_sandboxed,
        ),
        scenario(
            "engine-claude-code-sub-own-branch",
            engine_claude_sub_own_branch,
        ),
        scenario(
            "engine-claude-code-sub-full-access",
            engine_claude_sub_full_access,
        ),
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

/// Captures the launch of the sub agent of one harness the owner delegates one task to,
/// tools off, at one access level. Inputs: the scratch folder, the harness and the level.
/// Output: the capture.
fn engine_sub(scratch: &Path, harness: Harness, access: AgentAccess) -> Result<Value, String> {
    let team = Team::start_with(scratch, harness, access, false)?;
    team.act(TeamAction::Delegate {
        project_id: team.project.clone(),
        harness: Some(harness.id().to_owned()),
        prompt: "Reply with the single word done.".to_owned(),
    })?;
    team.launched(AgentRole::Sub)
}

fn engine_codex_sub_sandboxed(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::Codex, AgentAccess::Sandboxed)
}

fn engine_codex_sub_own_branch(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::Codex, AgentAccess::OwnBranch)
}

fn engine_codex_sub_full_access(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::Codex, AgentAccess::Full)
}

fn engine_claude_sub_sandboxed(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::ClaudeCode, AgentAccess::Sandboxed)
}

fn engine_claude_sub_own_branch(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::ClaudeCode, AgentAccess::OwnBranch)
}

fn engine_claude_sub_full_access(scratch: &Path) -> Result<Value, String> {
    engine_sub(scratch, Harness::ClaudeCode, AgentAccess::Full)
}

/// The harnesses the engine launches as team agents at 2.4.2, each answered by the
/// stand-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Harness {
    Codex,
    ClaudeCode,
}

impl Harness {
    /// The catalog id a team plan and a delegation name.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }

    /// The stand-in's file name, which picks the version line it answers.
    fn program(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }

    /// The engine flag that pins this harness.
    fn flag(self) -> &'static str {
        match self {
            Self::Codex => "--codex-executable",
            Self::ClaudeCode => "--claude-executable",
        }
    }
}

/// The project every engine launch scenario sets up.
const PROJECT: &str = "snapshot-p2-team";

/// The pool that hands the project its one account.
const POOL: &str = "snapshot-p2-pool";

/// The model the project's account names.
const MODEL: &str = "snapshot-model";

/// The connection and account a Claude Code team's pool holds: an Anthropic Messages
/// endpoint on the loopback recorder, which no account draft offers.
const ANTHROPIC_CONNECTION: &str = "snapshot-p2-anthropic";

/// The account the Anthropic Messages connection belongs to.
const ANTHROPIC_ACCOUNT: &str = "snapshot-p2-anthropic-account";

/// What the cargo target folder reads as in every capture: the one literal replaced, only
/// as a whole path prefix. The stand-in and the engine binary both sit below it.
const TARGET_TOKEN: &str = "<target>";

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

/// The cargo caches an agent home links to the owner's own when the owner has them
/// (toolchains.rs `CARGO_SHARED`, made by `prepare_cargo_home`).
const CARGO_SHARED: [&str; 2] = ["registry", "git"];

/// The rule kinds that name one path on a line of their own in a Seatbelt profile.
const PATH_RULES: [&str; 3] = ["subpath", "literal", "path-ancestors"];

/// The harness stand-in, written once per harness name: it answers that harness's version
/// probe, and on a launch records its arguments (NUL separated) and its variable names,
/// never their values, in [`RECORD`] beside its home, moved into place whole, then reads
/// its input until the pane closes.
const STAND_IN: &str = r#"#!/bin/sh
case "$1" in
  --version)
    case "${0##*/}" in
      claude) echo '2.1.0 (Claude Code)' ;;
      *) echo 'codex-cli 0.167.1' ;;
    esac
    exit 0 ;;
esac
h="${CODEX_HOME:-${CLAUDE_CONFIG_DIR:-$HOME}}"
r="$h/../tmp/snapshot-v242-launch"
/bin/mkdir -p "$r.part" || exit 1
for a in "$@"; do printf '%s\000' "$a"; done > "$r.part/argv"
/usr/bin/awk 'BEGIN { for (name in ENVIRON) print name }' > "$r.part/env"
/bin/mv "$r.part" "$r" || exit 1
while IFS= read -r line; do :; done
"#;

/// Writes the stand-in as `codex` and `claude` at its one fixed place directly below the
/// cargo target folder: outside every temp folder the engine refuses a harness in, and not
/// below cargo's `tmp`, whose name after `<target>` the runner would read as a temp folder.
/// Output: the canonical folder holding both.
pub(super) fn stand_in() -> Result<PathBuf, String> {
    let folder = Path::new(&target()?).join("snapshot-v242");
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let folder = folder.canonicalize().map_err(|error| error.to_string())?;
    for harness in [Harness::Codex, Harness::ClaudeCode] {
        let program = folder.join(harness.program());
        if std::fs::read_to_string(&program).ok().as_deref() != Some(STAND_IN) {
            std::fs::write(&program, STAND_IN).map_err(|error| error.to_string())?;
        }
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        assert!(program.is_file());
        if passed_over_as_agents(&program) {
            return Err(format!(
                "the engine's tool resolver passes over the {} stand-in as one agents can \
                 write, so 2.4.2 would refuse every launch with its text withheld: a folder \
                 above the cargo target folder holds .crazyagent; build with \
                 CARGO_TARGET_DIR outside it",
                harness.program()
            ));
        }
    }
    assert!(folder.is_absolute());
    Ok(folder)
}

/// Whether the engine's own tool resolver passes over a stand-in as a file agents can write,
/// as it does below any folder that holds `.crazyagent` (tools.rs `in_project`). The places
/// name no HOME, so every folder above the file counts, as in an engine on a fixture HOME.
/// Input: the stand-in. Output: the answer; any other refusal is not this one.
pub(super) fn passed_over_as_agents(program: &Path) -> bool {
    let places = crazyagent::tools::Places::new(Vec::new(), None);
    crazyagent::tools::resolve_entry(program, &places)
        .err()
        .and_then(|missing| missing.refused)
        .is_some_and(|refused| refused.why == crazyagent::tools::Refusal::AgentsCanWrite)
}

/// The canonical cargo target folder, parent of cargo's per-target temp folder and an
/// ancestor of the engine binary the engine grants its agents. Output: the folder text.
pub(super) fn target() -> Result<String, String> {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .ok_or_else(|| "cargo's temp folder has no parent".to_owned())?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let text = folder
        .to_str()
        .ok_or_else(|| "the target folder is not UTF-8".to_owned())?;
    assert!(text.starts_with('/') && !text.ends_with('/'));
    Ok(text.to_owned())
}

/// Replaces the target folder with [`TARGET_TOKEN`] wherever it starts a path and ends at
/// a `/` or at the path's end, never inside a longer path or name. Inputs: the text and
/// the folder. Output: the text with those prefixes replaced.
pub(super) fn without_target(text: &str, target: &str) -> String {
    let path_char = |c: char| c.is_ascii_alphanumeric() || "/._-~+@%".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, _) in text.match_indices(target) {
        let end = start + target.len();
        if start < copied {
            continue;
        }
        let starts_path = text[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !path_char(before));
        let whole = text[end..]
            .chars()
            .next()
            .is_none_or(|after| after == '/' || !path_char(after));
        if starts_path && whole {
            out.push_str(&text[copied..start]);
            out.push_str(TARGET_TOKEN);
            copied = end;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// A real engine started with one harness's stand-in pinned, holding one project with a
/// team plan of that harness's agents whose pool has one keyless account on a loopback
/// recorder that harness can run on, all seeded through owner requests. Dropping it ends
/// the engine's tmux server and every pane in it, then the engine.
pub(super) struct Team {
    pub(super) engine: Engine,
    pub(super) project: ProjectId,
    access: AgentAccess,
    target: String,
    projects: PathBuf,
    tmux: PathBuf,
    recorder: Recorder,
}

impl Drop for Team {
    fn drop(&mut self) {
        stop_tmux(&self.tmux, &self.engine.path().join(FOLDER).join(SOCKET));
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

/// Sends one model access change as the owner's model access screen does, against a fresh
/// read of the page of the collection it changes. Inputs: the owner, the change and what it
/// is, for the message. Output: nothing, or why the change was not applied.
async fn model_access(owner: &mut Owner, edit: ModelAccessEdit, what: &str) -> Result<(), String> {
    let collection = match &edit {
        ModelAccessEdit::AddAccount { .. } | ModelAccessEdit::RemoveAccount { .. } => {
            ModelAccessCollection::Accounts
        }
        ModelAccessEdit::CreateConnection { .. }
        | ModelAccessEdit::ReplaceConnection { .. }
        | ModelAccessEdit::RemoveConnection { .. } => ModelAccessCollection::Connections,
        _ => ModelAccessCollection::Pools,
    };
    let page = owner
        .controller
        .read(OwnerRead::ModelAccessPage {
            collection,
            cursor: None,
            limit: 1,
        })
        .await
        .map_err(|error| error.0)?;
    let OwnerData::ModelAccessPage { source_hash, .. } = &page.data else {
        return Err("the model access page did not come back".to_owned());
    };
    let generation = owner.next();
    owner
        .controller
        .begin_mutation(
            OwnerMutation::ModelAccess {
                expected_config_id: page.config_id.clone(),
                expected_source_hash: source_hash.clone(),
                edit,
            },
            generation,
        )
        .map_err(|error| error.0)?;
    let response = owner
        .controller
        .send_pending()
        .await
        .map_err(|error| error.0)?;
    applied(what, recorded(response)?)
}

/// A settings reference from fixed text. Input: the text. Output: the reference.
fn reference(value: &str) -> Result<CredentialRef, String> {
    CredentialRef::new(value).map_err(|error| error.to_string())
}

/// Creates the pool holding one account for one project, as the owner's pool screen saves
/// it. Inputs: the owner, the account and the project. Output: nothing, or why the pool was
/// not created.
async fn create_pool(
    owner: &mut Owner,
    member: &CredentialRef,
    project: &ProjectId,
) -> Result<(), String> {
    let pool = AccountPool {
        id: reference(POOL)?,
        name: None,
        members: vec![member.clone()],
        projects: vec![reference(project.as_str())?],
        roles: vec![reference("project_leader")?],
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::default(),
    };
    model_access(
        owner,
        ModelAccessEdit::CreatePool { pool },
        "create the pool",
    )
    .await
}

/// Adds the keyless Anthropic Messages connection on the loopback recorder after its
/// account, the two changes the owner's model access screen sends; Claude Code runs only
/// on that protocol and no account draft offers it off Anthropic's own host. Inputs: the
/// owner and the endpoint address. Output: the connection, or why it was not added.
async fn add_anthropic(owner: &mut Owner, url: String) -> Result<CredentialRef, String> {
    let connection = ModelConnection {
        id: reference(ANTHROPIC_CONNECTION)?,
        provider: reference("anthropic")?,
        harness: reference(Harness::ClaudeCode.id())?,
        protocol: reference("anthropic-messages")?,
        account_ref: reference(ANTHROPIC_ACCOUNT)?,
        endpoint: ModelEndpoint::Http { url },
        model: ModelReference {
            id: MODEL.to_owned(),
            author: reference("anthropic")?,
        },
        auth: ModelAuthentication::None {},
    };
    let id = connection.id.clone();
    let account = ModelAccessEdit::AddAccount {
        account_ref: connection.account_ref.clone(),
    };
    model_access(owner, account, "add the Anthropic Messages account").await?;
    let edit = ModelAccessEdit::CreateConnection { connection };
    model_access(owner, edit, "add the Anthropic Messages connection").await?;
    Ok(id)
}

/// The team plan every launch scenario saves: a master and one agent row of one harness.
/// Inputs: the harness and whether the agents run with tools. Output: the plan.
fn plan(harness: Harness, tools: bool) -> TeamPlan {
    TeamPlan {
        master: harness.id().to_owned(),
        master_tools: tools,
        sub_harnesses: vec![HarnessSlot {
            harness: harness.id().to_owned(),
            agents: 1,
            tools,
        }],
        min_agents: 1,
        max_agents: 2,
    }
}

impl Team {
    /// Starts a Codex team as [`Team::start_with`] does. Inputs: the scratch folder, the
    /// level and whether the agents run with tools. Output: the team.
    pub(super) fn start(scratch: &Path, access: AgentAccess, tools: bool) -> Result<Self, String> {
        Self::start_with(scratch, Harness::Codex, access, tools)
    }

    /// Starts the engine with one harness's stand-in pinned and seeds the project through
    /// owner requests: the projects home, the account (an OpenAI-compatible account draft
    /// for Codex, a connected Anthropic Messages account for Claude Code), its pool, the
    /// project with its team plan and its agent access level. Inputs: the scratch folder,
    /// the harness, the level and whether the agents run with tools. Output: the team, or
    /// the first step that was refused; UNAVAILABLE without tmux, since the engine then runs
    /// the agents headless and a headless launch is not the 2.4.2 pane launch.
    pub(super) fn start_with(
        scratch: &Path,
        harness: Harness,
        access: AgentAccess,
        tools: bool,
    ) -> Result<Self, String> {
        let folder = stand_in()?;
        let target = target()?;
        let projects = scratch
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join("projects");
        std::fs::create_dir(&projects).map_err(|error| error.to_string())?;
        let projects_home = projects.clone();
        let recorder = Recorder::start(false)?;
        let engine = engine(
            scratch,
            Some((harness.flag(), folder.join(harness.program()))),
        )?;
        let Some(tmux) = installed(engine.path()) else {
            return Err(TMUX.to_owned());
        };
        let project = ProjectId::new(PROJECT).map_err(|error| error.to_string())?;
        let url = match harness {
            Harness::Codex => format!("http://127.0.0.1:{}/v1", recorder.port),
            Harness::ClaudeCode => format!("http://127.0.0.1:{}", recorder.port),
        };
        let seeded = project.clone();
        with_owner(&engine, |mut owner| async move {
            let steps = async {
                applied("set the projects home", owner.set_home(&projects).await)?;
                let account = match harness {
                    Harness::Codex => {
                        let draft = AccountDraft {
                            provider: Provider::OpenaiCompatible,
                            method: SignInMethod::Url,
                            name: "Snapshot team".to_owned(),
                            url: Some(url),
                            model: Some(MODEL.to_owned()),
                        };
                        owner
                            .controller
                            .add_account(draft, None)
                            .await
                            .map_err(|error| format!("add_account: {}", error.0))?
                            .id
                    }
                    Harness::ClaudeCode => add_anthropic(&mut owner, url).await?,
                };
                create_pool(&mut owner, &account, &seeded).await?;
                let setup = ProjectSetup {
                    about: "One team launch, captured.".to_owned(),
                    tech_stack: Vec::new(),
                    platforms: Vec::new(),
                    research: Vec::new(),
                    pool: Some(CredentialRef::new(POOL).map_err(|error| error.to_string())?),
                    team: Some(plan(harness, tools)),
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
            access,
            target,
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
    /// installed for it (none only at full access, see [`profile_may_be_missing`]), with the cargo target folder read as
    /// [`TARGET_TOKEN`], the rules naming this Mac's own tool installs taken out and stable
    /// facts in their place, facts about its loopback remotes, and the engine root,
    /// projects home and agent home the masked paths in it stand for.
    /// Input: the agent id. Output: the capture.
    fn capture(&self, id: i64) -> Result<Value, String> {
        let record = self.record(id);
        let argv: Vec<String> = bounded(&record.join("argv"))?
            .split_terminator('\0')
            .map(|word| without_target(word, &self.target))
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
        let mut home_files = home_files(&home)?;
        let cargo_links = cargo_links_fact(&mut home_files)?;
        let installed = crazyagent::team::runtime::agent_profile(&state, id);
        let (profile, machine_reads, network) = if installed.exists() {
            let text = without_target(&bounded(&installed)?, &self.target);
            let (text, facts) = without_machine_reads(&text, &machine_folders()?);
            let network = loopback_facts(&text, self.recorder.port);
            (Some(text), facts, network)
        } else {
            profile_may_be_missing(self.access)?;
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
            "home_files": home_files,
            "cargo_shared_links": cargo_links,
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

/// Takes the cargo cache links out of an agent home listing and states whether each was
/// there exactly when the owner has that cache, so the listing reads the same on a Mac
/// without cargo. Input: the listing. Output: `as_the_owner_has_them`, or the first name
/// that differs.
fn cargo_links_fact(home_files: &mut Vec<String>) -> Result<String, String> {
    let homes = ToolHomes::owner().map_err(|error| format!("{error:?}"))?;
    let mut fact = "as_the_owner_has_them".to_owned();
    for name in CARGO_SHARED {
        let listed = format!(".cargo/{name}");
        let given = home_files.contains(&listed);
        let owned = homes.cargo().join(name).is_dir();
        home_files.retain(|file| *file != listed);
        if given != owned && fact == "as_the_owner_has_them" {
            fact = format!("{listed} differs");
        }
    }
    Ok(fact)
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

/// Refuses a launch captured with no Seatbelt profile unless it ran at full access, the only
/// level the engine starts without one, so a wrong profile path or a sandboxed launch that
/// installed none is never written as a golden. Input: the level. Output: nothing, or why.
pub(super) fn profile_may_be_missing(access: AgentAccess) -> Result<(), String> {
    match access {
        AgentAccess::Full => Ok(()),
        AgentAccess::Sandboxed | AgentAccess::OwnBranch => Err(format!(
            "the {access:?} launch installed no Seatbelt profile; only a full-access launch is captured without one"
        )),
    }
}

/// Spells a folder as the engine's Seatbelt regex rules do, every regex character escaped,
/// so a credential deny naming a machine folder is found. Input: the folder. Output: the
/// pattern text, without anchors.
fn seatbelt_escape(folder: &str) -> String {
    let mut pattern = String::with_capacity(folder.len() * 2);
    for character in folder.chars() {
        if ".^$*+?()[]{}|".contains(character) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    assert!(pattern.len() >= folder.len());
    pattern
}

/// Reads one profile line that is a whole rule naming exactly one machine folder by one of
/// the [`PATH_RULES`] forms, such as `(allow file-read* (literal "<folder>"))`. Inputs: the
/// trimmed line and the folders. Output: the folder, the form and the rule's operations, or
/// none for any other line.
fn one_line_rule<'a>(body: &'a str, folders: &'a [String]) -> Option<(&'a str, &'a str, &'a str)> {
    let rest = body
        .strip_prefix("(allow ")
        .or_else(|| body.strip_prefix("(deny "))?;
    folders.iter().find_map(|folder| {
        PATH_RULES.iter().find_map(|kind| {
            let operations = rest.strip_suffix(&format!(" ({kind} \"{folder}\"))"))?;
            let plain = !operations.is_empty() && !operations.contains(['(', ')', '"']);
            plain.then_some((folder.as_str(), *kind, operations))
        })
    })
}

/// Tells whether one profile line, its closing parentheses taken off, is the credential deny
/// the engine writes below a toolchain folder, anchored at one machine folder. Inputs: the
/// line so trimmed and the folders. Output: the folder, or none.
fn credential_deny<'a>(open: &str, folders: &'a [String]) -> Option<&'a str> {
    folders
        .iter()
        .find(|folder| {
            open.starts_with(&format!("(regex #\"^{}/(.*/)?(", seatbelt_escape(folder)))
                && open.ends_with(")(/.*)?$\"")
        })
        .map(String::as_str)
}

/// Takes out of a Seatbelt profile only the lines that name exactly one of the given
/// folders: a path line of a [`PATH_RULES`] form, a whole one-line rule of such a form and
/// the credential deny regex below the folder, so a rule widened to a parent or a child path
/// stays and shows as a change; a closing parenthesis on a removed line moves to the line
/// before it. Inputs: the profile and the folders. Output: the profile left and three facts
/// that read the same on a Mac with no such folder: whether every removed allow rule grants
/// no write, whether any folder is still named anywhere, in plain or regex spelling, and
/// whether every folder a removed allow rule grants by `subpath` lost a credential deny too.
pub(super) fn without_machine_reads(profile: &str, folders: &[String]) -> (String, Value) {
    let mut kept = String::with_capacity(profile.len());
    let mut rule = "";
    let mut read_only = true;
    let mut granted: BTreeSet<&str> = BTreeSet::new();
    let mut denied: BTreeSet<&str> = BTreeSet::new();
    for line in profile.split_inclusive('\n') {
        let body = line.trim();
        if line.starts_with('(') {
            rule = line;
            match one_line_rule(body, folders) {
                Some((folder, kind, operations)) if body.starts_with("(allow ") => {
                    read_only &= !operations.contains("file-write");
                    if kind == "subpath" {
                        granted.insert(folder);
                    }
                }
                Some(_) => {}
                None => kept.push_str(line),
            }
            continue;
        }
        let open = body.trim_end_matches(')');
        let closing = &body[open.len()..];
        if closing.is_empty() {
            kept.push_str(line);
            continue;
        }
        let allows = rule.starts_with("(allow ");
        let path = folders.iter().find_map(|folder| {
            PATH_RULES
                .iter()
                .find(|kind| open == format!("({kind} \"{folder}\""))
                .map(|kind| (folder.as_str(), *kind))
        });
        match (path, credential_deny(open, folders)) {
            (Some((folder, kind)), _) => {
                if allows {
                    read_only &= !rule.contains("file-write");
                    if kind == "subpath" {
                        granted.insert(folder);
                    }
                }
            }
            (None, Some(folder)) if rule.starts_with("(deny ") => {
                denied.insert(folder);
            }
            _ => {
                kept.push_str(line);
                continue;
            }
        }
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
    assert!(granted.len() <= folders.len() && denied.len() <= folders.len());
    let elsewhere = folders
        .iter()
        .any(|folder| kept.contains(folder.as_str()) || kept.contains(&seatbelt_escape(folder)));
    let credentials_denied = granted.iter().all(|folder| denied.contains(folder));
    (
        kept,
        json!({
            "read_only": read_only,
            "named_elsewhere": elsewhere,
            "credentials_denied": credentials_denied,
        }),
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
    assert_eq!(
        facts,
        json!({"read_only": true, "named_elsewhere": true, "credentials_denied": false})
    );
}

#[test]
fn machine_read_facts_read_the_same_on_a_mac_with_no_machine_folders() {
    let profile = "(allow file-read*\n  (subpath \"/opt/x\"))\n(deny file-read* file-write*\n  (subpath \"/s\"))\n";
    let (kept, facts) = without_machine_reads(profile, &[]);
    assert_eq!(kept, profile);
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let with_xcode = format!(
        "(allow file-read*\n  (subpath \"/opt/x\")\n  (subpath \"{developer}\"))\n(deny file-read* file-write*\n  (subpath \"/s\")\n  (regex #\"^/Applications/Xcode\\.app/Contents/Developer/(.*/)?(\\.netrc|\\.npmrc)(/.*)?$\"))\n(allow file-read* (literal \"{developer}\"))\n"
    );
    let (kept_there, facts_there) = without_machine_reads(&with_xcode, &[developer]);
    assert_eq!(kept_there, kept);
    assert_eq!(facts_there, facts);
    assert_eq!(
        facts,
        json!({"read_only": true, "named_elsewhere": false, "credentials_denied": true})
    );
}

#[test]
fn a_machine_folder_in_a_write_rule_turns_the_read_only_fact() {
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let profile =
        format!("(allow file-write*\n  (subpath \"/opt/w\")\n  (subpath \"{developer}\"))\n");
    let (kept, facts) = without_machine_reads(&profile, &[developer]);
    assert_eq!(kept, "(allow file-write*\n  (subpath \"/opt/w\"))\n");
    assert_eq!(
        facts,
        json!({"read_only": false, "named_elsewhere": false, "credentials_denied": false})
    );
}

#[test]
fn a_granted_machine_folder_without_its_credential_deny_turns_the_fact() {
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let profile = format!(
        "(allow file-read* file-map-executable\n  (subpath \"/opt/x\")\n  (subpath \"{developer}\"))\n(deny file-read* file-write*\n  (subpath \"/s\"))\n"
    );
    let (kept, facts) = without_machine_reads(&profile, &[developer]);
    assert_eq!(
        kept,
        "(allow file-read* file-map-executable\n  (subpath \"/opt/x\"))\n(deny file-read* file-write*\n  (subpath \"/s\"))\n"
    );
    assert_eq!(
        facts,
        json!({"read_only": true, "named_elsewhere": false, "credentials_denied": false})
    );
}

#[test]
fn a_one_line_rule_naming_a_machine_folder_is_taken_out_and_a_write_one_turns_read_only() {
    let graphify = "/Users/o/.local/share/uv/tools/graphifyy".to_owned();
    let base = "(allow file-read*\n  (subpath \"/opt/x\"))\n";
    let read = format!("{base}(allow file-read* (literal \"{graphify}\"))\n");
    let (kept, facts) = without_machine_reads(&read, std::slice::from_ref(&graphify));
    assert_eq!(kept, base);
    assert_eq!(
        facts,
        json!({"read_only": true, "named_elsewhere": false, "credentials_denied": true})
    );
    let write = format!("{base}(allow file-write* (subpath \"{graphify}\"))\n");
    let (kept, facts) = without_machine_reads(&write, &[graphify]);
    assert_eq!(kept, base);
    assert_eq!(
        facts,
        json!({"read_only": false, "named_elsewhere": false, "credentials_denied": false})
    );
}

#[test]
fn a_machine_folder_in_a_regex_kept_elsewhere_reads_as_named_elsewhere() {
    let developer = "/Applications/Xcode.app/Contents/Developer".to_owned();
    let profile = "(allow file-read*\n  (regex #\"^/Applications/Xcode\\.app/Contents/Developer/usr/bin/[^/]+$\"))\n";
    let (kept, facts) = without_machine_reads(profile, &[developer]);
    assert_eq!(kept, profile);
    assert_eq!(facts["named_elsewhere"], json!(true));
}

#[test]
fn the_escape_spells_a_folder_as_the_engine_s_seatbelt_patterns_do() {
    assert_eq!(
        seatbelt_escape("/Applications/Xcode.app/Contents/Developer"),
        "/Applications/Xcode\\.app/Contents/Developer"
    );
    assert_eq!(
        seatbelt_escape("/u/.local/a+b(c)[d]{e}|f^g$h*i?j"),
        "/u/\\.local/a\\+b\\(c\\)\\[d\\]\\{e\\}\\|f\\^g\\$h\\*i\\?j"
    );
}

#[test]
fn only_a_full_access_launch_may_be_captured_with_no_profile() {
    assert_eq!(profile_may_be_missing(AgentAccess::Full), Ok(()));
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        let refused = profile_may_be_missing(access);
        assert!(
            refused
                .as_ref()
                .is_err_and(|text| text.contains("no Seatbelt profile")),
            "{access:?}: {refused:?}"
        );
    }
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

#[test]
fn the_target_folder_is_replaced_only_as_a_whole_path_prefix() {
    let target = "/w/.local-state/target";
    for (text, read) in [
        (
            "(subpath \"/w/.local-state/target/debug\")",
            "(subpath \"<target>/debug\")",
        ),
        ("\"/w/.local-state/target\"", "\"<target>\""),
        (
            "/w/.local-state/target/snapshot-v242/codex",
            "<target>/snapshot-v242/codex",
        ),
        (
            "\"/w/.local-state/target2/debug\"",
            "\"/w/.local-state/target2/debug\"",
        ),
        (
            "\"/v/w/.local-state/target/debug\"",
            "\"/v/w/.local-state/target/debug\"",
        ),
    ] {
        assert_eq!(without_target(text, target), read, "{text}");
    }
}

#[test]
fn a_stand_in_below_a_crazyagent_folder_is_passed_over_and_one_outside_is_not() {
    let scratch = super::e2e_support::scratch("ca-snap-standin-");
    let root = scratch.path().canonicalize().unwrap();
    for (folder, marked) in [(root.join("marked"), true), (root.join("plain"), false)] {
        let bin = folder.join("target").join("snapshot-v242");
        std::fs::create_dir_all(&bin).unwrap();
        if marked {
            std::fs::create_dir(folder.join(".crazyagent")).unwrap();
        }
        let program = bin.join("codex");
        std::fs::write(&program, STAND_IN).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(passed_over_as_agents(&program), marked, "{marked}");
    }
}
