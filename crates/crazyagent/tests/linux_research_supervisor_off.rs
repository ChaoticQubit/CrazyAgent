#![cfg(target_os = "linux")]

mod linux_research_rig;
mod research_support;

use crazyagent::research::jobs::ResearchStatus;

/// With the owner's "Linux call supervisor" Off, a Linux research job fails once, before
/// its harness runs, with the line that says to turn the supervisor on (DEC-205 S6): no
/// retry can change the owner's choice, and no report lands.
#[test]
fn a_linux_research_job_with_the_supervisor_off_fails_once_with_the_supervisor_line() {
    let rig = linux_research_rig::rig("off");

    let (_, job) = rig.run();

    assert_eq!(job.status, ResearchStatus::Failed);
    assert_eq!(job.attempts, 1);
    assert_eq!(
        job.error.as_deref(),
        Some(
            "An agent confined to its own worktree or clone runs on Linux only under the call supervisor. Turn on \"Linux call supervisor\" in Settings, or this computer cannot run it."
        )
    );
    assert_eq!(rig.report(), None);
}
