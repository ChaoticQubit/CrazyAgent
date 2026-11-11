use super::{NOT_RESUMABLE, RECORD_MISSING, conversation, note};
use crate::harnesses::interactive::{self, Run, Transcript};
use crate::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest};
use crate::harnesses::terminal_session::PaneRecord;
use crate::harnesses::transcript::TranscriptReader;
use crate::protocol::ProjectId;
use crate::recovery::{Interrupted, LastTurn, RunState};
use crate::team::AgentRole;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROJECT: &str = "resume-runs";
const CLAUDE_SESSION: &str = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
const CODEX_SESSION: &str = "7d1e9a40-3b2c-4f5e-8a6b-0c9d8e7f6a5b";
const AT: i64 = 1_800_000_000_000;

fn private_folder(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn temporary(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = root.path().canonicalize().unwrap();
    (root, base)
}

fn lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = String::new();
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(path, body).unwrap();
}

fn codex_call(id: &str, command: &[&str]) -> String {
    let arguments = serde_json::json!({ "command": command }).to_string();
    serde_json::json!({
        "type": "response_item",
        "payload": {"type": "function_call", "name": "shell", "call_id": id, "arguments": arguments}
    })
    .to_string()
}

fn codex_output(id: &str) -> String {
    serde_json::json!({
        "type": "response_item",
        "payload": {"type": "function_call_output", "call_id": id, "output": "ok"}
    })
    .to_string()
}

fn task_started() -> String {
    serde_json::json!({"type": "event_msg", "payload": {"type": "task_started"}}).to_string()
}

/// A run cut short in the turn of team task 3, with the given session record.
fn run(
    harness: &str,
    record: Option<PaneRecord>,
    cwd: &Path,
    record_file: Option<PathBuf>,
) -> Interrupted {
    Interrupted {
        id: 1,
        agent: 7,
        project: ProjectId::new(PROJECT).unwrap(),
        kind: AgentRole::Sub,
        harness: harness.into(),
        at: AT,
        last_turn: LastTurn {
            number: 2,
            task: Some(3),
            message: None,
            asked: Some("Write the page".into()),
        },
        record,
        cwd: Some(cwd.to_path_buf()),
        record_file,
        paused: false,
        state: RunState::Interrupted,
        reason: None,
    }
}

/// An agent home, worktree and Seatbelt profile as the runtime gives one team agent.
struct Agent {
    _root: tempfile::TempDir,
    base: PathBuf,
}

impl Agent {
    fn new() -> Self {
        let (root, base) = temporary("ca-resume-command-");
        for folder in ["home", "work", "bin"] {
            private_folder(&base.join(folder));
        }
        std::fs::write(
            base.join("agent.sb"),
            format!(
                "(version 1)\n(allow default)\n(deny file-write* (subpath \"/Users\"))\n(allow file-write* (subpath \"{}\"))\n",
                base.display()
            ),
        )
        .unwrap();
        Self { _root: root, base }
    }

    fn home(&self) -> PathBuf {
        self.base.join("home")
    }

    fn work(&self) -> PathBuf {
        self.base.join("work")
    }

    fn request(&self, harness: &str) -> LaunchRequest {
        let executable = self.base.join("bin").join(harness);
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        LaunchRequest {
            harness: harness.into(),
            executable,
            working_dir: self.work(),
            account: AccountBinding::Subscription { home: self.home() },
            model: None,
            tools: false,
            sandbox_profile: Some(self.base.join("agent.sb")),
            unconfined: false,
            git_identity: None,
            limits: LaunchLimits::default(),
        }
    }
}

fn fresh(request: &LaunchRequest, session: &str) -> interactive::InteractiveCommand {
    let run = Run {
        session_id: session,
        key_file: None,
        resume: false,
    };
    interactive::command(request, None, &run).unwrap()
}

fn resumed(request: &LaunchRequest, session: &str) -> interactive::InteractiveCommand {
    let run = Run {
        session_id: session,
        key_file: None,
        resume: true,
    };
    interactive::command(request, None, &run).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// The resume of an interrupted run is the harness's own resume of the session the run
/// recorded, in the agent's worktree: Claude Code with `--resume <id>` reading the same
/// transcript, Codex with `resume <id>` reading the same rollout, both built by the real
/// interactive command builder. A run without its record cannot be resumed.
#[test]
fn resume_command_names_the_recorded_session_and_worktree() {
    let agent = Agent::new();
    let since = SystemTime::now();

    let claude = agent.request("claude-code");
    let started = fresh(&claude, CLAUDE_SESSION);
    let Transcript::ClaudeSession { file } = &started.transcript else {
        panic!("Claude Code keeps one transcript file");
    };
    lines(
        file,
        &[serde_json::json!({"type": "user", "message": {"role": "user", "content": "Write the page"}})
            .to_string()],
    );
    let record = PaneRecord::new(&started.transcript, &started.cwd, since, 4242, None).unwrap();
    let interrupted = run(
        "claude-code",
        Some(record),
        &started.cwd,
        Some(file.clone()),
    );
    let resume = conversation(&interrupted).unwrap();
    assert_eq!(resume.id, CLAUDE_SESSION);
    assert_eq!(&resume.record, file);
    let again = resumed(&claude, &resume.id);
    let at = again
        .argv
        .iter()
        .position(|argument| argument == "--resume")
        .expect("Claude Code resumes by --resume");
    assert_eq!(again.argv[at + 1], CLAUDE_SESSION);
    assert!(!again.argv.iter().any(|argument| argument == "--session-id"));
    assert_eq!(again.cwd, agent.work().canonicalize().unwrap());
    assert_eq!(
        again.transcript,
        Transcript::ClaudeSession {
            file: resume.record.clone()
        }
    );
    assert!(TranscriptReader::resumed(again.transcript, &again.cwd, &resume.record, None).is_ok());

    let codex = agent.request("codex");
    let started = fresh(&codex, CODEX_SESSION);
    let Transcript::CodexRollouts { sessions } = &started.transcript else {
        panic!("Codex keeps rollouts under its sessions folder");
    };
    let rollout = sessions
        .join("2026/10/27")
        .join(format!("rollout-2026-10-27T10-00-00-{CODEX_SESSION}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": CODEX_SESSION, "cwd": started.cwd.to_string_lossy()}
    });
    lines(&rollout, &[meta.to_string(), task_started()]);
    let record = PaneRecord::new(&started.transcript, &started.cwd, since, 4243, None).unwrap();
    let interrupted = run(
        "codex",
        Some(record.clone()),
        &started.cwd,
        Some(rollout.clone()),
    );
    let resume = conversation(&interrupted).unwrap();
    assert_eq!(resume.id, CODEX_SESSION);
    assert_eq!(resume.record, rollout);
    let again = resumed(&codex, &resume.id);
    let mut argv = started.argv.clone();
    argv.extend(strings(&["resume", CODEX_SESSION]));
    assert_eq!(again.argv, argv);
    assert_eq!(again.cwd, agent.work().canonicalize().unwrap());
    assert_eq!(again.transcript, started.transcript);
    assert!(TranscriptReader::resumed(again.transcript, &again.cwd, &resume.record, None).is_ok());

    let unnamed = sessions
        .join("2026/10/27")
        .join("rollout-2026-10-27T11-00-00-unnamed.jsonl");
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": "not-a-session-id", "cwd": started.cwd.to_string_lossy()}
    });
    lines(&unnamed, &[meta.to_string()]);
    assert_eq!(
        conversation(&run(
            "codex",
            Some(record.clone()),
            &started.cwd,
            Some(unnamed)
        )),
        Err(NOT_RESUMABLE)
    );
    assert_eq!(
        conversation(&run("codex", None, &started.cwd, Some(rollout.clone()))),
        Err(RECORD_MISSING)
    );
    std::fs::remove_file(&rollout).unwrap();
    assert_eq!(
        conversation(&run("codex", Some(record), &started.cwd, Some(rollout))),
        Err(RECORD_MISSING)
    );
}

/// The note an interrupted agent resumes with says when it was cut short, names each
/// command its record started without a result (a finished one is not named), says to
/// verify first, and tells it to finish its task or answer its message. A run whose
/// record cannot be read names no command.
#[test]
fn resume_note_lists_unknown_outcomes() {
    let (_root, base) = temporary("ca-resume-note-");
    let work = base.join("work");
    private_folder(&work);
    let sessions = base.join("sessions");
    let rollout = sessions
        .join("2026/10/27")
        .join(format!("rollout-2026-10-27T10-00-00-{CODEX_SESSION}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": {"id": CODEX_SESSION, "cwd": work.to_string_lossy()}
    });
    lines(
        &rollout,
        &[
            meta.to_string(),
            task_started(),
            codex_call("call-1", &["git", "status"]),
            codex_output("call-1"),
            codex_call("call-2", &["cargo", "publish"]),
            codex_call("call-3", &["touch", "page.md"]),
        ],
    );
    let transcript = Transcript::CodexRollouts {
        sessions: sessions.clone(),
    };
    let record = PaneRecord::new(&transcript, &work, SystemTime::now(), 4244, None).unwrap();
    let when = crate::research::runner::utc_date(
        UNIX_EPOCH + Duration::from_millis(u64::try_from(AT).unwrap()),
    );
    let task = run("codex", Some(record.clone()), &work, Some(rollout.clone()));
    assert_eq!(
        note(&task, None),
        format!(
            "You were interrupted at {when}. These commands have an unknown outcome: shell: cargo publish; shell: touch page.md. Verify before continuing. Then finish the task you were given and reply with a short summary of what you did."
        )
    );

    let mut message = run("codex", Some(record), &work, Some(rollout));
    message.kind = AgentRole::Master;
    message.last_turn.task = None;
    message.last_turn.message = Some(9);
    message.record_file = None;
    assert_eq!(
        note(&message, None),
        format!(
            "You were interrupted at {when}. No command it started is left with an unknown outcome. Verify before continuing. Then answer the message you were answering."
        )
    );
    assert_eq!(
        note(&task, Some("Fix the release page\nand link the notes.")),
        format!(
            "You were interrupted at {when}. These commands have an unknown outcome: shell: cargo publish; shell: touch page.md. Verify before continuing. Then finish the task you were given and reply with a short summary of what you did. The task, in case your session does not show it:\nFix the release page\nand link the notes."
        )
    );
    assert_eq!(
        note(&message, Some("Is the build green?")),
        format!(
            "You were interrupted at {when}. No command it started is left with an unknown outcome. Verify before continuing. Then answer the message you were answering. The message, in case your session does not show it:\nIs the build green?"
        )
    );
}

/// The note of an interrupted wake task carries no messages, so it does not quote a task
/// that says one is at its end (HB-S5-C). Any other task is quoted as it was.
#[test]
fn the_note_of_an_interrupted_wake_task_does_not_point_at_a_missing_message() {
    use crate::team::master::{WAKE_DELIVERED, WAKE_TASK};
    let work = std::path::PathBuf::from("/nowhere");
    let task = run("codex", None, &work, None);
    let written = note(&task, Some(WAKE_TASK));
    assert!(!written.contains(WAKE_TASK), "{written}");
    assert!(written.ends_with(WAKE_DELIVERED), "{written}");
}

/// A wake task stopped as its message was handed over leaves that message unread, so the
/// note says `inspect_work` lists it before it lets the agent end its turn (HB-S5-C-F1).
#[test]
fn the_note_of_an_interrupted_wake_task_says_inspect_work_lists_a_message_still_unread() {
    use crate::team::master::WAKE_TASK;
    let work = std::path::PathBuf::from("/nowhere");
    let task = run("codex", None, &work, None);
    let written = note(&task, Some(WAKE_TASK));
    let unread = format!(
        "{} lists every message you have not read",
        crate::team::tool::INSPECT_WORK_TOOL
    );
    let (listed, end) = (written.find(&unread), written.find("end your turn"));
    assert!(listed.is_some() && listed < end, "{written}");
}

#[cfg(target_os = "macos")]
mod failed {
    use super::super::{RETRY_LATER, RETRY_ONCE};
    use super::{PROJECT, RECORD_MISSING, conversation, private_folder, temporary};
    use crate::config::{
        AccountPool, ModelAccess, ModelAuthentication, ModelConnection, ModelEndpoint,
        ModelReference,
    };
    use crate::engine::EngineError;
    use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
    use crate::protocol::{CredentialRef, ProjectId};
    use crate::recovery::{self, Interrupted, RunState};
    use crate::team::runtime::{TeamLimits, TeamRuntime};
    use crate::terminal::tmux::{SOCKET, Tmux, installed};
    use crate::terminal::{AgentId, FOLDER, Host};
    use rusqlite::{Connection, OptionalExtension, params};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const ENDPOINT: &str = "local-a";
    const WAIT: Duration = Duration::from_secs(30);

    /// A Codex stand-in that resumes the rollout it is named by `resume <id>`, logs each
    /// launch and every typed line, and in a fresh session hangs once in a task that started
    /// a command it never finishes, as a turn cut short does, reading single keys until
    /// Escape, which ends that turn as interrupted in its rollout, as Codex records it, and
    /// then reads what is typed again; with a `deaf` file in its home it never answers
    /// Escape, and with a `stubborn` file it ignores terminate from the next key on. Any
    /// other turn, and every turn of a resumed session, even one whose note quotes that
    /// task, ends with "Done.".
    const RESUMING_CODEX: &str = r#"#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/10/27"
/bin/mkdir -p "$d" || exit 1
printf '%s\n' "$*" >> "$CODEX_HOME/launches"
id=""
resumed=""
prev=""
for a in "$@"; do
  [ "$prev" = "resume" ] && id="$a"
  prev="$a"
done
if [ -n "$id" ]; then
  f=""
  for c in "$d"/rollout-*-"$id".jsonl; do [ -f "$c" ] && f="$c"; done
  [ -n "$f" ] || { echo "no conversation $id"; exit 4; }
  resumed=1
else
  id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
  f="$d/rollout-2026-10-27T00-00-00-$id.jsonl"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
fi
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
hang=0
hung=""
said=""
while IFS= read -r line; do
  case "$line" in *"$e[200~"*) open=1 ;; esac
  printf '%s\n' "$line" >> "$CODEX_HOME/typed"
  said="$said$(printf '%s' "$line" | /usr/bin/sed "s/$e\[20[01]~//g") "
  case "$line" in *"hang here"*) [ -z "$resumed" ] && [ -z "$hung" ] && hang=1 ;; esac
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  text=$(printf '%s' "$said" | /usr/bin/tr -d '\000-\037' | /usr/bin/sed 's/\\/\\\\/g; s/"/\\"/g')
  said=""
  printf '{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"%s"}]}}\n' "$text" >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  if [ "$hang" = 1 ]; then
    /bin/stty -icanon min 1
    printf '{"type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"call-1","arguments":"{\\"command\\":[\\"touch\\",\\"page.md\\"]}"}}\n' >> "$f"
    hang=0
    hung=1
    while :; do
      c=$(/bin/dd bs=1 count=1 2>/dev/null | /usr/bin/od -An -tx1 | /usr/bin/tr -d ' \n')
      [ -n "$c" ] || exit 0
      [ -e "$CODEX_HOME/stubborn" ] && trap '' TERM
      [ "$c" = 1b ] && [ ! -e "$CODEX_HOME/deaf" ] && break
    done
    printf '{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}\n' >> "$f"
    /bin/stty icanon
    continue
  fi
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
"#;

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    /// A project with one Codex sub-agent slot on a local endpoint account, its engine
    /// database and the tmux server its agents' panes run in.
    struct Team {
        _root: tempfile::TempDir,
        base: PathBuf,
        tmux: PathBuf,
        connection: Connection,
        access: ModelAccess,
    }

    impl Team {
        fn new() -> Self {
            let (root, base) = temporary("ca-resume-team-");
            let tmux = installed(&base).expect("tmux must be installed on this machine");
            let connection = Connection::open(base.join("engine.db")).unwrap();
            crate::storage::initialize_authority_schemas(&connection).unwrap();
            let owner = connection.unchecked_transaction().unwrap();
            crate::conversation::initialize(&owner).unwrap();
            owner.commit().unwrap();
            crate::team::initialize(&connection).unwrap();
            crate::project_setup::initialize(&connection).unwrap();
            let folder = base.join("project");
            std::fs::create_dir(&folder).unwrap();
            let setup = StoredSetup {
                project_id: ProjectId::new(PROJECT).unwrap(),
                name: "Resume".into(),
                about: String::new(),
                tech_stack: vec![],
                platforms: vec![],
                research: vec![],
                pool: None,
                team: TeamPlan {
                    master: "codex".into(),
                    master_tools: false,
                    sub_harnesses: vec![HarnessSlot {
                        harness: "codex".into(),
                        agents: 1,
                        tools: false,
                    }],
                    min_agents: 1,
                    max_agents: 2,
                },
                documents: vec![],
                folder: folder.to_str().unwrap().into(),
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
            let access = ModelAccess {
                accounts: vec![reference("local-account")],
                connections: vec![ModelConnection {
                    id: reference(ENDPOINT),
                    provider: reference("openai-compatible"),
                    harness: reference("rcli"),
                    protocol: reference("openai-chat"),
                    account_ref: reference("local-account"),
                    endpoint: ModelEndpoint::Http {
                        url: "http://127.0.0.1:11434/v1".into(),
                    },
                    model: ModelReference {
                        id: "qwen2.5:0.5b".into(),
                        author: reference("openai-compatible"),
                    },
                    auth: ModelAuthentication::None {},
                }],
                pools: vec![AccountPool {
                    id: reference("team-pool"),
                    name: None,
                    members: vec![reference(ENDPOINT)],
                    projects: vec![reference(PROJECT)],
                    roles: vec![reference("project_leader")],
                    fallback_allowed: false,
                    provider_quota_policy: Default::default(),
                }],
            };
            let bin = base.join("bin");
            private_folder(&bin);
            let codex = bin.join("codex");
            std::fs::write(&codex, RESUMING_CODEX).unwrap();
            std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _root: root,
                base,
                tmux,
                connection,
                access,
            }
        }

        fn project(&self) -> ProjectId {
            ProjectId::new(PROJECT).unwrap()
        }

        fn host(&self) -> Arc<dyn Host> {
            Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
        }

        fn runtime(&self, host: &Arc<dyn Host>) -> TeamRuntime {
            let access = self.access.clone();
            TeamRuntime::open(
                &self.base.join("engine.db"),
                &self.base.join("team"),
                BTreeMap::from([("codex", self.base.join("bin/codex"))]),
                Box::new(move || Some(access.clone())),
                TeamLimits::default(),
            )
            .unwrap()
            .with_terminal(host.clone())
            .unwrap()
        }

        fn until(&self, team: &mut TeamRuntime, what: &str, done: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + WAIT;
            while !done(self) {
                assert!(Instant::now() < deadline, "{what}: {:#?}", self.runs());
                team.tick().unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        fn idle(&self, team: &mut TeamRuntime) {
            for _ in 0..20 {
                team.tick().unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        fn runs(&self) -> Vec<Interrupted> {
            let listed = recovery::list(&self.base, &self.project()).unwrap();
            let mut runs = [listed.waiting, listed.settled].concat();
            runs.sort_by_key(|run| run.id);
            runs
        }

        fn task(&self, task: i64) -> (String, Option<i64>, i64) {
            self.connection
                .query_row(
                    "SELECT status,agent,attempts FROM team_tasks WHERE id=?1",
                    [task],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        }

        fn sub(&self) -> Option<i64> {
            self.connection
            .query_row(
                "SELECT id FROM team_agents WHERE project=?1 AND role='sub' ORDER BY id LIMIT 1",
                [PROJECT],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
        }

        fn subs(&self) -> i64 {
            self.connection
                .query_row(
                    "SELECT COUNT(*) FROM team_agents WHERE project=?1 AND role='sub'",
                    [PROJECT],
                    |row| row.get(0),
                )
                .unwrap()
        }

        fn home_file(&self, agent: i64, name: &str) -> String {
            std::fs::read_to_string(
                self.base
                    .join("team/agents")
                    .join(format!("agent-{agent}"))
                    .join("home")
                    .join(name),
            )
            .unwrap_or_default()
        }

        fn rollouts(&self, agent: i64) -> String {
            let folder = self
                .base
                .join("team/agents")
                .join(format!("agent-{agent}"))
                .join("home/sessions/2026/10/27");
            let mut files = std::fs::read_dir(folder)
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            files.sort();
            files
                .iter()
                .map(|file| std::fs::read_to_string(file).unwrap_or_default())
                .collect()
        }

        fn mark(&self, agent: i64, marker: &str) {
            std::fs::write(
                self.base
                    .join("team/agents")
                    .join(format!("agent-{agent}"))
                    .join("home")
                    .join(marker),
                "",
            )
            .unwrap();
        }

        fn launches(&self, agent: i64) -> Vec<String> {
            self.home_file(agent, "launches")
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn agent_status(&self, agent: i64) -> String {
            self.connection
                .query_row(
                    "SELECT status FROM team_agents WHERE id=?1",
                    [agent],
                    |row| row.get(0),
                )
                .unwrap()
        }

        fn dispatched(&self, task: i64) -> Vec<Option<String>> {
            self.connection
                .prepare(
                    "SELECT outcome FROM ledger_actions WHERE kind='dispatch' AND action_key=?1 ORDER BY id",
                )
                .unwrap()
                .query_map([format!("team-task/{task}")], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }

        fn error(&self, task: i64) -> Option<String> {
            self.connection
                .query_row("SELECT error FROM team_tasks WHERE id=?1", [task], |row| {
                    row.get(0)
                })
                .unwrap()
        }

        fn hung(&self, team: &mut TeamRuntime) -> (i64, i64) {
            self.connection
                .execute(
                    "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'owner','codex',0,'hang here: write the page','queued',1)",
                    [PROJECT],
                )
                .unwrap();
            let task = self.connection.last_insert_rowid();
            self.until(team, "the sub-agent hangs in its task", |f| {
                f.sub().is_some_and(|sub| {
                    f.task(task) == ("running".into(), Some(sub), 1)
                        && f.home_file(sub, "typed").contains("hang here")
                        && f.rollouts(sub).contains("\"function_call\"")
                })
            });
            (task, self.sub().unwrap())
        }

        fn noted(&self, text: &str) -> bool {
            self.connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM team_messages WHERE project=?1 AND text=?2)",
                    params![PROJECT, text],
                    |row| row.get(0),
                )
                .unwrap()
        }
    }

    impl Drop for Team {
        fn drop(&mut self) {
            let _ = crate::spawn_gate::status(
                Command::new(&self.tmux)
                    .arg("-S")
                    .arg(self.base.join(FOLDER).join(SOCKET))
                    .arg("kill-server")
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }

    /// A sub-agent cut short in a turn whose session record is gone by the next start is
    /// recorded failed with that reason and a team note: its task stays held, so no agent
    /// starts it fresh. Once the record is back, the owner's retry resumes the agent in its own
    /// Codex session (`resume <id>`) in its worktree with the note naming the command left
    /// without a result, and the task finishes on that resumed turn.
    #[test]
    fn failed_resume_is_recorded_with_its_reason_and_retry_runs_it_again() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        assert_eq!(fixture.launches(sub).len(), 1);
        team.shutdown().unwrap();
        drop(team);
        host.kill(&AgentId::team(sub).unwrap()).unwrap();

        let mut team = fixture.runtime(&host);
        let found = recovery::scan(&fixture.base).unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].agent, sub);
        assert_eq!(found[0].last_turn.task, Some(task));
        let resume = conversation(&found[0]).unwrap();
        let rollout = found[0].record_file.clone().unwrap();
        let kept = std::fs::read(&rollout).unwrap();
        std::fs::remove_file(&rollout).unwrap();

        fixture.until(&mut team, "the resume fails for its missing record", |f| {
            f.runs()[0].state == RunState::Failed
        });
        assert_eq!(fixture.runs()[0].reason.as_deref(), Some(RECORD_MISSING));
        assert!(fixture.noted(&format!(
            "An agent cut short when CrazyAgent stopped could not resume: {RECORD_MISSING}. {RETRY_ONCE}."
        )));
        fixture.idle(&mut team);
        assert_eq!(fixture.task(task), ("queued".into(), None, 1), "held");
        assert_eq!(fixture.launches(sub).len(), 1, "no fresh start");
        assert_eq!(fixture.agent_status(sub), "stopped");
        assert_eq!(fixture.subs(), 1, "no other sub-agent took the task");

        std::fs::write(&rollout, &kept).unwrap();
        assert!(recovery::retry(&fixture.base, sub).unwrap());
        assert_eq!(fixture.runs()[0].state, RunState::Interrupted);
        assert_eq!(fixture.runs()[0].reason, None);
        fixture.until(
            &mut team,
            "the retried resume runs the task to its end",
            |f| f.runs()[0].state == RunState::Resumed && f.task(task).0 == "done",
        );
        let launched = fixture.launches(sub);
        assert_eq!(launched.len(), 2, "{launched:?}");
        assert!(
            launched[1].ends_with(&format!(" resume {}", resume.id)),
            "{launched:?}"
        );
        let typed = fixture.home_file(sub, "typed");
        assert!(typed.contains("You were interrupted at"), "{typed}");
        assert!(
            typed.contains("These commands have an unknown outcome: shell: touch page.md."),
            "{typed}"
        );
        assert!(typed.contains("Verify before continuing."), "{typed}");
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert!(!recovery::retry(&fixture.base, sub).unwrap());
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A sub-agent cut short in a turn while the store cannot be written at the next start
    /// holds its task: the runtime's tick fails before it does anything else, the run stays
    /// `stopped`, and no agent starts the task fresh. Once the store is writable again the
    /// next tick settles the run, and the agent resumes in its own Codex session and finishes
    /// the task there.
    #[test]
    fn a_run_the_store_cannot_settle_holds_its_task_until_it_resumes() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        team.shutdown().unwrap();
        drop(team);
        host.kill(&AgentId::team(sub).unwrap()).unwrap();

        let mut team = fixture.runtime(&host);
        let writer = Connection::open(fixture.base.join("engine.db")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            team.tick(),
            Err(EngineError("recovery_unavailable".into())),
            "the store is held by another writer"
        );
        let state: String = fixture
            .connection
            .query_row(
                "SELECT state FROM recovery_runs WHERE agent=?1",
                [sub],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "stopped");
        assert_eq!(fixture.task(task), ("queued".into(), None, 1), "held");
        assert_eq!(fixture.launches(sub).len(), 1, "no fresh start");
        assert_eq!(fixture.agent_status(sub), "stopped");
        assert_eq!(fixture.subs(), 1, "no other sub-agent took the task");

        writer.execute_batch("ROLLBACK").unwrap();
        drop(writer);
        fixture.until(
            &mut team,
            "the settled run resumes and finishes its task",
            |f| {
                f.runs()
                    .first()
                    .is_some_and(|run| run.state == RunState::Resumed)
                    && f.task(task).0 == "done"
            },
        );
        let runs = fixture.runs();
        assert_eq!(runs.len(), 1, "{runs:#?}");
        assert_eq!(runs[0].agent, sub);
        let resume = conversation(&runs[0]).unwrap();
        let launched = fixture.launches(sub);
        assert_eq!(launched.len(), 2, "{launched:?}");
        assert!(
            launched[1].ends_with(&format!(" resume {}", resume.id)),
            "{launched:?}"
        );
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert_eq!(fixture.subs(), 1, "the task never started fresh");
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A resume that gives up lets go of its task and settles that task's delivery, as the
    /// runtime's failed resume does; while another writer holds the store past its wait, that
    /// settle answers the plain `recovery_unavailable`, never SQLite's own text, and leaves the
    /// delivery open until the store is free.
    #[test]
    fn a_released_task_the_store_cannot_settle_answers_recovery_unavailable() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        team.shutdown().unwrap();
        drop(team);
        host.kill(&AgentId::team(sub).unwrap()).unwrap();
        let key = format!("team-task/{task}");
        let open = |connection: &Connection| {
            crate::ledger::pending(connection, crate::ledger::ActionKind::Dispatch)
                .unwrap()
                .into_iter()
                .any(|action| action.key == key)
        };
        assert!(open(&fixture.connection), "the task's delivery is open");

        recovery::release(&fixture.connection, recovery::HeldItem::Task(task)).unwrap();
        let writer = Connection::open(fixture.base.join("engine.db")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            crate::team::runtime::delivery::settle_released(
                &fixture.connection,
                crate::team::runtime::Busy::Task(task),
            ),
            Err(EngineError("recovery_unavailable".into())),
            "the store is held by another writer"
        );
        assert!(open(&fixture.connection), "the delivery stays open");

        writer.execute_batch("ROLLBACK").unwrap();
        drop(writer);
        assert_eq!(
            crate::team::runtime::delivery::settle_released(
                &fixture.connection,
                crate::team::runtime::Busy::Task(task),
            ),
            Ok(())
        );
        host.close(&[]).unwrap();
    }

    /// A task its sub-agent's record shows it had when the engine stopped is left to the
    /// agent's resume by the next start, never failed as already handed over: its delivery
    /// stays open while the run holds it, the agent resumes its own session with the task
    /// quoted in its note, and the delivery closes done once that resumed turn ends.
    #[test]
    fn a_task_its_agent_had_when_the_engine_stopped_is_resumed_not_failed_as_delivered() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        assert_eq!(fixture.dispatched(task), [None]);
        team.shutdown().unwrap();
        drop(team);
        host.kill(&AgentId::team(sub).unwrap()).unwrap();

        let mut team = fixture.runtime(&host);
        assert_eq!(fixture.task(task), ("queued".into(), None, 1), "held");
        assert_eq!(fixture.error(task), None);
        assert_eq!(fixture.dispatched(task), [None], "left open for the resume");
        fixture.until(&mut team, "the held task resumes and finishes", |f| {
            f.runs()
                .first()
                .is_some_and(|run| run.state == RunState::Resumed)
                && f.task(task).0 == "done"
        });
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert_eq!(fixture.error(task), None);
        assert_eq!(fixture.dispatched(task), [Some("done".to_owned())]);
        let launched = fixture.launches(sub);
        assert_eq!(launched.len(), 2, "{launched:?}");
        assert!(launched[1].contains(" resume "), "{launched:?}");
        let typed = fixture.home_file(sub, "typed");
        assert!(
            typed.contains("The task, in case your session does not show it:"),
            "{typed}"
        );
        assert_eq!(
            typed
                .lines()
                .filter(|line| line.contains("hang here: write the page"))
                .count(),
            2,
            "typed once and quoted once in the note: {typed}"
        );
        assert_eq!(fixture.subs(), 1, "the task never started fresh");
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A sub-agent whose pane the next engine takes over keeps the run of the task it was
    /// cut short in: the run resumes in that pane, where the note with the task quoted is
    /// typed into the session the agent never left, the task finishes on that turn with its
    /// delivery closed done, and the agent is never started again nor handed the task fresh.
    #[test]
    fn a_task_whose_agent_is_taken_over_in_its_pane_resumes_in_that_pane() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        assert_eq!(team.shutdown().unwrap(), vec![AgentId::team(sub).unwrap()]);
        drop(team);

        let mut team = fixture.runtime(&host);
        assert_eq!(
            fixture.dispatched(task),
            [None],
            "open until the panes are taken over"
        );
        fixture.until(&mut team, "the taken-over agent finishes its task", |f| {
            f.runs()
                .first()
                .is_some_and(|run| run.state == RunState::Resumed)
                && f.task(task).0 == "done"
        });
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert_eq!(fixture.error(task), None);
        assert_eq!(fixture.dispatched(task), [Some("done".to_owned())]);
        let runs = fixture.runs();
        assert_eq!(runs.len(), 1, "{runs:#?}");
        assert_eq!(runs[0].agent, sub);
        assert_eq!(
            fixture.launches(sub).len(),
            1,
            "resumed in its pane, not started again"
        );
        let typed = fixture.home_file(sub, "typed");
        assert!(
            typed.contains("The task, in case your session does not show it:"),
            "{typed}"
        );
        assert_eq!(
            typed
                .lines()
                .filter(|line| line.contains("hang here: write the page"))
                .count(),
            2,
            "typed once and quoted once in the note: {typed}"
        );
        assert_eq!(fixture.subs(), 1);
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A kept pane whose hung turn never ends on Escape cannot be taken over: the next
    /// engine ends it. The task its run holds waits for that takeover instead of going to
    /// the pane that is about to end, so it is not failed with that pane; it resumes in a
    /// new pane from the recorded Codex session (`resume <id>`) and finishes there.
    #[test]
    fn a_task_whose_kept_pane_cannot_be_taken_over_resumes_in_a_new_pane() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        fixture.mark(sub, "deaf");
        assert_eq!(team.shutdown().unwrap(), vec![AgentId::team(sub).unwrap()]);
        drop(team);

        let mut team = fixture.runtime(&host);
        fixture.until(&mut team, "the task ends", |f| {
            matches!(f.task(task).0.as_str(), "done" | "failed")
        });
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert_eq!(fixture.error(task), None);
        let runs = fixture.runs();
        assert_eq!(runs.len(), 1, "{runs:#?}");
        assert_eq!(runs[0].agent, sub);
        assert_eq!(runs[0].state, RunState::Resumed);
        let resume = conversation(&runs[0]).unwrap();
        let launched = fixture.launches(sub);
        assert_eq!(launched.len(), 2, "{launched:?}");
        assert!(
            launched[1].ends_with(&format!(" resume {}", resume.id)),
            "{launched:?}"
        );
        let typed = fixture.home_file(sub, "typed");
        assert!(
            typed.contains("The task, in case your session does not show it:"),
            "{typed}"
        );
        assert_eq!(fixture.subs(), 1);
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A kept pane being taken over takes no other queued task while the takeover settles:
    /// its agent stays held for its interrupted run. When the takeover fails, the other task
    /// spends no attempt on the pane that ended; it runs once, after the run resumed in a new
    /// pane.
    #[test]
    fn a_kept_pane_being_taken_over_takes_no_other_task() {
        let fixture = Team::new();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        fixture
            .connection
            .execute(
                "INSERT INTO team_reuse_idle(project,mode) VALUES (?1,'on')",
                [PROJECT],
            )
            .unwrap();
        fixture
            .connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'owner','codex',0,'then write the index','queued',2)",
                [PROJECT],
            )
            .unwrap();
        let other = fixture.connection.last_insert_rowid();
        fixture.mark(sub, "deaf");
        assert_eq!(team.shutdown().unwrap(), vec![AgentId::team(sub).unwrap()]);
        drop(team);

        let mut team = fixture.runtime(&host);
        fixture.until(&mut team, "both tasks end", |f| {
            [task, other]
                .iter()
                .all(|id| matches!(f.task(*id).0.as_str(), "done" | "failed"))
        });
        assert_eq!(fixture.task(task), ("done".into(), Some(sub), 2));
        assert_eq!(
            fixture.task(other),
            ("done".into(), Some(sub), 1),
            "{:?}",
            fixture.error(other)
        );
        let typed = fixture.home_file(sub, "typed");
        let note = typed
            .find("The task, in case your session does not show it:")
            .unwrap_or(usize::MAX);
        let index = typed.find("then write the index").unwrap_or(usize::MAX);
        assert!(note < index && index < usize::MAX, "{typed}");
        assert_eq!(fixture.launches(sub).len(), 2);
        assert_eq!(fixture.subs(), 1);
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }

    /// A kept pane whose takeover fails while the engine cannot confirm its processes gone
    /// is never started again beside them: its run waits, failed for now with that reason
    /// and a team note, and its task stays held for the owner's retry with no new pane. The
    /// agent runs at Full access, so no sandbox keeper ends its pane and the stand-in that
    /// ignores terminate outlasts the cleanup's stop.
    #[test]
    fn a_kept_pane_whose_takeover_cleanup_is_unconfirmed_holds_its_task() {
        let fixture = Team::new();
        crate::sandbox::policy::save(
            &fixture.connection,
            PROJECT,
            &crate::sandbox::policy::AgentPolicy {
                access: crate::sandbox::policy::AgentAccess::Full,
                ..crate::sandbox::policy::AgentPolicy::default()
            },
        )
        .unwrap();
        let host = fixture.host();
        let mut team = fixture.runtime(&host);
        let (task, sub) = fixture.hung(&mut team);
        fixture.mark(sub, "deaf");
        fixture.mark(sub, "stubborn");
        assert_eq!(team.shutdown().unwrap(), vec![AgentId::team(sub).unwrap()]);
        drop(team);

        let mut team = fixture.runtime(&host);
        fixture.until(&mut team, "the failed takeover settles its run", |f| {
            f.runs()
                .first()
                .is_some_and(|run| matches!(run.state, RunState::Failed | RunState::Resumed))
        });
        fixture.idle(&mut team);
        let runs = fixture.runs();
        assert_eq!(runs.len(), 1, "{runs:#?}");
        assert_eq!(runs[0].state, RunState::Failed, "{runs:#?}");
        let reason = "its old terminal was not confirmed stopped (launch_adopt_cleanup_uncertain)";
        assert_eq!(runs[0].reason.as_deref(), Some(reason));
        assert!(fixture.noted(&format!(
            "An agent cut short when CrazyAgent stopped could not resume: {reason}. {RETRY_LATER}."
        )));
        assert_eq!(fixture.task(task), ("queued".into(), None, 1), "held");
        assert_eq!(fixture.launches(sub).len(), 1, "no new pane");
        assert_eq!(host.alive().unwrap(), Vec::<AgentId>::new());
        assert_eq!(fixture.subs(), 1);
        team.shutdown().unwrap();
        host.close(&[]).unwrap();
    }
}

#[cfg(target_os = "macos")]
mod live {
    use super::{conversation, note};
    use crate::harnesses::terminal_session::{PaneRecord, Resume};
    use crate::protocol::ProjectId;
    use crate::recovery::{self, RunState};
    use crate::team::AgentRole;
    use crate::team::runtime::managed_agent::{
        AGENT, ManagedAgent, PROJECT, SignedIn, claude, codex,
    };
    use crate::team::runtime::{Grant, Order, Plan, Report, TurnEnd, TurnRules, per_turn};
    use crate::terminal::tmux::{Tmux, installed};
    use crate::terminal::{AgentId, FOLDER, Host};
    use rusqlite::{Connection, params};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    const TURN_TIME: Duration = Duration::from_secs(600);

    /// The agent's plan, run in a pane on `host`, continuing `resume` when given.
    fn in_pane(plan: &Plan, host: &Arc<dyn Host>, resume: Option<Resume>) -> Plan {
        Plan {
            harness: plan.harness.clone(),
            executable: plan.executable.clone(),
            entry: plan.entry.clone(),
            working_dir: plan.working_dir.clone(),
            model: plan.model.clone(),
            tools: plan.tools,
            sandbox_profile: plan.sandbox_profile.clone(),
            access: plan.access,
            toolchains: plan.toolchains.clone(),
            network: plan.network,
            layout: plan.layout,
            git_identity: plan.git_identity.clone(),
            limits: plan.limits,
            pane: Some((host.clone(), AgentId::team(AGENT).unwrap())),
            live: false,
            resume,
            continues: plan.continues,
            native: plan.native,
            tmux: plan.tmux.clone(),
            native_threads: plan.native_threads,
            hookless: plan.hookless,
            scratch_pin: plan.scratch_pin,
            proxy: plan.proxy.clone(),
            claude_display: plan.claude_display,
            task_start: plan.task_start.clone(),
        }
    }

    fn counted(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Runs one managed turn the way the runtime's per-turn worker does, in its own
    /// thread. Inputs: the agent, the plan, the held profile and the prompt. Output: the
    /// reports and the thread.
    fn start_turn(
        agent: &ManagedAgent,
        plan: Plan,
        signed_in: &mut SignedIn,
        launch: &str,
        prompt: String,
    ) -> (Receiver<Report>, std::thread::JoinHandle<Option<String>>) {
        let project = ProjectId::new(PROJECT).unwrap();
        let context = agent
            .runtime
            .context(
                &project,
                AGENT,
                AgentRole::Sub,
                true,
                crate::agent_git::Layout::Linked,
            )
            .unwrap();
        let (orders, ordered) = channel();
        let (reports, reported) = channel();
        orders
            .send(Order::Turn(
                prompt,
                Some(Box::new(Grant {
                    profile: signed_in.acquire(launch),
                    context,
                    access: crate::sandbox::policy::AgentAccess::Sandboxed,
                })),
                TurnRules {
                    limit: Some(TURN_TIME),
                    background: crate::team::BackgroundAtEnd::Wait,
                },
            ))
            .unwrap();
        drop(orders);
        let home = agent.home.clone();
        let thread = std::thread::spawn(move || {
            let cancel = AtomicBool::new(false);
            let interrupt = AtomicBool::new(false);
            per_turn(&plan, &home, &ordered, &reports, &cancel, &interrupt)
        });
        (reported, thread)
    }

    /// Kills a real managed agent's harness in the middle of a turn after its one command
    /// ran, as an app stop does to it, records the run through the recovery store, and
    /// resumes the agent in its own native session with the note. The resumed agent
    /// verifies instead of running its command again: the command's file holds one line.
    fn resumes_and_runs_nothing_twice(
        harness: &'static str,
        executable: &Path,
        mut signed_in: SignedIn,
    ) {
        let agent = ManagedAgent::new(harness, executable);
        let tmux = installed(&agent.base).expect("tmux must be installed on this machine");
        let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux), &agent.base.join(FOLDER)));
        let counter = agent.working_dir.join("ran.txt");
        let prompt = "Run this exact shell command once with your shell tool and wait for it to finish: printf 'ran\\n' >> ran.txt && sleep 300\nThen reply with the word DONE.".to_owned();

        let (reported, first) = start_turn(
            &agent,
            in_pane(&agent.plan, &host, None),
            &mut signed_in,
            "resume-live-first",
            prompt.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut record: Option<PaneRecord> = None;
        while counted(&counter) == 0 || record.is_none() {
            assert!(Instant::now() < deadline, "the agent never ran its command");
            match reported.recv_timeout(Duration::from_millis(200)) {
                Ok(Report::Ready {
                    pane: Some(pane), ..
                }) => record = Some(*pane),
                Ok(Report::Finished { end, cause, .. }) => {
                    panic!("the first turn ended before it was cut short: {end:?} {cause:?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the first turn stopped before it was cut short")
                }
                _ => {}
            }
        }
        let record = record.unwrap();
        host.kill(&AgentId::team(AGENT).unwrap()).unwrap();
        let ended = first.join().unwrap();
        println!("first turn ended: {ended:?}");
        assert_eq!(counted(&counter), 1);

        let db = Connection::open(agent.base.join("engine.db")).unwrap();
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,attempts,created_ms,started_ms) VALUES (?1,'master',?2,1,?3,'running',?4,1,1,1)",
            params![PROJECT, harness, prompt, AGENT],
        )
        .unwrap();
        let task = db.last_insert_rowid();
        db.execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,task,turns,created_ms,heartbeat_ms) VALUES (?1,?2,'sub',?3,1,'live-connection','working',?4,0,1,?5)",
            params![AGENT, PROJECT, harness, task, crate::team::now_ms()],
        )
        .unwrap();
        recovery::remember(&db, AGENT, &record).unwrap();
        recovery::capture(&db, Some(crate::team::now_ms())).unwrap();
        db.execute(
            "UPDATE team_agents SET status='stopped', task=NULL WHERE id=?1",
            [AGENT],
        )
        .unwrap();
        let found = recovery::scan(&agent.base).unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].state, RunState::Interrupted);
        let resume = conversation(&found[0]).unwrap();
        let note = note(&found[0], None);
        assert!(
            note.contains("ran.txt"),
            "the note names the command its record left without a result"
        );

        let (reported, second) = start_turn(
            &agent,
            in_pane(&agent.plan, &host, Some(resume.clone())),
            &mut signed_in,
            "resume-live-second",
            note,
        );
        let mut resumed = None;
        let mut finished = None;
        let deadline = Instant::now() + TURN_TIME;
        while finished.is_none() {
            assert!(Instant::now() < deadline, "the resumed turn never finished");
            match reported.recv_timeout(Duration::from_secs(1)) {
                Ok(Report::Ready {
                    pane: Some(pane), ..
                }) => resumed = Some(*pane),
                Ok(Report::Finished { end, cause, .. }) => finished = Some((end, cause)),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the resumed turn stopped without finishing")
                }
                _ => {}
            }
        }
        let (end, cause) = finished.unwrap();
        println!("resumed turn: end={end:?} cause={cause:?}");
        let _ = second.join();
        assert_eq!(end, TurnEnd::Success, "{cause:?}");
        let resumed = resumed.expect("the resumed session reported its pane");
        assert_eq!(
            resumed.resumed.map(|(file, _)| file),
            Some(resume.record.clone()),
            "the agent continued its own recorded session"
        );
        assert_eq!(counted(&counter), 1, "the command did not run twice");
        let _ = host.close(&[]);
    }

    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
    fn interrupted_agent_resumes_and_runs_nothing_twice() {
        resumes_and_runs_nothing_twice("claude-code", &claude(), SignedIn::claude());
    }

    #[test]
    #[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
    fn interrupted_codex_agent_resumes_and_runs_nothing_twice() {
        resumes_and_runs_nothing_twice("codex", &codex(), SignedIn::codex());
    }
}
