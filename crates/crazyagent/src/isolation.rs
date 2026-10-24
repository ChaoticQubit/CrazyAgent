use crate::qualification::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OUTPUT_MAX: u64 = 64 * 1024;
const SENTINEL: &str = "disposable trusted fixture\n";
const CREDENTIAL: &str = "synthetic nonsecret credential fixture\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxProfile {
    pub id: String,
    pub runtime_path: PathBuf,
    pub node_path: PathBuf,
    pub grants: Vec<PathBuf>,
    pub network_policy: NetworkPolicy,
    pub credential_refs: Vec<PathBuf>,
    pub resource_limits: ResourceLimits,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NetworkPolicy {
    DenyAll,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_seconds: u32,
    pub memory_bytes: u64,
    pub processes: u32,
    pub wall_time_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryEvidence {
    pub profile_id: String,
    pub allowed: BTreeMap<String, bool>,
    pub denied: BTreeMap<String, bool>,
    pub unenforced: Vec<String>,
    pub artifacts: Vec<PathBuf>,
    pub versions: BTreeMap<String, String>,
    pub measurements: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObservation {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_resource_probe(
    child_executable: &Path,
    evidence_root: &Path,
) -> Result<BTreeMap<String, ResourceObservation>, BoundaryError> {
    if !clean_absolute(child_executable) || !child_executable.is_file() {
        return Err(BoundaryError::Unavailable(
            "resource child executable missing".into(),
        ));
    }
    let mut observations = BTreeMap::new();
    for role in [
        "memory-baseline",
        "process-baseline",
        "limit-as-small",
        "limit-as",
        "limit-rss",
        "limit-nproc",
        "limit-cpu",
    ] {
        let output = evidence_root.join(format!("{role}.stdout"));
        let started = Instant::now();
        let status = bounded_command(
            child_executable,
            child_executable,
            &[role.into()],
            evidence_root,
            &output,
            Duration::from_secs(6),
        )?;
        let observation = ResourceObservation {
            exit_code: status.code(),
            signal: status.signal(),
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap(),
            stdout: read_bounded(&output)?,
            stderr: read_bounded(&output.with_extension("stderr"))?,
        };
        observations.insert(role.into(), observation);
    }
    write_json(&evidence_root.join("resource-evidence.json"), &observations)?;
    Ok(observations)
}

#[derive(Debug)]
pub enum BoundaryError {
    InvalidProfile(String),
    Unavailable(String),
    Io(io::Error),
    Failed(Box<BoundaryEvidence>),
    Unenforced(Box<BoundaryEvidence>),
}

impl fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfile(reason) => write!(f, "invalid boundary profile: {reason}"),
            Self::Unavailable(reason) => write!(f, "UNAVAILABLE: {reason}"),
            Self::Io(error) => write!(f, "boundary I/O: {error}"),
            Self::Failed(evidence) => write!(f, "boundary controls failed: {evidence:?}"),
            Self::Unenforced(evidence) => write!(f, "required controls unenforced: {evidence:?}"),
        }
    }
}

impl std::error::Error for BoundaryError {}

impl From<io::Error> for BoundaryError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl SandboxProfile {
    pub fn validate(&self, candidate: &Path, trusted: &Path) -> Result<(), BoundaryError> {
        let invalid = |reason: &str| BoundaryError::InvalidProfile(reason.into());
        if self.id.trim().is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control)
        {
            return Err(invalid("profile id must be 1..128 noncontrol bytes"));
        }
        for root in [candidate, trusted] {
            if !clean_absolute(root)
                || !root.is_dir()
                || root.canonicalize()? != root
                || root.parent().is_none()
            {
                return Err(invalid(
                    "fixture roots must be canonical absolute directories",
                ));
            }
        }
        if candidate == trusted || candidate.parent() != trusted.parent() {
            return Err(invalid("fixture roots must be distinct siblings"));
        }
        if self.grants != [candidate] {
            return Err(invalid("the only write grant must be the candidate root"));
        }
        if self.credential_refs.is_empty()
            || self.credential_refs.len() > 4
            || self.credential_refs.iter().any(|path| {
                !clean_absolute(path)
                    || path.parent() != Some(trusted)
                    || path.file_name().is_none()
                    || path.file_name().is_some_and(|name| name == "sentinel")
            })
        {
            return Err(invalid(
                "1..4 synthetic credential references must be direct trusted children",
            ));
        }
        let mut credentials = self.credential_refs.clone();
        credentials.sort();
        credentials.dedup();
        if credentials.len() != self.credential_refs.len() {
            return Err(invalid("duplicate credential reference"));
        }
        for executable in [&self.runtime_path, &self.node_path] {
            if !clean_absolute(executable) {
                return Err(invalid(
                    "runtime and Node paths must be absolute without traversal",
                ));
            }
        }
        let limits = &self.resource_limits;
        if !(1..=5).contains(&limits.cpu_seconds)
            || !(16 * 1024 * 1024..=256 * 1024 * 1024).contains(&limits.memory_bytes)
            || !(2..=8).contains(&limits.processes)
            || !(1..=15).contains(&limits.wall_time_seconds)
        {
            return Err(invalid(
                "probe limits exceed the approved bounded qualification envelope",
            ));
        }
        Ok(())
    }
}

impl BoundaryEvidence {
    pub fn admissible(&self) -> bool {
        self.confinement_outcome() == Outcome::Proven
    }

    pub fn confinement_outcome(&self) -> Outcome {
        let observed = [
            "candidate-write",
            "child-candidate-write",
            "loopback-control",
        ]
        .iter()
        .all(|name| self.allowed.get(*name) == Some(&true))
            && [
                "trusted-write",
                "traversal-escape",
                "symlink-escape",
                "child-trusted-write",
                "credential-read",
                "forbidden-network",
            ]
            .iter()
            .all(|name| self.denied.get(*name) == Some(&true));
        if !observed {
            Outcome::Failed
        } else if !self.unenforced.is_empty() {
            Outcome::Unavailable
        } else {
            Outcome::Proven
        }
    }
}

fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 1024
        && path.components().count() <= 32
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && path
            .to_str()
            .is_some_and(|s| !s.chars().any(char::is_control))
}

pub fn run_boundary_probe(
    profile: &SandboxProfile,
    candidate_root: &Path,
    trusted_root: &Path,
) -> Result<BoundaryEvidence, BoundaryError> {
    profile.validate(candidate_root, trusted_root)?;
    for executable in [&profile.runtime_path, &profile.node_path] {
        if !executable.is_file() {
            return Err(BoundaryError::Unavailable(format!(
                "executable missing: {}",
                executable.display()
            )));
        }
    }
    if fs::read_dir(candidate_root)?.next().is_some()
        || fs::read_dir(trusted_root)?.next().is_some()
    {
        return Err(BoundaryError::InvalidProfile(
            "probe requires empty disposable roots".into(),
        ));
    }
    let evidence_root = candidate_root.parent().unwrap().join("boundary-evidence");
    fs::create_dir(&evidence_root)?;
    let result = collect_boundary(profile, candidate_root, trusted_root, &evidence_root);
    match &result {
        Ok(evidence) => {
            write_json(&evidence_root.join("evidence.json"), evidence)?;
        }
        Err(BoundaryError::Failed(evidence) | BoundaryError::Unenforced(evidence)) => {
            write_json(&evidence_root.join("evidence.json"), evidence)?;
        }
        Err(error) => {
            write_new(
                &evidence_root.join("unavailable.txt"),
                error.to_string().as_bytes(),
            )?;
        }
    }
    result
}

fn collect_boundary(
    profile: &SandboxProfile,
    candidate: &Path,
    trusted: &Path,
    artifacts: &Path,
) -> Result<BoundaryEvidence, BoundaryError> {
    let timeout = Duration::from_secs(u64::from(profile.resource_limits.wall_time_seconds));
    let mut versions = BTreeMap::new();
    for (name, executable, arguments) in [
        ("node", &profile.node_path, vec!["--version".into()]),
        (
            "sandbox-runtime",
            &profile.runtime_path,
            vec!["--version".into()],
        ),
    ] {
        let output = artifacts.join(format!("{name}-version"));
        let status = bounded_command(
            &profile.node_path,
            executable,
            &arguments,
            candidate,
            &output,
            timeout,
        )?;
        let version = read_bounded(&output)?.trim().to_string();
        if !status.success() || (name == "sandbox-runtime" && version != "0.0.80") {
            return Err(BoundaryError::Unavailable(format!(
                "{name} version check failed: {version}; artifacts {}",
                artifacts.display()
            )));
        }
        versions.insert(name.into(), version);
    }
    write_new(&trusted.join("sentinel"), SENTINEL.as_bytes())?;
    for credential in &profile.credential_refs {
        write_new(credential, CREDENTIAL.as_bytes())?;
    }
    std::os::unix::fs::symlink(trusted, candidate.join("escape"))?;
    let settings = serde_json::json!({
        "network": {"allowedDomains": [], "deniedDomains": [],
            "allowUnixSockets": [], "allowAllUnixSockets": false, "allowLocalBinding": false},
        "filesystem": {"allowWrite": [candidate], "denyWrite": [trusted, artifacts],
            "denyRead": profile.credential_refs, "allowRead": []}
    });
    write_json(&artifacts.join("settings.json"), &settings)?;
    write_json(&artifacts.join("profile.json"), profile)?;
    write_new(&artifacts.join("probe.cjs"), PROBE.as_bytes())?;
    let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).map_err(|error| {
        BoundaryError::Unavailable(format!(
            "loopback listener: {error}; artifacts {}",
            artifacts.display()
        ))
    })?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let positive = crate::spawn_gate::connect_tcp_std_timeout(&address, Duration::from_secs(1))?;
    let accepted = accept_control(&listener, Duration::from_secs(1))?;
    drop(positive);
    drop(accepted);
    let args = vec![
        "--settings".into(),
        artifacts.join("settings.json").into_os_string(),
        "--".into(),
        profile.node_path.as_os_str().into(),
        artifacts.join("probe.cjs").into_os_string(),
        candidate.as_os_str().into(),
        trusted.as_os_str().into(),
        artifacts.join("profile.json").into_os_string(),
        address.port().to_string().into(),
    ];
    let output = artifacts.join("runtime.stdout");
    let status = bounded_command(
        &profile.node_path,
        &profile.runtime_path,
        &args,
        candidate,
        &output,
        timeout,
    )?;
    let result_path = candidate.join("probe-result.json");
    if !status.success() || !result_path.is_file() {
        return Err(BoundaryError::Unavailable(format!(
            "srt launch/probe failed ({status}); {}; artifacts {}",
            read_bounded(&output.with_extension("stderr"))?,
            artifacts.display()
        )));
    }
    let raw: ProbeResult = serde_json::from_str(&read_bounded(&result_path)?).map_err(|error| {
        BoundaryError::Unavailable(format!("invalid native observations: {error}"))
    })?;
    if versions.get("node") != Some(&raw.node_version) {
        return Err(BoundaryError::Unavailable(
            "child Node version differs from selected Node".into(),
        ));
    }
    versions.insert("child-node".into(), raw.node_version);
    let mut evidence = BoundaryEvidence {
        profile_id: profile.id.clone(),
        allowed: BTreeMap::new(),
        denied: BTreeMap::new(),
        unenforced: vec![
            "cpu-seconds".into(),
            "memory-bytes".into(),
            "process-count".into(),
            "engine-endpoint-not-yet-implemented".into(),
            "runtime-default-write-exceptions-not-qualified".into(),
        ],
        artifacts: vec![artifacts.into(), result_path],
        versions,
        measurements: raw.measurements,
    };
    record_case(&mut evidence.allowed, "loopback-control", true);
    for (name, file, expected) in [
        ("candidate-write", "candidate-output", "candidate"),
        ("child-candidate-write", "child-output", "child"),
    ] {
        record_case(
            &mut evidence.allowed,
            name,
            fs::read(candidate.join(file)).is_ok_and(|v| v == expected.as_bytes()),
        );
    }
    let sentinel_intact = fs::read(trusted.join("sentinel"))? == SENTINEL.as_bytes();
    for name in [
        "trusted-write",
        "traversal-escape",
        "symlink-escape",
        "child-trusted-write",
    ] {
        record_case(
            &mut evidence.denied,
            name,
            sentinel_intact && raw.denied.get(name) == Some(&true),
        );
    }
    record_case(
        &mut evidence.denied,
        "credential-read",
        raw.denied.get("credential-read") == Some(&true)
            && !candidate.join("credential-copy").exists()
            && profile
                .credential_refs
                .iter()
                .all(|path| fs::read(path).is_ok_and(|v| v == CREDENTIAL.as_bytes())),
    );
    let no_connection = match crate::spawn_gate::accept_tcp_std(&listener) {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
        Ok(_) => false,
        Err(error) => return Err(error.into()),
    };
    record_case(
        &mut evidence.denied,
        "forbidden-network",
        no_connection && raw.denied.get("forbidden-network") == Some(&true),
    );
    if evidence
        .allowed
        .values()
        .chain(evidence.denied.values())
        .any(|v| !v)
    {
        return Err(BoundaryError::Failed(Box::new(evidence)));
    }
    Err(BoundaryError::Unenforced(Box::new(evidence)))
}

/** Measures a disposable engine boundary with real worker and child operations.
Inputs are a validated fixture profile, sibling roots and the owned engine child.
Returns retained observations as failed or resource-unqualified evidence.
*/
pub fn run_engine_boundary_probe(
    profile: &SandboxProfile,
    candidate: &Path,
    trusted: &Path,
    engine: &mut std::process::Child,
) -> Result<BoundaryEvidence, BoundaryError> {
    use crate::protocol::*;

    profile.validate(candidate, trusted)?;
    if engine.try_wait()?.is_some() {
        return Err(BoundaryError::Unavailable(
            "disposable engine exited".into(),
        ));
    }
    let artifacts = candidate.parent().unwrap().join("engine-boundary-evidence");
    if !artifacts.is_dir()
        || !trusted.join("engine.sock").exists()
        || !trusted.join("engine.db").is_file()
    {
        return Err(BoundaryError::InvalidProfile(
            "requires live disposable engine and evidence directory".into(),
        ));
    }
    let timeout = Duration::from_secs(u64::from(profile.resource_limits.wall_time_seconds));
    let mut evidence = BoundaryEvidence {
        profile_id: profile.id.clone(),
        allowed: BTreeMap::new(),
        denied: BTreeMap::new(),
        unenforced: vec![
            "cpu-seconds".into(),
            "memory-bytes".into(),
            "process-count".into(),
            "runtime-default-write-exceptions-not-qualified".into(),
        ],
        artifacts: vec![artifacts.clone()],
        versions: BTreeMap::new(),
        measurements: BTreeMap::new(),
    };
    for (name, executable) in [
        ("node", &profile.node_path),
        ("sandbox-runtime", &profile.runtime_path),
    ] {
        let output = artifacts.join(format!("{name}.version"));
        let status = bounded_command(
            &profile.node_path,
            executable,
            &["--version".into()],
            candidate,
            &output,
            timeout,
        )?;
        let version = read_bounded(&output)?.trim().to_string();
        if !status.success() || (name == "sandbox-runtime" && version != "0.0.80") {
            return Err(BoundaryError::Unavailable(format!(
                "{name} version mismatch: {version}"
            )));
        }
        evidence.versions.insert(name.into(), version);
    }
    write_new(&artifacts.join("engine-probe.cjs"), ENGINE_PROBE.as_bytes())?;
    write_json(
        &artifacts.join("settings.json"),
        &serde_json::json!({
            "network": {"allowedDomains": [], "deniedDomains": [],
                "allowUnixSockets": [], "allowAllUnixSockets": false, "allowLocalBinding": false},
            "filesystem": {"allowWrite": [candidate], "denyWrite": [trusted, artifacts],
                "denyRead": [trusted], "allowRead": []}
        }),
    )?;
    for mode in ["unconfined", "confined"] {
        let mut requests = BTreeMap::new();
        for role in ["worker", "child"] {
            let identity = format!("{mode}-{role}");
            let snapshot = WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new(&identity).unwrap(),
                request_id: RequestId::new(format!("{identity}-snapshot")).unwrap(),
                expected_revision: Cursor::new(0),
                command: crate::protocol::Command::Snapshot,
            };
            let mut control = snapshot.clone();
            control.request_id = RequestId::new(format!("{identity}-control")).unwrap();
            control.command = crate::protocol::Command::CreateProject {
                project_id: ProjectId::new(&identity).unwrap(),
                spec: ProjectSpec {
                    name: identity,
                    repositories: vec![],
                },
            };
            requests.insert(
                role,
                serde_json::json!({"snapshot": snapshot, "control": control}),
            );
        }
        let request_path = artifacts.join(format!("{mode}-requests.json"));
        write_json(&request_path, &requests)?;
        let mut args: Vec<std::ffi::OsString> = vec![];
        let executable = if mode == "confined" {
            args.extend([
                "--settings".into(),
                artifacts.join("settings.json").into_os_string(),
                "--".into(),
                profile.node_path.as_os_str().into(),
            ]);
            &profile.runtime_path
        } else {
            &profile.node_path
        };
        args.extend([
            artifacts.join("engine-probe.cjs").into_os_string(),
            candidate.as_os_str().into(),
            trusted.as_os_str().into(),
            engine.id().to_string().into(),
            request_path.into_os_string(),
            mode.into(),
            "worker".into(),
        ]);
        let output = artifacts.join(format!("{mode}.stdout"));
        let started = Instant::now();
        let status = bounded_command(
            &profile.node_path,
            executable,
            &args,
            candidate,
            &output,
            timeout,
        )?;
        evidence.measurements.insert(
            format!("{mode}-elapsed-ms"),
            started.elapsed().as_millis().try_into().unwrap(),
        );
        if !status.success() {
            return Err(BoundaryError::Unavailable(format!(
                "{mode} probe exited {status}: {}; artifacts {}",
                read_bounded(&output.with_extension("stderr"))?,
                artifacts.display()
            )));
        }
        for role in ["worker", "child"] {
            let result_path = candidate.join(format!("{mode}-{role}.json"));
            let raw: EngineProbeResult = serde_json::from_str(&read_bounded(&result_path)?)
                .map_err(|error| {
                    BoundaryError::Unavailable(format!("invalid engine observations: {error}"))
                })?;
            evidence.artifacts.push(result_path);
            if evidence.versions.get("node") != Some(&raw.node_version) {
                return Err(BoundaryError::Unavailable(
                    "probe Node version mismatch".into(),
                ));
            }
            for case in [
                "contact",
                "control",
                "state-read",
                "state-write",
                "signal",
                "candidate-write",
            ] {
                let observation = raw.operations.get(case).map(String::as_str);
                if mode == "unconfined" || case == "candidate-write" {
                    let applied = case != "control"
                        || raw.reply.as_ref().is_some_and(|reply| {
                            reply.request_id.as_str() == format!("{mode}-{role}-control")
                                && reply.outcome == crate::protocol::Outcome::Applied
                        });
                    record_case(
                        &mut evidence.allowed,
                        &format!("{mode}-{role}-{case}"),
                        observation == Some("ok")
                            && applied
                            && (case != "candidate-write"
                                || fs::read(candidate.join(format!("{mode}-{role}.output")))
                                    .is_ok_and(|bytes| bytes == role.as_bytes())),
                    );
                    if mode == "confined" {
                        record_case(
                            &mut evidence.allowed,
                            &format!("{role}-{case}"),
                            observation == Some("ok"),
                        );
                    }
                } else {
                    record_case(
                        &mut evidence.denied,
                        &format!("{role}-{case}"),
                        matches!(observation, Some("EACCES" | "EPERM" | "EROFS")),
                    );
                }
            }
        }
        write_json(&artifacts.join(format!("{mode}-evidence.json")), &evidence)?;
        if evidence
            .allowed
            .values()
            .chain(evidence.denied.values())
            .any(|value| !value)
        {
            return Err(BoundaryError::Failed(Box::new(evidence)));
        }
    }
    assert_eq!(evidence.denied.len(), 10);
    Err(BoundaryError::Unenforced(Box::new(evidence)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineProbeResult {
    operations: BTreeMap<String, String>,
    node_version: String,
    reply: Option<crate::protocol::WireReply>,
}

const ENGINE_PROBE: &str = r#"const fs = require('node:fs');
const net = require('node:net');
const cp = require('node:child_process');
const path = require('node:path');
const [candidate, trusted, pid, requestsPath, mode, role] = process.argv.slice(2);
const requests = JSON.parse(fs.readFileSync(requestsPath, 'utf8'))[role];
const operations = {};
function observe(name, action) {
    try { action(); operations[name] = 'ok'; }
    catch (error) { operations[name] = error.code || String(error); }
}
function exchange(request, name) {
    return new Promise(resolve => {
        const socket = net.createConnection(path.join(trusted, 'engine.sock'));
        const payload = Buffer.from(JSON.stringify(request));
        const size = Buffer.alloc(4);
        size.writeUInt32BE(payload.length);
        let received = Buffer.alloc(0);
        let settled = false;
        function finish(error, reply) {
            if (settled) return;
            settled = true;
            operations[name] = error || 'ok';
            socket.destroy();
            resolve(reply);
        }
        socket.setTimeout(1500, () => finish('TIMEOUT'));
        socket.once('error', error => finish(error.code || String(error)));
        socket.once('connect', () => socket.write(Buffer.concat([size, payload])));
        socket.on('data', bytes => {
            received = Buffer.concat([received, bytes]);
            if (received.length > 65540) return finish('OVERSIZED');
            if (received.length < 4) return;
            const length = received.readUInt32BE();
            if (length === 0 || length > 65536) return finish('INVALID_FRAME');
            if (received.length === length + 4) {
                try {
                    const reply = JSON.parse(received.subarray(4));
                    if (reply.request_id !== request.request_id) return finish('INVALID_REPLY');
                    finish(null, reply);
                } catch { finish('INVALID_JSON'); }
            }
        });
        socket.once('close', () => finish('CLOSED'));
    });
}
async function main() {
    observe('candidate-write', () => fs.writeFileSync(path.join(candidate, `${mode}-${role}.output`), role));
    observe('state-read', () => {
        const fd = fs.openSync(path.join(trusted, 'engine.db'), 'r');
        try {
            const header = Buffer.alloc(16);
            if (fs.readSync(fd, header, 0, 16, 0) !== 16 || header.toString() !== 'SQLite format 3\0') throw new Error('INVALID_DB');
        } finally { fs.closeSync(fd); }
    });
    observe('state-write', () => {
        const fd = fs.openSync(path.join(trusted, 'engine.db'), 'r+');
        try { if (fs.writeSync(fd, Buffer.from('S'), 0, 1, 0) !== 1) throw new Error('SHORT_WRITE'); }
        finally { fs.closeSync(fd); }
    });
    observe('signal', () => process.kill(Number(pid), 'SIGCONT'));
    const snapshot = await exchange(requests.snapshot, 'contact');
    if (snapshot && snapshot.outcome.kind === 'Snapshot') requests.control.expected_revision = snapshot.revision;
    const reply = await exchange(requests.control, 'control');
    fs.writeFileSync(path.join(candidate, `${mode}-${role}.json`), JSON.stringify({operations, node_version: process.version, reply: reply || null}));
    if (role === 'worker') {
        const child = cp.spawnSync(process.execPath, [__filename, candidate, trusted, pid, requestsPath, mode, 'child'], {
            timeout: 7000, maxBuffer: 65536, env: process.env
        });
        if (child.status !== 0 || child.error) {
            process.stderr.write(child.stderr || String(child.error));
            process.exitCode = 1;
        }
    }
}
main().catch(error => { process.stderr.write(String(error)); process.exitCode = 1; });
"#;

fn record_case(cases: &mut BTreeMap<String, bool>, name: &str, outcome: bool) {
    assert!(
        cases.insert(name.into(), outcome).is_none(),
        "duplicate boundary case"
    );
}

fn accept_control(listener: &TcpListener, timeout: Duration) -> Result<TcpStream, BoundaryError> {
    let started = Instant::now();
    loop {
        match crate::spawn_gate::accept_tcp_std(listener) {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    return Err(BoundaryError::Unavailable(
                        "positive loopback control did not connect within deadline".into(),
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), BoundaryError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), BoundaryError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| BoundaryError::InvalidProfile(error.to_string()))?;
    write_new(path, &bytes)
}

fn read_bounded(path: &Path) -> Result<String, BoundaryError> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > OUTPUT_MAX {
        return Err(BoundaryError::Unavailable(
            "native output exceeded 64 KiB or is not regular".into(),
        ));
    }
    let mut value = String::new();
    file.take(OUTPUT_MAX + 1).read_to_string(&mut value)?;
    if value.len() as u64 > OUTPUT_MAX {
        return Err(BoundaryError::Unavailable(
            "native output exceeded 64 KiB".into(),
        ));
    }
    Ok(value)
}

fn bounded_command(
    dependency_executable: &Path,
    executable: &Path,
    args: &[std::ffi::OsString],
    cwd: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<ExitStatus, BoundaryError> {
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let stderr_path = output.with_extension("stderr");
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr_path)?;
    let node_dir = dependency_executable.parent().unwrap();
    let path = std::env::join_paths([node_dir, Path::new("/usr/bin"), Path::new("/bin")])
        .map_err(|error| BoundaryError::InvalidProfile(error.to_string()))?;
    let mut child = crate::spawn_gate::spawn(
        Command::new(executable)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path)
            .env("HOME", cwd)
            .env("TMPDIR", cwd)
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0),
    )
    .map_err(|error| {
        BoundaryError::Unavailable(format!("launch {}: {error}", executable.display()))
    })?;
    let result = (|| {
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if started.elapsed() >= timeout
                || fs::metadata(output)?.len() > OUTPUT_MAX
                || fs::metadata(&stderr_path)?.len() > OUTPUT_MAX
            {
                return Err(BoundaryError::Unavailable(format!(
                    "native command exceeded {} seconds or output bound; artifacts {}",
                    timeout.as_secs(),
                    output.display()
                )));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    kill_group(child.id());
    if result.is_err() {
        let _ = child.kill();
        child.wait()?;
    }
    result
}

fn kill_group(id: u32) {
    let pid = rustix::process::Pid::from_raw(id.try_into().unwrap()).unwrap();
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeResult {
    denied: BTreeMap<String, bool>,
    node_version: String,
    measurements: BTreeMap<String, u64>,
}

const PROBE: &str = r#"const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const cp = require('node:child_process');
const [candidate, trusted, profilePath, port] = process.argv.slice(2);
const profile = JSON.parse(fs.readFileSync(profilePath, 'utf8'));
const denied = {};
function deniedWrite(target) {
    try { fs.writeFileSync(target, 'changed'); return false; }
    catch (error) { return ['EACCES', 'EPERM', 'EROFS'].includes(error.code); }
}
fs.writeFileSync(path.join(candidate, 'candidate-output'), 'candidate');
const child = cp.spawnSync(process.execPath, ['-e',
    "require('node:fs').writeFileSync(process.argv[1], 'child')",
    path.join(candidate, 'child-output')], {timeout: 2000});
denied['trusted-write'] = deniedWrite(path.join(trusted, 'sentinel'));
denied['traversal-escape'] = deniedWrite(candidate + '/../' + path.basename(trusted) + '/sentinel');
denied['symlink-escape'] = deniedWrite(path.join(candidate, 'escape', 'sentinel'));
const childDenied = cp.spawnSync(process.execPath, ['-e',
    "try { require('node:fs').writeFileSync(process.argv[1], 'changed'); process.exit(1); } catch (e) { process.exit(['EACCES','EPERM','EROFS'].includes(e.code) ? 0 : 2); }",
    path.join(trusted, 'sentinel')], {timeout: 2000});
denied['child-trusted-write'] = childDenied.status === 0;
denied['credential-read'] = profile.credential_refs.every(file => {
    try { fs.writeFileSync(path.join(candidate, 'credential-copy'), fs.readFileSync(file)); return false; }
    catch (error) { return ['EACCES','EPERM'].includes(error.code); }
});
const socket = net.createConnection({host: '127.0.0.1', port: Number(port)});
let finished = false;
function finish(blocked) {
    if (finished) return;
    finished = true;
    socket.destroy();
    denied['forbidden-network'] = blocked;
    const usage = process.resourceUsage();
    fs.writeFileSync(path.join(candidate, 'probe-result.json'), JSON.stringify({
        denied, node_version: process.version,
        measurements: {user_cpu_microseconds: usage.userCPUTime,
            system_cpu_microseconds: usage.systemCPUTime,
            max_rss_kib: usage.maxRSS, successful_candidate_children: child.status === 0 ? 1 : 0}
    }));
}
socket.setTimeout(800, () => finish(false));
socket.on('connect', () => finish(false));
socket.on('error', error => finish(['EACCES','EPERM'].includes(error.code)));
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_loopback_control_waits_for_connection() {
        let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            crate::spawn_gate::connect_tcp_std_timeout(&address, Duration::from_secs(1)).unwrap()
        });
        let result = accept_control(&listener, Duration::from_secs(1));
        let _connection = sender.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn subprocess_watchdog_reaps_real_sleep() {
        let scratch =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local-state/boundary-tests");
        fs::create_dir_all(&scratch).unwrap();
        let root = tempfile::tempdir_in(scratch).unwrap();
        let profile = SandboxProfile {
            id: "watchdog".into(),
            runtime_path: "/bin/sh".into(),
            node_path: "/bin/sh".into(),
            grants: Vec::new(),
            network_policy: NetworkPolicy::DenyAll,
            credential_refs: Vec::new(),
            resource_limits: ResourceLimits {
                cpu_seconds: 1,
                memory_bytes: 64 * 1024 * 1024,
                processes: 2,
                wall_time_seconds: 1,
            },
        };
        let started = Instant::now();
        let result = bounded_command(
            &profile.node_path,
            Path::new("/bin/sleep"),
            &["5".into()],
            root.path(),
            &root.path().join("watchdog.stdout"),
            Duration::from_millis(40),
        );
        assert!(
            matches!(result, Err(BoundaryError::Unavailable(_))),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn positive_loopback_control_has_a_deadline() {
        let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = accept_control(&listener, Duration::from_millis(20));
        assert!(matches!(result, Err(BoundaryError::Unavailable(_))));
    }

    #[test]
    #[should_panic(expected = "duplicate boundary case")]
    fn duplicate_observation_trips_invariant() {
        let mut cases = BTreeMap::new();
        record_case(&mut cases, "case", true);
        record_case(&mut cases, "case", false);
    }

    #[test]
    #[should_panic]
    fn watchdog_refuses_init_process_group() {
        kill_group(1);
    }

    #[test]
    fn empty_or_partial_evidence_is_not_admissible() {
        let evidence = BoundaryEvidence {
            profile_id: "test".into(),
            allowed: BTreeMap::new(),
            denied: BTreeMap::new(),
            unenforced: Vec::new(),
            artifacts: Vec::new(),
            versions: BTreeMap::new(),
            measurements: BTreeMap::new(),
        };
        assert!(!evidence.admissible());
    }
}
