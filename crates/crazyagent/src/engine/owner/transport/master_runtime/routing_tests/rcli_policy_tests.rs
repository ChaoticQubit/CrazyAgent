use super::*;

/// RCLI-NARROWING: a running RCLI master turn checks the saved policy as native turns do.
/// Covered runs on; launched under Full it stops once the policy is sandboxed; a toolchain
/// granted at launch and turned off stops it with its folder marker gone (r1 sec F1). The
/// collect loop checks every POLICY_CHECK_EVERY against the toolchains fixed at launch.
#[tokio::test]
async fn a_running_rcli_turn_stops_when_the_saved_policy_no_longer_covers_its_launch() {
    use crate::sandbox::policy_store::save;
    use kernel_sandbox::policy::{AgentAccess, AgentPolicy, ToolchainChoice};
    use kernel_sandbox::toolchains::Toolchain;
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    case.register().await;
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("the master turn was prepared");
    };
    let mut prepared = *prepared;
    assert_eq!(prepared.agent_policy.access, AgentAccess::Sandboxed);
    let granted = prepared.agent_policy.allowed(&prepared.repository);
    case.runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &granted)
        .await
        .expect("a turn the saved policy still covers runs on");
    prepared.agent_policy.access = AgentAccess::Full;
    let stopped = case
        .runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &granted)
        .await
        .expect_err("a turn launched under Full stops once the saved policy is sandboxed");
    assert_eq!(stopped.0, "agent_access_lowered");
    assert_eq!(reason_for_error(&stopped), MasterReason::ConnectionChanged);
    prepared.agent_policy.access = AgentAccess::Sandboxed;
    assert!(
        !prepared
            .agent_policy
            .allowed(&prepared.repository)
            .contains(&Toolchain::Rust),
        "the fixture folder has no Rust marker now"
    );
    let off = AgentPolicy {
        toolchains: vec![ToolchainChoice {
            toolchain: Toolchain::Rust,
            allowed: false,
        }],
        ..Default::default()
    };
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer.busy_timeout(Duration::from_secs(5)).unwrap();
    save(&writer, case.session.project_id.as_str(), &off).unwrap();
    let lowered = case
        .runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &[Toolchain::Rust])
        .await
        .expect_err("a toolchain granted at launch and turned off now stops it, marker or not");
    assert_eq!(lowered.0, "agent_toolchain_lowered");
    let source = include_str!("../../master_rcli.rs");
    let start = source.find("async fn collect_rcli_master_turn(").unwrap();
    let end = start
        + source[start..]
            .find("async fn rcli_policy_covers(")
            .unwrap();
    let collect = &source[start..end];
    assert!(
        collect.contains("POLICY_CHECK_EVERY"),
        "the loop paces the check"
    );
    assert!(
        collect.contains("self.rcli_policy_covers(prepared, turn_id, &active.granted)"),
        "the loop stops the turn on a narrowing"
    );
    case.finish().await;
}

/// A running confined master turn stops with `agent_folder_hidden` once its project saves a
/// folder hidden that its sandbox did not hide at its start, and runs on while it hid that
/// folder; a turn at Full access, which hides nothing, runs on.
#[tokio::test]
async fn a_running_master_turn_stops_once_a_folder_it_did_not_hide_is_saved_hidden() {
    use crate::sandbox::access_store::hidden;
    use crate::sandbox::policy_store::save;
    use kernel_sandbox::policy::{AgentAccess, AgentPolicy};
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    case.register().await;
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("the master turn was prepared");
    };
    let mut prepared = *prepared;
    assert_eq!(prepared.agent_policy.access, AgentAccess::Sandboxed);
    assert!(prepared.hidden_paths.is_empty());
    let granted = prepared.agent_policy.allowed(&prepared.repository);
    let project = case.session.project_id.as_str();
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer.busy_timeout(Duration::from_secs(5)).unwrap();
    hidden::save(&writer, project, &[prepared.repository.join("secret")]).unwrap();
    let stopped = case
        .runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &granted)
        .await
        .expect_err("a confined turn that did not hide the saved folder stops");
    assert_eq!(stopped.0, "agent_folder_hidden");
    assert_eq!(reason_for_error(&stopped), MasterReason::ConnectionChanged);
    prepared.hidden_paths = hidden::load(&writer, project).unwrap();
    case.runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &granted)
        .await
        .expect("a confined turn that hid the saved folder runs on");
    let full = AgentPolicy {
        access: AgentAccess::Full,
        ..prepared.agent_policy.clone()
    };
    save(&writer, project, &full).unwrap();
    prepared.agent_policy = full;
    prepared.hidden_paths = Vec::new();
    case.runtime()
        .rcli_policy_covers(&prepared, &case.turn.id, &granted)
        .await
        .expect("a turn at Full access hides nothing and runs on");
    case.finish().await;
}
