#![cfg(target_os = "macos")]

mod e2e_support;

use base64::Engine as _;
use crazyagent::checks::profile::HOLDOUT_FILE_BYTES_MAX;
use crazyagent::checks::queue::JobState;
use crazyagent::checks::settings::{GateMode, RemovedTests};
use crazyagent::checks::{ChecksChoice, ChecksSettings, ProfileState, Trigger};
use crazyagent::engine::owner::{
    ChecksReply, ChecksRequest, OwnerClient, OwnerClientKind, OwnerResult, SettingsSaved,
};
use crazyagent::landing::{
    self, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace,
};
use crazyagent::protocol::ProjectId;
use e2e_support::{Engine, Owner, git, repository, scratch};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use trackwright_core::model::{OpKind, ProjectKey};

const OPS: [&str; 12] = [
    "cancel",
    "gate",
    "holdout_store",
    "profile_approve",
    "profile_propose",
    "profile_revoke",
    "profile_show",
    "profiles",
    "run",
    "runs",
    "settings_save",
    "settings_show",
];
const HOLDOUT: &[u8] = b"exit 0\n";
const SETTLED_WITHIN: Duration = Duration::from_secs(60);

/// The wire name of a request's op, matched without a fallback so a new op cannot be left
/// out of `OPS` unseen. Input: the request. Output: the op name.
fn op(request: &ChecksRequest) -> &'static str {
    match request {
        ChecksRequest::ProfilePropose { .. } => "profile_propose",
        ChecksRequest::Profiles { .. } => "profiles",
        ChecksRequest::ProfileShow { .. } => "profile_show",
        ChecksRequest::ProfileApprove { .. } => "profile_approve",
        ChecksRequest::ProfileRevoke { .. } => "profile_revoke",
        ChecksRequest::HoldoutStore { .. } => "holdout_store",
        ChecksRequest::SettingsShow { .. } => "settings_show",
        ChecksRequest::SettingsSave { .. } => "settings_save",
        ChecksRequest::Run { .. } => "run",
        ChecksRequest::Cancel { .. } => "cancel",
        ChecksRequest::Runs { .. } => "runs",
        ChecksRequest::Gate { .. } => "gate",
    }
}

/// One check of a profile as the owner writes it, run on request from the snapshot root
/// with network off and no toolchain. Inputs: the id and the argv. Output: the check.
fn check(id: &str, argv: &[&str]) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": null,
        "argv": argv,
        "cwd": ".",
        "format": "exit_code",
        "report": null,
        "timeout_s": 600,
        "network": "off",
        "when": ["on_request"],
        "required": true,
        "protected": false
    })
}

/// The owner's Checks session over the real owner socket, keeping every op it sent.
struct Asking<'a> {
    owner: &'a Owner,
    sent: BTreeSet<&'static str>,
}

impl Asking<'_> {
    async fn ask(&mut self, request: ChecksRequest) -> Result<ChecksReply, String> {
        self.sent.insert(op(&request));
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(encoded.len() < 48 * 1024);
        self.owner
            .controller
            .checks(request)
            .await
            .map_err(|refused| refused.0)
    }
}

/// Opens one sub-agent change set in the engine's own store the way a team turn end does:
/// a worktree added from a fresh repository, its start recorded, a file written and the
/// turn end recorded. Inputs: the engine, the project and the scratch folder. Output: the
/// change set.
fn agent_change_set(engine: &Engine, project: &ProjectId, disk: &Path) -> ChangeSetId {
    let repo = disk.join("repo");
    repository(&repo, "README", "start\n");
    let worktree = disk.join("agent-7");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let db = landing::open(engine.path()).unwrap();
    let workspace = Workspace {
        project: project.clone(),
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "7".into(),
            label: "Agent 7".into(),
        },
        repository: repo.clone(),
        worktree: worktree.clone(),
        git_dir: repo.join(".git/worktrees/agent-7"),
    };
    landing::record_worktree_start(&db, &repo, &worktree, &workspace.git_dir).unwrap();
    fs::write(worktree.join("slow.sh"), "sleep 30\n").unwrap();
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add the slow check".into(),
    };
    landing::record_turn_end(&db, &workspace, &turn)
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_checks_requests_round_trip_every_op() {
    let engine = Engine::start("ca-ve6-checks-");
    let mut owner = Owner::attach(&engine).await;
    let disk = scratch("ca-ve6-checks-home-");
    let root = disk.path().canonicalize().unwrap();
    let home = root.join("Projects");
    fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project("checks-1", "Checked buoys").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("checks-1").unwrap();
    let tracker = OwnerClient::discover(
        engine.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(20),
    )
    .await
    .unwrap()
    .0;
    tracker
        .tracker_attach(
            project.clone(),
            ProjectKey::new("CK").unwrap(),
            "Checked buoys".into(),
        )
        .await
        .unwrap();
    let change_set = agent_change_set(&engine, &project, &root);
    let mut asking = Asking {
        owner: &owner,
        sent: BTreeSet::new(),
    };

    let digest = match asking
        .ask(ChecksRequest::HoldoutStore {
            project: project.clone(),
            base64: base64::engine::general_purpose::STANDARD.encode(HOLDOUT),
        })
        .await
    {
        Ok(ChecksReply::HoldoutStored { digest }) => digest,
        other => panic!("{other:?}"),
    };
    assert_eq!(digest.len(), 64);
    for (size, stored) in [
        (HOLDOUT_FILE_BYTES_MAX, true),
        (HOLDOUT_FILE_BYTES_MAX + 1, false),
    ] {
        let answered = asking
            .ask(ChecksRequest::HoldoutStore {
                project: project.clone(),
                base64: base64::engine::general_purpose::STANDARD.encode(vec![0xA5; size]),
            })
            .await;
        match (answered, stored) {
            (Ok(ChecksReply::HoldoutStored { digest }), true) => assert_eq!(digest.len(), 64),
            (Err(code), false) => assert_eq!(
                code, "checks_holdout_invalid",
                "the store's own per-file bound refuses it, not the request bound"
            ),
            (other, _) => panic!("{size}: {other:?}"),
        }
    }
    let profile = json!({
        "version": 1,
        "checks": [check("slow", &["sh", "slow.sh"])],
        "holdouts": [{"check": check("sealed", &["sh", "sealed.sh"]), "files": {"sealed.sh": digest}}],
        "protected_paths": []
    });
    let proposal = match asking
        .ask(ChecksRequest::ProfilePropose {
            project: project.clone(),
            profile: serde_json::to_string_pretty(&profile).unwrap(),
        })
        .await
    {
        Ok(ChecksReply::Proposed { proposal }) => proposal,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        proposal.question, None,
        "the owner's own proposal asks nothing"
    );
    match asking
        .ask(ChecksRequest::Profiles {
            project: project.clone(),
            after: None,
        })
        .await
    {
        Ok(ChecksReply::Profiles { page }) => {
            assert_eq!(page.next, None);
            assert_eq!(page.profiles.len(), 1, "{page:?}");
            let row = &page.profiles[0];
            assert_eq!(row.id, proposal.id);
            assert_eq!(row.state, ProfileState::Proposed);
            assert_eq!((row.checks, row.holdouts), (1, 1));
            assert_eq!(row.proposed_by, "owner");
        }
        other => panic!("{other:?}"),
    }
    let shown = asking
        .ask(ChecksRequest::ProfileShow {
            project: project.clone(),
            id: proposal.id.clone(),
        })
        .await;
    match &shown {
        Ok(ChecksReply::Profile { profile }) => {
            assert_eq!(profile.state, ProfileState::Proposed);
            assert_eq!(profile.profile.holdouts.len(), 1);
            assert_eq!(
                profile.profile.holdouts[0].files.get("sealed.sh"),
                Some(&digest)
            );
        }
        other => panic!("{other:?}"),
    }
    let base64_holdout = base64::engine::general_purpose::STANDARD.encode(HOLDOUT);
    let shown_text = serde_json::to_string(&shown.unwrap()).unwrap();
    assert!(!shown_text.contains("exit 0") && !shown_text.contains(&base64_holdout));
    match asking
        .ask(ChecksRequest::ProfileApprove {
            project: project.clone(),
            id: proposal.id.clone(),
        })
        .await
    {
        Ok(ChecksReply::Approved { profile }) => assert_eq!(profile.id, proposal.id),
        other => panic!("{other:?}"),
    }

    match asking
        .ask(ChecksRequest::SettingsShow { project: None })
        .await
    {
        Ok(ChecksReply::Settings { settings }) => {
            assert_eq!(settings.project, None);
            assert_eq!(settings.version, 0);
            assert_eq!(settings.values, ChecksChoice::default());
            assert_eq!(settings.below, ChecksSettings::default());
            assert_eq!(settings.effective, ChecksSettings::default());
            assert_eq!(settings.choices.len(), 8, "{:?}", settings.choices);
        }
        other => panic!("{other:?}"),
    }
    let fewer = ChecksChoice {
        agent_requests: Some(2),
        ..ChecksChoice::default()
    };
    for (version, saved) in [
        (0, SettingsSaved::Saved { version: 1 }),
        (0, SettingsSaved::Stale { current: 1 }),
    ] {
        match asking
            .ask(ChecksRequest::SettingsSave {
                project: None,
                values: fewer,
                version,
            })
            .await
        {
            Ok(ChecksReply::Saved { saved: answered }) => assert_eq!(answered, saved),
            other => panic!("{other:?}"),
        }
    }
    match asking
        .ask(ChecksRequest::SettingsShow {
            project: Some(project.clone()),
        })
        .await
    {
        Ok(ChecksReply::Settings { settings }) => {
            assert_eq!(settings.project, Some(project.clone()));
            assert_eq!(settings.version, 0);
            assert_eq!(settings.below.agent_requests, 2);
            assert_eq!(settings.effective.agent_requests, 2);
        }
        other => panic!("{other:?}"),
    }

    let batch = match asking
        .ask(ChecksRequest::Run {
            project: project.clone(),
            change_set: change_set.clone(),
            checks: vec!["slow".into()],
        })
        .await
    {
        Ok(ChecksReply::Queued { batch }) => batch,
        other => panic!("{other:?}"),
    };
    match asking
        .ask(ChecksRequest::Cancel {
            project: project.clone(),
            change_set: change_set.clone(),
        })
        .await
    {
        Ok(ChecksReply::Cancelled { jobs }) => assert_eq!(jobs, 1),
        other => panic!("{other:?}"),
    }
    let started = Instant::now();
    let (jobs, latest) = loop {
        match asking
            .ask(ChecksRequest::Runs {
                project: project.clone(),
                change_set: change_set.clone(),
            })
            .await
        {
            Ok(ChecksReply::Runs { jobs, latest })
                if jobs
                    .iter()
                    .all(|job| !matches!(job.state, JobState::Queued | JobState::Running)) =>
            {
                break (jobs, latest);
            }
            Ok(ChecksReply::Runs { .. }) => {}
            other => panic!("{other:?}"),
        }
        assert!(
            started.elapsed() < SETTLED_WITHIN,
            "the cancel never settled"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].batch, batch);
    assert_eq!(jobs[0].check, "slow");
    assert_eq!(jobs[0].actor, "owner");
    assert_eq!(jobs[0].trigger, Trigger::OnRequest);
    match jobs[0].state {
        JobState::Cancelled => assert!(latest.is_empty(), "{latest:?}"),
        JobState::Recorded => {
            assert_eq!(latest.len(), 1, "{latest:?}");
            assert_eq!(latest[0].check, "slow");
            assert!(!latest[0].holdout);
            let envelope = tracker
                .tracker_op(OpKind::RunShow, json!({"id": latest[0].run_id.to_string()}))
                .await
                .unwrap();
            assert_eq!(envelope["ok"], true, "{envelope}");
        }
        state => panic!("{state:?}"),
    }
    match asking
        .ask(ChecksRequest::Gate {
            project: project.clone(),
            change_set: change_set.clone(),
        })
        .await
    {
        Ok(ChecksReply::Gate { gate }) => {
            assert_eq!(gate.gate, GateMode::Warn);
            assert_eq!(gate.removed_tests, RemovedTests::Flag);
            assert_eq!(gate.profile, Some(proposal.id.clone()));
            assert_eq!(gate.head.len(), 40, "{gate:?}");
            assert_eq!(
                gate.refusal, None,
                "warn lets a change with no passing run through"
            );
            assert_eq!((gate.gaps_total, gate.gaps.len()), (2, 2), "{gate:?}");
            let slow = gate
                .gaps
                .iter()
                .find(|gap| gap.check.as_deref() == Some("slow"));
            let slow = slow.expect("the required check has a gap");
            assert!(
                ["checks_not_run", "checks_failed"].contains(&slow.code.as_str()),
                "{slow:?}"
            );
            assert_eq!(
                slow.run,
                latest.first().map(|run| run.run_id.to_string()),
                "{slow:?}"
            );
            let sealed = gate
                .gaps
                .iter()
                .find(|gap| gap.check.as_deref() == Some("sealed"));
            let sealed = sealed.expect("the owner sees the holdout's gap");
            assert_eq!(
                (sealed.code.as_str(), &sealed.run),
                ("checks_not_run", &None)
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        asking
            .ask(ChecksRequest::Runs {
                project: ProjectId::new("checks-2").unwrap(),
                change_set: change_set.clone(),
            })
            .await,
        Err("checks_change_set_unknown".to_owned()),
        "a change set answers only under its own project"
    );

    match asking
        .ask(ChecksRequest::ProfileRevoke {
            project: project.clone(),
            id: proposal.id.clone(),
        })
        .await
    {
        Ok(ChecksReply::Revoked {}) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(
        asking
            .ask(ChecksRequest::Run {
                project: project.clone(),
                change_set,
                checks: Vec::new(),
            })
            .await,
        Err("checks_no_profile".to_owned())
    );
    assert_eq!(asking.sent, OPS.into_iter().collect::<BTreeSet<_>>());
}
