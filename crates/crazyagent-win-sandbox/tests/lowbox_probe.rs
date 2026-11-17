#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, Guarded, check_path};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-win-boundary");
const FOLDER_ONLY_WITHOUT_DELETE: &str = "(RD,WD,AD,REA,WEA,X,RA,WA,RC,S)";

fn plain(path: &Path) -> String {
    let text = path.to_str().unwrap().to_owned();
    assert_eq!(check_path(&text), Ok(()), "{text}");
    text
}

fn system(program: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\{program}")
}

fn ran(output: &Output) -> String {
    format!(
        "exit {:?} stdout {:?} stderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn owner(args: &[&str]) -> String {
    let output = Command::new(system("icacls.exe"))
        .args(args)
        .output()
        .unwrap();
    format!("owner icacls {args:?}: {}", ran(&output))
}

fn sddl(path: &Path) -> String {
    let output = Command::new(system(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Acl -LiteralPath $env:CRAZYAGENT_TEST_ACL_PATH).Sddl",
        ])
        .env("CRAZYAGENT_TEST_ACL_PATH", path)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn sddls(paths: &[&Path]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let joined = paths
        .iter()
        .map(|path| plain(path))
        .collect::<Vec<_>>()
        .join("|");
    let output = Command::new(system(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "foreach ($p in $env:CRAZYAGENT_TEST_ACL_PATHS.Split('|')) { try { $p + ' => ' + (Get-Acl -LiteralPath $p).Sddl } catch { $p + ' => ' + $_.Exception.Message } }",
        ])
        .env("CRAZYAGENT_TEST_ACL_PATHS", joined)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn content(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => format!(
            "{:?} readonly {:?}",
            String::from_utf8_lossy(&bytes),
            fs::metadata(path).map(|meta| meta.permissions().readonly())
        ),
        Err(error) => format!("unreadable: {error}"),
    }
}

fn launch(
    profile: &Path,
    cwd: &Path,
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    stdin: Stdio,
) -> Output {
    Command::new(HELPER)
        .arg("--profile")
        .arg(profile)
        .arg("--")
        .arg(program)
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn inside(profile: &Path, project: &Path, args: &[&str]) -> Output {
    launch(
        profile,
        project,
        &system("cmd.exe"),
        args,
        &[],
        Stdio::null(),
    )
}

#[test]
fn lowbox_probe_reports_what_the_container_can_do() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let project = root.join("project");
    let context = project.join("context");
    let shut = project.join("shut");
    let gone = project.join("gone");
    let grants = root.join("grants");
    for folder in [&context, &shut, &gone, &grants] {
        fs::create_dir_all(folder).unwrap();
    }
    let names = ["w_over", "w_app", "w_attr", "w_sort", "w_read", "w_del"];
    for name in names {
        fs::write(context.join(format!("{name}.txt")), "probe\r\n").unwrap();
    }
    let names = [
        "plain_over",
        "plain_del",
        "e_write",
        "e_del",
        "e_read",
        "l_med",
        "l_high",
    ];
    for name in names {
        fs::write(project.join(format!("{name}.txt")), "probe\r\n").unwrap();
    }
    fs::write(shut.join("file.txt"), "probe\r\n").unwrap();
    fs::write(gone.join("secret.txt"), "probe\r\n").unwrap();
    let profile = BoundaryProfile {
        container: "crazyagent.test.probe".into(),
        capabilities: Vec::new(),
        read_roots: Vec::new(),
        write_roots: vec![plain(&project)],
        guarded: vec![Guarded {
            path: plain(&context),
            guard: Guard::ReadOnly,
        }],
        active_processes: 32,
        grants: plain(&grants),
    };
    let profile_path = root.join("agent.profile");
    fs::write(&profile_path, profile.encode().unwrap()).unwrap();
    let mut report = Vec::new();
    let first = inside(
        &profile_path,
        &project,
        &["/C", "echo", "first>", &plain(&project.join("first.txt"))],
    );
    report.push(format!("first launch: {}", ran(&first)));
    let context_sddl = sddl(&context);
    report.push(format!("context after first launch: {context_sddl}"));
    let start = context_sddl.find("S-1-15-2-").expect(&context_sddl);
    let sid: String = context_sddl[start..]
        .chars()
        .take_while(|unit| unit.is_ascii_alphanumeric() || *unit == '-')
        .collect();
    let trustee = format!("*{sid}");
    let path = |folder: &Path, name: &str| plain(&folder.join(name));
    report.push(owner(&[
        &path(&project, "e_write.txt"),
        "/deny",
        &format!("{trustee}:(WD,AD,WEA,WA)"),
    ]));
    report.push(owner(&[
        &path(&project, "e_del.txt"),
        "/deny",
        &format!("{trustee}:(DE)"),
    ]));
    report.push(owner(&[
        &path(&project, "e_read.txt"),
        "/deny",
        &format!("{trustee}:(RD)"),
    ]));
    report.push(owner(&[
        &path(&project, "l_med.txt"),
        "/setintegritylevel",
        "M",
    ]));
    report.push(owner(&[
        &path(&project, "l_high.txt"),
        "/setintegritylevel",
        "H",
    ]));
    report.push(owner(&[&plain(&shut), "/inheritance:d"]));
    report.push(owner(&[&plain(&shut), "/remove:g", &trustee]));
    report.push(owner(&[
        &plain(&shut),
        "/grant",
        &format!("{trustee}:(OI)(CI)(RX)"),
    ]));
    report.push(owner(&[&plain(&gone), "/inheritance:d"]));
    report.push(owner(&[&plain(&gone), "/remove:g", &trustee]));
    let whoami = inside(&profile_path, &project, &["/C", "whoami", "/all"]);
    report.push(format!("whoami: {}", ran(&whoami)));
    let probes: [(&str, &Path, &str, [&str; 3]); 18] = [
        (
            "inherited deny, overwrite",
            &context,
            "w_over.txt",
            ["echo", "over>", ""],
        ),
        (
            "inherited deny, append",
            &context,
            "w_app.txt",
            ["echo", "app>>", ""],
        ),
        (
            "inherited deny, attributes",
            &context,
            "w_attr.txt",
            ["attrib", "+R", ""],
        ),
        (
            "inherited deny, sort output",
            &context,
            "w_sort.txt",
            ["sort", "/O", ""],
        ),
        (
            "inherited deny, read",
            &context,
            "w_read.txt",
            ["type", "", ""],
        ),
        (
            "inherited deny, delete",
            &context,
            "w_del.txt",
            ["del", "/Q", ""],
        ),
        (
            "allow only, overwrite",
            &project,
            "plain_over.txt",
            ["echo", "over>", ""],
        ),
        (
            "allow only, delete",
            &project,
            "plain_del.txt",
            ["del", "/Q", ""],
        ),
        (
            "explicit write deny, overwrite",
            &project,
            "e_write.txt",
            ["echo", "over>", ""],
        ),
        (
            "explicit delete deny, delete",
            &project,
            "e_del.txt",
            ["del", "/Q", ""],
        ),
        (
            "explicit read deny, read",
            &project,
            "e_read.txt",
            ["type", "", ""],
        ),
        (
            "medium label, overwrite",
            &project,
            "l_med.txt",
            ["echo", "over>", ""],
        ),
        (
            "high label, overwrite",
            &project,
            "l_high.txt",
            ["echo", "over>", ""],
        ),
        (
            "protected read only folder, overwrite",
            &shut,
            "file.txt",
            ["echo", "over>", ""],
        ),
        (
            "protected read only folder, read",
            &shut,
            "file.txt",
            ["type", "", ""],
        ),
        (
            "protected read only folder, create",
            &shut,
            "new.txt",
            ["echo", "new>", ""],
        ),
        (
            "protected read only folder, delete",
            &shut,
            "file.txt",
            ["del", "/Q", ""],
        ),
        (
            "protected folder with no entry, read",
            &gone,
            "secret.txt",
            ["type", "", ""],
        ),
    ];
    for (label, folder, name, words) in probes {
        let target = folder.join(name);
        let target_text = plain(&target);
        let mut args = vec!["/C"];
        args.extend(words.iter().copied().filter(|word| !word.is_empty()));
        args.push(&target_text);
        let output = inside(&profile_path, &project, &args);
        report.push(format!(
            "{label} [{args:?}]: {} | now {} | {}",
            ran(&output),
            content(&target),
            sddl(&target)
        ));
    }
    for folder in [&project, &context, &shut, &gone] {
        report.push(format!("{} => {}", folder.display(), sddl(folder)));
    }
    panic!(
        "lowbox probe report, diagnostic only\n{}",
        report.join("\n")
    );
}

/// A container with one writable root and no protected path, so the helper puts no deny entry
/// anywhere, plus a lab folder outside every root that the helper never touches. The owner
/// sets the lab's access lists by hand for the container SID, read from the root after the
/// first launch, and every probe's result goes into one report.
struct Lab {
    _temp: tempfile::TempDir,
    project: PathBuf,
    lab: PathBuf,
    profile: PathBuf,
    sid: String,
    env: Vec<(String, String)>,
    report: Vec<String>,
}

/// Makes a lab for one container: its root, the lab folder and the grants folder, writes the
/// profile, launches once so the root carries the container's entry, and reads the container
/// SID from that entry. Input: the container name. Output: the lab.
fn lab(container: &str) -> Lab {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let project = root.join("project");
    let lab = root.join("lab");
    let grants = root.join("grants");
    for folder in [&project, &lab, &grants] {
        fs::create_dir_all(folder).unwrap();
    }
    let profile = BoundaryProfile {
        container: container.into(),
        capabilities: Vec::new(),
        read_roots: Vec::new(),
        write_roots: vec![plain(&project)],
        guarded: Vec::new(),
        active_processes: 32,
        grants: plain(&grants),
    };
    let profile_path = root.join("agent.profile");
    fs::write(&profile_path, profile.encode().unwrap()).unwrap();
    let first = inside(
        &profile_path,
        &project,
        &["/C", "echo", "first>", &plain(&project.join("first.txt"))],
    );
    let project_sddl = sddl(&project);
    let start = project_sddl
        .find("S-1-15-2-")
        .unwrap_or_else(|| panic!("no container entry: {}\n{project_sddl}", ran(&first)));
    let sid: String = project_sddl[start..]
        .chars()
        .take_while(|unit| unit.is_ascii_alphanumeric() || *unit == '-')
        .collect();
    assert!(sid.len() > "S-1-15-2-".len(), "{project_sddl}");
    Lab {
        _temp: temp,
        project,
        lab,
        profile: profile_path,
        report: vec![
            format!("first launch: {}", ran(&first)),
            format!("container {sid}"),
        ],
        sid,
        env: Vec::new(),
    }
}

impl Lab {
    fn owner(&mut self, args: &[&str]) {
        self.report.push(owner(args));
    }

    fn grant(&mut self, path: &Path, rights: &str) {
        let entry = format!("*{}:{rights}", self.sid);
        self.owner(&[&plain(path), "/grant", &entry]);
    }

    fn cut(&mut self, path: &Path) {
        let trustee = format!("*{}", self.sid);
        self.owner(&[&plain(path), "/inheritance:d"]);
        self.owner(&[&plain(path), "/remove:g", &trustee]);
    }

    fn me(&self, rights: &str) -> String {
        format!("*{}:{rights}", self.sid)
    }

    fn exe(&mut self, label: &str, program: &str, args: &[&str], watched: &[&Path]) {
        let project = self.project.clone();
        self.run(label, &project, false, program, args, watched);
    }

    fn run(
        &mut self,
        label: &str,
        cwd: &Path,
        piped: bool,
        program: &str,
        args: &[&str],
        watched: &[&Path],
    ) {
        let before = sddls(watched);
        let stdin = if piped { Stdio::piped() } else { Stdio::null() };
        let output = launch(&self.profile, cwd, program, args, &self.env, stdin);
        let after = sddls(watched);
        let now = watched
            .iter()
            .filter(|path| !path.is_dir())
            .map(|path| format!("{} {}", path.display(), content(path)))
            .collect::<Vec<_>>()
            .join("; ");
        self.report.push(format!(
            "## {label}\n  args {args:?} cwd {} piped stdin {piped}\n  {}\n  before: {before}\n  after: {after}\n  now: {now}",
            cwd.display(),
            ran(&output)
        ));
    }

    fn cmd(&mut self, label: &str, args: &[&str], watched: &[&Path]) {
        let project = self.project.clone();
        self.cmd_in(label, &project, args, watched);
    }

    fn cmd_in(&mut self, label: &str, cwd: &Path, args: &[&str], watched: &[&Path]) {
        let mut line = vec!["/C"];
        line.extend_from_slice(args);
        self.run(label, cwd, false, &system("cmd.exe"), &line, watched);
    }

    fn git(&mut self) -> Option<String> {
        let exec = Command::new("git").arg("--exec-path").output().unwrap();
        self.report
            .push(format!("owner git --exec-path: {}", ran(&exec)));
        let git = PathBuf::from(
            String::from_utf8_lossy(&exec.stdout)
                .trim()
                .replace('/', "\\"),
        )
        .join("git.exe");
        git.is_file().then(|| plain(&git))
    }

    fn finish(self, title: &str) -> ! {
        panic!("{title}, diagnostic only\n{}", self.report.join("\n"));
    }
}

#[test]
fn lowbox_probe_whether_the_container_can_change_an_access_list() {
    let mut lab = lab("crazyagent.test.selfgrant");
    let full = lab.lab.join("full");
    let rx = lab.lab.join("rx");
    let rx_folder = lab.lab.join("rxfolder");
    let none = lab.lab.join("none");
    let parent = lab.lab.join("parent");
    let cut = parent.join("cut");
    for folder in [&full, &rx, &rx_folder, &none, &cut] {
        fs::create_dir_all(folder).unwrap();
    }
    let control = full.join("control.txt");
    let own = rx.join("self.txt");
    let take = rx.join("take.txt");
    let made = rx_folder.join("made.txt");
    let bare = none.join("bare.txt");
    let inner = cut.join("inner.txt");
    for file in [&control, &own, &take, &bare, &inner] {
        fs::write(file, "probe\r\n").unwrap();
    }
    lab.grant(&full, "(OI)(CI)(F)");
    lab.grant(&rx, "(OI)(CI)(RX)");
    lab.grant(&rx_folder, "(OI)(CI)(RX)");
    lab.grant(&parent, "(OI)(CI)(M)");
    lab.cut(&cut);
    lab.grant(&cut, "(OI)(CI)(RX)");
    lab.report.push(format!(
        "after setup:\n{}",
        sddls(&[
            &full, &rx, &rx_folder, &none, &parent, &cut, &own, &bare, &inner
        ])
    ));
    let icacls = system("icacls.exe");
    let takeown = system("takeown.exe");
    lab.exe(
        "control: grant LOCAL SERVICE read on a file the container holds F on, expect allowed",
        &icacls,
        &[&plain(&control), "/grant", "*S-1-5-19:(R)"],
        &[&control],
    );
    lab.cmd(
        "control: overwrite that file, expect allowed",
        &["echo", "over>", &plain(&control)],
        &[&control],
    );
    let entry = lab.me("(F)");
    lab.exe(
        "self-grant F on a read-only file, expect refused",
        &icacls,
        &[&plain(&own), "/grant", &entry],
        &[&own],
    );
    lab.cmd(
        "then overwrite it, expect refused",
        &["echo", "over>", &plain(&own)],
        &[&own],
    );
    let entry = lab.me("(OI)(CI)(F)");
    lab.exe(
        "self-grant F on a read-only folder, expect refused",
        &icacls,
        &[&plain(&rx_folder), "/grant", &entry],
        &[&rx_folder],
    );
    lab.cmd(
        "then create a file in it, expect refused",
        &["echo", "new>", &plain(&made)],
        &[&made],
    );
    let entry = lab.me("(F)");
    lab.exe(
        "self-grant F on a file with no container entry (owner rights only), expect refused",
        &icacls,
        &[&plain(&bare), "/grant", &entry],
        &[&bare],
    );
    lab.cmd(
        "then read it, expect refused",
        &["type", &plain(&bare)],
        &[&bare],
    );
    lab.exe(
        "turn inheritance back on for a cut folder, expect refused",
        &icacls,
        &[&plain(&cut), "/inheritance:e"],
        &[&cut, &inner],
    );
    lab.cmd(
        "then overwrite a file in it, expect refused",
        &["echo", "over>", &plain(&inner)],
        &[&inner],
    );
    lab.exe(
        "take ownership of a read-only file, expect refused",
        &takeown,
        &["/F", &plain(&take)],
        &[&take],
    );
    let entry = lab.me("(F)");
    lab.exe(
        "then self-grant F on it, expect refused",
        &icacls,
        &[&plain(&take), "/grant", &entry],
        &[&take],
    );
    lab.cmd(
        "then overwrite it, expect refused",
        &["echo", "over>", &plain(&take)],
        &[&take],
    );
    lab.cmd(
        "privileges of the container token",
        &["whoami", "/priv"],
        &[],
    );
    lab.finish("lowbox probe (a): can the container change an access list")
}

#[test]
fn lowbox_probe_a_folder_without_delete_on_its_own_entry() {
    let mut lab = lab("crazyagent.test.split");
    let parent = lab.lab.join("parent");
    let split = parent.join("split");
    let open = parent.join("open");
    let sub = split.join("sub");
    for folder in [&sub, &open] {
        fs::create_dir_all(folder).unwrap();
    }
    let keep = split.join("keep.txt");
    let over = split.join("over.txt");
    for file in [&keep, &over, &sub.join("inner.txt")] {
        fs::write(file, "probe\r\n").unwrap();
    }
    lab.grant(&parent, "(OI)(CI)(M)");
    lab.cut(&split);
    lab.grant(&split, FOLDER_ONLY_WITHOUT_DELETE);
    lab.grant(&split, "(OI)(CI)(IO)(M)");
    lab.report.push(format!(
        "after setup:\n{}",
        sddls(&[&parent, &split, &open, &sub, &keep])
    ));
    let made = split.join("new.txt");
    let folder = split.join("made");
    let lock = split.join("a.lock");
    lab.cmd(
        "control: rename a folder that inherits modify, expect allowed",
        &["ren", &plain(&open), "open-moved"],
        &[&parent],
    );
    lab.cmd(
        "rename the split folder, expect refused",
        &["ren", &plain(&split), "split-moved"],
        &[&split],
    );
    lab.cmd(
        "make a file in it, expect allowed",
        &["echo", "new>", &plain(&made)],
        &[&made],
    );
    lab.cmd(
        "delete that file, expect allowed",
        &["del", "/Q", &plain(&made)],
        &[&made],
    );
    lab.cmd(
        "make a folder in it, expect allowed",
        &["mkdir", &plain(&folder)],
        &[&folder],
    );
    lab.cmd(
        "remove that folder, expect allowed",
        &["rd", &plain(&folder)],
        &[&split],
    );
    lab.cmd(
        "rename a file in it, expect allowed",
        &["ren", &plain(&keep), "kept.txt"],
        &[&split],
    );
    lab.cmd(
        "make a lock file, expect allowed",
        &["echo", "lock>", &plain(&lock)],
        &[&lock],
    );
    lab.cmd(
        "rename the lock over a file, expect allowed",
        &["move", "/Y", &plain(&lock), &plain(&over)],
        &[&lock, &over],
    );
    lab.cmd(
        "rename the sub folder, expect allowed",
        &["ren", &plain(&sub), "sub-moved"],
        &[&split],
    );
    let entry = lab.me("(OI)(CI)(F)");
    lab.exe(
        "self-grant F on the split folder, expect refused",
        &system("icacls.exe"),
        &[&plain(&split), "/grant", &entry],
        &[&split],
    );
    lab.cmd(
        "rename the split folder again, expect refused",
        &["ren", &plain(&split), "split-moved"],
        &[&split],
    );
    lab.finish("lowbox probe (b): a folder without delete on its own entry")
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Anchor,
    FolderOnly,
    FolderOnlyFiles,
}

/// Builds a `.git` tree with hooks, objects, refs, logs, config, index and HEAD, and a
/// `.crazyagent\context` folder, in a lab folder the container may modify, and sets one
/// candidate layout on it. Anchor: `.git` keeps modify for its children; FolderOnly: option
/// (d), explicit grants only on git's write places; FolderOnlyFiles: (d) plus modify inherited
/// by new files. Inputs: the lab and the layout. Output: the work folder holding `.git`.
fn git_tree(lab: &mut Lab, layout: Layout) -> PathBuf {
    let work = lab.lab.join("work");
    let git = work.join(".git");
    let hooks = git.join("hooks");
    let objects = git.join("objects");
    let refs = git.join("refs");
    let logs = git.join("logs");
    let crazyagent = work.join(".crazyagent");
    let context = crazyagent.join("context");
    for folder in [
        &hooks,
        &objects.join("pack"),
        &refs.join("heads"),
        &logs,
        &context,
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    let config = git.join("config");
    let index = git.join("index");
    let head = git.join("HEAD");
    let main = refs.join("heads").join("main");
    let log = logs.join("HEAD");
    let sample = hooks.join("pre-commit.sample");
    for (file, text) in [
        (&config, "[core]\n\tbare = false\n"),
        (&index, "index\n"),
        (&head, "ref: refs/heads/main\n"),
        (&main, "main\n"),
        (&log, "log\n"),
        (&sample, "exit 0\n"),
    ] {
        fs::write(file, text).unwrap();
    }
    lab.grant(&work, "(OI)(CI)(M)");
    lab.cut(&git);
    lab.grant(&git, FOLDER_ONLY_WITHOUT_DELETE);
    match layout {
        Layout::Anchor => lab.grant(&git, "(OI)(CI)(IO)(M)"),
        Layout::FolderOnly | Layout::FolderOnlyFiles => {
            for folder in [&objects, &refs, &logs] {
                lab.grant(folder, "(OI)(CI)(M)");
            }
            for file in [&index, &head] {
                lab.grant(file, "(M)");
            }
            if matches!(layout, Layout::FolderOnlyFiles) {
                lab.grant(&git, "(OI)(IO)(M)");
            }
        }
    }
    lab.cut(&hooks);
    lab.grant(&hooks, "(OI)(CI)(RX)");
    lab.cut(&config);
    lab.grant(&config, "(RX)");
    lab.cut(&context);
    lab.grant(&context, "(OI)(CI)(RX)");
    lab.report.push(format!(
        "layout {layout:?} after setup:\n{}",
        sddls(&[
            &work, &git, &hooks, &sample, &config, &index, &head, &objects, &refs, &main, &context
        ])
    ));
    work
}

/// Builds the `.git` tree in one candidate layout, then runs git's write steps as cmd
/// builtins, the guarded-path refusals, the config rename the owner's git makes (finding F1),
/// and the staged instructions move, and panics with the report. Inputs: the container name and
/// the layout.
fn git_layout(container: &str, layout: Layout) -> ! {
    let mut lab = lab(container);
    let work = git_tree(&mut lab, layout);
    let git = work.join(".git");
    let hooks = git.join("hooks");
    let objects = git.join("objects");
    let refs = git.join("refs");
    let logs = git.join("logs");
    let crazyagent = work.join(".crazyagent");
    let context = crazyagent.join("context");
    let config = git.join("config");
    let index = git.join("index");
    let head = git.join("HEAD");
    let main = refs.join("heads").join("main");
    let log = logs.join("HEAD");
    let sample = hooks.join("pre-commit.sample");
    let index_lock = git.join("index.lock");
    let head_lock = git.join("HEAD.lock");
    let main_lock = refs.join("heads").join("main.lock");
    let object_folder = objects.join("ab");
    let object = object_folder.join("cd");
    let rebase = git.join("rebase-merge");
    let step = rebase.join("done");
    let config_lock = git.join("config.lock");
    let hook = hooks.join("post-checkout");
    lab.cmd(
        "make index.lock, expect allowed",
        &["echo", "staged>", &plain(&index_lock)],
        &[&index_lock],
    );
    lab.cmd(
        "rename index.lock over index, expect allowed",
        &["move", "/Y", &plain(&index_lock), &plain(&index)],
        &[&index_lock, &index],
    );
    lab.cmd(
        "make HEAD.lock, expect allowed",
        &["echo", "moved>", &plain(&head_lock)],
        &[&head_lock],
    );
    lab.cmd(
        "rename HEAD.lock over HEAD, expect allowed",
        &["move", "/Y", &plain(&head_lock), &plain(&head)],
        &[&head_lock, &head],
    );
    lab.cmd(
        "make refs\\heads\\main.lock, expect allowed",
        &["echo", "next>", &plain(&main_lock)],
        &[&main_lock],
    );
    lab.cmd(
        "rename it over main, expect allowed",
        &["move", "/Y", &plain(&main_lock), &plain(&main)],
        &[&main_lock, &main],
    );
    lab.cmd(
        "make an object folder, expect allowed",
        &["mkdir", &plain(&object_folder)],
        &[&object_folder],
    );
    lab.cmd(
        "write an object, expect allowed",
        &["echo", "object>", &plain(&object)],
        &[&object],
    );
    lab.cmd(
        "append to logs\\HEAD, expect allowed",
        &["echo", "entry>>", &plain(&log)],
        &[&log],
    );
    lab.cmd(
        "make .git\\rebase-merge, expect allowed",
        &["mkdir", &plain(&rebase)],
        &[&rebase],
    );
    lab.cmd(
        "write a file in it, expect allowed",
        &["echo", "step>", &plain(&step)],
        &[&step],
    );
    lab.cmd(
        "remove .git\\rebase-merge, expect allowed",
        &["rd", "/S", "/Q", &plain(&rebase)],
        &[&git],
    );
    lab.cmd(
        "create a hook, expect refused",
        &["echo", "hook>", &plain(&hook)],
        &[&hook],
    );
    lab.cmd(
        "delete a hook, expect refused",
        &["del", "/F", "/Q", &plain(&sample)],
        &[&sample],
    );
    lab.cmd(
        "rename .git\\hooks, expect refused",
        &["ren", &plain(&hooks), "hooks-moved"],
        &[&hooks],
    );
    lab.cmd(
        "read config, expect allowed",
        &["type", &plain(&config)],
        &[&config],
    );
    lab.cmd(
        "append to config, expect refused",
        &["echo", "changed>>", &plain(&config)],
        &[&config],
    );
    lab.cmd(
        "D6: make config.lock, expect allowed",
        &["echo", "lock>", &plain(&config_lock)],
        &[&config_lock],
    );
    lab.cmd(
        "D6: rename config.lock over config, expect refused",
        &["move", "/Y", &plain(&config_lock), &plain(&config)],
        &[&config_lock, &config],
    );
    lab.cmd(
        "D6: delete config, expect refused",
        &["del", "/F", "/Q", &plain(&config)],
        &[&config],
    );
    let _ = fs::remove_file(&config_lock);
    fs::write(&config_lock, "[core]\n\tbare = false\n\towner = 1\n").unwrap();
    let moved = Command::new(system("cmd.exe"))
        .args(["/C", "move", "/Y", &plain(&config_lock), &plain(&config)])
        .output()
        .unwrap();
    lab.report.push(format!(
        "F1: the owner renamed a new config.lock over config: {}\n  {}",
        ran(&moved),
        sddls(&[&config])
    ));
    lab.cmd(
        "F1: read config after the owner's rename, expect allowed",
        &["type", &plain(&config)],
        &[&config],
    );
    lab.cmd(
        "F1: append to config after the owner's rename, expect refused",
        &["echo", "changed>>", &plain(&config)],
        &[&config],
    );
    let staged = context.join("instructions.md.new");
    let instructions = crazyagent.join("instructions.md");
    fs::write(&staged, "instructions\n").unwrap();
    fs::rename(&staged, &instructions).unwrap();
    lab.report.push(format!(
        "the owner staged instructions.md in the cut context folder and moved it out:\n  {}",
        sddls(&[&instructions])
    ));
    lab.cmd(
        "read the moved instructions.md, expect allowed",
        &["type", &plain(&instructions)],
        &[&instructions],
    );
    lab.cmd(
        "append to the moved instructions.md, expect refused",
        &["echo", "changed>>", &plain(&instructions)],
        &[&instructions],
    );
    lab.cmd(
        "rename .git, expect refused",
        &["ren", &plain(&git), "git-moved"],
        &[&git],
    );
    lab.finish(&format!("lowbox probe (c)(d): git layout {layout:?}"))
}

#[test]
fn lowbox_probe_git_layout_anchor() {
    git_layout("crazyagent.test.gitanchor", Layout::Anchor)
}

#[test]
fn lowbox_probe_git_layout_folder_only() {
    git_layout("crazyagent.test.gitfolder", Layout::FolderOnly)
}

#[test]
fn lowbox_probe_git_layout_folder_only_files() {
    git_layout("crazyagent.test.gitfiles", Layout::FolderOnlyFiles)
}

#[test]
fn lowbox_probe_real_git_in_a_container() {
    let mut lab = lab("crazyagent.test.realgit");
    let Some(git) = lab.git() else {
        lab.finish("no git.exe beside the owner's git exec path");
    };
    let home = lab.lab.join("home");
    let control = lab.lab.join("control");
    let guarded = lab.lab.join("guarded");
    fs::create_dir_all(&home).unwrap();
    let identity = [
        "-c",
        "user.name=probe",
        "-c",
        "user.email=probe@example.invalid",
    ];
    for repo in [&control, &guarded] {
        let init = Command::new(&git)
            .args(["init", "-q", "-b", "main", &plain(repo)])
            .output()
            .unwrap();
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        let add = Command::new(&git)
            .args(["-C", &plain(repo), "add", "a.txt"])
            .output()
            .unwrap();
        let commit = Command::new(&git)
            .args(["-C", &plain(repo)])
            .args(identity)
            .args(["commit", "-q", "-m", "base"])
            .output()
            .unwrap();
        lab.report.push(format!(
            "owner repo {}: init {} | add {} | commit {}",
            repo.display(),
            ran(&init),
            ran(&add),
            ran(&commit)
        ));
    }
    for folder in [&home, &control, &guarded] {
        lab.grant(folder, "(OI)(CI)(M)");
    }
    let dot = guarded.join(".git");
    lab.cut(&dot);
    lab.grant(&dot, FOLDER_ONLY_WITHOUT_DELETE);
    lab.grant(&dot, "(OI)(CI)(IO)(M)");
    lab.cut(&dot.join("hooks"));
    lab.grant(&dot.join("hooks"), "(OI)(CI)(RX)");
    lab.cut(&dot.join("config"));
    lab.grant(&dot.join("config"), "(RX)");
    lab.env = git_env(&home);
    for (name, repo) in [("control", &control), ("anchor layout", &guarded)] {
        let path = plain(repo);
        let work = repo.join("w.txt");
        let config = repo.join(".git").join("config");
        let index = repo.join(".git").join("index");
        lab.cmd(
            &format!("{name}: write a work file, expect allowed"),
            &["echo", "work>", &plain(&work)],
            &[&work],
        );
        for step in [
            &["status", "--short"][..],
            &["add", "w.txt"],
            &["commit", "-q", "-m", "probe"],
            &["checkout", "-q", "-b", "probe"],
            &["checkout", "-q", "main"],
            &["log", "--oneline", "-3"],
            &["config", "core.hooksPath", "hooks-elsewhere"],
        ] {
            let mut args = vec!["-C", path.as_str()];
            args.extend(step.iter().copied());
            lab.exe(
                &format!("{name}: git {}", step.join(" ")),
                &git,
                &args,
                &[&config, &index],
            );
        }
    }
    lab.finish("lowbox probe: real git in a container, report only")
}

fn git_env(home: &Path) -> Vec<(String, String)> {
    [
        ("HOME", plain(home)),
        ("GIT_CONFIG_NOSYSTEM", "1".to_owned()),
        ("GIT_CONFIG_COUNT", "1".to_owned()),
        ("GIT_CONFIG_KEY_0", "safe.directory".to_owned()),
        ("GIT_CONFIG_VALUE_0", "*".to_owned()),
        ("GIT_AUTHOR_NAME", "probe".to_owned()),
        ("GIT_AUTHOR_EMAIL", "probe@example.invalid".to_owned()),
        ("GIT_COMMITTER_NAME", "probe".to_owned()),
        ("GIT_COMMITTER_EMAIL", "probe@example.invalid".to_owned()),
        ("GIT_TERMINAL_PROMPT", "0".to_owned()),
    ]
    .map(|(key, value)| (key.to_owned(), value))
    .to_vec()
}

/// Step 0b, B1: which parent folder lets the container delete or rename. Five parents, each
/// holding owner-made and container-made files and a container-made sub folder: one the owner
/// made, one the container made, one the owner made with a Low label, one the owner made that
/// grants the container DELETE_CHILD, and the root the helper grants. Every operation runs by
/// absolute path, then the owner-made parent and the root again by relative path from inside.
#[test]
fn lowbox_probe_b1_what_lets_the_container_delete_or_rename() {
    let mut lab = lab("crazyagent.test.b1");
    let parent = lab.lab.join("pd");
    fs::create_dir_all(&parent).unwrap();
    lab.grant(&parent, "(OI)(CI)(M)");
    let made = parent.join("made");
    lab.cmd(
        "the container makes a folder",
        &["mkdir", &plain(&made)],
        &[&made],
    );
    let owner_made = parent.join("owner");
    let low = parent.join("low");
    let child = parent.join("child");
    for folder in [&owner_made, &low, &child] {
        fs::create_dir_all(folder).unwrap();
    }
    lab.owner(&[&plain(&low), "/setintegritylevel", "(OI)(CI)L"]);
    lab.grant(&child, "(DC)");
    let project = lab.project.clone();
    let cases = [
        ("owner-made parent", &owner_made),
        ("container-made parent", &made),
        ("owner-made parent with a Low label", &low),
        ("owner-made parent granting DELETE_CHILD", &child),
        ("the helper-granted root", &project),
    ];
    for (name, folder) in cases {
        for file in [
            "o_del.txt",
            "o_ren.txt",
            "o_over.txt",
            "r_del.txt",
            "r_ren.txt",
        ] {
            fs::write(folder.join(file), "owner\r\n").unwrap();
        }
        for file in ["c_del.txt", "c_ren.txt", "c_lock.txt", "r_lock.txt"] {
            lab.cmd(
                &format!("{name}: the container makes {file}"),
                &["echo", "made>", &plain(&folder.join(file))],
                &[],
            );
        }
        lab.cmd(
            &format!("{name}: the container makes a sub folder"),
            &["mkdir", &plain(&folder.join("c_sub"))],
            &[],
        );
        lab.owner(&[&plain(folder)]);
        lab.owner(&[&plain(&folder.join("o_del.txt"))]);
        lab.owner(&[&plain(&folder.join("c_del.txt"))]);
    }
    lab.report.push(format!(
        "after setup:\n{}",
        sddls(&[&parent, &owner_made, &made, &low, &child, &project])
    ));
    for (name, folder) in cases {
        let at = |file: &str| folder.join(file);
        lab.cmd(
            &format!("{name}: delete an owner file by absolute path"),
            &["del", "/Q", &plain(&at("o_del.txt"))],
            &[&at("o_del.txt")],
        );
        lab.cmd(
            &format!("{name}: delete a container file by absolute path"),
            &["del", "/Q", &plain(&at("c_del.txt"))],
            &[&at("c_del.txt")],
        );
        lab.cmd(
            &format!("{name}: rename an owner file by absolute path"),
            &["ren", &plain(&at("o_ren.txt")), "o_moved.txt"],
            &[&at("o_ren.txt")],
        );
        lab.cmd(
            &format!("{name}: rename a container file by absolute path"),
            &["ren", &plain(&at("c_ren.txt")), "c_moved.txt"],
            &[&at("c_ren.txt")],
        );
        lab.cmd(
            &format!("{name}: rename a container lock over an owner file by absolute path"),
            &[
                "move",
                "/Y",
                &plain(&at("c_lock.txt")),
                &plain(&at("o_over.txt")),
            ],
            &[&at("c_lock.txt"), &at("o_over.txt")],
        );
        lab.cmd(
            &format!("{name}: rename a container sub folder by absolute path"),
            &["ren", &plain(&at("c_sub")), "c_sub_moved"],
            &[&at("c_sub")],
        );
    }
    for (name, folder) in [cases[0], cases[4]] {
        let at = |file: &str| folder.join(file);
        lab.cmd_in(
            &format!("{name}: delete an owner file by relative path"),
            folder,
            &["del", "/Q", "r_del.txt"],
            &[&at("r_del.txt")],
        );
        lab.cmd_in(
            &format!("{name}: rename an owner file by relative path"),
            folder,
            &["ren", "r_ren.txt", "r_moved.txt"],
            &[&at("r_ren.txt")],
        );
        lab.cmd_in(
            &format!("{name}: rename a container lock over a file by relative path"),
            folder,
            &["move", "/Y", "r_lock.txt", "r_moved.txt"],
            &[&at("r_lock.txt"), &at("r_moved.txt")],
        );
    }
    lab.finish("lowbox probe step 0b B1: what lets the container delete or rename")
}

/// Step 0b, B2: whether the container opens the NUL device, and whether git starts in it with a
/// null stdin and with a real stdin pipe.
#[test]
fn lowbox_probe_b2_whether_the_container_opens_nul_and_git_starts() {
    let mut lab = lab("crazyagent.test.b2");
    let copy = lab.project.join("from-nul.txt");
    lab.cmd("read NUL, expect allowed", &["type", "nul"], &[]);
    lab.cmd("write NUL, expect allowed", &["echo", "x>nul"], &[]);
    lab.cmd(
        "copy NUL to a file, expect allowed",
        &["copy", "/Y", "nul", &plain(&copy)],
        &[&copy],
    );
    lab.cmd(
        "read the NUL device path, expect allowed",
        &["type", r"\\.\NUL"],
        &[],
    );
    let Some(git) = lab.git() else {
        lab.finish("no git.exe beside the owner's git exec path");
    };
    let home = lab.project.join("home");
    fs::create_dir_all(&home).unwrap();
    lab.env = git_env(&home);
    let project = lab.project.clone();
    let repo = plain(&project.join("repo"));
    for piped in [false, true] {
        let stdin = if piped { "piped" } else { "null" };
        lab.run(
            &format!("git --version, {stdin} stdin"),
            &project,
            piped,
            &git,
            &["--version"],
            &[],
        );
        lab.run(
            &format!("git init, {stdin} stdin"),
            &project,
            piped,
            &git,
            &["init", "-q", &repo],
            &[],
        );
    }
    lab.finish("lowbox probe step 0b B2: NUL and git start in the container")
}

const CHILD_STEPS: &str = "CRAZYAGENT_PROBE_STEPS";

/// Step 0c: the probe binary launched inside the container runs only this test, with one file
/// call per line of CRAZYAGENT_PROBE_STEPS (`label|call|path|path`), and prints each call's raw
/// result on a STEP line. In a normal test run the variable is unset and the test does nothing.
#[test]
fn lowbox_probe_child() {
    let Ok(steps) = std::env::var(CHILD_STEPS) else {
        return;
    };
    for line in steps.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        assert!(parts.len() >= 3, "{line}");
        println!("STEP {} => {}", parts.join(" "), call(&parts[1..]));
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn last_error() -> String {
    format!("error {}", unsafe {
        windows_sys::Win32::Foundation::GetLastError()
    })
}

/// Makes one file call the way git and other tools do and returns its raw result. Input: the
/// call name and its paths. Output: "ok", the Win32 error, or std's raw OS error.
fn call(parts: &[&str]) -> String {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, DeleteFileW, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_READ_ATTRIBUTES, FileDispositionInfo,
        FileDispositionInfoEx, MOVEFILE_REPLACE_EXISTING, MoveFileExW, RemoveDirectoryW,
        SYNCHRONIZE,
    };
    let done = |ok: bool| if ok { "ok".to_owned() } else { last_error() };
    match parts {
        ["delete", path] => done(unsafe { DeleteFileW(wide(path).as_ptr()) } != 0),
        ["move", from, to] => done(
            unsafe {
                MoveFileExW(
                    wide(from).as_ptr(),
                    wide(to).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING,
                )
            } != 0,
        ),
        ["rmdir", path] => done(unsafe { RemoveDirectoryW(wide(path).as_ptr()) } != 0),
        ["std-remove", path] => {
            format!("{:?}", fs::remove_file(path).map_err(|e| e.raw_os_error()))
        }
        ["std-rename", from, to] => {
            format!("{:?}", fs::rename(from, to).map_err(|e| e.raw_os_error()))
        }
        ["std-open", path] => format!(
            "{:?}",
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map(drop)
                .map_err(|e| e.raw_os_error())
        ),
        ["open-delete", path] => {
            opened(path, DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES, |_| true)
        }
        ["open-read", path] => opened(path, GENERIC_READ, |_| true),
        ["open-rw", path] => opened(path, GENERIC_READ | GENERIC_WRITE, |_| true),
        ["dispose", path] => opened(path, DELETE | SYNCHRONIZE, |handle| {
            set(
                handle,
                FileDispositionInfo,
                &FILE_DISPOSITION_INFO { DeleteFile: true },
            )
        }),
        ["dispose-posix", path] => opened(path, DELETE | SYNCHRONIZE, |handle| {
            let flags = FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS;
            set(
                handle,
                FileDispositionInfoEx,
                &FILE_DISPOSITION_INFO_EX { Flags: flags },
            )
        }),
        _ => format!("unknown call {parts:?}"),
    }
}

fn set<T>(
    handle: windows_sys::Win32::Foundation::HANDLE,
    class: windows_sys::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
    info: &T,
) -> bool {
    let size = u32::try_from(size_of::<T>()).unwrap();
    unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle(
            handle,
            class,
            (info as *const T).cast(),
            size,
        ) != 0
    }
}

/// Opens a path with the given rights, as a folder too, without following a link, runs one call
/// on the handle and closes it. Inputs: the path, the rights and the call. Output: "ok", or the
/// open or call error.
fn opened(
    path: &str,
    access: u32,
    then: impl FnOnce(windows_sys::Win32::Foundation::HANDLE) -> bool,
) -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return format!("open {}", last_error());
    }
    let result = if then(handle) {
        "ok".to_owned()
    } else {
        format!("call {}", last_error())
    };
    unsafe { CloseHandle(handle) };
    result
}

/// Reads a device's owner, access list and label as SDDL through a handle opened for reading
/// its security only, on the owner's side. Input: the device path. Output: the SDDL text or
/// the failing call and its error.
fn device_sddl(path: &str) -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    };
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return format!("open {}", last_error());
    }
    let wanted =
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            wanted,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    unsafe { CloseHandle(handle) };
    if status != 0 {
        return format!("GetSecurityInfo error {status}");
    }
    let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
    let mut length = 0u32;
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            wanted,
            &mut text,
            &mut length,
        )
    };
    let sddl = if converted == 0 {
        format!("convert {}", last_error())
    } else {
        let units = unsafe { std::slice::from_raw_parts(text, length as usize) };
        String::from_utf16_lossy(units)
            .trim_end_matches('\0')
            .to_owned()
    };
    unsafe {
        LocalFree(text.cast());
        LocalFree(descriptor);
    }
    sddl
}

fn step(label: &str, call: &str, paths: &[&str]) -> String {
    [label, call]
        .into_iter()
        .chain(paths.iter().copied())
        .collect::<Vec<_>>()
        .join("|")
}

/// Launches the probe binary inside the lab's container in `cwd` to run the given steps, and
/// adds its STEP lines to the report. Inputs: the lab, the probe copy the container may run,
/// the working folder, a title and the steps.
fn child(lab: &mut Lab, exe: &Path, cwd: &Path, title: &str, steps: &[String]) {
    let mut env = lab.env.clone();
    env.push((CHILD_STEPS.to_owned(), steps.join("\n")));
    let output = launch(
        &lab.profile,
        cwd,
        &plain(exe),
        &[
            "lowbox_probe_child",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ],
        &env,
        Stdio::null(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let found: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("STEP "))
        .collect();
    lab.report.push(format!(
        "## {title}\n  cwd {}\n  exit {:?} stderr {:?}\n  {}",
        cwd.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        found.join("\n  ")
    ));
    if found.len() != steps.len() {
        lab.report.push(format!(
            "  the child printed {} of {} steps; stdout {stdout:?}",
            found.len(),
            steps.len()
        ));
    }
}

/// Step 0c, B1 and B2 below cmd: the container runs a copy of this test binary that makes the
/// file calls git and other tools use, by absolute and by relative path, in a plain folder and
/// in the anchor `.git` layout, and opens the NUL device. The owner side reads the NUL
/// device's security and the container's token groups.
#[test]
fn lowbox_probe_c1_file_calls_inside_the_container() {
    let mut lab = lab("crazyagent.test.c1");
    lab.report.push(format!(
        "NUL device, owner side: {}",
        device_sddl(r"\\.\NUL")
    ));
    let project = lab.project.clone();
    let exe = project.join("probe-child.exe");
    fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    lab.cmd("whoami /all in the container", &["whoami", "/all"], &[]);
    let nul = [
        step(
            "open NUL for read and write, expect allowed",
            "open-rw",
            &["NUL"],
        ),
        step(
            "open the NUL device path for read and write, expect allowed",
            "open-rw",
            &[r"\\.\NUL"],
        ),
        step("open NUL for read, expect allowed", "open-read", &["NUL"]),
        step(
            "std open NUL for read and write, expect allowed",
            "std-open",
            &["NUL"],
        ),
    ];
    child(
        &mut lab,
        &exe,
        &project,
        "NUL from inside the container",
        &nul,
    );
    let calls = project.join("calls");
    fs::create_dir_all(&calls).unwrap();
    for form in ["relative", "absolute"] {
        let prefix = if form == "absolute" {
            format!("{}\\", plain(&calls))
        } else {
            String::new()
        };
        let name = |file: &str| format!("{prefix}{form}-{file}");
        for file in [
            "del", "remove", "open", "dispose", "posix", "move", "over", "target", "ltarget",
            "rename", "rover", "rtarget",
        ] {
            fs::write(calls.join(format!("{form}-{file}")), "owner\r\n").unwrap();
        }
        fs::create_dir_all(calls.join(format!("{form}-dir"))).unwrap();
        let lock = calls.join(format!("{form}-lock"));
        lab.cmd(
            &format!("{form}: the container makes a lock"),
            &["echo", "lock>", &plain(&lock)],
            &[],
        );
        let steps = [
            step(
                "DeleteFileW an owner file, expect allowed",
                "delete",
                &[&name("del")],
            ),
            step(
                "std remove_file, expect allowed",
                "std-remove",
                &[&name("remove")],
            ),
            step(
                "open with DELETE, expect allowed",
                "open-delete",
                &[&name("open")],
            ),
            step(
                "delete by handle, expect allowed",
                "dispose",
                &[&name("dispose")],
            ),
            step(
                "POSIX delete by handle, expect allowed",
                "dispose-posix",
                &[&name("posix")],
            ),
            step(
                "MoveFileExW to a new name, expect allowed",
                "move",
                &[&name("move"), &name("moved")],
            ),
            step(
                "MoveFileExW over an owner file, expect allowed",
                "move",
                &[&name("over"), &name("target")],
            ),
            step(
                "MoveFileExW the container's lock over an owner file, expect allowed",
                "move",
                &[&name("lock"), &name("ltarget")],
            ),
            step(
                "std rename to a new name, expect allowed",
                "std-rename",
                &[&name("rename"), &name("renamed")],
            ),
            step(
                "std rename over an owner file, expect allowed",
                "std-rename",
                &[&name("rover"), &name("rtarget")],
            ),
            step(
                "RemoveDirectoryW an empty owner folder, expect allowed",
                "rmdir",
                &[&name("dir")],
            ),
        ];
        child(
            &mut lab,
            &exe,
            &calls,
            &format!("plain folder, {form} paths"),
            &steps,
        );
    }
    let work = git_tree(&mut lab, Layout::Anchor);
    let git = work.join(".git");
    for form in ["relative", "absolute"] {
        let prefix = if form == "absolute" {
            format!("{}\\", plain(&work))
        } else {
            String::new()
        };
        let at = |file: &str| format!("{prefix}.git\\{file}");
        for lock in ["index.lock", r"refs\heads\main.lock", "config.lock"] {
            lab.cmd(
                &format!("{form}: the container makes .git\\{lock}"),
                &["echo", "lock>", &plain(&git.join(lock))],
                &[],
            );
        }
        let rebase = git.join("rebase-merge");
        lab.cmd(
            &format!("{form}: the container makes .git\\rebase-merge"),
            &["mkdir", &plain(&rebase)],
            &[],
        );
        lab.cmd(
            &format!("{form}: the container writes .git\\rebase-merge\\done"),
            &["echo", "step>", &plain(&rebase.join("done"))],
            &[],
        );
        let steps = [
            step(
                "move index.lock over index, expect allowed",
                "move",
                &[&at("index.lock"), &at("index")],
            ),
            step(
                "move refs\\heads\\main.lock over main, expect allowed",
                "move",
                &[&at(r"refs\heads\main.lock"), &at(r"refs\heads\main")],
            ),
            step(
                "delete rebase-merge\\done, expect allowed",
                "delete",
                &[&at(r"rebase-merge\done")],
            ),
            step(
                "remove rebase-merge, expect allowed",
                "rmdir",
                &[&at("rebase-merge")],
            ),
            step(
                "D6: move config.lock over config, expect refused",
                "move",
                &[&at("config.lock"), &at("config")],
            ),
            step("delete config, expect refused", "delete", &[&at("config")]),
            step(
                "delete a hook, expect refused",
                "delete",
                &[&at(r"hooks\pre-commit.sample")],
            ),
            step(
                "move .git\\hooks, expect refused",
                "move",
                &[&at("hooks"), &at("hooks-moved")],
            ),
            step(
                "move .git, expect refused",
                "move",
                &[&format!("{prefix}.git"), &format!("{prefix}git-moved")],
            ),
        ];
        child(
            &mut lab,
            &exe,
            &work,
            &format!("anchor .git layout, {form} paths"),
            &steps,
        );
        let _ = fs::remove_file(git.join("config.lock"));
    }
    lab.finish("lowbox probe step 0c: file calls and NUL inside the container")
}
