use super::{
    Adoption, Grant, Launch, Managed, Mode, Order, Plan, Report, Seat, TeamLimits, TeamRuntime,
    TurnEnd, TurnRules, per_turn,
};
use crate::config::{
    AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};
use crate::engine::NativeProfileCustody;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::harnesses::terminal_session::{PaneRecord, Resume, paste};
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId};
use crate::sandbox::policy::{AgentAccess, AgentPolicy};
use crate::team::{AgentRole, TaskSource, TeamAccount, TeamReach};
use crate::terminal::{AgentId, Host};
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const PROJECT: &str = "dec-090";
pub(super) const AGENT: i64 = 1;
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
    pub(crate) background: crate::team::BackgroundAtEnd,
    role: AgentRole,
    stamp: HostStamp,
    binding: NativeProfileBinding,
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

/// Where a prepared agent's harness app runs: as its worker's child, in a pane on a given
/// host, or as a native-mode Claude Code lead on the engine's own terminal host at this
/// engine's root, the project's Claude Code agents set to native teammates.
enum Placement {
    Child,
    Pane(Arc<dyn Host>),
    NativeLead,
}

impl ManagedAgent {
    pub(crate) fn new(harness: &'static str, executable: &Path) -> Self {
        Self::open(harness, executable, Placement::Child, AgentRole::Sub)
    }

    /// The same agent, or the project's master when `role` says so, with its harness app in
    /// a pane on `host`, as agents run where the engine has a terminal. Inputs: the launch
    /// harness, its executable, the host and the role. Output: the prepared agent.
    pub(crate) fn in_pane(
        harness: &'static str,
        executable: &Path,
        host: Arc<dyn Host>,
        role: AgentRole,
    ) -> Self {
        Self::open(harness, executable, Placement::Pane(host), role)
    }

    /// A Claude Code lead the runtime itself plans as a native-mode lead (DEC-104, TTDL): the
    /// project's Claude Code agents run native teammates and the terminal host is the
    /// engine's own, so `prepare` installs the teammates' profile and names their server,
    /// and starting the lead gives its pane that server's anchor as `TMUX` and `TMUX_PANE`.
    /// Input: the real Claude Code executable. Output: the prepared lead.
    pub(crate) fn engine_native_lead(executable: &Path) -> Self {
        Self::open(
            "claude-code",
            executable,
            Placement::NativeLead,
            AgentRole::Sub,
        )
    }

    fn open(
        harness: &'static str,
        executable: &Path,
        placement: Placement,
        role: AgentRole,
    ) -> Self {
        let native = matches!(placement, Placement::NativeLead);
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
        if native {
            use crate::coord::settings::{HarnessKind, SubagentMode, set_mode};
            set_mode(
                &connection,
                &setup.project_id,
                HarnessKind::ClaudeCode,
                SubagentMode::Native,
            )
            .unwrap();
        }
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
        let pane = match placement {
            Placement::Child => None,
            Placement::Pane(host) => Some(host),
            Placement::NativeLead => Some(crate::terminal::tmux_host(
                &base,
                Some(
                    crate::terminal::tmux::installed(&base)
                        .expect("BLOCKED: tmux must be installed"),
                ),
            )),
        };
        if let Some(pane) = pane {
            runtime = runtime.with_terminal(pane).unwrap();
        }
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let custody = runtime.custody.as_mut().unwrap();
        let (custody_id, _) = custody.prepare(&host, &model).unwrap();
        let binding = NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection: model.clone(),
            custody_id,
        };
        let profile_home = custody.profile_home(&profile_ref);
        assert!(profile_home.is_dir(), "{}", profile_home.display());
        let account = TeamAccount {
            connection: model.id.clone(),
            reach: TeamReach::Managed {
                profile_ref,
                model: None,
            },
        };
        runtime
            .connection
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms)
                 VALUES (?1,?2,?3,?4,1,?5,'starting',1,1)",
                params![AGENT, PROJECT, role.name(), harness, model.id.as_str()],
            )
            .unwrap();
        if role == AgentRole::Sub {
            crate::team::names::assign_persona(&runtime.connection, &setup.project_id, AGENT)
                .unwrap();
        }
        let (_, plan, mode) = runtime
            .prepare(
                &setup,
                AGENT,
                role,
                harness,
                true,
                &account,
                false,
                crate::agent_git::Layout::Linked,
            )
            .unwrap();
        assert_eq!(
            plan.native, native,
            "the runtime plans a native lead exactly when the project runs native teammates"
        );
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
            background: crate::team::BackgroundAtEnd::Wait,
            role,
            stamp: host,
            binding,
            _root: root,
        }
    }

    /// Takes this engine's own managed profile for one launch, as the runtime's grant does.
    /// Input: the launch's name. Output: the profile held for execution.
    pub(crate) fn acquire(&mut self, launch: &str) -> NativeProfileUse {
        self.runtime
            .custody
            .as_mut()
            .unwrap()
            .acquire_use(
                &self.stamp,
                &self.binding,
                &RequestId::new(launch).unwrap(),
                1,
                NativeProfileUsePurpose::Execution,
            )
            .unwrap()
    }

    /// Runs one per-turn worker over several turns the way the runtime runs a managed
    /// agent, continuing `resume` when a wake gives one, under the agent's Background
    /// commands at task end choice. Inputs: the conversation to continue, each turn's
    /// prompt with the profile held for it and the turn's time. Output: every report the
    /// worker sent and the problem that ended it, if any.
    pub(super) fn run_turns(
        &mut self,
        resume: Option<Resume>,
        turns: Vec<(&str, NativeProfileUse)>,
        turn_time: Duration,
    ) -> (Vec<Report>, Option<String>) {
        let project = ProjectId::new(PROJECT).unwrap();
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        for (prompt, profile) in turns {
            let context = self
                .runtime
                .context(
                    &project,
                    AGENT,
                    self.role,
                    true,
                    crate::agent_git::Layout::Linked,
                )
                .unwrap();
            orders
                .send(Order::Turn(
                    prompt.to_owned(),
                    Some(Box::new(Grant {
                        profile,
                        context,
                        access: crate::sandbox::policy::AgentAccess::Sandboxed,
                    })),
                    TurnRules {
                        limit: Some(turn_time),
                        background: self.background,
                    },
                ))
                .unwrap();
        }
        drop(orders);
        self.plan.resume = resume;
        let cancel = AtomicBool::new(false);
        let interrupt = AtomicBool::new(false);
        let problem = per_turn(
            &self.plan, &self.home, &ordered, &reports, &cancel, &interrupt,
        );
        drop(reports);
        (reported.iter().collect(), problem)
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
            .context(
                &project,
                AGENT,
                self.role,
                true,
                crate::agent_git::Layout::Linked,
            )
            .unwrap();
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders
            .send(Order::Turn(
                prompt.to_owned(),
                Some(Box::new(Grant {
                    profile,
                    context,
                    access: crate::sandbox::policy::AgentAccess::Sandboxed,
                })),
                TurnRules {
                    limit: Some(turn_time),
                    background: crate::team::BackgroundAtEnd::Wait,
                },
            ))
            .unwrap();
        drop(orders);
        let cancel = AtomicBool::new(false);
        let interrupt = AtomicBool::new(false);
        let problem = per_turn(
            &self.plan, &self.home, &ordered, &reports, &cancel, &interrupt,
        );
        drop(reports);
        let finished = reported.iter().find_map(|report| match report {
            Report::Finished { text, end, cause } => Some((text, end, cause)),
            _ => None,
        });
        finished.unwrap_or_else(|| panic!("the turn never finished: {problem:?}"))
    }

    /// Makes this agent a native-mode lead (DEC-104) that runs in a tmux server of its own,
    /// as the runtime plans a native lead on a signed-in account. Input: none. Output: none.
    fn make_native(&mut self) {
        if self.plan.native {
            return;
        }
        let (host, agent) = self
            .plan
            .pane
            .clone()
            .expect("a native lead runs in a pane");
        self.plan.native = true;
        self.plan.tmux =
            Some(crate::terminal::tmux::installed(&self.base).expect("tmux must be installed"));
        self.plan.pane = Some((crate::terminal::own_server(host), agent));
    }

    /// The first order of a signed-in native lead: a turn carrying the held profile, the
    /// project context and the saved access level. Inputs: the profile, the prompt, the
    /// turn's time and the saved level. Output: the order.
    fn granted_turn(
        &self,
        profile: NativeProfileUse,
        prompt: &str,
        turn_time: Duration,
        access: AgentAccess,
    ) -> Order {
        let project = ProjectId::new(PROJECT).unwrap();
        let context = self
            .runtime
            .context(
                &project,
                AGENT,
                self.role,
                true,
                crate::agent_git::Layout::Linked,
            )
            .unwrap();
        Order::Turn(
            prompt.to_owned(),
            Some(Box::new(Grant {
                profile,
                context,
                access,
            })),
            TurnRules {
                limit: Some(turn_time),
                background: crate::team::BackgroundAtEnd::Wait,
            },
        )
    }

    /// Runs this agent as a native-mode lead on one kept session the way the runtime runs a
    /// signed-in native lead: its own tmux server, the first turn carrying the held profile
    /// and later turns none, then a Stop, leaving the pane running when `leave` is set.
    /// Inputs: the profile, the prompts, each turn's time and whether the engine leaves the
    /// pane. Output: every report the lead's worker sent and the problem that ended it.
    pub(super) fn run_native_lead(
        &mut self,
        profile: NativeProfileUse,
        prompts: &[&str],
        turn_time: Duration,
        leave: bool,
    ) -> (Vec<Report>, Option<String>) {
        assert!(!prompts.is_empty());
        self.make_native();
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders
            .send(self.granted_turn(profile, prompts[0], turn_time, AgentAccess::Sandboxed))
            .unwrap();
        for prompt in &prompts[1..] {
            orders
                .send(Order::Turn(
                    (*prompt).to_owned(),
                    None,
                    TurnRules {
                        limit: Some(turn_time),
                        background: crate::team::BackgroundAtEnd::Wait,
                    },
                ))
                .unwrap();
        }
        orders.send(Order::Stop).unwrap();
        let problem = super::native_session(
            &self.plan,
            &self.home,
            &ordered,
            &reports,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &AtomicBool::new(leave),
        );
        drop(reports);
        (reported.iter().collect(), problem)
    }

    /// Takes over the native lead a run with `leave` set left in its pane, as the next engine
    /// adopts it as Managed: the first turn carries a fresh held profile, whose credential is
    /// handed to the lead's home again, then a Stop. Inputs: the lead's pane record, the
    /// profile, the prompt and the turn's time. Output: every report and the ending problem.
    pub(super) fn adopt_native_lead(
        &mut self,
        record: PaneRecord,
        profile: NativeProfileUse,
        prompt: &str,
        turn_time: Duration,
    ) -> (Vec<Report>, Option<String>) {
        self.make_native();
        let (host, agent) = self
            .plan
            .pane
            .clone()
            .expect("a native lead runs in a pane");
        let adoption = Adoption {
            host,
            agent,
            record,
            limits: self.plan.limits,
            live: false,
            managed: Some(Managed {
                home: self.home.clone(),
                executable: self.plan.executable.clone(),
            }),
            access: crate::sandbox::policy::AgentAccess::Sandboxed,
        };
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders
            .send(self.granted_turn(profile, prompt, turn_time, AgentAccess::Sandboxed))
            .unwrap();
        orders.send(Order::Stop).unwrap();
        let problem = super::native_adopted(
            adoption,
            &ordered,
            &reports,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        );
        drop(reports);
        (reported.iter().collect(), problem)
    }

    /// Starts this agent as a native-mode lead in a real worker of its team runtime, as the
    /// runtime's first dispatch to a kept session does: the project saves `access`, the
    /// worker starts from the agent's plan at that level, its first turn carries the held
    /// profile and the worker then counts as briefed. Inputs: the profile, the first prompt,
    /// the turn's time, the saved level and the folder of the lead's terminal host when the
    /// caller made one, kept until the lead stopped. Output: the running team.
    pub(super) fn into_native_worker(
        mut self,
        profile: NativeProfileUse,
        prompt: &str,
        turn_time: Duration,
        access: AgentAccess,
        folder: Option<tempfile::TempDir>,
    ) -> NativeTeam {
        self.make_native();
        crate::sandbox::policy::save(
            &self.runtime.connection,
            PROJECT,
            &AgentPolicy {
                access,
                ..AgentPolicy::default()
            },
        )
        .unwrap();
        self.plan.access = access;
        let first = self.granted_turn(profile, prompt, turn_time, access);
        let pane = self
            .plan
            .pane
            .clone()
            .expect("a native lead runs in a pane");
        let seat = Seat {
            project: ProjectId::new(PROJECT).unwrap(),
            role: self.role,
            harness: self.plan.harness.clone(),
            connection: self.binding.connection.id.clone(),
            tools: true,
            hosted: None,
        };
        let Self {
            mut runtime,
            plan,
            home,
            _root,
            ..
        } = self;
        runtime
            .run_worker(
                AGENT,
                Launch::Fresh(
                    Box::new(plan),
                    Box::new(Mode::PerTurn { home: home.clone() }),
                ),
                seat,
            )
            .unwrap();
        let worker = runtime
            .workers
            .get_mut(&AGENT)
            .expect("the lead's worker runs");
        assert!(worker.managed && worker.kept_session && !worker.briefed);
        worker.orders.send(first).unwrap();
        worker.briefed |= worker.kept_session;
        NativeTeam {
            runtime,
            home,
            pane,
            _root,
            _folder: folder,
        }
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

/// A native lead running as a real worker of its team runtime, started by
/// [`ManagedAgent::into_native_worker`]: the runtime, the lead's home, its pane on its own
/// server and the test folders. Dropping it stops the lead before the folders go, so no
/// signed-in app outlives its test. Its methods call the runtime's own admission, dispatch
/// and access-level steps; the lead's reports are read here and never reach the runtime's
/// report handling, so task rows and the worker's busy state stay as dispatch left them.
pub(super) struct NativeTeam {
    pub(super) runtime: TeamRuntime,
    pub(super) home: PathBuf,
    pane: (Arc<dyn Host>, AgentId),
    _root: tempfile::TempDir,
    _folder: Option<tempfile::TempDir>,
}

impl NativeTeam {
    /// The lead's worker as the runtime records it. Output: the worker.
    pub(super) fn lead(&self) -> &super::Worker {
        &self.runtime.workers[&AGENT]
    }

    /// Waits for the lead's reports until one matches `last`. Inputs: the longest wait and
    /// the match. Output: every report up to and with the match, or a panic that says what
    /// ended the wait when the worker ended or the wait passed first.
    pub(super) fn reports_until(
        &self,
        wait: Duration,
        last: impl Fn(&Report) -> bool,
    ) -> Vec<Report> {
        let deadline = Instant::now() + wait;
        let mut seen = Vec::new();
        let mut ended = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lead().reports.recv_timeout(left) {
                Ok(report) => {
                    let done = last(&report);
                    if let Report::Ended { problem } = &report {
                        ended = Some(problem.clone());
                    }
                    seen.push(report);
                    if done {
                        return seen;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "no awaited report from the lead in {wait:?}, after {} others",
                        seen.len()
                    )
                }
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "the lead's worker ended before the awaited report, problem {ended:?}, after {} others",
                    seen.len()
                ),
            }
        }
    }

    /// Every report of the lead through its next finished turn. Input: the longest wait.
    /// Output: the reports, the finished turn last.
    pub(super) fn until_finished(&self, wait: Duration) -> Vec<Report> {
        self.reports_until(wait, |report| matches!(report, Report::Finished { .. }))
    }

    /// Every report of the lead until its worker ended. Input: the longest wait. Output:
    /// the reports before the end and the problem the worker ended with.
    pub(super) fn until_ended(&self, wait: Duration) -> (Vec<Report>, Option<String>) {
        let mut reports = self.reports_until(wait, |report| matches!(report, Report::Ended { .. }));
        let Some(Report::Ended { problem }) = reports.pop() else {
            unreachable!("the wait ends on the worker's end");
        };
        (reports, problem)
    }

    /// Sends the lead's worker one order, as the runtime does. Input: the order.
    pub(super) fn order(&self, order: Order) {
        self.lead().orders.send(order).unwrap();
    }

    fn setup(&self) -> (StoredSetup, ModelAccess) {
        let tx = self.runtime.connection.unchecked_transaction().unwrap();
        let stored = crate::project_setup::load_setup(&tx, &ProjectId::new(PROJECT).unwrap())
            .unwrap()
            .expect("the project's setup");
        drop(tx);
        (stored, (self.runtime.access)().expect("the team's pool"))
    }

    /// Queues one owner task for the team's sub-agents through the engine's own admission.
    /// Input: the prompt. Output: the task id, or a panic with the refusal code.
    pub(super) fn queue(&self, prompt: &str) -> i64 {
        let (stored, access) = self.setup();
        let tx = self.runtime.connection.unchecked_transaction().unwrap();
        let task = crate::team::admit(
            &tx,
            &access,
            &stored,
            Some(&self.lead().harness),
            prompt,
            TaskSource::Owner,
            None,
            super::now_ms(),
        )
        .unwrap()
        .unwrap_or_else(|code| panic!("the task was refused: {code}"));
        tx.commit().unwrap();
        task
    }

    /// Runs one pass of the runtime's task dispatch, as each engine tick does.
    pub(super) fn dispatch(&mut self) {
        let (stored, access) = self.setup();
        self.runtime
            .dispatch(&stored, &access, &BTreeSet::new())
            .unwrap();
    }

    /// The status, agent and error the engine recorded for one task. Input: the task id.
    /// Output: them.
    pub(super) fn task(&self, task: i64) -> (String, Option<i64>, Option<String>) {
        self.runtime
            .connection
            .query_row(
                "SELECT status,agent,error FROM team_tasks WHERE id=?1",
                [task],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    /// The owner saves a new access level for the project, and the runtime's next tick
    /// retires every agent whose running process started at a wider one (AL-2).
    /// Input: the new level.
    pub(super) fn save_access(&mut self, access: AgentAccess) {
        crate::sandbox::policy::save(
            &self.runtime.connection,
            PROJECT,
            &AgentPolicy {
                access,
                ..AgentPolicy::default()
            },
        )
        .unwrap();
        self.runtime
            .retire_lowered(&ProjectId::new(PROJECT).unwrap());
    }

    /// Types one prompt into the lead's pane as a person at its terminal does, outside the
    /// engine's turns, so its app starts a turn by itself. Input: the prompt.
    pub(super) fn type_into_pane(&self, prompt: &str) {
        let (host, agent) = &self.pane;
        for part in paste(prompt) {
            host.input(agent, part.as_bytes()).unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        host.input(agent, b"\r").unwrap();
    }

    /// Whether the lead's pane still runs on its own server. Output: true while it does,
    /// false once it or its server ended.
    pub(super) fn pane_alive(&self) -> bool {
        let (host, agent) = &self.pane;
        host.alive().is_ok_and(|alive| alive.contains(agent))
    }
}

impl Drop for NativeTeam {
    /// Stops the lead and waits, at most a minute, for its worker, so a test that ends
    /// early leaves no signed-in app running. Takes the team. Returns nothing.
    fn drop(&mut self) {
        let Some(worker) = self.runtime.workers.get_mut(&AGENT) else {
            return;
        };
        worker.cancel.store(true, Ordering::SeqCst);
        let _ = worker.orders.send(Order::Stop);
        let Some(thread) = worker.thread.take() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}
