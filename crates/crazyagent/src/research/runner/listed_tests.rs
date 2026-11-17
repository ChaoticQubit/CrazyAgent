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
    let policy = crate::sandbox::policy::AgentPolicy {
        access: AgentAccess::Full,
        ..Default::default()
    };
    crate::sandbox::policy::save(&connection, "alpha", &policy).unwrap();
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
    use crate::sandbox::hidden_paths::{RunKind, runs_seeing, starting};
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
