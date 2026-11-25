use super::tests::{EngineRoot, engine_root, register_project, repository_git};
use super::*;
use crate::config::{AccountPool, ModelAccess, ModelAuthentication, ModelEndpoint, ModelReference};
use crate::protocol::CredentialRef;
use std::os::unix::fs::PermissionsExt;

const REPORT: &str = "LISTED-19B4-REPORT the market has three rivals";

const MEMBER: &str = "local-acp";

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

/// The model access of one project whose pool holds one keyless OpenAI-compatible local
/// account, the wire format the stand-in ACP harness's `accounts` block takes. Output: the
/// access and its pool.
fn local_access() -> (ModelAccess, AccountPool) {
    let pool = AccountPool {
        id: reference("research-pool"),
        name: None,
        members: vec![reference(MEMBER)],
        projects: vec![reference("alpha")],
        roles: vec![reference("project_leader")],
        fallback_allowed: false,
        provider_quota_policy: Default::default(),
    };
    let access = ModelAccess {
        accounts: vec![reference("owner-account")],
        connections: vec![crate::config::ModelConnection {
            id: reference(MEMBER),
            provider: reference("openai-compatible"),
            harness: reference("rcli"),
            protocol: reference("openai-chat"),
            account_ref: reference("owner-account"),
            endpoint: ModelEndpoint::Http {
                url: "http://127.0.0.1:11434/v1".into(),
            },
            model: ModelReference {
                id: "qwen2.5:0.5b".into(),
                author: reference("openai-compatible"),
            },
            auth: ModelAuthentication::None {},
        }],
        pools: vec![pool.clone()],
    };
    (access, pool)
}

/// The stand-in ACP script whose every turn replies with one text, written into its state
/// folder in the private home it runs in. Input: the text. Output: the script text.
fn replying_standin(text: &str) -> String {
    const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin.sh");
    const MADE: &str = "mkdir -p \"$state\" || refuse \"the folder $state cannot be made.\"\n";
    assert_eq!(STANDIN.matches(MADE).count(), 1);
    let reply = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "standin-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}
            }
        }
    })
    .to_string();
    assert!(!reply.contains('\''), "{reply}");
    STANDIN.replace(
        MADE,
        &format!("{MADE}printf '%s\\n' '{reply}' >\"$state/turn.out\"\n"),
    )
}

/// A git project `alpha` with one start commit, a queued research job on the stand-in ACP
/// harness, and a runner that finds the stand-in script and reads the local provider as
/// reached, as the stand-in replays its replies and never dials it. Inputs: the engine root
/// and the script. Output: the project folder, the runner, the jobs and the job.
fn listed_job(root: &EngineRoot, script: &str) -> (PathBuf, ResearchRunner, ResearchJobs, i64) {
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    repository_git(&folder, &["init", "-q"]);
    repository_git(&folder, &["commit", "-q", "--allow-empty", "-m", "Start"]);
    register_project(&root.database, "alpha", &folder);
    let bin = root.base.join("listed-bin");
    fs::create_dir(&bin).unwrap();
    let standin = bin.join("standin-acp");
    fs::write(&standin, script).unwrap();
    fs::set_permissions(&standin, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = ResearchRunner::new(
        &root.database,
        &root.state,
        Path::new("/usr/bin/true"),
        ResearchLimits {
            wall_time: Duration::from_secs(60),
            ..ResearchLimits::default()
        },
    )
    .unwrap()
    .with_reach(ProviderReach::new(|_, _| Ok(Vec::new()), |_| Ok(())))
    .with_harness_lookup(Arc::new(move |id: &str| {
        (id == "standin-acp").then(|| standin.clone())
    }));
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "standin-acp", false)
        .unwrap();
    (folder, runner, jobs, id)
}

/// T19b 19b-4: a listed ACP harness whose manifest names research runs research under its
/// own id and manifest name, while every native name keeps its own harness.
#[test]
fn a_listed_acp_harness_that_names_research_runs_research_under_its_own_name() {
    let harness = ResearchHarness::of("standin-acp");

    assert_eq!(harness.map(ResearchHarness::id), Some("standin-acp"));
    assert_eq!(harness.map(ResearchHarness::label), Some("Stand-in ACP"));
    assert_eq!(ResearchHarness::of("codex"), Some(ResearchHarness::Codex));
    assert_eq!(
        ResearchHarness::of("claude-code"),
        Some(ResearchHarness::ClaudeCode)
    );
}

/// T19b 19b-4: a research job on a listed ACP harness runs its turn in the harness's private
/// home, handed the pool's keyless account, and finishes with the harness's reply as the
/// report the project gets.
#[test]
fn a_research_job_on_a_listed_acp_harness_finishes_with_its_reply_as_the_report() {
    let root = engine_root();
    let (folder, runner, jobs, id) = listed_job(&root, &replying_standin(REPORT));
    let (access, pool) = local_access();
    let picked = crate::research::accounts::pool_account(&access, &pool, "standin-acp", &|_| true);
    assert!(
        picked.is_some(),
        "the listed harness takes the pool's local account"
    );
    let (_, account) = picked.unwrap();
    assert_eq!(account.harness().id(), "standin-acp");
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: Some(account),
    };

    let run = runner.run_once(&project);

    assert!(run.is_ok(), "{:?}", run.as_ref().err());
    let run = run.unwrap();
    assert!(run.is_some(), "the queued job is claimed");
    let run = run.unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        (run.job, run.status),
        (id, ResearchStatus::Done),
        "{:?}",
        job.error
    );
    let report = run.report.expect("a finished job names its report");
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains(REPORT), "{text}");
}

/// T19b 19b-4 (QUESTION-0100 Q6): a listed harness is handed its account through the one
/// exception to the sandbox's variable guard, which Full access has no sandbox for, so its
/// research job at Full access ends at once with a plain line, spends no second attempt and
/// starts no harness turn.
#[test]
fn a_research_job_on_a_listed_acp_harness_at_full_access_ends_at_once_in_plain_words() {
    let root = engine_root();
    let (folder, runner, jobs, id) = listed_job(&root, &replying_standin(REPORT));
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let policy = kernel_sandbox::policy::AgentPolicy {
        access: AgentAccess::Full,
        ..Default::default()
    };
    crate::sandbox::policy_store::save(&connection, "alpha", &policy).unwrap();
    drop(connection);
    let (access, pool) = local_access();
    let picked = crate::research::accounts::pool_account(&access, &pool, "standin-acp", &|_| true);
    assert!(
        picked.is_some(),
        "the listed harness takes the pool's local account"
    );
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: picked.map(|(_, account)| account),
    };

    let run = runner.run_once(&project);

    let status = run.ok().flatten().map(|run| run.status);
    assert_eq!(status, Some(ResearchStatus::Failed));
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.attempts, 1);
    let error = job.error.unwrap_or_default();
    assert!(error.contains("Full access"), "{error}");
    assert!(!error.contains('_'), "a plain line, not a code: {error}");
}

/// T19b 19b-4 (QUESTION-0100 Q6): a run a listed harness can never take ends before it
/// starts with a plain line that spends no second attempt: Full access, and tools off on a
/// harness whose manifest says its tools cannot be switched off; tools on, or tools off on a
/// harness that switches them off, starts.
#[test]
fn a_listed_research_run_its_harness_can_never_take_is_refused_in_plain_words_before_it_starts() {
    const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-acp.yaml");
    let limited = STANDIN.replace(
        "sub_agents: \"app\"\n",
        "sub_agents: \"app\"\ntools_limited: \"Tools cannot be switched off.\"\n",
    );
    assert_ne!(limited, STANDIN);
    let parse = |text: &str| crate::harnesses::manifest::parse(text.as_bytes()).unwrap();
    let (standin, limited) = (parse(STANDIN), parse(&limited));

    let off = listed_refusal(&limited, AgentAccess::Sandboxed, false, false);

    assert_eq!(off, Some(LISTED_TOOLS_OFF));
    assert_eq!(
        listed_refusal(&limited, AgentAccess::Sandboxed, true, false),
        None
    );
    assert_eq!(
        listed_refusal(&standin, AgentAccess::OwnBranch, false, false),
        None
    );
    assert_eq!(
        listed_refusal(&standin, AgentAccess::Full, true, false),
        Some(LISTED_FULL_ACCESS)
    );
    for line in [LISTED_TOOLS_OFF, LISTED_FULL_ACCESS] {
        assert!(owner_refusal(line), "{line}");
        assert!(!line.contains('_'), "{line}");
    }
}

/// T19b 19b-4 (QUESTION-0100 Q6): a listed harness runs no hooks, so a tools-on research job
/// in a project that turned hooks on is refused at its start, as a listed team agent is; the
/// job ends with a plain line that spends no second attempt.
#[test]
fn a_listed_research_job_refused_for_its_projects_hooks_ends_in_plain_words() {
    let failure = listed_start_failure(LaunchError("launch_listed_hooks_unsupported".into(), None));

    let line = match failure {
        RunError::Invalid(line) => line,
        other => panic!("{other:?}"),
    };
    assert!(owner_refusal(&line), "{line}");
    assert!(line.contains("hooks"), "{line}");
    assert!(!line.contains('_'), "{line}");
}

/// T19b 19b-4 (rv-secreview note 1, QUESTION-0151 A): a tools-on research job on a listed
/// harness in a project that turned hooks on is refused by the check Full access meets,
/// before its worktree is made; with tools off no hook applies.
#[test]
fn a_tools_on_listed_research_job_in_a_project_with_hooks_is_refused_before_its_worktree() {
    const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-acp.yaml");
    let standin = crate::harnesses::manifest::parse(STANDIN.as_bytes()).unwrap();

    let hooked = listed_refusal(&standin, AgentAccess::Sandboxed, true, true);

    assert_eq!(hooked, Some(LISTED_HOOKS));
    assert_eq!(
        listed_refusal(&standin, AgentAccess::Sandboxed, false, false),
        None
    );
    assert_eq!(
        listed_refusal(&standin, AgentAccess::OwnBranch, true, false),
        None
    );
}

/// T19b 19b-4 (rv-secreview note 2, QUESTION-0151 A): a listed research run keeps its
/// harness's raw output as the bundled runs on pipes do, `events.jsonl` and `stderr.log` in
/// its agent folder, owner-only, so the owner has a record of what it did.
#[test]
fn a_listed_research_run_keeps_its_harness_output_in_owner_only_logs() {
    let root = engine_root();
    let (folder, runner, _jobs, id) = listed_job(&root, &replying_standin(REPORT));
    let (access, pool) = local_access();
    let picked = crate::research::accounts::pool_account(&access, &pool, "standin-acp", &|_| true);
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: picked.map(|(_, account)| account),
    };

    let run = runner.run_once(&project);

    assert!(run.is_ok(), "{:?}", run.as_ref().err());
    let agent = root.state.join("agents").join(format!("job-{id}-1"));
    let events = fs::read_to_string(agent.join(EVENTS_LOG)).unwrap_or_default();
    assert!(events.contains("agent_message_chunk"), "{events}");
    assert!(events.contains(REPORT), "{events}");
    for name in [EVENTS_LOG, "stderr.log"] {
        let mode = fs::symlink_metadata(agent.join(name)).map(|meta| meta.permissions().mode());
        assert_eq!(mode.ok().map(|mode| mode & 0o777), Some(0o600), "{name}");
    }
}

/// SA-VID14 on the harness layer (H5a): a listed or pane research agent whose stop the
/// engine could not confirm stays on the hidden folders list after its run returns, with
/// its credential and lease kept, since it may still run; a confirmed stop leaves the list.
#[test]
fn a_research_agent_not_surely_stopped_stays_listed_after_its_run_returns() {
    use kernel_sandbox::hidden_paths::{RunKind, runs_seeing, starting};
    let saved = [PathBuf::from("/tmp/sa-vid14-research/vault")];
    for (project, stopped, kept) in [
        ("sa-vid14-research-stopped", Ok(()), false),
        (
            "sa-vid14-research-uncertain",
            Err(Unstopped::Uncertain),
            true,
        ),
    ] {
        let listed = starting(RunKind::Research, project).expect("the list has room");
        listed.hid(&[]);
        let by = leftovers::HoldBy {
            project: Some(project.into()),
            who: "research job 1".into(),
        };
        let ended = settled(stopped, None, None, listed, by, || {
            leftovers::LeftoverMode::Keep
        });
        assert_eq!(ended.is_err(), kept, "{project}");
        let named = runs_seeing(project, &saved) == vec![RunKind::Research];
        assert_eq!(named, kept, "{project}");
    }
}

/// SA-VID14 on the harness layer (H5a r2): a listed or pane research agent whose stop left
/// processes running holds them with its listing under the owner's leftover setting, so it
/// stays on the hidden folders list while they run, and the run errs uncertain. Red at
/// c561ba133, whose Left arm held the processes but dropped the listing.
#[test]
fn a_research_agent_whose_stop_left_processes_stays_listed_while_they_run() {
    use crate::harnesses::leftovers::{Lineage, held_in};
    use kernel_sandbox::hidden_paths::{RunKind, runs_seeing, starting};
    let project = "sa-vid14-research-left";
    let saved = [PathBuf::from("/tmp/sa-vid14-research-left/vault")];
    let mut sleeper = kernel_processes::spawn_gate::spawn(
        std::process::Command::new("/usr/bin/perl")
            .args([
                "-mPOSIX",
                "-e",
                "POSIX::setsid() > 0 or exit 1; $| = 1; print qq(left\\n); sleep 60",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let pid = rustix::process::Pid::from_child(&sleeper);
    let mut line = String::new();
    let mut out = std::io::BufReader::new(sleeper.stdout.take().unwrap());
    std::io::BufRead::read_line(&mut out, &mut line).unwrap();
    assert_eq!(line, "left\n", "the sleeper never left its session");
    assert_eq!(rustix::process::getsid(Some(pid)), Ok(pid));
    let listed = starting(RunKind::Research, project).expect("the list has room");
    listed.hid(&[]);
    let by = leftovers::HoldBy {
        project: Some(project.into()),
        who: "research job 1".into(),
    };

    let left = Lineage::of_sandboxed(sleeper.id(), None).and_then(Lineage::settle);
    let ended = settled(left, None, None, listed, by, || {
        leftovers::LeftoverMode::Keep
    });
    let held = held_in(project);
    let named = runs_seeing(project, &saved) == vec![RunKind::Research];
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();

    assert!(
        matches!(ended, Err(RunError::Uncertain(_))),
        "a stop that left processes errs uncertain"
    );
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(named, "the listing is held while the process runs");
}

/// The shell line a stand-in harness runs to wait, without a timer of its own, until the row
/// releases it with SIGUSR1; the token in its command line lets [`release`] find it. Input:
/// the token. Output: the line, ending in a newline.
pub(super) fn held_until_released(token: &str) -> String {
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    format!("/usr/bin/perl -e '$SIG{{USR1}}=sub{{exit 0}}; sleep 60' {token}\n")
}

/// Sends SIGUSR1 to every process whose command line holds `token`, the stand-in waiting in
/// [`held_until_released`]. Input: the token. Output: whether any process was found.
pub(super) fn release(token: &str) -> bool {
    let found = kernel_processes::spawn_gate::output(
        std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", token])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    let ids: Vec<String> = String::from_utf8_lossy(&found.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    for id in &ids {
        let sent = kernel_processes::spawn_gate::status(
            std::process::Command::new("/bin/kill")
                .args(["-USR1", id])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(sent.success(), "{id}");
    }
    !ids.is_empty()
}

/// Tells whether a stand-in waits in [`held_until_released`] now. Input: the token. Output:
/// the answer.
pub(super) fn held(token: &str) -> bool {
    kernel_processes::spawn_gate::status(
        std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", token])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap()
    .success()
}

/// A research job whose agent finishes its turn right after a folder it did not hide at its
/// start is saved hidden, after its stop last read the saved folders, publishes no report:
/// the job is queued again with the hidden reason, as a save seen during the run does.
#[test]
fn a_research_job_finishing_right_after_a_folder_is_saved_hidden_publishes_no_report() {
    let root = engine_root();
    let token = format!(
        "cr3-job-{}",
        root.base
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches('.')
    );
    const AT: &str = "\t\tif [ \"$method\" = session/prompt ]; then\n\t\t\tnext_turn\n";
    let script = replying_standin(REPORT);
    assert_eq!(script.matches(AT).count(), 1);
    let script = script.replace(AT, &format!("{AT}\t\t\t{}", held_until_released(&token)));
    let (folder, runner, jobs, id) = listed_job(&root, &script);
    fs::create_dir(folder.join("vault")).unwrap();
    let (access, pool) = local_access();
    let picked = crate::research::accounts::pool_account(&access, &pool, "standin-acp", &|_| true);
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: picked.map(|(_, account)| account),
    };
    let beat = || {
        rusqlite::Connection::open(&root.database)
            .unwrap()
            .query_row(
                "SELECT heartbeat_ms FROM research_jobs WHERE id=?1",
                [id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap()
    };
    let run = std::thread::scope(|scope| {
        let run = scope.spawn(|| runner.run_once(&project));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !held(&token) {
            assert!(
                !run.is_finished(),
                "the run ended before the stand-in took the prompt: {:?}",
                jobs.get(id)
            );
            assert!(
                Instant::now() < deadline,
                "the stand-in never took the prompt"
            );
        }
        let mut seen = vec![beat()];
        while seen.len() < 3 {
            assert!(Instant::now() < deadline, "the run never beat twice");
            let now = beat();
            if seen.last() != Some(&now) {
                seen.push(now);
            }
        }
        let connection = rusqlite::Connection::open(&root.database).unwrap();
        crate::sandbox::access_store::hidden::save(&connection, "alpha", &[folder.join("vault")])
            .unwrap();
        release(&token);
        run.join().unwrap()
    });
    let run = run.unwrap().unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        (run.job, run.status),
        (id, ResearchStatus::Queued),
        "{:?}",
        job.error
    );
    assert!(run.report.is_none(), "{:?}", run.report);
    assert_eq!(job.error.as_deref(), Some(HIDDEN_REQUEUED));
    assert!(
        !folder
            .join(ResearchKind::MarketResearch.report_path())
            .exists()
    );
}

/// A confined research job on a piped harness whose agent exits 0 right after a folder it
/// did not hide at its start is saved hidden, before a heartbeat reads the saved folders,
/// publishes no report: the job is queued again with the hidden reason. The stand-in Codex
/// waits for the row's signal, which the row sends straight after a heartbeat and the save.
#[test]
fn a_piped_research_job_exiting_right_after_a_folder_is_saved_hidden_publishes_no_report() {
    let root = engine_root();
    let token = format!(
        "piped-hide-{}",
        root.base
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches('.')
    );
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    repository_git(&folder, &["init", "-q"]);
    repository_git(&folder, &["commit", "-q", "--allow-empty", "-m", "Start"]);
    register_project(&root.database, "alpha", &folder);
    fs::create_dir(folder.join("vault")).unwrap();
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let confined = kernel_sandbox::policy::AgentPolicy {
        access: AgentAccess::Sandboxed,
        ..Default::default()
    };
    crate::sandbox::policy_store::save(&connection, "alpha", &confined).unwrap();
    let bin = root.base.join("piped-bin");
    fs::create_dir(&bin).unwrap();
    let standin = bin.join("codex");
    fs::write(
        &standin,
        format!("#!/bin/sh\n{}", held_until_released(&token)),
    )
    .unwrap();
    fs::set_permissions(&standin, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = ResearchRunner::new(
        &root.database,
        &root.state,
        &standin,
        ResearchLimits {
            wall_time: Duration::from_secs(60),
            ..ResearchLimits::default()
        },
    )
    .unwrap();
    let jobs = ResearchJobs::open(&root.database).unwrap();
    let id = jobs
        .enqueue("alpha", "market_research", "codex", false)
        .unwrap();
    let provider = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let project = ResearchProject {
        id: "alpha",
        root: &folder,
        account: Some(ResearchAccount::Endpoint {
            base_url: format!(
                "http://127.0.0.1:{}/v1",
                provider.local_addr().unwrap().port()
            ),
            model: "local-model".into(),
            key: None,
        }),
    };
    let beat = || {
        connection
            .query_row(
                "SELECT heartbeat_ms FROM research_jobs WHERE id=?1",
                [id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap()
    };
    let run = std::thread::scope(|scope| {
        let run = scope.spawn(|| runner.run_once(&project));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !held(&token) {
            assert!(
                !run.is_finished(),
                "the run ended before the stand-in started: {:?}",
                jobs.get(id)
            );
            assert!(Instant::now() < deadline, "the stand-in never started");
        }
        let mut seen = vec![beat()];
        while seen.len() < 3 {
            assert!(Instant::now() < deadline, "the run never beat twice");
            let now = beat();
            if seen.last() != Some(&now) {
                seen.push(now);
            }
        }
        crate::sandbox::access_store::hidden::save(&connection, "alpha", &[folder.join("vault")])
            .unwrap();
        release(&token);
        run.join().unwrap()
    });
    let run = run.unwrap().unwrap();
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(
        (run.job, run.status),
        (id, ResearchStatus::Queued),
        "{:?}",
        job.error
    );
    assert!(run.report.is_none(), "{:?}", run.report);
    assert_eq!(job.error.as_deref(), Some(HIDDEN_REQUEUED));
    assert!(
        !folder
            .join(ResearchKind::MarketResearch.report_path())
            .exists()
    );
    drop(provider);
}
