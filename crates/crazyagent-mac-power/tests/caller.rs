#![cfg(target_os = "macos")]

mod support;

use crazyagent_mac_power::caller::{
    Entitlements, PRODUCTION, Policy, REQUIREMENT, Refused, judge_entitlements,
};
use crazyagent_mac_power::helper::{Config, Helper};
use crazyagent_mac_power::protocol::{LidDisplay, Refusal, Reply, Request, encode_request};
use crazyagent_mac_power::server::{self, Accepted};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs};
use support::StandIn;

const ROLE: &str = "MAC_POWER_ROW_ROLE";
const SOCKET: &str = "MAC_POWER_ROW_SOCKET";
const FOLDER: &str = "MAC_POWER_ROW_FOLDER";
const PMSET: &str = "MAC_POWER_ROW_PMSET";
const CONSOLE: &str = "MAC_POWER_ROW_CONSOLE";
const OWNER: &str = "MAC_POWER_ROW_OWNER";
const NOW_MS: u64 = 1_000;
const EXIT_WAIT: Duration = Duration::from_secs(10);
const ANY_VALID_CODE: Policy = Policy {
    requirement: "always",
    runtime_required: false,
    entitlements_allowed: &[],
};
const GET_TASK_ALLOW: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
\"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist \
version=\"1.0\"><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>\n";

fn role() -> Option<String> {
    env::var(ROLE).ok()
}

fn child(program: &Path, test: &str, role: &str, socket: &Path) -> Command {
    let mut command = Command::new(program);
    command.args(["--exact", test, "--nocapture", "--test-threads", "1"]);
    command.env(ROLE, role).env(SOCKET, socket);
    command
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let started = Instant::now();
    while started.elapsed() < EXIT_WAIT {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    panic!("the child did not exit within {EXIT_WAIT:?}");
}

fn ask(stream: &mut UnixStream, request: &Request) -> String {
    stream.write_all(&encode_request(request)).unwrap();
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    line
}

#[test]
fn the_production_policy_pins_team_identifier_developer_id_runtime_and_no_entitlements() {
    assert_eq!(
        REQUIREMENT,
        "anchor apple generic and certificate leaf[subject.OU] = \"GK729B5HZQ\" and identifier \
         \"crazyagent\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate \
         leaf[field.1.2.840.113635.100.6.1.13] exists"
    );
    let expected = Policy {
        requirement: REQUIREMENT,
        runtime_required: true,
        entitlements_allowed: &[],
    };
    assert_eq!(PRODUCTION, expected);
}

#[test]
fn a_caller_without_our_team_and_identifier_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let _client = UnixStream::connect(&path).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let refused = Accepted::take(stream, &PRODUCTION);
    assert!(matches!(refused, Err(Refused::Requirement)), "{refused:?}");
    let _second = UnixStream::connect(&path).unwrap();
    let (stream, _) = listener.accept().unwrap();
    assert!(
        Accepted::take(stream, &ANY_VALID_CODE).is_ok(),
        "the same caller, any valid code"
    );
}

#[test]
fn a_verified_caller_gets_one_reply_and_the_connection_closes() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let path = stand_in.path("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut client = UnixStream::connect(&path).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let accepted = Accepted::take(stream, &ANY_VALID_CODE);
    assert!(accepted.is_ok(), "{accepted:?}");
    client.write_all(&encode_request(&Request::Read)).unwrap();
    let served = accepted
        .unwrap()
        .serve(&ANY_VALID_CODE, &mut helper, NOW_MS);
    assert!(matches!(served, Ok(Reply::State(_))), "{served:?}");
    let mut text = String::new();
    client.read_to_string(&mut text).unwrap();
    assert!(text.starts_with("{\"reply\":\"state\""), "{text}");
    assert_eq!(text.matches('\n').count(), 1);
}

#[test]
fn exec_after_connect_role() {
    if role().as_deref() != Some("exec") {
        return;
    }
    let stream = UnixStream::connect(env::var(SOCKET).unwrap()).unwrap();
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    let request = String::from_utf8(encode_request(&Request::Read)).unwrap();
    assert!(!request.contains('\''));
    let script = format!("printf '%s\\n' '{}'; /bin/sleep 3", request.trim_end());
    let input = OwnedFd::from(stream.try_clone().unwrap());
    let error = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(OwnedFd::from(stream)))
        .exec();
    panic!("exec failed: {error}");
}

#[test]
fn a_caller_that_execs_after_connect_is_refused() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let path = stand_in.path("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let program = env::current_exe().unwrap();
    let mut caller = child(&program, "exec_after_connect_role", "exec", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let accepted = Accepted::take(stream, &ANY_VALID_CODE);
    assert!(
        accepted.is_ok(),
        "before exec the caller is valid code: {accepted:?}"
    );
    caller.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let served = accepted
        .unwrap()
        .serve(&ANY_VALID_CODE, &mut helper, NOW_MS);
    assert!(matches!(served, Err(Refused::NotFound)), "{served:?}");
    caller.kill().ok();
    caller.wait().ok();
}

#[test]
fn connect_and_read_role() {
    if role().as_deref() != Some("read") {
        return;
    }
    let mut stream = UnixStream::connect(env::var(SOCKET).unwrap()).unwrap();
    stream.write_all(&encode_request(&Request::Read)).unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).ok();
}

fn signed_copy(folder: &Path, entitlements: Option<&str>) -> PathBuf {
    let copy = folder.join("caller-copy");
    fs::copy(env::current_exe().unwrap(), &copy).unwrap();
    let mut codesign = Command::new("/usr/bin/codesign");
    codesign.args(["-f", "-s", "-"]);
    if let Some(text) = entitlements {
        let plist = folder.join("entitlements.plist");
        fs::write(&plist, text).unwrap();
        codesign.arg("--entitlements").arg(plist);
    }
    let status = codesign.arg(&copy).status().unwrap();
    assert!(status.success(), "codesign {status}");
    copy
}

#[test]
fn a_caller_signed_with_get_task_allow_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let plain = signed_copy(folder.path(), None);
    let mut first = child(&plain, "connect_and_read_role", "read", &path)
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    assert!(
        Accepted::take(stream, &ANY_VALID_CODE).is_ok(),
        "the copy without entitlements"
    );
    wait_exit(&mut first);
    fs::remove_file(&plain).unwrap();

    let debuggable = signed_copy(folder.path(), Some(GET_TASK_ALLOW));
    let mut second = child(&debuggable, "connect_and_read_role", "read", &path)
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let refused = Accepted::take(stream, &ANY_VALID_CODE);
    let expected = Refused::Entitlement("com.apple.security.get-task-allow".to_string());
    assert_eq!(refused.err(), Some(expected));
    wait_exit(&mut second);
}

#[test]
fn server_role() {
    if role().as_deref() != Some("server") {
        return;
    }
    let config = Config {
        folder: PathBuf::from(env::var(FOLDER).unwrap()),
        owner_uid: env::var(OWNER).unwrap().parse().unwrap(),
        pmset: PathBuf::from(env::var(PMSET).unwrap()),
        console: PathBuf::from(env::var(CONSOLE).unwrap()),
    };
    let listener = UnixListener::bind(env::var(SOCKET).unwrap()).unwrap();
    println!("ready");
    std::process::exit(server::run(listener, config, ANY_VALID_CODE));
}

#[test]
fn a_term_signal_puts_every_journaled_value_back() {
    let stand_in = StandIn::new(0);
    let config = stand_in.config();
    let path = stand_in.path("power.sock");
    let program = env::current_exe().unwrap();
    let mut server = child(&program, "server_role", "server", &path)
        .env(FOLDER, &config.folder)
        .env(OWNER, config.owner_uid.to_string())
        .env(PMSET, &config.pmset)
        .env(CONSOLE, &config.console)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(server.stdout.take().unwrap()).lines();
    let ready = lines.find(|line| matches!(line.as_deref().map(str::trim), Ok("ready")));
    assert!(ready.is_some(), "the server child never said ready");
    let mut stream = UnixStream::connect(&path).unwrap();
    let hold = Request::Hold {
        lease_s: 120,
        lid_display: LidDisplay::LeaveOn,
        low_power: false,
    };
    let held = ask(&mut stream, &hold);
    assert!(held.starts_with("{\"reply\":\"held\""), "{held}");
    assert_eq!(stand_in.sleep_disabled(), 1);
    let pid = server.id().to_string();
    let killed = Command::new("/bin/kill")
        .args(["-TERM", &pid])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = wait_exit(&mut server);
    assert_eq!(status.code(), Some(0));
    assert_eq!(stand_in.sleep_disabled(), 0);
    assert_eq!(
        stand_in.sleep_writes(),
        ["-a disablesleep 1", "-a disablesleep 0"]
    );
    assert_eq!(stand_in.journal_text().as_deref(), Some("[]"));
}

const ANY_VALID_RUNTIME: Policy = Policy {
    requirement: "always",
    runtime_required: true,
    entitlements_allowed: &[],
};
const TRICKLE_BYTES: usize = 8;
const TRICKLE_GAP: Duration = Duration::from_millis(400);
const CUT_OFF_MAX: Duration = Duration::from_millis(2_500);

#[test]
fn entitlements_in_another_form_than_a_dictionary_are_refused() {
    assert_eq!(
        judge_entitlements(Entitlements::Names(Vec::new()), &PRODUCTION),
        Ok(())
    );
    assert_eq!(
        judge_entitlements(Entitlements::OtherForm, &PRODUCTION),
        Err(Refused::SigningInformation)
    );
    let allowed = Policy {
        entitlements_allowed: &["com.apple.security.cs.allow-jit"],
        ..PRODUCTION
    };
    assert_eq!(
        judge_entitlements(Entitlements::OtherForm, &allowed),
        Err(Refused::SigningInformation),
        "an allow list cannot read a form it cannot name"
    );
    let names = vec![
        "com.apple.security.get-task-allow".to_string(),
        "com.apple.security.cs.allow-jit".to_string(),
    ];
    assert_eq!(
        judge_entitlements(Entitlements::Names(names), &allowed),
        Err(Refused::Entitlement(
            "com.apple.security.get-task-allow".to_string()
        ))
    );
}

#[test]
fn a_caller_that_trickles_its_request_is_cut_off_two_seconds_after_accept() {
    let stand_in = StandIn::new(0);
    let mut helper = Helper::start(stand_in.config());
    let path = stand_in.path("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut client = UnixStream::connect(&path).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let accepted = Accepted::take(stream, &ANY_VALID_CODE);
    assert!(accepted.is_ok(), "{accepted:?}");
    let trickle = std::thread::spawn(move || {
        for _ in 0..TRICKLE_BYTES {
            if client.write_all(b" ").is_err() {
                break;
            }
            std::thread::sleep(TRICKLE_GAP);
        }
        client
    });
    let started = Instant::now();
    let served = accepted
        .unwrap()
        .serve(&ANY_VALID_CODE, &mut helper, NOW_MS);
    let took = started.elapsed();
    let expected = Reply::Refused {
        reason: Refusal::Malformed,
    };
    assert_eq!(served, Ok(expected));
    assert!(
        took < CUT_OFF_MAX,
        "one deadline from accept, not one per read: {took:?}"
    );
    drop(trickle.join().unwrap());
}

fn runtime_copy(folder: &Path) -> PathBuf {
    let copy = folder.join("runtime-copy");
    fs::copy(env::current_exe().unwrap(), &copy).unwrap();
    let status = Command::new("/usr/bin/codesign")
        .args(["-f", "-s", "-", "-o", "runtime"])
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success(), "codesign {status}");
    copy
}

#[test]
fn a_policy_that_requires_the_hardened_runtime_refuses_a_caller_without_it() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("power.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let plain = signed_copy(folder.path(), None);
    let mut first = child(&plain, "connect_and_read_role", "read", &path)
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let refused = Accepted::take(stream, &ANY_VALID_RUNTIME);
    assert_eq!(refused.err(), Some(Refused::NoRuntime));
    wait_exit(&mut first);

    let hardened = runtime_copy(folder.path());
    let mut second = child(&hardened, "connect_and_read_role", "read", &path)
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let accepted = Accepted::take(stream, &ANY_VALID_RUNTIME);
    assert!(
        accepted.is_ok(),
        "a runtime-signed copy carries 0x10000: {accepted:?}"
    );
    drop(accepted);
    wait_exit(&mut second);
}
