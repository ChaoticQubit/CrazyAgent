#![cfg(windows)]

use crazyagent_win_sandbox::profile::{BoundaryProfile, Capability, Guard, Guarded, check_path};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-win-boundary");
const MODE: &str = "CRAZYAGENT_PROBE_MODE";
const FOLDER: &str = "CRAZYAGENT_PROBE_FOLDER";
const TARGETS: &str = "CRAZYAGENT_PROBE_TARGETS";
const MARKER_NAME: &str = "CRAZYAGENT_PROBE_MARKER";
const MARKER: &str = "crazyagent-cross-read-marker-7c3d91";
const CHILD: [&str; 5] = [
    "probe_child",
    "--exact",
    "--nocapture",
    "--test-threads",
    "1",
];
const QUERY_LIMITED: u32 = 0x1000;
const VM_READ: u32 = 0x0010;
const LIMIT_UNITS: usize = 1 << 19;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn ReadProcessMemory(
        process: *mut c_void,
        address: *const c_void,
        buffer: *mut c_void,
        size: usize,
        read: *mut usize,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: *mut c_void,
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// The basic information NtQueryInformationProcess gives for class 0 on 64-bit Windows:
/// the process's PEB address and its own id, which the read checks against the pid asked.
#[repr(C)]
struct Basic {
    _exit_status: i32,
    peb: usize,
    _affinity: usize,
    _priority: i32,
    pid: usize,
    _parent: usize,
}

/// An open process handle, closed when dropped.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// One agent's tree: a project, a home holding a copy of this test binary, and a profile for
/// its own container with the capability an open-network agent gets.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    profile: PathBuf,
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

fn ran(output: &Output) -> String {
    format!(
        "exit {:?} stdout {:?} stderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn case(container: &str) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let reset = Command::new(system("icacls.exe"))
        .arg(&root)
        .args(["/reset", "/q"])
        .output()
        .unwrap();
    assert!(reset.status.success(), "{}", ran(&reset));
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let profiles = root.join("profiles");
    let grants = root.join("grants");
    for folder in [
        &project,
        &project.join("context"),
        &home,
        &tmp,
        &profiles,
        &grants,
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    let profile = BoundaryProfile {
        container: container.into(),
        capabilities: vec![Capability::InternetClient],
        read_roots: Vec::new(),
        write_roots: vec![plain(&project), plain(&home), plain(&tmp)],
        guarded: vec![Guarded {
            path: plain(&project.join("context")),
            guard: Guard::ReadOnly,
        }],
        active_processes: 32,
        grants: plain(&grants),
    };
    let path = profiles.join("agent.profile");
    fs::write(&path, profile.encode().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), home.join("probe.exe")).unwrap();
    Case {
        _temp: temp,
        root,
        project,
        home,
        profile: path,
    }
}

/// The helper starting this test binary from the case's home as [`probe_child`] in `mode`,
/// inside the case's container, in its project. Inputs: the case and the mode. Output: the
/// command.
fn agent(case: &Case, mode: &str) -> Command {
    let mut command = Command::new(HELPER);
    command
        .arg("--profile")
        .arg(&case.profile)
        .arg("--")
        .arg(plain(&case.home.join("probe.exe")))
        .args(CHILD)
        .current_dir(&case.project)
        .env(MODE, mode)
        .env(FOLDER, &case.home)
        .stdin(Stdio::null());
    command
}

fn opened(pid: u32, access: u32) -> Result<Handle, u32> {
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        Err(unsafe { GetLastError() })
    } else {
        Ok(Handle(handle))
    }
}

fn word(handle: &Handle, address: usize) -> Result<usize, String> {
    let mut value = 0usize;
    let mut read = 0usize;
    let done = unsafe {
        ReadProcessMemory(
            handle.0,
            address as *const c_void,
            (&raw mut value).cast(),
            size_of::<usize>(),
            &raw mut read,
        )
    };
    if done == 0 || read != size_of::<usize>() {
        return Err(format!("word_err{}", unsafe { GetLastError() }));
    }
    Ok(value)
}

/// Reads another process's environment block through its PEB: the process parameters at
/// PEB+0x20, their environment pointer at +0x80, then the block up to its double NUL, in
/// chunks that halve where a read crosses the end of a mapping. Inputs: the handle with
/// VM_READ and the pid it names. Output: the block as text, or the step that failed.
fn environment(handle: &Handle, pid: u32) -> Result<String, String> {
    assert_eq!(size_of::<usize>(), 8);
    let mut basic = Basic {
        _exit_status: 0,
        peb: 0,
        _affinity: 0,
        _priority: 0,
        pid: 0,
        _parent: 0,
    };
    let mut returned = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            handle.0,
            0,
            (&raw mut basic).cast(),
            u32::try_from(size_of::<Basic>()).unwrap(),
            &raw mut returned,
        )
    };
    if status != 0 {
        return Err(format!("nt_status_{:#x}", status.cast_unsigned()));
    }
    assert_eq!(basic.pid, usize::try_from(pid).unwrap());
    let parameters = word(handle, basic.peb + 0x20)?;
    let block = word(handle, parameters + 0x80)?;
    let mut units: Vec<u16> = Vec::new();
    let mut chunk = 2048usize;
    while units.len() < LIMIT_UNITS {
        let mut page = vec![0u16; chunk];
        let mut read = 0usize;
        let done = unsafe {
            ReadProcessMemory(
                handle.0,
                (block + units.len() * 2) as *const c_void,
                page.as_mut_ptr().cast(),
                chunk * 2,
                &raw mut read,
            )
        };
        if done == 0 {
            if chunk > 1 {
                chunk /= 2;
                continue;
            }
            if units.is_empty() {
                return Err(format!("block_err{}", unsafe { GetLastError() }));
            }
            break;
        }
        units.extend_from_slice(&page[..read / 2]);
        if units.windows(2).any(|pair| pair[0] == 0 && pair[1] == 0) {
            break;
        }
    }
    Ok(String::from_utf16_lossy(&units))
}

fn yes(found: bool) -> &'static str {
    if found { "yes" } else { "no" }
}

/// One read attempt, step by step, as a line: the limited open, the VM_READ open and, when
/// that opens, whether the environment holds the marker and this probe's mode variable.
/// Inputs: a label and the pid. Output: the line.
fn report(label: &str, pid: u32) -> String {
    let limited = match opened(pid, QUERY_LIMITED) {
        Ok(_) => "ok".to_owned(),
        Err(code) => format!("err{code}"),
    };
    let mut line = format!("READ {label} pid={pid} open_limited={limited}");
    match opened(pid, QUERY_LIMITED | VM_READ) {
        Err(code) => line.push_str(&format!(" open_vm=err{code}")),
        Ok(handle) => {
            line.push_str(" open_vm=ok");
            match environment(&handle, pid) {
                Ok(text) => line.push_str(&format!(
                    " env=ok units={} marker={} mode_seen={}",
                    text.len(),
                    yes(text.contains(MARKER)),
                    yes(text.contains(MODE))
                )),
                Err(error) => line.push_str(&format!(" env=err {error}")),
            }
        }
    }
    line
}

fn waited_pid(folder: &Path) -> Option<u32> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        if let Ok(text) = fs::read_to_string(folder.join("pid.txt"))
            && let Ok(pid) = text.trim().parse()
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// The verdict on two target lines read from one container: READ when either holds the
/// marker, REFUSED when both VM_READ opens failed with access denied, else INCONCLUSIVE.
/// Input: the reader's stdout. Output: the verdict and the lines.
fn verdict(stdout: &str) -> (&'static str, Vec<&str>) {
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("READ agentB ") || line.starts_with("READ helperB "))
        .collect();
    let verdict = if lines.iter().any(|line| line.contains("marker=yes")) {
        "READ"
    } else if lines.len() == 2 && lines.iter().all(|line| line.contains("open_vm=err5")) {
        "REFUSED"
    } else {
        "INCONCLUSIVE"
    };
    (verdict, lines)
}

/// The child the probe starts in a container. `hold` writes its pid and waits for a stop
/// file; `read` prints a [`report`] on itself and on each pid in its targets. Without the
/// mode variable, as in a normal test run, it does nothing.
#[test]
fn probe_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let folder = PathBuf::from(std::env::var(FOLDER).unwrap());
    match mode.as_str() {
        "hold" => {
            fs::write(folder.join("pid.part"), std::process::id().to_string()).unwrap();
            fs::rename(folder.join("pid.part"), folder.join("pid.txt")).unwrap();
            let start = Instant::now();
            while !folder.join("stop").exists() && start.elapsed() < Duration::from_secs(300) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        "read" => {
            println!("{}", report("self", std::process::id()));
            for target in std::env::var(TARGETS).unwrap().split(',') {
                let (label, pid) = target.split_once(':').unwrap();
                println!("{}", report(label, pid.parse().unwrap()));
            }
        }
        other => panic!("unknown probe mode {other}"),
    }
}

#[test]
fn an_agent_in_one_container_reads_another_containers_environment_or_not() {
    let held = case("crazyagent.test.envb");
    let other = case("crazyagent.test.enva");
    let log = fs::File::create(held.root.join("holder.log")).unwrap();
    let mut holder = agent(&held, "hold");
    holder
        .env(MARKER_NAME, MARKER)
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let mut child = holder.spawn().unwrap();
    let helper = child.id();
    let Some(pid) = waited_pid(&held.home) else {
        let _ = child.kill();
        panic!(
            "the holder never wrote its pid: {}",
            fs::read_to_string(held.root.join("holder.log")).unwrap_or_default()
        );
    };
    println!("PROBE helperB={helper} agentB={pid}");
    let before = [
        report("owner->agentB", pid),
        report("owner->helperB", helper),
    ];
    let targets = format!("agentB:{pid},helperB:{helper}");
    let mut reader = agent(&other, "read");
    reader.env(TARGETS, &targets);
    let cross = reader.output().unwrap();
    let mut sibling_reader = agent(&held, "read");
    sibling_reader.env(TARGETS, &targets);
    let sibling = sibling_reader.output().unwrap();
    let after = report("owner->agentB-after", pid);
    fs::write(held.home.join("stop"), "").unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    for line in before.iter().chain([&after]) {
        println!("{line}");
    }
    println!("CROSS {}", ran(&cross));
    println!("SIBLING {}", ran(&sibling));
    assert!(
        before.iter().all(|line| line.contains("marker=yes")),
        "owner control failed: {before:?}"
    );
    assert!(after.contains("marker=yes"), "holder gone: {after}");
    let cross_out = String::from_utf8_lossy(&cross.stdout).into_owned();
    let own = cross_out
        .lines()
        .find(|line| line.starts_with("READ self "))
        .unwrap_or_default();
    assert!(
        own.contains("env=ok") && own.contains("mode_seen=yes") && own.contains("marker=no"),
        "container A could not read its own environment: {}",
        ran(&cross)
    );
    let (cross_verdict, cross_lines) = verdict(&cross_out);
    println!("PROBE VERDICT cross-container {cross_verdict} {cross_lines:?}");
    let sibling_out = String::from_utf8_lossy(&sibling.stdout).into_owned();
    let (sibling_verdict, sibling_lines) = verdict(&sibling_out);
    println!("PROBE SIBLING same-container {sibling_verdict} {sibling_lines:?}");
}
