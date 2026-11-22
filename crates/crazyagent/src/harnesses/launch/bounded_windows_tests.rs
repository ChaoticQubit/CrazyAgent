use super::{
    AccountBinding, GitIdentity, LaunchBoundary, LaunchLimits, LaunchRequest, build,
    claude_invocation, codex_invocation,
};
use crate::sandbox::windows::{HELPER_NAME, container_name};
use crate::sandbox::{
    AGENT_GIT_ENV, AGENT_HOME_ENV, AGENT_TLS_ENV, InstalledSandbox, LineageCheck, LinuxSupervisor,
    ProjectSandbox, SandboxNetwork,
};
use crazyagent_win_sandbox::private;
use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

const BOUNDARY_NAMES: [&str; 13] = [
    "USERPROFILE",
    "HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "TMPDIR",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "PATH",
];
const FOREIGN_CONTAINER: &str = "S-1-15-2-1-2-3-4-5-6-7";

/// The folders of one Windows launch case, under a canonical temp base: the project, the
/// agent's W1 private home and temp folder, and a profiles folder and the boundary's grants
/// folder outside every root, so a case reads no data root and no environment variable.
struct Case {
    _temp: tempfile::TempDir,
    base: PathBuf,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    profiles: PathBuf,
    grants: PathBuf,
}

/// Puts the boundary helper beside the test binary once, where `helper_beside_engine` looks.
/// The release suite's archive already ships it there (`deps`); a plain `cargo test` build
/// has it one folder up, in the target folder, and it is copied. It reads no environment
/// variable and no data root: each case gives its own grants folder (CR-1614).
fn helper_beside() {
    static COPIED: OnceLock<()> = OnceLock::new();
    COPIED.get_or_init(|| {
        let tests = std::env::current_exe().unwrap();
        let beside = tests.with_file_name(HELPER_NAME);
        if !beside.is_file() {
            let built = tests.parent().unwrap().with_file_name(HELPER_NAME);
            assert!(
                built.is_file(),
                "build crazyagent-win-boundary first: {}",
                built.display()
            );
            std::fs::copy(&built, &beside).unwrap();
        }
        assert!(beside.is_file());
    });
}

/// Makes the folders of one case. Output: the case.
fn case() -> Case {
    helper_beside();
    let temp = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(temp.path()).unwrap();
    let project = base.join("project");
    let home = base.join("agent").join("home");
    let tmp = base.join("agent").join("tmp");
    let profiles = base.join("profiles");
    let grants = base.join("grants");
    for folder in [&project, &base.join("agent"), &profiles, &grants] {
        std::fs::create_dir_all(folder).unwrap();
    }
    drop(private::dir(&home).unwrap());
    drop(private::dir(&tmp).unwrap());
    Case {
        _temp: temp,
        base,
        project,
        home,
        tmp,
        profiles,
        grants,
    }
}

/// Installs the boundary of one agent of `project` with `home` and the case's temp folder,
/// its profile at `profile` in the profiles folder, with the case's own grants folder.
/// Inputs: the case, the project, the home and the profile name. Output: the
/// installed boundary.
fn installed(case: &Case, project: &Path, home: &Path, profile: &str) -> Arc<InstalledSandbox> {
    let sandbox =
        ProjectSandbox::new(project, home, &case.tmp, SandboxNetwork::Only(Vec::new())).unwrap();
    let installed = InstalledSandbox::install_with_grants(
        sandbox,
        &case.profiles.join(profile),
        LinuxSupervisor::Off,
        &case.grants,
    )
    .unwrap();
    assert_eq!(installed.sandbox().agent_home(), home);
    Arc::new(installed)
}

/// A launch request for `harness` in the case's project, with `cmd.exe` as the executable,
/// a subscription home and the owner's git identity. Inputs: the case, the harness, the
/// boundary, the home and whether tools are on. Output: the request.
fn request(
    case: &Case,
    harness: &str,
    boundary: LaunchBoundary,
    home: &Path,
    tools: bool,
) -> LaunchRequest {
    let system = std::env::var("SystemRoot").unwrap();
    LaunchRequest {
        harness: harness.into(),
        executable: PathBuf::from(system).join("System32").join("cmd.exe"),
        working_dir: case.project.clone(),
        account: AccountBinding::Subscription {
            home: home.to_path_buf(),
        },
        model: None,
        tools,
        boundary,
        git_identity: Some(
            GitIdentity::new("Repo Owner".into(), "owner@example.invalid".into()).unwrap(),
        ),
        limits: LaunchLimits::default(),
    }
}

/// The value a built command sets for one variable. Inputs: the command and the name.
/// Output: the value as text, or None when it is not set.
fn value(command: &Command, name: &str) -> Option<String> {
    command
        .get_envs()
        .find(|(known, _)| *known == OsStr::new(name))
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())
}

/// The variable names a confined Windows launch of `request` must set, and no other: the
/// boundary's base, the harness's own variables apart from the ones the boundary pins, the
/// shell temp prefix, the fixed agent git and home settings and the git identity.
/// Input: the request. Output: the names.
fn expected_names(request: &LaunchRequest) -> BTreeSet<String> {
    let harness = match request.harness.as_str() {
        "codex" => codex_invocation(request, None, None).unwrap().1,
        "claude-code" => claude_invocation(request, None, None).1,
        other => panic!("no harness {other}"),
    };
    let pinned = |name: &str| {
        BOUNDARY_NAMES[..11]
            .iter()
            .any(|pinned| name.eq_ignore_ascii_case(pinned))
    };
    let mut names: BTreeSet<String> = BOUNDARY_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    names.extend(
        harness
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .filter(|name| !pinned(name)),
    );
    names.insert("TMPPREFIX".to_owned());
    names.extend(
        AGENT_GIT_ENV
            .iter()
            .chain(AGENT_TLS_ENV.iter())
            .chain(AGENT_HOME_ENV.iter())
            .map(|(name, _)| (*name).to_owned()),
    );
    let identity = request.git_identity.as_ref().unwrap();
    names.extend(identity.variables().map(|(name, _)| name.to_owned()));
    assert!(names.len() > BOUNDARY_NAMES.len());
    names
}

/// The PATH the boundary's base sets, from the system root. Output: the PATH text.
fn system_path() -> String {
    let system = std::env::var("SystemRoot").unwrap();
    let wbem = format!(r"{system}\System32\Wbem");
    let shell = format!(r"{system}\System32\WindowsPowerShell\v1.0");
    format!(r"{system}\System32;{system};{wbem};{shell}")
}

/// A SID's text form, `S-1-<authority>-<sub>...`, from its bytes. Input: the SID bytes.
/// Output: the text.
fn sid_text(sid: &[u8]) -> String {
    assert!(sid.len() >= 8);
    assert_eq!(sid.len(), 8 + 4 * usize::from(sid[1]));
    let authority = sid[2..8]
        .iter()
        .fold(0u64, |total, byte| (total << 8) | u64::from(*byte));
    let (parts, rest) = sid[8..].as_chunks::<4>();
    assert!(rest.is_empty());
    let parts = parts
        .iter()
        .map(|part| u32::from_le_bytes(*part).to_string());
    std::iter::once(format!("S-{}-{authority}", sid[0]))
        .chain(parts)
        .collect::<Vec<_>>()
        .join("-")
}

/// Gives a SID modify on a folder and what is made in it, as the boundary's own grant
/// does. Inputs: the folder and the SID text.
fn grant(folder: &Path, sid: &str) {
    let grant = format!("*{sid}:(OI)(CI)(M)");
    let status = kernel_processes::spawn_gate::status(
        Command::new("icacls")
            .args([folder.to_str().unwrap(), "/grant", grant.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "icacls grant {sid}");
}

/// Lists every path under `root`, sorted, so a row can tell that a refused launch made
/// nothing there. Input: the folder. Output: the paths below it.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                pending.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// H3 row 1 (ANSWER-0405 conditions 1-4, H3-Q1): codex and claude-code, with tools off and
/// on, start at the installed boundary in the project with only the boundary's base, the
/// harness and git variables: the System32 PATH, no Unix PATH, TMPDIR, LANG or tool homes,
/// and no lineage check.
#[test]
fn a_windows_launch_starts_at_its_installed_boundary_with_only_the_boundary_variables() {
    let case = case();
    let boundary = installed(&case, &case.project, &case.home, "agent.sb");
    assert_eq!(boundary.lineage_check(), LineageCheck::Unneeded);
    for harness in ["codex", "claude-code"] {
        for tools in [false, true] {
            let launch = LaunchBoundary::Installed(boundary.clone());
            let request = request(&case, harness, launch, &case.home, tools);
            let built = build(&request, None).unwrap();
            assert_eq!(built.get_program(), boundary.launcher().as_os_str());
            let args: Vec<&OsStr> = built.get_args().take(4).collect();
            let start = [
                OsStr::new(PROFILE_FLAG),
                boundary.profile().as_os_str(),
                OsStr::new(SEPARATOR),
                request.executable.as_os_str(),
            ];
            assert_eq!(args, start, "{harness} tools {tools}");
            assert_eq!(built.get_current_dir(), Some(case.project.as_path()));
            assert_eq!(value(&built, "PATH"), Some(system_path()));
            assert_eq!(value(&built, "HOME"), Some(case.home.display().to_string()));
            assert_eq!(
                value(&built, "TMPDIR"),
                Some(case.tmp.display().to_string())
            );
            for absent in ["LANG", "LC_ALL", "CARGO_HOME", "RUSTUP_HOME"] {
                assert_eq!(
                    value(&built, absent),
                    None,
                    "{harness} tools {tools} {absent}"
                );
            }
            let names: BTreeSet<String> = built
                .get_envs()
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect();
            assert_eq!(names, expected_names(&request), "{harness} tools {tools}");
        }
    }
}

/// H3 row 2 (ANSWER-0405 condition 2): a home that is not the boundary's agent home is
/// refused, while the same request with the agent home builds.
#[test]
fn a_windows_launch_with_another_home_than_its_boundary_is_refused() {
    let case = case();
    let boundary = installed(&case, &case.project, &case.home, "agent.sb");
    let other = case.base.join("agent").join("other");
    drop(private::dir(&other).unwrap());
    let launch = LaunchBoundary::Installed(boundary.clone());
    let refused = build(&request(&case, "codex", launch, &other, false), None).unwrap_err();
    assert_eq!(refused.0, "launch_environment_refused");
    let launch = LaunchBoundary::Installed(boundary);
    build(&request(&case, "codex", launch, &case.home, false), None).unwrap();
}

/// H3 row 3 (rv-sec2 #710 C5): a Full access boundary on Windows is refused and writes
/// nothing under the home, while the confined twin builds.
#[test]
fn a_full_access_launch_on_windows_is_refused_and_writes_nothing() {
    let case = case();
    let confined = installed(&case, &case.project, &case.home, "agent.sb");
    let full = installed(&case, &case.project, &case.home, "full.sb");
    let full = Arc::new(Arc::into_inner(full).unwrap().unconfined());
    assert!(full.is_unconfined());
    let before = tree(&case.home);
    let launch = LaunchBoundary::Installed(full);
    let refused = build(&request(&case, "codex", launch, &case.home, false), None).unwrap_err();
    assert_eq!(refused.0, "launch_sandbox_unavailable");
    assert_eq!(tree(&case.home), before);
    let launch = LaunchBoundary::Installed(confined);
    build(&request(&case, "codex", launch, &case.home, false), None).unwrap();
}

/// H3 row 4 (rv-sec2 #710 C5): a profile boundary off macOS, even one naming the installed
/// profile file, is refused, while the installed twin builds.
#[test]
fn a_profile_boundary_on_windows_is_refused() {
    let case = case();
    let boundary = installed(&case, &case.project, &case.home, "agent.sb");
    let launch = LaunchBoundary::Profile(boundary.profile().to_path_buf());
    let refused = build(&request(&case, "codex", launch, &case.home, false), None).unwrap_err();
    assert_eq!(refused.0, "launch_sandbox_unavailable");
    let launch = LaunchBoundary::Installed(boundary);
    build(&request(&case, "codex", launch, &case.home, false), None).unwrap();
}

/// H3 row 5 (Q9, rv-sec3 #712 N1): a home granted only its boundary's own container builds;
/// once it also grants another container, a made-up one or the container of a second
/// boundary of another project with the same home, the launch is refused, as the expected
/// SID comes only from the request's boundary.
#[test]
fn a_home_granted_another_container_than_its_boundary_is_refused() {
    let case = case();
    let shared = case.base.join("agent").join("shared");
    drop(private::dir(&shared).unwrap());
    let elsewhere = case.base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    for (home, second) in [(&case.home, None), (&shared, Some(&elsewhere))] {
        let boundary = installed(&case, &case.project, home, "agent.sb");
        let own = crazyagent_win_sandbox::container_sid(&container_name(boundary.sandbox()));
        let own = own.unwrap();
        grant(home, &sid_text(&own));
        let launch = LaunchBoundary::Installed(boundary.clone());
        build(&request(&case, "codex", launch, home, false), None).unwrap();
        let foreign = match second {
            None => FOREIGN_CONTAINER.to_owned(),
            Some(project) => {
                let second = installed(&case, project, home, "second.sb");
                let theirs = container_name(second.sandbox());
                let theirs = crazyagent_win_sandbox::container_sid(&theirs).unwrap();
                assert_ne!(theirs, own);
                sid_text(&theirs)
            }
        };
        grant(home, &foreign);
        let launch = LaunchBoundary::Installed(boundary);
        let refused = build(&request(&case, "codex", launch, home, false), None).unwrap_err();
        assert_eq!(refused.0, "launch_account_home_unsafe", "{foreign}");
    }
}

/// The engine's grants folder from an owner's data root with a slash, a UNC prefix or a
/// relative part is refused before the plain-path assert of `install_with_grants`, and a plain
/// drive folder is kept as it is (CR-1614 r4).
#[test]
fn grants_folder_outside_the_plain_drive_spelling_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let plain = crate::sandbox::canonical(temp.path())
        .unwrap()
        .join("grants");
    assert_eq!(crate::sandbox::plain_grants(plain.clone()).unwrap(), plain);
    for refused in [
        r"C:/data\grants",
        r"\\server\share\grants",
        r"C:\data\..\grants",
        "grants",
    ] {
        let error = crate::sandbox::plain_grants(PathBuf::from(refused)).unwrap_err();
        assert!(
            matches!(error, crate::sandbox::SandboxError::Invalid(_)),
            "{refused}: {error:?}"
        );
    }
}
