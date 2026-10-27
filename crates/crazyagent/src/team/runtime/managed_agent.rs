use super::{Grant, Mode, Order, Plan, Report, TeamLimits, TeamRuntime, TurnEnd, per_turn};
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::engine::NativeProfileCustody;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId};
use crate::team::{AgentRole, TeamAccount, TeamReach};
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::time::Duration;

const PROJECT: &str = "dec-090";
const AGENT: i64 = 1;
const SIGNED_IN_CLAUDE: &str = "CRAZYAGENT_MANAGED_CLAUDE_PROFILE";
const SIGNED_IN_CODEX: &str = "CRAZYAGENT_MANAGED_CODEX_PROFILE";

/// The owner's signed-in managed profile of a real engine, named by its slot folder in
/// `CRAZYAGENT_MANAGED_CLAUDE_PROFILE` or `CRAZYAGENT_MANAGED_CODEX_PROFILE`, held through
/// that engine's own custody, which only reads its database and takes a shared lock on the
/// slot. Output of `claude` and `codex`: the profile's connection and custody, or a panic
/// with the plain BLOCKED message.
pub(crate) struct SignedIn {
    pub(crate) connection: ModelConnection,
    custody: NativeProfileCustody,
    host: HostStamp,
    binding: NativeProfileBinding,
}

impl SignedIn {
    /// The owner's signed-in Claude account. Output: the held profile.
    pub(crate) fn claude() -> Self {
        Self::open(SIGNED_IN_CLAUDE, "claude", "a Claude account")
    }

    /// The owner's signed-in ChatGPT account for Codex. Output: the held profile.
    pub(crate) fn codex() -> Self {
        Self::open(SIGNED_IN_CODEX, "codex", "a ChatGPT account for Codex")
    }

    /// Opens the slot one variable names and checks it belongs to the expected harness.
    /// Inputs: the variable, the harness reference and the account wording. Output: the
    /// held profile, or a panic with the plain BLOCKED message.
    fn open(variable: &str, harness: &str, account: &str) -> Self {
        let slot = std::env::var_os(variable).unwrap_or_else(|| {
            panic!(
                "BLOCKED: no signed-in managed {harness} profile. Sign in to {account} in CrazyAgent, then run with {variable}=<engine folder>/native-profiles/<profile key>"
            )
        });
        let slot = PathBuf::from(slot)
            .canonicalize()
            .unwrap_or_else(|_| panic!("BLOCKED: {variable} does not name a profile slot"));
        let manifest = std::fs::read(slot.join("identity.json"))
            .unwrap_or_else(|_| panic!("BLOCKED: {variable} has no identity manifest"));
        let identity: Value = serde_json::from_slice(&manifest).unwrap();
        let bound = |key: &str| reference(identity["binding"][key].as_str().unwrap());
        if bound("harness").as_str() != harness {
            panic!(
                "BLOCKED: {variable} names a {} profile, not a {harness} one",
                bound("harness").as_str()
            );
        }
        let database = slot
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("engine.db");
        let instance = identity["root"]["database_instance"]
            .as_str()
            .unwrap()
            .to_owned();
        let connection = ModelConnection {
            id: reference(&format!("{harness}-live-connection")),
            provider: bound("provider"),
            harness: bound("harness"),
            protocol: bound("protocol"),
            account_ref: bound("account_ref"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "native-default".into(),
                author: bound("provider"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: bound("profile_ref"),
            },
        };
        let custody =
            NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap())
                .unwrap();
        let host = HostStamp {
            db_instance: instance.clone(),
            engine_epoch: "b".repeat(64),
        };
        let binding = NativeProfileBinding {
            database_instance: instance,
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection: connection.clone(),
            custody_id: format!("{:x}", Sha256::digest(&manifest)),
        };
        Self {
            connection,
            custody,
            host,
            binding,
        }
    }

    /// The profile reference the owner's sign-in is kept under. Output: the reference.
    pub(crate) fn profile_ref(&self) -> CredentialRef {
        match &self.connection.auth {
            ModelAuthentication::NativeManaged { profile_ref } => profile_ref.clone(),
            other => panic!("the signed-in profile is not managed: {other:?}"),
        }
    }

    /// Takes the profile for one launch, as every launch on a managed account does.
    /// Input: the launch's name. Output: the profile held for execution.
    pub(crate) fn acquire(&mut self, launch: &str) -> NativeProfileUse {
        self.custody
            .acquire_use(
                &self.host,
                &self.binding,
                &RequestId::new(launch).unwrap(),
                1,
                NativeProfileUsePurpose::Execution,
            )
            .unwrap()
    }
}

/// A tools-on team sub-agent that the real team runtime prepared on a native managed
/// account: a real engine database, the profile slot the engine's custody made, the
/// agent's git worktree and the Seatbelt profile the runtime installed for it.
/// Input: the launch harness and its real executable. Output: the paths the agent and
/// the engine's own processes use, and commands that run as the agent does.
pub(crate) struct ManagedAgent {
    runtime: TeamRuntime,
    plan: Plan,
    pub(crate) base: PathBuf,
    pub(crate) profile_home: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) tmp: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) working_dir: PathBuf,
    pub(crate) sandbox_profile: PathBuf,
    _root: tempfile::TempDir,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

/// Finds a real harness executable from its override variable or Homebrew.
/// Input: the variable and the Homebrew command name. Output: its canonical path,
/// or a panic with the plain BLOCKED message when it is not installed.
fn installed(variable: &str, name: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("/opt/homebrew/bin").join(name))
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: the real {name} executable is not installed"))
}

/// The real Codex executable, from `CRAZYAGENT_CODEX` or Homebrew.
pub(crate) fn codex() -> PathBuf {
    installed("CRAZYAGENT_CODEX", "codex")
}

/// The real Claude Code executable, from `CRAZYAGENT_CLAUDE` or Homebrew.
pub(crate) fn claude() -> PathBuf {
    installed("CRAZYAGENT_CLAUDE", "claude")
}

impl ManagedAgent {
    pub(crate) fn new(harness: &'static str, executable: &Path) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-dec-090-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let database = base.join("engine.db");
        let connection = Connection::open(&database).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        crate::team::initialize(&connection).unwrap();
        crate::project_setup::initialize(&connection).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let project = base.join("project");
        std::fs::create_dir(&project).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "DEC-090".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: harness.into(),
                master_tools: true,
                sub_harnesses: vec![HarnessSlot {
                    harness: harness.into(),
                    agents: 1,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 2,
            },
            documents: vec![],
            folder: project.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        let row = crate::protocol::StoredProject {
            project_id: setup.project_id.clone(),
            spec: crate::protocol::ProjectSpec {
                name: setup.name.clone(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                params![PROJECT, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                params![PROJECT, serde_json::to_string(&setup).unwrap()],
            )
            .unwrap();
        drop(connection);
        let (provider, harness_ref) = match harness {
            "codex" => ("openai", "codex"),
            "claude-code" => ("anthropic", "claude"),
            other => panic!("no managed account shape for {other}"),
        };
        let profile_ref = reference(&format!("{harness_ref}-dec-090-profile"));
        let model = ModelConnection {
            id: reference(&format!("{harness_ref}-dec-090-connection")),
            provider: reference(provider),
            harness: reference(harness_ref),
            protocol: reference("native"),
            account_ref: reference(&format!("{harness_ref}-dec-090-account")),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "native-default".into(),
                author: reference(provider),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: profile_ref.clone(),
            },
        };
        let access = ModelAccess {
            accounts: vec![model.account_ref.clone()],
            connections: vec![model.clone()],
            pools: vec![AccountPool {
                id: reference("dec-090-pool"),
                name: None,
                members: vec![model.id.clone()],
                projects: vec![reference(PROJECT)],
                roles: vec![reference("project_leader")],
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            }],
        };
        let mut runtime = TeamRuntime::open(
            &database,
            &base.join("team"),
            BTreeMap::from([(harness, executable.to_path_buf())]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap();
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let custody = runtime.custody.as_mut().unwrap();
        custody.prepare(&host, &model).unwrap();
        let profile_home = custody.profile_home(&profile_ref);
        assert!(profile_home.is_dir(), "{}", profile_home.display());
        let account = TeamAccount {
            connection: model.id.clone(),
            reach: TeamReach::Managed {
                profile_ref,
                model: None,
            },
        };
        let (_, plan, mode) = runtime
            .prepare(
                &setup,
                AGENT,
                AgentRole::Sub,
                harness,
                true,
                &account,
                false,
            )
            .unwrap();
        let home = match mode {
            Mode::PerTurn { home, .. } => home,
            Mode::Kept { .. } => panic!("a managed agent keeps no process across turns"),
        };
        let tmp = runtime.agent_folder(AGENT).join("tmp");
        let sandbox_profile = plan.sandbox_profile.clone();
        let working_dir = plan.working_dir.clone();
        Self {
            runtime,
            plan,
            base,
            profile_home,
            home,
            tmp,
            project: project.canonicalize().unwrap(),
            working_dir,
            sandbox_profile,
            _root: root,
        }
    }

    /// Runs one turn of the agent the way the runtime runs every turn on a managed
    /// account: the runtime's per-turn launch hands `profile`'s credential copy to the
    /// agent's home, starts the harness in the agent's Seatbelt profile and reads the turn.
    /// Inputs: the signed-in profile held for this turn, the prompt and the turn's time.
    /// Output: the reply text, how the turn ended and its cause.
    pub(super) fn run_turn(
        &self,
        profile: NativeProfileUse,
        prompt: &str,
        turn_time: Duration,
    ) -> (String, TurnEnd, Option<String>) {
        let project = ProjectId::new(PROJECT).unwrap();
        let context = self
            .runtime
            .context(&project, AGENT, AgentRole::Sub, true)
            .unwrap();
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders
            .send(Order::Turn(
                prompt.to_owned(),
                Some(Box::new(Grant { profile, context })),
            ))
            .unwrap();
        drop(orders);
        let cancel = AtomicBool::new(false);
        let interrupt = AtomicBool::new(false);
        let problem = per_turn(
            &self.plan, &self.home, &ordered, &reports, &cancel, &interrupt, turn_time,
        );
        drop(reports);
        let finished = reported.iter().find_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text, end, cause)),
            _ => None,
        });
        finished.unwrap_or_else(|| panic!("the turn never finished: {problem:?}"))
    }

    /// Builds a command that runs `program` the way the agent's harness runs its tools:
    /// inside the agent's installed Seatbelt profile, in its worktree, with the home and
    /// temp folder the runtime gave it and a system path. Input: the program and its
    /// arguments. Output: the command, with stdin closed and stdout and stderr captured.
    pub(crate) fn command(&self, program: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-f")
            .arg(&self.sandbox_profile)
            .arg(program)
            .args(arguments)
            .current_dir(&self.working_dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("CODEX_HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Runs one `/bin/sh` script as the agent. Input: the script.
    /// Output: the finished process with its captured output.
    pub(crate) fn shell(&self, script: &str) -> Output {
        crate::spawn_gate::output(&mut self.command(Path::new("/bin/sh"), &["-c", script])).unwrap()
    }
}
