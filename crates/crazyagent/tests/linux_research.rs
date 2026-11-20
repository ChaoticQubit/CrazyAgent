#![cfg(target_os = "linux")]

mod linux_research_rig;
mod research_support;

use crazyagent::research::jobs::ResearchStatus;

/// A Linux research job runs to Done in its own worktree under the call supervisor, with
/// the owner's "Linux call supervisor" On (DEC-205 S6): its harness runs headless inside the
/// installed boundary, which refuses its append to the worktree's admin `HEAD` and runs it
/// under a seccomp filter, and its report lands in the project.
#[test]
fn a_linux_research_job_runs_to_done_inside_its_supervised_boundary() {
    let rig = linux_research_rig::rig("on");

    let (ran, job) = rig.run();

    let run = ran.unwrap().unwrap();
    assert_eq!(run.status, ResearchStatus::Done, "{:?}", job.error);
    assert_eq!(job.status, ResearchStatus::Done, "{:?}", job.error);
    let report = rig.report().expect("the report lands in the project");
    let probes = report
        .lines()
        .find(|line| line.starts_with("STANDIN "))
        .unwrap_or_else(|| panic!("{report}"));
    assert!(probes.contains("head=denied("), "{probes}");
    assert!(
        probes.contains("Permission denied") || probes.contains("Operation not permitted"),
        "{probes}"
    );
    assert!(
        !probes.contains("seccomp=0") && !probes.contains("seccomp=none"),
        "{probes}"
    );
}
