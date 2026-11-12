use super::*;
use crate::team::runtime::managed_agent::{ManagedAgent, SignedIn, claude};
use std::os::unix::fs::PermissionsExt;

const PROJECT: &str = "dec-090";
const BRIEF: &str = "A launch check of the research agent. The whole report is the single word OK.";

/// Qualifies a managed Claude research run's sign-in and its research sandbox through the
/// production research launch, on the pinned fast model `haiku` so a full report still fits
/// the 300 s bound. It does not qualify the account's default model.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_claude_research_run_on_a_fast_model_is_signed_in_inside_the_research_sandbox() {
    let mut signed_in = SignedIn::claude();
    let engine = ManagedAgent::new("claude-code", &claude());
    let root = engine.project.clone();
    let context = crate::projects_home::state_folder(&root).join("context");
    fs::create_dir_all(&context).unwrap();
    fs::write(context.join("brief.md"), BRIEF).unwrap();
    let state = engine.base.join("research");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let database = engine.base.join("engine.db");
    let limits = ResearchLimits {
        wall_time: Duration::from_secs(300),
        ..ResearchLimits::default()
    };
    let runner = ResearchRunner::new(&database, &state, Path::new("/usr/bin/true"), limits)
        .unwrap()
        .with_claude(&claude())
        .unwrap();
    let jobs = ResearchJobs::open(&database).unwrap();
    jobs.enqueue(PROJECT, "market_research", "claude-code", false)
        .unwrap();
    let job = jobs.claim(PROJECT, &runner.claimer, 1).unwrap().unwrap();
    let account = ResearchAccount::ManagedClaude {
        connection: signed_in.connection.clone(),
        model: Some("haiku".into()),
    };
    let project = ResearchProject {
        id: PROJECT,
        root: &root,
        account: Some(account.clone()),
    };
    let place = runner.job_place(&jobs, &job, &project).unwrap();
    let agent = place.agent.clone();
    let profile = signed_in.acquire(&format!("research-{}", place.name));
    let managed = profile.profile_path().unwrap();
    let started = Instant::now();
    let (ending, _, output) = runner
        .launch_leased(&jobs, &job, &account, place, Some((profile, None)), None)
        .unwrap();
    let seconds = started.elapsed().as_secs();
    let report = fs::read_to_string(&output).unwrap_or_default();
    let errors = fs::read_to_string(agent.join("stderr.log")).unwrap_or_default();
    let head: String = report.chars().take(80).collect();
    println!("real claude research run: ending={ending:?} seconds={seconds} report={head:?}");
    assert!(matches!(ending, Ending::Exited(true)), "{errors}");
    assert!(!report.trim().is_empty());
    assert!(
        !report.contains("Not logged in") && !report.contains("/login"),
        "{report}"
    );
    assert!(!agent.join("home").join(".credentials.json").exists());
    let rules = fs::read_to_string(state.join("profiles").join(format!(
        "{}.sb",
        agent.file_name().unwrap().to_str().unwrap()
    )))
    .unwrap();
    assert!(!rules.contains(managed.to_str().unwrap()));
}

const CUT_PROMPT: &str = "Write every whole number from 1 to 5000 in order, separated by single spaces, and nothing else.";
const CUT_TOKENS: u32 = 64;
const NO_TOOLS_INPUT_MAX: u64 = 1000;
const CONTINUED: &str = "Output token limit hit";
const CONTINUES_MAX: usize = 3;

/// Reads the model requests of a Claude Code run from the transcripts under its home's
/// `projects` folder: per message id with any tokens, the input tokens with cache reads
/// and writes, the output tokens and the stop reason; Claude Code's own error message
/// counts no tokens and is left out. Input: the folder. Output: the requests in file order
/// and how many transcript lines carry Claude Code's continue message.
fn model_requests(projects: &Path) -> (Vec<(u64, u64, String)>, usize) {
    let mut requests: Vec<(String, (u64, u64, String))> = Vec::new();
    let mut continued = 0;
    for project in fs::read_dir(projects).unwrap() {
        for file in fs::read_dir(project.unwrap().path()).unwrap() {
            let file = file.unwrap().path();
            if file.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                continue;
            }
            for line in fs::read_to_string(&file).unwrap().lines() {
                let entry: serde_json::Value = serde_json::from_str(line).unwrap();
                if entry["type"] == "user" && line.contains(CONTINUED) {
                    continued += 1;
                }
                if entry["type"] != "assistant" {
                    continue;
                }
                let message = &entry["message"];
                let usage = &message["usage"];
                let tokens = |key: &str| usage[key].as_u64().unwrap_or(0);
                if tokens("input_tokens") + tokens("output_tokens") == 0 {
                    continue;
                }
                let request = (
                    tokens("input_tokens")
                        + tokens("cache_creation_input_tokens")
                        + tokens("cache_read_input_tokens"),
                    tokens("output_tokens"),
                    message["stop_reason"].as_str().unwrap_or("").to_owned(),
                );
                let id = message["id"].as_str().unwrap().to_owned();
                match requests.iter_mut().find(|(seen, _)| *seen == id) {
                    Some((_, kept)) => *kept = request,
                    None => requests.push((id, request)),
                }
            }
        }
    }
    (
        requests.into_iter().map(|(_, request)| request).collect(),
        continued,
    )
}

/// Qualifies the tools-off Claude Code read-only run, the mapping agent's launch, on the
/// real harness and a managed account: it runs under the no-tools system prompt, not
/// Claude Code's own agent prompt, which alone costs about 2700 input tokens; every request
/// stops at its output cap; and an answer cut there ends the run on its own after Claude
/// Code 2.1.309's at most three continue requests, which `--max-turns 1` does not stop.
/// One call on the fast model `haiku`, with a prompt that carries no project data,
/// reading an empty folder.
#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CLAUDE_PROFILE"]
fn a_real_tools_off_claude_run_stops_every_request_at_its_output_cap_and_ends_on_its_own() {
    let mut signed_in = SignedIn::claude();
    let engine = ManagedAgent::new("claude-code", &claude());
    let state = engine.base.join("research");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let folder = engine.base.join("empty");
    fs::create_dir(&folder).unwrap();
    let folder = folder.canonicalize().unwrap();
    let runner = ResearchRunner::new(
        &engine.base.join("engine.db"),
        &state,
        Path::new("/usr/bin/true"),
        ResearchLimits::default(),
    )
    .unwrap()
    .with_claude(&claude())
    .unwrap();
    let account = ResearchAccount::ManagedClaude {
        connection: signed_in.connection.clone(),
        model: Some("haiku".into()),
    };
    let name = "onboard-m-live-cut-1";
    let run = ReadOnlyRun {
        name,
        agent: "onboarding",
        project: None,
        folder: &folder,
        prompt: CUT_PROMPT,
        account: &account,
        wall_time: Duration::from_secs(300),
        output_tokens: Some(CUT_TOKENS),
    };
    let profile = signed_in.acquire(&format!("research-{name}"));
    let started = Instant::now();
    let ending = runner
        .run_read_only_leased(&run, Some(profile), &|| false)
        .unwrap();
    let seconds = started.elapsed().as_secs();
    let home = state.join("agents").join(name).join("home");
    let (requests, continued) = model_requests(&home.join("projects"));
    let shown = match &ending {
        ReadOnlyEnding::Answered(text) => format!("answered {} chars", text.len()),
        other => format!("{other:?}"),
    };
    println!(
        "real tools-off claude run: {shown}, {seconds} s, requests (input, output, stop) {requests:?}, continue messages {continued}"
    );
    assert!(
        !matches!(ending, ReadOnlyEnding::TimedOut | ReadOnlyEnding::Stopped),
        "{shown}"
    );
    assert!(!requests.is_empty(), "the run made no model request");
    assert!(requests.len() <= 1 + CONTINUES_MAX, "{requests:?}");
    assert_eq!(continued, requests.len() - 1);
    for (_, output, stop) in &requests {
        assert_eq!(stop, "max_tokens");
        assert!(*output <= u64::from(CUT_TOKENS), "{output}");
    }
    assert!(requests[0].0 < NO_TOOLS_INPUT_MAX, "{requests:?}");
    assert!(seconds < 120, "{seconds}");
}
