use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::windows::ConptyHost;
use crate::terminal::{AgentId, Host};
use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Instant;

const CONNECT: &str = "PROBE_CONNECT_PORT";
const EXEMPT: &str = "PROBE_LOOPBACK_EXEMPT";
const SEEN_WAIT: Duration = Duration::from_secs(30);
const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// Turns a command into the pane command a terminal host starts: its program and arguments
/// as text, its folder and the variables it sets. Input: the command, plain or wrapped by
/// the boundary. Output: the pane command, with no transcript.
fn pane_command(command: &Command) -> InteractiveCommand {
    let argv = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_str().unwrap().to_owned())
        .collect();
    let env = command
        .get_envs()
        .filter_map(|(name, value)| Some((name.to_str()?.to_owned(), value?.to_str()?.to_owned())))
        .collect();
    InteractiveCommand {
        argv,
        cwd: command.get_current_dir().unwrap().to_path_buf(),
        env,
        transcript: Transcript::Unavailable,
    }
}

/// Waits until a pane's screen shows `text` at least `times` times, or [`SEEN_WAIT`] ends.
/// Inputs: the host, the pane, the text and the count. Output: whether it was seen, and the
/// last screen as text.
fn shows(host: &dyn Host, agent: &AgentId, text: &str, times: usize) -> (bool, String) {
    assert!(times > 0);
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < SEEN_WAIT {
        if let Ok(screen) = host.screen(agent) {
            last = String::from_utf8_lossy(&screen).into_owned();
            if last.matches(text).count() >= times {
                return (true, last);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    (false, last)
}

/// P5 (NOTE-0207): a shell started through the real boundary helper on a ConPTY pane, as an
/// agent pane would start: does it draw, take typed keys and stay in its container, beside
/// the same shell started plainly as the control. Only the control is asserted, so a broken
/// host fails the row instead of printing a false fact; each fact is one PROBE line.
#[test]
fn p5_a_contained_shell_on_a_pseudoconsole() {
    let folders = folders();
    let host = ConptyHost::new();
    let mut plain = Command::new(cmd());
    plain.arg("/K").current_dir(&folders.project);
    let control = AgentId::new("p5-control").unwrap();
    host.spawn(&control, &pane_command(&plain)).unwrap();
    host.input(&control, b"echo p5 control 4242\r").unwrap();
    let (seen, screen) = shows(&host, &control, "p5 control 4242", 2);
    assert!(seen, "the plain control shell never echoed: {screen}");
    println!("PROBE P5 control: echoed");
    host.kill(&control).unwrap();
    let installed = WindowsSandbox::install(
        policy(&folders, SandboxNetwork::Only(Vec::new())),
        &helper(),
        &folders.profiles.join("agent.sb"),
        &folders.grants,
    )
    .unwrap();
    let mut shell = Command::new(cmd());
    shell.arg("/K").current_dir(&folders.project);
    let wrapped = Sandbox::wrap(&installed, &shell).unwrap();
    let agent = AgentId::new("p5-agent").unwrap();
    if let Err(error) = host.spawn(&agent, &pane_command(&wrapped)) {
        println!("PROBE P5 spawn: refused {error:?}");
        return;
    }
    println!("PROBE P5 spawn: started");
    let (drawn, screen) = shows(&host, &agent, ">", 1);
    println!("PROBE P5 screen: drawn={drawn} {:?}", screen.trim());
    host.input(&agent, b"echo p5 typed 4242\r").unwrap();
    let (typed, screen) = shows(&host, &agent, "p5 typed 4242", 2);
    println!("PROBE P5 keys: echoed={typed} {:?}", screen.trim());
    let brief = folders.project.join("context").join("brief.md");
    host.input(&agent, b"echo p5>p5.txt\r").unwrap();
    host.input(
        &agent,
        format!("echo changed>\"{}\"\r", brief.display()).as_bytes(),
    )
    .unwrap();
    host.input(&agent, b"echo p5 done 4242\r").unwrap();
    let (done, _) = shows(&host, &agent, "p5 done 4242", 2);
    println!(
        "PROBE P5 contained: done={done} project_write={} protected_unchanged={}",
        folders.project.join("p5.txt").is_file(),
        fs::read_to_string(&brief).unwrap() == "brief\r\n"
    );
    host.kill(&agent).unwrap();
}

/// The loopback probe command: the test binary copy run as [`net_child`] with the port, in
/// the project folder. Inputs: the copy, the folders and the port. Output: the command,
/// still to be wrapped for a container run.
fn net_command(probe: &Path, folders: &Folders, port: u16) -> Command {
    let mut command = Command::new(probe);
    command
        .args([
            "sandbox::windows::tests::probes::net_child",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CONNECT, port.to_string())
        .current_dir(&folders.project);
    command
}

/// Runs a loopback probe command and keeps its one NET line. Input: the command, plain or
/// wrapped. Output: the line, or what the run printed and how it ended when no line came.
fn net_line(mut command: Command) -> String {
    let output = crate::spawn_gate::collected(&mut command).unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.lines()
        .find(|line| line.starts_with("NET "))
        .map_or_else(
            || format!("no NET line, {}: {}", output.status, text.trim()),
            str::to_owned,
        )
}

/// Runs CheckNetIsolation with one loopback exemption verb for a container by name.
/// Inputs: the verb (`-a` adds, `-d` removes) and the container name. Output: the exit and
/// what it printed, as one line.
fn exemption(verb: &str, container: &str) -> String {
    let tool = PathBuf::from(system_root())
        .join("System32")
        .join("CheckNetIsolation.exe");
    let mut command = Command::new(tool);
    command.args(["LoopbackExempt", verb, &format!("-n={container}")]);
    let output = crate::spawn_gate::collected(&mut command).unwrap();
    format!(
        "{} {}",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim()
    )
}

/// P6 (NOTE-0207): does a container started through the real helper reach a port the engine
/// listens on at 127.0.0.1, with the internet client capability and with no network, beside
/// an unconfined copy as the control. With PROBE_LOOPBACK_EXEMPT set it also exempts the
/// open container through CheckNetIsolation, tries again and removes the exemption.
#[test]
fn p6_a_container_and_the_engine_loopback_port() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(b"ok");
        }
    });
    let folders = folders();
    let probe = folders.home.join("net-call.exe");
    fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
    let control = net_line(net_command(&probe, &folders, port));
    assert!(
        control.starts_with("NET connected"),
        "the unconfined control did not connect: {control}"
    );
    println!("PROBE P6 control: {control}");
    for (label, network) in [
        ("open", SandboxNetwork::Open),
        ("none", SandboxNetwork::Only(Vec::new())),
    ] {
        let profile = folders.profiles.join(format!("{label}.sb"));
        let installed = WindowsSandbox::install(
            policy(&folders, network),
            &helper(),
            &profile,
            &folders.grants,
        )
        .unwrap();
        let contained =
            || net_line(Sandbox::wrap(&installed, &net_command(&probe, &folders, port)).unwrap());
        println!("PROBE P6 {label}: {}", contained());
        if label == "open" && std::env::var_os(EXEMPT).is_some() {
            let container = BoundaryProfile::load(&profile).unwrap().container;
            println!("PROBE P6 exempt add: {}", exemption("-a", &container));
            println!("PROBE P6 open exempt: {}", contained());
            println!("PROBE P6 exempt remove: {}", exemption("-d", &container));
        }
    }
}

/// The loopback child P6 starts: connects to 127.0.0.1 at the port in [`CONNECT`] within
/// [`CONNECT_WAIT`] and prints one NET line with what it read or the error and its code.
/// Without the variable, as in a normal test run, it does nothing.
#[test]
fn net_child() {
    let Ok(port) = std::env::var(CONNECT) else {
        return;
    };
    let address = SocketAddr::from(([127, 0, 0, 1], port.parse::<u16>().unwrap()));
    match TcpStream::connect_timeout(&address, CONNECT_WAIT) {
        Ok(mut stream) => {
            let mut reply = [0u8; 2];
            let read = stream
                .read(&mut reply)
                .map(|count| String::from_utf8_lossy(&reply[..count]).into_owned());
            println!("NET connected read={read:?}");
        }
        Err(error) => println!("NET refused {error} code={:?}", error.raw_os_error()),
    }
}
