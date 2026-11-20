use super::{master_sandbox, master_tools_sandbox};
use crate::config::ModelEndpoint;
use crate::sandbox::policy::{AgentAccess, AgentPolicy};
use crazyagent_win_sandbox::private;
use std::path::Path;

/// V244-WIN-MASTER S6 (PLAN-0410 S6 build): until C1, W3-4b and S9 land, a Windows master
/// turn stays refused by code on the production path, tools off and on, even for a Sandboxed
/// policy, whatever S6's boundary rows can build; lifting it is the leader's call.
#[test]
fn a_windows_master_turn_stays_refused_until_its_gates_land() {
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let path = base.join("gen");
    drop(private::dir(&path).unwrap());
    let root = super::super::Root::open(&path).unwrap();
    let (runtime_ref, _, _) = super::allocate_master_capture(&root).unwrap();
    let policy = AgentPolicy {
        access: AgentAccess::Sandboxed,
        ..AgentPolicy::default()
    };
    let place = Path::new(r"C:\unused");
    let off = master_sandbox(
        &root,
        &runtime_ref,
        place,
        place,
        place,
        place,
        &ModelEndpoint::Native {},
        &policy,
        &[],
    );
    let on = master_tools_sandbox(
        &root,
        &runtime_ref,
        place,
        place,
        place,
        place,
        &ModelEndpoint::Native {},
        place,
        &policy,
        &[],
    );
    for refused in [off, on] {
        match refused {
            Ok(_) => panic!("a Windows master turn got a boundary"),
            Err(error) => assert_eq!(error.0, "master_sandbox_unavailable"),
        }
    }
}
