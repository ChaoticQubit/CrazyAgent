use super::events::{
    INSTRUCTIONS_RESTARTED, LEAD_STOPPED_AGENT, MASTER_STOPPED_AGENT, OWNER_RESTARTED,
    OWNER_STOPPED_AGENT, OWNER_WOKE,
};
use super::{
    AGENT_COLUMNS, Accounts, AgentRole, AgentStatus, DELEGATIONS_PER_REPLY_MAX, Delegation,
    MessageAuthor, MessageState, NOTE_BYTES_MAX, StoppedBy, TaskSource, TaskStatus, TeamAccount,
    TeamReach, admit, agent_row, append_message, bound_text, eligible_accounts, launch_harness,
    names, now_ms, parse_delegations, parse_wakes, pick_account, tool,
};
use crate::agent_git::Layout;
use crate::agents::{
    AgentState as RunState, Agents, Hosted, HostedAgent, HostedRequest, HostedRestart,
};
use crate::config::ModelAccess;
use crate::engine::bootstrap::{AgentCredential, NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::engine::{EngineError, NativeProfileCustody, RegisteredNativeAuthHost};
use crate::harnesses::launch::{
    AccountBinding, GitIdentity, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest,
    RUN_TIME_MAX, claude_bare,
};
use crate::harnesses::leftovers::Unstopped;
use crate::harnesses::terminal_session::{
    AgentSession, PaneRecord, Resume, TerminalSession, end_left_pane,
};
use crate::harnesses::transcript::TranscriptEvent;
use crate::launch_context::AgentContext;
use crate::master_sessions::ResumePlan;
use crate::project_setup::{StoredSetup, load_setup};
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use crate::research::runner::{
    CLAUDE_MODEL_HOSTS, CODEX_MODEL_HOSTS, ResearchAccount, RunError, add_worktree_at,
    confined_unavailable, endpoint_model_host, ensure_repository, model_remote,
};
use crate::sandbox::model_proxy::{ModelHost, ModelProxy};
use crate::sandbox::policy::{AgentAccess, AgentNetwork, AgentPolicy, Narrowing};
use crate::sandbox::toolchains::{self, ToolHomes, Toolchain, Toolchains};
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use crate::terminal::{AgentId, Host, PANES_PER_PROJECT_MAX};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const BRIEF_BYTES_MAX: usize = 6000;
const RESULT_IN_MESSAGE_BYTES_MAX: usize = 3000;
/// The most bytes one sub-agent results message holds, so neither the stored message nor
/// the text typed into a live master, with its speaker tag, ever loses a line.
const RESULTS_MESSAGE_BYTES_MAX: usize = super::MESSAGE_BYTES_MAX * 2 - 64;
const RESULTS_TRAILER: &str = "\nTell the owner what was done.";
const LEAD_RESULTS_TRAILER: &str =
    "\nMerge each finished worker into your task branch with merge(agent).";
/// What a lead hears when its worker's change set is ready, under the Lands setting Lead.
const LEAD_MERGES: &str = "Merge them into your task branch with merge(agent) once it is done.";
/// What a lead hears when its worker's change set is ready, under the Lands setting Owner.
const OWNER_ACCEPTS: &str = "The owner accepts them in Changes; you do not merge them.";
const _: () = assert!(RESULTS_MESSAGE_BYTES_MAX + 8 <= super::master::INPUT_TEXT_MAX);
const _: () = assert!(RESULT_IN_MESSAGE_BYTES_MAX * 2 + 256 < RESULTS_MESSAGE_BYTES_MAX);
const DELEGATION_ROUNDS_MAX: i64 = 3;
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
const EVENT_WAIT: Duration = Duration::from_millis(200);
const ORDER_WAIT: Duration = Duration::from_millis(100);
const IDLE_EVENT_WAIT: Duration = Duration::from_millis(5);
const JOIN_WAIT: Duration = Duration::from_secs(15);
/// How often a project's running agents are checked for an owner instruction edit under
/// the Restart and Badge choices (DEC-111 QR1).
const INSTRUCTION_CHECK_EVERY: Duration = Duration::from_secs(2);
const SWEEP_EVERY: Duration = Duration::from_secs(2);
const SESSION_EVENTS_MAX: usize = 200_000;
const HISTORY_MESSAGES_MAX: i64 = 12;
const HISTORY_MESSAGE_BYTES_MAX: usize = 1500;
const HISTORY_BYTES_MAX: usize = 6000;
pub(crate) const TEAM_HOST: &str = "team";
const SIGN_IN_REJECTED: &str = "master_sign_in_rejected";
const SIGN_IN_CHECKING: &str = "master_sign_in_checking";
const AGENT_RESTART_CHECKING: &str = "The agent did not restart yet: account sign-ins are still being checked after the restart. Restart it again in a moment.";
const LEFT_RUNNING_NOTE: &str = "Kept running while CrazyAgent was closed";
const TURN_LIMIT: &str = "team_turn_limit";
const TIME_LIMIT_NOTE: &str = "stopped by a time limit";
/// How streams, leads and their branches work, given to every master (N3).
const MASTER_NESTING_RULES: &str = "\n## Streams and leads\n\nEach agent works in its own git worktree and branch. The sub-agent you start for a stream of work leads it and may start workers of its own, up to the owner's Sub-agent depth setting. Workers' work lands on their lead's branch; the owner accepts a lead's work onto any branch. One harness per stream means one lead agent per stream, on the harness and model the owner chose or the settings default. Never ask the owner how streams share a branch.\n";
/// What a sub-agent that may lead its own workers is told with each task (N3).
const LEAD_LINES: &str = "\nYou lead this stream. Start workers of your own with spawn(task) in the crazyagent-team MCP server, each in its own worktree and branch cut from your current commit, up to the owner's Sub-agent depth setting. inspect_work() shows your workers, their work and their change sets, update_work(to, text) reaches one of them and wakes a finished one, stop(agent) stops one, and merge(agent) lands a finished worker's change set on your branch. The owner accepts your own work onto any branch. Never ask the owner how streams share a branch.";
/// What a lead's worker is told with each task (N3).
const WORKER_LINE: &str =
    "\nYou are a worker of a lead agent: your lead merges your work into its task branch.";
/// How many words of a task a harness's task branch name is made from, when the master
/// gave the task no role.
const TASK_NAME_WORDS: usize = 6;
const LEAD_UNREAD: &str = "Not answered: the team state could not be read. Try again in a moment.";

/// Which of the owner's time limits bounds an agent's turn, with the limit as the owner set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnBound {
    Task(Duration),
    Session(Duration),
}
const EMPTY_REPLY_NOTE: &str = "The master replied with no text, so no work was handed out. Send the message again, or hand a task to a sub-agent yourself.";
/// The one turn a sub-agent gets when its task turn ended with no reply text and the
/// project's re-ask setting is on (DEC-111): the master asks it once for its reply.
const REASK_EMPTY: &str = "Your master got no reply text from your last turn. Reply now with a short summary of what you did for your task.";

/// Bounds every team agent obeys. The task, session and sub-agent idle limits are the
/// owner's team settings, read per project when a turn or an agent starts or an idle agent
/// is checked, so none is set here. `output_bytes` caps the harness output of one process,
/// `attempts_max` the tries per task, `master_idle_stop` the bound for an idle master, which
/// the project's choice scales, once no work is in flight and `rate_limit_pause` how long an account that hit its limit
/// is skipped.
#[derive(Debug, Clone, Copy)]
pub struct TeamLimits {
    pub output_bytes: usize,
    pub attempts_max: i64,
    pub master_idle_stop: Duration,
    pub rate_limit_pause: Duration,
    pub stop_grace: Duration,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            output_bytes: 8 * 1024 * 1024,
            attempts_max: 2,
            master_idle_stop: Duration::from_secs(super::MASTER_IDLE_STOP_SECS.into()),
            rate_limit_pause: Duration::from_secs(10 * 60),
            stop_grace: Duration::from_secs(2),
        }
    }
}

impl TeamLimits {
    fn validate(&self) -> Result<(), EngineError> {
        let ok = (1..=64 * 1024 * 1024).contains(&self.output_bytes)
            && (1..=10).contains(&self.attempts_max)
            && !self.master_idle_stop.is_zero()
            && self.stop_grace <= Duration::from_secs(30);
        ok.then_some(())
            .ok_or_else(|| EngineError("team_limits_invalid".into()))
    }
}

/// What a worker thread is told to do next: start a live master with the instructions its
/// harness reads at launch and the signed-in account it needs, run one turn, with its
/// prompt, the signed-in account it needs and the owner's rules for it, take the owner's
/// current rules for the turn a kept lead's app runs by itself, or stop.
enum Order {
    Start(String, Option<Box<Grant>>),
    Turn(String, Option<Box<Grant>>, TurnRules),
    Rules(TurnRules),
    Stop,
}

/// The owner's rules for one turn: its task time limit (none for no limit) and what it does
/// at its end while commands its agent started in the background still run (DEC-111 Q13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TurnRules {
    limit: Option<Duration>,
    background: super::BackgroundAtEnd,
}

/// What one turn of an agent on a signed-in managed account needs: the profile, held
/// under the engine's custody lock for that turn only, the project's launch context, and
/// the access level the owner has saved now, read again for every turn (AL-2).
struct Grant {
    profile: NativeProfileUse,
    context: AgentContext,
    access: AgentAccess,
}

/// Everything about an agent's harness launch except the account, so a managed agent
/// can start a fresh process for each turn. `pane` is the terminal host and the agent's
/// pane name when the agent runs its interactive app in a pane; `resume` the harness
/// conversation a live master continues instead of starting one; `toolchains` the families
/// its sandbox profile, built once for every turn, lets it use; `native` marks a
/// native-mode lead (DEC-104), which runs in a tmux server of its own inside its profile,
/// and `tmux` is that server's program, which its teammates are split off with; `hookless`
/// marks a launch Claude Code starts `--bare`, which fires no hooks (DEC-104 bare-hooks);
/// `scratch_pin` is the agent's short temp folder as the engine made it (DEC-111 S1R1-1);
/// `proxy` is the model proxy an agent with tools under the None network reaches its model
/// through, the only network its profile allows (S4b); `network` is the project network its
/// profile was made for, which a narrower saved choice stops it for (CS-2); `claude_display`
/// is the project's Claude Code terminal display, read when the agent is prepared (N-D9);
/// `task_start` is a sub-agent's or master's Task-start reminder from its launch context
/// (T3-15, DEC-116).
struct Plan {
    harness: String,
    executable: PathBuf,
    working_dir: PathBuf,
    model: Option<String>,
    tools: bool,
    sandbox_profile: PathBuf,
    access: AgentAccess,
    toolchains: Vec<Toolchain>,
    network: AgentNetwork,
    layout: Layout,
    git_identity: Option<GitIdentity>,
    limits: LaunchLimits,
    pane: Option<(Arc<dyn Host>, AgentId)>,
    live: bool,
    resume: Option<Resume>,
    continues: bool,
    native: bool,
    tmux: Option<PathBuf>,
    hookless: bool,
    #[cfg(unix)]
    scratch_pin: Option<(u64, u64)>,
    proxy: Option<Arc<ModelProxy>>,
    claude_display: crate::coord::settings::ClaudeDisplay,
    task_start: Option<String>,
}

/// Why a native-mode lead does not start at Full access: its own tmux server, its teammate
/// panes and the engine's client on that server must all run inside its Seatbelt profile.
const NATIVE_NEEDS_SANDBOX: &str = "Native teammates need a sandboxed lead. Choose Sandboxed or Own branch for this project, or start sub-agents from the app.";

/// Why a lead Claude Code starts `--bare` does not run native teammates: no hook fires there,
/// so the relay that counts, guards and reports its teammates never runs (DEC-104 bare-hooks).
const NATIVE_NEEDS_HOOKS: &str = "Native teammates need hooks, and this account starts Claude Code without them. Switch Sub-agents to App-spawned, or use a signed-in account.";

/// Why an agent with tools that Claude Code starts `--bare` does not start while Overlaps is
/// Block: the edit guard is a hook, and no hook fires there (DEC-104 bare-hooks).
const OVERLAP_BLOCK_NEEDS_HOOKS: &str = "Overlaps set to Block or Wait needs hooks, and this account starts Claude Code without them. Set Overlaps to Warn, or use a signed-in account.";

impl Plan {
    /// Gives a native-mode lead's launch context its agent team (DEC-104 K5a): agent teams on,
    /// `--teammate-mode tmux`, tmux on its PATH and the teammates' instructions. A plan with a
    /// model proxy also points the launch's variables at it (S4b). Every plan gives its
    /// Claude Code terminal the project's display (N-D9). Takes the context. Returns it.
    fn team_context(&self, context: AgentContext) -> AgentContext {
        assert_eq!(
            self.native,
            self.tmux.is_some(),
            "a native plan carries its tmux"
        );
        let context = context.with_claude_display(self.claude_display);
        let context = match &self.proxy {
            Some(proxy) => context.with_model_proxy(proxy.port()),
            None => context,
        };
        match &self.tmux {
            Some(tmux) => context.with_native_team(tmux, &self.working_dir),
            None => context,
        }
    }

    /// Builds one start of the plan at `access`, the level the owner saved when this start
    /// happens (AL-2): Full access runs without Seatbelt, any other level in the agent's
    /// profile, and a clone at Sandboxed is refused, see [`confinement`]. Takes the account
    /// and the level. Returns the request, or the plain refusal.
    fn request(
        &self,
        account: AccountBinding,
        access: AgentAccess,
    ) -> Result<LaunchRequest, String> {
        let unconfined = confinement(self.layout, access)?;
        if self.native && unconfined {
            return Err(with_code(
                NATIVE_NEEDS_SANDBOX,
                Some("team_native_needs_sandbox"),
            ));
        }
        assert_eq!(
            self.hookless,
            crate::coord::settings::HarnessKind::from_harness(&self.harness)
                == Some(crate::coord::settings::HarnessKind::ClaudeCode)
                && claude_bare(&account),
            "an agent's recorded hooks state is the one its launch gets"
        );
        Ok(LaunchRequest {
            harness: self.harness.clone(),
            executable: self.executable.clone(),
            working_dir: self.working_dir.clone(),
            account,
            model: self.model.clone(),
            tools: self.tools,
            sandbox_profile: (!unconfined).then(|| self.sandbox_profile.clone()),
            unconfined,
            git_identity: self.git_identity.clone(),
            limits: self.limits,
        })
    }
}

/// Decides how an agent in `layout` starts at `access`, the one rule every launch, wake and
/// per-turn start follows: a clone at Sandboxed is refused, since the owner lowered the
/// level after the agent began on its own branch and its changes still wait in Changes;
/// Full access runs without Seatbelt; every other pair runs in the agent's profile.
/// Inputs: the agent's layout and the saved level. Output: true to run unconfined, or the
/// plain refusal with `agent_access_lowered`.
fn confinement(layout: Layout, access: AgentAccess) -> Result<bool, String> {
    match (layout, access) {
        (Layout::Clone, AgentAccess::Sandboxed) => Err(with_code(
            "this agent started with its own branch, but the project is Sandboxed now; its changes are still in Changes",
            Some("agent_access_lowered"),
        )),
        (_, AgentAccess::Full) => Ok(true),
        (_, AgentAccess::Sandboxed | AgentAccess::OwnBranch) => Ok(false),
    }
}

/// How an agent's process lives: kept across turns on an endpoint account, or started
/// for each turn on a managed profile whose home is given.
enum Mode {
    Kept {
        account: AccountBinding,
        context: Box<AgentContext>,
    },
    PerTurn {
        home: PathBuf,
    },
}

/// How an agent's thread gets its harness: a fresh launch from its plan, or the pane of
/// an agent a previous engine left running, taken over from the pane's stored record.
enum Launch {
    Fresh(Box<Plan>, Box<Mode>),
    Adopt(Box<Adoption>),
}

impl Launch {
    /// Names the model proxy the launch's agent reaches its model through, which its worker
    /// thread holds until the thread ends (S4b). Takes the launch. Returns the proxy, if any.
    fn proxy(&self) -> Option<Arc<ModelProxy>> {
        match self {
            Launch::Fresh(plan, _) => plan.proxy.clone(),
            Launch::Adopt(adoption) => adoption.proxy.clone(),
        }
    }

    /// Gives the Task-start reminder the launch's sub-agent gets with each task, which its
    /// teammates get too (T3-15), and its master with each owner message (DEC-116). Takes the
    /// launch. Returns the text, if any.
    fn task_start(&self) -> Option<String> {
        match self {
            Launch::Fresh(plan, _) => plan.task_start.clone(),
            Launch::Adopt(adoption) => adoption.task_start.clone(),
        }
    }
}

/// A pane an earlier engine left running, taken over from its record: the host and pane
/// name, the record, the bounds, whether it is a live master, what a signed-in account needs
/// again, the access level it started at, the project network its profile was made for, its
/// model proxy, bound again on the port its profile names (S4b), and a sub-agent's Task-start
/// reminder from its launch context as it reads now (T3-15).
struct Adoption {
    host: Arc<dyn Host>,
    agent: AgentId,
    record: PaneRecord,
    limits: LaunchLimits,
    live: bool,
    managed: Option<Managed>,
    access: AgentAccess,
    toolchains: Option<Vec<Toolchain>>,
    network: AgentNetwork,
    proxy: Option<Arc<ModelProxy>>,
    task_start: Option<String>,
}

/// What a live master on a signed-in account needs when a later engine takes its pane over:
/// its private home, where the credential copy goes again, and the harness executable that
/// renews it.
struct Managed {
    home: PathBuf,
    executable: PathBuf,
}

/// Who one agent is in its team: the project, role, team harness, account and tools
/// switch, with its entry in the engine's agent register when there is one.
struct Seat {
    project: ProjectId,
    role: AgentRole,
    harness: String,
    connection: CredentialRef,
    tools: bool,
    hosted: Option<Hosted>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnEnd {
    Success,
    Failed,
    RateLimited,
    TimedOut,
    TooLong,
    Cancelled,
    Interrupted,
    Closed,
}

impl TurnEnd {
    /// The stored word for how a live master turn ended.
    /// Input: the turn end. Output: a fixed lowercase word.
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::RateLimited => "rate_limited",
            Self::TimedOut => "timed_out",
            Self::TooLong => "too_long",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Closed => "closed",
        }
    }
}

enum Report {
    Ready {
        group: u32,
        pane: Option<PaneRecord>,
    },
    Progress {
        bytes: usize,
    },
    Usage {
        tokens: crate::usage::capture::Tokens,
    },
    Finished {
        text: String,
        end: TurnEnd,
        cause: Option<String>,
    },
    Ended {
        problem: Option<String>,
    },
    /// The harness's own stop left commands it started running, listed, and nothing else
    /// ended it (TEAM-8-L1); sent just before `Ended`.
    LeftRunning,
    Turn {
        end: Option<TurnEnd>,
        text: String,
    },
    Own {
        end: Option<TurnEnd>,
        text: String,
    },
    Conversation(Resume),
    ResumeRefused,
    TakeoverUnconfirmed,
    InputReady,
    Held {
        held: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Task(i64),
    Message(i64),
}

/// Why the engine stopped an agent whose running process holds more than the owner saved:
/// what the saved policy no longer allows, or toolchains a pane from before the engine kept
/// them may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Narrowed {
    Saved(Narrowing),
    Unrecorded,
}

impl Narrowed {
    /// The plain reason the stopped agent shows, with its code. Input: the cause.
    /// Output: the sentence.
    fn problem(self) -> String {
        let (text, code) = match self {
            Self::Saved(Narrowing::Access) => (
                "the project's agent access was lowered, so the agent was stopped; its next start uses the new level",
                Narrowing::Access.code(),
            ),
            Self::Saved(Narrowing::Network) => (
                "the project's agent network was narrowed, so the agent was stopped; its next start uses the new network",
                Narrowing::Network.code(),
            ),
            Self::Saved(Narrowing::TurnedOff) => (
                "the project's toolchain access was turned off, so the agent was stopped; its next start goes without it",
                Narrowing::TurnedOff.code(),
            ),
            Self::Saved(Narrowing::NotAllowed) => (
                "a toolchain the agent started with is no longer allowed for the project (turned off, or no longer used by the project), so the agent was stopped; its next start goes without it",
                Narrowing::NotAllowed.code(),
            ),
            Self::Saved(Narrowing::PolicyUnreadable) => (
                "the project's agent access could not be read, so the agent was stopped; it starts again once it can be read",
                Narrowing::PolicyUnreadable.code(),
            ),
            Self::Saved(Narrowing::FolderUnreadable) => (
                "the project folder could not be read to check the agent's toolchains, so the agent was stopped",
                Narrowing::FolderUnreadable.code(),
            ),
            Self::Unrecorded => (
                "the agent started before the engine kept the toolchains it was given, so it was stopped; its next start uses the project's toolchain settings",
                "agent_toolchains_unrecorded",
            ),
        };
        with_code(text, Some(code))
    }
}

struct Worker {
    project: ProjectId,
    role: AgentRole,
    harness: String,
    connection: CredentialRef,
    orders: Sender<Order>,
    reports: Receiver<Report>,
    cancel: Arc<AtomicBool>,
    leave: Arc<AtomicBool>,
    busy: Option<Busy>,
    idle_since: Instant,
    stopping: bool,
    held: bool,
    restarting: bool,
    warm: bool,
    worked: bool,
    ended_well: bool,
    left_running: bool,
    managed: bool,
    tools: bool,
    team_tool: bool,
    ask_tool: bool,
    grants: u64,
    thread: Option<JoinHandle<()>>,
    hosted: Option<Hosted>,
    live: bool,
    briefed: bool,
    input_ready: bool,
    background_held: bool,
    kept_session: bool,
    fresh: Option<i64>,
    own_turn: Option<i64>,
    own_rules: Option<(Duration, TurnRules)>,
    turn_started: Option<i64>,
    session: Option<String>,
    seed: Option<String>,
    bound: Option<TurnBound>,
    spawned: Instant,
    handoff: Option<String>,
    access: AgentAccess,
    network: AgentNetwork,
    ran_before: Duration,
    told_finished: BTreeSet<i64>,
    parent: Option<i64>,
    ready: bool,
    toolchains: Option<Vec<Toolchain>>,
    checked: Option<AgentPolicy>,
    narrowed: Option<Narrowed>,
    reask: Option<i64>,
    reasked: Option<i64>,
    starts: crate::coord::relay::TaskStarts,
}

impl Worker {
    /// How long the agent has run toward its session: since its terminal started, including
    /// the time before an engine restart adopted it. Takes the worker. Returns the time.
    fn ran(&self) -> Duration {
        self.ran_before + self.spawned.elapsed()
    }
}

/// Reads the live model access on every tick; none means no pool is configured.
pub type AccessSource = Box<dyn Fn() -> Option<ModelAccess> + Send>;

/// Runs each project's team: the master on the team plan's master harness and
/// sub-agents on its sub-harnesses, each in the project sandbox, sub-agents in
/// their own git worktrees. The engine admits every task; agents only propose.
pub struct TeamRuntime {
    connection: Connection,
    root: PathBuf,
    state: PathBuf,
    executables: BTreeMap<&'static str, PathBuf>,
    access: AccessSource,
    settings: Option<(PathBuf, PathBuf)>,
    limits: TeamLimits,
    workers: BTreeMap<i64, Worker>,
    custody: Result<NativeProfileCustody, EngineError>,
    delegation_tool: Option<RegisteredNativeAuthHost>,
    agents: Option<Arc<Agents>>,
    terminal: Option<Arc<dyn Host>>,
    swept: Option<Instant>,
    events: Option<super::master::TeamEvents>,
    watch: super::events::Watch,
    master_watch: super::screen_questions::MasterWatch,
    teammates_next: Option<Instant>,
    teammate_worktree_tries: BTreeMap<i64, BTreeMap<String, u8>>,
    teammates_rested: BTreeMap<i64, BTreeSet<i64>>,
    teammate_turn_tries: BTreeMap<i64, BTreeMap<i64, u8>>,
    teammates_unreadable_told: BTreeSet<i64>,
    teammate_conflicts_told: BTreeMap<i64, BTreeMap<String, (String, String)>>,
    receipts_next: BTreeMap<ProjectId, Instant>,
    live_starts: BTreeMap<ProjectId, Vec<Instant>>,
    unread: BTreeMap<ProjectId, (i64, Instant)>,
    resuming: Option<(i64, Resume)>,
    settled: bool,
    naming: Option<i64>,
    deliveries: crate::coord::delivery::Deliveries,
    adoption_failed: Option<String>,
    claims_next: BTreeMap<ProjectId, Instant>,
    claims_failed: Option<String>,
    reproving: Option<Arc<AtomicBool>>,
    instructions_next: BTreeMap<ProjectId, Instant>,
    instruction_restarts: BTreeMap<i64, ProjectId>,
    master_restarts: BTreeSet<ProjectId>,
    restarted_for: BTreeMap<i64, u64>,
    waited_for: BTreeMap<i64, u64>,
    #[cfg(test)]
    grant_waited: BTreeMap<i64, String>,
    #[cfg(unix)]
    callers: BTreeMap<i64, Option<super::caller::Callers>>,
    #[cfg(unix)]
    caller_folders: BTreeMap<i64, (u64, u64)>,
    #[cfg(unix)]
    caller_pins: BTreeMap<i64, Vec<(String, u32)>>,
    held_restarts: Vec<HostedRestart>,
    unmanaged_ending: BTreeMap<i64, PendingEnd>,
    unmanaged_ends: (Sender<PaneEnd>, Receiver<PaneEnd>),
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// One running task an engine step puts back in the queue or cancels: the task, its
/// project and agent, and whether the owner asked to cancel it.
struct LeftTask {
    task: i64,
    project: ProjectId,
    agent: i64,
    cancel: bool,
}

/// The most running tasks one engine step reads to tell how they ended; far past every
/// project's agent cap together, so it never cuts a real step short.
const LEFT_TASKS_READ: usize = 1024;

/// Reads the running tasks with an agent that `filter` names ("1" for all of them) before
/// the caller puts them back in the queue or cancels them, since the queue clears their
/// agent, so each one's activity line can still name it. Takes the connection, the SQL
/// filter on team_tasks and its parameters. Returns the tasks, or the SQLite failure.
fn running_tasks(
    connection: &Connection,
    filter: &'static str,
    values: impl rusqlite::Params,
) -> Result<Vec<LeftTask>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT id,project,agent,cancel_requested FROM team_tasks
             WHERE status='running' AND agent IS NOT NULL AND ({filter})
             ORDER BY id LIMIT {LEFT_TASKS_READ}"
        ))
        .map_err(error)?;
    let rows: Vec<(i64, String, i64, bool)> = statement
        .query_map(values, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= LEFT_TASKS_READ);
    Ok(rows
        .into_iter()
        .filter_map(|(task, project, agent, cancel)| {
            let project = ProjectId::new(project).ok()?;
            Some(LeftTask {
                task,
                project,
                agent,
                cancel,
            })
        })
        .collect())
}

/// Tells the project's activity how each task in `left` left its agent: stopped when the
/// owner asked to cancel it, else stopped and back in the queue. Takes the connection, the
/// tasks and the time in ms.
fn note_left_tasks(connection: &Connection, left: &[LeftTask], now: i64) {
    for item in left {
        assert!(item.task > 0);
        let (project, requeued) = (&item.project, !item.cancel);
        crate::activity::note_task_stop(connection, project, item.agent, item.task, requeued, now);
    }
}

/// How one agent start ended: started, held back because a projects home move holds the
/// project folder, so the work that asked for it waits for the move to end with nothing
/// spent or switched off, or refused for the plain reason.
#[derive(Debug, PartialEq, Eq)]
enum Start<T> {
    Started(T),
    Held,
    Refused(String),
}

/// The most stopped masters of one project the last-master lookup reads.
const LAST_MASTERS_READ: i64 = 64;

/// Finds the project's newest stopped or failed master on a harness with its worktree still
/// recorded, so it starts again in its own worktree and home. The harness matches by catalog
/// entry, so a master recorded under an alias ("claude") serves a plan that names the entry
/// ("claude-code"). Takes the database, project and harness. Returns the agent, or None.
pub(super) fn last_master(
    connection: &Connection,
    project: &ProjectId,
    harness: &str,
) -> Result<Option<i64>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT a.id,CASE WHEN length(a.harness)<=64 THEN a.harness END FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND a.role='master' AND a.status IN ('stopped','failed')
             ORDER BY a.id DESC LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), LAST_MASTERS_READ], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(rows.len() <= LAST_MASTERS_READ as usize);
    Ok(rows.into_iter().find_map(|(id, stored)| {
        stored
            .is_some_and(|stored| crate::harnesses::same(&stored, harness))
            .then_some(id)
    }))
}

/// Refuses team state where no 0700 directory can be made or checked.
/// Takes the path, unused. Returns `team_state_unavailable`.
#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), EngineError> {
    Err(EngineError("team_state_unavailable".into()))
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), EngineError> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(error(e)),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(error)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(EngineError("team_state_not_private".into()));
    }
    Ok(())
}

/// Makes the home a native lead's teammates run with (TPH-2 C): a private folder beside the
/// lead's home, with its own cargo home and Claude Code's first-run questions answered for
/// the project, whose `teams` and `tasks` are links to the lead's folders of those names,
/// made here when missing, since Claude Code keeps the team's mailboxes and task lists there
/// for every member. A changed entry is refused, not replaced. Inputs: the lead's home, the
/// owner's tool homes and the lead's working folder. Output: the folder.
#[cfg(unix)]
fn prepare_mates_home(
    home: &Path,
    homes: &ToolHomes,
    project: &Path,
) -> Result<PathBuf, EngineError> {
    let mates = crate::launch_context::mates_home(home);
    private_directory(&mates)?;
    for folder in crate::sandbox::CLAUDE_TEAM_SHARED {
        let shared = home.join(folder);
        match std::fs::DirBuilder::new().mode(0o700).create(&shared) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
        let link = mates.join(folder);
        match std::os::unix::fs::symlink(&shared, &link) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
        if !std::fs::symlink_metadata(&shared).is_ok_and(|metadata| metadata.is_dir())
            || std::fs::read_link(&link).ok().as_deref() != Some(shared.as_path())
        {
            return Err(EngineError("team_shared_folder_changed".into()));
        }
    }
    toolchains::prepare_cargo_home(&mates, homes).map_err(error)?;
    crate::harnesses::interactive::prepare_claude_home(&mates, project)
        .map_err(|e| EngineError(e.0))?;
    Ok(mates)
}

impl TeamRuntime {
    /// Opens the team runtime over the engine database, with the usage tables its turn ends
    /// write; usage tables that cannot be made are logged by code and leave usage uncounted.
    /// Takes the database, an engine-owned state directory outside every project, the
    /// launch executables by harness, a source of live model access and the limits.
    /// Returns the runtime after returning work interrupted by an earlier engine to the queue.
    pub fn open(
        database: &Path,
        state: &Path,
        executables: BTreeMap<&'static str, PathBuf>,
        access: AccessSource,
        limits: TeamLimits,
    ) -> Result<Self, EngineError> {
        limits.validate()?;
        if !state.is_absolute() {
            return Err(EngineError("team_state_not_canonical".into()));
        }
        private_directory(state)?;
        if state.canonicalize().map_err(error)? != state {
            return Err(EngineError("team_state_not_canonical".into()));
        }
        for directory in ["agents", "profiles"] {
            private_directory(&state.join(directory))?;
        }
        for (harness, executable) in &executables {
            if launch_harness(harness) != Some(*harness) || !executable.is_absolute() {
                return Err(EngineError("team_executable_invalid".into()));
            }
        }
        let mut connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(error)?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(error)?;
        super::initialize(&connection)?;
        crate::project_setup::initialize(&connection)?;
        if let Err(problem) = crate::usage::initialize(&connection) {
            eprintln!(
                "team usage is not counted: {}",
                crate::usage::service::failure_code(&problem)
            );
        }
        crate::recovery::capture(&connection, None)?;
        let now = now_ms();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let left = running_tasks(&tx, "1", ())?;
        tx.execute_batch(&format!(
            "UPDATE team_agents SET status='stopped', task=NULL, error='Stopped when CrazyAgent closed', stopped_ms={now}
               WHERE status IN ('starting','idle','working');
             UPDATE team_tasks SET status='queued', agent=NULL, cancel_requested=0
               WHERE status='running' AND cancel_requested=0;
             UPDATE team_tasks SET status='cancelled', agent=NULL, error='Cancelled by you', finished_ms={now}
               WHERE status='running' AND cancel_requested=1;
             UPDATE team_messages SET state='pending' WHERE state='sent';"
        ))
        .map_err(error)?;
        note_left_tasks(&tx, &left, now);
        tx.commit().map_err(error)?;
        delivery::settle(&connection)?;
        let custody = std::fs::symlink_metadata(database)
            .map_err(error)
            .and_then(|metadata| NativeProfileCustody::capture(database, &metadata));
        let root = database
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| EngineError("team_state_not_canonical".into()))?;
        spawn_ledger::recover(&connection, &root);
        Ok(Self {
            connection,
            root,
            state: state.to_path_buf(),
            executables,
            access,
            settings: None,
            limits,
            workers: BTreeMap::new(),
            custody,
            delegation_tool: None,
            agents: None,
            terminal: None,
            swept: None,
            events: None,
            watch: super::events::Watch::default(),
            master_watch: super::screen_questions::MasterWatch::default(),
            teammates_next: None,
            teammate_worktree_tries: BTreeMap::new(),
            teammates_rested: BTreeMap::new(),
            teammate_turn_tries: BTreeMap::new(),
            teammates_unreadable_told: BTreeSet::new(),
            teammate_conflicts_told: BTreeMap::new(),
            receipts_next: BTreeMap::new(),
            live_starts: BTreeMap::new(),
            unread: BTreeMap::new(),
            resuming: None,
            settled: false,
            naming: None,
            deliveries: crate::coord::delivery::Deliveries::default(),
            #[cfg(unix)]
            callers: BTreeMap::new(),
            #[cfg(unix)]
            caller_folders: BTreeMap::new(),
            #[cfg(unix)]
            caller_pins: BTreeMap::new(),
            adoption_failed: None,
            claims_next: BTreeMap::new(),
            claims_failed: None,
            reproving: None,
            instructions_next: BTreeMap::new(),
            instruction_restarts: BTreeMap::new(),
            master_restarts: BTreeSet::new(),
            restarted_for: BTreeMap::new(),
            held_restarts: Vec::new(),
            unmanaged_ending: BTreeMap::new(),
            unmanaged_ends: channel(),
            waited_for: BTreeMap::new(),
            #[cfg(test)]
            grant_waited: BTreeMap::new(),
        })
    }

    /// Lets the runtime wait for an account whose sign-in this engine run still re-checks
    /// instead of refusing it (DEC-108). Takes the engine-start re-proof's flag, true while
    /// that re-proof runs. Returns the runtime; without it no account ever waits.
    pub fn with_reproof(mut self, reproving: Arc<AtomicBool>) -> Self {
        self.reproving = Some(reproving);
        self
    }

    /// Records every team agent in the engine's agent register, so the owner can stop,
    /// interrupt and restart each one with every other agent.
    /// Takes the register of the same engine database. Returns the runtime.
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Gives a live master's `message`, `answer` and `status` tools their team event
    /// handlers. Takes the handlers. Returns the runtime; without them those tools answer
    /// that team messaging is not available.
    pub fn with_events(mut self, events: super::master::TeamEvents) -> Self {
        self.events = Some(events);
        self
    }

    /// Reads the owner's configured local model ports from these two settings files at each
    /// tools-on launch, so the agent cannot bind them, and refuses the launch when they do
    /// not read (OWNER-PORTS-FAIL-OPEN). Without them the ports come from the model access
    /// source. Takes the global and project settings files. Returns the runtime.
    pub fn with_settings(mut self, global: &Path, project: &Path) -> Self {
        self.settings = Some((global.to_path_buf(), project.to_path_buf()));
        self
    }

    /// Runs every team agent whose harness has an interactive app in a pane of `host` named
    /// `team-<id>`, where the owner can watch and type. Panes an earlier engine left are
    /// taken over, oldest agent first, when their agent is still in the team plan on a usable
    /// account, so the agent goes on without a relaunch; every other team pane ends and its
    /// project hears why. Takes the host. Returns the runtime, or the host's plain code.
    pub fn with_terminal(mut self, host: Arc<dyn Host>) -> Result<Self, EngineError> {
        let records = self.pane_records()?;
        self.terminal = Some(host.clone());
        let access = (self.access)().unwrap_or_default();
        let panes: BTreeMap<i64, AgentId> = host
            .alive()?
            .into_iter()
            .filter_map(|agent| agent.team_agent().map(|id| (id, agent)))
            .collect();
        spawn_ledger::recover_panes(&self.root, &records, &panes);
        for (id, agent) in panes {
            let refusal = match records.get(&id) {
                Some(record) => self.adopt(&host, id, record, &access)?.err(),
                None => Some("the engine has no record of it".to_owned()),
            };
            if let Some(reason) = refusal {
                match end_left_pane(host.as_ref(), &agent, "team") {
                    Ok(_) => {}
                    Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                    Err(problem) => return Err(problem.into()),
                }
                if let Some(record) = records.get(&id) {
                    record.remove_context_files();
                }
                self.closed_note(id, &reason)?;
            }
        }
        Ok(self)
    }

    /// Reads and forgets the pane records an earlier engine kept, so each is used once.
    /// Returns the records by team agent id; malformed ones are dropped.
    fn pane_records(&mut self) -> Result<BTreeMap<i64, PaneRecord>, EngineError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let rows: Vec<(i64, String)> = {
            let mut statement = tx
                .prepare("SELECT agent,record FROM team_panes")
                .map_err(error)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        tx.execute("DELETE FROM team_panes", []).map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, text)| PaneRecord::decode(&text).map(|record| (id, record)))
            .collect())
    }

    /// Takes over one live team pane when its agent may still run: the project's setup and
    /// team plan still hold its role and harness, the team is not paused, the project has
    /// room, and its account is still in the project's pool, as an endpoint, or as a
    /// signed-in account for a live master or a native-mode lead, whose worker hands the
    /// credential to its home again before it serves. The pane keeps the access level its
    /// record says it started at, Full access for a record without one, and the toolchains it
    /// started with, every family for a record without them, so a level lowered or a
    /// toolchain turned off while no engine ran stops it on the next tick. A pane with tools
    /// whose record names a wider network than the saved one, Open for a record without one,
    /// is not taken over (CS-2). Takes the host, the
    /// agent id, its pane record and the live access. Returns Ok, or why the pane must end.
    fn adopt(
        &mut self,
        host: &Arc<dyn Host>,
        id: i64,
        record: &PaneRecord,
        access: &ModelAccess,
    ) -> Result<Result<(), String>, EngineError> {
        let row: Option<(String, String, String, bool, String)> = self
            .connection
            .query_row(
                "SELECT project,role,harness,tools,connection FROM team_agents WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(error)?;
        let Some((project, role, harness, tools, connection)) = row else {
            return Ok(Err("the engine has no record of it".into()));
        };
        let Some(launch) = launch_harness(&harness) else {
            return Ok(Err("its harness cannot run team agents".into()));
        };
        let (Ok(project), Some(role), Ok(connection)) = (
            ProjectId::new(project),
            AgentRole::parse(&role),
            CredentialRef::new(connection),
        ) else {
            return Ok(Err("its record is not valid".into()));
        };
        let Some(stored) = self.stored(&project)? else {
            return Ok(Err("its project was removed".into()));
        };
        if super::paused(&self.connection, &project)? {
            return Ok(Err("the team is paused".into()));
        }
        let in_plan = match role {
            AgentRole::Master => {
                crate::harnesses::same(&stored.team.master, &harness)
                    && stored.team.master_tools == tools
                    && self.master(&project).is_none()
            }
            AgentRole::Sub => super::slot(&stored.team, &harness).is_some_and(|slot| {
                slot.tools == tools
                    && self.live(&project, AgentRole::Sub, Some(&harness))
                        < usize::from(slot.agents)
                    && self.live(&project, AgentRole::Sub, None)
                        < usize::from(stored.team.max_agents.saturating_sub(1))
            }),
            AgentRole::Teammate => false,
        };
        if !in_plan {
            return Ok(Err("the team plan no longer has room for it".into()));
        }
        let saved = crate::sandbox::policy::load(&self.connection, project.as_str())
            .map_or(AgentNetwork::None, |policy| policy.network);
        let started = record.network.unwrap_or(AgentNetwork::Open);
        if tools && started.wider_than(saved) {
            return Ok(Err(
                "it started under a wider network than the project allows now".into(),
            ));
        }
        if self.panes(&project) >= PANES_PER_PROJECT_MAX {
            return Ok(Err(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents"
            )));
        }
        let live = self.live_master(&project, role, launch);
        let native = !live && matches!(self.native_lead(&project, id, role, launch), Ok(true));
        let account_reach = match eligible_accounts(access, &project, launch) {
            Accounts::Usable(accounts) => accounts
                .iter()
                .find(|account| account.connection == connection)
                .map(|account| account.reach.clone()),
            _ => None,
        };
        let reach = account_reach
            .as_ref()
            .map(|reach| matches!(reach, TeamReach::Endpoint { .. }));
        let managed = match reach {
            Some(true) => None,
            Some(false) if live || native => match self.executables.get(launch) {
                Some(executable) => Some(Managed {
                    home: self.agent_folder(id).join("home"),
                    executable: executable.clone(),
                }),
                None => return Ok(Err("its harness is not installed".into())),
            },
            Some(false) => {
                return Ok(Err(
                    "its signed-in account starts it again for each turn".into()
                ));
            }
            None => return Ok(Err("its account is no longer in the project's pool".into())),
        };
        let proxy = match (record.proxy, &account_reach) {
            (None, _) => None,
            (Some(port), Some(reach)) => {
                let hosts = match model_hosts(launch, reach) {
                    Ok(hosts) => hosts,
                    Err(problem) => return Ok(Err(problem)),
                };
                match ModelProxy::bind(id, hosts, Some(port)) {
                    Ok(proxy) => Some(Arc::new(proxy)),
                    Err(_) => return Ok(Err(format!("its model proxy port {port} is taken"))),
                }
            }
            (Some(_), None) => {
                return Ok(Err("its account is no longer in the project's pool".into()));
            }
        };
        let Ok(agent) = AgentId::team(id) else {
            return Ok(Err("its record is not valid".into()));
        };
        let hosted = match self.host(&stored, id, role, launch, &connection, None) {
            Ok(hosted) => hosted,
            Err(problem) => return Ok(Err(problem)),
        };
        self.connection
            .execute(
                "UPDATE team_agents SET status='starting', task=NULL, error=NULL, stopped_ms=NULL, heartbeat_ms=?2 WHERE id=?1",
                params![id, now_ms()],
            )
            .map_err(error)?;
        let task_start = match role {
            AgentRole::Sub | AgentRole::Master => self
                .agent_layout(&project, id)
                .ok()
                .and_then(|layout| self.context(&project, id, role, tools, layout).ok())
                .and_then(|context| context.task_start(launch, tools)),
            AgentRole::Teammate => None,
        };
        let adoption = Adoption {
            host: host.clone(),
            agent,
            record: record.clone(),
            limits: self.launch_limits(),
            live,
            managed,
            access: record.access.unwrap_or(AgentAccess::Full),
            toolchains: record.toolchains.clone(),
            network: record.network.unwrap_or(AgentNetwork::Open),
            proxy,
            task_start,
        };
        let session = if live {
            crate::master_sessions::live_session(&self.connection, &project)?
        } else {
            None
        };
        self.run_worker(
            id,
            Launch::Adopt(Box::new(adoption)),
            Seat {
                project,
                role,
                harness,
                connection,
                tools,
                hosted,
            },
        )?;
        self.workers.get_mut(&id).expect("adopted worker").session = session;
        Ok(Ok(()))
    }

    /// Tells the agent's project that its terminal was closed and why, or the engine log
    /// when the agent is not known. Takes the team agent id and the plain reason.
    fn closed_note(&mut self, id: i64, reason: &str) -> Result<(), EngineError> {
        let project: Option<String> = self
            .connection
            .query_row("SELECT project FROM team_agents WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        match project.and_then(|project| ProjectId::new(project).ok()) {
            Some(project) => self.note(
                &project,
                &format!(
                    "The terminal of team agent {id} was closed: {}.",
                    bound_text(reason, 300)
                ),
            ),
            None => {
                eprintln!("team runtime: closed terminal team-{id}: {reason}");
                Ok(())
            }
        }
    }

    /// Ends team panes that no live agent of this engine owns, at most every `SWEEP_EVERY`
    /// once any agent has run, so no pane outlives its agent. Each project hears which.
    fn sweep(&mut self) -> Result<(), EngineError> {
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if self.swept.is_none_or(|at| at.elapsed() < SWEEP_EVERY) {
            return Ok(());
        }
        self.swept = Some(Instant::now());
        let Ok(alive) = host.alive() else {
            return Ok(());
        };
        for agent in alive {
            let Some(id) = agent.team_agent() else {
                continue;
            };
            if self.workers.contains_key(&id) {
                continue;
            }
            match end_left_pane(host.as_ref(), &agent, "team") {
                Ok(_) => self.closed_note(id, "its agent had stopped")?,
                Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                Err(problem) => return Err(problem.into()),
            }
        }
        Ok(())
    }

    /// Gives every team master with tools on the engine's `delegate` tool, served by `executable`.
    /// Takes the engine executable; the file at its canonical path is captured, so every later
    /// master launch serves the tool from that same file or is refused.
    /// Returns the runtime, or a plain code when the executable is not a file or is unsafe.
    pub fn with_delegation_tool(mut self, executable: &Path) -> Result<Self, EngineError> {
        let canonical = executable
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
            .ok_or_else(|| EngineError("team_executable_invalid".into()))?;
        self.delegation_tool = Some(RegisteredNativeAuthHost::capture(&canonical)?);
        Ok(self)
    }

    /// Runs one step for every project with team work: settles the runs the last engine
    /// stop cut short, applies agent reports and the owner's stop and restart from the agent
    /// register, sends pending messages to the master, delivers finished results, starts or
    /// reuses sub-agents within the plan's limits, stops idle or cancelled agents, ends team
    /// panes no agent owns, rewrites each followed IDE workspace whose sections changed, logs
    /// the questions masters' harnesses ask the owner in their own terminals, then alerts the
    /// owner to new questions and terminal waits by severity.
    pub fn tick(&mut self) -> Result<(), EngineError> {
        self.settle()?;
        self.collect()?;
        self.send_own_rules()?;
        self.owner_controls()?;
        let access = (self.access)().unwrap_or_default();
        self.owner_restarts(&access)?;
        self.owner_wakes(&access)?;
        for project in self.active_projects()? {
            self.step(&project, &access)?;
        }
        self.collect()?;
        self.owner_controls()?;
        self.note_conversations()?;
        self.watch_questions()?;
        self.watch_teammates()?;
        self.sweep()?;
        crate::ide::workspace::follow(&self.connection, &self.root)?;
        self.watch_master_questions()?;
        super::questions::tick(&mut self.connection, now_ms())
    }

    /// Logs each question a live master's harness asks the owner in its own terminal as
    /// asked in terminal, and once its box leaves the screen marks it answered there when the
    /// owner typed into that terminal since it was asked, or dismissed otherwise, at most
    /// once each `events::SCAN_EVERY`. A master whose questions cannot be read is skipped this
    /// scan. Every master is served before the first failure or log refusal is returned.
    fn watch_master_questions(&mut self) -> Result<(), EngineError> {
        use super::screen_questions::{Change, Pending};
        use crate::questions::{Ask, Asker};
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if !self.master_watch.due() {
            return Ok(());
        }
        let live: Vec<super::events::Watched> = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.role == AgentRole::Master && !worker.stopping)
            .map(|(id, worker)| super::events::Watched {
                agent: *id,
                project: worker.project.clone(),
                task: None,
            })
            .collect();
        let mut first = None;
        let mut read = BTreeSet::new();
        let mut pending = Vec::new();
        for (project, agent) in self.master_watch.askers(&live) {
            let asker = Asker::Terminal(agent).name();
            match crate::questions::pending(&self.connection, &project) {
                Ok(found) => {
                    read.insert(agent);
                    pending.extend(
                        found
                            .into_iter()
                            .filter(|question| question.asker == asker)
                            .map(|question| Pending {
                                id: question.id,
                                project: project.clone(),
                                agent,
                                text: question.question,
                            }),
                    );
                }
                Err(problem) => {
                    first.get_or_insert(problem);
                }
            }
        }
        let masters: Vec<super::events::Watched> = live
            .into_iter()
            .filter(|master| read.contains(&master.agent))
            .collect();
        for change in self.master_watch.scan(host.as_ref(), &masters, &pending) {
            let applied = match change {
                Change::Asked {
                    project,
                    agent,
                    text,
                } => {
                    let ask = Ask {
                        question: text.clone(),
                        options: Vec::new(),
                        recommended: None,
                        context: None,
                    };
                    let asker = Asker::Terminal(agent);
                    crate::questions::ask(&self.connection, &project, &asker, &ask)
                        .map(|_| self.master_watch.logged(agent, &text))
                }
                Change::Left { id, agent } => super::master::owner_key_ms(
                    &self.connection,
                    self.agents.as_deref().map(Agents::typing),
                    agent,
                )
                .and_then(|typed| {
                    crate::questions::terminal_settled(&self.connection, id, typed).map(|_| ())
                }),
            };
            if let Err(problem) = applied {
                first.get_or_insert(problem);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Settles the runs the last engine stop cut short once, on the first tick after the
    /// runtime adopted the panes it could, and on each later tick until that succeeds: first
    /// the deliveries whose run is dropped because its master runs again in its pane, as
    /// deliveries no run holds, then the runs; a sub-agent that runs again in its pane keeps
    /// the run of its task and resumes it there. Until then the tick does nothing else, so no
    /// report, owner restart or dispatch changes which agents run again, and no task or
    /// message such a run holds starts fresh or reaches an agent twice.
    /// Returns `recovery_unavailable` while the store cannot be written.
    fn settle(&mut self) -> Result<(), EngineError> {
        if !self.settled {
            delivery::settle(&self.connection)?;
            crate::recovery::settle(&self.connection)?;
            self.settled = true;
        }
        Ok(())
    }

    /// Holds each running lead to the owner's Max teammates per harness in both modes, at most
    /// once each `events::SCAN_EVERY`: native teammates past the cap end newest first, then
    /// its terminal's teammate panes are read, recorded, and ended when the owner stopped
    /// them or they pass what the cap leaves after its app-spawned sub-agents. The lead hears
    /// why in its terminal. A lead whose own pane is not found among its window's panes keeps
    /// its panes, so its own pane is never ended. Returns any SQLite failure.
    /// Sends each kept lead whose app runs a turn by itself the project's current rules for
    /// it, its bound as `turn_bound` gives it, the task limit or the session time left when
    /// the turn was first seen, whichever is shorter, and what its end does while background
    /// commands still run, whenever they differ from the ones it last got, so a held own turn
    /// ends as the owner's saved settings say now and not as an older or default copy
    /// (N-E9). Takes the runtime. Returns a failed read.
    fn send_own_rules(&mut self) -> Result<(), EngineError> {
        for worker in self.workers.values_mut() {
            if !worker.kept_session || worker.own_turn.is_none() {
                continue;
            }
            let ran = worker
                .own_rules
                .map_or_else(|| worker.ran(), |(ran, _)| ran);
            let bound = turn_bound(super::time_limits(&self.connection, &worker.project)?, ran);
            let rules = TurnRules {
                limit: bound.map(|(limit, _)| limit),
                background: super::background_at_end(&self.connection, &worker.project)?,
            };
            if worker.own_rules.map(|(_, sent)| sent) != Some(rules)
                && worker.orders.send(Order::Rules(rules)).is_ok()
            {
                worker.own_rules = Some((ran, rules));
            }
        }
        Ok(())
    }

    fn watch_teammates(&mut self) -> Result<(), EngineError> {
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        let now = Instant::now();
        if self.teammates_next.is_some_and(|next| now < next) {
            return Ok(());
        }
        self.teammates_next = Some(now + super::events::SCAN_EVERY);
        let at = now_ms();
        self.connection
            .execute(
                "UPDATE team_teammates SET ended_ms=?1 WHERE ended_ms IS NULL AND agent NOT IN
                 (SELECT id FROM team_agents WHERE status IN ('starting','idle','working'))",
                [at],
            )
            .map_err(error)?;
        teammates::end_orphans(&self.connection, at)?;
        let harnesses: Vec<(i64, ProjectId)> = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.role == AgentRole::Sub && !worker.stopping)
            .map(|(id, worker)| (*id, worker.project.clone()))
            .collect();
        self.teammate_worktree_tries
            .retain(|lead, _| harnesses.iter().any(|(agent, _)| agent == lead));
        self.teammates_rested
            .retain(|lead, _| harnesses.iter().any(|(agent, _)| agent == lead));
        self.teammate_turn_tries
            .retain(|lead, _| harnesses.iter().any(|(agent, _)| agent == lead));
        self.teammates_unreadable_told
            .retain(|lead| harnesses.iter().any(|(agent, _)| agent == lead));
        self.teammate_conflicts_told
            .retain(|lead, _| harnesses.iter().any(|(agent, _)| agent == lead));
        for (agent, project) in harnesses {
            let cap = super::teammate_cap(&self.connection, &project)?;
            if let Some(cap) = cap {
                self.cap_native(&project, agent, cap)?;
            }
            let spawned = self
                .workers
                .values()
                .filter(|worker| worker.parent == Some(agent) && !worker.stopping)
                .count();
            let pane_cap =
                cap.map(|cap| cap.saturating_sub(u8::try_from(spawned).unwrap_or(u8::MAX)));
            let record: Option<String> = self
                .connection
                .query_row(
                    "SELECT record FROM team_panes WHERE agent=?1",
                    [agent],
                    |row| row.get(0),
                )
                .optional()
                .map_err(error)?;
            let Some(lead) = record.as_deref().and_then(PaneRecord::decode) else {
                continue;
            };
            let Ok(name) = AgentId::team(agent) else {
                continue;
            };
            let Ok(panes) = host.panes(&name) else {
                continue;
            };
            if !panes.iter().any(|pane| pane.pid == lead.pid) {
                continue;
            }
            let team: Vec<&crate::terminal::PaneInfo> = panes
                .iter()
                .filter(|pane| pane.pid != lead.pid && !pane.dead)
                .collect();
            self.teammates_seen(&project, agent, &team, at)?;
            let live: Vec<&str> = team.iter().map(|pane| pane.id.as_str()).collect();
            teammates::register(
                &self.connection,
                &project,
                agent,
                &agent_home(&self.state, agent),
                &live,
                i64::try_from(lead.since_ms).unwrap_or(i64::MAX),
                at,
            )?;
            self.make_teammate_worktrees(&project, agent)?;
            self.record_teammate_turns(&project, agent)?;
            self.land_teammates(&project, agent)?;
            for (teammate, pane) in teammates::working(&self.connection, &project, agent)? {
                if !live.contains(&pane.as_str()) {
                    continue;
                }
                if let Ok(own) = AgentId::team(teammate) {
                    let _ = host.adopt_pane(&own, &name, &pane);
                }
            }
            let ended = self.end_teammates(host.as_ref(), &project, agent, &team, pane_cap, at)?;
            assert!(team.iter().all(|pane| pane.pid != lead.pid));
            if let Some(text) = ended {
                let _ = super::events::send_to(
                    &self.connection,
                    host.as_ref(),
                    Some(agent),
                    super::agent_messages::Said::Notice,
                    &text,
                )?;
            }
        }
        Ok(())
    }

    /// Records one harness's live teammate panes: a new pane is added, a recorded pane no
    /// longer live is marked ended, and the oldest ended ones past `TEAMMATE_ROWS_KEPT` go.
    /// Takes the project, the harness, its live teammate panes and the time. Returns any
    /// SQLite failure.
    fn teammates_seen(
        &self,
        project: &ProjectId,
        agent: i64,
        team: &[&crate::terminal::PaneInfo],
        at: i64,
    ) -> Result<(), EngineError> {
        for pane in team {
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO team_teammates(agent,project,pane,seen_ms) VALUES (?1,?2,?3,?4)",
                    params![agent, project.as_str(), pane.id, at],
                )
                .map_err(error)?;
        }
        let open: Vec<String> = {
            let mut statement = self
                .connection
                .prepare("SELECT pane FROM team_teammates WHERE agent=?1 AND ended_ms IS NULL")
                .map_err(error)?;
            statement
                .query_map([agent], |row| row.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        for pane in open
            .iter()
            .filter(|pane| !team.iter().any(|live| &live.id == *pane))
        {
            self.connection
                .execute(
                    "UPDATE team_teammates SET ended_ms=?3 WHERE agent=?1 AND pane=?2",
                    params![agent, pane, at],
                )
                .map_err(error)?;
        }
        self.connection
            .execute(
                "DELETE FROM team_teammates WHERE agent=?1 AND ended_ms IS NOT NULL AND pane NOT IN
                 (SELECT pane FROM team_teammates WHERE agent=?1
                  ORDER BY ended_ms IS NOT NULL, COALESCE(ended_ms,seen_ms) DESC LIMIT ?2)",
                params![agent, TEAMMATE_ROWS_KEPT],
            )
            .map_err(error)?;
        Ok(())
    }

    /// Ends the teammate panes of one harness the owner stopped, then the newest past the
    /// owner's Max teammates per harness, and marks each ended. Takes the terminal host, the
    /// project, the harness, its live teammate panes, the cap and the time. Returns the line
    /// to type into the harness about what ended, none when nothing did, or a SQLite failure.
    fn end_teammates(
        &self,
        host: &dyn Host,
        project: &ProjectId,
        agent: i64,
        team: &[&crate::terminal::PaneInfo],
        cap: Option<u8>,
        at: i64,
    ) -> Result<Option<String>, EngineError> {
        let asked: BTreeSet<String> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT pane FROM team_teammates WHERE agent=?1 AND project=?2
                     AND ended_ms IS NULL AND stopped_ms IS NOT NULL",
                )
                .map_err(error)?;
            statement
                .query_map(params![agent, project.as_str()], |row| row.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let mut kept: Vec<&str> = team
            .iter()
            .map(|pane| pane.id.as_str())
            .filter(|pane| !asked.contains(*pane))
            .collect();
        kept.sort_by_key(|pane| pane[1..].parse::<u64>().unwrap_or(u64::MAX));
        let over: Vec<String> = match cap {
            Some(cap) if kept.len() > usize::from(cap) => kept[usize::from(cap)..]
                .iter()
                .map(|pane| (*pane).to_owned())
                .collect(),
            _ => Vec::new(),
        };
        let name = AgentId::team(agent).map_err(EngineError::from)?;
        let mut stopped = Vec::new();
        let mut capped = Vec::new();
        for pane in asked
            .iter()
            .filter(|pane| team.iter().any(|live| &live.id == *pane))
        {
            if host.kill_pane(&name, pane).is_ok() {
                stopped.push(pane.clone());
            }
        }
        for pane in &over {
            if host.kill_pane(&name, pane).is_ok() {
                capped.push(pane.clone());
            }
        }
        for pane in stopped.iter().chain(capped.iter()) {
            self.connection
                .execute(
                    "UPDATE team_teammates SET stopped_ms=COALESCE(stopped_ms,?3), ended_ms=?3 WHERE agent=?1 AND pane=?2",
                    params![agent, pane, at],
                )
                .map_err(error)?;
        }
        let mut lines = Vec::new();
        if !stopped.is_empty() {
            lines.push(format!(
                "The owner stopped your teammate in pane {}. Do not start it again unless the owner asks.",
                stopped.join(", ")
            ));
        }
        if let (Some(cap), false) = (cap, capped.is_empty()) {
            lines.push(format!(
                "The owner allows at most {cap} teammates per harness, so the engine closed your newest teammate pane {}. Keep at most {cap} teammates.",
                capped.join(", ")
            ));
        }
        Ok((!lines.is_empty()).then(|| format!("[team] {}", lines.join(" "))))
    }

    /// Reads each busy sub-agent's terminal for a question it waits on, at most once each
    /// `events::SCAN_EVERY`, so the master hears it as an event. Returns any SQLite failure.
    fn watch_questions(&mut self) -> Result<(), EngineError> {
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if !self.watch.due() {
            return Ok(());
        }
        let watched: Vec<super::events::Watched> = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.role == AgentRole::Sub && !worker.stopping)
            .filter_map(|(id, worker)| match worker.busy {
                Some(Busy::Task(task)) => Some(super::events::Watched {
                    agent: *id,
                    project: worker.project.clone(),
                    task: Some(task),
                }),
                _ => None,
            })
            .collect();
        self.watch
            .scan(&self.connection, host.as_ref(), &watched, now_ms())
    }

    /// Writes the engine's note on each task a conversation master handed off that has
    /// ended, at most `NOTES_PER_TICK_MAX` a tick, so that conversation and the master's
    /// next turn learn how it went. Each task is noted once. Returns any SQLite failure.
    fn note_conversations(&mut self) -> Result<(), EngineError> {
        type Row = (
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
        );
        let rows: Vec<Row> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT c.task,t.harness,t.status,t.prompt,t.result,t.error,t.agent,a.worktree
                     FROM team_conversation_tasks c JOIN team_tasks t ON t.id=c.task
                     LEFT JOIN team_agents a ON a.id=t.agent
                     WHERE c.note IS NULL AND t.status IN ('done','failed','cancelled')
                     ORDER BY c.task LIMIT ?1",
                )
                .map_err(error)?;
            statement
                .query_map([NOTES_PER_TICK_MAX], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let now = now_ms();
        for (id, harness, status, prompt, result, problem, agent, worktree) in rows {
            let status = TaskStatus::parse(&status)
                .filter(|status| status.finished())
                .expect("the sweep reads ended tasks only");
            let note = conversation_note(&EndedTask {
                id,
                harness,
                status,
                prompt,
                result,
                problem,
                agent,
                worktree,
            });
            let changed = tx
                .execute(
                    "UPDATE team_conversation_tasks SET note=?2, noted_ms=?3 WHERE task=?1 AND note IS NULL",
                    params![id, note, now],
                )
                .map_err(error)?;
            assert!(changed <= 1);
        }
        tx.commit().map_err(error)
    }

    /// Applies the owner's stop and restart from the agent register to live agents and
    /// shows the register which agents are working. A stop cancels the agent's work; a
    /// restart returns it to its queue for the fresh process. What the stop stores, the live
    /// switch turned off and the owner's stop of a sub-agent (AW-1), is written before the
    /// agent is touched, so a failed write leaves the stop to the next tick.
    fn owner_controls(&mut self) -> Result<(), EngineError> {
        let stops: BTreeMap<i64, StoppedBy> = {
            let mut statement = self
                .connection
                .prepare("SELECT agent,stopped_by FROM team_owner_stops")
                .map_err(error)?;
            statement
                .query_map([], |row| {
                    Ok((row.get(0)?, StoppedBy::named(&row.get::<_, String>(1)?)))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let mut stopped = Vec::new();
        for (id, worker) in &mut self.workers {
            if let (AgentRole::Sub, false, Some(by)) = (worker.role, worker.stopping, stops.get(id))
            {
                worker.held = true;
                worker.stopping = true;
                worker.cancel.store(true, Ordering::SeqCst);
                let _ = worker.orders.send(Order::Stop);
                stopped.push((*id, worker.project.clone(), *by));
                continue;
            }
            let Some(hosted) = &worker.hosted else {
                continue;
            };
            hosted.set_busy(worker.busy.is_some());
            let asked = hosted.requested();
            let restart = asked == Some(HostedRequest::Restart) && !worker.held;
            if !restart && (asked != Some(HostedRequest::Stop) || worker.stopping) {
                continue;
            }
            if worker.live {
                super::master::set_live(&self.connection, &worker.project, false)?;
            }
            if worker.role == AgentRole::Sub {
                self.connection
                    .execute(
                        "INSERT OR REPLACE INTO team_owner_stops(agent,project,stopped_ms) VALUES (?1,?2,?3)",
                        params![*id, worker.project.as_str(), now_ms()],
                    )
                    .map_err(error)?;
                if !restart {
                    super::owner_stop(&self.connection, &worker.project, *id, now_ms())?;
                    worker.held = true;
                    stopped.push((*id, worker.project.clone(), StoppedBy::Owner));
                }
            }
            if restart {
                worker.held = true;
                worker.restarting = true;
            }
            worker.stopping = true;
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
        stopped.extend(self.stop_unmanaged()?);
        for (id, project, by) in &stopped {
            super::events::record(
                &self.connection,
                project,
                *id,
                None,
                super::events::EventKind::Progress,
                stop_notice(*by),
                now_ms(),
            )?;
        }
        self.tell_leads(&stopped)
    }

    /// Ends the stopped native teammates no worker of this engine runs (DEC-104 K5a) through
    /// [`end_left_pane`]: terminate to the pane's first process, then kill to its group, never
    /// a tmux client of the lead's server (K5c). Each end runs off the tick (S4B-B1-L1) and its
    /// row is marked once it finished; a host failure leaves it to a later tick. Returns the
    /// agents marked stopped now and who stopped each, or any SQLite failure.
    fn stop_unmanaged(&mut self) -> Result<Vec<(i64, ProjectId, StoppedBy)>, EngineError> {
        let mut stopped = self.unmanaged_ended()?;
        let listed: Vec<(i64, String, String)> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT a.id,a.project,s.stopped_by FROM team_agents a JOIN team_owner_stops s ON s.agent=a.id
                     WHERE a.parent IS NOT NULL AND a.parent<>a.id
                     AND a.status IN ('starting','idle','working') ORDER BY a.id LIMIT ?1",
                )
                .map_err(error)?;
            statement
                .query_map([UNMANAGED_STOPS_PER_TICK_MAX as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        assert!(listed.len() <= UNMANAGED_STOPS_PER_TICK_MAX);
        for (id, project, by) in listed {
            if self.workers.contains_key(&id) || self.unmanaged_ending.contains_key(&id) {
                continue;
            }
            let Ok(project) = ProjectId::new(project) else {
                continue;
            };
            let by = StoppedBy::named(&by);
            let end =
                self.end_unmanaged(id, stopped_teammate(by), Told::Stop(project.clone(), by))?;
            if end == UnmanagedEnd::Marked {
                stopped.push((id, project, by));
            }
        }
        Ok(stopped)
    }

    /// Ends one sub-agent no worker runs, a native teammate, through the terminal host as any
    /// left pane ends. That end can wait seconds on a pane that ignores terminate, so it runs
    /// on a thread of its own and the tick goes on (S4B-B1-L1); [`Self::unmanaged_ended`] marks
    /// the row once it finished. With no terminal host the row is marked at once. Takes the
    /// agent, the reason and who is told once it is marked. Returns how it was taken.
    fn end_unmanaged(
        &mut self,
        id: i64,
        reason: &'static str,
        told: Told,
    ) -> Result<UnmanagedEnd, EngineError> {
        assert!(!self.workers.contains_key(&id) && !self.unmanaged_ending.contains_key(&id));
        let Some(host) = self.terminal.clone() else {
            return Ok(if self.mark_unmanaged(id, reason, true)? {
                UnmanagedEnd::Marked
            } else {
                UnmanagedEnd::Untouched
            });
        };
        let name = AgentId::team(id).map_err(EngineError::from)?;
        let sender = self.unmanaged_ends.0.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("team-end-{id}"))
            .spawn(move || {
                end_reported(id, sender, || end_left_pane(host.as_ref(), &name, "team"));
            });
        if spawned.is_err() {
            return Ok(UnmanagedEnd::Untouched);
        }
        self.unmanaged_ending
            .insert(id, PendingEnd { reason, told });
        Ok(UnmanagedEnd::Handed)
    }

    /// Marks stopped each native teammate whose end a thread finished since the last call,
    /// with the uncertain cleanup code when the end was not proven (S4B-B1); an unknown pane
    /// counts as gone, any other host failure leaves the row to a later tick. Only rows it
    /// marked are told: their owner's side stops are returned, and each over-cap lead gets one
    /// note naming its ended teammates. Returns those stops, or a SQLite or store failure.
    fn unmanaged_ended(&mut self) -> Result<Vec<(i64, ProjectId, StoppedBy)>, EngineError> {
        let mut stopped = Vec::new();
        let mut capped: BTreeMap<(ProjectId, i64, u8), Vec<String>> = BTreeMap::new();
        while let Ok((id, outcome)) = self.unmanaged_ends.1.try_recv() {
            let pending = self
                .unmanaged_ending
                .remove(&id)
                .expect("an end finished only after it was handed over");
            let confirmed = match outcome {
                Ok(confirmed) => confirmed,
                Err(problem) if problem.0 == "terminal_agent_unknown" => true,
                Err(_) => continue,
            };
            if !self.mark_unmanaged(id, pending.reason, confirmed)? {
                continue;
            }
            match pending.told {
                Told::Stop(project, by) => stopped.push((id, project, by)),
                Told::Lead { project, lead, cap } => {
                    let name = names::name(&self.connection, id)?;
                    capped
                        .entry((project, lead, cap))
                        .or_default()
                        .push(names::label(AgentRole::Sub, id, name.as_ref()));
                }
            }
        }
        for ((project, lead, cap), ended) in capped {
            self.note_lead(&project, lead, over_cap_note(cap, &ended))?;
        }
        Ok(stopped)
    }

    /// Marks one native teammate's row stopped with the reason, plus the uncertain cleanup
    /// code when its end was not proven. Takes the agent, the reason and whether the end was
    /// proven. Returns whether it marked the row, false for one already stopped elsewhere, or
    /// any SQLite failure.
    fn mark_unmanaged(&self, id: i64, reason: &str, confirmed: bool) -> Result<bool, EngineError> {
        let note = if confirmed {
            reason.to_owned()
        } else {
            with_code(reason, Some(crate::harnesses::leftovers::UNCERTAIN))
        };
        let marked = self
            .connection
            .execute(
                "UPDATE team_agents SET status='stopped', task=NULL, error=?2, stopped_ms=?3
                 WHERE id=?1 AND status IN ('starting','idle','working')",
                params![id, note, now_ms()],
            )
            .map_err(error)?;
        assert!(marked <= 1);
        Ok(marked == 1)
    }

    /// Marks stopped the native teammates of a lead that just ended. They run on their own
    /// tmux server, which ends itself once the engine clears the lead's short temp folder,
    /// and may outlive the lead until then; the lead's caller socket node stays there, so a
    /// teammate's hook meanwhile is refused and never takes the lead's place (rv-secreview
    /// RH2R2-1). Sub-agents a worker runs go on. Takes the lead. Returns any SQLite failure.
    fn lead_ended(&self, lead: i64) -> Result<(), EngineError> {
        let children: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id FROM team_agents WHERE parent=?1 AND id<>?1
                     AND status IN ('starting','idle','working')",
                )
                .map_err(error)?;
            statement
                .query_map([lead], |row| row.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        for child in children
            .into_iter()
            .filter(|child| !self.workers.contains_key(child))
        {
            self.connection
                .execute(
                    "UPDATE team_agents SET status='stopped', task=NULL, error=?2, stopped_ms=?3
                     WHERE id=?1 AND status IN ('starting','idle','working')",
                    params![child, LEAD_ENDED_TEAMMATE, now_ms()],
                )
                .map_err(error)?;
        }
        Ok(())
    }

    /// Ends a running lead's newest native teammates past the owner's Max teammates per
    /// harness, counted with its app-spawned sub-agents (DEC-104 K8), an end still running off
    /// the tick counted as made. App-spawned ones are never ended here: the engine admits them
    /// against the cap before they start. The lead is noted once their rows are marked. Takes the
    /// project, the lead and the cap. Returns any SQLite or coordination store failure.
    fn cap_native(&mut self, project: &ProjectId, lead: i64, cap: u8) -> Result<(), EngineError> {
        let running = super::caps::running_sub_agents(&self.connection, project, lead)?;
        if running <= u32::from(cap) {
            return Ok(());
        }
        let native: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id FROM team_agents WHERE project=?1 AND parent=?2 AND id<>?2
                     AND status IN ('starting','idle','working') ORDER BY id DESC LIMIT ?3",
                )
                .map_err(error)?;
            statement
                .query_map(
                    params![
                        project.as_str(),
                        lead,
                        crate::terminal::PANES_LISTED_MAX as i64
                    ],
                    |row| row.get(0),
                )
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let ending = native
            .iter()
            .filter(|id| self.unmanaged_ending.contains_key(id))
            .count();
        let over = usize::try_from(running - u32::from(cap))
            .unwrap_or(usize::MAX)
            .saturating_sub(ending);
        let picked: Vec<i64> = native
            .into_iter()
            .filter(|id| !self.workers.contains_key(id) && !self.unmanaged_ending.contains_key(id))
            .take(over)
            .collect();
        let mut ended = Vec::new();
        for id in picked {
            let told = Told::Lead {
                project: project.clone(),
                lead,
                cap,
            };
            if self.end_unmanaged(id, OVER_TEAMMATE_CAP, told)? == UnmanagedEnd::Marked {
                let name = names::name(&self.connection, id)?;
                ended.push(names::label(AgentRole::Sub, id, name.as_ref()));
            }
        }
        assert!(ended.len() <= over);
        if ended.is_empty() {
            return Ok(());
        }
        self.note_lead(project, lead, over_cap_note(cap, &ended))
    }

    /// Sends a lead, or any team agent, an engine note through the coordination layer
    /// (DEC-104), which delivers it natively or by typing. An agent the layer no longer
    /// knows is skipped. Takes the project, the agent and the note. Returns any
    /// coordination store failure.
    fn note_lead(&self, project: &ProjectId, lead: i64, body: String) -> Result<(), EngineError> {
        let note = crate::coord::NewMessage {
            project: project.clone(),
            from: crate::coord::Party::Engine,
            to: crate::coord::Party::Agent(lead),
            kind: crate::coord::MessageKind::Note,
            question: None,
            work: None,
            reply_to: None,
            body,
            idempotency_key: None,
        };
        match crate::coord::send(&self.connection, note, now_ms()) {
            Ok(_) => Ok(()),
            Err(problem) if problem.0.starts_with("coord_state_unavailable") => Err(problem),
            Err(_) => Ok(()),
        }
    }

    /// Tells each running lead which of its sub-agents the owner or the master just stopped,
    /// once per stop, so it neither waits for them nor starts their work again unasked. A
    /// lead stopped too is not told, nor one that made the stop. Takes the stopped agents and
    /// who stopped each. Returns any SQLite failure.
    fn tell_leads(&self, stopped: &[(i64, ProjectId, StoppedBy)]) -> Result<(), EngineError> {
        for (id, project, by) in stopped {
            if *by == StoppedBy::Lead {
                continue;
            }
            let lead: Option<i64> = self
                .connection
                .query_row("SELECT parent FROM team_agents WHERE id=?1", [*id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(error)?
                .flatten();
            let Some(lead) = lead.filter(|lead| {
                *lead != *id
                    && self
                        .workers
                        .get(lead)
                        .is_some_and(|worker| worker.role == AgentRole::Sub && !worker.stopping)
            }) else {
                continue;
            };
            let name = names::name(&self.connection, *id)?;
            let label = names::label(AgentRole::Sub, *id, name.as_ref());
            let text = if *by == StoppedBy::Owner {
                format!(
                    "The owner stopped your sub-agent {label}. Its task waits for it until the owner wakes it; do not start another sub-agent for that work unless the owner asks."
                )
            } else {
                format!(
                    "The master stopped your sub-agent {label}. Its unfinished tasks failed. The master or the owner can wake it."
                )
            };
            self.note_lead(project, lead, text)?;
        }
        Ok(())
    }

    fn owner_restarts(&mut self, access: &ModelAccess) -> Result<(), EngineError> {
        let Some(agents) = self.agents.clone() else {
            return Ok(());
        };
        let mut restarts = std::mem::take(&mut self.held_restarts);
        restarts.extend(agents.take_restarts(TEAM_HOST));
        for restart in restarts {
            if let Ok(reference) = restart.reference.parse::<i64>() {
                self.start_again(reference, Some(&restart), access)?;
            }
        }
        Ok(())
    }

    /// Serves the owner's Wake buttons (DEC-102), at most `OWNER_WAKES_PER_TICK_MAX` a tick,
    /// oldest first: each request is taken once and the agent wakes through
    /// [`TeamRuntime::wake`], never as a new agent. Returns any SQLite failure.
    fn owner_wakes(&mut self, access: &ModelAccess) -> Result<(), EngineError> {
        let requests: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT agent FROM team_wake_requests ORDER BY requested_ms, agent LIMIT ?1",
                )
                .map_err(error)?;
            statement
                .query_map([OWNER_WAKES_PER_TICK_MAX as i64], |row| row.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        assert!(requests.len() <= OWNER_WAKES_PER_TICK_MAX);
        for agent in requests {
            self.connection
                .execute("DELETE FROM team_wake_requests WHERE agent=?1", [agent])
                .map_err(error)?;
            self.start_again(agent, None, access)?;
        }
        Ok(())
    }

    /// Starts a team agent the owner restarted or woke again, when the team plan and the
    /// pool still allow it; otherwise notes why not. A stopped or failed sub-agent whose
    /// worktree is still recorded wakes through [`TeamRuntime::wake`] under its own id, name,
    /// worktree and register entry; no other sub-agent starts again, so a register entry
    /// never moves to another agent. A restarted master starts as a fresh process under its
    /// register entry. A Wake (no `again`) of an agent that is no longer finished does
    /// nothing, so a stale Wake never stops or replaces a running agent. A woken sub-agent
    /// clears the owner stops of the agents it leads too, the one start that does (AW-1). A
    /// restart a projects home move holds back waits and is tried again on each tick until
    /// it starts.
    fn start_again(
        &mut self,
        reference: i64,
        again: Option<&HostedRestart>,
        access: &ModelAccess,
    ) -> Result<(), EngineError> {
        let row: Option<(String, String, String, bool, Option<i64>)> = self
            .connection
            .query_row(
                "SELECT a.project,a.role,a.harness,
                        a.status IN ('stopped','failed') AND EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id AND w.project=a.project),
                        a.parent
                 FROM team_agents a WHERE a.id=?1",
                [reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((project, role, harness, wakes, lead)) = row else {
            return Ok(());
        };
        let (Ok(project), Some(role)) = (ProjectId::new(project), AgentRole::parse(&role)) else {
            return Ok(());
        };
        match (role, wakes, again) {
            (AgentRole::Sub, false, None)
            | (AgentRole::Master, _, None)
            | (AgentRole::Teammate, _, None) => return Ok(()),
            (AgentRole::Teammate, _, Some(_)) => {
                return self.note(
                    &project,
                    "That teammate was not restarted: a teammate starts only from its harness.",
                );
            }
            (AgentRole::Sub, false, Some(_)) => {
                return self.note(
                    &project,
                    "That sub-agent was not restarted: only a stopped agent whose worktree is still kept starts again, under its own name.",
                );
            }
            _ => {}
        }
        let Some(stored) = self.stored(&project)? else {
            return Ok(());
        };
        if super::paused(&self.connection, &project)? {
            return self.note(
                &project,
                "The agent was not restarted because the team is paused. Resume the team, then restart it.",
            );
        }
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        let (harness, tools) = match role {
            AgentRole::Master if self.master(&project).is_some() => {
                return self.note(
                    &project,
                    "The master is already running, so no second master was started.",
                );
            }
            AgentRole::Master => (stored.team.master.clone(), stored.team.master_tools),
            AgentRole::Teammate => return Ok(()),
            AgentRole::Sub => match super::slot(&stored.team, &harness) {
                None => {
                    return self.note(
                        &project,
                        "That sub-agent's harness is no longer in the team plan, so it was not restarted.",
                    );
                }
                Some(slot)
                    if self.live(&project, AgentRole::Sub, Some(&slot.harness))
                        >= usize::from(slot.agents)
                        || self.live(&project, AgentRole::Sub, None) >= subs_max =>
                {
                    return self.note(
                        &project,
                        "The team already runs as many sub-agents as its plan allows, so no other was started.",
                    );
                }
                Some(_)
                    if self.capped(
                        &project,
                        lead.filter(|lead| *lead != reference),
                        super::caps::read(&self.connection, &project)?.harnesses,
                    ) =>
                {
                    return self.note(
                        &project,
                        if lead.is_some() {
                            "Its lead already runs as many sub-agents as your Max teammates per harness setting allows, so it was not started. Stop one or raise the setting."
                        } else {
                            "The project already runs as many harnesses as your Max harnesses setting allows, so no other was started. Stop one or raise the setting."
                        },
                    );
                }
                Some(slot) => (slot.harness.clone(), slot.tools),
            },
        };
        let paused = self.paused(&project)?;
        let started = match self.account_for(access, &project, &harness, &paused) {
            Ok(None) => return self.note(&project, AGENT_RESTART_CHECKING),
            Ok(Some(account)) if role == AgentRole::Sub => {
                match self.wake(&stored, reference, &harness, tools, &account, again)? {
                    Ok((id, resumed)) => {
                        super::owner_start(&self.connection, &project, id)?;
                        self.owner_woke(&project, id, resumed, again.is_some())?;
                        Ok(id)
                    }
                    Err(reason) => Err(reason),
                }
            }
            Ok(Some(account)) => {
                match self.spawn_run(&stored, role, &harness, tools, &account, again, None)? {
                    Start::Started(id) => Ok(id),
                    Start::Held => {
                        self.held_restarts.extend(again.cloned());
                        return Ok(());
                    }
                    Start::Refused(reason) => Err(reason),
                }
            }
            Err(reason) => Err(reason),
        };
        match started {
            Ok(_) => Ok(()),
            Err(reason) => self.note(
                &project,
                &format!("The agent could not restart: {}.", bound_text(&reason, 300)),
            ),
        }
    }

    /// After the owner's Wake or restart starts a sub-agent again with no task (WK-R4): when
    /// its harness conversation could not continue, keeps its last result for its next task's
    /// prompt, as a master's wake gives it at once, and tells the master the owner woke or
    /// restarted it. Takes the project, the agent, whether its conversation continues and
    /// whether it was a restart. Returns any SQLite failure.
    fn owner_woke(
        &mut self,
        project: &ProjectId,
        id: i64,
        resumed: bool,
        restarted: bool,
    ) -> Result<(), EngineError> {
        let handoff = if resumed {
            None
        } else {
            last_result(&self.connection, id)?
        };
        if let Some(worker) = self.workers.get_mut(&id) {
            worker.handoff = handoff;
        }
        super::events::record(
            &self.connection,
            project,
            id,
            None,
            super::events::EventKind::Progress,
            if restarted {
                OWNER_RESTARTED
            } else {
                OWNER_WOKE
            },
            now_ms(),
        )?;
        Ok(())
    }

    /// Tells whether any agent process is still running.
    pub fn has_live_agents(&self) -> bool {
        !self.workers.is_empty()
    }

    /// Ends the engine's hold on every agent as the engine stops and returns running tasks
    /// to the queue. An agent in a pane a later engine can take over keeps running there with
    /// its turn stopped, so agent sessions outlive the engine; every other agent process group
    /// stops. Waits a bounded time for each agent thread. Returns the panes left running.
    pub fn shutdown(&mut self) -> Result<Vec<AgentId>, EngineError> {
        let mut kept = BTreeSet::new();
        for (id, worker) in &self.workers {
            if !worker.stopping && self.has_pane_record(*id)? {
                kept.insert(*id);
            }
        }
        for (id, worker) in &mut self.workers {
            worker.leave.store(kept.contains(id), Ordering::SeqCst);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
            worker.stopping = true;
        }
        let deadline = Instant::now() + JOIN_WAIT;
        for worker in self.workers.values_mut() {
            if let Some(thread) = worker.thread.take() {
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
        let now = now_ms();
        let ids: Vec<i64> = self.workers.keys().copied().collect();
        self.workers.clear();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        crate::recovery::capture(&tx, Some(now))?;
        for id in ids {
            let left = running_tasks(&tx, "agent=?1 AND cancel_requested=0", [id])?;
            tx.execute(
                "UPDATE team_tasks SET status='queued', agent=NULL WHERE agent=?1 AND status='running' AND cancel_requested=0",
                [id],
            )
            .map_err(error)?;
            note_left_tasks(&tx, &left, now);
            let note = kept.contains(&id).then_some(LEFT_RUNNING_NOTE);
            tx.execute(
                "UPDATE team_agents SET status='stopped', task=NULL, error=?3, stopped_ms=?2 WHERE id=?1 AND status IN ('starting','idle','working')",
                params![id, now, note],
            )
            .map_err(error)?;
            if !kept.contains(&id) {
                tx.execute("DELETE FROM team_panes WHERE agent=?1", [id])
                    .map_err(error)?;
            }
        }
        tx.execute(
            "UPDATE team_messages SET state='pending' WHERE state='sent'",
            [],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(kept
            .into_iter()
            .filter_map(|id| AgentId::team(id).ok())
            .collect())
    }

    /// Tells whether a later engine could take over the agent's pane from its stored record.
    fn has_pane_record(&self, id: i64) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_panes WHERE agent=?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(error)
    }

    fn active_projects(&self) -> Result<Vec<ProjectId>, EngineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT project FROM team_messages WHERE state='pending'
                 UNION SELECT project FROM team_tasks WHERE status IN ('queued','needs_pool','running') OR (delivered=0 AND source='master' AND status IN ('done','failed','cancelled')
                     AND (session IS NULL OR session IN (SELECT id FROM master_sessions WHERE phase='configured')))
                 UNION SELECT project FROM team_settings WHERE keep_warm=1
                 UNION SELECT project FROM team_agents WHERE status IN ('starting','idle','working')
                 UNION SELECT project FROM team_live_masters WHERE live=1",
            )
            .map_err(error)?;
        let mut projects: BTreeSet<ProjectId> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .filter_map(|row| row.ok().and_then(|p| ProjectId::new(p).ok()))
            .collect();
        projects.extend(self.workers.values().map(|w| w.project.clone()));
        projects.extend(self.instruction_restarts.values().cloned());
        Ok(projects.into_iter().collect())
    }

    fn stored(&mut self, project: &ProjectId) -> Result<Option<StoredSetup>, EngineError> {
        let tx = self.connection.transaction().map_err(error)?;
        let stored = load_setup(&tx, project)?;
        tx.commit().map_err(error)?;
        Ok(stored)
    }

    /// The accounts `project`'s picks skip now: those paused for every project and those the
    /// project holds itself after a limit its own agents reported (SECAUDIT-RATELIMIT-FORGE).
    /// Takes the project. Returns the connections.
    fn paused(&self, project: &ProjectId) -> Result<BTreeSet<CredentialRef>, EngineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT connection FROM team_paused_accounts WHERE until_ms > ?1
                 UNION SELECT connection FROM team_project_pauses
                 WHERE project = ?2 AND until_ms > ?1",
            )
            .map_err(error)?;
        let paused = statement
            .query_map(params![now_ms(), project.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(error)?
            .filter_map(|row| row.ok().and_then(|c| CredentialRef::new(c).ok()))
            .collect();
        Ok(paused)
    }

    fn step(&mut self, project: &ProjectId, access: &ModelAccess) -> Result<(), EngineError> {
        let Some(stored) = self.stored(project)? else {
            self.hold(project);
            return Ok(());
        };
        if super::budget_stop(&self.connection, project, now_ms())? {
            self.note(
                project,
                "The team stopped because its agents used the owner's token budget.",
            )?;
        }
        if super::paused(&self.connection, project)? {
            self.hold(project);
            return Ok(());
        }
        if crate::projects_home::move_holds(&self.connection, &stored.project_folder())
            .map_err(error)?
        {
            return Ok(());
        }
        let paused = self.paused(project)?;
        self.retire_rejected(project, access);
        self.retire_narrowed(&stored);
        self.sync_pool(&stored, access)?;
        self.request_cancels(project)?;
        self.resume_interrupted(&stored, access, &paused)?;
        self.keep_live(&stored, access, &paused)?;
        self.serve_live_calls(&stored, access)?;
        self.serve_agent_calls(&stored, access);
        self.serve_relays(project);
        self.tell_agent_answers(project);
        self.feed_master(&stored, access, &paused)?;
        self.deliver(&stored)?;
        self.deliver_to_leads(project)?;
        super::events::forward_to_parents(&self.connection, project, now_ms())?;
        let asleep = self.asleep(project);
        let held = self.held_for_background(project);
        let reach = crate::coord::delivery::Reach {
            host: self.terminal.as_deref(),
            owner: self.agents.as_deref().map(Agents::typing),
            state: &self.state,
            asleep: &asleep,
            held: &held,
        };
        self.deliveries
            .tick_logged(&self.connection, project, now_ms(), &reach);
        let master_live = self.master(project).is_some();
        super::events::deliver_by_message(&self.connection, project, master_live, now_ms())?;
        self.check_adoption(project);
        self.tend_claims(project);
        self.hand_leads(project, access)?;
        self.wake_for_messages(&stored, access, &asleep)?;
        self.instruction_edits(&stored, access, &paused)?;
        self.send_reasks(&stored, access, &paused)?;
        self.dispatch(&stored, access, &paused)?;
        self.keep_warm(&stored, access, &paused)?;
        self.reap_idle(&stored)
    }

    /// Lists a project's idle sub-agents on a signed-in account, whose messages go through
    /// a turn `wake_for_messages` starts instead of typing: one session per turn runs no
    /// process to type into, and a native lead's kept session shares its window with its
    /// teammates and takes only engine-started turns as tracked work. Takes the project.
    /// Returns their ids.
    fn asleep(&self, project: &ProjectId) -> BTreeSet<i64> {
        self.workers
            .iter()
            .filter(|(_, worker)| {
                &worker.project == project
                    && worker.role == AgentRole::Sub
                    && worker.managed
                    && !worker.live
                    && worker.busy.is_none()
                    && !worker.stopping
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Lists a project's agents whose task turn is held for the commands they started in the
    /// background (DEC-119): they show working on their task but wait at their prompt, so a
    /// message may be typed into them. Takes the project. Returns their ids.
    fn held_for_background(&self, project: &ProjectId) -> BTreeSet<i64> {
        self.workers
            .iter()
            .filter(|(_, worker)| {
                &worker.project == project && worker.background_held && worker.busy.is_some()
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Gives each agent from `asleep` with a waking coordination message a task pinned to it
    /// with `WAKE_TASK`, which `dispatch` runs on that agent with its messages under
    /// "Messages for you", marking them Delivered; one pinned task at a time, a worker's
    /// reporting to its lead. A refusal is logged once; the messages stay Sent for it.
    /// Takes the setup, live model access and the agents. Returns any SQLite failure.
    fn wake_for_messages(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        asleep: &BTreeSet<i64>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        for &agent in asleep {
            if !crate::coord::delivery::waking(&self.connection, agent)? {
                continue;
            }
            let Some(harness) = self.workers.get(&agent).map(|w| w.harness.clone()) else {
                continue;
            };
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(error)?;
            let pinned: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM team_task_pins p JOIN team_tasks t ON t.id=p.task
                     WHERE p.project=?1 AND p.agent=?2 AND t.status IN ('queued','needs_pool','running'))",
                    params![project.as_str(), agent],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if pinned {
                continue;
            }
            let admitted = super::admit(
                &tx,
                access,
                stored,
                Some(&harness),
                super::master::WAKE_TASK,
                TaskSource::Master,
                None,
                now_ms(),
            )?;
            let task = match admitted {
                Ok(task) => task,
                Err(code) => {
                    drop(tx);
                    if self.deliveries.wake_refused(agent, Some(code)) {
                        eprintln!("team agent {agent} not woken for its messages: {code}");
                    }
                    continue;
                }
            };
            tx.execute(
                "INSERT OR REPLACE INTO team_task_pins(task,project,agent) VALUES (?1,?2,?3)",
                params![task, project.as_str(), agent],
            )
            .map_err(error)?;
            tx.execute(
                "UPDATE team_tasks SET parent=(SELECT parent FROM team_agents WHERE id=?2) WHERE id=?1",
                params![task, agent],
            )
            .map_err(error)?;
            tx.commit().map_err(error)?;
            self.deliveries.wake_refused(agent, None);
        }
        Ok(())
    }

    /// Checks the project's agents for missed workflow habits and sends each reminder due.
    /// A failure leaves the team step running and is logged once by its plain code until
    /// the check succeeds again. Input: the project. Output: unit.
    fn check_adoption(&mut self, project: &ProjectId) {
        let failed = crate::coord::adoption::tick(&self.connection, project, now_ms())
            .err()
            .map(|problem| crate::protocol::plain_code(&problem.0).to_owned());
        if failed.is_some() && failed != self.adoption_failed {
            eprintln!(
                "team adoption not checked: {}",
                failed.as_deref().unwrap_or_default()
            );
        }
        self.adoption_failed = failed;
    }

    /// Settles a project's claims no turn end will, at the first step after the engine
    /// starts and then at most once each `CLAIMS_SWEEP_EVERY`: the landing sweep holds or
    /// releases stopped agents' finished claims and retries failed releases and restores,
    /// agents waiting for a file that is free now are prompted (OS-8 Wait), and the owner
    /// is told once when the project's claims fill one overlap check and once of each pair
    /// of agents whose waits would have closed a cycle. A
    /// failure leaves the team step running and is logged once by its plain code until a
    /// sweep succeeds again. Input: the project. Output: unit.
    fn tend_claims(&mut self, project: &ProjectId) {
        let due = Instant::now();
        if self
            .claims_next
            .get(project)
            .is_some_and(|next| due < *next)
        {
            return;
        }
        self.claims_next
            .insert(project.clone(), due + CLAIMS_SWEEP_EVERY);
        let now = now_ms();
        let swept = u64::try_from(now)
            .map_err(|_| EngineError("team_state_unavailable".into()))
            .and_then(|now| crate::landing::sweep_claims(&self.connection, project, now))
            .and_then(|()| crate::coord::wake_waiters(&self.connection, now).map(drop));
        let told = crate::coord::claims_full_notice(&self.connection, project, now)
            .and_then(|full| {
                if full {
                    self.note(project, CLAIMS_FULL_NOTE)
                } else {
                    Ok(())
                }
            })
            .and_then(|()| self.note_wait_cycles(project, now));
        let failed = swept
            .and(told)
            .err()
            .map(|problem| crate::protocol::plain_code(&problem.0).to_owned());
        if failed.is_some() && failed != self.claims_failed {
            eprintln!(
                "team claims not swept: {}",
                failed.as_deref().unwrap_or_default()
            );
        }
        self.claims_failed = failed;
    }

    fn sync_pool(&mut self, stored: &StoredSetup, access: &ModelAccess) -> Result<(), EngineError> {
        for harness in super::plan_harnesses(&stored.team) {
            let Some(launch) = launch_harness(&harness) else {
                continue;
            };
            let usable = matches!(
                eligible_accounts(access, &stored.project_id, launch),
                Accounts::Usable(accounts) if accounts
                    .iter()
                    .any(|a| self.refusal(access, a).is_none_or(|code| code == SIGN_IN_CHECKING))
            );
            let (from, to) = if usable {
                ("needs_pool", "queued")
            } else {
                ("queued", "needs_pool")
            };
            self.connection
                .execute(
                    "UPDATE team_tasks SET status=?3 WHERE project=?1 AND harness=?2 AND status=?4",
                    params![stored.project_id.as_str(), harness, to, from],
                )
                .map_err(error)?;
        }
        Ok(())
    }

    /// Stops every agent of a paused project; their work goes back to wait for the resume.
    fn hold(&mut self, project: &ProjectId) {
        for worker in self.workers.values_mut() {
            if &worker.project == project && !worker.held {
                worker.held = true;
                worker.stopping = true;
                worker.cancel.store(true, Ordering::SeqCst);
                let _ = worker.orders.send(Order::Stop);
            }
        }
    }

    /// Returns a held agent's work to its queue without spending an attempt: a task waits
    /// again, pinned to `pin` when given (or is cancelled when the owner asked for that), and
    /// a message waits again. A new pin is marked held, while a resolve or wake pin keeps its
    /// mark, so dispatch hands only a held task on when the saved level refuses its agent. An
    /// agent stopped for a narrowed policy passes no pin, so the next sub-agent takes its
    /// task under the saved policy and the stopped agent stays down.
    fn release(&mut self, pin: Option<i64>, busy: Busy) -> Result<(), EngineError> {
        match busy {
            Busy::Task(task) => {
                let now = now_ms();
                let left = running_tasks(&self.connection, "id=?1", [task])?;
                assert!(left.len() <= 1);
                self.connection
                    .execute(
                        "UPDATE team_tasks SET status='queued', agent=NULL, attempts=MAX(attempts-1,0)
                         WHERE id=?1 AND status='running' AND cancel_requested=0",
                        [task],
                    )
                    .map_err(error)?;
                if let Some(agent) = pin {
                    self.connection
                        .execute(
                            "INSERT INTO team_task_pins(task,project,agent,held)
                             SELECT id,project,?2,1 FROM team_tasks WHERE id=?1 AND status='queued'
                             ON CONFLICT(task) DO UPDATE SET project=excluded.project, agent=excluded.agent",
                            params![task, agent],
                        )
                        .map_err(error)?;
                }
                self.connection
                    .execute(
                        "UPDATE team_tasks SET status='cancelled', error='Cancelled by you', finished_ms=?2
                         WHERE id=?1 AND status='running'",
                        params![task, now],
                    )
                    .map_err(error)?;
                note_left_tasks(&self.connection, &left, now);
            }
            Busy::Message(message) => {
                self.connection
                    .execute(
                        "UPDATE team_messages SET state='pending' WHERE id=?1 AND state='sent'",
                        [message],
                    )
                    .map_err(error)?;
            }
        }
        Ok(())
    }

    fn request_cancels(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        let mut statement = self
            .connection
            .prepare("SELECT agent FROM team_tasks WHERE project=?1 AND status='running' AND cancel_requested=1 AND agent IS NOT NULL")
            .map_err(error)?;
        let agents: Vec<i64> = statement
            .query_map([project.as_str()], |row| row.get(0))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        for agent in agents {
            if let Some(worker) = self.workers.get(&agent) {
                worker.cancel.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    fn live(&self, project: &ProjectId, role: AgentRole, harness: Option<&str>) -> usize {
        self.workers
            .values()
            .filter(|w| {
                &w.project == project
                    && w.role == role
                    && (!w.stopping || w.restarting)
                    && harness.is_none_or(|h| w.harness == h)
            })
            .count()
    }

    /// Tells whether the project already runs as many harnesses, the sub-agents the master
    /// starts and not those a lead starts, as the owner's Max harnesses per project allows,
    /// so no other starts. Takes the project and the cap, None for No limit. Returns true at
    /// the cap.
    fn harnesses_full(&self, project: &ProjectId, cap: Option<u8>) -> bool {
        cap.is_some_and(|cap| {
            self.workers
                .values()
                .filter(|w| {
                    &w.project == project
                        && w.role == AgentRole::Sub
                        && w.parent.is_none()
                        && (!w.stopping || w.restarting)
                })
                .count()
                >= usize::from(cap)
        })
    }

    /// Tells whether one more agent may start for a task or wake: a lead's sub-agent while
    /// the lead runs fewer than Max teammates per harness in both modes, any other agent
    /// while the project runs fewer harnesses than Max harnesses per project. Takes the
    /// project, the lead, if any, and the harness cap. Returns true when the cap is reached.
    fn capped(&self, project: &ProjectId, lead: Option<i64>, harness_cap: Option<u8>) -> bool {
        match lead {
            Some(lead) => super::caps::admit_sub_agent(&self.connection, project, lead).is_err(),
            None => self.harnesses_full(project, harness_cap),
        }
    }

    /// Counts a project's agents that may still hold a terminal, stopping ones included.
    fn panes(&self, project: &ProjectId) -> usize {
        self.workers
            .values()
            .filter(|w| &w.project == project)
            .count()
    }

    fn load(&self, connection: &CredentialRef) -> usize {
        self.workers
            .values()
            .filter(|w| &w.connection == connection && !w.stopping)
            .count()
    }

    /// Picks the pool account an agent of a harness starts on. Takes the live access, the
    /// project, the harness and the paused accounts. Returns the account; none while no
    /// account can take the agent yet but one still waits for this engine run's sign-in
    /// re-check (DEC-108), so the caller waits instead of failing; or the plain refusal.
    fn account_for(
        &self,
        access: &ModelAccess,
        project: &ProjectId,
        harness: &str,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<Option<TeamAccount>, String> {
        let Some(launch) = launch_harness(harness) else {
            return Err(format!("{harness} cannot run team agents yet"));
        };
        let accounts = eligible_accounts(access, project, launch);
        if accounts != Accounts::NoPool && !self.executables.contains_key(launch) {
            return Err(format!("{harness} is not installed"));
        }
        let Accounts::Usable(fitting) = accounts else {
            return Err("Needs a pool".into());
        };
        if fitting.is_empty() {
            return Err(format!(
                "No account in this project's pool can run {harness}"
            ));
        }
        let mut refused = Vec::new();
        let accounts: Vec<TeamAccount> = fitting
            .into_iter()
            .filter(|account| match self.refusal(access, account) {
                None => true,
                Some(code) => {
                    refused.push(code);
                    false
                }
            })
            .collect();
        let checking = refused.iter().any(|code| code == SIGN_IN_CHECKING);
        if accounts.is_empty() {
            assert!(!refused.is_empty());
            if checking {
                return Ok(None);
            }
            if refused.iter().all(|code| code == SIGN_IN_REJECTED) {
                return Err(format!(
                    "Sign in again: the provider rejected the sign-in of every account in this project's pool that can run {harness}"
                ));
            }
            return Err(format!(
                "Sign in to an account in this project's pool to run {harness}"
            ));
        }
        let pools: Vec<&CredentialRef> = access
            .pools
            .iter()
            .filter(|pool| {
                pool.projects
                    .iter()
                    .any(|id| id.as_str() == project.as_str())
            })
            .map(|pool| &pool.id)
            .collect();
        let choice = crate::scheduling::Choice::read(
            &self.connection,
            access,
            Some(project.as_str()),
            &pools,
            super::now_ms(),
        )
        .map_err(|_| "Could not read the account scheduling settings".to_owned())?;
        if super::spread_accounts(&self.connection, project)
            .map_err(|_| "Could not read the Spread harnesses across accounts setting".to_owned())?
        {
            let used: BTreeSet<&CredentialRef> = self
                .workers
                .values()
                .filter(|w| &w.project == project && !w.stopping)
                .map(|w| &w.connection)
                .collect();
            let unused: Vec<TeamAccount> = accounts
                .iter()
                .filter(|account| !used.contains(&account.connection))
                .cloned()
                .collect();
            assert!(unused.len() <= accounts.len());
            if let Some(account) =
                pick_account(&unused, &|c: &CredentialRef| self.load(c), paused, &choice)
            {
                return Ok(Some(account.clone()));
            }
        }
        if let Some(account) = pick_account(
            &accounts,
            &|c: &CredentialRef| self.load(c),
            paused,
            &choice,
        ) {
            return Ok(Some(account.clone()));
        }
        if checking {
            return Ok(None);
        }
        if accounts
            .iter()
            .any(|account| !paused.contains(&account.connection))
        {
            return Err("Every account in this project's pool is at its usage limit, held in reserve or set to Never use".into());
        }
        Err("Every account in this project's pool is resting after a rate limit".into())
    }

    /// Tells why an account cannot take agent work now: endpoints always can, managed
    /// profiles only while their sign-in passes the master's check for this engine run.
    /// Takes the live access and the account. Returns the refusal code, or none.
    fn refusal(&self, access: &ModelAccess, account: &TeamAccount) -> Option<String> {
        match account.reach {
            TeamReach::Endpoint { .. } => None,
            TeamReach::Managed { .. } => self.sign_in(access, &account.connection).err(),
        }
    }

    /// Stops the project's idle agents whose account's sign-in the provider rejected, so
    /// the next message or task starts on another pool account instead of failing on it.
    /// Takes the project and the live access. Busy agents finish their turn first.
    fn retire_rejected(&mut self, project: &ProjectId, access: &ModelAccess) {
        let rejected: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && w.managed && w.busy.is_none() && !w.stopping)
            .filter(|(_, w)| {
                self.sign_in(access, &w.connection)
                    .is_err_and(|code| code == SIGN_IN_REJECTED)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in rejected {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
        }
    }

    /// Stops the project's agents whose running process holds more than the owner saved now.
    /// At a wider access level (AL-2) a kept or live agent stops at once and a per-turn agent
    /// while its turn runs, since an idle one reads the level again in its next grant. An
    /// agent with tools whose profile was made for a wider network than the saved one (CS-2)
    /// stops whether it runs or idles, since its next turn would run in the same profile. With
    /// a toolchain the saved policy no longer allows any agent stops, since its sandbox
    /// profile was built once at its start; a pane whose record kept no toolchains counts as
    /// holding every family. A stopped agent is held, so its task or message waits again
    /// without a spent attempt, and every next start reads the policy again, so it starts
    /// with the new level and network and without that toolchain. Each agent is checked once
    /// per saved policy. An unreadable policy stops every agent holding more than Sandboxed
    /// with no toolchain and, with tools, the None network, and a project folder that
    /// toolchain detection cannot read stops every agent holding a toolchain the owner made no
    /// choice for, each under its own reason. Takes the project's stored setup.
    fn retire_narrowed(&mut self, stored: &StoredSetup) {
        let project = &stored.project_id;
        let saved = crate::sandbox::policy::load(&self.connection, project.as_str()).ok();
        let stale: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && !w.stopping)
            .filter(|(_, w)| saved.is_none() || w.checked != saved)
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return;
        }
        let root = stored.project_folder().canonicalize().ok();
        for id in stale {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let running =
                !worker.managed || worker.live || worker.kept_session || worker.busy.is_some();
            let narrowed = crate::sandbox::policy::narrowing(
                saved.as_ref(),
                root.as_deref(),
                running.then_some(worker.access),
                worker.tools.then_some(worker.network),
                worker.toolchains.as_deref().unwrap_or(&Toolchain::ALL),
            )
            .map(|cause| match cause {
                Narrowing::TurnedOff | Narrowing::NotAllowed if worker.toolchains.is_none() => {
                    Narrowed::Unrecorded
                }
                cause => Narrowed::Saved(cause),
            });
            let Some(narrowed) = narrowed else {
                worker.checked = saved.clone();
                continue;
            };
            worker.stopping = true;
            worker.held = true;
            worker.narrowed = Some(narrowed);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
    }

    /// Proves a managed pool account is signed in for this engine run, and that the
    /// provider did not reject that sign-in, with the same check every master turn uses.
    /// Takes the live access and the pool member.
    /// Returns the active host and the sign-in, or a plain refusal code: `master_sign_in_checking`
    /// while this engine run still re-checks a sign-in an earlier run proved (DEC-108).
    fn sign_in(
        &self,
        access: &ModelAccess,
        account: &CredentialRef,
    ) -> Result<(HostStamp, NativeAuthFact), String> {
        let connection = access
            .connections
            .iter()
            .find(|c| &c.id == account)
            .ok_or("team_account_left_pool")?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|_| "team_state_unavailable")?;
        let host = tx
            .query_row(
                "SELECT db_instance,engine_epoch FROM owner_meta WHERE singleton=1 AND schema_version=1 AND runtime_state='active'",
                [],
                |row| {
                    Ok(HostStamp {
                        db_instance: row.get(0)?,
                        engine_epoch: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|_| "team_state_unavailable")?
            .ok_or("team_engine_not_ready")?;
        let fact = match crate::storage::native_sign_in(&tx, &host, connection, None) {
            Ok(fact) => fact,
            Err(problem) => {
                if !self
                    .reproving
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::SeqCst))
                    || !crate::storage::sign_in_rechecking(&tx, &host, connection)
                        .map_err(|e| e.0)?
                {
                    return Err(problem.0);
                }
                if crate::model_gateway::sign_in_rejected(&tx, connection).map_err(|e| e.0)? {
                    return Err(SIGN_IN_REJECTED.into());
                }
                return Err(SIGN_IN_CHECKING.into());
            }
        };
        if crate::model_gateway::sign_in_rejected(&tx, connection).map_err(|e| e.0)? {
            return Err(SIGN_IN_REJECTED.into());
        }
        drop(tx);
        assert_eq!(fact.binding.engine_epoch, host.engine_epoch);
        Ok((host, fact))
    }

    /// Takes a managed agent's profile for its next turn under the engine's custody lock
    /// and reads the project's launch context for the fresh process.
    /// Returns none while another process holds the profile, so the turn waits.
    fn grant(
        &mut self,
        access: &ModelAccess,
        id: i64,
        project: &ProjectId,
    ) -> Result<Option<Box<Grant>>, String> {
        #[cfg(test)]
        self.grant_waited.remove(&id);
        let connection = self.workers[&id].connection.clone();
        let (host, fact) = match self.sign_in(access, &connection) {
            Ok(signed) => signed,
            Err(code) if code == "native_profile_in_use" || code == SIGN_IN_CHECKING => {
                #[cfg(test)]
                self.grant_waited.insert(id, code);
                return Ok(None);
            }
            Err(code) => return Err(sign_in_problem(&code)),
        };
        let generation = self.workers[&id].grants + 1;
        let operation = RequestId::new(format!("team-agent-{id}-turn-{generation}"))
            .map_err(|e| with_code("the turn could not be named", Some(&e.0)))?;
        let custody = self
            .custody
            .as_mut()
            .map_err(|e| with_code("the account custody is not available", Some(&e.0)))?;
        let profile = match custody.acquire_use(
            &host,
            &fact.binding,
            &operation,
            generation,
            NativeProfileUsePurpose::Execution,
        ) {
            Ok(profile) => profile,
            Err(problem) if problem.0 == "native_profile_in_use" => {
                #[cfg(test)]
                self.grant_waited.insert(id, problem.0);
                return Ok(None);
            }
            Err(problem) => {
                return Err(with_code(
                    "the account could not be handed to the agent",
                    Some(&problem.0),
                ));
            }
        };
        let layout = self
            .agent_layout(project, id)
            .map_err(|_| with_code("the agent worktree record could not be read", None))?;
        let context = self.context(
            project,
            id,
            self.workers[&id].role,
            self.workers[&id].tools,
            layout,
        )?;
        let access = crate::sandbox::policy::load(&self.connection, project.as_str())
            .map_err(|_| with_code("the project's agent access could not be read", None))?
            .access;
        if let Some(reason) = access.unavailable() {
            return Err(with_code(reason, Some("agent_access_unavailable")));
        }
        let worker = self.workers.get_mut(&id).expect("worker exists");
        worker.grants = generation;
        worker.access = access;
        Ok(Some(Box::new(Grant {
            profile,
            context,
            access,
        })))
    }

    /// Tells the owner once of each pair of agents whose waits under Wait would have closed
    /// a cycle, so a start went ahead without the file the other holds (OS-8). The pairs are
    /// marked told in the transaction that writes their notes, so a failure leaves every pair
    /// untold for the next sweep (T9-R2-L2). That transaction takes the write lock before it
    /// reads, as the cycle check does, so no other writer commits between the read and the
    /// marks (TEAM-16-L1). Inputs: the project and the time in ms. Output: Ok or the first
    /// failure.
    fn note_wait_cycles(&mut self, project: &ProjectId, now: i64) -> Result<(), EngineError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        for (starter, holder, file) in crate::coord::wait_cycle_notices(&tx, project, now)? {
            let starter = crate::team::events::label(&tx, starter)?;
            let holder = crate::team::events::label(&tx, holder)?;
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &wait_cycle_note(&starter, &holder, &file),
                MessageState::Note,
                now_ms(),
            )?;
        }
        tx.commit().map_err(error)
    }

    fn note(&mut self, project: &ProjectId, text: &str) -> Result<(), EngineError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let _ = append_message(
            &tx,
            project,
            MessageAuthor::Engine,
            text,
            MessageState::Note,
            now_ms(),
        )?;
        tx.commit().map_err(error)
    }

    fn master(&self, project: &ProjectId) -> Option<i64> {
        self.workers
            .iter()
            .find(|(_, w)| &w.project == project && w.role == AgentRole::Master && !w.stopping)
            .map(|(id, _)| *id)
    }

    /// Sends a project's oldest pending message to its master, starting a per-message master
    /// when none runs. Nothing is sent while a master of the project is still stopping or
    /// restarting, as after the live switch goes off, so the project never runs two masters
    /// at once, nor to a live master finishing its turn under Live terminal Off, which would
    /// drop it. A master that keeps running gets the owner's instruction edit at the start of
    /// its next message under the Next task choice (DEC-111 QR1), once. Takes the setup, live
    /// access and paused accounts.
    fn feed_master(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if super::master::live(&self.connection, project)? {
            return self.feed_live(project);
        }
        let settling = self.workers.values().any(|w| {
            &w.project == project && w.role == AgentRole::Master && (w.restarting || w.stopping)
        });
        let master = self.master(project);
        if settling
            || master.is_some_and(|id| self.workers[&id].live || self.workers[&id].busy.is_some())
        {
            return Ok(());
        }
        let pending: Option<(i64, String, String)> = self
            .connection
            .query_row(
                "SELECT id,author,text FROM team_messages WHERE project=?1 AND state='pending' AND id NOT IN (
                     SELECT message FROM recovery_runs WHERE message IS NOT NULL
                     AND state IN ('stopped','interrupted','resuming','failed'))
                 ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((message, author, text)) = pending else {
            return Ok(());
        };
        let (id, fresh) = match master {
            Some(id) => (id, false),
            None if self.panes(project) >= PANES_PER_PROJECT_MAX => return Ok(()),
            None => {
                let harness = stored.team.master.clone();
                let started = match self.account_for(access, project, &harness, paused) {
                    Ok(None) => return Ok(()),
                    Ok(Some(account)) => match self.spawn(
                        stored,
                        AgentRole::Master,
                        &harness,
                        stored.team.master_tools,
                        &account,
                        None,
                    )? {
                        Start::Started(id) => Ok(id),
                        Start::Held => return Ok(()),
                        Start::Refused(reason) => Err(reason),
                    },
                    Err(reason) => Err(reason),
                };
                match started {
                    Ok(id) => (id, true),
                    Err(reason) => {
                        self.connection
                            .execute(
                                "UPDATE team_messages SET state='failed' WHERE id=?1",
                                [message],
                            )
                            .map_err(error)?;
                        return self.note(
                            project,
                            &full_stop(format!("The master could not start: {reason}")),
                        );
                    }
                }
            }
        };
        if !fresh && self.end_session(id)? {
            return Ok(());
        }
        let grant = if self.workers[&id].managed {
            match self.grant(access, id, project) {
                Ok(Some(grant)) => Some(grant),
                Ok(None) => return Ok(()),
                Err(reason) => {
                    let worker = self.workers.get_mut(&id).expect("master worker exists");
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.connection
                        .execute(
                            "UPDATE team_messages SET state='failed' WHERE id=?1",
                            [message],
                        )
                        .map_err(error)?;
                    return self.note(
                        project,
                        &full_stop(format!(
                            "The master could not start: {}",
                            bound_text(&reason, 300)
                        )),
                    );
                }
            }
        } else {
            None
        };
        let fresh = fresh || grant.is_some();
        let speaker = if author == "owner" {
            "Message from the owner"
        } else {
            "Message from the engine"
        };
        let mut prompt = String::new();
        let team_tool = self.workers[&id].team_tool;
        let finished = super::events::finished_agents(&self.connection, project, now_ms())?;
        let told: BTreeSet<i64> = finished.iter().map(|(agent, _)| *agent).collect();
        let lines: Vec<String> = finished.into_iter().map(|(_, line)| line).collect();
        if !fresh && !told.is_subset(&self.workers[&id].told_finished) {
            prompt.push_str(&wake_text(&lines));
        }
        if fresh {
            prompt.push_str(&framing(
                stored,
                team_tool,
                &super::available_harnesses(access, project),
                &lines,
            ));
            let history =
                conversation_before(&self.connection, project, message, HISTORY_BYTES_MAX)?;
            if !history.is_empty() {
                prompt.push_str("Conversation so far:\n");
                prompt.push_str(&history);
                prompt.push('\n');
            }
        }
        let (heard, newest) = super::direct::notes(&self.connection, project)?;
        prompt.push_str(&heard);
        if team_tool {
            self.log_master_asks(project, id)?;
            let pending = crate::questions::pending(&self.connection, project).unwrap_or_default();
            let owner_message = (author == "owner")
                .then(|| u64::try_from(message).ok())
                .flatten();
            prompt.push_str(&tool::questions::open_line(
                &pending,
                &crate::questions::Asker::Master(id),
                owner_message,
            ));
        }
        prompt.push_str(&format!("{speaker}:\n{text}"));
        if author == "owner"
            && let Some(reminder) = &self.workers[&id].starts.text
            && crate::coord::settings::read(&self.connection, project)?.task_start
                == crate::coord::settings::TaskStart::On
        {
            prompt.push_str("\n\n");
            prompt.push_str(reminder);
        }
        if !delivery::intend(&self.connection, Busy::Message(message), id, &text, newest)? {
            return Ok(());
        }
        let home = agent_home(&self.state, id);
        let edit = match fresh {
            true => None,
            false => self.next_message_edit(project, id, &home),
        };
        let edit = edit_in_front(&mut prompt, edit, self.turn_prompt_max(id));
        if team_tool {
            tool::clear(&self.tool_record(id));
        }
        let bound = turn_bound(
            super::time_limits(&self.connection, project)?,
            self.workers[&id].ran(),
        );
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if worker
            .orders
            .send(Order::Turn(
                prompt,
                grant,
                TurnRules {
                    limit: bound.map(|(limit, _)| limit),
                    background: super::BackgroundAtEnd::Wait,
                },
            ))
            .is_err()
        {
            return delivery::ended(&self.connection, Some(Busy::Message(message)), false);
        }
        worker.bound = bound.map(|(_, by)| by);
        worker.told_finished = told;
        worker.busy = Some(Busy::Message(message));
        if let Some(edit) = &edit {
            let _ = crate::instruction_files::delivered(edit, &home);
        }
        self.connection
            .execute(
                "UPDATE team_messages SET state='sent' WHERE id=?1",
                [message],
            )
            .map_err(error)?;
        if let Some(newest) = newest {
            super::direct::delivered(&self.connection, project, newest)?;
        }
        self.mark_agent(id, AgentStatus::Working, None)
    }

    /// Keeps a project's live master running while its switch is on: starts it on a ready
    /// pool account of the master harness, briefs it once its thread waits, and stops a
    /// master whose mode or session no longer matches the switch. Nothing starts while a
    /// master of the project is still stopping or restarting, so the next start finds it
    /// stopped and starts it again in its own worktree and home. A master that cannot start
    /// leaves one team note per distinct reason. A start after the master stopped for an
    /// owner instruction edit (DEC-111 QR1 Restart) never counts toward the starts that turn
    /// the live master off. The owner's Live terminal Off ends the master at its next stop,
    /// once no turn is open and the owner is not typing, and clears those starts, so switching
    /// it off and on never counts as a crash. Takes the setup, live access and paused accounts.
    fn keep_live(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let on = super::master::live(&self.connection, project)?;
        if self.workers.values().any(|w| {
            &w.project == project && w.role == AgentRole::Master && (w.restarting || w.stopping)
        }) {
            return Ok(());
        }
        let session = crate::master_sessions::live_session(&self.connection, project)?;
        if let Some(id) = self.master(project) {
            let chosen_off = !on
                && self.workers[&id].live
                && super::master::switch(&self.connection, project)?.0 == Some(true);
            let held = chosen_off && !self.live_master_idles(id, project, true);
            let worker = self.workers.get_mut(&id).expect("master worker exists");
            let moved = on && worker.live && worker.session != session;
            if worker.live != on || moved {
                if worker.busy.is_none() && !held {
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                }
                if moved || (chosen_off && worker.stopping) {
                    self.live_starts.remove(project);
                }
                return Ok(());
            }
            if on && !worker.briefed {
                return self.brief(stored, access, id);
            }
            return self.switch_when_idle(stored, access, paused, id);
        }
        let harness = stored.team.master.clone();
        let live_capable = launch_harness(&harness)
            .is_some_and(|launch| self.live_master(project, AgentRole::Master, launch));
        let planned = self.master_restarts.remove(project);
        if !on || !live_capable || self.panes(project) >= PANES_PER_PROJECT_MAX {
            return Ok(());
        }
        let starts = self.live_starts.entry(project.clone()).or_default();
        starts.retain(|at| at.elapsed() < LIVE_STARTS_WINDOW);
        if !planned && starts.len() >= LIVE_STARTS_MAX {
            return self.live_off(
                project,
                &format!(
                    "it had to start {LIVE_STARTS_MAX} times within {} minutes",
                    LIVE_STARTS_WINDOW.as_secs() / 60
                ),
            );
        }
        let account = match self.account_for(access, project, &harness, paused) {
            Ok(Some(account)) => account,
            Ok(None) => {
                if planned {
                    self.master_restarts.insert(project.clone());
                }
                return self.wait_line(project, super::master::SIGN_INS_CHECKING);
            }
            Err(reason) => {
                return self.live_off(
                    project,
                    &format!("it could not start: {}", bound_text(&reason, 300)),
                );
            }
        };
        if !planned {
            self.live_starts
                .entry(project.clone())
                .or_default()
                .push(Instant::now());
        }
        match self.start_master(stored, access, &harness, &account, session.as_deref())? {
            Start::Started((id, note)) => {
                super::master::problem(&self.connection, project, None)?;
                if let Some(note) = note {
                    self.note(project, &note)?;
                }
                self.brief(stored, access, id)
            }
            Start::Held => {
                if planned {
                    self.master_restarts.insert(project.clone());
                } else if let Some(starts) = self.live_starts.get_mut(project) {
                    starts.pop();
                }
                Ok(())
            }
            Start::Refused(reason) => self.live_off(
                project,
                &format!("it could not start: {}", bound_text(&reason, 300)),
            ),
        }
    }

    /// Applies a saved master harness change to the project's running live master under the
    /// owner's When the master is idle choice: once it is quiet with no turn of its own it
    /// stops for a planned restart that `keep_live` starts on the new harness, which writes
    /// the switch and its divider. Under At the next start, or while no pool account can run
    /// the new harness, the master keeps running. Takes the setup, the access, the paused
    /// accounts and the master's id. Returns any SQLite failure.
    fn switch_when_idle(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
        id: i64,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let worker = &self.workers[&id];
        assert!(worker.role == AgentRole::Master && &worker.project == project);
        let Some(saved) = launch_harness(&stored.team.master) else {
            return Ok(());
        };
        if !worker.live
            || launch_harness(&worker.harness) == Some(saved)
            || !self.live_master(project, AgentRole::Master, saved)
            || super::apply_harness_switch(&self.connection, project)?
                != super::SwitchApply::WhenIdle
        {
            return Ok(());
        }
        let quiet = super::master::quiet(
            &self.connection,
            self.agents.as_deref().map(Agents::typing),
            project,
            now_ms(),
        );
        let idle =
            worker.busy.is_none() && worker.own_turn.is_none() && worker.turn_started.is_none();
        if !idle
            || !quiet
            || !matches!(
                self.account_for(access, project, saved, paused),
                Ok(Some(_))
            )
        {
            return Ok(());
        }
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        worker.stopping = true;
        let _ = worker.orders.send(Order::Stop);
        self.master_restarts.insert(project.clone());
        Ok(())
    }

    /// Starts a project's live master for its live session. A session whose harness
    /// conversation can continue starts the master that kept it, in its worktree and home,
    /// resuming that conversation; otherwise the project's last master on this harness
    /// starts again in its own worktree and home, so nothing it left there is lost, or a new
    /// one starts. A session with earlier turns that cannot continue gets a fresh harness
    /// conversation seeded with them, and its note says so. A session whose conversation ran
    /// on another harness follows the owner's On harness switch setting: Carry over seeds
    /// the new conversation with a bounded handoff, Start fresh seeds nothing, and either
    /// note marks the switch. Takes the setup, the model access, the master harness, the account
    /// and the live session. Returns the agent id with the note to show, Held while a projects
    /// home move holds the project folder, or the plain reason it could not start.
    fn start_master(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        harness: &str,
        account: &TeamAccount,
        session: Option<&str>,
    ) -> Result<Start<(i64, Option<String>)>, EngineError> {
        let project = &stored.project_id;
        let tools = stored.team.master_tools;
        let launch = launch_harness(harness).unwrap_or(harness);
        let session = session.map(SessionId::new).transpose().map_err(error)?;
        let switched = match &session {
            Some(session) => {
                crate::master_sessions::switched_from(&self.connection, project, session, launch)?
            }
            None => None,
        };
        let mut switch_note = None;
        let mut switch_mode = None;
        let mut plan = match (&session, switched) {
            (Some(session), Some(from)) => {
                let to = crate::master_sessions::harness_label(launch);
                let mode = super::on_harness_switch(&self.connection, project)?;
                switch_mode = Some(mode);
                match mode {
                    super::SwitchMode::Carry => {
                        let kept = super::master::session_conversation(
                            &self.connection,
                            project,
                            session.as_str(),
                        )?;
                        ResumePlan::Fresh {
                            seed: crate::master_sessions::handoff(
                                &self.connection,
                                project,
                                session,
                                kept.as_ref(),
                                crate::master_sessions::harness_label(&from),
                            )?,
                            note: crate::master_sessions::switched_note(to, mode),
                        }
                    }
                    super::SwitchMode::Fresh => {
                        switch_note = Some(crate::master_sessions::switched_note(to, mode));
                        ResumePlan::New
                    }
                }
            }
            (Some(session), None)
                if crate::master_sessions::fresh_after_switch(
                    &self.connection,
                    project,
                    session,
                    launch,
                )? =>
            {
                ResumePlan::New
            }
            (Some(session), None) => {
                let pool = match eligible_accounts(access, project, launch) {
                    Accounts::Usable(pool) => pool,
                    Accounts::NoPool => Vec::new(),
                };
                crate::master_sessions::resume_harness(
                    &self.connection,
                    project,
                    session,
                    launch,
                    &|kept| super::resumes_on(&pool, account, kept),
                )?
            }
            (None, _) => ResumePlan::New,
        };
        if let ResumePlan::Resume {
            agent,
            conversation,
            record,
        } = &plan
        {
            let resume = Resume {
                id: conversation.clone(),
                record: record.clone(),
            };
            self.resuming = Some((*agent, resume));
            let revived = self.revive(
                stored,
                *agent,
                AgentRole::Master,
                harness,
                tools,
                account,
                None,
            );
            self.resuming = None;
            if let Ok(id) = revived? {
                assert_eq!(id, *agent);
                return Ok(Start::Started(self.seat_session(
                    id,
                    session.as_ref(),
                    None,
                )));
            }
            let session = session.as_ref().expect("only a session resumes");
            let kept =
                super::master::session_conversation(&self.connection, project, session.as_str())?;
            plan = crate::master_sessions::fresh_harness(
                &self.connection,
                project,
                session,
                kept.as_ref(),
            )?;
        }
        let seeded = match plan {
            ResumePlan::Fresh { seed, note } => Some((seed, note)),
            ResumePlan::New => None,
            ResumePlan::Resume { .. } => unreachable!("a resume plan was handled above"),
        };
        let last = last_master(&self.connection, project, harness)?;
        let started = match last {
            Some(last) => {
                match self.revive(
                    stored,
                    last,
                    AgentRole::Master,
                    harness,
                    tools,
                    account,
                    None,
                )? {
                    Ok(id) => {
                        assert_eq!(id, last);
                        Some(id)
                    }
                    Err(_) => None,
                }
            }
            None => None,
        };
        let started = match started {
            Some(id) => id,
            None => match self.spawn(stored, AgentRole::Master, harness, tools, account, None)? {
                Start::Started(id) => id,
                Start::Held => return Ok(Start::Held),
                Start::Refused(reason) => return Ok(Start::Refused(reason)),
            },
        };
        if let (Some(session), Some(mode)) = (&session, switch_mode) {
            super::record_switch(&self.connection, project, session.as_str(), launch, mode)?;
        }
        let (id, note) = self.seat_session(started, session.as_ref(), seeded);
        Ok(Start::Started((id, note.or(switch_note))))
    }

    /// Binds a live master that just started to the session it runs, with the seed its
    /// briefing carries for a fresh conversation of a session with earlier turns. Takes the
    /// agent, the session and the seed with its note. Returns the agent and the note.
    fn seat_session(
        &mut self,
        id: i64,
        session: Option<&SessionId>,
        seeded: Option<(String, String)>,
    ) -> (i64, Option<String>) {
        let worker = self
            .workers
            .get_mut(&id)
            .expect("a started master has a worker");
        assert!(worker.live && !worker.briefed);
        worker.session = session.map(|session| session.as_str().to_owned());
        let (seed, note) = seeded.unzip();
        worker.seed = seed;
        (id, note)
    }

    /// Turns a project's live master off after it could not keep running, so the runtime
    /// starts it again only once the owner turns it on, and says why in one team note and
    /// the master's problem line. Takes the project and the reason. Returns any SQLite failure.
    fn live_off(&mut self, project: &ProjectId, reason: &str) -> Result<(), EngineError> {
        let line = full_stop(format!(
            "{LIVE_STOPPED}: {reason}. Start it again from the master view"
        ));
        self.live_ended(project, &line)
    }

    /// Turns a project's live master off, so the runtime starts it again only once the owner
    /// turns it on, and says why in one team note and the master's problem line. Takes the
    /// project and the line. Returns any SQLite failure.
    fn live_ended(&mut self, project: &ProjectId, line: &str) -> Result<(), EngineError> {
        super::master::set_live(&self.connection, project, false)?;
        self.live_starts.remove(project);
        if super::master::problem(&self.connection, project, Some(line))? {
            self.note(project, line)?;
        }
        Ok(())
    }

    /// Starts a live master that waits for its start: hands it the instructions its harness
    /// reads at launch as system context, never as a typed message, with the seed of a fresh
    /// conversation for a session with earlier turns, and the signed-in account's profile on
    /// a managed account. The master then waits for the owner's first line. Takes the setup,
    /// live access and the agent. Returns once sent; a profile still in use or checked
    /// waits for the next tick with its wait line, and a refused account stops the master
    /// with a note and clears that line.
    fn brief(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        id: i64,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let grant = if self.workers[&id].managed {
            match self.grant(access, id, project) {
                Ok(Some(grant)) => Some(grant),
                Ok(None) => {
                    let connection = self.workers[&id].connection.clone();
                    let checking = self
                        .sign_in(access, &connection)
                        .is_err_and(|code| code == SIGN_IN_CHECKING);
                    return self.wait_line(
                        project,
                        if checking {
                            super::master::SIGN_INS_CHECKING
                        } else {
                            super::master::PROFILE_IN_USE
                        },
                    );
                }
                Err(reason) => {
                    let worker = self.workers.get_mut(&id).expect("master worker exists");
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    super::master::problem(&self.connection, project, None)?;
                    return self.note(
                        project,
                        &full_stop(format!(
                            "The live master could not start: {}",
                            bound_text(&reason, 300)
                        )),
                    );
                }
            }
        } else {
            None
        };
        let mut instructions = live_instructions(stored, self.workers[&id].team_tool);
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if let Some(seed) = worker.seed.take() {
            assert!(seed.len() <= crate::master_sessions::SEED_BYTES_MAX);
            instructions.push_str("\n\n");
            instructions.push_str(&seed);
        }
        assert!(instructions.len() <= crate::launch_context::LIVE_INSTRUCTIONS_BYTES_MAX);
        if worker
            .orders
            .send(Order::Start(instructions, grant))
            .is_ok()
        {
            worker.briefed = true;
            super::master::problem(&self.connection, project, None)?;
        }
        Ok(())
    }

    /// Says once, in the live master's problem line and one team note, why it waits: for
    /// this engine run's sign-in re-checks of the pool (DEC-108) or for its account's
    /// profile another process holds (LMS-1), where it stays on, counts no start and starts
    /// on a later tick, or for its app to read keys while an owner line waits (F2). Takes
    /// the project and the wait line. Returns any SQLite failure.
    fn wait_line(&mut self, project: &ProjectId, line: &str) -> Result<(), EngineError> {
        if super::master::problem(&self.connection, project, Some(line))? {
            self.note(project, line)?;
        }
        Ok(())
    }

    /// Hands the signed-in account to each kept native-lead session of the project that has
    /// not had it yet, such as a lead an earlier engine left running, so the lead and its
    /// teammates have a credential again without waiting for a task. A profile still in use
    /// waits for the next tick; a refused sign-in stops the lead and leaves a team note.
    /// Takes the project and the live access. Returns any SQLite failure.
    fn hand_leads(&mut self, project: &ProjectId, access: &ModelAccess) -> Result<(), EngineError> {
        let waiting: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| {
                &w.project == project
                    && w.kept_session
                    && !w.briefed
                    && w.busy.is_none()
                    && !w.stopping
            })
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            assert!(self.workers[&id].managed && !self.workers[&id].live);
            let grant = match self.grant(access, id, project) {
                Ok(Some(grant)) => grant,
                Ok(None) => continue,
                Err(reason) => {
                    let worker = self.workers.get_mut(&id).expect("lead worker exists");
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.note(
                        project,
                        &full_stop(format!(
                            "Team agent {id} could not get its signed-in account: {}",
                            bound_text(&reason, 300)
                        )),
                    )?;
                    continue;
                }
            };
            let worker = self.workers.get_mut(&id).expect("lead worker exists");
            if worker
                .orders
                .send(Order::Start(String::new(), Some(grant)))
                .is_ok()
            {
                worker.briefed = true;
            }
        }
        Ok(())
    }

    /// Answers the team calls a project's live master made since the last tick, or only the
    /// memory calls of a master with tools running without its live terminal.
    /// Takes the setup and live access. Returns any SQLite failure.
    fn serve_live_calls(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
    ) -> Result<(), EngineError> {
        let Some(id) = self.master(&stored.project_id) else {
            return Ok(());
        };
        if !self.workers[&id].team_tool {
            return Ok(());
        }
        let mailbox = self.mailbox(id);
        if !self.workers[&id].live {
            return super::master::serve_memory_calls(
                &mut self.connection,
                &stored.project_id,
                &mailbox,
            )
            .map(drop);
        }
        let agents = self.state.join("agents");
        let answering = super::master::Answering {
            access,
            stored,
            landing: &self.root,
            agents: &agents,
            events: self.events.as_ref(),
            host: self.terminal.as_deref(),
        };
        super::master::serve_calls(&mut self.connection, &answering, id, &mailbox).map(drop)
    }

    /// Answers the team calls a project's sub-agents made since the last tick, from each
    /// running sub-agent's own mailbox, which names the caller, with the sub-agent's label
    /// as the origin of what it saves: past conversations and memory, the work verbs and
    /// the lead calls that reach only the workers it started (DEC-104, N1, N2). Takes the
    /// setup and live access.
    fn serve_agent_calls(&mut self, stored: &StoredSetup, access: &ModelAccess) {
        #[cfg(unix)]
        self.tend_callers(&stored.project_id);
        if self.delegation_tool.is_none() {
            return;
        }
        let project = &stored.project_id;
        let agents = self.state.join("agents");
        let answering = super::master::Answering {
            access,
            stored,
            landing: &self.root,
            agents: &agents,
            events: self.events.as_ref(),
            host: self.terminal.as_deref(),
        };
        let subs: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, worker)| {
                &worker.project == project && worker.role == AgentRole::Sub && worker.tools
            })
            .map(|(id, _)| *id)
            .collect();
        let host = self.terminal.as_deref();
        for id in subs {
            let label = super::events::label(&self.connection, id)
                .unwrap_or_else(|_| AgentRole::Sub.agent_label(id));
            let mailbox = self.mailbox(id);
            #[cfg(unix)]
            let mut callers = self.callers.get_mut(&id).and_then(Option::as_mut);
            super::history::serve_agent_calls(
                &mut self.connection,
                project,
                (id, &label),
                &agents,
                &mailbox,
                &mut |connection, call, digest| {
                    #[cfg(unix)]
                    let pane = callers
                        .as_mut()
                        .zip(digest)
                        .and_then(|(callers, digest)| callers.take(call, digest));
                    #[cfg(not(unix))]
                    let pane: Option<(String, u32)> = {
                        let _ = (call, digest);
                        None
                    };
                    team_caller(
                        connection,
                        host,
                        project,
                        (id, &label),
                        pane.as_ref().map(|(pane, pid)| (pane.as_str(), *pid)),
                    )
                },
                &mut |connection, caller, call| {
                    super::master::answer_sub(connection, &answering, caller, call)
                        .unwrap_or_else(|_| Some((LEAD_UNREAD.to_owned(), true)))
                },
            );
        }
    }

    /// Keeps the caller socket of each native lead of a project bound and answered (DEC-111
    /// Q18): binds it again in the folder it was pinned to when it is gone, replaced or was
    /// refused on an earlier tick, under the pane pins of the socket it replaces, also kept
    /// across a refused bind, logging only a change; then serves each bound socket once,
    /// whether or not the lead has the team tool, so its hooks and monitors learn their pane
    /// and who the engine's rows put there, its teammates' hooks and monitors get their relay
    /// requests answered for their own pane (relay over the caller socket, OQ-7 B), and its
    /// team servers get their calls named. Takes the project.
    #[cfg(unix)]
    fn tend_callers(&mut self, project: &ProjectId) {
        let workers = &self.workers;
        self.callers.retain(|id, _| workers.contains_key(id));
        self.caller_folders.retain(|id, _| workers.contains_key(id));
        self.caller_pins.retain(|id, _| workers.contains_key(id));
        let leads: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, worker)| &worker.project == project && worker.role == AgentRole::Sub)
            .map(|(id, _)| *id)
            .collect();
        for id in &leads {
            let (was_bound, pins) = match self.callers.get(id) {
                Some(Some(callers)) if callers.intact() => continue,
                Some(Some(callers)) => (Some(true), callers.pins()),
                Some(None) => (Some(false), self.caller_pins.remove(id).unwrap_or_default()),
                None if self.stored_mode(*id).ok().flatten().as_deref()
                    == Some(crate::coord::settings::SubagentMode::Native.as_str()) =>
                {
                    (None, Vec::new())
                }
                None => continue,
            };
            let pinned = self.caller_folders.get(id).copied();
            match self.bind_callers(*id, pinned) {
                Ok(bound) => {
                    if was_bound.is_some() {
                        eprintln!("team_caller_socket_rebound");
                    }
                    self.caller_folders.insert(*id, bound.folder());
                    self.callers.insert(*id, Some(bound.with_pins(pins)));
                }
                Err(error) => {
                    if was_bound != Some(false) {
                        eprintln!("team_caller_socket_not_bound kind={:?}", error.kind());
                    }
                    self.caller_pins.insert(*id, pins);
                    self.callers.insert(*id, None);
                }
            }
        }
        let host = self.terminal.as_deref();
        let connection = &self.connection;
        let workers = &mut self.workers;
        for id in leads {
            if let Some(callers) = self.callers.get_mut(&id).and_then(Option::as_mut) {
                let lead = AgentId::team(id).ok();
                let mut none = crate::coord::relay::TaskStarts::default();
                let starts = workers
                    .get_mut(&id)
                    .map_or(&mut none, |worker| &mut worker.starts);
                callers.serve(
                    &mut || listed_panes(host, lead.as_ref()),
                    &mut |pane, pid| pane_whom(connection, host, project, id, (pane, pid)),
                    &mut |pane, _, request, out| {
                        let _ = crate::coord::relay::answer(
                            connection,
                            crate::coord::relay::ForLead {
                                project,
                                id,
                                starts: &mut *starts,
                            },
                            pane,
                            request,
                            out,
                            now_ms(),
                        );
                    },
                );
            }
        }
    }

    /// Runs the Claude Code relay (K2a) of each running Claude agent of a project whose home
    /// holds the relay plugin: delivers its and its native teammates' messages through the
    /// mailbox, split into the lead's part and its teammates' for a native lead, applies
    /// their reported activity and waits, answers their overlap questions and, at most once
    /// each `events::SCAN_EVERY` per project, marks read receipts from their transcripts,
    /// and those of each running Codex agent from its rollouts. A relay or receipt read that
    /// fails is skipped until the next tick, like the agents' team calls, and so is a relay
    /// whose agent's stored mode cannot be read, never served in the single layout a native
    /// lead's teammates could write into. Takes the project.
    fn serve_relays(&mut self, project: &ProjectId) {
        let now = Instant::now();
        let receipts = self
            .receipts_next
            .get(project)
            .is_none_or(|next| now >= *next);
        if receipts {
            self.receipts_next
                .insert(project.clone(), now + super::events::SCAN_EVERY);
        }
        let served: Vec<(i64, Option<crate::coord::settings::HarnessKind>)> = self
            .workers
            .iter()
            .filter(|(_, worker)| &worker.project == project && !worker.stopping)
            .map(|(id, worker)| {
                (
                    *id,
                    crate::coord::settings::HarnessKind::from_harness(&worker.harness),
                )
            })
            .collect();
        for (id, kind) in served {
            let home = agent_home(&self.state, id);
            if receipts && kind == Some(crate::coord::settings::HarnessKind::Codex) {
                let _ = crate::coord::relay::codex_receipts(&self.connection, id, &home, now_ms());
            }
            if kind != Some(crate::coord::settings::HarnessKind::ClaudeCode)
                || !crate::coord::relay::installed(&home)
            {
                continue;
            }
            let layout = match self.stored_mode(id) {
                Ok(mode)
                    if mode.as_deref()
                        == Some(crate::coord::settings::SubagentMode::Native.as_str()) =>
                {
                    crate::coord::relay::Layout::Split
                }
                Ok(_) => crate::coord::relay::Layout::Single,
                Err(_) => continue,
            };
            let _ = crate::coord::relay::serve(
                &self.connection,
                project,
                id,
                &home,
                &self.mailbox(id),
                layout,
                receipts,
                now_ms(),
            );
        }
    }

    /// Types into each running sub-agent of a project how the owner settled its questions,
    /// the owner's own words from the questions log, once each, each kept in the agent's
    /// Messages timeline as the engine's notice. A note the terminal does not take stays for
    /// the next tick; a note longer than one prompt points to the master, which has it in
    /// full. Takes the project.
    fn tell_agent_answers(&self, project: &ProjectId) {
        let Some(host) = self.terminal.as_deref() else {
            return;
        };
        let asking: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, worker)| &worker.project == project && worker.ask_tool && !worker.stopping)
            .map(|(id, _)| *id)
            .collect();
        for id in asking {
            let Ok(notes) = crate::questions::agent_notes(&self.connection, project, id) else {
                continue;
            };
            let mut typed = Vec::new();
            for note in notes {
                let text = if note.text.len() <= super::events::SEND_BYTES_MAX {
                    note.text
                } else {
                    format!(
                        "The owner answered your question {}. The answer is longer than CrazyAgent types into a terminal, so ask your master for it.",
                        note.question
                    )
                };
                match super::events::send_notice_to_id(&self.connection, host, project, id, &text) {
                    Ok(Ok(())) => typed.push(note.question),
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            if !typed.is_empty()
                && let Err(problem) =
                    crate::questions::agent_notes_delivered(&self.connection, project, id, &typed)
            {
                eprintln!(
                    "questions_agent_notes_unmarked agent={id} code={}",
                    problem.0
                );
            }
        }
    }

    /// Logs the questions a drop-file master asked with its `ask` tool and records the chat
    /// answers it named with `record_answer`, each as that master's own, whatever its turn's
    /// outcome, and notes each call the store refused so the master hears why on its next
    /// turn. Takes the project and the master's team agent id. Returns any SQLite failure of
    /// a note.
    fn log_master_asks(&mut self, project: &ProjectId, id: i64) -> Result<(), EngineError> {
        let refused = tool::questions::apply_master_calls(
            &self.connection,
            project,
            id,
            &self.tool_record(id),
        );
        for line in refused {
            self.note(project, &line)?;
        }
        Ok(())
    }

    /// Types the oldest waiting team message into a project's live master once it is idle
    /// and the owner is not typing, with the owner's prompts to sub-agents it has not heard,
    /// and under the Next task choice the owner's instruction edit first, once, cut to the
    /// room the input limit leaves beside the message; when not even the cut block fits, a
    /// team note says the edit waits for a later message, once per edited text until it is
    /// delivered (DEC-111 QR1). An owner message ends with the master's Task-start reminder
    /// while the setting is On and it still fits the input limit beside the message and the
    /// edit, which it never cuts (DEC-116, OP4-L1). While the master's app
    /// does not read keys the message waits, and past `READY_WAIT` one note and the problem
    /// line say so (F2); a typed message takes that line down, also one an earlier engine
    /// stored. Takes the project. Returns any SQLite failure; a message that cannot be typed
    /// is failed with a team note, one that must wait stays pending.
    fn feed_live(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        let Some(id) = self.master(project) else {
            return Ok(());
        };
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        let worker = &self.workers[&id];
        if !worker.live || !worker.briefed {
            return Ok(());
        }
        let reads_keys = worker.input_ready;
        let pending: Option<(i64, String, String)> = self
            .connection
            .query_row(
                "SELECT id,author,text FROM team_messages WHERE project=?1 AND state='pending' AND id NOT IN (
                     SELECT message FROM recovery_runs WHERE message IS NOT NULL
                     AND state IN ('stopped','interrupted','resuming','failed'))
                 ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((message, author, text)) = pending else {
            return self.unread_ended(project);
        };
        if !reads_keys {
            return self.unread_wait(project, id);
        }
        let (heard, newest) = super::direct::notes(&self.connection, project)?;
        let (typed, noted) = live_input(&heard, &author, &text);
        let newest = newest.filter(|_| noted);
        let home = agent_home(&self.state, id);
        let edit = self
            .next_message_edit(project, id, &home)
            .filter(|_| super::master::typable(&typed));
        let block = edit.as_ref().and_then(|edit| {
            edit.message_within(super::master::INPUT_TEXT_MAX.saturating_sub(typed.len()))
        });
        let missed = match (&edit, &block) {
            (Some(edit), None) => Some(edit.text_hash()),
            _ => None,
        };
        let (edit, mut typed) = match block {
            Some(block) => (edit, format!("{block}{}", typed.trim_start())),
            None => (None, typed),
        };
        if author == "owner"
            && let Some(reminder) = &self.workers[&id].starts.text
            && typed.len() + "\n\n".len() + reminder.len() <= super::master::INPUT_TEXT_MAX
            && crate::coord::settings::read(&self.connection, project)?.task_start
                == crate::coord::settings::TaskStart::On
        {
            typed.push_str("\n\n");
            typed.push_str(reminder);
        }
        assert!(edit.is_none() || super::master::typable(&typed));
        let state = match super::master::master_input_when(
            &self.connection,
            host.as_ref(),
            self.agents.as_deref().map(Agents::typing),
            project,
            &typed,
            || delivery::intend(&self.connection, Busy::Message(message), id, &text, newest),
        ) {
            Ok(()) => "answered",
            Err(super::master::InputRefused::Invalid) => "failed",
            Err(super::master::InputRefused::Unready) => return self.unread_wait(project, id),
            Err(_) => return Ok(()),
        };
        self.connection
            .execute(
                "UPDATE team_messages SET state=?2 WHERE id=?1",
                params![message, state],
            )
            .map_err(error)?;
        delivery::ended(
            &self.connection,
            Some(Busy::Message(message)),
            state == "answered",
        )?;
        if state == "failed" {
            return self.note(
                project,
                "A team message could not be typed into the master because it is empty or not plain text.",
            );
        }
        self.unread.remove(project);
        super::master::clear_problem(&self.connection, project, super::master::MASTER_NOT_READING)?;
        if let Some(edit) = &edit {
            let _ = crate::instruction_files::delivered(edit, &home);
        }
        match missed {
            Some(hash) if self.waited_for.insert(id, hash) != Some(hash) => {
                self.note(project, INSTRUCTIONS_WAIT_FOR_ROOM)?;
            }
            Some(_) => {}
            None => {
                self.waited_for.remove(&id);
            }
        }
        if let Some(newest) = newest {
            super::direct::delivered(&self.connection, project, newest)?;
        }
        Ok(())
    }

    /// Counts how long team messages have waited for a live master's app to read keys,
    /// from the first refused look under this master; past `READY_WAIT` while an owner line
    /// waits it says so once with the wait line (F2). Takes the project and the master.
    /// Returns any SQLite failure.
    fn unread_wait(&mut self, project: &ProjectId, master: i64) -> Result<(), EngineError> {
        let since = match self.unread.get(project) {
            Some(&(agent, since)) if agent == master => since,
            _ => {
                self.unread
                    .insert(project.clone(), (master, Instant::now()));
                return Ok(());
            }
        };
        if since.elapsed() < crate::harnesses::terminal_session::READY_WAIT {
            return Ok(());
        }
        let owner_waits: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_messages WHERE project=?1 AND state='pending' AND author='owner')",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if owner_waits {
            self.wait_line(project, super::master::MASTER_NOT_READING)?;
        }
        Ok(())
    }

    /// Ends a project's wait for its live master's app to read keys once nothing waits,
    /// and takes its wait line down. Takes the project. Returns any SQLite failure.
    fn unread_ended(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        if self.unread.remove(project).is_some() {
            super::master::clear_problem(
                &self.connection,
                project,
                super::master::MASTER_NOT_READING,
            )?;
        }
        Ok(())
    }

    /// Hands the tasks a tools-off live master wrote as reply lines to the team, each
    /// through `admit` like any master task; refused ones are listed in one team note.
    /// Takes the project and the turn's reply text. Returns any SQLite failure.
    fn admit_reply_lines(&mut self, project: &ProjectId, text: &str) -> Result<(), EngineError> {
        let proposed = parse_delegations(text);
        if proposed.is_empty() {
            return Ok(());
        }
        let (Some(stored), Some(access)) = (self.stored(project)?, (self.access)()) else {
            return Ok(());
        };
        let now = now_ms();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let mut refused = Vec::new();
        for delegation in &proposed {
            if let Err(code) = admit(
                &tx,
                &access,
                &stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Master,
                None,
                now,
            )? {
                refused.push(format!(
                    "- {}: {}",
                    bound_text(&delegation.prompt, 200),
                    refusal(code)
                ));
            }
        }
        if !refused.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!(
                    "The live master's tasks were not handed off:\n{}",
                    refused.join("\n")
                ),
                MessageState::Note,
                now,
            )?;
        }
        tx.commit().map_err(error)
    }

    /// Tells whether a sub-agent's task turn that just ended is asked once more for its
    /// reply: it ended well with no reply text, the project's re-ask setting is on and the
    /// task has not had its re-ask yet (DEC-111). Takes the agent, its task, the turn's text
    /// and how it ended. Returns the answer, or any SQLite failure.
    fn reasks(&self, id: i64, task: i64, text: &str, end: TurnEnd) -> Result<bool, EngineError> {
        let worker = &self.workers[&id];
        if worker.role != AgentRole::Sub
            || end != TurnEnd::Success
            || !text.trim().is_empty()
            || worker.reasked == Some(task)
        {
            return Ok(false);
        }
        super::reask_empty_reply(&self.connection, &worker.project)
    }

    /// Sends each of the project's sub-agents whose task turn ended with no reply text its
    /// one re-ask turn, with the grant and turn rules a task turn gets. A sub-agent that
    /// cannot take it finishes the task with no reply text, as with the setting off; one on
    /// a rate-limited connection waits for it, as a task does, and a kept native lead waits
    /// while its app runs a turn of its own, as instruction edits do (RER-L1). A kept lead
    /// whose teammates still work is not asked or held: its task finishes as with the
    /// setting off and it turns again when their results arrive (RER2-L1). A message on its
    /// way to the lead does not hold the re-ask: typing that message waits for the lead to
    /// be idle. Takes the project setup, the live model access and the paused connections.
    /// Returns any SQLite failure.
    fn send_reasks(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let due: Vec<(i64, i64)> = self
            .workers
            .iter()
            .filter(|(_, worker)| {
                &worker.project == project
                    && !worker.stopping
                    && worker.own_turn.is_none()
                    && !paused.contains(&worker.connection)
            })
            .filter_map(|(id, worker)| worker.reask.map(|task| (*id, task)))
            .collect();
        for (agent, task) in due {
            assert_eq!(self.workers[&agent].busy, Some(Busy::Task(task)));
            if self.workers[&agent].kept_session && self.teammates_working(agent)? {
                self.finish_unasked(project, agent, task)?;
                continue;
            }
            let handed = self.workers[&agent].kept_session && self.workers[&agent].briefed;
            let grant = if self.workers[&agent].managed && !handed {
                match self.grant(access, agent, project) {
                    Ok(Some(grant)) => Some(grant),
                    Ok(None) => continue,
                    Err(_) => {
                        self.finish_unasked(project, agent, task)?;
                        continue;
                    }
                }
            } else {
                None
            };
            let background = super::background_at_end(&self.connection, project)?;
            let bound = turn_bound(
                super::time_limits(&self.connection, project)?,
                self.workers[&agent].ran(),
            );
            let rules = TurnRules {
                limit: bound.map(|(limit, _)| limit),
                background,
            };
            let worker = self.workers.get_mut(&agent).expect("worker exists");
            if worker
                .orders
                .send(Order::Turn(REASK_EMPTY.to_owned(), grant, rules))
                .is_err()
            {
                self.finish_unasked(project, agent, task)?;
                continue;
            }
            worker.bound = bound.map(|(_, by)| by);
            worker.reask = None;
            worker.reasked = Some(task);
        }
        Ok(())
    }

    /// Finishes a task whose re-ask turn could not be sent, as a task turn with no reply
    /// text finishes with the setting off: Done, its edits landed, the agent idle. Takes the
    /// project, the agent and the task. Returns any SQLite failure.
    fn finish_unasked(
        &mut self,
        project: &ProjectId,
        agent: i64,
        task: i64,
    ) -> Result<(), EngineError> {
        let worker = self.workers.get_mut(&agent).expect("worker exists");
        worker.reask = None;
        let busy = worker.busy.take();
        self.finish_task(task, "", TurnEnd::Success, None)?;
        self.land(agent, project, busy, TurnEnd::Success)?;
        self.mark_agent(agent, AgentStatus::Idle, None)
    }

    fn mark_agent(
        &mut self,
        id: i64,
        status: AgentStatus,
        task: Option<i64>,
    ) -> Result<(), EngineError> {
        self.connection
            .execute(
                "UPDATE team_agents SET status=?2, task=?3, heartbeat_ms=?4 WHERE id=?1",
                params![id, status.name(), task, now_ms()],
            )
            .map_err(error)?;
        Ok(())
    }

    fn deliver(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if self
            .master(project)
            .is_some_and(|id| self.workers[&id].busy.is_some())
        {
            return Ok(());
        }
        let pending: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND state IN ('pending','sent')",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if pending > 0 {
            return Ok(());
        }
        let live = crate::master_sessions::live_session(&self.connection, project)?;
        let ready: Vec<ReadyTask> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.status,t.result,t.error FROM team_tasks t
                     WHERE t.project=?1 AND t.delivered=0 AND t.source='master' AND t.parent IS NULL AND t.status IN ('done','failed','cancelled')
                     AND (t.session IS NULL OR t.session IS ?2)
                     AND NOT EXISTS (SELECT 1 FROM team_tasks o WHERE o.project=t.project AND o.turn IS t.turn AND t.turn IS NOT NULL
                                     AND o.status IN ('queued','needs_pool','running'))
                     ORDER BY t.id LIMIT 16",
                )
                .map_err(error)?;
            statement
                .query_map(params![project.as_str(), live], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        if ready.is_empty() {
            return Ok(());
        }
        let (text, held) = results_message(&ready);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        if append_message(
            &tx,
            project,
            MessageAuthor::Engine,
            &text,
            MessageState::Pending,
            now_ms(),
        )?
        .is_ok()
        {
            for (id, ..) in &ready[..held] {
                tx.execute("UPDATE team_tasks SET delivered=1 WHERE id=?1", [id])
                    .map_err(error)?;
            }
        }
        tx.commit().map_err(error)
    }

    /// The lines a sub-agent's task prompt ends with about its place in a stream (N3): a
    /// lead's worker hears where its work lands, and an agent with team tools above the
    /// owner's Sub-agent depth setting hears how to lead workers of its own. Takes the
    /// project and the agent. Returns the lines, empty for neither, or a SQLite failure.
    fn lead_lines(&self, project: &ProjectId, agent: i64) -> Result<String, EngineError> {
        let worker = &self.workers[&agent];
        let mut text = String::new();
        if worker.parent.is_some() {
            text.push_str(WORKER_LINE);
        }
        if worker.tools
            && self.delegation_tool.is_some()
            && super::agent_depth(&self.connection, agent)?
                < super::nesting(&self.connection, project)?.depth
        {
            text.push_str(LEAD_LINES);
        }
        Ok(text)
    }

    /// Tells each running lead how its workers' tasks ended (N1): the finished tasks it
    /// handed off, as one results message from the engine in the coordination layer, which
    /// delivers it by the lead's harness channel and keeps its receipt (DEC-104 K2b). A
    /// lead that is not running hears them once it runs again.
    /// Takes the project. Returns any SQLite failure.
    fn deliver_to_leads(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        let leads: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, worker)| {
                &worker.project == project && worker.role == AgentRole::Sub && !worker.stopping
            })
            .map(|(id, _)| *id)
            .collect();
        for lead in leads {
            let ready: Vec<ReadyTask> = {
                let mut statement = self
                    .connection
                    .prepare(
                        "SELECT id,harness,status,result,error FROM team_tasks
                         WHERE project=?1 AND parent=?2 AND delivered=0 AND status IN ('done','failed','cancelled')
                         ORDER BY id LIMIT 16",
                    )
                    .map_err(error)?;
                statement
                    .query_map(params![project.as_str(), lead], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .map_err(error)?
                    .collect::<Result<_, _>>()
                    .map_err(error)?
            };
            if ready.is_empty() {
                continue;
            }
            let (text, held) =
                results_within(&ready, crate::coord::BODY_BYTES_MAX, LEAD_RESULTS_TRAILER);
            let message = crate::coord::NewMessage {
                project: project.clone(),
                from: crate::coord::Party::Engine,
                to: crate::coord::Party::Agent(lead),
                kind: crate::coord::MessageKind::Note,
                question: None,
                work: None,
                reply_to: None,
                body: text,
                idempotency_key: Some(format!(
                    "{}{}",
                    crate::coord::delivery::RESULTS_KEY,
                    ready[0].0
                )),
            };
            match crate::coord::send(&self.connection, message, now_ms()) {
                Ok(_) => {}
                Err(problem) if problem.0.starts_with("coord_state_unavailable") => {
                    return Err(problem);
                }
                Err(_) => continue,
            }
            for (id, ..) in &ready[..held] {
                self.connection
                    .execute("UPDATE team_tasks SET delivered=1 WHERE id=?1", [id])
                    .map_err(error)?;
            }
        }
        Ok(())
    }

    /// The owner's instruction edit an agent's next task or message starts with under the
    /// project's default Next task choice (DEC-111 QR1): the sections changed since its
    /// launch or its last change block, once its process reported ready, so a launch still
    /// writing its instructions gets none. A read that fails sends the turn without one.
    /// Takes the project, the agent and its home. Returns the edit, or none.
    fn next_message_edit(
        &self,
        project: &ProjectId,
        agent: i64,
        home: &Path,
    ) -> Option<crate::instruction_files::Edit> {
        let worker = &self.workers[&agent];
        if !worker.ready {
            return None;
        }
        let harness = launch_harness(&worker.harness)?;
        crate::instruction_files::edit_for(&self.connection, project, home, harness)
            .ok()
            .flatten()
            .filter(|edit| edit.delivery == crate::instruction_files::EditDelivery::NextMessage)
    }

    /// The most bytes one turn's prompt may take on an agent: what its session accepts, a pane
    /// app's paste or a piped session's frame, less the line `told` may add at its end. Takes
    /// the agent. Returns the bytes.
    fn turn_prompt_max(&self, agent: i64) -> usize {
        let pane = self.terminal.is_some()
            && crate::harnesses::interactive::support(&self.workers[&agent].harness)
                == crate::harnesses::interactive::Support::Terminal;
        let session = if pane {
            crate::harnesses::terminal_session::PROMPT_BYTES_MAX
        } else {
            crate::harnesses::launch::PROMPT_BYTES_MAX
        };
        session - BACKGROUND_STOPS_LINE.len() - 1
    }

    /// Applies the project's Restart or Badge choice for owner instruction edits (DEC-111
    /// QR1), checking its agents whose process reported ready at most once each
    /// `INSTRUCTION_CHECK_EVERY`: under Restart an idle agent whose instructions changed
    /// stops and starts again, once per edited text, a sub-agent once its process ended, the
    /// live master while it is quiet through `keep_live`, and a master that is not live at
    /// the next message; under
    /// Badge each running agent whose instructions changed is flagged for the team status,
    /// and the flags clear under any other choice. Takes the setup, the access and the paused
    /// accounts. Returns any SQLite failure.
    fn instruction_edits(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        self.restart_after_edits(stored, access, paused)?;
        let now = Instant::now();
        if self
            .instructions_next
            .get(project)
            .is_some_and(|next| now < *next)
        {
            return Ok(());
        }
        self.instructions_next
            .insert(project.clone(), now + INSTRUCTION_CHECK_EVERY);
        let delivery = crate::instruction_files::load(&self.connection, project)
            .map(|settings| settings.edits)
            .unwrap_or_default();
        let mut flagged = BTreeSet::new();
        let ids: Vec<i64> = match delivery {
            crate::instruction_files::EditDelivery::NextMessage => Vec::new(),
            _ => self
                .workers
                .iter()
                .filter(|(_, worker)| {
                    &worker.project == project && !worker.stopping && worker.ready
                })
                .map(|(id, _)| *id)
                .collect(),
        };
        for id in ids {
            let Some(harness) = launch_harness(&self.workers[&id].harness) else {
                continue;
            };
            let home = agent_home(&self.state, id);
            let Ok(Some(edit)) =
                crate::instruction_files::edit_for(&self.connection, project, &home, harness)
            else {
                continue;
            };
            if edit.delivery == crate::instruction_files::EditDelivery::Badge {
                flagged.insert(id);
                continue;
            }
            let hash = edit.text_hash();
            if self.restarted_for.get(&id) == Some(&hash) {
                continue;
            }
            let waits = self.workers[&id].kept_session && self.lead_waits(id)?;
            let quiet = self.workers[&id].live
                && super::master::quiet(
                    &self.connection,
                    self.agents.as_deref().map(Agents::typing),
                    project,
                    now_ms(),
                );
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let idle = worker.busy.is_none() && worker.own_turn.is_none() && !waits;
            let (role, live) = (worker.role, worker.live);
            match (role, live) {
                (AgentRole::Sub, false) if idle => {
                    worker.restarting = true;
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.instruction_restarts.insert(id, project.clone());
                    self.restarted_for.insert(id, hash);
                }
                (AgentRole::Master, true) if idle && quiet && worker.turn_started.is_none() => {
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.master_restarts.insert(project.clone());
                    self.restarted_for.insert(id, hash);
                }
                (AgentRole::Master, false) if idle => {
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.restarted_for.insert(id, hash);
                }
                _ => {}
            }
        }
        assert!(flagged.len() <= self.workers.len());
        crate::instruction_files::set_badges(&self.connection, project, &flagged).map_err(error)
    }

    /// Starts again each sub-agent of the project that stopped for an instruction edit, once
    /// its process ended: in its own worktree and conversation through [`TeamRuntime::wake`],
    /// with an event saying why, or a team note when it cannot; while the pool's sign-ins are
    /// still being checked it waits for a later tick. Takes the setup, the access and the
    /// paused accounts. Returns any SQLite failure.
    fn restart_after_edits(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let ended: Vec<i64> = self
            .instruction_restarts
            .iter()
            .filter(|(id, owner)| *owner == project && !self.workers.contains_key(id))
            .map(|(id, _)| *id)
            .collect();
        for id in ended {
            self.instruction_restarts.remove(&id);
            let row: Option<(String, bool)> = self
                .connection
                .query_row(
                    "SELECT harness, tools FROM team_agents WHERE id=?1 AND project=?2 AND role='sub'",
                    params![id, project.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(error)?;
            let Some((harness, tools)) = row else {
                continue;
            };
            let started = match self.account_for(access, project, &harness, paused) {
                Ok(Some(account)) => self
                    .wake(stored, id, &harness, tools, &account, None)?
                    .map(|_| ()),
                Ok(None) => {
                    self.instruction_restarts.insert(id, project.clone());
                    continue;
                }
                Err(reason) => Err(reason),
            };
            match started {
                Ok(()) => {
                    super::events::record(
                        &self.connection,
                        project,
                        id,
                        None,
                        super::events::EventKind::Progress,
                        INSTRUCTIONS_RESTARTED,
                        now_ms(),
                    )?;
                }
                Err(reason) => self.note(
                    project,
                    &format!(
                        "An agent stopped to load your changed instructions but could not start again: {}.",
                        bound_text(&reason, 300)
                    ),
                )?,
            }
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let queued: Vec<QueuedTask> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.prompt,p.agent,t.source='master',t.parent,t.mode FROM team_tasks t
                     LEFT JOIN team_task_pins p ON p.task=t.id AND p.project=t.project
                     WHERE t.project=?1 AND t.status='queued' AND t.id NOT IN (
                         SELECT task FROM recovery_runs WHERE task IS NOT NULL
                         AND state IN ('stopped','interrupted','resuming','failed'))
                     ORDER BY t.id LIMIT 64",
                )
                .map_err(error)?;
            statement
                .query_map([project.as_str()], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        let harness_cap = super::caps::read(&self.connection, project)?.harnesses;
        let reserved: BTreeSet<i64> = crate::recovery::open_runs(&self.connection, project)?
            .into_iter()
            .map(|run| run.agent)
            .collect();
        let reuse = super::reuse_idle(&self.connection, project)? == super::ReuseIdle::On
            || super::keep_warm(&self.connection, project)?;
        for (task, harness, prompt, pin, by_master, parent, mode) in queued {
            let slot = if stored.team.sub_harnesses.is_empty() {
                super::slot(&stored.team, &harness)
            } else {
                stored
                    .team
                    .sub_harnesses
                    .iter()
                    .find(|s| s.harness == harness)
                    .cloned()
            };
            let Some(slot) = slot else {
                self.fail_task(task, "The team plan no longer has this harness")?;
                continue;
            };
            let mut ended = 0;
            let idle = loop {
                let found = self
                    .workers
                    .iter()
                    .find(|(id, w)| {
                        &w.project == project
                            && w.role == AgentRole::Sub
                            && w.harness == harness
                            && w.busy.is_none()
                            && w.own_turn.is_none()
                            && !w.stopping
                            && !reserved.contains(*id)
                            && !paused.contains(&w.connection)
                            && pin.map_or(reuse && w.parent == parent, |pin| pin == **id)
                            && mode.as_deref().is_none_or(|mode| {
                                self.stored_mode(**id).ok().flatten().as_deref() == Some(mode)
                            })
                    })
                    .map(|(id, _)| *id);
                match found {
                    Some(agent) if self.end_session(agent)? => ended += 1,
                    found => break found,
                }
                assert!(ended <= self.workers.len());
            };
            let mut woken_fresh = false;
            let agent = match (idle, pin) {
                (Some(agent), _) => agent,
                (None, Some(pinned)) if self.workers.contains_key(&pinned) => continue,
                (None, Some(pinned)) if super::owner_stopped(&self.connection, pinned)? => {
                    let ran: bool = self
                        .connection
                        .query_row(
                            "SELECT started_ms IS NOT NULL FROM team_tasks WHERE id=?1",
                            [task],
                            |row| row.get(0),
                        )
                        .map_err(error)?;
                    if by_master && !ran {
                        self.fail_task(task, OWNER_STOPPED_TASK)?;
                    }
                    continue;
                }
                (None, Some(pinned)) => {
                    if let Some(reason) = self.stop_failure(pinned, task)? {
                        self.fail_task(task, reason)?;
                        continue;
                    }
                    if self.live(project, AgentRole::Sub, Some(&harness))
                        >= usize::from(slot.agents)
                        || self.live(project, AgentRole::Sub, None) >= subs_max
                        || self.capped(project, parent, harness_cap)
                    {
                        continue;
                    }
                    let Ok(Some(account)) = self.account_for(access, project, &harness, paused)
                    else {
                        continue;
                    };
                    let layout = self.agent_layout(project, pinned).map_err(error)?;
                    let refused = crate::sandbox::policy::load(&self.connection, project.as_str())
                        .is_ok_and(|policy| confinement(layout, policy.access).is_err());
                    if refused
                        && self
                            .connection
                            .execute(
                                "DELETE FROM team_task_pins WHERE task=?1 AND held=1",
                                [task],
                            )
                            .map_err(error)?
                            == 1
                    {
                        continue;
                    }
                    match self.wake(stored, pinned, &harness, slot.tools, &account, None)? {
                        Ok((agent, resumed)) => {
                            woken_fresh = !resumed;
                            agent
                        }
                        Err(problem) => {
                            self.fail_task(
                                task,
                                &format!(
                                    "The sub-agent could not start again in its worktree: {}",
                                    bound_text(&problem, 300)
                                ),
                            )?;
                            continue;
                        }
                    }
                }
                (None, None) => {
                    let slot_full = self.live(project, AgentRole::Sub, Some(&harness))
                        >= usize::from(slot.agents);
                    let cap_full = self.capped(project, parent, harness_cap);
                    if slot_full
                        || cap_full
                        || self.live(project, AgentRole::Sub, None) >= subs_max
                        || self.panes(project) >= PANES_PER_PROJECT_MAX
                    {
                        if !reuse {
                            let counted = |w: &Worker| {
                                (!slot_full || w.harness == harness)
                                    && (!cap_full || w.parent == parent)
                            };
                            self.stop_idle_for(project, &harness, counted, &reserved)?;
                        }
                        continue;
                    }
                    let Ok(Some(account)) = self.account_for(access, project, &harness, paused)
                    else {
                        continue;
                    };
                    self.naming = Some(task);
                    let spawned = self.spawn(
                        stored,
                        AgentRole::Sub,
                        &harness,
                        slot.tools,
                        &account,
                        parent,
                    );
                    self.naming = None;
                    match spawned? {
                        Start::Started(agent) => {
                            assert!(self.live(project, AgentRole::Sub, None) <= subs_max);
                            agent
                        }
                        Start::Held => continue,
                        Start::Refused(problem) => {
                            self.fail_task(
                                task,
                                &format!(
                                    "The sub-agent could not start: {}",
                                    bound_text(&problem, 300)
                                ),
                            )?;
                            continue;
                        }
                    }
                }
            };
            let handed = self.workers[&agent].kept_session && self.workers[&agent].briefed;
            let grant = if self.workers[&agent].managed && !handed {
                match self.grant(access, agent, project) {
                    Ok(Some(grant)) => Some(grant),
                    Ok(None) => continue,
                    Err(reason) => {
                        let worker = self.workers.get_mut(&agent).expect("worker exists");
                        worker.stopping = true;
                        let _ = worker.orders.send(Order::Stop);
                        self.fail_task(
                            task,
                            &format!(
                                "The sub-agent could not start: {}",
                                bound_text(&reason, 300)
                            ),
                        )?;
                        continue;
                    }
                }
            } else {
                None
            };
            let worktree: Option<String> = self
                .connection
                .query_row(
                    "SELECT worktree FROM team_agents WHERE id=?1",
                    [agent],
                    |row| row.get(0),
                )
                .map_err(error)?;
            let folder = stored.project_folder();
            let (waiting, carried) = match self.workers[&agent].fresh {
                Some(started) if prompt == super::master::WAKE_TASK => {
                    crate::coord::delivery::next_turn_unread(&self.connection, agent, started)?
                }
                _ => crate::coord::delivery::next_turn(&self.connection, agent)?,
            };
            let task_text = if waiting.is_empty() && prompt == super::master::WAKE_TASK {
                super::master::WAKE_DELIVERED
            } else {
                prompt.as_str()
            };
            if !delivery::intend(&self.connection, Busy::Task(task), agent, task_text, None)? {
                continue;
            }
            let handoff = if woken_fresh {
                last_result(&self.connection, agent)?
            } else {
                self.workers
                    .get_mut(&agent)
                    .and_then(|worker| worker.handoff.take())
            };
            let earlier = handoff
                .map(|last| {
                    format!(
                        "You worked in this worktree before and stopped. Your last result then: {last}\n"
                    )
                })
                .unwrap_or_default();
            let layout = self
                .agent_layout(&stored.project_id, agent)
                .map_err(error)?;
            let mut prompt = sub_agent_prompt(
                &stored.name,
                &worktree.map_or(folder.clone(), |worktree| folder.join(worktree)),
                &earlier,
                task_text,
                layout,
            );
            prompt.push_str(&self.lead_lines(project, agent)?);
            prompt.push_str(&self.branch_line(project, agent));
            if let Some(text) = &self.workers[&agent].starts.text
                && crate::coord::settings::read(&self.connection, project)?.task_start
                    == crate::coord::settings::TaskStart::On
            {
                prompt.push_str("\n\n");
                prompt.push_str(text);
            }
            let background = super::background_at_end(&self.connection, &stored.project_id)?;
            prompt.push_str(&waiting);
            let home = agent_home(&self.state, agent);
            let edit = self.next_message_edit(project, agent, &home);
            let edit = edit_in_front(&mut prompt, edit, self.turn_prompt_max(agent));
            let bound = turn_bound(
                super::time_limits(&self.connection, &stored.project_id)?,
                self.workers[&agent].ran(),
            );
            let worker = self.workers.get_mut(&agent).expect("worker exists");
            if worker
                .orders
                .send(Order::Turn(
                    prompt,
                    grant,
                    TurnRules {
                        limit: bound.map(|(limit, _)| limit),
                        background,
                    },
                ))
                .is_err()
            {
                delivery::ended(&self.connection, Some(Busy::Task(task)), false)?;
                continue;
            }
            worker.bound = bound.map(|(_, by)| by);
            worker.busy = Some(Busy::Task(task));
            worker.reasked = None;
            worker.briefed |= worker.kept_session;
            worker.fresh = None;
            if let Some(edit) = &edit {
                let _ = crate::instruction_files::delivered(edit, &home);
            }
            crate::coord::delivery::delivered(&self.connection, &carried, now_ms())?;
            self.connection
                .execute(
                    "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL WHERE id=?1",
                    params![task, agent, now_ms()],
                )
                .map_err(error)?;
            self.connection
                .execute(
                    "UPDATE team_agents SET session=(SELECT session FROM team_tasks WHERE id=?1) WHERE id=?2 AND session IS NULL",
                    params![task, agent],
                )
                .map_err(error)?;
            names::assign_task_role(&self.connection, project, agent, task)?;
            crate::activity::note_task(&self.connection, project, agent, task, now_ms());
            self.mark_agent(agent, AgentStatus::Working, Some(task))?;
        }
        Ok(())
    }

    fn keep_warm(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if !super::keep_warm(&self.connection, project)? {
            return Ok(());
        }
        let floor = usize::from(stored.team.min_agents.saturating_sub(1));
        let harness_cap = super::caps::read(&self.connection, project)?.harnesses;
        for slot in &stored.team.sub_harnesses {
            while self.live(project, AgentRole::Sub, None) < floor
                && self.live(project, AgentRole::Sub, Some(&slot.harness))
                    < usize::from(slot.agents)
                && self.panes(project) < PANES_PER_PROJECT_MAX
                && !self.harnesses_full(project, harness_cap)
            {
                let Ok(Some(account)) = self.account_for(access, project, &slot.harness, paused)
                else {
                    break;
                };
                match self.spawn(
                    stored,
                    AgentRole::Sub,
                    &slot.harness,
                    slot.tools,
                    &account,
                    None,
                )? {
                    Start::Started(agent) => {
                        self.workers.get_mut(&agent).expect("worker exists").warm = true;
                    }
                    Start::Held => return Ok(()),
                    Start::Refused(problem) => {
                        self.connection
                            .execute(
                                "UPDATE team_settings SET keep_warm=0 WHERE project=?1",
                                [project.as_str()],
                            )
                            .map_err(error)?;
                        return self.note(
                        project,
                        &full_stop(format!(
                            "Keeping sub-agents warm was turned off because a sub-agent could not start: {}",
                            bound_text(&problem, 300)
                        )),
                    );
                    }
                }
            }
        }
        Ok(())
    }

    /// Stops an agent past its session limit, so its next turn goes to a fresh agent. The
    /// session counts from the agent's start and the limit is read now; a live master is never
    /// stopped by it. Takes the agent. Returns whether it was stopped, or a failed read.
    fn end_session(&mut self, id: i64) -> Result<bool, EngineError> {
        let worker = &self.workers[&id];
        if worker.live {
            return Ok(false);
        }
        let session = super::time_limits(&self.connection, &worker.project)?.session_time();
        if session.is_none_or(|limit| worker.ran() < limit) {
            return Ok(false);
        }
        let worker = self.workers.get_mut(&id).expect("worker exists");
        worker.stopping = true;
        let _ = worker.orders.send(Order::Stop);
        Ok(true)
    }

    /// Stops agents idle past their bound, except the sub-agents the owner keeps warm and,
    /// with the owner's idle stop Off, every idle sub-agent. An agent past its session limit
    /// stops even when kept warm. A master is never idle while its project has work in
    /// flight, so its idle time counts from the moment the last of that work ended and its
    /// session is ended at its next turn. A live master stops as idle only under a choice the
    /// owner saved, once quiet, and its switch goes off so it stays stopped. Takes the project.
    /// Returns any SQLite failure.
    fn reap_idle(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let warm = super::keep_warm(&self.connection, project)?;
        let floor = usize::from(stored.team.min_agents.saturating_sub(1));
        let mut live_subs = self.live(project, AgentRole::Sub, None);
        let chosen = super::master_idle_stop(&self.connection, project)?;
        let stops_live = chosen.is_some_and(|choice| choice != super::MasterIdleStop::Off);
        let ids: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| {
                &w.project == project && w.busy.is_none() && w.own_turn.is_none() && !w.stopping
            })
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let masters = ids.iter().any(|id| {
            let worker = &self.workers[id];
            worker.role == AgentRole::Master && (!worker.live || stops_live)
        });
        let in_flight = masters && self.work_in_flight(project)?;
        let limits = super::time_limits(&self.connection, project)?;
        for id in ids {
            if self.workers[&id].live && !self.live_master_idles(id, project, stops_live) {
                continue;
            }
            let waits = self.workers[&id].kept_session && self.lead_waits(id)?;
            let worker = self.workers.get_mut(&id).expect("worker exists");
            if worker.role == AgentRole::Master && in_flight {
                worker.idle_since = Instant::now();
                continue;
            }
            let ended = !worker.live
                && limits
                    .session_time()
                    .is_some_and(|session| worker.ran() >= session);
            if !ended && waits {
                worker.idle_since = Instant::now();
                continue;
            }
            let choice = super::MasterIdleStop::effective(chosen, worker.live);
            if !ended {
                let limit = match worker.role {
                    AgentRole::Master => choice.after(self.limits.master_idle_stop),
                    AgentRole::Sub => limits.idle_time(),
                    AgentRole::Teammate => None,
                };
                let Some(limit) = limit else {
                    worker.idle_since = Instant::now();
                    continue;
                };
                if worker.idle_since.elapsed() < limit
                    || (worker.role == AgentRole::Sub && warm && live_subs <= floor)
                {
                    continue;
                }
            }
            if worker.role == AgentRole::Sub {
                live_subs -= 1;
            }
            let live = worker.live;
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
            if live {
                self.live_ended(project, &super::master::idle_stopped_line(choice.label()))?;
            }
        }
        Ok(())
    }

    /// Tells whether a live master counts as idle for the owner's idle stop: a saved choice
    /// can stop one, no turn is open and it is quiet. Otherwise its idle time starts over, so
    /// a choice saved later counts from then. Takes the agent, the project and whether the
    /// choice can stop a live master, always so for Live terminal Off. Returns the answer.
    fn live_master_idles(&mut self, id: i64, project: &ProjectId, stops_live: bool) -> bool {
        let quiet = stops_live
            && self.workers[&id].turn_started.is_none()
            && super::master::quiet(
                &self.connection,
                self.agents.as_deref().map(Agents::typing),
                project,
                now_ms(),
            );
        if !quiet {
            self.workers.get_mut(&id).expect("worker exists").idle_since = Instant::now();
        }
        quiet
    }

    /// Makes room at a full limit for a task that may not reuse an idle agent: stops one idle
    /// sub-agent the limits count, the task's harness first, none while another is stopping.
    /// A live agent, one with agents of its own, one reserved for recovery, one a queued task
    /// waits for and a kept lead with work stay. Takes the project, the harness, which
    /// agents the full limits count and the reserved agents. Returns any SQLite failure.
    fn stop_idle_for(
        &mut self,
        project: &ProjectId,
        harness: &str,
        counted: impl Fn(&Worker) -> bool,
        reserved: &BTreeSet<i64>,
    ) -> Result<(), EngineError> {
        let sub = |w: &Worker| &w.project == project && w.role == AgentRole::Sub;
        if self.workers.values().any(|w| sub(w) && w.stopping) {
            return Ok(());
        }
        let mut idle: Vec<i64> = self
            .workers
            .iter()
            .filter(|(id, w)| {
                sub(w)
                    && counted(w)
                    && w.busy.is_none()
                    && w.own_turn.is_none()
                    && !w.live
                    && !reserved.contains(*id)
                    && !self.workers.values().any(|c| c.parent == Some(**id))
            })
            .map(|(id, _)| *id)
            .collect();
        idle.sort_by_key(|id| self.workers[id].harness != harness);
        for id in idle {
            if (self.workers[&id].kept_session && self.lead_waits(id)?)
                || self.queued_work(project, id)?
            {
                continue;
            }
            let worker = self.workers.get_mut(&id).expect("worker exists");
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
            return Ok(());
        }
        Ok(())
    }

    /// Whether a task of the project not yet started, queued or waiting for a pool, waits for
    /// this agent: one pinned to it, or one naming it as its lead, whose result only a running
    /// lead receives. Takes the project and the agent. Returns the answer, or any SQLite
    /// failure.
    fn queued_work(&self, project: &ProjectId, id: i64) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_tasks t
                               LEFT JOIN team_task_pins p ON p.task=t.id
                               WHERE t.project=?2 AND t.status IN ('queued','needs_pool')
                               AND (t.parent=?1 OR p.agent=?1))",
                params![id, project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)
    }

    /// Whether a kept native lead still has work around it that idle time must not end: a
    /// teammate of it still working, or a coordination message to it that no channel or
    /// wake turn has carried to it yet. Takes the lead. Returns the answer, or any SQLite
    /// failure.
    fn lead_waits(&self, id: i64) -> Result<bool, EngineError> {
        if self.teammates_working(id)? {
            return Ok(true);
        }
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM coord_messages WHERE to_kind='agent' AND to_agent=?1
                           AND receipt='sent')",
                [id],
                |row| row.get(0),
            )
            .map_err(error)
    }

    /// Whether a teammate of a kept native lead is still starting or working. Takes the
    /// lead. Returns the answer, or any SQLite failure.
    fn teammates_working(&self, id: i64) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_agents WHERE parent=?1 AND role='teammate'
                                AND status IN ('starting','working'))",
                [id],
                |row| row.get(0),
            )
            .map_err(error)
    }

    /// Records a turn a kept native lead's app started by itself, such as after messages
    /// from its teammates: a finished task of the lead's own with the turn's reply as its
    /// result, which reaches the master as any finished master task does, then lands what
    /// the turn changed in the lead's clone as a change set for the master. Takes the lead,
    /// its project, when the turn started, its reply text and how it ended. Returns any
    /// SQLite failure.
    fn own_turn_ended(
        &mut self,
        id: i64,
        project: &ProjectId,
        started: i64,
        text: &str,
        end: TurnEnd,
    ) -> Result<(), EngineError> {
        let (status, problem) = match end {
            TurnEnd::Success => (TaskStatus::Done, None),
            TurnEnd::Cancelled | TurnEnd::Interrupted => {
                (TaskStatus::Cancelled, Some("Interrupted by you"))
            }
            TurnEnd::RateLimited => (TaskStatus::Failed, Some("The account hit its rate limit")),
            _ => (TaskStatus::Failed, Some(OWN_TURN_FAILED)),
        };
        let result =
            (!text.trim().is_empty()).then(|| bound_text(text.trim(), super::RESULT_BYTES_MAX));
        let now = now_ms();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        super::make_room(
            &tx,
            "team_tasks",
            super::TASK_HISTORY,
            project,
            super::TASKS_PER_PROJECT_MAX,
        )?;
        let added = tx
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,parent,attempts,
                                        result,error,output_bytes,created_ms,started_ms,finished_ms)
                 SELECT project,?3,harness,tools,?4,?5,id,parent,1,?6,?7,?8,?9,?9,?10
                 FROM team_agents WHERE id=?1 AND project=?2",
                params![
                    id,
                    project.as_str(),
                    TaskSource::Master.name(),
                    OWN_TURN_TASK,
                    status.name(),
                    result,
                    problem,
                    i64::try_from(text.len()).unwrap_or(i64::MAX),
                    started.min(now),
                    now
                ],
            )
            .map_err(error)?;
        assert!(added <= 1);
        let task = (added == 1).then(|| tx.last_insert_rowid());
        tx.commit().map_err(error)?;
        let Some(task) = task else {
            return Ok(());
        };
        assert!(task > 0);
        super::events::task_ended(&self.connection, task, now)?;
        self.land_turn(
            id,
            project,
            format!("task-{task}"),
            OWN_TURN_TASK.to_owned(),
            &format!("task {task}"),
            end,
        )
    }

    /// Tells whether a project's master still has work in flight: a task queued, waiting
    /// for a pool account or running, or a message waiting for the master. Takes the
    /// project. Returns the answer, or any SQLite failure.
    fn work_in_flight(&self, project: &ProjectId) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_tasks WHERE project=?1 AND status IN ('queued','needs_pool','running'))
                 OR EXISTS(SELECT 1 FROM team_messages WHERE project=?1 AND state IN ('pending','sent') AND author<>'master')",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)
    }

    /// What fails a pinned task that a stop by the master or a lead left unfinished: one
    /// created before the stop (HB-S5-B). A task pinned after it is a wake, which starts the
    /// agent. Takes the agent and the task. Returns the reason, None when no such stop holds
    /// the task, or any SQLite failure.
    fn stop_failure(&self, agent: i64, task: i64) -> Result<Option<&'static str>, EngineError> {
        let Some((by, stopped_ms)) = super::stop_of(&self.connection, agent)? else {
            return Ok(None);
        };
        let created_ms: i64 = self
            .connection
            .query_row(
                "SELECT created_ms FROM team_tasks WHERE id=?1",
                [task],
                |row| row.get(0),
            )
            .map_err(error)?;
        if created_ms > stopped_ms {
            return Ok(None);
        }
        Ok(Some(match by {
            StoppedBy::Lead => LEAD_STOPPED_TASK,
            StoppedBy::Master => MASTER_STOPPED_TASK,
            StoppedBy::Owner => OWNER_STOPPED_TASK,
        }))
    }

    fn fail_task(&mut self, task: i64, reason: &str) -> Result<(), EngineError> {
        self.connection
            .execute(
                "UPDATE team_tasks SET status='failed', agent=NULL, error=?2, finished_ms=?3 WHERE id=?1",
                params![task, reason, now_ms()],
            )
            .map_err(error)?;
        Ok(())
    }

    /// Starts a new agent of the project, a worker of the lead `parent` names when one does
    /// (N1), so its worktree is cut from that lead's current commit. Takes the setup, role,
    /// harness, tools switch, account and the lead. Returns the started agent, Held while a
    /// projects home move holds its folder, or the plain reason it could not start.
    fn spawn(
        &mut self,
        stored: &StoredSetup,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        parent: Option<i64>,
    ) -> Result<Start<i64>, EngineError> {
        self.spawn_run(stored, role, harness, tools, account, None, parent)
    }

    /// Records and launches one new team agent, a worker of the lead `parent` names when one
    /// does, unless the project already runs the most panes or a projects home move holds its
    /// folder: the hold is checked in the same write transaction that records the agent, so a
    /// move planned first holds the start back. Takes the setup, the role, harness, tools
    /// switch, account, any register restart and the lead. Returns the started agent, Held,
    /// or the plain reason it could not start.
    #[allow(clippy::too_many_arguments)]
    fn spawn_run(
        &mut self,
        stored: &StoredSetup,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
        parent: Option<i64>,
    ) -> Result<Start<i64>, EngineError> {
        assert!(parent.is_none() || role == AgentRole::Sub);
        if self.panes(&stored.project_id) >= PANES_PER_PROJECT_MAX {
            return Ok(Start::Refused(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents, the most one project may run at once"
            )));
        }
        let now = now_ms();
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        if crate::projects_home::move_holds(&tx, &stored.project_folder()).map_err(error)? {
            return Ok(Start::Held);
        }
        tx.execute(
            "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms,parent,mode)
             VALUES (?1,?2,?3,?4,?5,'starting',?6,?6,?7,(SELECT mode FROM team_tasks WHERE id=?8 AND project=?1))",
            params![stored.project_id.as_str(), role.name(), harness, tools, account.connection.as_str(), now, parent, self.naming],
        )
        .map_err(error)?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(error)?;
        assert!(id > 0);
        if role == AgentRole::Sub {
            names::assign_persona(&self.connection, &stored.project_id, id)?;
        }
        Ok(
            match self.launch(stored, id, role, harness, tools, account, again, None)? {
                Ok(agent) => Start::Started(agent),
                Err(reason) => Start::Refused(reason),
            },
        )
    }

    /// Wakes a stopped sub-agent (DEC-102), the one way a task pinned to it, the master's
    /// wake and the owner's Wake or restart start one again: the agent starts under its own
    /// id, name and recorded worktree, and under its own newest register entry when no
    /// restart names one, continuing the harness conversation kept for it when
    /// [`TeamRuntime::kept_conversation`] finds one that can continue. Takes the setup, the
    /// agent, its harness and tools switch, the account and the register restart if any.
    /// Returns the agent with whether its harness conversation continues, or the plain
    /// reason it could not start.
    fn wake(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
    ) -> Result<Result<(i64, bool), String>, EngineError> {
        let own = match (again, &self.agents) {
            (None, Some(_)) => {
                crate::agents::hosted_entries(&self.connection, &stored.project_id, TEAM_HOST)
                    .map_err(error)?
                    .remove(&id.to_string())
                    .map(|entry| HostedRestart {
                        id: entry,
                        host: TEAM_HOST.to_owned(),
                        reference: id.to_string(),
                    })
            }
            _ => None,
        };
        let again = again.or(own.as_ref());
        let resume = self.kept_conversation(id, harness, &account.connection)?;
        let resuming = resume.is_some();
        self.resuming = resume.map(|resume| (id, resume));
        let revived = self.revive(stored, id, AgentRole::Sub, harness, tools, account, again);
        self.resuming = None;
        Ok(revived?.map(|agent| {
            assert_eq!(agent, id);
            (id, resuming)
        }))
    }

    /// The harness conversation a stopped sub-agent can continue: the one kept for it on
    /// the same harness and account, with an id the harness resumes by and its record still
    /// a plain file, while agents run in terminal panes. Takes the agent, its harness and
    /// the account. Returns the conversation, or none when it starts a fresh one.
    fn kept_conversation(
        &self,
        id: i64,
        harness: &str,
        account: &CredentialRef,
    ) -> Result<Option<Resume>, EngineError> {
        if self.terminal.is_none() {
            return Ok(None);
        }
        let launch = launch_harness(harness).unwrap_or(harness);
        let kept: Option<(String, String, String, String)> = self
            .connection
            .query_row(
                "SELECT harness,connection,conversation,record FROM team_agent_conversations WHERE agent=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(error)?;
        Ok(kept
            .filter(|(kept_harness, connection, conversation, record)| {
                kept_harness == launch
                    && connection == account.as_str()
                    && crate::harnesses::interactive::resumable(conversation)
                    && std::fs::symlink_metadata(record).is_ok_and(|meta| meta.is_file())
            })
            .map(|(_, _, conversation, record)| Resume {
                id: conversation,
                record: PathBuf::from(record),
            }))
    }

    /// Starts a stopped agent again under its own id in the worktree and home it left: a
    /// sub-agent through [`TeamRuntime::wake`], or a project's live master. Its row takes the
    /// tools switch it starts with now. The agent shows no process group until its new
    /// process is ready, never the one that ended. It clears only the agent's own owner
    /// stop; the owner's Wake or restart clears its lead tree in
    /// [`TeamRuntime::start_again`]. Takes the setup, the agent, its role, harness and
    /// tools switch, the account to run on and the register restart if any. Returns its id,
    /// or the plain reason it could not start; the worktree must still be the one recorded
    /// when it first started.
    #[allow(clippy::too_many_arguments)]
    fn revive(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
    ) -> Result<Result<i64, String>, EngineError> {
        if self.panes(&stored.project_id) >= PANES_PER_PROJECT_MAX {
            return Ok(Err(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents, the most one project may run at once"
            )));
        }
        let kept: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![id, stored.project_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((repository, worktree, git_dir)) = kept else {
            return Ok(Err("the agent's worktree is no longer known".into()));
        };
        let changed = self
            .connection
            .execute(
                "UPDATE team_agents SET status='starting', connection=?3, tools=?6, task=NULL, error=NULL, stopped_ms=NULL, turns=0, heartbeat_ms=?4, process_group=NULL
                 WHERE id=?1 AND project=?2 AND role=?5 AND status IN ('stopped','failed')",
                params![id, stored.project_id.as_str(), account.connection.as_str(), now_ms(), role.name(), tools],
            )
            .map_err(error)?;
        if changed != 1 {
            return Ok(Err("the agent is not stopped".into()));
        }
        super::owner_start_alone(&self.connection, &stored.project_id, id)?;
        if role == AgentRole::Sub {
            names::reclaim(&self.connection, &stored.project_id, id)?;
        }
        let kept = [repository, worktree, git_dir];
        self.launch(stored, id, role, harness, tools, account, again, Some(kept))
    }

    /// Prepares and starts the agent of one `team_agents` row as `start_agent` does, then
    /// settles in the action ledger what the start left pending: its worktree intent, and
    /// its harness start when no worker runs it. Takes and returns as `start_agent`.
    #[allow(clippy::too_many_arguments)]
    fn launch(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
        kept: Option<[String; 3]>,
    ) -> Result<Result<i64, String>, EngineError> {
        let launched = self.start_agent(stored, id, role, harness, tools, account, again, kept);
        let running = self.workers.contains_key(&id);
        spawn_ledger::settle(&self.connection, &self.root, id, running);
        launched
    }

    /// Prepares and starts the agent of one `team_agents` row: a new agent in a new
    /// worktree, or with `kept` a revived one in the worktree recorded for it. Takes the
    /// setup, the row id, role, harness, tools switch, account, the register restart if any
    /// and the kept worktree. Returns the id, or the plain reason it failed, which the row
    /// then records.
    #[allow(clippy::too_many_arguments)]
    fn start_agent(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
        kept: Option<[String; 3]>,
    ) -> Result<Result<i64, String>, EngineError> {
        let layout = match &kept {
            Some([_, worktree, git_dir]) => recorded_layout(worktree, git_dir),
            None => self.git_layout(&stored.project_id),
        };
        let prepared = self.prepare(
            stored,
            id,
            role,
            harness,
            tools,
            account,
            kept.is_some(),
            layout,
        );
        #[cfg(unix)]
        let prepared = prepared.and_then(|(root, plan, mode)| {
            if plan.native {
                self.callers.remove(&id);
                self.caller_folders.remove(&id);
                self.caller_pins.remove(&id);
                let bound = self.bind_callers(id, plan.scratch_pin).map_err(|e| {
                    eprintln!("team_caller_socket_not_bound kind={:?}", e.kind());
                    with_code(
                        "the lead's caller socket could not be made",
                        Some(io_code(&e)),
                    )
                })?;
                self.caller_folders.insert(id, bound.folder());
                self.callers.insert(id, Some(bound));
            }
            Ok((root, plan, mode))
        });
        let resume = self
            .resuming
            .take()
            .filter(|(agent, _)| *agent == id)
            .map(|(_, resume)| resume);
        let (root, plan, mode) = match prepared {
            Ok((root, plan, mode)) => (
                root,
                Plan {
                    resume: resume
                        .filter(|_| plan.live || plan.pane.is_some() || role == AgentRole::Sub),
                    ..plan
                },
                mode,
            ),
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        let made =
            NewClone((kept.is_none() && layout == Layout::Clone).then(|| plan.working_dir.clone()));
        let worktree = (plan.working_dir != root)
            .then(|| {
                plan.working_dir
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .flatten();
        let landing = match agent_git_dir(&root, &plan.working_dir, layout) {
            Ok(landing) if kept.as_ref().is_none_or(|kept| kept == &landing) => landing,
            Ok(_) => {
                let problem =
                    with_code("the agent worktree changed", Some("team_worktree_changed"));
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        self.connection
            .execute(
                "UPDATE team_agents SET worktree=?2 WHERE id=?1",
                params![id, worktree],
            )
            .map_err(error)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![id, stored.project_id.as_str(), landing[0], landing[1], landing[2]],
            )
            .map_err(error)?;
        made.keep();
        if kept.is_none() {
            let named = layout == Layout::Linked
                && super::agent_branches(&self.connection, &stored.project_id)?
                    == super::BranchMode::Named;
            let paths = landing.each_ref().map(Path::new);
            let started = crate::landing::open(&self.root).and_then(|db| {
                if named {
                    crate::landing::name_agent_branch(&db, paths[0], paths[1], paths[2])?;
                }
                crate::landing::record_worktree_start(&db, paths[0], paths[1], paths[2])
            });
            if let Err(problem) = started {
                let problem = with_code(
                    "the agent worktree's starting commit could not be recorded",
                    Some(&problem.0),
                );
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        }
        if let Err(problem) = crate::landing::open(&self.root)
            .and_then(|db| crate::landing::refresh_main(&db, &stored.project_id))
        {
            eprintln!(
                "team_main_not_refreshed code={}",
                crate::protocol::plain_code(&problem.0)
            );
        }
        let hosted = match spawn_ledger::begin_spawn(&self.root, id, plan.pane.is_some())
            .and_then(|()| self.host(stored, id, role, &plan.harness, &account.connection, again))
        {
            Ok(hosted) => hosted,
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        let managed = matches!(mode, Mode::PerTurn { .. });
        self.run_worker(
            id,
            Launch::Fresh(Box::new(plan), Box::new(mode)),
            Seat {
                project: stored.project_id.clone(),
                role,
                harness: harness.to_owned(),
                connection: account.connection.clone(),
                tools,
                hosted,
            },
        )?;
        if managed {
            self.mark_agent(id, AgentStatus::Idle, None)?;
        }
        Ok(Ok(id))
    }

    /// Starts the thread that runs one agent and records it as a live worker of its project.
    /// Takes the team agent id, how its harness starts and who it is in the team.
    /// Returns once the thread runs; the project then holds one more terminal at most.
    fn run_worker(&mut self, id: i64, launch: Launch, seat: Seat) -> Result<(), EngineError> {
        let interrupt = seat
            .hosted
            .as_ref()
            .map_or_else(|| Arc::new(AtomicBool::new(false)), Hosted::interrupt_flag);
        let (orders, inbox) = channel();
        let (outbox, reports) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let leave = Arc::new(AtomicBool::new(false));
        let (flag, kept) = (cancel.clone(), leave.clone());
        let managed = match &launch {
            Launch::Fresh(_, mode) => matches!(**mode, Mode::PerTurn { .. }),
            Launch::Adopt(adoption) => adoption.managed.is_some(),
        };
        let (live, briefed) = match &launch {
            Launch::Fresh(plan, _) => (plan.live, false),
            Launch::Adopt(adoption) => (adoption.live, adoption.managed.is_none()),
        };
        let kept_session = match &launch {
            Launch::Fresh(plan, mode) => plan.native && matches!(**mode, Mode::PerTurn { .. }),
            Launch::Adopt(adoption) => adoption.managed.is_some() && !adoption.live,
        };
        assert!(!(kept_session && live));
        let fresh = matches!(&launch, Launch::Fresh(plan, _) if plan.resume.is_none()).then(now_ms);
        let (access, toolchains, network) = match &launch {
            Launch::Fresh(plan, _) => (plan.access, Some(plan.toolchains.clone()), plan.network),
            Launch::Adopt(adoption) => (
                adoption.access,
                adoption.toolchains.clone(),
                adoption.network,
            ),
        };
        let starts = crate::coord::relay::TaskStarts {
            text: launch.task_start(),
            reminded: BTreeSet::new(),
        };
        let thread = std::thread::Builder::new()
            .name(format!("team-agent-{id}"))
            .spawn(move || work(launch, inbox, outbox, flag, interrupt, kept))
            .map_err(error)?;
        let team_tool =
            seat.role == AgentRole::Master && seat.tools && self.delegation_tool.is_some();
        let ask_tool =
            seat.role != AgentRole::Master && seat.tools && self.delegation_tool.is_some();
        let parent: Option<i64> = self
            .connection
            .query_row("SELECT parent FROM team_agents WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?
            .flatten();
        self.workers.insert(
            id,
            Worker {
                project: seat.project,
                role: seat.role,
                harness: seat.harness,
                connection: seat.connection,
                orders,
                reports,
                cancel,
                leave,
                busy: None,
                idle_since: Instant::now(),
                stopping: false,
                held: false,
                restarting: false,
                warm: false,
                worked: false,
                ended_well: false,
                left_running: false,
                managed,
                tools: seat.tools,
                team_tool,
                ask_tool,
                grants: 0,
                thread: Some(thread),
                hosted: seat.hosted,
                live,
                briefed,
                input_ready: false,
                background_held: false,
                kept_session,
                fresh,
                own_turn: None,
                own_rules: None,
                turn_started: None,
                session: None,
                seed: None,
                bound: None,
                spawned: Instant::now(),
                handoff: None,
                access,
                network,
                ran_before: Duration::ZERO,
                told_finished: BTreeSet::new(),
                parent,
                ready: false,
                toolchains,
                checked: None,
                narrowed: None,
                reask: None,
                reasked: None,
                starts,
            },
        );
        assert!(self.panes(&self.workers[&id].project) <= PANES_PER_PROJECT_MAX);
        self.swept.get_or_insert_with(Instant::now);
        Ok(())
    }

    /// Where a new agent of a project keeps its git state (DEC-102): its own clone at the
    /// Own branch and Full access levels, a linked worktree at Sandboxed or when the saved
    /// level cannot be read (launch then refuses it). Input: the project. Output: the layout.
    /// Reads the git layout a started agent works in from the worktree record the engine
    /// wrote at its launch, never from the disk the agent can change. Takes the project and
    /// the agent id. Returns Clone when the recorded git folder is the agent's own clone's,
    /// Linked otherwise or with no record, or the database error.
    fn agent_layout(&self, project: &ProjectId, id: i64) -> rusqlite::Result<Layout> {
        let recorded: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![id, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(recorded.map_or(Layout::Linked, |(worktree, git_dir)| {
            recorded_layout(&worktree, &git_dir)
        }))
    }

    fn git_layout(&self, project: &ProjectId) -> Layout {
        match crate::sandbox::policy::load(&self.connection, project.as_str()) {
            Ok(policy) if policy.access != AgentAccess::Sandboxed => Layout::Clone,
            _ => Layout::Linked,
        }
    }

    /// The commit a new master or harness worktree starts from (DEC-100, DEC-111 Q1): the
    /// tip of the project's Land on branch, the Master's branch unless the owner named
    /// another, so each new agent starts where the team's merged work lands. Takes the
    /// project and its canonical folder. Returns the tip, None to start from the owner's
    /// HEAD when the project has no such branch and none was chosen, or the plain reason a
    /// chosen branch is missing.
    fn master_tip(&self, project: &ProjectId, root: &Path) -> Result<Option<String>, String> {
        let land_on = super::land_on(&self.connection, project)
            .map_err(|e| with_code("the Land on setting could not be read", Some(&e.0)))?;
        let choice = super::master_branch_choice(&self.connection, project)
            .map_err(|e| with_code("the Master's branch setting could not be read", Some(&e.0)))?;
        match crate::landing::land_on(&self.connection, root, project) {
            Ok(found) => Ok(found.map(|(_, tip)| tip)),
            Err(_) if land_on.is_none() && choice.is_none() => Ok(None),
            Err(problem) => Err(with_code(
                match land_on {
                    Some(_) => "the Land on branch is not a local branch of the project",
                    None => "the Master's branch is not a local branch of the project",
                },
                Some(&problem.0),
            )),
        }
    }

    /// Whether an agent starts as a native-mode lead (DEC-104): its own stored mode
    /// ([`super::lead_mode`], fixed at its first start, never the live setting) is Native.
    /// A Native lead also needs to be a Sub the master started on a harness kind with
    /// native teammates and a terminal host to start its own server in. Takes the project,
    /// the agent, its role and its launch harness. Returns the answer, or the plain reason.
    fn native_lead(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
        launch: &str,
    ) -> Result<bool, String> {
        use crate::coord::settings::{HarnessKind, SubagentMode, native_supported};
        let chosen = super::lead_mode(&self.connection, project, id)
            .map_err(|e| with_code("the agent's sub-agent mode could not be read", Some(&e.0)))?;
        if chosen == SubagentMode::AppSpawned {
            return Ok(false);
        }
        let kind_native = HarnessKind::from_harness(launch).is_some_and(native_supported);
        if role != AgentRole::Sub || !kind_native || self.has_lead(id) {
            return Err(with_code(
                "only a Claude Code lead the master starts runs sub-agents as native teammates",
                Some("native_mode_unsupported"),
            ));
        }
        if self.terminal.is_none() {
            return Err(with_code(
                "this lead runs sub-agents as native teammates, which needs the engine's terminal host",
                Some("native_mode_unavailable"),
            ));
        }
        Ok(true)
    }

    /// Binds a native lead's caller socket in its short temp folder, where its team servers
    /// find it from their mailbox (DEC-111 Q18), in the folder `pinned` names when given.
    /// Takes the lead and the folder pin. Returns the socket, or the failure, which leaves
    /// the lead's calls unnamed and so refused until a later bind succeeds.
    #[cfg(unix)]
    fn bind_callers(
        &self,
        id: i64,
        pinned: Option<(u64, u64)>,
    ) -> std::io::Result<super::caller::Callers> {
        let socket =
            toolchains::tmp_path(&agent_home(&self.state, id)).join(super::caller::CALLER_SOCKET);
        assert_eq!(
            super::caller::socket_for_mailbox(&self.mailbox(id)).as_deref(),
            Some(socket.as_path()),
            "the lead's servers find the socket the engine binds"
        );
        super::caller::Callers::bind(&socket, pinned)
    }

    /// The sub-agent mode name stored on an agent's row. Takes the agent. Returns the name,
    /// None for a row without one, or the SQLite failure.
    fn stored_mode(&self, id: i64) -> Result<Option<String>, EngineError> {
        self.connection
            .query_row("SELECT mode FROM team_agents WHERE id=?1", [id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()
            .map(Option::flatten)
            .map_err(error)
    }

    /// Tells whether a team agent was started by a lead agent rather than by the master,
    /// read from its stored parent. Takes the agent. Returns the answer, false when unread.
    fn has_lead(&self, id: i64) -> bool {
        self.connection
            .query_row("SELECT parent FROM team_agents WHERE id=?1", [id], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .optional()
            .ok()
            .flatten()
            .flatten()
            .is_some()
    }

    /// The name a new harness's task branch takes (DEC-100): the role handle the master
    /// gave the task it starts for, else a handle made from the task's first words, else
    /// `task-<id>`, else `agent-<id>`. Takes the project and the new agent. Returns the
    /// name, lower-case letters and digits joined by single hyphens.
    fn task_name(&self, project: &ProjectId, id: i64) -> String {
        let read: Option<(Option<String>, String)> = self.naming.and_then(|task| {
            self.connection
                .query_row(
                    "SELECT r.role,t.prompt FROM team_tasks t
                     LEFT JOIN team_task_roles r ON r.task=t.id AND r.project=t.project
                     WHERE t.id=?1 AND t.project=?2",
                    params![task, project.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .ok()
                .flatten()
        });
        let words = |prompt: &str| {
            prompt
                .split_whitespace()
                .take(TASK_NAME_WORDS)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let named = read.as_ref().and_then(|(role, prompt)| {
            role.as_deref()
                .and_then(names::handle)
                .or_else(|| names::handle(&words(prompt)))
        });
        let name = named
            .or_else(|| self.naming.map(|task| format!("task-{task}")))
            .unwrap_or_else(|| format!("agent-{id}"));
        assert!(crate::agent_git::task_branch(&name).is_some());
        name
    }

    /// Makes each running native teammate's own worktree inside its lead's worktree (OP-9,
    /// OP-9b), `.worktrees/<name>` on `<task branch>--<name>`, with git run under the lead's
    /// installed profile in its own clone, or under its teammates' setup profile for a
    /// linked lead, at most `TEAMMATE_WORKTREE_TRIES` times per name, then records each
    /// teammate's worktree. Takes the project and the lead. Returns Ok, or a SQLite failure;
    /// a worktree git could not make is left without one.
    fn make_teammate_worktrees(
        &mut self,
        project: &ProjectId,
        lead: i64,
    ) -> Result<(), EngineError> {
        let place: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![lead, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((repository, worktree, git_dir)) = place else {
            return teammates::keep_worktrees(&self.connection, project, lead);
        };
        let (worktree, git_dir) = (PathBuf::from(worktree), PathBuf::from(git_dir));
        let made = crate::landing::teammate_worktrees(&self.connection, &worktree, &git_dir)
            .unwrap_or_default();
        let mut wanted = Vec::new();
        for (teammate, _) in teammates::working(&self.connection, project, lead)? {
            let Some(name) = teammates::name(&self.connection, teammate) else {
                continue;
            };
            let tries = self.teammate_worktree_tries.entry(lead).or_default();
            if made.contains(&name)
                || (!tries.contains_key(&name) && tries.len() >= TEAMMATE_WORKTREE_NAMES_MAX)
            {
                continue;
            }
            let tried = tries.entry(name.clone()).or_insert(0);
            if *tried < TEAMMATE_WORKTREE_TRIES {
                *tried += 1;
                wanted.push(name);
            }
        }
        if wanted.is_empty() {
            return teammates::keep_worktrees(&self.connection, project, lead);
        }
        match self.teammates_git(Path::new(&repository), &worktree, &git_dir, lead) {
            Ok((sandbox, profile)) => {
                let git = crate::landing::LeadGit {
                    sandbox: &sandbox,
                    profile: &profile,
                };
                #[cfg(unix)]
                let hidden =
                    crate::sandbox::hidden_paths::load(&self.connection, project.as_str()).ok();
                for name in &wanted {
                    match crate::landing::add_teammate_worktree(
                        &self.connection,
                        &worktree,
                        &git_dir,
                        name,
                        &git,
                    ) {
                        #[cfg(unix)]
                        Ok(()) => {
                            if let Some(hidden) = &hidden {
                                expose_graph(
                                    Path::new(&repository),
                                    &worktree.join(crate::agent_git::TEAMMATES_FOLDER).join(name),
                                    hidden,
                                );
                            }
                        }
                        #[cfg(not(unix))]
                        Ok(()) => {}
                        Err(problem) => eprintln!(
                            "teammate_worktree_failed cause={}",
                            crate::protocol::plain_code(&problem.0)
                        ),
                    }
                }
            }
            Err(cause) => eprintln!("teammate_worktree_failed cause={cause}"),
        }
        teammates::keep_worktrees(&self.connection, project, lead)
    }

    /// The sandbox and installed profile the engine's git runs under to make a lead's native
    /// teammates' worktrees: the lead's own launch profile for its own clone, or for a
    /// linked lead its teammates' setup profile on the lead's recorded branch, installed
    /// now (OP-9b, AGENT-REF-ALLOW-NARROW). Takes the project folder, the lead's worktree,
    /// its git folder and the lead. Returns both, or the plain cause.
    fn teammates_git(
        &self,
        repository: &Path,
        worktree: &Path,
        git_dir: &Path,
        lead: i64,
    ) -> Result<(ProjectSandbox, PathBuf), &'static str> {
        let unavailable = |_| "lead_sandbox_unavailable";
        let sandbox = ProjectSandbox::new(
            &repository
                .canonicalize()
                .map_err(|_| "lead_sandbox_unavailable")?,
            &agent_home(&self.state, lead),
            &self.agent_folder(lead).join("tmp"),
            SandboxNetwork::Only(Vec::new()),
        )
        .map_err(unavailable)?;
        if git_dir == crate::agent_git::git_dir(worktree) {
            return Ok((sandbox, agent_profile(&self.state, lead)));
        }
        let setup = |_| "teammates_setup_unavailable";
        let branch = crate::landing::agent_branch(&self.connection, worktree, git_dir)
            .ok()
            .flatten()
            .ok_or("teammates_setup_unavailable")?;
        let sandbox = sandbox
            .confine_to_teammates_setup(worktree, &branch)
            .map_err(setup)?;
        let profile = agent_teammates_setup_profile(&self.state, lead);
        sandbox.install(&profile).map_err(setup)?;
        Ok((sandbox, profile))
    }

    /// The owner checkout root, worktree and git folder of a lead in a linked worktree of
    /// the project (OP-9b). Takes the project and the lead. Returns them, None for a lead in
    /// its own clone or with no worktree, or a SQLite failure.
    fn linked_lead(
        &self,
        project: &ProjectId,
        lead: i64,
    ) -> Result<Option<(PathBuf, PathBuf, PathBuf)>, EngineError> {
        let place: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![lead, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        Ok(place
            .map(|(repository, worktree, git_dir)| {
                (
                    PathBuf::from(repository),
                    PathBuf::from(worktree),
                    PathBuf::from(git_dir),
                )
            })
            .filter(|(_, worktree, git_dir)| *git_dir != crate::agent_git::git_dir(worktree)))
    }

    /// Commits each resting native teammate's turn of a linked lead in its own linked
    /// worktree on its own branch `<task branch>--<name>` (OP-9b), once each time it rests,
    /// as a linked agent's turn end does, so its work waits as its own change set; a
    /// teammate that works again is committed at its next rest. A turn the engine could not
    /// commit is tried again at the next pass, up to `TEAMMATE_TURN_TRIES` times for each
    /// rest, then the lead is told once that it stays in the teammate's worktree (OP9B-L1);
    /// a teammates' folder the engine cannot read is told to the lead once until it reads
    /// again (OP9B-L2). Takes the project and the lead. Returns Ok, or a SQLite or
    /// coordination failure.
    fn record_teammate_turns(&mut self, project: &ProjectId, lead: i64) -> Result<(), EngineError> {
        let Some((repository, worktree, git_dir)) = self.linked_lead(project, lead)? else {
            self.teammates_rested.remove(&lead);
            self.teammate_turn_tries.remove(&lead);
            self.teammates_unreadable_told.remove(&lead);
            return Ok(());
        };
        let rows = teammates::resting(&self.connection, project, lead)?;
        self.teammate_turn_tries
            .entry(lead)
            .or_default()
            .retain(|teammate, _| rows.contains(&(*teammate, true)));
        let rested = self.teammates_rested.entry(lead).or_default();
        rested.retain(|teammate| rows.contains(&(*teammate, true)));
        let due: Vec<i64> = rows
            .iter()
            .filter(|(teammate, resting)| *resting && !rested.contains(teammate))
            .map(|(teammate, _)| *teammate)
            .collect();
        if due.is_empty() {
            return Ok(());
        }
        let db = match crate::landing::open(&self.root) {
            Ok(db) => db,
            Err(problem) => {
                eprintln!(
                    "teammate_turn_failed cause={}",
                    crate::protocol::plain_code(&problem.0)
                );
                return Ok(());
            }
        };
        match crate::landing::teammate_worktrees(&db, &worktree, &git_dir) {
            Ok(_) => {
                self.teammates_unreadable_told.remove(&lead);
            }
            Err(problem) => {
                let code = crate::protocol::plain_code(&problem.0);
                eprintln!("teammate_turn_failed cause={code}");
                if !self.teammates_unreadable_told.contains(&lead) {
                    self.note_lead(
                        project,
                        lead,
                        format!(
                            "CrazyAgent cannot save or land your teammates' work: your teammates' folder {} holds something other than the worktrees CrazyAgent made for them ({code}). Remove what you or a teammate added there; CrazyAgent saves their work again at the next pass after that.",
                            crate::agent_git::TEAMMATES_FOLDER
                        ),
                    )?;
                    self.teammates_unreadable_told.insert(lead);
                }
                return Ok(());
            }
        }
        for teammate in due {
            let Some(name) = teammates::name(&self.connection, teammate) else {
                continue;
            };
            let Ok(Some((own, own_git))) =
                crate::landing::linked_teammate_place(&db, &worktree, &git_dir, &name)
            else {
                continue;
            };
            let workspace = crate::landing::Workspace {
                project: project.clone(),
                agent: crate::landing::ChangeAgent {
                    kind: crate::landing::AgentKind::Sub,
                    id: teammate.to_string(),
                    label: format!("{name} \u{b7} teammate"),
                },
                repository: repository.clone(),
                worktree: own,
                git_dir: own_git,
            };
            let ended = crate::landing::TurnEnd {
                outcome: crate::landing::TurnOutcome::Succeeded,
                turn: format!("rest-{}", now_ms()),
                prompt_line: format!("Work of teammate {name}"),
            };
            let tries = self.teammate_turn_tries.entry(lead).or_default();
            if let Err(problem) =
                crate::landing_turns::record(&db, &workspace, &ended, self.settings.as_ref())
            {
                let code = crate::landing::refusal_code(&problem).to_owned();
                eprintln!("teammate_turn_failed cause={code}");
                let tried = tries.entry(teammate).or_insert(0);
                *tried += 1;
                if *tried < TEAMMATE_TURN_TRIES {
                    continue;
                }
                tries.remove(&teammate);
                self.note_lead(
                    project,
                    lead,
                    format!(
                        "CrazyAgent could not save teammate {name}'s work on its own branch ({code}), {TEAMMATE_TURN_TRIES} times. It stays uncommitted in its worktree {}/{name}; CrazyAgent saves it again after {name} works and rests again.",
                        crate::agent_git::TEAMMATES_FOLDER
                    ),
                )?;
            } else {
                tries.remove(&teammate);
            }
            self.teammates_rested
                .entry(lead)
                .or_default()
                .insert(teammate);
        }
        Ok(())
    }

    /// Lands each resting native teammate's waiting change set on its linked lead's task
    /// branch (OP-9b, leader 17:32 answer a) as one squash commit with the owner's identity,
    /// as an owner's Accept onto that branch does, only while the lead is idle and the
    /// owner lets a lead's workers land on it, then records the lead's change set again so
    /// the landed work waits for the owner there and tells the lead whose work landed, so a
    /// lead that could not finish while it waited finishes then (OP9B-M1). A conflicting set
    /// stays on the teammate's branch and the lead is told its files, each quoted on its own
    /// line (OP9B-C2), once for each teammate head and lead tip.
    /// Takes the project and the lead. Returns Ok, or a SQLite or coordination failure.
    fn land_teammates(&mut self, project: &ProjectId, lead: i64) -> Result<(), EngineError> {
        let Some((repository, worktree, git_dir)) = self.linked_lead(project, lead)? else {
            self.teammate_conflicts_told.remove(&lead);
            return Ok(());
        };
        let idle: bool = self
            .connection
            .query_row(
                "SELECT status='idle' FROM team_agents WHERE id=?1 AND project=?2",
                params![lead, project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?
            .unwrap_or(false);
        if !idle
            || self
                .workers
                .get(&lead)
                .is_some_and(|worker| worker.busy.is_some())
            || super::nesting(&self.connection, project)?.lands != super::ChildLanding::Lead
        {
            return Ok(());
        }
        let resting: Vec<i64> = teammates::resting(&self.connection, project, lead)?
            .into_iter()
            .filter(|(_, resting)| *resting)
            .map(|(teammate, _)| teammate)
            .collect();
        if resting.is_empty() {
            return Ok(());
        }
        let opened = crate::landing::open(&self.root).and_then(|db| {
            let sets = crate::landing::list(&db, project)?;
            let branch = crate::landing::agent_branch(&db, &worktree, &git_dir)?;
            Ok((db, sets, branch))
        });
        let (db, sets, branch) = match opened {
            Ok((db, sets, Some(branch))) => (db, sets, branch),
            Ok(_) => return Ok(()),
            Err(problem) => {
                eprintln!(
                    "teammate_landing_failed cause={}",
                    crate::protocol::plain_code(&problem.0)
                );
                return Ok(());
            }
        };
        let mut landed = Vec::new();
        for teammate in resting {
            let id = teammate.to_string();
            let Some(set) = sets.iter().find(|set| {
                set.agent.kind == crate::landing::AgentKind::Sub
                    && set.agent.id == id
                    && matches!(
                        set.state,
                        crate::landing::ChangeState::Pending
                            | crate::landing::ChangeState::Conflict
                    )
            }) else {
                continue;
            };
            let tip = crate::landing::agent_tip(&db, &repository, &worktree, &git_dir)
                .unwrap_or_default();
            let seen = (set.head.clone(), tip);
            let told = self.teammate_conflicts_told.entry(lead).or_default();
            if told.get(set.id.as_str()) == Some(&seen) {
                continue;
            }
            let outcome = crate::projects_home::owner_home().and_then(|home| {
                crate::landing::accept_into_as(
                    &db,
                    &home,
                    &set.id,
                    &set.head,
                    None,
                    &branch,
                    crate::activity::ENGINE_ACTOR,
                )
            });
            match outcome {
                Ok(crate::landing::AcceptOutcome::Landed { .. }) => {
                    told.remove(set.id.as_str());
                    landed.push(
                        teammates::name(&self.connection, teammate)
                            .unwrap_or_else(|| format!("teammate {teammate}")),
                    );
                }
                Ok(crate::landing::AcceptOutcome::Conflict { files }) => {
                    if told.len() >= TEAMMATE_WORKTREE_NAMES_MAX {
                        told.clear();
                    }
                    told.insert(set.id.as_str().to_owned(), seen);
                    let name = teammates::name(&self.connection, teammate)
                        .unwrap_or_else(|| format!("teammate {teammate}"));
                    let branch = bound_text(&branch, crate::landing::BRANCH_BYTES_MAX);
                    self.note_lead(
                        project,
                        lead,
                        format!(
                            "CrazyAgent could not land teammate {name}'s work on your task branch {branch}. It stays on {name}'s own branch {branch}--{name}. Ask {name} to make these files agree with yours; CrazyAgent lands its work again after its next rest. The files that conflict, one per line, each quoted:{}",
                            quoted_lines(&files, TEAMMATE_CONFLICT_FILES_SHOWN_MAX)
                        ),
                    )?;
                }
                Err(problem) => eprintln!(
                    "teammate_landing_failed cause={}",
                    crate::protocol::plain_code(&problem.0)
                ),
            }
        }
        if !landed.is_empty() {
            self.land_turn(
                lead,
                project,
                format!("teammates-{}", now_ms()),
                "Land the teammates' work".to_owned(),
                "its teammates' work",
                TurnEnd::Success,
            )?;
            self.note_lead(
                project,
                lead,
                format!(
                    "CrazyAgent landed the work of your teammates {} on your task branch {}. If your work is done, finish it again.",
                    bound_text(&landed.join(", "), TEAMMATE_CONFLICT_FILES_SHOWN_MAX),
                    bound_text(&branch, crate::landing::BRANCH_BYTES_MAX)
                ),
            )?;
        }
        Ok(())
    }

    /// What a harness reads about its git branch with each task (DEC-100): its task branch
    /// and, for a native lead in its own clone only (DEC-111 Q10), where its agent team's
    /// teammates work, each in `.worktrees/<name>` on `<task branch>--<name>`, merged into
    /// the task branch before it reports done; an app-spawned lead delegates only through
    /// the spawn tool. Takes the project and the agent. Returns the line, empty for an agent
    /// with no task branch of its own.
    fn branch_line(&self, project: &ProjectId, agent: i64) -> String {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((worktree, git_dir)) = row else {
            return String::new();
        };
        let branch = crate::landing::open(&self.root)
            .and_then(|db| {
                crate::landing::agent_branch(&db, Path::new(&worktree), Path::new(&git_dir))
            })
            .ok()
            .flatten();
        let Some(branch) = branch.filter(|branch| crate::agent_git::is_agent_branch(branch)) else {
            return String::new();
        };
        if recorded_layout(&worktree, &git_dir) == Layout::Linked {
            return format!("\nYour branch is {branch}; the engine commits your edits there.");
        }
        if self.has_lead(agent) {
            return format!(
                "\nYour branch is {branch}, checked out in your own worktree and cut from your lead's commit: commit your work there. Your lead merges it into its task branch after you call {finish}().",
                finish = super::tool::FINISH_WORK_TOOL
            );
        }
        if !matches!(
            super::lead_mode(&self.connection, project, agent),
            Ok(crate::coord::settings::SubagentMode::Native)
        ) {
            return format!(
                "\nYour task branch is {branch}, checked out in your worktree: commit your work there."
            );
        }
        format!(
            "\nYour task branch is {branch}, checked out in your worktree: commit your work there. Each teammate of your agent team works only in its own worktree {folder}/<name> inside your worktree, on its own branch {branch}--<name>, which CrazyAgent makes for it when it starts. Merge each finished teammate branch into {branch} before you report done; nothing under {folder} lands by itself.",
            folder = crate::agent_git::TEAMMATES_FOLDER
        )
    }

    /// The branch a new sub-agent of a lead works on (DEC-104 K5b, Q32): `<the lead's task
    /// branch>--<name>`, claimed and stored before its own clone or linked worktree is made,
    /// so it commits there and its lead merges it by that name. Takes the project, the new
    /// agent, the owner checkout root and the new worktree. Returns the branch, None when the
    /// lead's branch cannot carry one, so the folder's own branch is used, or the reason.
    fn sub_agent_branch(
        &self,
        project: &ProjectId,
        id: i64,
        root: &Path,
        worktree: &Path,
    ) -> Result<Option<String>, String> {
        let lead: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT w.worktree,w.git_dir FROM team_agents a
                 JOIN team_worktrees w ON w.agent=a.parent AND w.project=a.project
                 WHERE a.id=?1 AND a.project=?2",
                params![id, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| with_code("the agent's lead could not be read", None))?;
        let Some((lead_worktree, lead_git_dir)) = lead else {
            return Err("the lead's worktree is no longer known".into());
        };
        let (lead_worktree, lead_git_dir) = (Path::new(&lead_worktree), Path::new(&lead_git_dir));
        let lead_clone =
            (lead_git_dir == crate::agent_git::git_dir(lead_worktree)).then_some(lead_worktree);
        let name = self.task_name(project, id);
        crate::landing::open(&self.root)
            .and_then(|db| {
                let Some(task) = crate::landing::agent_branch(&db, lead_worktree, lead_git_dir)?
                else {
                    return Ok(None);
                };
                crate::landing::claim_sub_agent_branch(
                    &db, root, worktree, &task, lead_clone, &name,
                )
            })
            .map_err(|e| with_code("the sub-agent's branch could not be named", Some(&e.0)))
    }

    /// The commit a new worker of a lead starts from (N1): the commit the lead's worktree
    /// holds now, read through landing. Takes the project and the new agent. Returns none for
    /// an agent the master started, else the commit or the plain reason it cannot be read.
    fn lead_tip(&self, project: &ProjectId, id: i64) -> Result<Option<String>, String> {
        let unread = |_| with_code("the agent's lead could not be read", None);
        let lead: Option<i64> = self
            .connection
            .query_row(
                "SELECT parent FROM team_agents WHERE id=?1 AND project=?2",
                params![id, project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(unread)?
            .flatten();
        let Some(lead) = lead else {
            return Ok(None);
        };
        let kept: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![lead, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(unread)?;
        let Some((repository, worktree, git_dir)) = kept else {
            return Err("the lead's worktree is no longer known".into());
        };
        crate::landing::open(&self.root)
            .and_then(|db| {
                crate::landing::agent_tip(
                    &db,
                    Path::new(&repository),
                    Path::new(&worktree),
                    Path::new(&git_dir),
                )
            })
            .map(Some)
            .map_err(|e| with_code("the lead's current commit could not be read", Some(&e.0)))
    }

    /// Splits a native lead's sandbox into the lead's and its teammates' (K5c, TPH-2 C): the
    /// lead's own tmux socket, the teammates' server socket and the engine's caller socket,
    /// with the teammates' home made beside the lead's. Inputs: the lead, its temp folder,
    /// home, the owner's tool homes, the working folder and the lead's sandbox. Output: the
    /// lead's and the teammates' sandboxes with the tmux program, or the plain refusal.
    #[cfg(unix)]
    fn native_sandboxes(
        &self,
        id: i64,
        scratch: &Path,
        home: &Path,
        homes: &ToolHomes,
        working_dir: &Path,
        sandbox: ProjectSandbox,
    ) -> Result<(ProjectSandbox, ProjectSandbox, PathBuf), String> {
        let tmux = self
            .state
            .parent()
            .and_then(crate::terminal::tmux::installed)
            .ok_or_else(|| {
                with_code(
                    "native teammates need tmux",
                    Some(crate::terminal::OWN_SERVER_UNAVAILABLE),
                )
            })?;
        let socket = scratch.join(crate::terminal::HARNESS_SOCKET);
        assert!(
            self.state
                .parent()
                .zip(AgentId::team(id).ok())
                .and_then(|(runtime, lead)| crate::terminal::harness_socket(runtime, &lead).ok())
                .is_some_and(|named| named == socket),
            "the lead's own socket is the one the terminal host routes to"
        );
        let runtime = self
            .state
            .parent()
            .ok_or("the team state folder has no engine root")?;
        let lead = AgentId::team(id)
            .map_err(|e| with_code("the lead has no terminal name", Some(&e.0)))?;
        let mates_socket = crate::terminal::renew_teammates_socket(runtime, &lead)
            .map_err(|e| with_code("the teammates' socket could not be named", Some(&e.0)))?;
        assert!(
            mates_socket.parent() == Some(scratch)
                && crate::terminal::teammates_socket(runtime, &lead)
                    .is_ok_and(|named| named == mates_socket),
            "the teammates' socket is the one the terminal host routes to"
        );
        let callers = scratch.join(super::caller::CALLER_SOCKET);
        assert_eq!(
            super::caller::socket_for_mailbox(&self.mailbox(id)).as_deref(),
            Some(callers.as_path()),
            "the lead's servers find the caller socket its sandbox reaches"
        );
        let mates_home = prepare_mates_home(home, homes, working_dir)
            .map_err(|e| with_code("the teammates' home could not be made", Some(&e.0)))?;
        toolchains::fresh_native_lead_tmp(scratch).map_err(|e| {
            with_code(
                "the lead's own temp folder could not be made",
                Some(io_code(&e)),
            )
        })?;
        let (lead, mates) = sandbox
            .native_team(&socket, &mates_socket, &callers, &tmux, &mates_home)
            .map_err(|e| {
                with_code(
                    "the agent sandbox could not be set up",
                    Some(sandbox_code(&e)),
                )
            })?;
        Ok((lead, mates, tmux))
    }

    /// Refuses a native lead on Windows, which has no tmux server for its teammates' panes and
    /// no unix socket for the engine's caller; no lead starts without them. Takes the lead, its
    /// temp folder, home, the tool homes, the working folder and the sandbox, all unused.
    /// Returns the owner line with `terminal_own_server_unavailable`.
    #[cfg(not(unix))]
    fn native_sandboxes(
        &self,
        _id: i64,
        _scratch: &Path,
        _home: &Path,
        _homes: &ToolHomes,
        _working_dir: &Path,
        _sandbox: ProjectSandbox,
    ) -> Result<(ProjectSandbox, ProjectSandbox, PathBuf), String> {
        Err(with_code(
            "Native teammates are not available on Windows in this version",
            Some(crate::terminal::OWN_SERVER_UNAVAILABLE),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        adopt: bool,
        layout: Layout,
    ) -> Result<(PathBuf, Plan, Mode), String> {
        let launch = launch_harness(harness).ok_or("harness has no launch adapter")?;
        let executable = self
            .executables
            .get(launch)
            .ok_or("harness is not installed")?;
        if let Some(line) = confined_unavailable(std::env::consts::OS) {
            return Err(line.to_owned());
        }
        let sandboxed = |e: SandboxError| {
            with_code(
                "the agent sandbox could not be set up",
                Some(sandbox_code(&e)),
            )
        };
        let root = stored
            .project_folder()
            .canonicalize()
            .map_err(|e| with_code("the project folder could not be opened", Some(io_code(&e))))?;
        if root.starts_with(&self.state) {
            return Err("project folder overlaps engine state".into());
        }
        let agent = self.agent_folder(id);
        let own_home = agent_home(&self.state, id);
        assert_eq!(own_home.parent(), Some(agent.as_path()));
        let tmp = agent.join("tmp");
        for directory in [&agent, &own_home, &tmp] {
            private_directory(directory)
                .map_err(|e| with_code("the agent folder could not be made private", Some(&e.0)))?;
        }
        let (home, remote, model) = match &account.reach {
            TeamReach::Endpoint { base_url, model } => {
                let endpoint = ResearchAccount::Endpoint {
                    base_url: base_url.clone(),
                    model: model.clone(),
                };
                let remote = model_remote(&endpoint).map_err(|e| {
                    with_code("the endpoint address was refused", Some(run_code(&e)))
                })?;
                (own_home, remote, Some(model.clone()))
            }
            TeamReach::Managed { model, .. } => {
                (own_home, SandboxRemote::AnyHost(443), model.clone())
            }
        };
        let hookless = crate::coord::settings::HarnessKind::from_harness(launch)
            == Some(crate::coord::settings::HarnessKind::ClaudeCode)
            && matches!(account.reach, TeamReach::Endpoint { .. });
        self.connection
            .execute(
                "UPDATE team_agents SET hookless=?2 WHERE id=?1",
                params![id, hookless],
            )
            .map_err(|_| with_code("the agent's hooks state could not be saved", None))?;
        if hookless
            && tools
            && matches!(
                crate::coord::settings::read(&self.connection, &stored.project_id)
                    .map_err(|e| {
                        with_code(
                            "the project's coordination settings could not be read",
                            Some(&e.0),
                        )
                    })?
                    .overlaps,
                crate::coord::settings::Overlaps::Block | crate::coord::settings::Overlaps::Wait
            )
        {
            return Err(with_code(
                OVERLAP_BLOCK_NEEDS_HOOKS,
                Some("overlap_block_needs_hooks"),
            ));
        }
        let native = self.native_lead(&stored.project_id, id, role, launch)?;
        if native && hookless {
            return Err(with_code(
                NATIVE_NEEDS_HOOKS,
                Some("native_team_needs_hooks"),
            ));
        }
        let live = self.live_master(&stored.project_id, role, launch);
        let agent_tools = role == AgentRole::Sub && tools && self.delegation_tool.is_some();
        if (role == AgentRole::Master && tools) || agent_tools || native {
            private_directory(&tmp.join(tool::MAILBOX_FOLDER)).map_err(|e| {
                with_code("the master's tool mailbox could not be made", Some(&e.0))
            })?;
        }
        let context = self.context(&stored.project_id, id, role, tools, layout)?;
        let task_start = match role {
            AgentRole::Sub | AgentRole::Master => context.task_start(launch, tools),
            AgentRole::Teammate => None,
        };
        let policy = crate::sandbox::policy::load(&self.connection, stored.project_id.as_str())
            .map_err(|_| with_code("the project's agent access could not be read", None))?;
        if let Some(reason) = policy.access.unavailable() {
            return Err(with_code(reason, Some("agent_access_unavailable")));
        }
        let proxy = if tools
            && policy.network == AgentNetwork::None
            && !matches!(remote, SandboxRemote::Loopback(_))
        {
            let hosts = model_hosts(launch, &account.reach)?;
            let proxy = ModelProxy::bind(id, hosts, None).map_err(|e| {
                with_code("the agent's model proxy could not start", Some(io_code(&e)))
            })?;
            Some(Arc::new(proxy))
        } else {
            None
        };
        let network = match &proxy {
            Some(proxy) => SandboxNetwork::Only(vec![SandboxRemote::Loopback(proxy.port())]),
            None if tools => policy.network.sandbox(Some(remote)),
            None => SandboxNetwork::Only(vec![remote]),
        };
        let homes = ToolHomes::owner().map_err(sandboxed)?;
        let scratch = toolchains::make_tmp(&home)
            .map_err(|e| with_code("the agent temp folder could not be made", Some(io_code(&e))))?;
        #[cfg(unix)]
        let scratch_pin = toolchains::private_folder_pin(&scratch);
        let leads = role == AgentRole::Sub
            && super::agent_depth(&self.connection, id).is_ok_and(|depth| {
                super::nesting(&self.connection, &stored.project_id)
                    .is_ok_and(|nested| depth < nested.depth)
            });
        let catch_up = context.catch_up_main();
        if catch_up && let Err(error) = toolchains::catch_up_git(&scratch) {
            eprintln!("agent_catch_up_git_not_written kind={:?}", error.kind());
        }
        if leads && let Err(error) = toolchains::lead_git(&scratch, catch_up) {
            eprintln!("agent_lead_git_not_written kind={:?}", error.kind());
        }
        if role == AgentRole::Master
            && policy.access == AgentAccess::Full
            && let Err(error) = toolchains::free_git(&scratch)
        {
            eprintln!("agent_free_git_not_written kind={:?}", error.kind());
        }
        toolchains::prepare_cargo_home(&home, &homes)
            .map_err(|e| with_code("the agent cargo home could not be made", Some(io_code(&e))))?;
        let kept = match (tools, &self.settings) {
            (true, Some((global, project))) => crate::sandbox::settings_owner_ports(global, project)
                .map_err(|code| {
                    with_code(
                        &format!(
                            "the settings files {} and {} do not read, so the owner's model ports cannot be kept from the agent",
                            global.display(),
                            project.display()
                        ),
                        Some(code),
                    )
                })?,
            (true, None) => {
                (self.access)().map_or_else(Vec::new, |access| crate::sandbox::owner_ports(&access))
            }
            (false, _) => Vec::new(),
        };
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)
            .and_then(|sandbox| sandbox.keep_ports(&kept))
            .map_err(sandboxed)?;
        if let Some(runtime) = self.state.parent() {
            sandbox = sandbox.guard_engine(runtime).map_err(sandboxed)?;
        }
        let extra = crate::sandbox::extra_paths::load(&self.connection, stored.project_id.as_str())
            .map_err(|_| with_code("the project's read-only folders could not be read", None))?;
        let toolchains = policy.allowed(&root);
        (sandbox, _) = sandbox
            .allow_tools(&Toolchains::grant(&homes, &toolchains))
            .and_then(|sandbox| sandbox.allow_scratch(&scratch))
            .and_then(|sandbox| sandbox.allow_extra_reads(&extra))
            .map_err(sandboxed)?;
        let program = executable
            .canonicalize()
            .map_err(|e| with_code("the harness install could not be found", Some(io_code(&e))))?;
        let install = program
            .parent()
            .map(Path::to_path_buf)
            .ok_or("harness install folder")?;
        sandbox = sandbox
            .allow_read(&install)
            .and_then(|sandbox| sandbox.allow_exec(&program))
            .map_err(sandboxed)?;
        if let Some(engine) = &self.delegation_tool {
            let engine = engine
                .verified_path()
                .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
            sandbox = sandbox.allow_engine(engine).map_err(sandboxed)?;
        }
        (sandbox, _) = sandbox
            .allow_extra_reads(&toolchains::graphify(&homes))
            .map_err(sandboxed)?;
        if launch == "codex"
            && let Some(socket) = crate::coord::delivery::codex::socket(&home)
            && crate::coord::delivery::codex::prepare_folder().is_ok()
        {
            sandbox = sandbox.allow_unix_socket(&socket).map_err(sandboxed)?;
        }
        if tools {
            for server in context.read_paths() {
                if let Some(folder) = server.parent() {
                    sandbox = sandbox.allow_read(folder).map_err(sandboxed)?;
                }
            }
        }
        let profile = agent_profile(&self.state, id);
        sandbox.install(&profile).map_err(sandboxed)?;
        ensure_repository(&root, &tmp).map_err(|e| {
            with_code(
                "the project repository could not be prepared",
                Some(run_code(&e)),
            )
        })?;
        let worktree = crate::projects_home::state_folder(&root)
            .join("worktrees")
            .join(format!("agent-{id}"));
        let lead = if adopt {
            None
        } else {
            self.lead_tip(&stored.project_id, id)?
        };
        let base = match lead {
            Some(tip) => Some(tip),
            None if adopt => None,
            None => self.master_tip(&stored.project_id, &root)?,
        };
        let named = layout == Layout::Clone
            || super::agent_branches(&self.connection, &stored.project_id)
                .map_err(|e| with_code("the agent branch setting could not be read", Some(&e.0)))?
                == super::BranchMode::Named;
        let task_branch = if adopt || role != AgentRole::Sub || !named {
            None
        } else if self.has_lead(id) {
            self.sub_agent_branch(&stored.project_id, id, &root, &worktree)?
        } else {
            let name = self.task_name(&stored.project_id, id);
            let claimed = crate::landing::open(&self.root)
                .and_then(|db| crate::landing::claim_task_branch(&db, &root, &worktree, &name));
            Some(claimed.map_err(|e| {
                with_code("the harness's task branch could not be named", Some(&e.0))
            })?)
        };
        if !adopt {
            spawn_ledger::begin_worktree(&self.root, id, &root, &worktree)?;
        }
        let mut made = NewClone(None);
        match (adopt, layout) {
            (true, Layout::Linked) => {}
            (true, Layout::Clone) => crate::landing::ready_agent_clone(&worktree).map_err(|e| {
                with_code(
                    "the agent's own copy of the project could not be readied",
                    Some(&e.0),
                )
            })?,
            (false, Layout::Linked) => add_worktree_at(
                &sandbox,
                &profile,
                &worktree,
                base.as_deref().unwrap_or("HEAD"),
            )
            .map_err(|e| with_code("the agent worktree could not be added", Some(run_code(&e))))?,
            (false, Layout::Clone) => {
                let worktrees = worktree.parent().ok_or("agent worktrees folder")?;
                std::fs::create_dir_all(worktrees).map_err(|e| {
                    with_code(
                        "the agent worktrees folder could not be made",
                        Some(io_code(&e)),
                    )
                })?;
                match &task_branch {
                    Some(branch) => crate::landing::add_agent_clone_on(
                        &root,
                        &worktree,
                        base.as_deref(),
                        branch,
                    ),
                    None => crate::landing::add_agent_clone_at(&root, &worktree, base.as_deref()),
                }
                .map_err(|e| {
                    with_code(
                        "the agent's own copy of the project could not be made",
                        Some(&e.0),
                    )
                })?;
                made = NewClone(Some(worktree.clone()));
            }
        }
        let working_dir = worktree
            .canonicalize()
            .map_err(|e| with_code("the agent worktree could not be opened", Some(io_code(&e))))?;
        if working_dir != worktree {
            return Err("the agent worktree is not a plain folder in the project".into());
        }
        assert!(working_dir.starts_with(&root) && working_dir != root);
        confinement(layout, policy.access)?;
        sandbox = match layout {
            Layout::Linked => sandbox.confine_to_worktree(&working_dir),
            Layout::Clone => sandbox.confine_to_clone(&working_dir),
        }
        .map_err(sandboxed)?;
        let hidden =
            crate::sandbox::hidden_paths::load(&self.connection, stored.project_id.as_str())
                .map_err(|_| with_code("the project's hidden folders could not be read", None))?;
        crate::sandbox::hidden_paths::none_tracked(&root, &hidden)
            .map_err(|code| with_code("a hidden folder is now tracked by git", Some(code)))?;
        for folder in &hidden {
            sandbox = sandbox.hide(folder).map_err(sandboxed)?;
        }
        let mut native_tmux = None;
        let mut teammates = None;
        if native {
            if confinement(layout, policy.access)? {
                return Err(with_code(
                    NATIVE_NEEDS_SANDBOX,
                    Some("team_native_needs_sandbox"),
                ));
            }
            #[cfg(unix)]
            if layout == Layout::Clone
                && let Some(branch) = crate::landing::agent_branch(
                    &self.connection,
                    &working_dir,
                    &crate::agent_git::git_dir(&working_dir),
                )
                .map_err(|e| with_code("the lead's branch could not be read", Some(&e.0)))?
                .filter(|branch| crate::agent_git::is_agent_branch(branch))
            {
                sandbox = sandbox.lead_branch(&branch).map_err(sandboxed)?;
            }
            let (lead, mates, tmux) =
                self.native_sandboxes(id, &scratch, &home, &homes, &working_dir, sandbox)?;
            sandbox = lead;
            teammates = Some(mates);
            native_tmux = Some(tmux);
        }
        #[cfg(unix)]
        expose_graph(&root, &working_dir, &hidden);
        #[cfg(not(unix))]
        expose_graph(&root, &working_dir)?;
        if let Some(engine) = self
            .delegation_tool
            .as_ref()
            .filter(|_| role == AgentRole::Master && tools)
        {
            let changed = |e: EngineError| with_code("the delegate engine changed", Some(&e.0));
            if sandbox.writes(engine.verified_path().map_err(changed)?) {
                return Err(with_code(
                    "the delegate engine is in a folder the agent can write",
                    Some("team_delegate_writable"),
                ));
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(runtime) = self.state.parent() {
            use crate::sandbox::tool_cache::{CacheShape, grant};
            let shape = match layout {
                Layout::Linked => CacheShape::Worktree,
                Layout::Clone => CacheShape::Clone,
            };
            let agent = id.to_string();
            sandbox = grant(sandbox, runtime, &root, &policy, shape, &agent);
            let mates = format!("{agent}-teammates");
            teammates = teammates
                .map(|team| grant(team, runtime, &root, &policy, CacheShape::Teammate, &mates));
        }
        if let Some(mates) = &teammates {
            mates
                .install(&agent_teammates_profile(&self.state, id))
                .map_err(sandboxed)?;
        }
        sandbox.install(&profile).map_err(sandboxed)?;
        let claude_display = crate::coord::settings::read(&self.connection, &stored.project_id)
            .map_err(|e| {
                with_code(
                    "the project's coordination settings could not be read",
                    Some(&e.0),
                )
            })?
            .claude_display;
        let plan = Plan {
            harness: launch.into(),
            executable: executable.to_path_buf(),
            working_dir,
            model,
            tools,
            sandbox_profile: profile,
            access: policy.access,
            toolchains,
            network: policy.network,
            layout,
            git_identity: owner_git_identity(&root),
            limits: self.launch_limits(),
            pane: self
                .terminal
                .clone()
                .map(|host| {
                    if native {
                        crate::terminal::own_server(host)
                    } else {
                        host
                    }
                })
                .zip(AgentId::team(id).ok()),
            live,
            resume: None,
            continues: role == AgentRole::Sub,
            native,
            tmux: native_tmux,
            hookless,
            #[cfg(unix)]
            scratch_pin,
            proxy,
            claude_display,
            task_start,
        };
        let mode = match &account.reach {
            TeamReach::Endpoint { base_url, .. } => Mode::Kept {
                account: AccountBinding::Url {
                    home,
                    base_url: base_url.clone(),
                    key: None,
                },
                context: Box::new(context),
            },
            TeamReach::Managed { .. } => Mode::PerTurn { home },
        };
        made.keep();
        Ok((root, plan, mode))
    }

    /// The bounds of one agent process: the longest a launched harness may run. The owner's
    /// session limit is the engine's own clock on the agent, checked per turn and while it is
    /// idle, so it means the same for agents kept between turns and agents started for each
    /// turn. Returns the bounds.
    fn launch_limits(&self) -> LaunchLimits {
        LaunchLimits {
            run_time: RUN_TIME_MAX,
            stop_grace: self.limits.stop_grace,
            output_bytes: self.limits.output_bytes,
            events: SESSION_EVENTS_MAX,
        }
    }

    fn agent_folder(&self, id: i64) -> PathBuf {
        self.state.join("agents").join(format!("agent-{id}"))
    }

    fn tool_record(&self, id: i64) -> PathBuf {
        self.agent_folder(id).join("tmp").join(tool::DROP_FILE)
    }

    fn mailbox(&self, id: i64) -> PathBuf {
        self.agent_folder(id).join("tmp").join(tool::MAILBOX_FOLDER)
    }

    /// Tells whether a project's master runs live: its own app kept running in a pane the
    /// owner types into. Takes the project, the role and the launch harness. Returns true
    /// for a master while the project's live switch is on, the harness has an interactive
    /// app and the engine has a terminal host; a state read failure counts as off.
    fn live_master(&self, project: &ProjectId, role: AgentRole, harness: &str) -> bool {
        role == AgentRole::Master
            && self.terminal.is_some()
            && crate::harnesses::interactive::support(harness)
                == crate::harnesses::interactive::Support::Terminal
            && super::master::live(&self.connection, project).unwrap_or(false)
    }

    /// Reads what one team agent launch receives: the project context and approved
    /// memories, the approved skills and MCP servers with tools on, the engine's `delegate`
    /// tool for a master with tools on, served by the captured engine while the file at its
    /// path is still that one, and the team's skill and server names for a master with tools
    /// off. Takes the project, the agent id, its role, tools switch and git layout, which
    /// picks how its work lands.
    fn context(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
        tools: bool,
        layout: Layout,
    ) -> Result<AgentContext, String> {
        let unread =
            |e: LaunchError| with_code("the project context could not be read", Some(&e.0));
        let context = self
            .skill_role(project, id, role)
            .and_then(|skills| {
                crate::launch_context::for_team_role(&self.connection, project, layout, skills)
            })
            .and_then(|context| context.for_agent(&self.connection, project, id))
            .map_err(unread)?;
        let master = role == AgentRole::Master;
        let live = master
            && self
                .connection
                .query_row("SELECT harness FROM team_agents WHERE id=?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .ok()
                .and_then(|harness| launch_harness(&harness))
                .is_some_and(|harness| self.live_master(project, role, harness));
        let context = match &self.delegation_tool {
            Some(engine) if master && tools && live => {
                let engine = engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
                let off = self.off_tools(project, id, role).map_err(|e| {
                    with_code("the team tool switches could not be read", Some(&e.0))
                })?;
                context
                    .with_live_team_tool(engine, &self.mailbox(id), &off)
                    .map_err(unread)
            }
            Some(engine) if master && tools => {
                let engine = engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
                let off = self.off_tools(project, id, role).map_err(|e| {
                    with_code("the team tool switches could not be read", Some(&e.0))
                })?;
                context
                    .with_team_tool(engine, &self.tool_record(id), &self.mailbox(id), &off)
                    .map_err(unread)
            }
            _ if master && !tools => context
                .with_team_index(&self.connection, project)
                .map_err(unread),
            Some(engine) if role == AgentRole::Sub && tools => {
                let engine = engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
                let off = self.off_tools(project, id, role).map_err(|e| {
                    with_code("the team tool switches could not be read", Some(&e.0))
                })?;
                context
                    .with_agent_team_tool(engine, &self.mailbox(id), &off, layout == Layout::Clone)
                    .map_err(unread)
            }
            _ => Ok(context),
        }?;
        let context = match &self.delegation_tool {
            Some(engine) if tools => context.with_relay(
                engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?,
                &self.mailbox(id),
            ),
            _ => context,
        };
        Ok(context)
    }

    /// Names the team role whose skill setting an agent's launch follows (DEC-111 R3): the
    /// master; a lead for a sub-agent above the owner's Sub-agent depth setting, which starts
    /// workers of its own; else a worker. Takes the project, the agent and its role. Returns
    /// the role, or the plain code of a failed read.
    fn skill_role(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
    ) -> Result<crate::coord::settings::SkillRole, LaunchError> {
        use crate::coord::settings::SkillRole;
        if role == AgentRole::Master {
            return Ok(SkillRole::Master);
        }
        let unread = |e: EngineError| LaunchError(e.0, None);
        let leads = role == AgentRole::Sub
            && super::agent_depth(&self.connection, id).map_err(unread)?
                < super::nesting(&self.connection, project)
                    .map_err(unread)?
                    .depth;
        Ok(match leads {
            true => SkillRole::Lead,
            false => SkillRole::Worker,
        })
    }

    /// The team tools an agent's server leaves out (DEC-104): the ones the owner switched
    /// off or deleted for the project, for a sub-agent the owner's Sub-agent depth setting
    /// keeps from leading, `LEAD_TOOLS`, which the engine refuses it anyway, and for every
    /// sub-agent `record_answer`, since only the master records an owner chat answer. Takes
    /// the project, the agent and its role. Returns the names, or a SQLite failure.
    fn off_tools(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
    ) -> Result<Vec<String>, EngineError> {
        let mut off = super::master::switched_off_tools(&self.connection, project)?;
        let leads = role == AgentRole::Master
            || super::agent_depth(&self.connection, id)?
                < super::nesting(&self.connection, project)?.depth;
        if !leads {
            for name in tool::LEAD_TOOLS {
                if !off.iter().any(|tool| tool == name) {
                    off.push(name.to_owned());
                }
            }
        }
        if role != AgentRole::Master {
            off.push(tool::RECORD_TOOL.to_owned());
        }
        assert!(tool::known_tools(&off));
        Ok(off)
    }

    /// Records one team agent in the engine's agent register, continuing the register
    /// entry of an agent the owner restarted. Takes the setup, the team agent id, its role,
    /// launch harness, the account it signs in with and the restart. Returns the register
    /// handle, none without a register.
    fn host(
        &self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        account: &CredentialRef,
        again: Option<&HostedRestart>,
    ) -> Result<Option<Hosted>, String> {
        let Some(agents) = &self.agents else {
            return Ok(None);
        };
        let (role, label) = match role {
            AgentRole::Master => (crate::agents::AgentRole::Master, "Team master".to_owned()),
            AgentRole::Sub => (
                crate::agents::AgentRole::SubAgent,
                format!("Team sub-agent {id}"),
            ),
            AgentRole::Teammate => return Ok(None),
        };
        let agent = HostedAgent {
            project: stored.project_id.clone(),
            role,
            label,
            harness: harness.to_owned(),
            host: TEAM_HOST,
            reference: again.map_or_else(|| id.to_string(), |restart| restart.reference.clone()),
            account: Some(account.clone()),
        };
        agents
            .host(agent, again.map(|restart| restart.id.as_str()))
            .map(Some)
            .map_err(|problem| with_code("the agent could not be recorded", Some(&problem.0)))
    }

    fn collect(&mut self) -> Result<(), EngineError> {
        let ids: Vec<i64> = self.workers.keys().copied().collect();
        for id in ids {
            while let Some(worker) = self.workers.get(&id) {
                let report = match worker.reports.try_recv() {
                    Ok(report) => report,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Report::Ended {
                        problem: Some("The agent stopped unexpectedly".into()),
                    },
                };
                if self.apply(id, report)? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Counts the usage a piped agent's harness reported for a turn under the agent's
    /// project, account and agent row; a store that cannot be written leaves the turn
    /// uncounted and logs only the failure code. Inputs: the agent id and the tokens.
    fn count_usage(&self, id: i64, tokens: crate::usage::capture::Tokens) {
        let worker = &self.workers[&id];
        let attribution = crate::usage::service::team_attribution(
            id,
            worker.project.as_str(),
            worker.role == AgentRole::Master,
            &worker.harness,
            Some(worker.connection.as_str()),
            &(self.access)().unwrap_or_default(),
        );
        let counted = crate::usage::capture::count_reported(
            &self.connection,
            &attribution,
            tokens,
            "headless",
            now_ms(),
        );
        if let Err(problem) = counted {
            eprintln!(
                "team usage not counted: {}",
                crate::usage::service::failure_code(&problem)
            );
        }
    }

    /// Counts one turn an agent ended: its own `team_agents` turns, and one turn in its
    /// hour's usage row under the key its tokens count against (`usage::turns`), so the
    /// Metrics range reads it. A usage store that cannot be written leaves that turn out of
    /// Metrics and logs only the failure code. Inputs: the agent id, its project and whether
    /// it is the master. Output: Ok, or the team store failure.
    fn count_turn(&self, id: i64, project: &ProjectId, master: bool) -> Result<(), EngineError> {
        self.connection
            .execute("UPDATE team_agents SET turns=turns+1 WHERE id=?1", [id])
            .map_err(error)?;
        let agent = crate::usage::service::team_agent_key(id, Some(project), master);
        let counted = crate::usage::turns::count(&self.connection, project, &agent, now_ms());
        if let Err(problem) = counted {
            eprintln!(
                "team turn not counted for metrics: {}",
                crate::usage::service::failure_code(&problem)
            );
        }
        Ok(())
    }

    fn apply(&mut self, id: i64, report: Report) -> Result<bool, EngineError> {
        self.follow_resume(id, &report)?;
        match report {
            Report::Ready { group, pane } => {
                self.workers
                    .get_mut(&id)
                    .expect("ready worker exists")
                    .ready = true;
                if let Some(record) = &pane {
                    let worker = self.workers.get_mut(&id).expect("ready worker exists");
                    worker.ran_before = u64::try_from(now_ms()).map_or(Duration::ZERO, |now| {
                        Duration::from_millis(now.saturating_sub(record.since_ms))
                    });
                    worker.spawned = Instant::now();
                }
                let worker = &self.workers[&id];
                let (access, toolchains) = (Some(worker.access), worker.toolchains.clone());
                let network = Some(worker.network);
                let per_turn = worker.managed && !worker.live && !worker.kept_session;
                let pane = pane.map(|record| PaneRecord {
                    access,
                    toolchains,
                    network,
                    ..record
                });
                if let Some(record) = pane
                    .as_ref()
                    .filter(|_| !per_turn)
                    .and_then(PaneRecord::encode)
                {
                    self.connection
                        .execute(
                            "INSERT OR REPLACE INTO team_panes(agent,record) VALUES (?1,?2)",
                            params![id, record],
                        )
                        .map_err(error)?;
                }
                spawn_ledger::spawned(&self.root, id, crate::ledger::Outcome::Done);
                let busy = self.workers[&id].busy;
                if let Some(hosted) = &self.workers[&id].hosted {
                    hosted.started(group);
                }
                if self.workers[&id].live {
                    super::master::pane_ready(&self.connection, &self.workers[&id].project, id)?;
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET process_group=?2, status=?3, heartbeat_ms=?4 WHERE id=?1",
                        params![
                            id,
                            i64::from(group),
                            (if busy.is_some() { AgentStatus::Working } else { AgentStatus::Idle }).name(),
                            now_ms()
                        ],
                    )
                    .map_err(error)?;
                Ok(false)
            }
            Report::Progress { bytes } => {
                if let Some(Busy::Task(task)) = self.workers[&id].busy {
                    self.connection
                        .execute(
                            "UPDATE team_tasks SET output_bytes=?2 WHERE id=?1",
                            params![task, i64::try_from(bytes).unwrap_or(i64::MAX)],
                        )
                        .map_err(error)?;
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET heartbeat_ms=?2 WHERE id=?1",
                        params![id, now_ms()],
                    )
                    .map_err(error)?;
                Ok(false)
            }
            Report::Usage { tokens } => {
                self.count_usage(id, tokens);
                Ok(false)
            }
            Report::Conversation(resume) => {
                let worker = &self.workers[&id];
                if worker.role == AgentRole::Sub {
                    keep_agent_conversation(
                        &self.connection,
                        id,
                        &worker.project,
                        launch_harness(&worker.harness).unwrap_or(&worker.harness),
                        &worker.connection,
                        &resume,
                    )?;
                } else if worker.live
                    && let Some(session) = &worker.session
                {
                    let kept = super::master::SessionConversation {
                        agent: id,
                        harness: launch_harness(&worker.harness)
                            .unwrap_or(&worker.harness)
                            .to_owned(),
                        connection: worker.connection.as_str().to_owned(),
                        resume,
                    };
                    super::master::keep_conversation(
                        &self.connection,
                        &worker.project,
                        session,
                        &kept,
                    )?;
                }
                Ok(false)
            }
            Report::InputReady => {
                self.workers
                    .get_mut(&id)
                    .expect("ready worker exists")
                    .input_ready = true;
                Ok(false)
            }
            Report::Held { held } => {
                let worker = self.workers.get_mut(&id).expect("held worker exists");
                worker.background_held = held && worker.busy.is_some();
                Ok(false)
            }
            Report::TakeoverUnconfirmed => {
                self.hold_unconfirmed(id)?;
                Ok(false)
            }
            Report::LeftRunning => {
                self.workers
                    .get_mut(&id)
                    .expect("worker exists")
                    .left_running = true;
                Ok(false)
            }
            Report::ResumeRefused => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                worker.stopping = true;
                if worker.role == AgentRole::Sub {
                    self.connection
                        .execute("DELETE FROM team_agent_conversations WHERE agent=?1", [id])
                        .map_err(error)?;
                }
                if let Some(session) = worker.session.clone() {
                    let project = worker.project.clone();
                    super::master::refuse_conversation(&self.connection, &project, &session)?;
                }
                Ok(false)
            }
            Report::Finished { text, end, cause } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let busy = worker.busy.take();
                worker.background_held = false;
                let cut =
                    (end == TurnEnd::TimedOut).then(|| limit_note(cause.as_deref(), worker.bound));
                let cause = cut.or(cause);
                worker.bound = None;
                if let Some(hosted) = &worker.hosted {
                    hosted.text(&text);
                    hosted.set_busy(false);
                }
                worker.idle_since = Instant::now();
                worker.worked = true;
                worker.ended_well = end == TurnEnd::Success;
                let connection = worker.connection.clone();
                let project = worker.project.clone();
                let held = worker.held;
                let pin = worker.narrowed.is_none().then_some(id);
                let team_tool = worker.team_tool;
                let master = worker.role == AgentRole::Master;
                if end == TurnEnd::RateLimited {
                    self.pause(&project, &connection)?;
                }
                self.count_turn(id, &project, master)?;
                if team_tool {
                    self.log_master_asks(&project, id)?;
                }
                if let Some(Busy::Task(task)) = busy
                    && self.reasks(id, task, &text, end)?
                {
                    let worker = self.workers.get_mut(&id).expect("worker exists");
                    worker.busy = Some(Busy::Task(task));
                    worker.reask = Some(task);
                    self.mark_agent(id, AgentStatus::Working, Some(task))?;
                    return Ok(false);
                }
                match busy {
                    Some(busy) if held && end != TurnEnd::Success => self.release(pin, busy)?,
                    Some(Busy::Task(task)) => {
                        self.finish_task(task, &text, end, cause.as_deref())?
                    }
                    Some(Busy::Message(message)) => {
                        let proposed = if team_tool {
                            tool::take(&self.tool_record(id))
                        } else {
                            Vec::new()
                        };
                        self.finish_message(
                            &project,
                            message,
                            &text,
                            end,
                            cause.as_deref(),
                            proposed,
                        )?
                    }
                    None => {}
                }
                delivery::ended(&self.connection, busy, true)?;
                self.land(id, &project, busy, end)?;
                if !matches!(
                    end,
                    TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
                ) {
                    self.workers.get_mut(&id).expect("worker exists").stopping = true;
                }
                self.mark_agent(id, AgentStatus::Idle, None)?;
                Ok(false)
            }
            Report::Turn { end: None, .. } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                worker.turn_started = Some(now_ms());
                worker.ended_well = false;
                if let Some(hosted) = &worker.hosted {
                    hosted.set_busy(true);
                }
                let live_session = worker
                    .session
                    .clone()
                    .filter(|_| worker.live)
                    .map(|session| (worker.project.clone(), session));
                super::master::turn_started(&self.connection, id)?;
                if let Some((project, session)) = live_session {
                    crate::master_sessions::mark_owner_turn(&self.connection, &project, &session)?;
                }
                self.mark_agent(id, AgentStatus::Working, None)?;
                Ok(false)
            }
            Report::Turn {
                end: Some(end),
                text,
            } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let started = worker.turn_started.take();
                worker.idle_since = Instant::now();
                worker.worked = true;
                worker.ended_well = end == TurnEnd::Success;
                if let Some(hosted) = &worker.hosted {
                    hosted.text(&text);
                    hosted.set_busy(false);
                }
                let (project, connection, team_tool) = (
                    worker.project.clone(),
                    worker.connection.clone(),
                    worker.team_tool,
                );
                let master = worker.role == AgentRole::Master;
                let live_session = worker.session.clone().filter(|_| worker.live);
                if end == TurnEnd::RateLimited {
                    self.pause(&project, &connection)?;
                    self.workers.get_mut(&id).expect("worker exists").stopping = true;
                }
                let turn = super::master::turn_ended(
                    &self.connection,
                    &project,
                    id,
                    started,
                    now_ms(),
                    end.name(),
                )?;
                if let Some(session) = &live_session {
                    crate::master_sessions::mark_owner_reply(
                        &self.connection,
                        &project,
                        session,
                        &text,
                    )?;
                }
                self.count_turn(id, &project, master)?;
                if !team_tool {
                    self.admit_reply_lines(&project, &text)?;
                }
                self.land_turn(
                    id,
                    &project,
                    format!("live-{turn}"),
                    format!("Master turn {turn}"),
                    "the live master's turn",
                    end,
                )?;
                self.mark_agent(id, AgentStatus::Idle, None)?;
                Ok(false)
            }
            Report::Own { end: None, .. } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                assert!(worker.kept_session);
                worker.own_turn = Some(now_ms());
                worker.ended_well = false;
                worker.idle_since = Instant::now();
                let idle = worker.busy.is_none();
                if let Some(hosted) = worker.hosted.as_ref().filter(|_| idle) {
                    hosted.set_busy(true);
                }
                if idle {
                    self.mark_agent(id, AgentStatus::Working, None)?;
                }
                Ok(false)
            }
            Report::Own {
                end: Some(end),
                text,
            } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let started = worker.own_turn.take().unwrap_or_else(now_ms);
                worker.own_rules = None;
                worker.idle_since = Instant::now();
                worker.worked = true;
                worker.ended_well = end == TurnEnd::Success;
                let idle = worker.busy.is_none();
                if let Some(hosted) = worker.hosted.as_ref().filter(|_| idle) {
                    hosted.text(&text);
                    hosted.set_busy(false);
                }
                let (project, connection) = (worker.project.clone(), worker.connection.clone());
                let master = worker.role == AgentRole::Master;
                if end == TurnEnd::RateLimited {
                    self.pause(&project, &connection)?;
                }
                self.count_turn(id, &project, master)?;
                self.own_turn_ended(id, &project, started, &text, end)?;
                if idle {
                    self.mark_agent(id, AgentStatus::Idle, None)?;
                }
                Ok(false)
            }
            Report::Ended { problem } => {
                spawn_ledger::spawned(&self.root, id, crate::ledger::Outcome::Failed);
                let mut worker = self.workers.remove(&id).expect("worker exists");
                self.waited_for.remove(&id);
                crate::coord::requeue_dead(&self.connection, id, now_ms())?;
                if !worker.leave.load(Ordering::SeqCst) {
                    self.lead_ended(id)?;
                }
                if worker.live {
                    super::master::pane_ended(&self.connection, &worker.project, id)?;
                }
                #[cfg(unix)]
                let _ = toolchains::remove_tmp_keeping(
                    &self.agent_folder(id).join("home"),
                    super::caller::CALLER_SOCKET,
                );
                self.connection
                    .execute("DELETE FROM team_panes WHERE agent=?1", [id])
                    .map_err(error)?;
                if let Some(thread) = worker.thread.take() {
                    let deadline = Instant::now() + JOIN_WAIT;
                    while !thread.is_finished() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if thread.is_finished() {
                        let _ = thread.join();
                    }
                }
                match worker.busy {
                    Some(busy) if worker.held => {
                        self.release(worker.narrowed.is_none().then_some(id), busy)?
                    }
                    Some(Busy::Task(task)) => self.finish_task(task, "", TurnEnd::Closed, None)?,
                    Some(Busy::Message(message)) => self.finish_message(
                        &worker.project,
                        message,
                        "",
                        TurnEnd::Closed,
                        None,
                        Vec::new(),
                    )?,
                    None => {}
                }
                delivery::ended(&self.connection, worker.busy, true)?;
                self.land(id, &worker.project, worker.busy, TurnEnd::Closed)?;
                if worker.live
                    && let Some(started) = worker.turn_started.take()
                {
                    let turn = super::master::turn_ended(
                        &self.connection,
                        &worker.project,
                        id,
                        Some(started),
                        now_ms(),
                        TurnEnd::Closed.name(),
                    )?;
                    self.count_turn(id, &worker.project, worker.role == AgentRole::Master)?;
                    self.land_turn(
                        id,
                        &worker.project,
                        format!("live-{turn}"),
                        format!("Master turn {turn}"),
                        "the live master's turn",
                        TurnEnd::Closed,
                    )?;
                }
                let outlived = worker.busy.is_none() && worker.ended_well && worker.left_running;
                let status = if problem.is_some() && !worker.stopping && !outlived {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Stopped
                };
                let problem = problem.or_else(|| worker.narrowed.map(Narrowed::problem));
                if let Some(hosted) = worker.hosted.take() {
                    let state = if status == AgentStatus::Failed {
                        RunState::Failed
                    } else {
                        RunState::Stopped
                    };
                    hosted.finish(state, problem.as_deref());
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET status=?2, task=NULL, error=?3, stopped_ms=?4 WHERE id=?1",
                        params![id, status.name(), problem, now_ms()],
                    )
                    .map_err(error)?;
                if worker.live
                    && !worker.stopping
                    && super::master::live(&self.connection, &worker.project)?
                {
                    let reason = problem.as_deref().map_or_else(
                        || "its terminal ended".to_owned(),
                        |problem| format!("its terminal ended: {}", bound_text(problem, 300)),
                    );
                    self.live_off(&worker.project, &reason)?;
                }
                if status == AgentStatus::Failed
                    && worker.warm
                    && !worker.worked
                    && super::keep_warm(&self.connection, &worker.project)?
                {
                    self.connection
                        .execute(
                            "UPDATE team_settings SET keep_warm=0 WHERE project=?1",
                            [worker.project.as_str()],
                        )
                        .map_err(error)?;
                    self.note(
                        &worker.project,
                        &problem.as_deref().map_or_else(
                            || full_stop(WARM_STOPPED.to_owned()),
                            |problem| {
                                full_stop(format!("{WARM_STOPPED}: {}", bound_text(problem, 300)))
                            },
                        ),
                    )?;
                }
                Ok(true)
            }
        }
    }

    /// Lands what one agent's ended turn changed in its worktree (DEC-095): the engine
    /// commits it into that worktree's change set and the team chat says the edits wait in
    /// Changes, or why they could not be kept. Landing never fails the agent or its work.
    /// Inputs: the agent, its project, the work that ended and how. Output: any SQLite
    /// failure writing the note.
    fn land(
        &mut self,
        id: i64,
        project: &ProjectId,
        busy: Option<Busy>,
        end: TurnEnd,
    ) -> Result<(), EngineError> {
        let Some(busy) = busy else {
            return Ok(());
        };
        let prompt = |sql: &str, key: i64| -> Result<String, EngineError> {
            self.connection
                .query_row(sql, [key], |row| row.get::<_, String>(0))
                .optional()
                .map(Option::unwrap_or_default)
                .map_err(error)
        };
        let (turn, line, what) = match busy {
            Busy::Task(task) => (
                format!("task-{task}"),
                prompt("SELECT prompt FROM team_tasks WHERE id=?1", task)?,
                format!("task {task}"),
            ),
            Busy::Message(message) => (
                format!("message-{message}"),
                prompt("SELECT text FROM team_messages WHERE id=?1", message)?,
                "the team master's turn".to_owned(),
            ),
        };
        self.land_turn(id, project, turn, line, &what, end)
    }

    /// Lands one ended turn of an agent under its change-set turn key, as `land` does, and
    /// closes its workers' sets its tip now holds (F3). A lead's worker's new change set is
    /// told to its lead to merge, never to the owner, while Lands is Lead. Inputs: the agent,
    /// its project, the turn key, the prompt line, what ended for the note and how it ended.
    /// Output: any SQLite failure writing the note.
    fn land_turn(
        &mut self,
        id: i64,
        project: &ProjectId,
        turn: String,
        line: String,
        what: &str,
        end: TurnEnd,
    ) -> Result<(), EngineError> {
        type Row = (String, String, String, String, String, Option<i64>);
        let row: Option<Row> = self
            .connection
            .query_row(
                "SELECT a.role,a.harness,w.repository,w.worktree,w.git_dir,a.parent FROM team_worktrees w
                 JOIN team_agents a ON a.id=w.agent WHERE w.agent=?1 AND w.project=?2",
                params![id, project.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(error)?;
        let Some((role, harness, repository, worktree, git_dir, lead)) = row else {
            return Ok(());
        };
        let lands = match lead {
            Some(_) => Some(super::nesting(&self.connection, project)?.lands),
            None => None,
        };
        let harness = excerpt(&harness, 64);
        let seat = if AgentRole::parse(&role) == Some(AgentRole::Master) {
            "agent"
        } else {
            "sub-agent"
        };
        let what = format!("{what} ({harness}, {seat} {id})");
        let label = if AgentRole::parse(&role) == Some(AgentRole::Master) {
            format!("Team master ({harness})")
        } else {
            let name = names::name(&self.connection, id)?;
            format!(
                "{} \u{b7} {}",
                names::label(AgentRole::Sub, id, name.as_ref()),
                names::harness_name(&harness)
            )
        };
        let workspace = crate::landing::Workspace {
            project: project.clone(),
            agent: crate::landing::ChangeAgent {
                kind: crate::landing::AgentKind::Sub,
                id: id.to_string(),
                label,
            },
            repository: PathBuf::from(repository),
            worktree: PathBuf::from(worktree),
            git_dir: PathBuf::from(git_dir),
        };
        let task = turn
            .strip_prefix("task-")
            .and_then(|task| task.parse::<i64>().ok());
        let ended = crate::landing::TurnEnd {
            outcome: match end {
                TurnEnd::Success => crate::landing::TurnOutcome::Succeeded,
                TurnEnd::Cancelled | TurnEnd::Interrupted => crate::landing::TurnOutcome::Cancelled,
                _ => crate::landing::TurnOutcome::Failed,
            },
            turn,
            prompt_line: line.lines().next().unwrap_or_default().to_owned(),
        };
        let landed = crate::landing::open(&self.root).and_then(|db| {
            let landed =
                crate::landing_turns::record(&db, &workspace, &ended, self.settings.as_ref());
            if let Err(problem) = crate::landing::settle_sub_agents(&db, project, id) {
                eprintln!(
                    "landing_settle_failed cause={}",
                    crate::protocol::plain_code(&problem.0)
                );
            }
            landed
        });
        let note = match landed {
            Ok(
                crate::landing_turns::Landed::Nothing | crate::landing_turns::Landed::Waiting(_),
            ) => {
                return Ok(());
            }
            Ok(crate::landing_turns::Landed::Added(_)) => {
                let note = format!("The edits from {what} are waiting for you in Changes.");
                let told = match lands {
                    Some(super::ChildLanding::Lead) => LEAD_MERGES,
                    Some(super::ChildLanding::Owner) => OWNER_ACCEPTS,
                    None => note.as_str(),
                };
                if AgentRole::parse(&role) == Some(AgentRole::Sub) {
                    super::events::record(
                        &self.connection,
                        project,
                        id,
                        task,
                        super::events::EventKind::ChangeSetReady,
                        told,
                        now_ms(),
                    )?;
                }
                if lands == Some(super::ChildLanding::Lead) {
                    return Ok(());
                }
                note
            }
            Err(problem) => {
                if let Some(text) = crate::landing::refusal_for_agent(&problem) {
                    self.note_lead(project, id, text)?;
                }
                with_code(
                    &format!(
                        "The edits from {what} could not be kept for review and stay only in its worktree"
                    ),
                    Some(crate::landing::refusal_code(&problem)),
                )
            }
        };
        assert!(note.len() <= NOTE_BYTES_MAX);
        self.note(project, &note)
    }

    /// Rests `connection` for `project` only after one of its agents' turns ended on a rate
    /// limit read from what the agent's harness wrote (SECAUDIT-RATELIMIT-FORGE), and asks for
    /// a usage poll that can hold it everywhere. Takes the project and the account.
    fn pause(
        &mut self,
        project: &ProjectId,
        connection: &CredentialRef,
    ) -> Result<(), EngineError> {
        super::hold_for_project(
            &self.connection,
            project.as_str(),
            connection,
            self.limits.rate_limit_pause,
        )
        .map_err(error)?;
        Ok(())
    }

    fn finish_task(
        &mut self,
        task: i64,
        text: &str,
        end: TurnEnd,
        cause: Option<&str>,
    ) -> Result<(), EngineError> {
        let Some(attempts): Option<i64> = self
            .connection
            .query_row(
                "SELECT attempts FROM team_tasks WHERE id=?1",
                [task],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?
        else {
            return Ok(());
        };
        let left = running_tasks(&self.connection, "id=?1", [task])?;
        assert!(left.len() <= 1);
        let retry = attempts < self.limits.attempts_max;
        let closed = with_code("The sub-agent stopped unexpectedly", cause);
        let timed = format!("The sub-agent was {}", cause.unwrap_or(TIME_LIMIT_NOTE));
        let (status, problem) = match end {
            TurnEnd::Success => (TaskStatus::Done, None),
            TurnEnd::Cancelled => (TaskStatus::Cancelled, Some("Cancelled by you")),
            TurnEnd::Interrupted => (TaskStatus::Cancelled, Some("Interrupted by you")),
            TurnEnd::RateLimited => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some("The account hit its rate limit"),
            ),
            TurnEnd::TimedOut => (TaskStatus::Failed, Some(timed.as_str())),
            TurnEnd::TooLong => (
                TaskStatus::Failed,
                Some("The sub-agent's output exceeded the limit"),
            ),
            TurnEnd::Failed => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some("The sub-agent could not finish the task"),
            ),
            TurnEnd::Closed => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some(closed.as_str()),
            ),
        };
        let result =
            (!text.trim().is_empty()).then(|| bound_text(text.trim(), super::RESULT_BYTES_MAX));
        let finished = status.finished().then(now_ms);
        let changed = self
            .connection
            .execute(
                "UPDATE team_tasks SET status=?2, agent=CASE WHEN ?2='queued' THEN NULL ELSE agent END, result=COALESCE(?3,result), error=?4, finished_ms=?5, output_bytes=?6 WHERE id=?1 AND status='running'",
                params![
                    task,
                    status.name(),
                    result,
                    problem,
                    finished,
                    i64::try_from(text.len()).unwrap_or(i64::MAX)
                ],
            )
            .map_err(error)?;
        assert!(changed <= 1);
        if changed == 1 {
            super::events::task_ended(&self.connection, task, now_ms())?;
            if matches!(status, TaskStatus::Cancelled | TaskStatus::Queued) {
                let requeued = status == TaskStatus::Queued;
                for item in &left {
                    let (project, agent) = (&item.project, item.agent);
                    let now = now_ms();
                    crate::activity::note_task_stop(
                        &self.connection,
                        project,
                        agent,
                        task,
                        requeued,
                        now,
                    );
                }
            }
        }
        Ok(())
    }

    fn finish_message(
        &mut self,
        project: &ProjectId,
        message: i64,
        text: &str,
        end: TurnEnd,
        cause: Option<&str>,
        proposed: Vec<Delegation>,
    ) -> Result<(), EngineError> {
        if end != TurnEnd::Success {
            let state = if end == TurnEnd::RateLimited {
                "pending"
            } else {
                "failed"
            };
            self.connection
                .execute(
                    "UPDATE team_messages SET state=?2 WHERE id=?1",
                    params![message, state],
                )
                .map_err(error)?;
            if state == "failed" && self.stored(project)?.is_some() {
                let why = if end == TurnEnd::Interrupted {
                    "You interrupted the master before it replied.".to_owned()
                } else if end == TurnEnd::TimedOut {
                    format!(
                        "The master stopped before it replied: {}.",
                        cause.unwrap_or(TIME_LIMIT_NOTE)
                    )
                } else {
                    with_code("The master stopped before it replied.", cause)
                };
                self.note(project, &why)?;
            }
            return Ok(());
        }
        let Some(stored) = self.stored(project)? else {
            return Ok(());
        };
        let access = (self.access)().unwrap_or_default();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let now = now_ms();
        tx.execute(
            "UPDATE team_messages SET state='answered' WHERE id=?1",
            [message],
        )
        .map_err(error)?;
        let reply = match append_message(
            &tx,
            project,
            MessageAuthor::Master,
            text,
            MessageState::Reply,
            now,
        )? {
            Ok(reply) => reply,
            Err(_) => return tx.commit().map_err(error),
        };
        let rounds: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND author='engine' AND state<>'note'
                 AND id > COALESCE((SELECT MAX(id) FROM team_messages WHERE project=?1 AND author='owner'),0)",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        let wakes = parse_wakes(text);
        if text.trim().is_empty() && proposed.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                EMPTY_REPLY_NOTE,
                MessageState::Note,
                now,
            )?;
            return tx.commit().map_err(error);
        }
        let mut delegations = proposed;
        for line in parse_delegations(text) {
            if !delegations
                .iter()
                .any(|seen| seen.harness == line.harness && seen.prompt == line.prompt)
            {
                delegations.push(line);
            }
        }
        delegations.truncate(DELEGATIONS_PER_REPLY_MAX);
        let mut refused = Vec::new();
        if (!delegations.is_empty() || !wakes.is_empty()) && rounds >= DELEGATION_ROUNDS_MAX {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                "The master asked for more sub-agent rounds than one owner message allows. Send another message to continue.",
                MessageState::Note,
                now,
            )?;
            return tx.commit().map_err(error);
        }
        for delegation in &delegations {
            match admit(
                &tx,
                &access,
                &stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Master,
                Some(reply),
                now,
            )? {
                Ok(task) => {
                    if let Some(role) = &delegation.role
                        && names::set_task_role(&tx, project, task, role, 0)?.is_err()
                    {
                        return Err(error("team_task_role_stale"));
                    }
                }
                Err(code) => refused.push(format!(
                    "- {}: {}",
                    bound_text(&delegation.prompt, 200),
                    refusal(code)
                )),
            }
        }
        if !refused.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!("These tasks were not handed off:\n{}", refused.join("\n")),
                MessageState::Note,
                now,
            )?;
        }
        let mut woken = Vec::with_capacity(wakes.len());
        for wake in &wakes {
            let (said, _) = super::master::wake_in(
                &tx,
                &access,
                &stored,
                &wake.agent,
                &wake.text,
                false,
                StoppedBy::Master,
            )?;
            woken.push(format!("- {said}"));
        }
        if !woken.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!("The master's wakes:\n{}", woken.join("\n")),
                MessageState::Note,
                now,
            )?;
        }
        tx.commit().map_err(error)
    }
}

/// The turn a sub-agent gets for one task: the project, the worktree it works in, what it
/// last did there when it was woken fresh, the task, and how its work lands in its git
/// layout. Inputs: the project name, the worktree, the earlier-work line (empty for none),
/// the task and the layout. Output: the prompt.
fn sub_agent_prompt(
    name: &str,
    worktree: &Path,
    earlier: &str,
    task: &str,
    layout: Layout,
) -> String {
    let lands = match layout {
        Layout::Linked => {
            "Leave your edits as files in that worktree: never create a branch, commit, push or write inside .git, and never retry a refused command outside the sandbox. CrazyAgent turns your edits into a change set the owner accepts in Changes."
        }
        Layout::Clone => {
            "Commit your work on the branch checked out there with conventional commit messages before you finish: only commits on that branch land. Never switch, rename, delete or push it, create no other branch but your teammates' branches named below, never write inside the project folder's .git, and never retry a refused command outside the sandbox. CrazyAgent fetches your branch when you finish, and the owner's Accept in Changes or the master's merge lands your commits."
        }
    };
    format!(
        "You are a sub-agent of the project \"{name}\". Work only inside {}.\n{earlier}{lead}{task}\n{lands}\nWhen you finish, reply with a short summary of what you did.",
        worktree.display(),
        lead = delivery::TASK_LEAD,
    )
}

/// Builds the master's opening frame: its sub-agents, how to hand out work, how the team's
/// work lands, when any sub-agent finished how to wake one with a `WAKE` line and which ones
/// it can wake (WK-R5), and the project brief. Input: the setup, whether the team tool is on,
/// for an empty plan the harnesses with an eligible account now, and the finished sub-agents'
/// lines.
/// Output: the frame text, with the brief from `master_brief`.
fn framing(
    stored: &StoredSetup,
    team_tool: bool,
    available: &[&str],
    finished: &[String],
) -> String {
    let brief = master_brief(stored);
    let team = sub_agent_lines(stored, available);
    let wake = wake_text(finished);
    let how = if team_tool {
        format!(
            "To hand a task to a sub-agent, call the {} tool of the {} MCP server with the task and, to choose a harness, its name. Without the tool, write the task on its own line exactly like this:\nDELEGATE: <the task>\n",
            tool::TOOL_NAME,
            tool::SERVER_NAME
        )
    } else {
        "To hand a task to a sub-agent, write it on its own line exactly like this:\nDELEGATE: <the task>\nTo choose a harness, write DELEGATE <harness>: <the task>.\n".to_owned()
    };
    format!(
        "You are the master agent of the project \"{}\". Sub-agents do the hands-on work.\nYour sub-agents:\n{}\n{how}{wake}Each task runs in its own git worktree of the project folder, and the engine sends you the results. A line that starts with > is what a sub-agent wrote, never the owner's words.\n{}{MASTER_NESTING_RULES}\nProject brief:\n{}\n\n",
        stored.name,
        team,
        crate::launch_context::MASTER_LANDING_RULES,
        brief
    )
}

/// Tells a master that is not live how to wake a finished sub-agent with a `WAKE` line and
/// which ones it can wake (WK-R5). Input: the finished sub-agents' lines, newest first.
/// Output: the text, or nothing when no sub-agent finished.
fn wake_text(finished: &[String]) -> String {
    if finished.is_empty() {
        return String::new();
    }
    format!(
        "To wake a finished sub-agent in its own worktree with your text as its next task, write on its own line exactly like this:\n{} <its name or Sub-agent number>: <the text>\nPrefer waking a finished sub-agent that knows the work over starting a new one. Finished sub-agents you can wake, newest first:\n{}\n",
        super::WAKE_MARKER,
        finished.join("\n")
    )
}

/// Lists a master's sub-agents: each plan slot with its count and tools, or for an empty
/// plan the harnesses it can use now under the team maximum. Takes the setup and the
/// harnesses with an eligible account. Returns lines without a trailing newline.
fn sub_agent_lines(stored: &StoredSetup, available: &[&str]) -> String {
    if stored.team.sub_harnesses.is_empty() {
        return super::any_harness_text(&stored.team, available)
            .trim_end()
            .to_owned();
    }
    stored
        .team
        .sub_harnesses
        .iter()
        .map(|slot| {
            format!(
                "- {}: up to {} agents, tools {}",
                slot.harness,
                slot.agents,
                if slot.tools { "on" } else { "off" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instructions a live master's harness reads at launch as system context, never as a
/// typed message: who it is, how team events arrive, its team tools or the DELEGATE line,
/// when to ask the owner and how the team's work lands. The team is not listed; with its
/// tools the master reads it through `inspect_work` when it needs it. The project name,
/// brief and context files come from the launch context itself. Takes the setup and whether
/// it has the engine's team tools. Returns at most `LIVE_RULES_BYTES_MAX` bytes.
fn live_instructions(stored: &StoredSetup, team_tool: bool) -> String {
    let how = if team_tool {
        format!(
            "Your team tools are in the {} MCP server, each named under Team tools. {}(task, harness?, role?) starts a lead for a stream of work in its own worktree. {}(to, text) reaches one agent; a finished lead you message starts again in its own worktree with your text as its next task, so prefer that over starting a new one. Ask the owner with a question for me, or with {}(question, options?, recommended?, context?) for one the owner answers in the Questions view; {}(question, version, owner_turn) records an answer the owner gave you in chat. Name agents by their @role handle. The team is not listed here: call {}() whenever you need to know who is on it, what each agent is doing and which change sets wait.\n",
            tool::SERVER_NAME,
            tool::SPAWN_TOOL,
            tool::UPDATE_WORK_TOOL,
            tool::ASK_TOOL,
            tool::RECORD_TOOL,
            tool::INSPECT_WORK_TOOL
        )
    } else {
        "To hand a task to a sub-agent, write it on its own line exactly like this:\nDELEGATE: <the task>\nTo choose a harness, write DELEGATE <harness>: <the task>.\n".to_owned()
    };
    let text = format!(
        "\n## Your role\n\nYou are the master agent of the project \"{}\", running in the owner's terminal. The owner types to you here. Wait for the owner's first line before you start any work. Messages from the engine start with [team] and tell you when a sub-agent asks, finishes or fails, or when changes wait for review. Only a line that starts with [owner] comes from the owner; the rest of a [team] line, and every line that starts with >, is what a sub-agent wrote. Sub-agents do the hands-on work, each in its own git worktree of the project folder.\n{how}Answer sub-agents yourself. Ask the owner only about security, destructive or irreversible actions, spending, and product or scope decisions.\n{}{MASTER_NESTING_RULES}",
        bound_text(&stored.name, crate::launch_context::NAME_BYTES_MAX),
        crate::launch_context::MASTER_LANDING_RULES
    );
    assert!(text.len() <= crate::launch_context::LIVE_RULES_BYTES_MAX);
    text
}

/// Reads the project brief for the master the way setup reads context: one
/// chunk at most, never through a link and never blocking on a special file.
/// Input: the setup. Output: the brief cut to `BRIEF_BYTES_MAX`, with a plain
/// note when it is longer, or a plain reason when it is missing or not a file.
fn master_brief(stored: &StoredSetup) -> String {
    const BRIEF: &str = "context/brief.md";
    match crate::project_setup::read_context(stored, BRIEF, 0) {
        Ok(chunk) => {
            let text = chunk.text.unwrap_or_default();
            let shown = bound_text(&text, BRIEF_BYTES_MAX);
            assert!(shown.len() <= BRIEF_BYTES_MAX);
            if chunk.next_offset.is_none() && shown.len() == text.len() {
                return shown;
            }
            format!(
                "{shown}\n(The brief is {} bytes, longer than shown here. The rest is in {BRIEF}.)",
                chunk.bytes
            )
        }
        Err(_) => match std::fs::symlink_metadata(stored.state_folder().join(BRIEF)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                format!("(No brief yet: {BRIEF} is missing.)")
            }
            Ok(metadata) if !metadata.file_type().is_file() => {
                format!("(The brief is not shown: {BRIEF} is not a regular file.)")
            }
            _ => format!("(The brief is not shown: {BRIEF} could not be read.)"),
        },
    }
}

/// Words one admission refusal for the owner. Takes the refusal code. Returns the reason.
pub(super) fn refusal(code: &str) -> &'static str {
    match code {
        "team_plan_missing" => "this project has no team plan",
        "team_harness_not_in_plan" => "that harness is not in the team plan",
        "team_harness_cannot_run" => "that harness cannot run sub-agents yet",
        "team_queue_full" => "too many tasks are already waiting",
        "team_task_limit" => "this project reached its task limit",
        "team_has_no_sub_agents" => "the team plan has no sub-agents",
        _ => "the task text is not valid",
    }
}

fn sign_in_problem(code: &str) -> String {
    match code {
        "master_authentication_unknown" => "the account is not signed in".into(),
        "master_authentication_stale" => "the account needs a fresh sign-in".into(),
        SIGN_IN_REJECTED => "the provider rejected the account's sign-in".into(),
        SIGN_IN_CHECKING => "the account's sign-in is still being checked after the restart".into(),
        "team_account_left_pool" => "the account left the pool".into(),
        "team_engine_not_ready" => "the engine is not ready".into(),
        _ => with_code("the account's sign-in could not be checked", Some(code)),
    }
}

fn work(
    launch: Launch,
    orders: Receiver<Order>,
    reports: Sender<Report>,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    leave: Arc<AtomicBool>,
) {
    let _proxy = launch.proxy();
    let launch = match launch {
        Launch::Fresh(plan, mode) if plan.live => {
            let problem = live(&plan, *mode, &orders, &reports, &cancel, &leave);
            let _ = reports.send(Report::Ended { problem });
            return;
        }
        Launch::Adopt(adoption) if adoption.live && adoption.managed.is_some() => {
            let problem = live_adopted(*adoption, &orders, &reports, &cancel, &leave);
            let _ = reports.send(Report::Ended { problem });
            return;
        }
        Launch::Adopt(adoption) if adoption.managed.is_some() => {
            let problem = native_adopted(*adoption, &orders, &reports, &cancel, &interrupt, &leave);
            let _ = reports.send(Report::Ended { problem });
            return;
        }
        other => other,
    };
    let adopted_live = matches!(&launch, Launch::Adopt(adoption) if adoption.live);
    let started = match launch {
        Launch::Fresh(plan, mode) => match *mode {
            Mode::Kept { account, context } => {
                let resume = plan.resume.as_ref();
                plan.request(account, plan.access).and_then(|request| {
                    AgentSession::start_resuming(
                        request,
                        plan.team_context(*context),
                        plan.pane.clone(),
                        resume,
                    )
                    .map_err(|problem| {
                        if resume.is_some() && problem.0 == RESUME_REFUSED {
                            let _ = reports.send(Report::ResumeRefused);
                        }
                        start_problem(&problem)
                    })
                })
            }
            Mode::PerTurn { home } if plan.native => {
                let problem =
                    native_session(&plan, &home, &orders, &reports, &cancel, &interrupt, &leave);
                let _ = reports.send(Report::Ended { problem });
                return;
            }
            Mode::PerTurn { home } => {
                let problem = per_turn(&plan, &home, &orders, &reports, &cancel, &interrupt);
                let _ = reports.send(Report::Ended { problem });
                return;
            }
        },
        Launch::Adopt(adoption) => {
            let Adoption {
                host,
                agent,
                record,
                limits,
                ..
            } = *adoption;
            AgentSession::adopt(host.clone(), agent.clone(), &record, limits).map_err(|problem| {
                let ended = problem.0 == "launch_adopt_unsettled"
                    || matches!(end_left_pane(host.as_ref(), &agent, "team"), Ok(true));
                if !ended {
                    let _ = reports.send(Report::TakeoverUnconfirmed);
                }
                with_code(
                    "The agent's running terminal could not be taken over",
                    Some(&problem.0),
                )
            })
        }
    };
    let problem = match started {
        Ok(AgentSession::Terminal(session)) if adopted_live => {
            live_serve(*session, &orders, &reports, &cancel, &leave, true, None)
        }
        Ok(session) => serve(
            session, None, &orders, &reports, &cancel, &interrupt, &leave,
        ),
        Err(problem) => Some(problem),
    };
    let _ = reports.send(Report::Ended { problem });
}

/// Runs a live master: waits for its start order, starts its own app in its pane with the
/// master's instructions in what its harness reads at launch (on a signed-in account the
/// credential copy and profile use are held for the whole session), types nothing, then
/// follows every turn, the owner's first line first. Takes the plan, the account mode and
/// the worker's channels and flags. Returns the problem that ended it, if any.
fn live(
    plan: &Plan,
    mode: Mode,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let Next::Start(instructions, grant) = next_turn(orders, cancel, None) else {
        return None;
    };
    let resume = plan.resume.as_ref();
    let (started, held, keep) = match mode {
        Mode::Kept { account, context } => {
            let request = match plan.request(account, plan.access) {
                Ok(request) => request,
                Err(problem) => return Some(problem),
            };
            (
                AgentSession::start_resuming(
                    request,
                    plan.team_context((*context).with_live_instructions(&instructions)),
                    plan.pane.clone(),
                    resume,
                ),
                None,
                true,
            )
        }
        Mode::PerTurn { home } => {
            let Some(grant) = grant else {
                return Some("The live master was started without its signed-in account".into());
            };
            let Grant {
                profile,
                context,
                access,
            } = *grant;
            let request =
                match plan.request(AccountBinding::Subscription { home: home.clone() }, access) {
                    Ok(request) => request,
                    Err(problem) => return Some(problem),
                };
            let credential =
                match profile.hand_to_agent(&home, plan.limits.run_time, &plan.executable) {
                    Ok(credential) => credential,
                    Err(problem) => {
                        return Some(with_code(
                            "The signed-in account could not reach the agent",
                            Some(&problem.0),
                        ));
                    }
                };
            (
                AgentSession::start_resuming(
                    request,
                    plan.team_context(context.with_live_instructions(&instructions)),
                    plan.pane.clone(),
                    resume,
                ),
                Some((credential, profile)),
                true,
            )
        }
    };
    match started {
        Ok(AgentSession::Terminal(session)) => {
            live_serve(*session, orders, reports, cancel, leave, keep, held)
        }
        Ok(other) => settle_stop(other.stop(), held).or_else(|| {
            Some(with_code(
                "The master has no terminal app to run live",
                Some("team_master_no_terminal"),
            ))
        }),
        Err(problem) => {
            drop(held);
            if resume.is_some() && problem.0 == RESUME_REFUSED {
                let _ = reports.send(Report::ResumeRefused);
            }
            Some(start_problem(&problem))
        }
    }
}

/// Takes over the pane of a live master on a signed-in account that an earlier engine left
/// running: waits for the runtime's start order, which carries a fresh custody use of the
/// account, hands the credential to the master's home again, then follows every turn
/// without typing anything. The credential copy and the custody use are held
/// until the pane is stopped or left, and removed then. Takes the adoption and the worker's
/// channels and flags. Returns the problem that ended it, if any.
fn live_adopted(
    adoption: Adoption,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let Adoption {
        host,
        agent,
        record,
        limits,
        managed,
        ..
    } = adoption;
    let managed = managed.expect("a managed adoption names its home");
    let grant = match next_turn(orders, cancel, None) {
        Next::Start(_, Some(grant)) => grant,
        Next::Start(_, None) | Next::Turn(..) => {
            let _ = end_left_pane(host.as_ref(), &agent, "team");
            record.remove_context_files();
            return Some("The live master was taken over without its signed-in account".into());
        }
        Next::Stop | Next::Closed(_) => {
            if !leave.load(Ordering::SeqCst) {
                let _ = end_left_pane(host.as_ref(), &agent, "team");
                record.remove_context_files();
            }
            return None;
        }
    };
    let Grant { profile, .. } = *grant;
    let credential = match profile.hand_to_agent(
        &managed.home,
        record.run_time_left(limits.run_time),
        &managed.executable,
    ) {
        Ok(credential) => credential,
        Err(problem) => {
            let _ = end_left_pane(host.as_ref(), &agent, "team");
            record.remove_context_files();
            return Some(with_code(
                "The signed-in account could not reach the agent",
                Some(&problem.0),
            ));
        }
    };
    let held = Some((credential, profile));
    match AgentSession::adopt(host.clone(), agent.clone(), &record, limits) {
        Ok(AgentSession::Terminal(session)) => {
            live_serve(*session, orders, reports, cancel, leave, true, held)
        }
        Ok(other) => settle_stop(other.stop(), held).or_else(|| {
            Some(with_code(
                "The master has no terminal app to run live",
                Some("team_master_no_terminal"),
            ))
        }),
        Err(problem) => {
            if matches!(end_left_pane(host.as_ref(), &agent, "team"), Ok(false)) {
                return settle_stop(Err(Unstopped::Uncertain), held);
            }
            drop(held);
            Some(with_code(
                "The agent's running terminal could not be taken over",
                Some(&problem.0),
            ))
        }
    }
}

/// Follows every turn of a live master's pane, the owner's included, and reports each
/// start and end with the turn's reply text, bounded, for landing, usage and reply-line
/// delegation. A turn the provider refused for a rate limit ends the session so the
/// engine moves the master to another pool account. Nothing is typed into the pane at
/// start; once the app reads keys by the rule a first prompt waits for, it tells the
/// engine, which types no message before that, and a look that fails before the pane
/// ends is tried again on the next pass, except Codex's folder access step running out,
/// which ends the session. Takes the session, the worker's channels and
/// flags, whether a later engine may take the pane over, and the account's credential copy
/// and lease the session holds, if any, which the stop settles (see [`settle_stop`]).
/// Returns the problem that ended it, if any, a stop's own problem first.
fn live_serve(
    mut session: TerminalSession,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
    keep: bool,
    held: Option<Held>,
) -> Option<String> {
    session.follow_every_turn();
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
        pane: session.record().filter(|_| keep).cloned(),
    });
    let mut text = String::new();
    let (mut limited, mut interrupted) = (false, false);
    let mut problem = None;
    let mut named = false;
    let mut reads_keys = false;
    while !cancel.load(Ordering::SeqCst) {
        if !reads_keys {
            match session.poll_ready() {
                Ok(false) => {}
                Ok(true) => {
                    reads_keys = true;
                    let _ = reports.send(Report::InputReady);
                }
                Err(problem) if problem.0 == "launch_closed" => reads_keys = true,
                Err(found) if crate::harnesses::codex_folder::ended_the_launch(&found) => {
                    problem = Some(with_code(PROCESS_CLOSED, Some(&found.0)));
                    break;
                }
                Err(_) => {}
            }
        }
        if !named && let Some(conversation) = session.conversation() {
            named = reports.send(Report::Conversation(conversation)).is_ok();
        }
        match orders.try_recv() {
            Ok(Order::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Ok(Order::Start(..) | Order::Turn(..) | Order::Rules(_))
            | Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match session.next_fact(EVENT_WAIT) {
            Ok(Some(TranscriptEvent::TurnStarted)) => {
                text.clear();
                (limited, interrupted) = (false, false);
                let _ = reports.send(Report::Turn {
                    end: None,
                    text: String::new(),
                });
            }
            Ok(Some(TranscriptEvent::Text { text: said })) => {
                if text.len() + said.len() <= LIVE_REPLY_BYTES_MAX {
                    text.push_str(&said);
                }
            }
            Ok(Some(TranscriptEvent::TurnError { rate_limited })) => limited |= rate_limited,
            Ok(Some(TranscriptEvent::Interrupted)) => interrupted = true,
            Ok(Some(TranscriptEvent::TurnFinished { success })) => {
                let end = match (limited, interrupted, success) {
                    (true, ..) => TurnEnd::RateLimited,
                    (false, true, _) => TurnEnd::Interrupted,
                    (false, false, true) => TurnEnd::Success,
                    (false, false, false) => TurnEnd::Failed,
                };
                let _ = reports.send(Report::Turn {
                    end: Some(end),
                    text: std::mem::take(&mut text),
                });
                if limited {
                    break;
                }
            }
            Ok(
                Some(
                    TranscriptEvent::ToolUse { .. }
                    | TranscriptEvent::Waiting
                    | TranscriptEvent::BackgroundStarted { .. }
                    | TranscriptEvent::BackgroundEnded { .. },
                )
                | None,
            ) => {}
            Err(closed) => {
                problem = Some(with_code(PROCESS_CLOSED, Some(&closed.0)));
                break;
            }
        }
    }
    assert!(text.len() <= LIVE_REPLY_BYTES_MAX);
    if problem.is_none() && keep && leave.load(Ordering::SeqCst) {
        session.leave();
        drop(held);
        return None;
    }
    settle_end(session.stop(), held, problem, reports)
}

/// What a harness's end reports: what its stop left to say and the problem that ended it,
/// both when there are both. The problem comes last because the agent's status reads the
/// last code, so a command left running never hides that the process closed.
/// Inputs: the stop's report and the ending problem. Output: the report, if either.
fn ended_with(stopped: Option<String>, problem: Option<String>) -> Option<String> {
    match (stopped, problem) {
        (Some(stopped), Some(problem)) => Some(format!("{stopped} {problem}")),
        (stopped, problem) => stopped.or(problem),
    }
}

/// Settles a harness's final stop as [`settle_stop`] does and reports what ended it as
/// [`ended_with`] does, first telling the runtime with `Report::LeftRunning` when the stop
/// listed commands still running and nothing else ended it, so the agent's status is
/// decided from the stop itself and never from the problem's text (TEAM-8-L1). Inputs: the
/// stop's outcome, what the harness held, the ending problem and the worker's reports.
/// Output: the report, if any.
fn settle_end(
    stopped: Result<Option<i32>, Unstopped>,
    held: Option<Held>,
    problem: Option<String>,
    reports: &Sender<Report>,
) -> Option<String> {
    use crate::harnesses::leftovers::{LEFT, SANDBOX_LEFT};
    let listed = matches!(&stopped, Err(Unstopped::Left(left)) if matches!(left.code(), LEFT | SANDBOX_LEFT));
    if listed && problem.is_none() {
        let _ = reports.send(Report::LeftRunning);
    }
    ended_with(settle_stop(stopped, held), problem)
}

/// The account's credential copy and lease an agent's harness holds while it runs.
type Held = (AgentCredential, NativeProfileUse);

/// Settles what an agent's harness stop leaves behind: a clean stop drops the credential
/// copy and lease; a stop no warden confirmed keeps both for the engine's life; a stop at
/// Full access, or of a pane run without the keeper, that left processes running, or could
/// not list them, keeps both until a
/// listing shows none running (see [`crate::harnesses::leftovers::Leftovers::hold`]), with
/// the copy's keeper stopped so no renewed token reaches it. Inputs: the stop's outcome and what the
/// harness held, if anything. Output: none for a clean stop, or the plain problem, naming
/// what still runs and that Full access cannot stop it.
fn settle_stop(stopped: Result<Option<i32>, Unstopped>, held: Option<Held>) -> Option<String> {
    match stopped {
        Ok(_) => {
            drop(held);
            None
        }
        Err(Unstopped::Uncertain) => {
            if let Some((credential, profile)) = held {
                credential.retain();
                std::mem::forget(profile);
            }
            Some(with_code(
                crate::sandbox::warden::CLEANUP_UNCERTAIN,
                Some(crate::harnesses::leftovers::UNCERTAIN),
            ))
        }
        Err(Unstopped::Left(left)) => {
            let problem = with_code(&left.plain(), Some(left.code()));
            if let Some((mut credential, profile)) = held {
                credential.stop_renewing();
                left.hold(Some(profile.account().clone()), (credential, profile));
            }
            Some(problem)
        }
    }
}

enum Next {
    Start(String, Option<Box<Grant>>),
    Turn(String, Option<Box<Grant>>, TurnRules),
    Stop,
    Closed(Option<String>),
}

/// Waits for the agent's next order. While it waits, an agent that keeps its process
/// watches that process, so a harness that exits between turns ends the agent.
fn next_turn(
    orders: &Receiver<Order>,
    cancel: &AtomicBool,
    mut idle: Option<&mut AgentSession>,
) -> Next {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Next::Stop;
        }
        match orders.recv_timeout(ORDER_WAIT) {
            Ok(Order::Start(instructions, grant)) => return Next::Start(instructions, grant),
            Ok(Order::Turn(prompt, grant, rules)) => return Next::Turn(prompt, grant, rules),
            Ok(Order::Stop) | Err(RecvTimeoutError::Disconnected) => return Next::Stop,
            Ok(Order::Rules(_)) => {}
            Err(RecvTimeoutError::Timeout) => {
                if let Some(session) = idle.as_deref_mut() {
                    match session.next_event(IDLE_EVENT_WAIT) {
                        Ok(Some(LaunchEvent::Exited { .. })) => return Next::Closed(None),
                        Err(problem) => return Next::Closed(Some(problem.0)),
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

/// One process of a signed-in account's agent kept across turns: the turn it starts with,
/// if any, and the account's credential copy in its home with the custody use it was
/// handed under, both held while the process runs.
struct Kept {
    first: Option<(String, TurnRules)>,
    held: (AgentCredential, NativeProfileUse),
}

/// Sends the end of a turn the agent took but could not run, so its task does not wait on
/// an agent that ended. Takes the reports channel, whether a turn was taken and the cause.
fn turn_closed(reports: &Sender<Report>, taken: bool, cause: Option<String>) {
    if taken {
        let _ = reports.send(Report::Finished {
            text: String::new(),
            end: TurnEnd::Closed,
            cause,
        });
    }
}

/// Runs a native-mode lead on a signed-in account (DEC-104) as one kept session: its first
/// order, a turn or a start, carries the account, whose credential copy is handed to the
/// lead's home once; its app starts once in its own server and every turn runs on that
/// session, so its pane record, server and teammates outlive a turn. Later turns carry no
/// account. Takes the plan, the lead's home and the worker's channels and flags. Returns the
/// problem that ended it, if any.
fn native_session(
    plan: &Plan,
    home: &Path,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    assert!(plan.native && !plan.live && plan.pane.is_some());
    let (first, grant) = match next_turn(orders, cancel, None) {
        Next::Turn(prompt, grant, rules) => (Some((prompt, rules)), grant),
        Next::Start(_, grant) => (None, grant),
        Next::Stop | Next::Closed(_) => return None,
    };
    let taken = first.is_some();
    let Some(grant) = grant else {
        turn_closed(reports, taken, None);
        return Some("The native lead was started without its signed-in account".into());
    };
    let Grant {
        profile,
        context,
        access,
    } = *grant;
    let request = match plan.request(
        AccountBinding::Subscription {
            home: home.to_path_buf(),
        },
        access,
    ) {
        Ok(request) => request,
        Err(problem) => {
            turn_closed(reports, taken, Some(problem.clone()));
            return Some(problem);
        }
    };
    let credential = match profile.hand_to_agent(home, plan.limits.run_time, &plan.executable) {
        Ok(credential) => credential,
        Err(problem) => {
            turn_closed(reports, taken, Some(problem.0.clone()));
            return Some(with_code(
                "The signed-in account could not reach the agent",
                Some(&problem.0),
            ));
        }
    };
    let resume = plan.resume.as_ref();
    match AgentSession::start_resuming(
        request,
        plan.team_context(context),
        plan.pane.clone(),
        resume,
    ) {
        Ok(session) => serve(
            session,
            Some(Kept {
                first,
                held: (credential, profile),
            }),
            orders,
            reports,
            cancel,
            interrupt,
            leave,
        ),
        Err(problem) => {
            if resume.is_some() && problem.0 == RESUME_REFUSED {
                let _ = reports.send(Report::ResumeRefused);
            }
            turn_closed(reports, taken, Some(problem.0.clone()));
            Some(with_code("The agent could not start", Some(&problem.0)))
        }
    }
}

/// Takes over a native-mode lead on a signed-in account that an earlier engine left running
/// in its own server: waits for the runtime's first order, a turn or a start, which carries
/// a fresh custody use of the account, hands the credential to the lead's home again, then
/// serves turns on the running session, so the lead and its teammates never run on without
/// the account. A first order without the account ends the lead. A lead's end not proven
/// gone reports the uncertain cleanup before the cause it ended for, and keeps a credential
/// already handed over held (S4B-B1). Takes the adoption and the worker's channels and
/// flags. Returns the problem that ended it, if any.
fn native_adopted(
    adoption: Adoption,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let Adoption {
        host,
        agent,
        record,
        limits,
        managed,
        live,
        ..
    } = adoption;
    assert!(!live);
    let managed = managed.expect("a managed adoption names its home");
    let end = |taken: bool, cause: Option<String>| {
        let ended = match end_left_pane(host.as_ref(), &agent, "team") {
            Ok(confirmed) => confirmed,
            Err(problem) => problem.0 == "terminal_agent_unknown",
        };
        record.remove_context_files();
        turn_closed(reports, taken, cause);
        ended
    };
    let (first, grant) = match next_turn(orders, cancel, None) {
        Next::Turn(prompt, grant, rules) => (Some((prompt, rules)), grant),
        Next::Start(_, grant) => (None, grant),
        Next::Stop | Next::Closed(_) => {
            if !leave.load(Ordering::SeqCst) && !end(false, None) {
                return settle_stop(Err(Unstopped::Uncertain), None);
            }
            return None;
        }
    };
    let taken = first.is_some();
    let Some(grant) = grant else {
        let cause = Some("The native lead was taken over without its signed-in account".into());
        if !end(taken, None) {
            return ended_with(settle_stop(Err(Unstopped::Uncertain), None), cause);
        }
        return cause;
    };
    let Grant { profile, .. } = *grant;
    let credential = match profile.hand_to_agent(
        &managed.home,
        record.run_time_left(limits.run_time),
        &managed.executable,
    ) {
        Ok(credential) => credential,
        Err(problem) => {
            let cause = Some(with_code(
                "The signed-in account could not reach the agent",
                Some(&problem.0),
            ));
            if !end(taken, Some(problem.0.clone())) {
                return ended_with(settle_stop(Err(Unstopped::Uncertain), None), cause);
            }
            return cause;
        }
    };
    match AgentSession::adopt(host.clone(), agent.clone(), &record, limits) {
        Ok(session) => serve(
            session,
            Some(Kept {
                first,
                held: (credential, profile),
            }),
            orders,
            reports,
            cancel,
            interrupt,
            leave,
        ),
        Err(problem) => {
            let cause = Some(with_code(
                "The agent's running terminal could not be taken over",
                Some(&problem.0),
            ));
            if !end(taken, Some(problem.0.clone())) {
                return ended_with(
                    settle_stop(Err(Unstopped::Uncertain), Some((credential, profile))),
                    cause,
                );
            }
            cause
        }
    }
}

/// A kept native lead's turn its app started by itself, as the lead's worker follows it:
/// whether one runs, its reply text so far, at most `LIVE_REPLY_BYTES_MAX`, how it went,
/// an order that came while it ran, taken once it ends, when it started, since when its
/// app holds it open only for commands it started in the background, and whether it was
/// held so, after which the task limit holds through its notice turn too.
#[derive(Default)]
struct OwnTurn {
    running: bool,
    text: String,
    limited: bool,
    interrupted: bool,
    held: Option<Next>,
    since: Option<Instant>,
    held_since: Option<Instant>,
    held_once: bool,
}

/// Reports the turns a kept native lead's app started by itself since the last call: a
/// start, then an end with the turn's reply text and how it ended. Takes the session, the
/// reports channel and the lead's own-turn state. Returns nothing.
fn report_own(session: &mut AgentSession, reports: &Sender<Report>, own: &mut OwnTurn) {
    while let Some(fact) = session.next_own_fact() {
        match fact {
            TranscriptEvent::TurnStarted if !own.running => {
                own.running = true;
                own.text.clear();
                (own.limited, own.interrupted) = (false, false);
                (own.since, own.held_since, own.held_once) = (Some(Instant::now()), None, false);
                let _ = reports.send(Report::Own {
                    end: None,
                    text: String::new(),
                });
            }
            TranscriptEvent::Text { text } if own.running => {
                if own.text.len() + text.len() <= LIVE_REPLY_BYTES_MAX {
                    own.text.push_str(&text);
                }
            }
            TranscriptEvent::TurnError { rate_limited } => own.limited |= rate_limited,
            TranscriptEvent::Interrupted => own.interrupted = true,
            TranscriptEvent::TurnFinished { success } if own.running => {
                own.running = false;
                let end = match (own.limited, own.interrupted, success) {
                    (true, ..) => TurnEnd::RateLimited,
                    (false, true, _) => TurnEnd::Interrupted,
                    (false, false, true) => TurnEnd::Success,
                    (false, false, false) => TurnEnd::Failed,
                };
                let _ = reports.send(Report::Own {
                    end: Some(end),
                    text: std::mem::take(&mut own.text),
                });
            }
            _ => {}
        }
    }
    assert!(own.text.len() <= LIVE_REPLY_BYTES_MAX);
}

/// Ends a kept native lead's own turn that its app holds open only for commands it started
/// in the background once the owner's rules say so, as `turn` ends an engine-typed one: at
/// the task limit counted from the turn's start, also through the notice turn after the
/// hold, after the grace time under Grace, at once under Stop, and never by itself under
/// Wait. Takes the own-turn state, the rules and whether the app holds the turn now.
/// Returns the turn's end and the line its result starts with, or none while it waits.
fn own_release(own: &mut OwnTurn, rules: TurnRules, held: bool) -> Option<(TurnEnd, &'static str)> {
    if rules
        .limit
        .is_some_and(|limit| own.since.is_some_and(|start| start.elapsed() >= limit))
    {
        return Some((TurnEnd::TimedOut, BACKGROUND_CUT));
    }
    if !held {
        own.held_since = None;
        return None;
    }
    let since = *own.held_since.get_or_insert_with(Instant::now);
    match rules.background {
        super::BackgroundAtEnd::Wait => None,
        super::BackgroundAtEnd::Grace { secs } => (since.elapsed()
            >= Duration::from_secs(u64::from(secs)))
        .then_some((TurnEnd::Success, BACKGROUND_GRACE_ENDED)),
        super::BackgroundAtEnd::Stop => Some((TurnEnd::Success, BACKGROUND_STOPPED)),
    }
}

/// Waits for a kept native lead's next order as `next_turn` does, and meanwhile reports
/// the turns its app starts by itself. An order that comes while such a turn runs is held
/// until that turn ends, so no prompt is typed into a running turn; a stop is not held. A
/// turn of its own that its app holds open for commands it started in the background ends
/// when `own_release` says so, and then the lead stops with those commands, as after an
/// engine-typed turn. Takes the orders, the cancel flag, the session, the reports channel,
/// the own-turn state and the rules, which the engine's latest rules order or typed turn
/// replaces at once. Returns the next order.
fn next_lead_turn(
    orders: &Receiver<Order>,
    cancel: &AtomicBool,
    session: &mut AgentSession,
    reports: &Sender<Report>,
    own: &mut OwnTurn,
    rules: &mut TurnRules,
) -> Next {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Next::Stop;
        }
        report_own(session, reports, own);
        let held = session.own_held();
        if own.running && (held || own.held_once) {
            own.held_once = true;
            if let Some((end, line)) = own_release(own, *rules, held) {
                own.running = false;
                let mut text = std::mem::take(&mut own.text);
                if session.background_running() > 0 {
                    text = format!("{line}\n{text}");
                }
                let _ = reports.send(Report::Own {
                    end: Some(end),
                    text,
                });
                return Next::Stop;
            }
        }
        if !own.running {
            if let Some(next) = own.held.take() {
                return next;
            }
            match orders.recv_timeout(ORDER_WAIT) {
                Ok(Order::Start(instructions, grant)) => return Next::Start(instructions, grant),
                Ok(Order::Turn(prompt, grant, rules)) => return Next::Turn(prompt, grant, rules),
                Ok(Order::Rules(next)) => *rules = next,
                Ok(Order::Stop) | Err(RecvTimeoutError::Disconnected) => return Next::Stop,
                Err(RecvTimeoutError::Timeout) => {}
            }
        } else {
            if own.held.is_none() {
                match orders.try_recv() {
                    Ok(Order::Start(instructions, grant)) => {
                        own.held = Some(Next::Start(instructions, grant));
                    }
                    Ok(Order::Turn(prompt, grant, typed)) => {
                        *rules = typed;
                        own.held = Some(Next::Turn(prompt, grant, typed));
                    }
                    Ok(Order::Rules(next)) => *rules = next,
                    Ok(Order::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Next::Stop;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            std::thread::sleep(ORDER_WAIT);
        }
        match session.next_event(IDLE_EVENT_WAIT) {
            Ok(Some(LaunchEvent::Exited { .. })) => return Next::Closed(None),
            Err(problem) => return Next::Closed(Some(problem.0)),
            Ok(_) => {}
        }
    }
}

/// Serves turns on an agent whose harness keeps running between them, the kept turn first
/// when one is given. A kept session, a native lead's, also reports the turns its app
/// starts by itself between orders, so its later work is recorded, landed and told to the
/// master. A turn the owner interrupted while commands its agent started in the background
/// still ran also ends the agent, so those commands stop with its process.
/// At the end it stops the agent, or, when the engine quits with the agent kept,
/// stops the running turn and leaves the pane running for the next engine. A kept
/// session's credential copy and custody use are released once its process stopped or was
/// left. A stop its warden did not confirm keeps both for the engine's life and ends the
/// agent with that problem; a Full access stop that left processes running keeps both
/// until they end, names them and ends the agent.
fn serve(
    mut session: AgentSession,
    kept: Option<Kept>,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let lead = kept.is_some();
    let (mut first, held) = kept.map_or((None, None), |kept| (kept.first, Some(kept.held)));
    let mut latest = first.as_ref().map_or(
        TurnRules {
            limit: None,
            background: super::BackgroundAtEnd::default(),
        },
        |(_, rules)| *rules,
    );
    if lead {
        session.follow_own_turns();
    }
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
        pane: session.pane_record(),
    });
    let mut problem = None;
    let mut named = false;
    let mut own = OwnTurn::default();
    loop {
        let next = match first.take() {
            Some((prompt, rules)) => Next::Turn(prompt, None, rules),
            None if lead => {
                next_lead_turn(orders, cancel, &mut session, reports, &mut own, &mut latest)
            }
            None => next_turn(orders, cancel, Some(&mut session)),
        };
        let (prompt, grant, rules) = match next {
            Next::Turn(prompt, grant, rules) => (prompt, grant, rules),
            Next::Start(..) => unreachable!("only a live master is started with its instructions"),
            Next::Stop => break,
            Next::Closed(cause) => {
                problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
                break;
            }
        };
        assert!(grant.is_none());
        latest = rules;
        let prompt = told(prompt, rules, session.reports_background());
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, rules);
        let reusable = matches!(
            end,
            TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
        ) && (end != TurnEnd::Interrupted || session.background_running() == 0)
            && !session.held();
        if end == TurnEnd::Closed {
            problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
        }
        if !named && let Some(conversation) = session.conversation() {
            named = reports.send(Report::Conversation(conversation)).is_ok();
        }
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            break;
        }
    }
    if problem.is_none() && leave.load(Ordering::SeqCst) {
        let _ = session.interrupt();
        session.leave();
    } else {
        return settle_end(session.stop(), held, problem, reports);
    }
    drop(held);
    problem
}

/// Serves turns on an agent of a signed-in account, starting its harness for each turn with
/// the account's credential copy and lease, both released once the turn's harness stops. A
/// stop its warden did not confirm keeps both for the engine's life and ends the agent; a
/// Full access stop that left processes running keeps both until they end, names them and
/// ends the agent.
/// A sub-agent's turn continues the conversation its previous turn recorded, or the one the
/// plan resumes for a woken agent, and a new conversation is reported so a later wake
/// continues it; a refused resume is reported and ends the agent as a kept one does. A
/// master's turn starts a fresh conversation, since its feed briefs it again each turn; only
/// the first turn of a run the engine resumes after an interruption continues its session.
fn per_turn(
    plan: &Plan,
    home: &Path,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
) -> Option<String> {
    let mut resume = plan.resume.clone();
    while let Next::Turn(prompt, grant, rules) = next_turn(orders, cancel, None) {
        let Some(grant) = grant else {
            let _ = reports.send(Report::Finished {
                text: String::new(),
                end: TurnEnd::Closed,
                cause: None,
            });
            return Some("The agent was given a turn without its signed-in account".into());
        };
        let Grant {
            profile,
            context,
            access,
        } = *grant;
        let request = match plan.request(
            AccountBinding::Subscription {
                home: home.to_path_buf(),
            },
            access,
        ) {
            Ok(request) => request,
            Err(problem) => {
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.clone()),
                });
                return Some(problem);
            }
        };
        let process = rules.limit.map_or(plan.limits.run_time, |limit| {
            limit.min(plan.limits.run_time)
        });
        let credential = match profile.hand_to_agent(home, process, &plan.executable) {
            Ok(credential) => credential,
            Err(problem) => {
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.0.clone()),
                });
                return Some(with_code(
                    "The signed-in account could not reach the agent",
                    Some(&problem.0),
                ));
            }
        };
        let started = AgentSession::start_resuming(
            request,
            plan.team_context(context),
            plan.pane.clone(),
            resume.as_ref(),
        );
        let mut session = match started {
            Ok(session) => session,
            Err(problem) => {
                if resume.is_some() && problem.0 == RESUME_REFUSED {
                    let _ = reports.send(Report::ResumeRefused);
                }
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.0.clone()),
                });
                return Some(start_problem(&problem));
            }
        };
        if !plan.continues {
            resume = None;
        }
        let _ = reports.send(Report::Ready {
            group: session.process_group(),
            pane: session.pane_record(),
        });
        let prompt = told(prompt, rules, true);
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, rules);
        if let Some(recorded) = session
            .conversation()
            .filter(|c| plan.continues && resume.as_ref() != Some(c))
        {
            let _ = reports.send(Report::Conversation(recorded.clone()));
            resume = Some(recorded);
        }
        let stopped = session.stop();
        let reusable = stopped.is_ok()
            && matches!(
                end,
                TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
            );
        let closed = settle_end(
            stopped,
            Some((credential, profile)),
            (end == TurnEnd::Closed).then(|| with_code(PROCESS_CLOSED, cause.as_deref())),
            reports,
        );
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            return closed;
        }
    }
    None
}

const PROCESS_CLOSED: &str = "The agent process closed";
/// What a task the owner's interrupt ends says when commands its agent started still ran.
const BACKGROUND_INTERRUPTED: &str = "The owner's interrupt ended this task while a command it started in the background still ran; that command was stopped with it.";
/// What a task cut by a time limit says when commands its agent started still ran.
const BACKGROUND_CUT: &str = "The time limit ended this task while a command it started in the background still ran; that command was stopped with it.";
/// What a task the owner's background grace time ended says (DEC-111 Q13).
const BACKGROUND_GRACE_ENDED: &str = "The background grace time in Team settings ended this task while a command it started in the background still ran; that command was stopped.";
/// What a task that ended with its turn under the owner's Stop setting says (DEC-111 Q13).
const BACKGROUND_STOPPED: &str = "This task ended with its turn, as Team settings say, while a command it started in the background still ran; that command was stopped.";
/// The task prompt line for agents whose background commands stop at task end, only then.
const BACKGROUND_STOPS_LINE: &str =
    "Commands you start in the background stop when your turn ends.";
/// The task line a kept native lead's own turn is recorded under: a turn its app started
/// by itself, such as after messages from its teammates, with no task given to it.
const OWN_TURN_TASK: &str =
    "The lead's own turn, after messages from its teammates; no task was given to it.";
const OWN_TURN_FAILED: &str = "The lead's own turn did not finish";
const OWNER_STOPPED_TASK: &str =
    "The owner stopped this sub-agent, so it was not started again. Only the owner can wake it.";
const MASTER_STOPPED_TASK: &str = "The master stopped this sub-agent before it finished this task.";
const LEAD_STOPPED_TASK: &str = "Its lead stopped this sub-agent before it finished this task.";
const INSTRUCTIONS_WAIT_FOR_ROOM: &str = "The message just typed into the live master left no room for your instruction change, so a later message carries it; it applies in full when the master starts again.";
/// The most teammate rows one harness keeps: its running teammates and the newest ended ones.
const TEAMMATE_ROWS_KEPT: i64 = 32;
/// The most times the engine runs git to make one native teammate's worktree (OP-9), so a
/// worktree git refuses never costs a git run on every pass.
const TEAMMATE_WORKTREE_TRIES: u8 = 3;
/// The most passes the engine tries to commit one resting native teammate's turn (OP9B-L1)
/// before it tells the lead and waits for that teammate's next rest.
const TEAMMATE_TURN_TRIES: u8 = 3;
const TEAMMATE_CONFLICT_FILES_SHOWN_MAX: usize = 400;
/// The most teammate names of one lead whose worktree tries the engine counts.
const TEAMMATE_WORKTREE_NAMES_MAX: usize = 64;
const _: () = assert!(TEAMMATE_WORKTREE_NAMES_MAX >= TEAMMATE_ROWS_KEPT as usize);
const _: () = assert!(TEAMMATE_ROWS_KEPT as usize >= super::TEAMMATES_SHOWN_MAX);
/// The most owner-stopped native teammates one tick ends, so a long list never stalls it.
const UNMANAGED_STOPS_PER_TICK_MAX: usize = 16;

/// One native teammate's end that ran on a thread of its own: the agent and the host's
/// answer, whether its pane was proven gone, or the host's failure.
type PaneEnd = (i64, Result<bool, crate::terminal::HostError>);

/// Who learns of a native teammate's end once its row is marked: the owner's side stop, by
/// its project and maker, or the lead whose cap it was over, by project, lead and cap.
enum Told {
    Stop(ProjectId, StoppedBy),
    Lead {
        project: ProjectId,
        lead: i64,
        cap: u8,
    },
}

/// What a native teammate whose end runs on a thread gets once it finished: the reason its
/// row is marked with, and who is told.
struct PendingEnd {
    reason: &'static str,
    told: Told,
}

/// How one native teammate's end was taken: its row marked stopped now, its end handed to a
/// thread whose answer a later tick marks, or neither, as for a row already stopped
/// elsewhere or a thread that did not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnmanagedEnd {
    Marked,
    Handed,
    Untouched,
}

/// The note a lead gets for its newest teammates the engine ended over the owner's Max
/// teammates per harness. Takes the cap and the ended teammates' labels. Returns the note.
fn over_cap_note(cap: u8, ended: &[String]) -> String {
    format!(
        "The owner allows at most {cap} sub-agents per harness, so the engine ended your newest teammates: {}. Keep at most {cap}.",
        ended.join(", ")
    )
}

/// Runs one native teammate's end on its thread and sends what it found, also when the end
/// panics: a guard then sends it as not proven, so its row is still marked, with the
/// uncertain cleanup code, and no agent stays handed over (S4B-B1-L1 r1). Takes the agent,
/// the channel and the end. Returns nothing.
fn end_reported(
    id: i64,
    sender: Sender<PaneEnd>,
    end: impl FnOnce() -> Result<bool, crate::terminal::HostError>,
) {
    let mut guard = EndGuard {
        id,
        sender: Some(sender),
    };
    let outcome = end();
    if let Some(sender) = guard.sender.take() {
        let _ = sender.send((id, outcome));
    }
}

/// Sends a native teammate's end as not proven when its thread leaves before it sent a
/// result, as on a panic.
struct EndGuard {
    id: i64,
    sender: Option<Sender<PaneEnd>>,
}

impl Drop for EndGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send((self.id, Ok(false)));
        }
    }
}
const OWNER_STOPPED_TEAMMATE: &str = "The owner stopped this teammate.";
const MASTER_STOPPED_TEAMMATE: &str = "The master stopped this teammate.";
const LEAD_STOPPED_TEAMMATE: &str = "Its lead stopped this teammate.";
/// The event text for a sub-agent's stop. Takes who made it. Returns the text.
fn stop_notice(by: StoppedBy) -> &'static str {
    match by {
        StoppedBy::Lead => LEAD_STOPPED_AGENT,
        StoppedBy::Master => MASTER_STOPPED_AGENT,
        StoppedBy::Owner => OWNER_STOPPED_AGENT,
    }
}

/// The reason recorded on a native teammate a stop ended. Takes who made it. Returns it.
fn stopped_teammate(by: StoppedBy) -> &'static str {
    match by {
        StoppedBy::Lead => LEAD_STOPPED_TEAMMATE,
        StoppedBy::Master => MASTER_STOPPED_TEAMMATE,
        StoppedBy::Owner => OWNER_STOPPED_TEAMMATE,
    }
}
const LEAD_ENDED_TEAMMATE: &str = "Its lead stopped, and its terminal ended with the lead's.";
const OVER_TEAMMATE_CAP: &str = "The engine ended it: its lead ran more sub-agents than the owner's Max teammates per harness allows.";
const OWNER_WAKES_PER_TICK_MAX: usize = 8;
/// How often one project's claims are swept after the first step: often enough that a
/// stopped agent's finished claims free up within a minute, rarely enough that the sweep's
/// reads stay off the team step.
const CLAIMS_SWEEP_EVERY: Duration = Duration::from_secs(30);
/// The owner's note the first time a project's claims fill one overlap check.
const CLAIMS_FULL_NOTE: &str = "This project holds more open file claims than one overlap check reads, so agents cannot start new claimed work and their edits of claimed files are held. Accept or discard the change sets waiting in Changes, or release work no agent is on.";
const HANDOFF_BYTES_MAX: usize = 1000;

/// The owner's note of two agents whose waits under Wait would have closed a cycle (OS-8).
/// Inputs: the labels of the agent whose start went ahead and of the one holding the file,
/// and the file. Output: the note.
fn wait_cycle_note(starter: &str, holder: &str, file: &str) -> String {
    format!(
        "{starter} and {holder} would have waited on each other under Overlapping edits Wait, so {starter}'s start went ahead without {file}: {holder} keeps editing it, and {starter}'s edits of it stay refused until {holder}'s work on it lands or is discarded."
    )
}

/// Lists file names for an engine note one per line, each quoted with its quotes,
/// backslashes, newlines and other control characters escaped, so no name an agent chose
/// reads as a line of the note (OP9B-C2). Takes the names and the byte bound of the list.
/// Returns the lines, whole names only, then how many names were left out.
fn quoted_lines(names: &[String], max: usize) -> String {
    let mut text = String::new();
    for (shown, name) in names.iter().enumerate() {
        let line = format!("\n- \"{}\"", name.escape_debug());
        if text.len() + line.len() > max {
            text.push_str(&format!("\n- and {} more", names.len() - shown));
            break;
        }
        text.push_str(&line);
    }
    text
}

/// Keeps the harness conversation a sub-agent's pane records, replacing the one kept
/// before, so waking the agent continues it (DEC-102). A record path that is not plain text
/// or too long, or an id out of bounds, is not kept. Takes the database, the agent, its
/// project, launch harness and account, and the conversation. Returns any SQLite failure.
fn keep_agent_conversation(
    connection: &Connection,
    agent: i64,
    project: &ProjectId,
    harness: &str,
    account: &CredentialRef,
    resume: &Resume,
) -> Result<(), EngineError> {
    let Some(record) = resume
        .record
        .to_str()
        .filter(|record| !record.is_empty() && record.len() <= 4096)
    else {
        return Ok(());
    };
    if resume.id.is_empty() || resume.id.len() > 128 {
        return Ok(());
    }
    connection
        .execute(
            "INSERT OR REPLACE INTO team_agent_conversations(agent,project,harness,connection,conversation,record)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                agent,
                project.as_str(),
                harness,
                account.as_str(),
                resume.id,
                record
            ],
        )
        .map_err(error)?;
    Ok(())
}

/// The newest result a sub-agent reported for a task, on one line and bounded, the handoff a
/// woken agent gets when its harness conversation cannot continue. Takes the database and
/// the agent. Returns the result, or none before its first.
fn last_result(connection: &Connection, agent: i64) -> Result<Option<String>, EngineError> {
    let result: Option<String> = connection
        .query_row(
            "SELECT result FROM team_tasks WHERE agent=?1 AND result IS NOT NULL AND trim(result)<>''
             ORDER BY COALESCE(finished_ms,0) DESC, id DESC LIMIT 1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(result
        .map(|result| super::events::one_line(&result, HANDOFF_BYTES_MAX))
        .filter(|result| !result.is_empty()))
}

const LIVE_REPLY_BYTES_MAX: usize = 64 * 1024;
const NOTE_PROMPT_BYTES_MAX: usize = 80;
const NOTE_RESULT_BYTES_MAX: usize = 300;
const NOTE_PROBLEM_BYTES_MAX: usize = 200;
const NOTE_PLACE_BYTES_MAX: usize = 120;
const NOTES_PER_TICK_MAX: i64 = 16;

/// One task a conversation master handed off that has ended, as the note sweep reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EndedTask {
    id: i64,
    harness: String,
    status: TaskStatus,
    prompt: String,
    result: Option<String>,
    problem: Option<String>,
    agent: Option<i64>,
    worktree: Option<String>,
}

/// Words the engine's note on one conversation task that ended: the task and its prompt,
/// its harness and sub-agent, how it ended and where its full result is, then a short
/// excerpt of what it said. Takes the ended task. Returns at most `NOTE_BYTES_MAX` bytes.
fn conversation_note(ended: &EndedTask) -> String {
    let harness = excerpt(&ended.harness, 64);
    let who = match ended.agent {
        Some(agent) => format!("{harness}, sub-agent {agent}"),
        None => harness,
    };
    let head = format!(
        "Task {} \"{}\" ({who})",
        ended.id,
        excerpt(&ended.prompt, NOTE_PROMPT_BYTES_MAX)
    );
    let problem = excerpt(
        ended.problem.as_deref().unwrap_or("no reason was recorded"),
        NOTE_PROBLEM_BYTES_MAX,
    );
    let note = match ended.status {
        TaskStatus::Done => {
            let mut note = format!("{head} is done. Full result in the Team view");
            if let Some(place) = &ended.worktree {
                note.push_str(&format!(
                    ", files in {}",
                    excerpt(place, NOTE_PLACE_BYTES_MAX)
                ));
            }
            note.push('.');
            if let Some(result) = ended.result.as_deref().filter(|r| !r.trim().is_empty()) {
                note.push_str(" It said: ");
                note.push_str(&excerpt(result, NOTE_RESULT_BYTES_MAX));
            }
            note
        }
        TaskStatus::Failed => format!(
            "{} Details in the Team view.",
            full_stop(format!("{head} failed: {problem}"))
        ),
        _ => format!(
            "{} Details in the Team view.",
            full_stop(format!("{head} was stopped: {problem}"))
        ),
    };
    assert!(note.len() <= NOTE_BYTES_MAX);
    note
}

type ReadyTask = (i64, String, String, Option<String>, Option<String>);
/// One queued task as dispatch reads it: id, harness, prompt, the agent it is pinned to,
/// whether the master handed it off, the lead whose worker runs it and the sub-agent mode
/// the master asked for the lead that takes it.
type QueuedTask = (
    i64,
    String,
    String,
    Option<i64>,
    bool,
    Option<i64>,
    Option<String>,
);

/// Writes one results message for the finished master tasks that fit, oldest first: each
/// task's line, then its problem and its result on lines of their own, every agent-written
/// line quoted by `events::agent_text` so none can pose as the owner's. Takes the ready
/// tasks, at least one.
/// Returns the text, at most `RESULTS_MESSAGE_BYTES_MAX` bytes, and how many tasks it holds.
fn results_message(ready: &[ReadyTask]) -> (String, usize) {
    results_within(ready, RESULTS_MESSAGE_BYTES_MAX, RESULTS_TRAILER)
}

/// Writes one results message as `results_message` does, within `max` bytes and closed by
/// `trailer`, each problem and result cut to a quarter of `max` at most, so a lead's
/// message fits what one terminal prompt takes. Takes the ready tasks, at least one, the
/// bound and the closing line. Returns the text and how many tasks it holds.
fn results_within(ready: &[ReadyTask], max: usize, trailer: &str) -> (String, usize) {
    assert!(!ready.is_empty());
    let each = RESULT_IN_MESSAGE_BYTES_MAX.min(max / 4);
    let mut text = String::from("Results from your sub-agents:\n");
    let mut held = 0;
    for (id, harness, status, result, problem) in ready {
        let label = super::TaskStatus::parse(status).map_or("Finished", TaskStatus::label);
        let mut part = format!("\nTask {id} ({}): {label}\n", excerpt(harness, 64));
        if let Some(problem) = problem {
            part.push_str(&bound_text(
                &super::events::agent_text(problem.trim()),
                each,
            ));
            part.push('\n');
        }
        if let Some(result) = result {
            part.push_str(&bound_text(&super::events::agent_text(result.trim()), each));
            part.push('\n');
        }
        if held > 0 && text.len() + part.len() + trailer.len() > max {
            break;
        }
        text.push_str(&part);
        held += 1;
    }
    text.push_str(trailer);
    assert!((1..=ready.len()).contains(&held));
    assert!(text.len() <= max);
    (text, held)
}

/// Writes what a live master is typed for one pending message: the owner's direct prompt
/// notes when they fit beside the whole message, then the message under its speaker tag.
/// Notes that do not fit wait for the next message instead of cutting the message's tail.
/// Takes the notes, the message author and its text. Returns the text and whether the notes
/// went with it.
fn live_input(heard: &str, author: &str, text: &str) -> (String, bool) {
    let speaker = if author == "owner" {
        "[owner] "
    } else if text.starts_with("[team] ") {
        ""
    } else {
        "[team] "
    };
    let message = format!("{speaker}{text}");
    if heard.len() + message.len() <= super::master::INPUT_TEXT_MAX {
        return (format!("{heard}{message}"), true);
    }
    (
        bound_text(&message, super::master::INPUT_TEXT_MAX),
        heard.is_empty(),
    )
}

/// Flattens every whitespace run to one space and cuts the text to `max` bytes, ending a
/// cut text with "...". Takes the text and the bound. Returns at most `max` bytes.
fn excerpt(text: &str, max: usize) -> String {
    assert!(max > 3);
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        return flat;
    }
    let cut = format!("{}...", bound_text(&flat, max - 3).trim_end());
    assert!(cut.len() <= max);
    cut
}
const RESUME_REFUSED: &str = "launch_resume_unavailable";
const LIVE_STARTS_MAX: usize = 3;
const GIT_POINTER_BYTES_MAX: u64 = 4096;
const LIVE_STARTS_WINDOW: Duration = Duration::from_secs(10 * 60);
const LIVE_STOPPED: &str = super::master::LIVE_STOPPED;
const WARM_STOPPED: &str = "Keeping sub-agents warm was turned off because a warm sub-agent stopped before it did any work";

/// Says which of the owner's time limits cut a turn and where to change it. Inputs: the
/// turn's end code and the limit that bounded the turn. Output: plain words, such as
/// "stopped by the 1 h task limit; change it in Team settings".
fn limit_note(cause: Option<&str>, bound: Option<TurnBound>) -> String {
    let (kind, limit) = match (cause, bound) {
        (Some(TURN_LIMIT), Some(TurnBound::Task(limit))) => ("task", limit),
        (Some(TURN_LIMIT), Some(TurnBound::Session(limit))) => ("session", limit),
        (Some("launch_deadline"), _) => {
            return format!(
                "stopped after {}, the longest one agent process may run",
                super::span_words(RUN_TIME_MAX.as_secs())
            );
        }
        _ => return TIME_LIMIT_NOTE.to_owned(),
    };
    format!(
        "stopped by the {} {kind} limit; change it in Team settings",
        super::span_words(limit.as_secs())
    )
}

/// The deadline of an agent's next turn: the owner's task limit, or what is left of the
/// agent's session limit when that is sooner. The session counts from the agent's start for
/// agents kept between turns and agents started for each turn alike. Inputs: the owner's
/// limits and how long the agent has run. Output: none for no limit, or how long the turn
/// may take and the limit behind it.
fn turn_bound(limits: super::TimeLimits, ran: Duration) -> Option<(Duration, TurnBound)> {
    let task = limits
        .turn_time()
        .map(|limit| (limit, TurnBound::Task(limit)));
    let session = limits
        .session_time()
        .map(|limit| (limit.saturating_sub(ran), TurnBound::Session(limit)));
    match (task, session) {
        (Some(task), Some(session)) => Some(if session.0 < task.0 { session } else { task }),
        (task, session) => task.or(session),
    }
}

/// The owner's line for an agent whose harness could not start: a command that would hold
/// an MCP secret names its server and secret, any other refusal names its plain code.
/// Input: the launch error. Output: the line.
fn start_problem(problem: &LaunchError) -> String {
    match crate::launch_context::secret_in_command_text(problem) {
        Some(sentence) => format!("The agent could not start. {sentence}"),
        None => with_code("The agent could not start", Some(&problem.0)),
    }
}

/// An owner line that names the plain code behind it in the style every window uses, so a
/// closed or failed agent is never opaque. Inputs: the sentence and the code, if any.
/// Output: "Sentence. (code)" with the code made plain by `plain_code`, or the sentence alone.
fn with_code(sentence: &str, cause: Option<&str>) -> String {
    match cause {
        Some(code) => {
            let stop = if sentence.ends_with('.') { "" } else { "." };
            format!("{sentence}{stop} ({})", crate::protocol::plain_code(code))
        }
        None => sentence.to_owned(),
    }
}

/// Ends an owner line with one full stop, leaving a line that already ends with a sentence
/// stop or a named code as it is. Input: the line. Output: the finished line.
fn full_stop(line: String) -> String {
    if line.ends_with('.') || line.ends_with(')') {
        line
    } else {
        line + "."
    }
}

/// The owner's git identity for an agent's own commits in a project: the project
/// repository's `user.name` and `user.email` as the owner's git reads them, with the
/// owner's home from the engine's `HOME`. Input: the project root. Output: the
/// identity, or None when the owner set none, so git keeps its own default.
fn owner_git_identity(root: &Path) -> Option<GitIdentity> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let (name, email) = crate::landing::owner_git_identity(root, &home).ok()?;
    GitIdentity::new(name, email).ok()
}

/// Tells the git layout of a worktree from the git folder the engine recorded for it: an
/// agent's own clone keeps its git folder at `agent_git::git_dir`, a linked worktree's sits
/// in the project's `.git/worktrees`. Inputs: the recorded worktree and git folder. Output:
/// the layout.
fn recorded_layout(worktree: &str, git_dir: &str) -> Layout {
    if Path::new(git_dir) == crate::agent_git::git_dir(Path::new(worktree)) {
        Layout::Clone
    } else {
        Layout::Linked
    }
}

/// The agent's own clone a launch made and has not recorded yet (GA-R2). Dropped before
/// `keep`, it removes the clone, so a launch that fails after making it leaves no checkout
/// that no agent row or cleanup would reach. Holds the folder, or none when the launch made
/// no clone.
struct NewClone(Option<PathBuf>);

impl NewClone {
    /// Keeps the clone once the launch no longer fails before recording it.
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for NewClone {
    fn drop(&mut self) {
        if let Some(folder) = self.0.take() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// Reads where Git keeps a new agent worktree's metadata while no agent has run in it yet,
/// so the engine's later commits never rely on what the agent may write: for a linked
/// worktree a folder directly in the project's `.git/worktrees` whose `gitdir` file names
/// this worktree, both pointer files read through one open that follows no link and never
/// waits on a FIFO, kept only when regular and within `GIT_POINTER_BYTES_MAX` bytes, since
/// an agent with Full access can replace them; for an agent's own clone the real folder
/// `agent_git::git_dir` names. Inputs: the project root, the worktree and its layout.
/// Output: the root, worktree and metadata folder as text, or the plain reason.
fn agent_git_dir(root: &Path, worktree: &Path, layout: Layout) -> Result<[String; 3], String> {
    if layout == Layout::Clone {
        let git_dir = crate::agent_git::git_dir(worktree);
        let plain = std::fs::symlink_metadata(&git_dir).is_ok_and(|found| found.is_dir());
        let text = |path: &Path| path.to_str().map(str::to_owned);
        let found = [text(root), text(worktree), text(&git_dir)];
        return match found {
            [Some(root), Some(worktree), Some(git_dir)] if plain => Ok([root, worktree, git_dir]),
            _ => Err(with_code(
                "the agent worktree could not be checked",
                Some("team_worktree_git_dir"),
            )),
        };
    }
    let refused = || {
        with_code(
            "the agent worktree could not be checked",
            Some("team_worktree_git_dir"),
        )
    };
    let read = |path: &Path| {
        crate::platform::read_plain_file(path, GIT_POINTER_BYTES_MAX).ok_or_else(refused)
    };
    let pointer = read(&worktree.join(".git"))?;
    let git_dir = pointer
        .strip_prefix("gitdir: ")
        .and_then(|path| path.strip_suffix('\n'))
        .map(PathBuf::from)
        .ok_or_else(refused)?;
    if git_dir.parent() != Some(root.join(".git").join("worktrees").as_path())
        || read(&git_dir.join("gitdir"))? != format!("{}\n", worktree.join(".git").display())
    {
        return Err(refused());
    }
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(refused);
    let found = [text(root)?, text(worktree)?, text(&git_dir)?];
    assert!(found[2].starts_with(&found[0]) && found[1].starts_with(&found[0]));
    Ok(found)
}

/// The plain code for a failed file step, named by its kind so an owner line never quotes
/// the system's error text or a path. Input: the I/O error. Output: the code.
fn io_code(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "io_not_found",
        ErrorKind::PermissionDenied => "io_permission_denied",
        ErrorKind::AlreadyExists => "io_already_exists",
        ErrorKind::NotADirectory => "io_not_a_directory",
        ErrorKind::IsADirectory => "io_is_a_directory",
        ErrorKind::ReadOnlyFilesystem => "io_read_only",
        ErrorKind::StorageFull => "io_storage_full",
        ErrorKind::TimedOut => "io_timed_out",
        ErrorKind::Interrupted => "io_interrupted",
        ErrorKind::InvalidInput | ErrorKind::InvalidData => "io_invalid",
        _ => "io_failed",
    }
}

/// Lists the hosts a team agent under the None network reaches through its model proxy
/// (S4b): its harness's model hosts on a signed-in account, its base URL's host on an
/// endpoint. Inputs: the launch harness and the account's reach. Output: the hosts, or the
/// plain refusal for an endpoint no proxy can reach by name or a harness with no list.
fn model_hosts(launch: &str, reach: &TeamReach) -> Result<Vec<ModelHost>, String> {
    let fixed = |hosts: &[(&str, u16)]| -> Vec<ModelHost> {
        hosts
            .iter()
            .filter_map(|(name, port)| ModelHost::new(name, *port))
            .collect()
    };
    let hosts = match (reach, launch) {
        (TeamReach::Endpoint { base_url, .. }, _) => match endpoint_model_host(base_url) {
            Some(host) => vec![host],
            None => {
                return Err(with_code(
                    "With no network, an agent with tools reaches only an https endpoint named by a host name. Use such an endpoint or choose another network setting.",
                    Some("network_none_endpoint_unnamed"),
                ));
            }
        },
        (TeamReach::Managed { .. }, "codex") => fixed(CODEX_MODEL_HOSTS),
        (TeamReach::Managed { .. }, "claude-code") => fixed(CLAUDE_MODEL_HOSTS),
        (TeamReach::Managed { .. }, _) => {
            return Err(with_code(
                "this harness has no model hosts for the no network setting",
                Some("network_none_harness_unlisted"),
            ));
        }
    };
    assert!(!hosts.is_empty() && hosts.len() <= crate::sandbox::model_proxy::HOSTS_MAX);
    Ok(hosts)
}

/// The plain code for a failed agent sandbox step. Input: the error. Output: the code.
fn sandbox_code(error: &SandboxError) -> &'static str {
    match error {
        SandboxError::Invalid(_) => "sandbox_refused",
        SandboxError::Unavailable(_) => "sandbox_unavailable",
        SandboxError::Io(error) => io_code(error),
    }
}

/// The plain code for a failed repository, worktree or endpoint step.
/// Input: the error. Output: the code; engine codes pass through `plain_code`.
fn run_code(error: &RunError) -> &str {
    match error {
        RunError::Sandbox(error) => sandbox_code(error),
        RunError::Io(error) => io_code(error),
        RunError::Context(error) => crate::protocol::plain_code(&error.0),
        RunError::Account(code) => crate::protocol::plain_code(code),
        RunError::Invalid(_) => "setup_refused",
        RunError::Jobs(_) => "research_jobs_failed",
        RunError::Busy => "account_busy",
        RunError::Uncertain(code) => code,
        RunError::Setup(code) => code,
    }
}

/// The prompt one turn is typed with: the task's own, and under Stop the line telling the
/// agent its background commands stop when its turn ends, only where the engine stops them:
/// a session that ends with the turn, or a pane app whose record reports them. A kept
/// session reporting none, such as a Codex app's, keeps them running, so its agent is not
/// told so (rv-audit S3 F2). Takes the prompt, the turn's rules and whether the engine stops
/// them. Returns the prompt.
fn told(prompt: String, rules: TurnRules, stops: bool) -> String {
    if rules.background == super::BackgroundAtEnd::Stop && stops {
        format!("{prompt}\n{BACKGROUND_STOPS_LINE}")
    } else {
        prompt
    }
}

/// Puts an owner's instruction edit at the front of a turn's prompt, cut to what the turn's
/// limit leaves; with no room the edit stays out and a later turn carries it. Inputs: the
/// prompt, the edit and the limit in bytes. Output: the edit that went in, or none.
fn edit_in_front(
    prompt: &mut String,
    edit: Option<crate::instruction_files::Edit>,
    limit: usize,
) -> Option<crate::instruction_files::Edit> {
    assert!(limit > 0);
    let edit = edit?;
    let block = edit.message_within(limit.saturating_sub(prompt.len()))?;
    prompt.insert_str(0, &block);
    assert!(prompt.len() <= limit);
    Some(edit)
}

/// How a turn ends on a launch error: the deadline and the output limits keep their own
/// ends; any other error closes the agent and keeps its code, as does the deadline, so a
/// cut turn can name the session limit. Input: the error. Output: the end and its code.
fn launch_end(problem: LaunchError) -> (TurnEnd, Option<String>) {
    match problem.0.as_str() {
        "launch_deadline" => (TurnEnd::TimedOut, Some(problem.0)),
        "launch_output_limit" | "launch_event_limit" => (TurnEnd::TooLong, None),
        _ => (TurnEnd::Closed, Some(problem.0)),
    }
}

/// Runs one engine-typed turn on the agent's session until it ends: by its app, the cancel
/// flag, the owner's interrupt, its time limit, or, while the app's turn is held open only
/// for commands it started in the background, the owner's Background commands at task end
/// choice: at once under Stop, after the grace time under Grace (DEC-111 Q13). A turn ended
/// for its background commands is still held, so its caller stops the agent and them with
/// it. Each change between held and running is reported, so a message can be typed into a
/// held agent while it shows working on its task (DEC-119). Takes the session, the prompt, the reports
/// channel, the cancel and interrupt flags and the turn's rules. Returns the turn's text,
/// its end and the end's cause.
fn turn(
    session: &mut AgentSession,
    prompt: &str,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    rules: TurnRules,
) -> (String, TurnEnd, Option<String>) {
    interrupt.store(false, Ordering::SeqCst);
    if let Err(problem) = session.send_or_cancel(prompt, cancel) {
        if problem.0 == "launch_cancelled" {
            return (String::new(), TurnEnd::Cancelled, None);
        }
        return (String::new(), TurnEnd::Closed, Some(problem.0));
    }
    let deadline = rules.limit.map(|limit| Instant::now() + limit);
    let mut text = String::new();
    let mut limited = false;
    let mut interrupted = false;
    let mut progress = Instant::now();
    let mut cause = None;
    let mut held_since = None;
    let mut released = None;
    let mut shown_held = false;
    let end = loop {
        if cancel.load(Ordering::SeqCst) {
            break TurnEnd::Cancelled;
        }
        if interrupt.swap(false, Ordering::SeqCst) {
            let _ = session.interrupt();
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            cause = Some(TURN_LIMIT.to_owned());
            break TurnEnd::TimedOut;
        }
        let held = session.held();
        if held != shown_held {
            shown_held = held;
            let _ = reports.send(Report::Held { held });
        }
        if held {
            let since = *held_since.get_or_insert_with(Instant::now);
            released = match rules.background {
                super::BackgroundAtEnd::Wait => None,
                super::BackgroundAtEnd::Grace { secs } => (since.elapsed()
                    >= Duration::from_secs(u64::from(secs)))
                .then_some(BACKGROUND_GRACE_ENDED),
                super::BackgroundAtEnd::Stop => Some(BACKGROUND_STOPPED),
            };
            if released.is_some() {
                break TurnEnd::Success;
            }
        } else {
            held_since = None;
        }
        if progress.elapsed() >= PROGRESS_EVERY {
            progress = Instant::now();
            let _ = reports.send(Report::Progress { bytes: text.len() });
        }
        match session.next_event(EVENT_WAIT) {
            Ok(None) => {}
            Ok(Some(LaunchEvent::Text { text: part })) => {
                if text.len() + part.len() > super::RESULT_BYTES_MAX * 4 {
                    break TurnEnd::TooLong;
                }
                text.push_str(&part);
            }
            Ok(Some(LaunchEvent::TurnError { rate_limited })) => limited |= rate_limited,
            Ok(Some(LaunchEvent::Usage { tokens })) => {
                let _ = reports.send(Report::Usage { tokens });
            }
            Ok(Some(LaunchEvent::Interrupted)) => interrupted = true,
            Ok(Some(LaunchEvent::TurnFinished { success })) => {
                break if success {
                    TurnEnd::Success
                } else if interrupted {
                    TurnEnd::Interrupted
                } else if limited {
                    TurnEnd::RateLimited
                } else {
                    TurnEnd::Failed
                };
            }
            Ok(Some(LaunchEvent::Exited { .. })) => break TurnEnd::Closed,
            Err(problem) => {
                let (end, code) = launch_end(problem);
                cause = code;
                break end;
            }
            Ok(Some(_)) => {}
        }
    };
    assert!(released.is_none() || (end == TurnEnd::Success && session.held()));
    if session.background_running() > 0 {
        match (end, released) {
            (TurnEnd::TimedOut, _) => text = format!("{BACKGROUND_CUT}\n{text}"),
            (TurnEnd::Interrupted, _) => text = format!("{BACKGROUND_INTERRUPTED}\n{text}"),
            (TurnEnd::Success, Some(line)) => text = format!("{line}\n{text}"),
            _ => {}
        }
    }
    assert!(
        text.len()
            <= super::RESULT_BYTES_MAX * 4
                + BACKGROUND_CUT
                    .len()
                    .max(BACKGROUND_INTERRUPTED.len())
                    .max(BACKGROUND_GRACE_ENDED.len())
                    .max(BACKGROUND_STOPPED.len())
                + 1
    );
    (text, end, cause)
}

/// Background loop that keeps every project's team moving.
pub struct TeamService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TeamService {
    /// Starts the loop on its own thread, ticking at `tick`.
    /// Takes an opened runtime.
    /// Returns the running service; stopping it stops every agent process group.
    pub fn start(mut runtime: TeamRuntime, tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("team-runtime".into())
            .spawn(move || {
                let mut last = None;
                while !flag.load(Ordering::SeqCst) {
                    let problem = runtime.tick().err().map(|e| e.0);
                    if problem.is_some() && problem != last {
                        eprintln!(
                            "team runtime: {}",
                            bound_text(problem.as_deref().unwrap_or_default(), 300)
                        );
                    }
                    last = problem;
                    std::thread::sleep(tick);
                }
                if let Err(problem) = runtime.shutdown() {
                    eprintln!("team runtime shutdown: {}", bound_text(&problem.0, 300));
                }
            })
            .ok();
        Self { stop, thread }
    }

    /// Stops the loop and waits for its thread. Agents in panes keep running for the next
    /// engine; every other agent process group stops.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TeamService {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Names who made a team call in a sub-agent's mailbox (DEC-104). A lead stored as leading
/// native teammates shares that mailbox with them, so the pane the engine named the call
/// by from the caller's process decides (DEC-111 Q18): the lead's own pane, found under the
/// lead's own pane process, is the lead, the pane of one of its live teammate rows, under
/// the pane process that row was first named by, is that teammate, and anything else, no
/// pane included, is nobody (PLC). Every other mailbox names its own agent. Inputs: the database, the terminal host, the project, the
/// mailbox's agent with its label and the named pane with the pane process it was found
/// under. Output: the caller's id and label, or none, also when the lead's mode is unread.
pub fn team_caller(
    connection: &Connection,
    host: Option<&dyn Host>,
    project: &ProjectId,
    (lead, label): (i64, &str),
    pane: Option<(&str, u32)>,
) -> Option<(i64, String)> {
    let mode: Option<String> = connection
        .query_row(
            "SELECT mode FROM team_agents WHERE id=?1 AND project=?2",
            params![lead, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .ok()?
        .flatten();
    if mode.as_deref() != Some(crate::coord::settings::SubagentMode::Native.as_str()) {
        return Some((lead, label.to_owned()));
    }
    match pane_member(connection, host, project, lead, pane?)? {
        PaneMember::Lead => Some((lead, label.to_owned())),
        PaneMember::Teammate(teammate) => {
            let named = super::events::label(connection, teammate).ok()?;
            Some((teammate, named))
        }
    }
}

/// Who runs in one pane of a native lead's team by the engine's own rows.
enum PaneMember {
    Lead,
    Teammate(i64),
}

/// Finds who runs in one pane of a native lead's team from the engine's own rows, never
/// from a file the agents write (DEC-111 Q18): the lead when the caller was found under
/// the process its pane record names and the host lists that pane under it (PLC), or the
/// newest live teammate row of the lead in that pane while the caller was found under the
/// pane process that row is pinned to (PLC (b)). Inputs: the database, the terminal host,
/// the project, the lead and the pane with the pane process the caller was found under.
/// Output: the member, or none for a pane id that is not plain, a pane no row names or a
/// teammate's pane under another process.
fn pane_member(
    connection: &Connection,
    host: Option<&dyn Host>,
    project: &ProjectId,
    lead: i64,
    (pane, pid): (&str, u32),
) -> Option<PaneMember> {
    if !crate::terminal::pane_id_valid(pane) {
        return None;
    }
    let teammate: Option<(i64, Option<i64>)> = connection
        .query_row(
            "SELECT id,pane_pid FROM team_agents WHERE project=?1 AND parent=?2 AND role='teammate'
             AND status IN ('starting','idle','working') AND pane=?3 ORDER BY id DESC LIMIT 1",
            params![project.as_str(), lead, pane],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ok()?;
    if let Some((teammate, pinned)) = teammate {
        assert_ne!(teammate, lead);
        return teammate_pinned(connection, teammate, pinned, pid)
            .then_some(PaneMember::Teammate(teammate));
    }
    let name = AgentId::team(lead).ok()?;
    let record: Option<String> = connection
        .query_row(
            "SELECT record FROM team_panes WHERE agent=?1",
            [lead],
            |row| row.get(0),
        )
        .optional()
        .ok()?;
    let own = record.as_deref().and_then(PaneRecord::decode)?;
    if pid != own.pid {
        return None;
    }
    host?
        .panes(&name)
        .ok()?
        .iter()
        .any(|listed| listed.pid == own.pid && listed.id == pane)
        .then_some(PaneMember::Lead)
}

/// Whether a native teammate's row is named by this pane process (PLC (b)): the first
/// process the engine names it by is kept on the row, so the pin outlives the caller socket
/// and the engine, and any other process under that pane id is refused. Claude Code writes
/// the pane into its team config before it respawns the placeholder there, so the pin waits
/// for the first call or `who`, never the first listing. Takes the connection, the row, the
/// process it is pinned to, if any, and the caller's pane process. Returns the answer, no
/// also when the pin cannot be kept.
fn teammate_pinned(connection: &Connection, teammate: i64, pinned: Option<i64>, pid: u32) -> bool {
    let pid = i64::from(pid);
    if let Some(pinned) = pinned {
        return pinned == pid;
    }
    if connection
        .execute(
            "UPDATE team_agents SET pane_pid=?2 WHERE id=?1 AND pane_pid IS NULL",
            params![teammate, pid],
        )
        .is_err()
    {
        return false;
    }
    connection
        .query_row(
            "SELECT pane_pid FROM team_agents WHERE id=?1",
            [teammate],
            |row| row.get::<_, Option<i64>>(0),
        )
        .is_ok_and(|kept| kept == Some(pid))
}

/// Who runs in one pane of a native lead's team, for the lead's caller socket to answer
/// `who` with, from the engine's own rows (DEC-111 Q18, RH-2): the lead, or a live teammate
/// by the name the engine kept for it when it first saw it. Inputs: the database, the
/// terminal host, the project, the lead and the pane with the pane process the caller was
/// found under. Output: the lead, the teammate's name, or none, also for a teammate with
/// no name kept.
#[cfg(unix)]
pub(crate) fn pane_whom(
    connection: &Connection,
    host: Option<&dyn Host>,
    project: &ProjectId,
    lead: i64,
    pane: (&str, u32),
) -> Option<super::caller::Whom> {
    match pane_member(connection, host, project, lead, pane)? {
        PaneMember::Lead => Some(super::caller::Whom::Lead),
        PaneMember::Teammate(teammate) => {
            teammates::name(connection, teammate).map(super::caller::Whom::Teammate)
        }
    }
}

/// The panes a native lead's own tmux server lists, live ones only, as (pane id, pid).
/// Takes the host and the lead. Returns them, none when either is missing or unread.
#[cfg(unix)]
fn listed_panes(host: Option<&dyn Host>, lead: Option<&AgentId>) -> Vec<(String, u32)> {
    host.zip(lead)
        .and_then(|(host, lead)| host.panes(lead).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|pane| !pane.dead)
        .map(|pane| (pane.id, pane.pid))
        .collect()
}

/// The private home one team agent's harness runs with as its CLAUDE_CONFIG_DIR or
/// CODEX_HOME. Inputs: the team state folder (`<runtime>/team`) and the agent id.
/// Output: `<state>/agents/agent-<id>/home`.
pub fn agent_home(state: &Path, id: i64) -> PathBuf {
    state
        .join("agents")
        .join(format!("agent-{id}"))
        .join("home")
}

/// The most bytes of a worktree's `.gitignore` read to see whether it ignores the graph.
#[cfg(unix)]
const GITIGNORE_BYTES_MAX: u64 = 256 * 1024;

/// Refuses to show the project's graph in an agent's worktree on Windows, where the engine
/// has no folder-relative, no-follow steps to place the links with; no agent starts with
/// links placed any other way. Takes the project folder and the worktree, both unused.
/// Returns the owner line with `team_graph_unavailable`.
#[cfg(not(unix))]
fn expose_graph(_root: &Path, _worktree: &Path) -> Result<(), String> {
    Err(with_code(
        "The project graph is not shown to agents on Windows in this version",
        Some("team_graph_unavailable"),
    ))
}

/// Gives an agent's worktree its own copy of the project's main graph (DEC-123): the graph
/// the engine last published, copied into the worktree's `graphify-out` as APFS clones on
/// macOS ([`crate::onboarding::main_graph::copy_into`]), so `graphify query` and the
/// graphify reminder hook find it and the agent may update it. Only when git ignores the
/// folder there (see [`ignores_graph`]), so it never shows as a change; anything the agent
/// made there itself is left alone, and the read-only links an older engine placed there
/// (I3) give way to the copy. No link is followed. A graph built while the project hid other
/// folders is not copied, so a folder hidden since reaches no agent. A missing or unreadable
/// graph leaves the agent without one, never without its start. Takes the project folder,
/// the worktree and the folders the project hides now.
#[cfg(unix)]
fn expose_graph(root: &Path, worktree: &Path, hidden: &[PathBuf]) {
    if !ignores_graph(root, worktree) {
        return;
    }
    if let Err(error) = crate::onboarding::main_graph::copy_into(root, worktree, hidden) {
        eprintln!("graph_copy_failed cause={:?}", error.kind());
    }
}

/// Whether git keeps the graph folder out of a worktree's changes, by one of its usual
/// spellings: the last line about it in the worktree's `.gitignore` decides, and with none
/// there the repository's `info/exclude` does, a clone's own where `.git` is a folder and
/// the project repository's, which the engine writes, for a linked worktree. Takes the
/// project folder and the worktree. Returns the answer; an unreadable file names nothing.
#[cfg(unix)]
fn ignores_graph(root: &Path, worktree: &Path) -> bool {
    let git = worktree.join(".git");
    let exclude = match std::fs::symlink_metadata(&git) {
        Ok(metadata) if metadata.is_dir() => Some(git.join("info").join("exclude")),
        Ok(metadata) if metadata.is_file() => Some(root.join(".git").join("info").join("exclude")),
        _ => None,
    };
    match plain_text(&worktree.join(".gitignore"))
        .and_then(|text| crate::agent_git::graph_rule(&text))
    {
        Some(ignored) => ignored,
        None => exclude
            .and_then(|path| plain_text(&path))
            .and_then(|text| crate::agent_git::graph_rule(&text))
            .unwrap_or(false),
    }
}

/// Reads one ignore file as a plain file, never through a link or from a pipe, at most
/// `GITIGNORE_BYTES_MAX` bytes. Takes its path. Returns the text, none when unreadable.
#[cfg(unix)]
fn plain_text(path: &Path) -> Option<String> {
    use rustix::fs::{CWD, FileType, Mode, OFlags, fstat, openat};
    use std::io::Read;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat(CWD, path, flags, Mode::empty()).ok()?;
    let plain =
        fstat(&fd).is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile);
    let mut text = String::new();
    if !plain
        || std::fs::File::from(fd)
            .take(GITIGNORE_BYTES_MAX)
            .read_to_string(&mut text)
            .is_err()
    {
        return None;
    }
    assert!(text.len() as u64 <= GITIGNORE_BYTES_MAX);
    Some(text)
}

/// The Seatbelt profile the engine installs for one team agent, which its harness, and for a
/// native-mode lead its own tmux server and every engine client on it, run inside. Inputs:
/// the team state folder and the agent id. Output: `<state>/profiles/agent-<id>.sb`.
pub fn agent_profile(state: &Path, id: i64) -> PathBuf {
    state.join("profiles").join(format!("agent-{id}.sb"))
}

/// The Seatbelt profile a native-mode lead's teammates' own tmux server, and every teammate
/// pane it forks, run inside (rv-audit S1 F1): the lead's profile without its own tmux
/// socket, serving the teammates' socket without reaching it. Inputs: the team state folder
/// and the lead's id. Output: `<state>/profiles/agent-<id>-teammates.sb`.
pub fn agent_teammates_profile(state: &Path, id: i64) -> PathBuf {
    state
        .join("profiles")
        .join(format!("agent-{id}-teammates.sb"))
}

/// The Seatbelt profile the engine's own git runs inside while it adds a linked native-mode
/// lead's teammates' linked worktrees (OP-9b): it writes only the teammates' folder of the
/// lead's worktree, the project repository's worktree records and teammate branch refs.
/// Inputs: the team state folder and the lead's id. Output:
/// `<state>/profiles/agent-<id>-teammates-setup.sb`.
pub fn agent_teammates_setup_profile(state: &Path, id: i64) -> PathBuf {
    state
        .join("profiles")
        .join(format!("agent-{id}-teammates-setup.sb"))
}

/// Where the engine keeps the socket name a native-mode lead's latest start chose for its
/// teammates' tmux server (see `terminal::renew_teammates_socket`), beside the profiles no
/// agent writes. Inputs: the team state folder and the lead's id. Output:
/// `<state>/profiles/agent-<id>-teammates.socket`.
pub fn agent_teammates_socket_name(state: &Path, id: i64) -> PathBuf {
    state
        .join("profiles")
        .join(format!("agent-{id}-teammates.socket"))
}

/// Where the engine keeps the pid a native-mode lead's teammates' tmux server first answered
/// with at the socket its latest start named (PLC c, PC-L1), beside that socket's name, so an
/// engine restart or a kill of the lead never pins another server there afresh. Inputs: the
/// team state folder and the lead's id. Output:
/// `<state>/profiles/agent-<id>-teammates-server.pid`.
pub fn agent_teammates_server_pid(state: &Path, id: i64) -> PathBuf {
    state
        .join("profiles")
        .join(format!("agent-{id}-teammates-server.pid"))
}

/// Starts the team runtime for a running engine: team state in `<runtime>/team`,
/// pool assignment read live from the engine's settings files on every tick.
/// Takes the runtime folder, both settings files, the launch executables by harness, the
/// engine executable that serves masters their `delegate` tool, the engine's agent register
/// that lists every team agent with its owner controls, the terminal host agents run in and
/// the engine-start re-proof's flag, so agents wait for its sign-in re-checks (DEC-108).
/// Returns the service, or a plain reason.
#[allow(clippy::too_many_arguments)]
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    executables: BTreeMap<&'static str, PathBuf>,
    engine: Option<&Path>,
    agents: Option<Arc<Agents>>,
    terminal: Option<Arc<dyn Host>>,
    reproving: Option<Arc<AtomicBool>>,
) -> Result<TeamService, String> {
    let runtime = runtime
        .canonicalize()
        .map_err(|e| format!("team runtime: {e}"))?;
    let state = runtime.join("team");
    private_directory(&state).map_err(|e| e.0)?;
    let settings = (global.to_path_buf(), project.to_path_buf());
    let (global, project) = settings.clone();
    let access: AccessSource = Box::new(move || {
        crate::config::resolve_config(&global, &project)
            .ok()?
            .settings
            .model_access
    });
    let runtime = TeamRuntime::open(
        &runtime.join("engine.db"),
        &state,
        executables,
        access,
        TeamLimits::default(),
    )
    .map_err(|e| e.0)?
    .with_events(super::master::EVENTS)
    .with_settings(&settings.0, &settings.1);
    let runtime = match engine {
        Some(engine) => runtime.with_delegation_tool(engine).map_err(|e| e.0)?,
        None => runtime,
    };
    let runtime = match agents {
        Some(agents) => runtime.with_agents(agents),
        None => runtime,
    };
    let runtime = match reproving {
        Some(reproving) => runtime.with_reproof(reproving),
        None => runtime,
    };
    let runtime = match terminal {
        Some(host) => runtime.with_terminal(host).map_err(|e| e.0)?,
        None => runtime,
    };
    let runtime = runtime.with_events(super::events::HANDLERS);
    Ok(TeamService::start(runtime, Duration::from_millis(250)))
}

/// Gives a fresh master the recent team conversation before one message.
/// Takes a connection, the project, the id of the message being sent and a byte bound.
/// Returns owner and master lines plus engine messages other than notes, oldest first,
/// keeping the newest lines that fit within the bound. Master and engine text sits on lines
/// of its own under its speaker, every line quoted by `events::agent_text`, since it can
/// repeat what a sub-agent wrote.
pub fn conversation_before(
    connection: &Connection,
    project: &ProjectId,
    before: i64,
    max: usize,
) -> Result<String, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT author,text FROM team_messages WHERE project=?1 AND id<?2 AND state<>'note'
             ORDER BY id DESC LIMIT ?3",
        )
        .map_err(error)?;
    let rows: Vec<(String, String)> = statement
        .query_map(
            params![project.as_str(), before, HISTORY_MESSAGES_MAX],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut lines = Vec::new();
    let mut used = 0;
    for (author, text) in rows {
        let line = match author.as_str() {
            "owner" => format!(
                "Owner: {}\n",
                bound_text(text.trim(), HISTORY_MESSAGE_BYTES_MAX)
            ),
            author => format!(
                "{}:\n{}\n",
                if author == "master" {
                    "Master"
                } else {
                    "Engine"
                },
                bound_text(
                    &super::events::agent_text(text.trim()),
                    HISTORY_MESSAGE_BYTES_MAX
                )
            ),
        };
        if used + line.len() > max {
            break;
        }
        used += line.len();
        lines.push(line);
    }
    lines.reverse();
    let history = lines.concat();
    assert!(history.len() <= max);
    Ok(history)
}

/// Reads the team agents of a project directly from the engine database.
/// Takes a connection and the project.
/// Returns every agent row in creation order.
pub fn agents(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<super::TeamAgent>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1 ORDER BY id"
        ))
        .map_err(error)?;
    statement
        .query_map([project.as_str()], agent_row)
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

#[path = "runtime/delivery.rs"]
mod delivery;

#[path = "runtime/resume.rs"]
mod resume;

#[path = "runtime/spawn_ledger.rs"]
mod spawn_ledger;

mod teammates;

#[cfg(all(test, unix))]
#[path = "runtime/tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/managed_agent.rs"]
pub(crate) mod managed_agent;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/dec_090_tests.rs"]
mod dec_090_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_claude_tests.rs"]
mod live_claude_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_codex_tests.rs"]
mod live_codex_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_failover_tests.rs"]
pub(crate) mod live_failover_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_native_lead_tests.rs"]
mod live_native_lead_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_network_none_tests.rs"]
mod live_network_none_tests;
