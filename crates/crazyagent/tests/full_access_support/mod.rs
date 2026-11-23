use crazyagent::harnesses::launch::LaunchBoundary;
use crazyagent::sandbox::{InstalledSandbox, ProjectSandbox};
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::settings::{LinuxStrict, LinuxSupervisor};
use std::path::Path;
use std::sync::Arc;

/// Builds the Full access boundary a launch row starts in (DEC-102): the project sandbox
/// over `project`, installed with its home, temp folder and profile in `scratch`, then
/// marked unconfined. Inputs: two canonical, existing folders. Output: the boundary.
pub fn full_access(project: &Path, scratch: &Path) -> LaunchBoundary {
    let home = scratch.join("full-access-home");
    let tmp = scratch.join("full-access-tmp");
    for folder in [&home, &tmp] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &crazyagent::sandbox::PROFILE_HOOKS,
        project,
        &home,
        &tmp,
        SandboxNetwork::Open,
    )
    .unwrap();
    let installed = InstalledSandbox::install(
        sandbox,
        &scratch.join("full-access.sb"),
        LinuxSupervisor::On,
        LinuxStrict::Off,
    )
    .unwrap();
    let boundary = LaunchBoundary::Installed(Arc::new(installed.unconfined()));
    assert!(boundary.is_unconfined());
    boundary
}
