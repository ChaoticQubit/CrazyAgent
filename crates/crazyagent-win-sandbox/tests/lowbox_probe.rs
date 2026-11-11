#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, Guarded, check_path};
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-win-boundary");

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

fn inside(profile: &Path, project: &Path, args: &[&str]) -> Output {
    Command::new(HELPER)
        .arg("--profile")
        .arg(profile)
        .arg("--")
        .arg(system("cmd.exe"))
        .args(args)
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
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
