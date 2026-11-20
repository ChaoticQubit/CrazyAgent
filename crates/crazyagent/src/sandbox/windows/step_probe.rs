use super::owner_git::{farm, install_root};
use super::{GRANTS_FOLDER, HELPER_NAME, WindowsSandbox};
use crate::sandbox::{ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, canonical};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The facts one probe run took, one line each, in the order taken.
struct Facts(String);

impl Facts {
    /// Records one line. Input: the line.
    fn note(&mut self, line: &str) {
        self.0.push_str(line);
        self.0.push('\n');
    }

    /// Runs a wrapped command and records its exit code and the start of its output, or why
    /// it never started. Inputs: the case name and the wrapped command or its refusal.
    fn ran(&mut self, name: &str, wrapped: Result<Command, SandboxError>) {
        let line = match wrapped {
            Err(refused) => format!("{name}: wrap refused {refused:?}"),
            Ok(mut command) => {
                let started = std::time::Instant::now();
                let ran = crate::spawn_gate::collected(command.stdin(Stdio::null()));
                let millis = started.elapsed().as_millis();
                match ran {
                    Err(error) => format!("{name}: start failed {error}"),
                    Ok(out) => format!(
                        "{name}: {millis} ms rc={:?} out={:?} err={:?}",
                        out.status.code(),
                        clip(&out.stdout),
                        clip(&out.stderr)
                    ),
                }
            }
        };
        self.note(&line);
    }
}

/// The first 600 characters of a process output, as text. Input: the bytes. Output: the text.
fn clip(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(600).collect()
}

/// Runs the engine's own git in `folder`, failing the probe when it fails. Inputs: git, the
/// folder and the arguments. Output: what it printed.
fn plain(git: &Path, folder: &Path, args: &[&str]) -> String {
    let out = crate::spawn_gate::collected(
        Command::new(git)
            .args(["-c", "user.name=p", "-c", "user.email=p@p.invalid"])
            .args(args)
            .current_dir(folder)
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Where one step git runs and what it names: the start folder, `GIT_DIR`, the work tree,
/// the index and the null device spelling for its settings.
struct Step<'a> {
    git: &'a Path,
    start: &'a Path,
    git_dir: &'a OsStr,
    work: &'a Path,
    index: &'a Path,
    null: &'a str,
}

impl Step<'_> {
    /// The git command for `args`, built as a Windows step would build it. Input: the
    /// arguments. Output: the unwrapped command.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(self.git);
        command
            .current_dir(self.start)
            .env("GIT_DIR", self.git_dir)
            .env("GIT_WORK_TREE", self.work)
            .env("GIT_INDEX_FILE", self.index)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C")
            .arg("-c")
            .arg(format!("core.attributesFile={}", self.null))
            .arg("-c")
            .arg(format!("core.excludesFile={}", self.null))
            .args(args);
        command
    }
}

/// Installs one step policy, or records why not. Inputs: the facts, the case name, the
/// policy, the helper and the folder for profiles and grants. Output: the installed sandbox.
fn installed(
    facts: &mut Facts,
    name: &str,
    policy: Result<ProjectSandbox, SandboxError>,
    helper: &Path,
    place: &Path,
) -> Option<WindowsSandbox> {
    let profile = place.join(format!("{name}.sb"));
    match policy.and_then(|policy| {
        WindowsSandbox::install(policy, helper, &profile, &place.join(GRANTS_FOLDER))
    }) {
        Ok(sandbox) => {
            facts.note(&format!(
                "{name}: installed {}",
                sandbox.sandbox().project_root().display()
            ));
            Some(sandbox)
        }
        Err(refused) => {
            facts.note(&format!("{name}: install refused {refused:?}"));
            None
        }
    }
}

/// W5b D2 probe (PLAN-1336, #746 C1-C5): takes the Windows facts the confined landing step
/// rests on, through the real ProjectSandbox, WindowsSandbox::install and wrap path, and writes
/// them to `w5b-probe.txt` in the runner's temp folder. It asserts nothing about the answers.
#[test]
#[ignore = "probe: needs unc-share-setup.ps1's device entries on an elevated runner (test farm)"]
fn w5b_step_probe() {
    let mut facts = Facts(String::new());
    let temp = tempfile::tempdir().unwrap();
    let root = canonical(temp.path()).unwrap();
    let found =
        crate::github::git_program(std::env::var_os("PATH").as_deref()).expect("git on PATH");
    let git = PathBuf::from(found.to_str().unwrap().trim_start_matches(r"\\?\"));
    let install = canonical(install_root(&git).unwrap()).unwrap();
    facts.note(&format!(
        "git found {} run as {} install {}",
        found.display(),
        git.display(),
        install.display()
    ));
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    plain(&git, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    let many = repo.join("many");
    std::fs::create_dir(&many).unwrap();
    for at in 0..2000 {
        std::fs::write(many.join(format!("f{at}.txt")), format!("file {at}\n")).unwrap();
    }
    plain(&git, &repo, &["add", "a.txt", "many"]);
    plain(&git, &repo, &["commit", "-q", "-m", "one"]);
    let work = root.join("wt");
    plain(
        &git,
        &repo,
        &["worktree", "add", "-q", "-b", "w1", work.to_str().unwrap()],
    );
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("marker.txt"), "outside\n").unwrap();
    let common = repo.join(".git");
    let private = common.join("worktrees").join("wt");
    let step = private.join("crazyagent-step");
    for folder in ["", "home", "tmp", "o-home", "o-tmp", "w-home", "w-tmp"] {
        std::fs::create_dir_all(step.join(folder)).unwrap();
    }
    let place = root.join("place");
    std::fs::create_dir_all(place.join(GRANTS_FOLDER)).unwrap();
    farm::own(&root);
    let helper: PathBuf = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .with_file_name(HELPER_NAME);
    let network = || SandboxNetwork::Only(Vec::new());
    let reads = |sandbox: ProjectSandbox| {
        sandbox
            .allow_read(&work)
            .and_then(|sandbox| sandbox.allow_read(&common))
            .and_then(|sandbox| sandbox.allow_read(&install))
    };
    let index_policy = ProjectSandbox::new(&step, &step.join("home"), &step.join("tmp"), network())
        .map(ProjectSandbox::as_job_folder)
        .and_then(reads);
    let objects_policy =
        ProjectSandbox::new(&step, &step.join("o-home"), &step.join("o-tmp"), network())
            .map(ProjectSandbox::as_job_folder)
            .and_then(|sandbox| sandbox.allow_worktree(&common.join("objects")))
            .and_then(reads);
    let work_policy =
        ProjectSandbox::new(&work, &step.join("w-home"), &step.join("w-tmp"), network())
            .and_then(|sandbox| sandbox.allow_read(&common))
            .and_then(|sandbox| sandbox.allow_read(&install))
            .and_then(|sandbox| sandbox.allow_read(&step));
    let index_box = installed(&mut facts, "index", index_policy, &helper, &place);
    let objects_box = installed(&mut facts, "objects", objects_policy, &helper, &place);
    let work_box = installed(&mut facts, "worktree", work_policy, &helper, &place);
    if let (Some(a), Some(b), Some(c)) = (&index_box, &objects_box, &work_box) {
        let name = |sandbox: &WindowsSandbox| {
            super::boundary_profile(sandbox.sandbox(), &place.join(GRANTS_FOLDER))
                .map(|profile| profile.container)
        };
        facts.note(&format!(
            "containers index={:?} objects={:?} worktree={:?}",
            name(a),
            name(b),
            name(c)
        ));
    }
    for folder in [&step, &common.join("objects"), &work, &private] {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        facts.note(&format!("after install {}: {names:?}", folder.display()));
    }
    std::fs::write(work.join("a.txt"), "two\n").unwrap();
    std::fs::write(work.join("u.txt"), "untracked\n").unwrap();
    let index = step.join("index");
    let absolute = Step {
        git: &git,
        start: &step,
        git_dir: private.as_os_str(),
        work: &work,
        index: &index,
        null: "NUL",
    };
    if let Some(sandbox) = &index_box {
        for (name, args) in [
            ("index abs rev-parse", &["rev-parse", "HEAD"][..]),
            ("index abs read-tree", &["read-tree", "HEAD"][..]),
            (
                "index abs refresh",
                &["update-index", "-q", "--refresh"][..],
            ),
            ("index abs diff-files", &["diff-files", "--name-only"][..]),
            (
                "index abs others",
                &["ls-files", "--others", "--exclude-standard"][..],
            ),
            (
                "index abs config-list",
                &["config", "--show-origin", "--list"][..],
            ),
            (
                "index abs hash-object-w",
                &["hash-object", "-w", "a.txt"][..],
            ),
        ] {
            facts.ran(name, sandbox.wrap(&absolute.command(args)));
        }
        let lock = step.join("index.lock");
        facts.note(&format!("index abs leaves index.lock: {}", lock.exists()));
        let relative = Step {
            git_dir: OsStr::new(".."),
            index: &step.join("index-rel"),
            ..absolute
        };
        facts.ran(
            "index rel rev-parse",
            sandbox.wrap(&relative.command(&["rev-parse", "HEAD"])),
        );
        facts.ran(
            "index rel read-tree",
            sandbox.wrap(&relative.command(&["read-tree", "HEAD"])),
        );
        let dev_null = Step {
            null: "/dev/null",
            ..absolute
        };
        facts.ran(
            "index devnull others",
            sandbox.wrap(&dev_null.command(&["ls-files", "--others", "--exclude-standard"])),
        );
        for (name, file) in [
            ("index graft NUL", "NUL"),
            ("index graft devnull", "/dev/null"),
        ] {
            let mut command = absolute.command(&["rev-parse", "HEAD"]);
            command.env("GIT_GRAFT_FILE", file);
            facts.ran(name, sandbox.wrap(&command));
        }
        for (name, file) in [
            ("index config-file NUL", "NUL"),
            ("index config-file devnull", "/dev/null"),
        ] {
            facts.ran(
                name,
                sandbox.wrap(&absolute.command(&["config", "--file", file, "--list"])),
            );
        }
        let mut started_in_work = absolute.command(&["rev-parse", "HEAD"]);
        started_in_work.current_dir(&work);
        facts.ran("index start in work tree", sandbox.wrap(&started_in_work));
        let system = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let mut listed = Command::new(system.join("System32").join("cmd.exe"));
        listed
            .args(["/d", "/c", "set"])
            .current_dir(&step)
            .env("GIT_DIR", &private)
            .env("GIT_INDEX_FILE", &index)
            .env("PROBE_KEPT", "1");
        facts.ran("index cmd set", sandbox.wrap(&listed));
    }
    let empty = step.join("empty-excludes");
    std::fs::write(&empty, b"").unwrap();
    let empty_text = empty.to_str().unwrap().to_owned();
    let entries = |index: &Path| -> usize {
        crate::spawn_gate::collected(
            Command::new(&git)
                .env("GIT_DIR", &private)
                .env("GIT_INDEX_FILE", index)
                .args(["ls-files"])
                .stdin(Stdio::null()),
        )
        .map(|out| out.stdout.iter().filter(|byte| **byte == b'\n').count())
        .unwrap_or(0)
    };
    let system =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let shell = |line: &str| {
        let mut command = Command::new(system.join("System32").join("cmd.exe"));
        command.args(["/d", "/c", line]).current_dir(&step);
        command
    };
    let many = work.join("many");
    let listing = format!("dir /s /b \"{}\"", many.display());
    let reading = format!("findstr /s /m zzz \"{}\\*\"", many.display());
    if let Some(sandbox) = &objects_box {
        for (name, null) in [
            ("timed contained NUL", "NUL"),
            ("timed contained empty", empty_text.as_str()),
        ] {
            let index_t = step.join(format!("index-{}", name.len()));
            let timed = Step {
                index: &index_t,
                null,
                ..absolute
            };
            facts.ran(
                &format!("{name} read-tree"),
                sandbox.wrap(&timed.command(&["read-tree", "HEAD"])),
            );
            facts.ran(
                &format!("{name} add -A"),
                sandbox.wrap(&timed.command(&["add", "-A"])),
            );
            facts.note(&format!("{name} entries {}", entries(&index_t)));
            facts.ran(
                &format!("{name} add -A again"),
                sandbox.wrap(&timed.command(&["add", "-A"])),
            );
        }
        facts.ran("timed contained dir /s", sandbox.wrap(&shell(&listing)));
        facts.ran("timed contained findstr /s", sandbox.wrap(&shell(&reading)));
    }
    let index_u = step.join("index-tu");
    let twin = Step {
        index: &index_u,
        null: &empty_text,
        ..absolute
    };
    facts.ran(
        "timed uncontained read-tree",
        Ok(twin.command(&["read-tree", "HEAD"])),
    );
    facts.ran("timed uncontained add -A", Ok(twin.command(&["add", "-A"])));
    facts.note(&format!("timed uncontained entries {}", entries(&index_u)));
    facts.ran(
        "timed uncontained add -A again",
        Ok(twin.command(&["add", "-A"])),
    );
    facts.ran("timed uncontained dir /s", Ok(shell(&listing)));
    facts.ran("timed uncontained findstr /s", Ok(shell(&reading)));
    let engine = |null: &str, args: &[&str]| {
        let mut command = Command::new(&git);
        command.env_clear();
        for name in ["PATH", "SystemRoot"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_GRAFT_FILE", null)
            .arg("-c")
            .arg(format!("core.attributesFile={null}"))
            .arg("--git-dir")
            .arg(&private)
            .args(args)
            .current_dir(&private);
        command
    };
    for (name, null) in [("NUL", "NUL"), ("empty", empty_text.as_str())] {
        facts.ran(
            &format!("engine {name} check-attr"),
            Ok(engine(
                null,
                &["check-attr", "--source", "HEAD", "-a", "a.txt"],
            )),
        );
        facts.ran(
            &format!("engine {name} rev-list"),
            Ok(engine(null, &["rev-list", "--count", "HEAD"])),
        );
        facts.ran(
            &format!("engine {name} log -p"),
            Ok(engine(
                null,
                &["log", "-1", "-p", "--format=%H", "HEAD", "--", "a.txt"],
            )),
        );
    }
    let junction = work.join("jn");
    let made = crate::spawn_gate::collected(
        Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .stdin(Stdio::null()),
    )
    .unwrap();
    facts.note(&format!("junction made: {:?}", made.status.code()));
    let readable = work.join("jn-git");
    let made = crate::spawn_gate::collected(
        Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&readable)
            .arg(&common)
            .stdin(Stdio::null()),
    )
    .unwrap();
    facts.note(&format!(
        "junction to the readable .git made: {:?}",
        made.status.code()
    ));
    if let Some(sandbox) = &objects_box {
        let objects = Step {
            index: &step.join("index-o"),
            ..absolute
        };
        facts.ran(
            "objects read-tree",
            sandbox.wrap(&objects.command(&["read-tree", "HEAD"])),
        );
        facts.ran(
            "objects hash-object-w",
            sandbox.wrap(&objects.command(&["hash-object", "-w", "a.txt"])),
        );
        let index_o = step.join("index-o");
        let uncontained = |facts: &mut Facts, name: &str, copy: &str| {
            let index = step.join(copy);
            std::fs::copy(&index_o, &index).unwrap();
            let twin = Step {
                index: &index,
                ..absolute
            };
            facts.ran(name, Ok(twin.command(&["add", "-A"])));
        };
        uncontained(&mut facts, "objects add -A uncontained before", "index-u1");
        facts.ran(
            "objects add -A",
            sandbox.wrap(&objects.command(&["add", "-A"])),
        );
        uncontained(&mut facts, "objects add -A uncontained after", "index-u2");
        facts.ran(
            "objects add -A again",
            sandbox.wrap(&objects.command(&["add", "-A"])),
        );
        facts.ran(
            "objects rev-parse after add",
            sandbox.wrap(&objects.command(&["rev-parse", "HEAD"])),
        );
        let mut listed = Command::new(&git);
        listed
            .current_dir(&step)
            .env("GIT_DIR", &private)
            .env("GIT_WORK_TREE", &work)
            .env("GIT_INDEX_FILE", step.join("index-o"))
            .args(["ls-files", "-s"]);
        facts.ran("objects engine ls-files -s", Ok(listed));
        let mut objects_start = objects.command(&["rev-parse", "HEAD"]);
        objects_start.current_dir(common.join("objects"));
        facts.ran("objects start in objects", sandbox.wrap(&objects_start));
    }
    if let Some(sandbox) = &work_box {
        let in_work = Step {
            start: &work,
            ..absolute
        };
        let system = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let shell = |line: &str| {
            let mut command = Command::new(system.join("System32").join("cmd.exe"));
            command.args(["/d", "/c", line]).current_dir(&work);
            command
        };
        let acl = |path: &Path| {
            crate::spawn_gate::collected(Command::new("icacls").arg(path).stdin(Stdio::null()))
                .map(|out| clip(&out.stdout))
                .unwrap_or_default()
        };
        facts.note(&format!("worktree acl before: {}", acl(&work)));
        facts.note(&format!(
            "worktree a.txt acl before: {}",
            acl(&work.join("a.txt"))
        ));
        facts.ran(
            "worktree cmd new file",
            sandbox.wrap(&shell("echo x> probe-new.txt")),
        );
        facts.ran(
            "worktree cmd new file in many",
            sandbox.wrap(&shell("echo x> many\\probe-new.txt")),
        );
        facts.ran(
            "worktree cmd rewrite a.txt",
            sandbox.wrap(&shell("echo y> a.txt")),
        );
        facts.ran(
            "worktree checkout-index",
            sandbox.wrap(&in_work.command(&["checkout-index", "-f", "a.txt"])),
        );
        facts.note(&format!("worktree acl after: {}", acl(&work)));
        facts.note(&format!(
            "worktree a.txt now {:?}",
            std::fs::read_to_string(work.join("a.txt")).ok()
        ));
        facts.ran(
            "worktree read-tree -u",
            sandbox.wrap(&in_work.command(&["read-tree", "-u", "-m", "HEAD"])),
        );
        facts.ran(
            "worktree checkout-index through junction",
            sandbox.wrap(&in_work.command(&["checkout-index", "-f", "--prefix=jn/", "a.txt"])),
        );
        facts.note(&format!(
            "outside a.txt written: {}",
            outside.join("a.txt").exists()
        ));
        let in_step = Step {
            start: &step,
            ..absolute
        };
        facts.ran(
            "worktree start in step",
            sandbox.wrap(&in_step.command(&["rev-parse", "HEAD"])),
        );
    }
    let out = std::env::var_os("RUNNER_TEMP").map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::write(out.join("w5b-probe.txt"), &facts.0).unwrap();
    assert!(facts.0.lines().count() > 10);
}
