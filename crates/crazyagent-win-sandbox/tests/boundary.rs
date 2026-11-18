#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, Guarded, HEADER, check_path};
use crazyagent_win_sandbox::request::REFUSED_EXIT;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-win-boundary");

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    outside: PathBuf,
    secret: PathBuf,
    grants: PathBuf,
    profile: PathBuf,
    base: BoundaryProfile,
}

fn plain(path: &Path) -> String {
    let text = path.to_str().unwrap().to_owned();
    assert_eq!(check_path(&text), Ok(()), "{text}");
    text
}

fn system(program: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32\{program}")
}

fn case(container: &str) -> Case {
    case_in(container, true)
}

/// The case tree under a root whose access list is auto-inherited, or, when `auto_inherited`
/// is false, holds the same entries as explicit copies with no AI flag: the state of a root
/// made under a parent whose list has none, which every folder made in it then shares.
/// Inputs: the container name and the root's state. Output: the case.
fn case_in(container: &str, auto_inherited: bool) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let reset = Command::new(system("icacls.exe"))
        .arg(&root)
        .args(["/reset", "/q"])
        .output()
        .unwrap();
    assert!(reset.status.success(), "{}", ran(&reset));
    let reset_sddl = read_sddl(&root);
    assert!(reset_sddl.starts_with("D:AI("), "{reset_sddl}");
    if !auto_inherited {
        let copies = access(&reset_sddl);
        assert!(
            copies.starts_with("D:(") && !copies.contains("ID;"),
            "{copies}"
        );
        write_raw(&root, &copies);
        assert_eq!(read_sddl(&root), copies);
    }
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let owner = root.join("owner");
    let outside = root.join("outside");
    let profiles = root.join("profiles");
    let grants = root.join("grants");
    for folder in [
        &project,
        &project.join("context"),
        &home,
        &tmp,
        &owner.join(".ssh"),
        &outside,
        &profiles,
        &grants,
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(project.join("readme.txt"), "project readme\r\n").unwrap();
    fs::write(project.join("context").join("brief.md"), "brief\r\n").unwrap();
    let secret = owner.join(".ssh").join("id_ed25519");
    fs::write(&secret, "PRIVATE KEY MATERIAL\r\n").unwrap();
    let profile = BoundaryProfile {
        container: container.into(),
        capabilities: Vec::new(),
        read_roots: Vec::new(),
        write_roots: vec![plain(&project), plain(&home), plain(&tmp)],
        guarded: vec![
            Guarded {
                path: plain(&project.join("context")),
                guard: Guard::ReadOnly,
            },
            Guarded {
                path: plain(&owner.join(".ssh")),
                guard: Guard::Hidden,
            },
        ],
        active_processes: 32,
        grants: plain(&grants),
    };
    let profile_path = profiles.join("agent.profile");
    fs::write(&profile_path, profile.encode().unwrap()).unwrap();
    Case {
        _temp: temp,
        project,
        home,
        outside,
        secret,
        grants,
        profile: profile_path,
        base: profile,
    }
}

/// An SDDL line without the flags that record where its entries came from, AI on the list
/// and ID on each entry, so two lines compare equal when they grant the same access with the
/// same protection. Input: a D: line. Output: the line without those flags.
fn access(sddl: &str) -> String {
    let start = sddl.find('(').unwrap_or(sddl.len());
    let list = &sddl[..start];
    assert!(list.starts_with("D:"), "{sddl}");
    format!(
        "{}{}",
        list.replace("AI", ""),
        rewrite_flags(&sddl[start..], |flags| flags.replace("ID", ""))
    )
}

/// The SDDL line Windows gives a list of explicit copies once it applies inheritance to it:
/// the AI flag on the list and the inherited flag on every entry. Input: a D: line with no
/// list flags. Output: the line.
fn inherited(sddl: &str) -> String {
    let aces = sddl.strip_prefix("D:").expect(sddl);
    assert!(aces.starts_with('('), "{sddl}");
    format!("D:AI{}", rewrite_flags(aces, |flags| format!("{flags}ID")))
}

/// The entries of an SDDL line, each with its flags field rewritten. Inputs: the entries,
/// from the first `(` on, and the rewrite. Output: the entries.
fn rewrite_flags(aces: &str, rewrite: impl Fn(&str) -> String) -> String {
    assert!(
        aces.is_empty() || (aces.starts_with('(') && aces.ends_with(')')),
        "{aces}"
    );
    aces.split_inclusive(')')
        .map(|ace| {
            let mut fields: Vec<String> = ace.split(';').map(str::to_owned).collect();
            assert!(fields.len() >= 6, "{aces}");
            fields[1] = rewrite(&fields[1]);
            fields.join(";")
        })
        .collect()
}

/// Writes `sddl` as the access list of `path` through SetFileSecurityW, which applies no
/// inheritance, so the list holds exactly the line: its entries' flags and no AI flag the
/// line does not carry. Relies on `sddl` being a D: line; the descriptor the conversion
/// allocates is freed after the write. Inputs: the path and the line. Output: none.
fn write_raw(path: &Path, sddl: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW,
    };
    let line: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let target: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            line.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(converted, 0, "{sddl}: {}", std::io::Error::last_os_error());
    assert!(!descriptor.is_null());
    let written =
        unsafe { SetFileSecurityW(target.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
    let error = std::io::Error::last_os_error();
    unsafe { LocalFree(descriptor) };
    assert_ne!(written, 0, "{}: {error}", path.display());
}

fn install(case: &Case, profile: &BoundaryProfile) {
    fs::write(&case.profile, profile.encode().unwrap()).unwrap();
}

/// The access list of `path` as the SDDL line `icacls /save` writes, from `D:` on. icacls is
/// native, while a PowerShell Get-Acl start costs about 3 s on windows-11-arm; it saves no
/// owner or group. Input: the path. Output: the line, or what failed.
fn read_sddl(path: &Path) -> String {
    let saved = tempfile::tempdir().unwrap();
    let file = saved.path().join("acl.txt");
    let read = Command::new(system("icacls.exe"))
        .arg(path)
        .arg("/save")
        .arg(&file)
        .arg("/q")
        .output();
    let output = match read {
        Ok(output) => output,
        Err(error) => return format!("icacls failed: {error}"),
    };
    if !output.status.success() {
        return format!("icacls failed: {}", ran(&output));
    }
    let bytes = fs::read(&file).unwrap();
    let text = if bytes.starts_with(&[0xFF, 0xFE]) || bytes.get(1) == Some(&0) {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(|unit: char| unit == '\u{feff}' || unit.is_whitespace()))
        .find(|line| line.starts_with("D:"))
        .map_or_else(
            || format!("icacls saved no access list: {text:?}"),
            str::to_owned,
        )
}

/// The owner of each path as Get-Acl names it, all read in one PowerShell start: the one
/// PowerShell read left in these rows, since icacls saves no owner. Input: the paths. Output:
/// one owner per path in order, or fewer lines when the read failed.
fn owners(paths: &[&Path]) -> Vec<String> {
    let joined = paths
        .iter()
        .map(|path| plain(path))
        .collect::<Vec<_>>()
        .join("|");
    let read = Command::new(system(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$env:CRAZYAGENT_TEST_OWNER_PATHS -split '\\|' | ForEach-Object { (Get-Acl -LiteralPath $_).Owner }",
        ])
        .env("CRAZYAGENT_TEST_OWNER_PATHS", joined)
        .output()
        .unwrap();
    String::from_utf8_lossy(&read.stdout)
        .lines()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

fn sddl(paths: &[&Path]) -> String {
    paths
        .iter()
        .map(|path| format!("{} => {}", path.display(), read_sddl(path)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A path's access list in SDDL as icacls saves it, with no owner or group and only the
/// auto-inherited flag (AI) left out, so two lists compare equal when their protection and
/// every entry with its flags and order match (DEC-130 option b). Input: the path. Output:
/// the text.
fn entries(path: &Path) -> String {
    let text = read_sddl(path);
    let Some(start) = text.find("D:") else {
        return text;
    };
    let flags_end = text[start..].find('(').map_or(text.len(), |at| start + at);
    format!(
        "{}{}{}",
        &text[..start + 2],
        text[start + 2..flags_end].replace("AI", ""),
        &text[flags_end..]
    )
}

/// The SDDL text with every entry naming `sid` removed. Inputs: the text and the SID. Output:
/// the text without those entries.
fn without(sddl: &str, sid: &str) -> String {
    let mut kept = String::new();
    let mut rest = sddl;
    while let Some(open) = rest.find('(') {
        let close = open + rest[open..].find(')').expect(sddl);
        kept.push_str(&rest[..open]);
        if !rest[open..=close].contains(sid) {
            kept.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    kept.push_str(rest);
    kept
}

/// The container SID on the entry the boundary put on the project folder. Input: the case,
/// launched at least once. Output: the SID text.
fn container_sid(case: &Case) -> String {
    let text = read_sddl(&case.project);
    let start = text.find("S-1-15-2-").expect(&text);
    let sid: String = text[start..]
        .chars()
        .take_while(|unit| unit.is_ascii_alphanumeric() || *unit == '-')
        .collect();
    assert!(sid.len() > "S-1-15-2-".len(), "{text}");
    sid
}

fn ran(output: &Output) -> String {
    format!(
        "exit {:?} stdout {:?} stderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn agent(profile: &Path, cwd: &Path, program: &str) -> Command {
    let mut command = Command::new(HELPER);
    command
        .arg("--profile")
        .arg(profile)
        .arg("--")
        .arg(program)
        .current_dir(cwd);
    command
}

fn boundary(case: &Case) -> Command {
    agent(&case.profile, &case.project, &system("cmd.exe"))
}

fn output(mut command: Command, args: &[&str]) -> Output {
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn run(case: &Case, args: &[&str]) -> Output {
    output(boundary(case), args)
}

fn exe(case: &Case, program: &str, args: &[&str]) -> Output {
    output(agent(&case.profile, &case.project, &system(program)), args)
}

const CALL: &str = "CRAZYAGENT_FILE_CALL";
const CHILD: [&str; 5] = [
    "file_call_child",
    "--exact",
    "--nocapture",
    "--test-threads",
    "1",
];

/// Runs one file call through the Windows API inside the case's container: this test binary,
/// copied into the agent's home, started through the helper as [`file_call_child`]. `cmd`'s
/// `del`, `ren` and `move` fail in any AppContainer (DEC-193 step 0c, B1), so a delete or
/// rename the boundary allows or refuses is only seen through the API. Inputs: the case and
/// the call, such as `["rename", from, to]`. Output: what the child printed and its exit.
fn call(case: &Case, step: &[&str]) -> Output {
    assert!(!step.is_empty() && step.len() <= 3);
    let probe = case.home.join("file-call.exe");
    if !probe.is_file() {
        fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
    }
    let mut command = agent(&case.profile, &case.project, &plain(&probe));
    command.env(CALL, step.join("|"));
    output(command, &CHILD)
}

/// Whether Windows refused a [`call`]: the child ran and its call failed with access denied
/// (error 5), so a path left in place is the boundary's doing, not a child that never started.
fn denied(output: &Output) -> bool {
    output.status.code() == Some(1)
        && String::from_utf8_lossy(&output.stdout).contains("(os error 5)")
}

/// The rows read an access list as the SDDL `icacls /save` writes for that path alone, from
/// its `D:` on, and never through PowerShell, whose start costs about 3 s on windows-11-arm
/// and timed rows out under load. A Users read entry granted here must show in the text.
#[test]
fn an_access_list_read_is_the_saved_sddl_of_that_path() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("read");
    fs::create_dir(&folder).unwrap();
    let granted = Command::new(system("icacls.exe"))
        .arg(&folder)
        .args(["/grant", "*S-1-5-32-545:(R)"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "{}", ran(&granted));
    let text = read_sddl(&folder);
    assert!(text.starts_with("D:"), "{text}");
    assert!(text.contains(";;;BU)"), "{text}");
    assert!(!text.contains('\n'), "{text}");
}

/// The child [`call`] starts: makes the one call named in its environment and exits 1 when
/// Windows refuses it; without that variable, as in a normal test run, it does nothing.
/// Calls: `delete`, `rename` (replacing the target), `rmdir`, `rmtree` and `mkdir`.
#[test]
fn file_call_child() {
    let Ok(line) = std::env::var(CALL) else {
        return;
    };
    let parts: Vec<&str> = line.split('|').collect();
    let done = match parts.as_slice() {
        ["delete", path] => fs::remove_file(path),
        ["rename", from, to] => fs::rename(from, to),
        ["rmdir", path] => fs::remove_dir(path),
        ["rmtree", path] => fs::remove_dir_all(path),
        ["mkdir", path] => fs::create_dir(path),
        _ => panic!("unknown file call {line}"),
    };
    if let Err(error) = done {
        println!("CALL {line} => {error}");
        std::process::exit(1);
    }
    println!("CALL {line} => done");
}

#[test]
fn an_agent_reads_its_project_but_not_a_secret_under_the_owners_home() {
    let case = case("crazyagent.test.read");
    let readme = run(
        &case,
        &["/C", "type", &plain(&case.project.join("readme.txt"))],
    );
    assert!(
        readme.status.success(),
        "{}",
        String::from_utf8_lossy(&readme.stderr)
    );
    assert!(String::from_utf8_lossy(&readme.stdout).contains("project readme"));
    let secret = run(&case, &["/C", "type", &plain(&case.secret)]);
    assert!(!secret.status.success());
    assert!(!String::from_utf8_lossy(&secret.stdout).contains("PRIVATE KEY"));
    assert!(!String::from_utf8_lossy(&secret.stderr).contains("PRIVATE KEY"));
}

#[test]
fn an_agent_writes_in_its_roots_but_not_outside_them_or_into_protected_paths() {
    let case = case("crazyagent.test.write");
    let inside = case.project.join("inside.txt");
    let home = case.home.join("home.txt");
    let escape = case.outside.join("escape.txt");
    let brief = case.project.join("context").join("brief.md");
    let context = case.project.join("context");
    let temp = case.project.parent().unwrap();
    let watched = [
        temp.parent().unwrap(),
        temp,
        case.project.as_path(),
        context.as_path(),
        brief.as_path(),
    ];
    let before = sddl(&watched);
    run(&case, &["/C", "echo", "inside>", &plain(&inside)]);
    let granted = sddl(&watched);
    run(&case, &["/C", "echo", "home>", &plain(&home)]);
    run(&case, &["/C", "echo", "escape>", &plain(&escape)]);
    let changed = run(&case, &["/C", "echo", "changed>", &plain(&brief)]);
    let deleted = call(&case, &["delete", &plain(&brief)]);
    assert!(denied(&deleted), "delete: {}", ran(&deleted));
    assert!(fs::read_to_string(&inside).unwrap().starts_with("inside"));
    assert!(fs::read_to_string(&home).unwrap().starts_with("home"));
    assert!(!escape.exists());
    assert_eq!(
        fs::read_to_string(&brief).unwrap(),
        "brief\r\n",
        "write: {}\ndelete: {}\nbefore:\n{before}\nafter the first launch:\n{granted}\nafter:\n{}",
        ran(&changed),
        ran(&deleted),
        sddl(&watched)
    );
}

#[test]
fn killing_the_helper_kills_the_agent_it_started() {
    let case = case("crazyagent.test.kill");
    let mut helper = boundary(&case)
        .arg("/K")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = helper.stdin.take().unwrap();
    let stdout = helper.stdout.take().unwrap();
    let (events, received) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
            if line.trim() == "ready" {
                let _ = events.send("ready");
            }
            line.clear();
        }
        let _ = events.send("closed");
    });
    stdin.write_all(b"echo ready\r\n").unwrap();
    stdin.flush().unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("ready"));
    helper.kill().unwrap();
    helper.wait().unwrap();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(30)),
        Ok("closed"),
        "the agent kept its output open after the helper died"
    );
    drop(stdin);
}

#[test]
fn a_bad_profile_or_batch_program_starts_nothing() {
    let case = case("crazyagent.test.refuse");
    let marker = case.project.join("started.txt");
    fs::write(
        &case.profile,
        format!("{HEADER}\ncontainer crazyagent.test.refuse\n"),
    )
    .unwrap();
    let output = run(&case, &["/C", "echo", "started>", &plain(&marker)]);
    assert_eq!(output.status.code(), Some(i32::from(REFUSED_EXIT)));
    assert!(String::from_utf8_lossy(&output.stderr).contains("boundary profile refused"));
    assert!(!marker.exists());
    let batch = Command::new(HELPER)
        .arg("--profile")
        .arg(&case.profile)
        .arg("--")
        .arg(plain(&case.project.join("run.bat")))
        .output()
        .unwrap();
    assert_eq!(batch.status.code(), Some(i32::from(REFUSED_EXIT)));
    assert!(String::from_utf8_lossy(&batch.stderr).contains("batch files are refused"));
}

/// V244-WIN-NPM: a read root no app container reads and this standard user cannot change,
/// made by an administrator and named in `CRAZYAGENT_TEST_UNCHANGEABLE_ROOT`, is refused
/// before the helper runs, and the helper given it anyway names it in its refusal and starts
/// nothing. A root every package reads and a folder this user owns are not refused.
#[test]
#[ignore = "needs an administrator-made folder and a standard user (test farm)"]
fn a_read_root_only_an_administrator_can_open_to_containers_is_refused_by_name() {
    let fixture = PathBuf::from(std::env::var("CRAZYAGENT_TEST_UNCHANGEABLE_ROOT").unwrap());
    let programs = PathBuf::from(std::env::var("ProgramFiles").unwrap());
    let case = case("crazyagent.test.unchangeable");
    let refused = |path: &Path| crazyagent_win_sandbox::unreadable_root(path, &case.base.container);
    assert!(refused(&fixture));
    assert!(!refused(&programs));
    assert!(!refused(&case.outside));
    let mut profile = case.base.clone();
    profile.read_roots = vec![plain(&fixture)];
    install(&case, &profile);
    let marker = case.project.join("started.txt");
    let output = run(&case, &["/C", "echo", "started>", &plain(&marker)]);
    assert_eq!(
        output.status.code(),
        Some(i32::from(REFUSED_EXIT)),
        "{}",
        ran(&output)
    );
    let named = format!(
        "could not change an access list of {} (Windows error 5)",
        plain(&fixture)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&named),
        "{}",
        ran(&output)
    );
    assert!(!marker.exists());
}

/// V244-WIN-NPM r2 F1: the read root of the row above, inside a folder every app container
/// reads, is not refused when that folder is a read root too, since the helper grants only the
/// outer one; on its own it is refused. The farm makes its parent readable to every package.
#[test]
#[ignore = "needs an administrator-made folder and a standard user (test farm)"]
fn a_read_root_inside_a_granted_one_is_not_refused_for_itself() {
    let fixture = PathBuf::from(std::env::var("CRAZYAGENT_TEST_UNCHANGEABLE_ROOT").unwrap());
    let outer = fixture.parent().unwrap().to_path_buf();
    let case = case("crazyagent.test.nested");
    let mut profile = case.base.clone();
    profile.read_roots = vec![plain(&outer), plain(&fixture)];
    assert_eq!(crazyagent_win_sandbox::unreadable_read_root(&profile), None);
    profile.read_roots = vec![plain(&fixture)];
    let alone = plain(&fixture);
    assert_eq!(
        crazyagent_win_sandbox::unreadable_read_root(&profile),
        Some(alone.as_str())
    );
}

#[test]
fn a_narrower_profile_takes_back_what_the_one_before_granted() {
    let case = case("crazyagent.test.contract");
    let before = entries(&case.outside);
    let temp = case.outside.parent().unwrap();
    let watched = [temp.parent().unwrap(), temp, case.outside.as_path()];
    let before_sddl = sddl(&watched);
    let first = case.outside.join("first.txt");
    let mut wide = case.base.clone();
    wide.write_roots.push(plain(&case.outside));
    install(&case, &wide);
    run(&case, &["/C", "echo", "first>", &plain(&first)]);
    assert!(
        fs::read_to_string(&first).unwrap().starts_with("first"),
        "the wide profile lets the agent write there"
    );
    assert_ne!(entries(&case.outside), before);
    let packages = "*S-1-15-2-1";
    let readable = Command::new(system("icacls.exe"))
        .args([
            plain(&case.outside).as_str(),
            "/grant",
            &format!("{packages}:(OI)(CI)(RX)"),
        ])
        .output()
        .unwrap();
    assert!(readable.status.success(), "{}", ran(&readable));

    let mut narrowed = case.base.clone();
    narrowed.read_roots.push(plain(&case.outside));
    install(&case, &narrowed);
    let read = run(&case, &["/C", "type", &plain(&first)]);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert!(String::from_utf8_lossy(&read.stdout).contains("first"));
    let second = case.outside.join("second.txt");
    run(&case, &["/C", "echo", "second>", &plain(&second)]);
    let deleted = call(&case, &["delete", &plain(&first)]);
    assert!(
        !second.exists(),
        "modify narrowed to read and run takes back writes, on a root every package reads too"
    );
    assert!(
        first.is_file() && denied(&deleted),
        "and deletes: {}",
        ran(&deleted)
    );
    let shared = Command::new(system("icacls.exe"))
        .args([plain(&case.outside).as_str(), "/remove:g", packages])
        .output()
        .unwrap();
    assert!(shared.status.success(), "{}", ran(&shared));

    install(&case, &case.base);
    let hidden = run(&case, &["/C", "type", &plain(&first)]);
    assert!(!hidden.status.success());
    assert!(!String::from_utf8_lossy(&hidden.stdout).contains("first"));
    assert_eq!(
        entries(&case.outside),
        before,
        "a root the profile no longer names keeps no entry of the container\nbefore:\n{before_sddl}\nafter:\n{}",
        sddl(&watched)
    );
}

#[test]
fn releasing_a_profile_takes_back_every_grant_its_record_and_its_container() {
    let case = case("crazyagent.test.release");
    let context = case.project.join("context");
    let before = [&case.project, &context, &case.home].map(|path| entries(path));
    let temp = case.project.parent().unwrap();
    let watched = [
        temp.parent().unwrap(),
        temp,
        case.project.as_path(),
        context.as_path(),
        case.home.as_path(),
    ];
    let before_sddl = sddl(&watched);
    let readme = run(
        &case,
        &["/C", "type", &plain(&case.project.join("readme.txt"))],
    );
    assert!(
        readme.status.success(),
        "{}",
        String::from_utf8_lossy(&readme.stderr)
    );
    let granted = [&case.project, &context, &case.home].map(|path| entries(path));
    for (granted, before) in granted.iter().zip(&before) {
        assert_ne!(granted, before);
    }
    let record = case.grants.join("container-crazyagent.test.release.grants");
    assert!(record.is_file());
    let container = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("Packages")
        .join("crazyagent.test.release");
    assert!(container.is_dir());
    let granted_sddl = sddl(&watched);
    assert!(matches!(
        crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.other"),
        Err(crazyagent_win_sandbox::BoundaryError::Refused(_))
    ));
    assert!(record.is_file());
    assert!(container.is_dir());
    assert_eq!(
        sddl(&watched),
        granted_sddl,
        "a refused release changes nothing"
    );
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.release").unwrap();
    assert_eq!(
        [&case.project, &context, &case.home].map(|path| entries(path)),
        before,
        "every root and protected path is back to the access list it had\nbefore:\n{before_sddl}\ngranted:\n{granted_sddl}\nafter:\n{}",
        sddl(&watched)
    );
    assert!(!record.exists());
    assert!(
        !container.exists(),
        "the container's own folder goes with it"
    );
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.release").unwrap();
}

const REPO_FILES: [(&str, &str); 9] = [
    (r".git\HEAD", "ref: refs/heads/main\n"),
    (r".git\config", "[core]\n\tbare = false\n"),
    (r".git\index", "index\n"),
    (r".git\hooks\pre-commit.sample", "exit 0\n"),
    (r".git\refs\heads\main", "main\n"),
    (r".git\logs\HEAD", "log\n"),
    (r".crazyagent\context\memo.md", "memo\n"),
    (r".crazyagent\instructions.md", "instructions\n"),
    (r".vault\key.txt", "VAULT KEY MATERIAL\n"),
];

const REPO_GUARDS: [(&str, Guard); 6] = [
    (r".crazyagent\context", Guard::ReadOnly),
    (r".crazyagent\instructions.md", Guard::ReadOnly),
    (r".git\hooks", Guard::ReadOnly),
    (r".git\config", Guard::ReadOnly),
    (r".git\modules", Guard::ReadOnly),
    (".vault", Guard::Hidden),
];

fn at(case: &Case, relative: &str) -> PathBuf {
    relative
        .split('\\')
        .fold(case.project.clone(), |path, part| path.join(part))
}

/// A case whose project is a git work tree guarded the way the engine guards one on Windows:
/// `context`, `.crazyagent\context`, `.crazyagent\instructions.md`, `.git\hooks`,
/// `.git\config` and `.git\modules` read-only, `.git\modules` present as the engine makes it
/// before each launch, plus a hidden `.vault`. Inputs: the container name and the project
/// paths to remove before the first launch. Output: the case with that profile installed.
fn repo(container: &str, missing: &[&str]) -> Case {
    let mut case = case(container);
    for folder in [
        r".git\hooks",
        r".git\modules",
        r".git\objects\pack",
        r".git\refs\heads",
        r".git\logs",
        r".crazyagent\context",
        ".vault",
    ] {
        fs::create_dir_all(at(&case, folder)).unwrap();
    }
    for (file, text) in REPO_FILES {
        fs::write(at(&case, file), text).unwrap();
    }
    for relative in missing {
        let path = at(&case, relative);
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
    for (relative, guard) in REPO_GUARDS {
        let path = plain(&at(&case, relative));
        case.base.guarded.push(Guarded { path, guard });
    }
    install(&case, &case.base);
    case
}

/// Every boundary case one row expects, kept so a row reports all of them at once with what
/// the agent's command printed.
struct Checks(Vec<String>);

impl Checks {
    fn expect(&mut self, holds: bool, what: &str, output: &Output) {
        if !holds {
            self.0.push(format!("{what}: {}", ran(output)));
        }
    }

    fn done(self, watched: &[&Path]) {
        assert!(
            self.0.is_empty(),
            "{}\naccess lists now:\n{}",
            self.0.join("\n"),
            sddl(watched)
        );
    }
}

fn reads(path: &Path, text: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|now| now == text)
}

fn starts(path: &Path, text: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|now| now.starts_with(text))
}

#[test]
fn read_only_paths_refuse_every_change_while_the_project_stays_writable() {
    let case = repo("crazyagent.test.readonly", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let note = path("note.txt");
    let made = run(&case, &["/C", "echo", "note>", &plain(&note)]);
    checks.expect(
        starts(&note, "note"),
        "a new project file is written",
        &made,
    );
    for (relative, words) in [
        (r"context\brief.md", ["echo", "changed>"]),
        (r".git\config", ["echo", "changed>>"]),
        (r".crazyagent\instructions.md", ["echo", "changed>"]),
        (r".crazyagent\context\memo.md", ["echo", "changed>"]),
    ] {
        let target = path(relative);
        let before = fs::read_to_string(&target).unwrap();
        let output = run(&case, &["/C", words[0], words[1], &plain(&target)]);
        checks.expect(
            reads(&target, &before),
            &format!("{relative} stays unchanged"),
            &output,
        );
    }
    for relative in [r".git\hooks\post-checkout", r".crazyagent\context\new.md"] {
        let target = path(relative);
        let output = run(&case, &["/C", "echo", "new>", &plain(&target)]);
        checks.expect(
            !target.exists(),
            &format!("{relative} is not created"),
            &output,
        );
    }
    let sample = path(r".git\hooks\pre-commit.sample");
    let deleted = call(&case, &["delete", &plain(&sample)]);
    checks.expect(
        sample.is_file() && denied(&deleted),
        "a hook is not deleted",
        &deleted,
    );
    let config = path(r".git\config");
    let lock = path(r".git\config.lock");
    let made = run(&case, &["/C", "echo", "planted>", &plain(&lock)]);
    checks.expect(starts(&lock, "planted"), "config.lock is made", &made);
    let replaced = call(&case, &["rename", &plain(&lock), &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n") && denied(&replaced),
        "config.lock is not renamed over config",
        &replaced,
    );
    let removed = call(&case, &["delete", &plain(&config)]);
    checks.expect(
        config.is_file() && denied(&removed),
        "config is not deleted",
        &removed,
    );
    for relative in [r".git\hooks", "context", r".crazyagent\context"] {
        let folder = path(relative);
        let moved = plain(&folder.with_file_name("moved"));
        let output = call(&case, &["rename", &plain(&folder), &moved]);
        checks.expect(
            folder.is_dir() && !folder.with_file_name("moved").exists() && denied(&output),
            &format!("{relative} is not renamed"),
            &output,
        );
    }
    let watched = [
        path(".git"),
        path(r".git\hooks"),
        path(r".git\config"),
        path("context"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
    ];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn anchored_folders_stay_in_place_while_git_writes_its_own_files() {
    let case = repo("crazyagent.test.anchor", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    for (name, mark) in [
        (r".git\index", "staged"),
        (r".git\HEAD", "moved"),
        (r".git\refs\heads\main", "next"),
    ] {
        let target = path(name);
        let lock = target.with_file_name(format!(
            "{}.lock",
            target.file_name().unwrap().to_str().unwrap()
        ));
        let made = run(&case, &["/C", "echo", &format!("{mark}>"), &plain(&lock)]);
        checks.expect(starts(&lock, mark), &format!("{name}.lock is made"), &made);
        let moved = call(&case, &["rename", &plain(&lock), &plain(&target)]);
        checks.expect(
            starts(&target, mark) && !lock.exists() && moved.status.success(),
            &format!("{name}.lock is renamed over {name}"),
            &moved,
        );
    }
    let objects = path(r".git\objects\ab");
    let folder = run(&case, &["/C", "mkdir", &plain(&objects)]);
    checks.expect(objects.is_dir(), "an object folder is made", &folder);
    let object = objects.join("cd");
    let written = run(&case, &["/C", "echo", "object>", &plain(&object)]);
    checks.expect(starts(&object, "object"), "an object is written", &written);
    let log = path(r".git\logs\HEAD");
    let appended = run(&case, &["/C", "echo", "entry>>", &plain(&log)]);
    checks.expect(
        fs::read_to_string(&log).is_ok_and(|text| text.contains("entry")),
        "the reflog is appended",
        &appended,
    );
    let rebase = path(r".git\rebase-merge");
    let step = rebase.join("done");
    let made = run(&case, &["/C", "mkdir", &plain(&rebase)]);
    checks.expect(rebase.is_dir(), "a rebase folder is made", &made);
    let stepped = run(&case, &["/C", "echo", "step>", &plain(&step)]);
    checks.expect(starts(&step, "step"), "a rebase step is written", &stepped);
    let done = call(&case, &["delete", &plain(&step)]);
    checks.expect(
        !step.exists() && done.status.success(),
        "a rebase step is deleted",
        &done,
    );
    let removed = call(&case, &["rmdir", &plain(&rebase)]);
    checks.expect(
        !rebase.exists() && removed.status.success(),
        "the rebase folder is removed",
        &removed,
    );
    let hooks = path(r".git\hooks");
    let moved = call(&case, &["rename", &plain(&hooks), &plain(&path("hooks"))]);
    checks.expect(
        hooks.is_dir() && denied(&moved),
        "the hooks folder is not moved out",
        &moved,
    );
    for (relative, name) in [(".git", "git-moved"), (".crazyagent", "crazyagent-moved")] {
        let folder = path(relative);
        let output = call(&case, &["rename", &plain(&folder), &plain(&path(name))]);
        checks.expect(
            folder.is_dir() && !path(name).exists() && denied(&output),
            &format!("{relative} is not renamed"),
            &output,
        );
    }
    let watched = [case.project.clone(), path(".git"), path(".crazyagent")];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn a_missing_protected_path_cannot_be_made() {
    let case = repo(
        "crazyagent.test.missing",
        &[
            r".git\hooks",
            r".git\modules",
            r".crazyagent\instructions.md",
        ],
    );
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    for relative in [r".git\hooks", r".git\modules"] {
        let folder = path(relative);
        let output = run(&case, &["/C", "mkdir", &plain(&folder)]);
        checks.expect(
            !folder.exists(),
            &format!("{relative} is not made"),
            &output,
        );
    }
    let instructions = path(r".crazyagent\instructions.md");
    let written = run(&case, &["/C", "echo", "planted>", &plain(&instructions)]);
    checks.expect(
        !starts(&instructions, "planted"),
        "instructions.md is not planted",
        &written,
    );
    let lock = path(r".git\index.lock");
    let made = run(&case, &["/C", "echo", "staged>", &plain(&lock)]);
    checks.expect(
        !lock.exists(),
        "no entry is added to .git, the cost the engine's pre-create avoids",
        &made,
    );
    let log = path(r".git\logs\HEAD");
    let appended = run(&case, &["/C", "echo", "entry>>", &plain(&log)]);
    checks.expect(
        fs::read_to_string(&log).is_ok_and(|text| text.contains("entry")),
        "files already in .git are still written",
        &appended,
    );
    let objects = path(r".git\objects\ab");
    let folder = call(&case, &["mkdir", &plain(&objects)]);
    checks.expect(
        objects.is_dir() && folder.status.success(),
        "work below .git goes on",
        &folder,
    );
    let watched = [path(".git"), path(".crazyagent")];
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn a_hidden_folder_inside_the_project_is_neither_read_nor_listed() {
    let case = repo("crazyagent.test.hidden", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let readme = run(&case, &["/C", "type", &plain(&path("readme.txt"))]);
    checks.expect(
        String::from_utf8_lossy(&readme.stdout).contains("project readme"),
        "the project readme is read",
        &readme,
    );
    let key = path(r".vault\key.txt");
    let leaked = |output: &Output| {
        String::from_utf8_lossy(&output.stdout).contains("VAULT KEY")
            || String::from_utf8_lossy(&output.stderr).contains("VAULT KEY")
    };
    let read = run(&case, &["/C", "type", &plain(&key)]);
    checks.expect(
        !read.status.success() && !leaked(&read),
        "the hidden key is not read",
        &read,
    );
    let listed = run(&case, &["/C", "dir", "/B", &plain(&path(".vault"))]);
    checks.expect(
        !String::from_utf8_lossy(&listed.stdout).contains("key.txt"),
        "the hidden folder is not listed",
        &listed,
    );
    let copy = path("copy.txt");
    let copied = run(&case, &["/C", "copy", &plain(&key), &plain(&copy)]);
    checks.expect(
        !fs::read_to_string(&copy).is_ok_and(|text| text.contains("VAULT KEY")),
        "the hidden key is not copied out",
        &copied,
    );
    checks.done(&[&path(".vault"), &key]);
}

#[test]
fn an_agent_cannot_change_an_access_list_or_take_ownership() {
    let case = repo("crazyagent.test.acl", &[]);
    let path = |relative: &str| at(&case, relative);
    run(&case, &["/C", "echo", "first>", &plain(&path("first.txt"))]);
    let sid = container_sid(&case);
    let hooks = path(r".git\hooks");
    let config = path(r".git\config");
    let context = path("context");
    let guarded = [&hooks, &config, &context];
    let before = guarded.map(|path| entries(path));
    let mut checks = Checks(Vec::new());
    let shown = exe(&case, "icacls.exe", &[&plain(&hooks)]);
    checks.expect(
        shown.status.success(),
        "icacls runs in the container and reads an access list",
        &shown,
    );
    let all = format!("*{sid}:(OI)(CI)(F)");
    let one = format!("*{sid}:(F)");
    let (hooks_text, config_text, context_text) = (plain(&hooks), plain(&config), plain(&context));
    let attempts: [(&str, &[&str]); 6] = [
        ("icacls.exe", &[&hooks_text, "/grant", &all]),
        ("icacls.exe", &[&hooks_text, "/inheritance:e"]),
        ("icacls.exe", &[&config_text, "/grant", &one]),
        ("icacls.exe", &[&config_text, "/reset"]),
        ("takeown.exe", &["/F", &config_text]),
        ("icacls.exe", &[&context_text, "/grant", &all]),
    ];
    let mut taken = None;
    for (program, args) in attempts {
        let output = exe(&case, program, args);
        checks.expect(
            guarded.map(|path| entries(path)) == before,
            &format!("{program} {args:?} changes no access list"),
            &output,
        );
        if program == "takeown.exe" {
            taken = Some(output);
        }
    }
    let taken = taken.unwrap();
    checks.expect(
        !taken.status.success() && !String::from_utf8_lossy(&taken.stdout).contains("now owned"),
        "takeown cannot take ownership of .git\\config",
        &taken,
    );
    let owned = owners(&[&config, &case.project]);
    checks.expect(
        owned.len() == 2 && !owned[0].is_empty() && owned[0] == owned[1],
        &format!(".git\\config keeps the owner the project folder has: {owned:?}"),
        &taken,
    );
    let hook = hooks.join("post-checkout");
    let planted = run(&case, &["/C", "echo", "hook>", &plain(&hook)]);
    checks.expect(!hook.exists(), "no hook is created afterwards", &planted);
    let appended = run(&case, &["/C", "echo", "changed>>", &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n"),
        "config stays unchanged afterwards",
        &appended,
    );
    let brief = context.join("brief.md");
    let changed = run(&case, &["/C", "echo", "changed>", &plain(&brief)]);
    checks.expect(
        reads(&brief, "brief\r\n"),
        "the brief stays unchanged afterwards",
        &changed,
    );
    checks.done(&guarded.map(PathBuf::as_path));
}

#[test]
fn two_agents_on_one_project_keep_each_others_guards() {
    let case = repo("crazyagent.test.first", &[]);
    let path = |relative: &str| at(&case, relative);
    let watched = [
        case.project.clone(),
        path(".git"),
        path(r".git\hooks"),
        path(r".git\config"),
        path("context"),
        path(".crazyagent"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
        path(".vault"),
    ];
    let before = watched.each_ref().map(|path| entries(path));
    let mut second = case.base.clone();
    second.container = "crazyagent.test.second".into();
    let second_profile = case.profile.with_file_name("second.profile");
    fs::write(&second_profile, second.encode().unwrap()).unwrap();
    let as_second = |args: &[&str]| {
        output(
            agent(&second_profile, &case.project, &system("cmd.exe")),
            args,
        )
    };
    let mut checks = Checks(Vec::new());
    let first = run(&case, &["/C", "echo", "a>", &plain(&path("a.txt"))]);
    checks.expect(
        starts(&path("a.txt"), "a"),
        "the first agent writes",
        &first,
    );
    let second_run = as_second(&["/C", "echo", "b>", &plain(&path("b.txt"))]);
    checks.expect(
        starts(&path("b.txt"), "b"),
        "the second agent writes",
        &second_run,
    );
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.first").unwrap();
    let hook = path(r".git\hooks\post-checkout");
    let planted = as_second(&["/C", "echo", "hook>", &plain(&hook)]);
    checks.expect(
        !hook.exists(),
        "the second agent still cannot create a hook after the first is released",
        &planted,
    );
    let config = path(r".git\config");
    let appended = as_second(&["/C", "echo", "changed>>", &plain(&config)]);
    checks.expect(
        reads(&config, "[core]\n\tbare = false\n"),
        "the second agent still cannot change config",
        &appended,
    );
    let still = as_second(&["/C", "echo", "c>", &plain(&path("c.txt"))]);
    checks.expect(
        starts(&path("c.txt"), "c"),
        "the second agent still writes its project",
        &still,
    );
    crazyagent_win_sandbox::release(&second_profile, "crazyagent.test.second").unwrap();
    let after = watched.each_ref().map(|path| entries(path));
    if after != before {
        checks.0.push(format!(
            "every access list is back to before once both are released\nbefore:\n{}\nafter:\n{}",
            before.join("\n"),
            after.join("\n")
        ));
    }
    checks.done(&watched.each_ref().map(PathBuf::as_path));
}

#[test]
fn an_agent_works_normally_in_its_project_and_home() {
    let case = repo("crazyagent.test.work", &[]);
    let path = |relative: &str| at(&case, relative);
    let mut checks = Checks(Vec::new());
    let deep = path(r"build\out\deep");
    let made = run(&case, &["/C", "mkdir", &plain(&deep)]);
    checks.expect(deep.is_dir(), "a nested build folder is made", &made);
    let artifact = deep.join("o.txt");
    let written = run(&case, &["/C", "echo", "built>", &plain(&artifact)]);
    checks.expect(
        starts(&artifact, "built"),
        "a build file is written",
        &written,
    );
    let renamed = call(
        &case,
        &[
            "rename",
            &plain(&path(r"build\out")),
            &plain(&path(r"build\out-moved")),
        ],
    );
    checks.expect(
        path(r"build\out-moved\deep\o.txt").is_file() && renamed.status.success(),
        "a build folder is renamed",
        &renamed,
    );
    let (old, new) = (path("old.txt"), path("new.txt"));
    run(&case, &["/C", "echo", "old>", &plain(&old)]);
    run(&case, &["/C", "echo", "new>", &plain(&new)]);
    let replaced = call(&case, &["rename", &plain(&new), &plain(&old)]);
    checks.expect(
        starts(&old, "new") && !new.exists() && replaced.status.success(),
        "a file is renamed over another",
        &replaced,
    );
    let deleted = call(&case, &["delete", &plain(&old)]);
    checks.expect(
        !old.exists() && deleted.status.success(),
        "a file is deleted",
        &deleted,
    );
    let removed = call(&case, &["rmtree", &plain(&path("build"))]);
    checks.expect(
        !path("build").exists() && removed.status.success(),
        "a build tree is removed",
        &removed,
    );
    let home = case.home.join("notes.txt");
    let noted = run(&case, &["/C", "echo", "home>", &plain(&home)]);
    checks.expect(starts(&home, "home"), "the agent home is written", &noted);
    checks.done(&[&case.project, &case.home]);
}

#[test]
fn releasing_restores_every_guarded_path_the_owner_left_alone() {
    let case = repo("crazyagent.test.restore", &[]);
    let path = |relative: &str| at(&case, relative);
    let watched = [
        case.project.clone(),
        path(".git"),
        path(r".git\hooks"),
        path(r".git\hooks\pre-commit.sample"),
        path(r".git\config"),
        path("context"),
        path(r"context\brief.md"),
        path(".crazyagent"),
        path(r".crazyagent\context"),
        path(r".crazyagent\instructions.md"),
        path(".vault"),
        case.home.clone(),
    ];
    let before = watched.each_ref().map(|path| entries(path));
    let readme = run(&case, &["/C", "type", &plain(&path("readme.txt"))]);
    assert!(readme.status.success(), "{}", ran(&readme));
    let granted = watched.each_ref().map(|path| entries(path));
    assert_ne!(granted, before);
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.restore").unwrap();
    let after = watched.each_ref().map(|path| entries(path));
    assert_eq!(
        after,
        before,
        "every guarded path is back to the access list it had\ngranted:\n{}",
        granted.join("\n")
    );
    let record = case.grants.join("container-crazyagent.test.restore.grants");
    assert!(!record.exists());
}

#[test]
fn releasing_keeps_an_access_list_the_owner_changed_and_drops_only_the_agents_entries() {
    let case = repo("crazyagent.test.owner", &[]);
    let path = |relative: &str| at(&case, relative);
    let config = path(r".git\config");
    let config_before = entries(&config);
    run(&case, &["/C", "echo", "first>", &plain(&path("first.txt"))]);
    let sid = container_sid(&case);
    let hooks = path(r".git\hooks");
    let granted = Command::new(system("icacls.exe"))
        .args([plain(&hooks).as_str(), "/grant", "*S-1-5-19:(OI)(CI)(RX)"])
        .output()
        .unwrap();
    assert!(granted.status.success(), "{}", ran(&granted));
    let changed = entries(&hooks);
    assert!(changed.contains(";;;LS)"), "{changed}");
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.owner").unwrap();
    let after = entries(&hooks);
    assert_eq!(
        after,
        without(&changed, &sid),
        "the owner's entry stays and only the agent's entries go\nchanged:\n{changed}"
    );
    fs::write(hooks.join("owner.sample"), "owner\n").unwrap();
    assert_eq!(
        entries(&config),
        config_before,
        "a guarded path the owner left alone is restored"
    );
}

#[test]
fn releasing_under_a_parent_without_auto_inheritance_keeps_the_access_with_inherited_entries() {
    let case = case_in("crazyagent.test.explicit", false);
    let context = case.project.join("context");
    let written = [
        case.project.clone(),
        case.project.join("readme.txt"),
        context.clone(),
        context.join("brief.md"),
        case.home.clone(),
    ];
    let untouched = [
        case.secret.parent().unwrap().to_path_buf(),
        case.secret.clone(),
    ];
    let watched: Vec<&PathBuf> = written.iter().chain(&untouched).collect();
    let read = || {
        watched
            .iter()
            .map(|path| read_sddl(path))
            .collect::<Vec<_>>()
    };
    let before = read();
    for line in &before {
        assert!(
            line.starts_with("D:(") && !line.contains("ID;"),
            "a path made under the root holds explicit copies: {line}"
        );
    }
    let readme = run(
        &case,
        &["/C", "type", &plain(&case.project.join("readme.txt"))],
    );
    assert!(readme.status.success(), "{}", ran(&readme));
    let sid = container_sid(&case);
    let granted = read();
    assert_ne!(granted, before);
    crazyagent_win_sandbox::release(&case.profile, "crazyagent.test.explicit").unwrap();
    let after = read();
    let shown = |lines: &[String]| {
        watched
            .iter()
            .zip(lines)
            .map(|(path, line)| format!("{} => {line}", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let report = format!(
        "before:\n{}\ngranted:\n{}\nafter:\n{}",
        shown(&before),
        shown(&granted),
        shown(&after)
    );
    assert!(
        after.iter().all(|line| !line.contains(&sid)),
        "no entry of the container is left\n{report}"
    );
    assert_eq!(
        after.iter().map(|line| access(line)).collect::<Vec<_>>(),
        before.iter().map(|line| access(line)).collect::<Vec<_>>(),
        "each path grants the access it did before\n{report}"
    );
    let expected: Vec<String> = before
        .iter()
        .enumerate()
        .map(|(at, line)| {
            if at < written.len() {
                inherited(line)
            } else {
                line.clone()
            }
        })
        .collect();
    assert_eq!(
        after, expected,
        "a path the grant wrote holds its entries as inherited ones under the AI flag, as Windows \
         leaves it at the next change above it; a path it never wrote stays exact\n{report}"
    );
}
