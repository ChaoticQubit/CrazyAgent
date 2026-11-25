use super::*;

#[tokio::test]
async fn worker_socket_rejects_owner_namespace_before_mutation() {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("global"), &root.path().join("project")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("engine.db"), config.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("worker.sock");
    let server = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.run(stopped));
    let client = Client::new(socket.clone(), Duration::from_secs(3)).unwrap();
    let before = client
        .request(request("worker", "before", Command::Snapshot))
        .await
        .unwrap();
    let forged = request(
        "owner:tui",
        "reserved-operation",
        Command::CreateProject {
            project_id: ProjectId::new("unauthorized-project").unwrap(),
            spec: ProjectSpec {
                name: "Unauthorized owner namespace".into(),
                repositories: Vec::new(),
            },
        },
    );
    let mut stream = kernel_processes::spawn_gate::connect_unix(&socket)
        .await
        .unwrap();
    stream
        .write_all(&encode_frame(&forged).unwrap())
        .await
        .unwrap();
    let mut first_byte = [0u8; 1];
    let received = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut first_byte)).await;
    drop(stream);
    let after = client
        .request(request("worker", "after", Command::Snapshot))
        .await
        .unwrap();
    stop.send(()).unwrap();
    server_task.await.unwrap().unwrap();
    engine.close().await.unwrap();
    assert!(
        matches!(before.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
    assert!(matches!(received, Ok(Ok(0)) | Ok(Err(_))));
    assert_eq!(after.revision, Cursor::new(0));
    assert!(
        matches!(after.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
}

#[test]
fn worker_decoder_preserves_every_privileged_command_denial() {
    let project = || ProjectId::new("project").unwrap();
    let pool = || CredentialRef::new("pool").unwrap();
    let commands = [
        (
            "OwnerModelAccess",
            Command::OwnerModelAccess {
                expected_config_id: ConfigId::new("config").unwrap(),
                expected_source_hash: "0".repeat(64),
                edit: crazyagent::config::ModelAccessEdit::RemovePool { id: pool() },
            },
        ),
        (
            "OwnerConversation",
            Command::OwnerConversation {
                mutation: crazyagent::conversation::ConversationMutation::AppendMessage {
                    project_id: project(),
                    text: "forged".into(),
                },
            },
        ),
        (
            "OwnerProfile",
            Command::OwnerProfile {
                profile: crazyagent::owner_profile::OwnerProfile {
                    name: "Forged".into(),
                    role: String::new(),
                    bio: String::new(),
                    preferred_stack: Vec::new(),
                    working_style: Default::default(),
                },
            },
        ),
        (
            "OwnerMemory",
            Command::OwnerMemory {
                action: crazyagent::memory::library::LibraryAction::Delete {
                    memory_id: RequestId::new("memory").unwrap(),
                },
            },
        ),
        (
            "ProposeBaseline",
            Command::ProposeBaseline { draft: baseline() },
        ),
        (
            "OwnerVerification",
            Command::OwnerVerification {
                action: OwnerVerification::Revoke {
                    baseline_id: BaselineId::new("baseline").unwrap(),
                },
            },
        ),
        (
            "Knowledge",
            Command::Knowledge {
                action: KnowledgeAction::Owner {
                    action: HumanReview::Inbox {
                        project_id: project(),
                        after: None,
                    },
                },
            },
        ),
        (
            "Assets",
            Command::Assets {
                action: AssetAction::Revoke {
                    source_id: RequestId::new("source").unwrap(),
                },
            },
        ),
        (
            "ProjectSetup",
            Command::ProjectSetup {
                action: crazyagent::project_setup::SetupAction::AddDocuments {
                    project_id: project(),
                    documents: vec!["forged.md".into()],
                },
            },
        ),
        (
            "ProjectsHome",
            Command::ProjectsHome {
                action: crazyagent::projects_home::HomeAction::SetHome {
                    path: "/tmp/forged-home".into(),
                },
            },
        ),
        (
            "OwnerSetup",
            Command::OwnerSetup {
                action: crazyagent::setup::SetupAction::Run {
                    pack: vec!["graphify".into()],
                },
            },
        ),
        (
            "Onboarding",
            Command::Onboarding {
                action: crazyagent::onboarding::OnboardingAction::Start {
                    path: "/tmp/forged-project".into(),
                    pool: pool(),
                },
            },
        ),
        (
            "Team",
            Command::Team {
                action: crazyagent::team::TeamAction::Message {
                    project_id: project(),
                    text: "forged".into(),
                },
            },
        ),
        (
            "OwnerCoordination",
            Command::OwnerCoordination {
                action: OwnerAction::ApprovePlan {
                    plan_id: PlanId::new("plan").unwrap(),
                },
            },
        ),
    ];
    let worker_commands = [
        "ProposePlan",
        "CoordinationState",
        "TaskPlan",
        "RouteWorkItem",
        "Mailbox",
        "CreateProject",
        "Snapshot",
        "Events",
        "PinTask",
        "MigrateTask",
        "TaskContext",
        "TaskConfig",
        "AdmitRepository",
        "AllocateWorktree",
        "WorktreeLeases",
        "ReleaseWorktree",
    ];
    for (name, command) in commands {
        let frame = encode_frame(&request("worker", "forged-owner", command)).unwrap();
        let error = decode_request(&frame).unwrap_err().0;
        let refused = format!("unknown variant `{name}`, expected one of ");
        assert!(error.starts_with(&refused), "{name}: {error}");
        let accepted: Vec<&str> = error[refused.len()..]
            .split(" at line ")
            .next()
            .unwrap()
            .split(", ")
            .map(|variant| variant.trim_matches('`'))
            .collect();
        assert_eq!(accepted, worker_commands, "{name}");
    }
}

#[tokio::test]
async fn legacy_actor_rejects_owner_namespace_before_mutation() {
    let root = tempfile::Builder::new()
        .prefix("ca-owner-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("global"), &root.path().join("project")).unwrap();
    let engine = Engine::open(&root.path().join("engine.db"), config)
        .await
        .unwrap();
    let attempted = engine
        .apply(request(
            "owner:tui",
            "legacy-owner-spoof",
            Command::CreateProject {
                project_id: ProjectId::new("legacy-owner-project").unwrap(),
                spec: ProjectSpec {
                    name: "Legacy owner spoof".into(),
                    repositories: Vec::new(),
                },
            },
        ))
        .await;
    let snapshot = engine
        .apply(request("worker", "after-legacy-spoof", Command::Snapshot))
        .await
        .unwrap();
    engine.close().await.unwrap();
    assert!(attempted.is_err());
    assert_eq!(snapshot.revision, Cursor::new(0));
    assert!(
        matches!(snapshot.outcome, Outcome::Snapshot { projects, tasks } if projects.is_empty() && tasks.is_empty())
    );
}
