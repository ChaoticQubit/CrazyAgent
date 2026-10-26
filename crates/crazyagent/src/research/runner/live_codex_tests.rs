use super::*;
use crate::team::runtime::managed_agent::{ManagedAgent, SignedIn, codex};
use std::os::unix::fs::PermissionsExt;

const PROJECT: &str = "dec-090";
const BRIEF: &str = "A launch check of the research agent. The whole report is the single word OK.";

#[test]
#[ignore = "live qualification: needs CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_research_run_is_signed_in_inside_the_research_sandbox() {
    let mut signed_in = SignedIn::codex();
    let engine = ManagedAgent::new("codex", &codex());
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
    let runner = ResearchRunner::new(&database, &state, &codex(), limits).unwrap();
    let jobs = ResearchJobs::open(&database).unwrap();
    jobs.enqueue(PROJECT, "market_research", "codex", false)
        .unwrap();
    let job = jobs.claim(PROJECT, &runner.claimer, 1).unwrap().unwrap();
    let account = ResearchAccount::ManagedCodex {
        connection: signed_in.connection.clone(),
        model: None,
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
    let (ending, _, output) = runner
        .launch_leased(&jobs, &job, &account, place, Some((profile, None)))
        .unwrap();
    let report = fs::read_to_string(&output).unwrap_or_default();
    let errors = fs::read_to_string(agent.join("stderr.log")).unwrap_or_default();
    let events = fs::read_to_string(agent.join("events.jsonl")).unwrap_or_default();
    println!("real codex research run: ending={ending:?} report={report:?}");
    assert!(matches!(ending, Ending::Exited(true)), "{errors}\n{events}");
    assert!(report.contains("OK"), "{report}");
    assert!(!agent.join("home").join("auth.json").exists());
    let rules = fs::read_to_string(state.join("profiles").join(format!(
        "{}.sb",
        agent.file_name().unwrap().to_str().unwrap()
    )))
    .unwrap();
    assert!(!rules.contains(managed.to_str().unwrap()));
}
