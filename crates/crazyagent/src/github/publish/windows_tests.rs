use super::{clone, git_safe, project_git};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HOOKS: [&str; 5] = [
    "reference-transaction",
    "pre-commit",
    "commit-msg",
    "post-commit",
    "post-checkout",
];
const ARMORS: [(&str, &str, &str); 3] = [
    (
        "ssh",
        "-----BEGIN SSH SIGNATURE-----",
        "-----END SSH SIGNATURE-----",
    ),
    (
        "pgp",
        "-----BEGIN PGP SIGNATURE-----",
        "-----END PGP SIGNATURE-----",
    ),
    (
        "x509",
        "-----BEGIN SIGNED MESSAGE-----",
        "-----END SIGNED MESSAGE-----",
    ),
];
const SUBJECT: &str = "Signed subject";
const PERSON: &str = "Owner <owner@example.invalid> 1700000000 +0000";

/// The marker programs one row planted, each writing `<itself>.ran` with its own path when
/// started, removed again with every folder the row made when the row ends, pass or fail.
struct Planted {
    files: Vec<PathBuf>,
    folders: Vec<PathBuf>,
}

impl Planted {
    /// Copies `marker` to `path`, making its missing folders, and refuses a path that
    /// already holds something. Inputs: the marker program and the place. Output: none.
    fn plant(&mut self, marker: &Path, path: &Path) {
        assert!(!path.exists(), "refusing to plant over {}", path.display());
        let mut missing = Vec::new();
        let mut folder = path.parent();
        while let Some(parent) = folder.filter(|parent| !parent.exists()) {
            missing.push(parent.to_path_buf());
            folder = parent.parent();
        }
        for parent in missing.into_iter().rev() {
            fs::create_dir(&parent).unwrap();
            self.folders.push(parent);
        }
        fs::copy(marker, path).unwrap();
        self.files.push(path.to_path_buf());
    }

    /// Every planted program that ran since the last call, by the path it wrote, its marks
    /// removed for the next command. Output: the paths, sorted.
    fn ran(&self) -> Vec<String> {
        let mut ran = BTreeSet::new();
        for file in &self.files {
            let mark = file.with_extension("ran");
            if let Ok(text) = fs::read_to_string(&mark) {
                ran.insert(text);
                fs::remove_file(mark).unwrap();
            }
        }
        ran.into_iter().collect()
    }
}

impl Drop for Planted {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = fs::remove_file(file.with_extension("ran"));
            let _ = fs::remove_file(file);
        }
        for folder in self.folders.iter().rev() {
            let _ = fs::remove_dir(folder);
        }
    }
}

/// Builds the marker program with rustc: it writes `<itself>.ran` holding its own path and
/// exits 1. Input: a scratch folder. Output: the program.
fn marker(scratch: &Path) -> PathBuf {
    let source = scratch.join("marker.rs");
    fs::write(
        &source,
        "fn main() {\n    let me = std::env::current_exe().unwrap();\n    let _ = std::fs::write(me.with_extension(\"ran\"), me.display().to_string());\n    std::process::exit(1);\n}\n",
    )
    .unwrap();
    let program = scratch.join("marker.exe");
    let built = crate::spawn_gate::status(
        Command::new("rustc")
            .arg("-O")
            .arg(source)
            .arg("-o")
            .arg(&program),
    )
    .unwrap();
    assert!(built.success(), "rustc built no marker");
    program
}

/// Runs a setup git and returns what it printed, trimmed, failing the row when it fails.
/// Input: the command. Output: the standard output.
fn setup(command: &mut Command) -> String {
    let output = crate::spawn_gate::collected(command).unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A project with a base commit on `main` and one commit signed in each armor, its config
/// naming an empty allowed-signers file, and an empty home beside it. Inputs: git and the
/// folder to make them in. Output: the project, the home and the signed commits.
fn fixture(git: &Path, base: &Path) -> (PathBuf, PathBuf, Vec<(&'static str, String)>) {
    let project = base.join("project");
    let home = base.join("home");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&home).unwrap();
    let plain = || {
        let mut command = clone::git_command(git, &home);
        command.current_dir(&project);
        command
    };
    setup(plain().args(["init", "-q", "--template=", "-b", "main"]));
    let tree = setup(plain().arg("mktree"));
    let first = setup(plain().args([
        "-c",
        "user.name=Owner",
        "-c",
        "user.email=owner@example.invalid",
        "commit-tree",
        tree.as_str(),
        "-m",
        "base",
    ]));
    setup(plain().args(["update-ref", "refs/heads/main", first.as_str()]));
    let signers = base.join("allowed_signers");
    fs::write(&signers, "").unwrap();
    setup(
        plain()
            .args(["config", "gpg.ssh.allowedSignersFile"])
            .arg(signers),
    );
    let mut commits = Vec::new();
    for (armor, begin, end) in ARMORS {
        let text = base.join(format!("commit-{armor}.txt"));
        fs::write(
            &text,
            format!(
                "tree {tree}\nauthor {PERSON}\ncommitter {PERSON}\ngpgsig {begin}\n \n U0lHTkFUVVJF\n {end}\n\n{SUBJECT}\n"
            ),
        )
        .unwrap();
        let commit = setup(
            plain()
                .args(["hash-object", "-t", "commit", "-w"])
                .arg(text),
        );
        commits.push((armor, commit));
    }
    (project, home, commits)
}

/// The root folder of the drive `path` is on, such as `C:\`. Input: an absolute path.
/// Output: the root.
fn drive_root(path: &Path) -> PathBuf {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            PathBuf::from(format!("{}\\", prefix.as_os_str().to_str().unwrap()))
        }
        _ => panic!("{} names no drive", path.display()),
    }
}

/// Gives a git the temp folder an owner's git has, `TEMP` and `TMP`, which the engine's
/// cleared environment lacks: without one a signature check cannot write the signature
/// file and starts no program (probe 44347783949). Input: the command. Output: the same
/// command.
fn with_temp(command: &mut Command) -> &mut Command {
    let temp = std::env::temp_dir();
    command.env("TEMP", &temp).env("TMP", temp)
}

/// A git as the IDE server would start it, under [`crate::ide::git::config_env`], with
/// `path` as its search path. Inputs: git, the project, its home and the search path.
/// Output: the command, without arguments.
fn ide_git(git: &Path, project: &Path, home: &Path, path: &str) -> Command {
    let mut command = Command::new(git);
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "NUL")
        .envs(crate::ide::git::config_env().iter().copied())
        .current_dir(project)
        .stdin(Stdio::null());
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

/// V244-WIN-GIT-SAFE-PROGRAM-PATH: with a marker planted where a drive-relative path, a
/// path lookup or a hook folder could reach (each drive's `usr\bin\false`, `dev\null.exe`
/// and `dev\null\<hook>.exe`, the project, a folder first on the path), publish's git, a
/// stage init, a commit, a signature check per armor (also with the owner's temp folder,
/// which it needs to start a program) and the IDE's git run none, and every git ends well.
#[test]
#[ignore = "plants programs under the drive roots: the farm runs it by name (V244-WIN-GIT-SAFE-PROGRAM-PATH)"]
fn no_planted_program_runs_for_a_hook_a_signature_check_or_the_ssh_command() {
    let engine_path = std::env::var_os("PATH").unwrap();
    let git = clone::git_program(Some(engine_path.as_os_str())).expect("git on the PATH");
    let publish_git = PublishGit::plain(git.clone());
    let scratch = tempfile::tempdir().unwrap();
    let elsewhere = std::env::var_os("RUNNER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let profile = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    let bases = [
        tempfile::Builder::new()
            .prefix("ca-plant")
            .tempdir_in(elsewhere)
            .unwrap(),
        tempfile::Builder::new()
            .prefix("ca-plant")
            .tempdir_in(profile)
            .unwrap(),
    ];
    let marker = marker(scratch.path());
    let fixtures: Vec<_> = bases
        .iter()
        .map(|base| fixture(&git, base.path()))
        .collect();
    let pathdir = scratch.path().join("pathdir");
    let mut planted = Planted {
        files: Vec::new(),
        folders: Vec::new(),
    };
    let drives: BTreeSet<PathBuf> = bases.iter().map(|base| drive_root(base.path())).collect();
    for drive in &drives {
        for place in [r"usr\bin\false", r"usr\bin\false.exe", r"dev\null.exe"] {
            planted.plant(&marker, &drive.join(place));
        }
        for hook in HOOKS {
            planted.plant(
                &marker,
                &drive.join(r"dev\null").join(format!("{hook}.exe")),
            );
        }
    }
    planted.plant(&marker, &pathdir.join("false.exe"));
    for (project, _, _) in &fixtures {
        for place in ["false.exe", r"usr\bin\false.exe", r"dev\null.exe"] {
            planted.plant(&marker, &project.join(place));
        }
    }
    let path = format!("{};{}", pathdir.display(), engine_path.to_str().unwrap());
    let mut failures = Vec::new();
    let mut check = |label: String, output: Output, succeeds: bool, ends: Option<&str>| {
        let ran = planted.ran();
        if !ran.is_empty() {
            failures.push(format!("{label}: ran {ran:?}"));
        }
        if output.status.success() != succeeds {
            failures.push(format!(
                "{label}: ended {:?}, {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if ends.is_some_and(|end| !stdout.ends_with(end)) {
            failures.push(format!("{label}: printed {stdout:?}"));
        }
    };
    let subject = format!("{SUBJECT}\n");
    for (project, home, commits) in &fixtures {
        let at = drive_root(project).display().to_string();
        let first = &commits[0].1;
        check(
            format!("{at} publish update-ref"),
            crate::spawn_gate::collected(project_git(&publish_git, home, project).args([
                "update-ref",
                "-m",
                "row",
                "refs/heads/published",
                first.as_str(),
            ]))
            .unwrap(),
            true,
            None,
        );
        check(
            format!("{at} stage init"),
            crate::spawn_gate::collected(
                clone::git_command(&git, home)
                    .args(git_safe())
                    .args(["init", "--bare", "-q", "--template=", "--"])
                    .arg(home.join("stage")),
            )
            .unwrap(),
            true,
            None,
        );
        check(
            format!("{at} commit"),
            crate::spawn_gate::collected(
                project_git(&publish_git, home, project)
                    .args([
                        "-c",
                        "user.name=Owner",
                        "-c",
                        "user.email=owner@example.invalid",
                    ])
                    .args(["commit", "-q", "--allow-empty", "-m", "row"]),
            )
            .unwrap(),
            true,
            None,
        );
        for (armor, commit) in commits {
            check(
                format!("{at} {armor} signature check"),
                crate::spawn_gate::collected(project_git(&publish_git, home, project).args([
                    "log",
                    "--show-signature",
                    "-1",
                    "--format=%s",
                    commit.as_str(),
                ]))
                .unwrap(),
                true,
                Some(subject.as_str()),
            );
            check(
                format!("{at} {armor} signature check with TEMP"),
                crate::spawn_gate::collected(
                    with_temp(&mut project_git(&publish_git, home, project)).args([
                        "log",
                        "--show-signature",
                        "-1",
                        "--format=%s",
                        commit.as_str(),
                    ]),
                )
                .unwrap(),
                true,
                Some(subject.as_str()),
            );
            check(
                format!("{at} {armor} IDE signature check with TEMP"),
                crate::spawn_gate::collected(
                    with_temp(&mut ide_git(&git, project, home, &path)).args([
                        "log",
                        "--show-signature",
                        "-1",
                        "--format=%s",
                        commit.as_str(),
                    ]),
                )
                .unwrap(),
                true,
                Some(subject.as_str()),
            );
            check(
                format!("{at} {armor} IDE signature check"),
                crate::spawn_gate::collected(ide_git(&git, project, home, &path).args([
                    "log",
                    "--show-signature",
                    "-1",
                    "--format=%s",
                    commit.as_str(),
                ]))
                .unwrap(),
                true,
                Some(subject.as_str()),
            );
        }
        check(
            format!("{at} IDE ssh"),
            crate::spawn_gate::collected(
                ide_git(&git, project, home, &path).args(["ls-remote", "ssh://example.invalid/x"]),
            )
            .unwrap(),
            false,
            None,
        );
        check(
            format!("{at} IDE commit"),
            crate::spawn_gate::collected(
                ide_git(&git, project, home, &path)
                    .args([
                        "-c",
                        "user.name=Owner",
                        "-c",
                        "user.email=owner@example.invalid",
                    ])
                    .args(["commit", "-q", "--allow-empty", "-m", "ide"]),
            )
            .unwrap(),
            true,
            None,
        );
    }
    assert!(
        failures.is_empty(),
        "a Windows git ran a planted program or ended wrong: {failures:#?}"
    );
}
