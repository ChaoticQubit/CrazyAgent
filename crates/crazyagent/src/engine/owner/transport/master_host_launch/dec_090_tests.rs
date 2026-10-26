use super::master_command;
use crate::adapters::codex::CodexProtocol;
use crate::team::runtime::managed_agent::{ManagedAgent, codex};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const MARKER: &str = "DEC090-PLANTED";
const REPLY_WAIT: Duration = Duration::from_secs(60);
const SKILLS_LIST_ID: i64 = 9001;

/// The real Codex app-server of one master turn, killed with its whole process group
/// when the test lets go of it on every path out.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let pid = rustix::process::Pid::from_raw(self.0.id().try_into().unwrap()).unwrap();
        assert!(pid.as_raw_nonzero().get() > 1);
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        let _ = self.0.wait();
    }
}

/// Reads app-server lines until the reply to `request` arrives, handing each line to
/// the protocol while it follows the session. Input: the line channel, the encoded
/// request and the protocol. Output: the reply, or a panic at the deadline.
fn reply(
    lines: &Receiver<String>,
    request: &[u8],
    mut protocol: Option<&mut CodexProtocol>,
) -> Value {
    let id = serde_json::from_slice::<Value>(request).unwrap()["id"].clone();
    assert!(id.is_i64(), "{id}");
    let deadline = Instant::now() + REPLY_WAIT;
    loop {
        let line = lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("the Codex master sent no reply to request {id}"));
        if let Some(protocol) = protocol.as_deref_mut() {
            protocol
                .receive(format!("{line}\n").as_bytes())
                .expect("the real app-server spoke outside the protocol");
        }
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["id"] == id && value.get("method").is_none() {
            return value;
        }
    }
}

/// Writes one encoded request to the app-server. Input: its stdin and the bytes.
fn send(input: &mut impl Write, bytes: &[u8]) {
    input.write_all(bytes).unwrap();
    if !bytes.ends_with(b"\n") {
        input.write_all(b"\n").unwrap();
    }
    input.flush().unwrap();
}

/// Reads the path one reported instruction source names, as text or as an object.
fn source_path(source: &Value) -> Option<PathBuf> {
    source
        .as_str()
        .or_else(|| source["path"].as_str())
        .map(PathBuf::from)
}

#[test]
fn a_codex_master_loads_no_instructions_or_skills_from_where_a_tools_on_agent_writes() {
    let codex = codex();
    let agent = ManagedAgent::new("codex", &codex);
    let profile = format!("'{}'", agent.profile_home.display());
    let plant = format!(
        "skill() {{ mkdir -p \"$1/dec090-planted\" && printf '%s\\n' '---' 'name: dec090-planted' 'description: {MARKER}' '---' '{MARKER}' > \"$1/dec090-planted/SKILL.md\"; }}\n\
         for home in {profile} \"$HOME\"; do\n\
         (printf '{MARKER}\\n' > \"$home/AGENTS.md\") 2>/dev/null && echo \"planted $home/AGENTS.md\"\n\
         (printf '{MARKER}\\n' > \"$home/AGENTS.override.md\") 2>/dev/null && echo \"planted $home/AGENTS.override.md\"\n\
         (skill \"$home/skills\") 2>/dev/null && echo \"planted $home/skills\"\n\
         (skill \"$home/.agents/skills\") 2>/dev/null && echo \"planted $home/.agents/skills\"\n\
         done\n"
    );
    let planted = agent.shell(&plant);
    let planted = String::from_utf8_lossy(&planted.stdout).into_owned();
    let mut command = master_command(
        &codex,
        &agent.profile_home,
        &agent.project,
        "native-default",
        None,
    )
    .unwrap();
    command.stderr(Stdio::null());
    let mut server = Server(crate::spawn_gate::spawn(&mut command).unwrap());
    let mut input = server.0.stdin.take().unwrap();
    let output = server.0.stdout.take().unwrap();
    let (sender, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut protocol = CodexProtocol::new();
    let request = protocol.initialize().unwrap();
    send(&mut input, &request);
    reply(&lines, &request, Some(&mut protocol));
    send(&mut input, &protocol.initialized().unwrap());
    let request = protocol
        .start_instructed_thread(&agent.project, "Reply with the word ready.")
        .unwrap();
    send(&mut input, &request);
    let thread = reply(&lines, &request, Some(&mut protocol));
    let request = serde_json::to_vec(&json!({
        "id": SKILLS_LIST_ID,
        "method": "skills/list",
        "params": {}
    }))
    .unwrap();
    send(&mut input, &request);
    let skills = reply(&lines, &request, None);
    drop(input);
    drop(server);
    let sources = thread["result"]["instructionSources"]
        .as_array()
        .unwrap_or_else(|| panic!("thread/start reported no instruction sources: {thread}"));
    let listed = skills["result"]["data"]
        .as_array()
        .unwrap_or_else(|| panic!("skills/list reported no skills: {skills}"))
        .iter()
        .flat_map(|entry| entry["skills"].as_array().cloned().unwrap_or_default())
        .collect::<Vec<_>>();
    let mut problems = Vec::new();
    let mut loaded = Vec::new();
    for source in sources {
        let Some(path) = source_path(source) else {
            problems.push(format!("unreadable instruction source {source}"));
            continue;
        };
        if std::fs::read_to_string(&path).is_ok_and(|text| text.contains(MARKER)) {
            problems.push(format!(
                "planted instructions loaded from {}",
                path.display()
            ));
        }
        loaded.push(path);
    }
    for skill in &listed {
        let path = skill["path"].as_str().map(PathBuf::from);
        let text = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        if skill.to_string().contains(MARKER) || text.contains(MARKER) {
            problems.push(format!("planted skill listed: {skill}"));
        }
        match path {
            Some(path) => loaded.push(path),
            None => problems.push(format!("skill without a path: {skill}")),
        }
    }
    let probe = loaded
        .iter()
        .map(|path| {
            let text = path.to_str().unwrap();
            assert!(!text.contains('\''), "{text}");
            let folder = path.parent().unwrap_or(Path::new("/")).display();
            format!(
                "(: >> '{text}') 2>/dev/null && echo \"agent can write {text}\"\n\
                 (mkdir '{folder}/.dec-090-probe' && rmdir '{folder}/.dec-090-probe') 2>/dev/null && echo \"agent can write in {folder}\"\n"
            )
        })
        .collect::<String>();
    let writable = agent.shell(&probe);
    problems.extend(
        String::from_utf8_lossy(&writable.stdout)
            .lines()
            .map(str::to_owned),
    );
    assert!(
        problems.is_empty(),
        "a Codex master on the managed profile home {} loads what a tools-on agent writes:\n{}\nagent planted:\n{planted}",
        agent.profile_home.display(),
        problems.join("\n")
    );
}
