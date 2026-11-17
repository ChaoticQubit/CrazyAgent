use super::Scenario;
use super::e2e_support::{ENGINE, Owner};
use super::workflow::{attached, engine, private_folder, recorded, seeded};
use crazyagent::engine::Client;
use crazyagent::engine::owner::{OwnerMutation, OwnerRead};
use crazyagent::protocol::{
    BaselineId, ClientId, Command, ConfigId, CredentialRef, Cursor, EnvironmentId,
    ExecutionContext, PROTOCOL_VERSION, ProjectId, RequestId, SnapshotId, TaskId, WireRequest,
};
use crazyagent::verification::{
    BaselineDraft, BaselineOrigin, CheckKind, CheckSpec, Risk, RiskRule, VerificationReceipt,
    VerifyRequest, verify,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command as Process;
use std::time::Duration;

/// How long one wire request to the engine socket may take.
const WIRE_TIME: Duration = Duration::from_secs(10);

/// The source revision the qualification scenarios name.
const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";

/// The qualification profile the scenarios name.
const PROFILE: &str = "snapshot-profile";

/// Lists the check profile scenarios: the verification refusal through the engine, its
/// receipt rules and the qualification report reads. Output: the scenarios in run order.
pub(super) fn scenarios() -> Vec<Scenario> {
    let check = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "check",
        name,
        capture,
    };
    vec![
        check("verify-path-without-register", verify_path_without_register),
        check("verify-unknown-candidate", verify_unknown_candidate),
        check(
            "baseline-without-integrity-floor",
            baseline_without_integrity_floor,
        ),
        check("verify-request-valid", verify_request_valid),
        check("verify-request-invalid", verify_request_invalid),
        check("receipt-from-json-unproven", receipt_from_json_unproven),
        check("qualification-report", qualification_report),
        check(
            "qualification-admit-unavailable",
            qualification_admit_unavailable,
        ),
        check("qualification-admit-source-mismatch", admit_source_mismatch),
        check(
            "qualification-admit-profile-mismatch",
            admit_profile_mismatch,
        ),
    ]
}

/// Makes a typed id from fixed text. Inputs: the text and the constructor.
/// Output: the id, or the constructor's refusal as text.
fn typed<T, E: std::fmt::Display>(
    text: &str,
    make: impl FnOnce(String) -> Result<T, E>,
) -> Result<T, String> {
    make(text.to_owned()).map_err(|error| format!("{text}: {error}"))
}

/// The baseline every verification scenario proposes for [`super::workflow::PROJECT`]: a code
/// check and a test integrity check at the lowest risk, and a high-risk holdout.
/// Output: the draft.
fn draft() -> Result<BaselineDraft, String> {
    let spec = |id: &str, kind: CheckKind, minimum_risk: Risk, content: &[u8]| {
        typed(id, RequestId::new).map(|id| CheckSpec {
            id,
            kind,
            minimum_risk,
            content: content.to_vec(),
        })
    };
    Ok(BaselineDraft {
        version: 1,
        project_id: typed(super::workflow::PROJECT, ProjectId::new)?,
        origin: BaselineOrigin::Imported,
        requirements: "Return a correct sum and detect weakened tests".into(),
        checks: vec![
            spec(
                "code",
                CheckKind::Code,
                Risk::Low,
                b"assert_eq!(sum(2, 3), 5)",
            )?,
            spec(
                "integrity",
                CheckKind::TestIntegrity,
                Risk::Low,
                b"reject broken sums",
            )?,
            spec(
                "holdout",
                CheckKind::Holdout,
                Risk::High,
                b"approved extra cases",
            )?,
        ],
        rules: vec![RiskRule {
            path_prefix: "src".into(),
            minimum: Risk::Medium,
        }],
        retry_limit: 0,
    })
}

/// A host client on the engine's request socket that keeps the revision the engine last
/// answered, as a hosted agent's client does.
struct Wire {
    client: Client,
    revision: u64,
    sent: u64,
}

impl Wire {
    /// Connects to the running engine's socket. Input: the engine's socket path.
    /// Output: the client, at revision 0 until the first reply.
    fn new(socket: &Path) -> Result<Self, String> {
        Ok(Self {
            client: Client::new(socket.to_path_buf(), WIRE_TIME).map_err(|error| error.0)?,
            revision: 0,
            sent: 0,
        })
    }

    /// Sends one command and records its outcome whole. Input: the command.
    /// Output: the outcome as JSON; a failed exchange on the socket is an error, never a
    /// capture.
    async fn call(&mut self, command: Command) -> Result<Value, String> {
        self.sent += 1;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: typed("host", ClientId::new)?,
            request_id: typed(&format!("op-{}", self.sent), RequestId::new)?,
            expected_revision: Cursor::new(self.revision),
            command,
        };
        let answer = self
            .client
            .request(request)
            .await
            .map_err(|error| format!("the engine socket failed: {}", error.0))?;
        self.revision = answer.revision.value();
        serde_json::to_value(&answer.outcome).map_err(|error| error.to_string())
    }
}

/// Reads one owner read the way the desktop does. Inputs: the owner and the read.
/// Output: `{"data": ..}`, or `{"refused": code}` with the engine's plain code.
async fn owner_read(owner: &mut Owner, read: OwnerRead) -> Result<Value, String> {
    match owner.controller.read(read).await {
        Ok(reply) => serde_json::to_value(&reply.data)
            .map(|data| json!({ "data": data }))
            .map_err(|error| error.to_string()),
        Err(error) => Ok(json!({ "refused": error.0 })),
    }
}

/// Sends one owner change the way the desktop does after a fresh read.
/// Inputs: the owner and the change. Output: the change's result, or the whole response
/// when it was not recorded.
async fn owner_change(owner: &mut Owner, mutation: OwnerMutation) -> Result<Value, String> {
    owner
        .controller
        .read(OwnerRead::Capabilities)
        .await
        .map_err(|error| error.0)?;
    let generation = owner.next();
    if let Err(error) = owner.controller.begin_mutation(mutation, generation) {
        return Ok(json!({ "refused": error.0 }));
    }
    recorded(owner.controller.send_pending().await)
}

/// The engine's config id, which a pinned task must name. Input: the owner.
/// Output: the id from a fresh read.
async fn config_id(owner: &mut Owner) -> Result<ConfigId, String> {
    let read = owner.controller.read(OwnerRead::Capabilities).await;
    read.map(|answer| answer.config_id).map_err(|error| error.0)
}

/// The 2.4.2 verification path: the owner proposes and approves a baseline, a host client
/// pins a task, then the owner's Verify, Inspect and Accept on a candidate id, and whether
/// the outcomes name the draft's identity, which the runner masks. Verify of a registered
/// candidate is not covered: 2.4.2 has no owner or wire route that registers one.
/// Input: the scratch folder. Output: each step's outcome by name.
fn verify_path_without_register(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let socket = engine.socket();
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let captured = verify_steps(&mut owner, &scratch, &socket).await;
        (owner, captured)
    })
}

/// The steps of [`verify_path_without_register`]. Inputs: the owner, the scratch folder and
/// the engine socket. Output: each step's outcome by name.
async fn verify_steps(owner: &mut Owner, scratch: &Path, socket: &Path) -> Result<Value, String> {
    let project = seeded(owner, scratch).await?;
    let draft = draft()?;
    let baseline_id: BaselineId = draft.identity().map_err(|error| error.0)?;
    let identity = baseline_id.as_str().to_owned();
    let mut steps = Map::new();
    let propose = OwnerMutation::ProposeBaseline { draft };
    steps.insert("1-propose".into(), owner_change(owner, propose).await?);
    let approve = OwnerMutation::ApproveBaseline {
        baseline_id: baseline_id.clone(),
    };
    steps.insert("2-approve".into(), owner_change(owner, approve).await?);
    let context = ExecutionContext {
        project_id: project,
        task_id: typed("task", TaskId::new)?,
        config_id: config_id(owner).await?,
        snapshot_id: typed("initial", SnapshotId::new)?,
        baseline_id,
        allowed_destinations: vec![],
        budget_ref: typed("local", CredentialRef::new)?,
    };
    let mut wire = Wire::new(socket)?;
    wire.call(Command::Snapshot).await?;
    steps.insert(
        "3-pin".into(),
        wire.call(Command::PinTask { context }).await?,
    );
    let candidate = typed("candidate", RequestId::new)?;
    let attempt = typed("attempt", RequestId::new)?;
    let verify = OwnerMutation::Verify {
        candidate_id: candidate.clone(),
        attempt_id: attempt.clone(),
    };
    steps.insert("4-verify".into(), owner_change(owner, verify).await?);
    let inspect = OwnerRead::Inspect {
        candidate_id: candidate.clone(),
    };
    steps.insert("5-inspect".into(), owner_read(owner, inspect).await?);
    let accept = OwnerRead::Accept {
        candidate_id: candidate,
        attempt_id: attempt,
    };
    steps.insert("6-accept".into(), owner_read(owner, accept).await?);
    let text = Value::Object(steps.clone()).to_string();
    steps.insert(
        "7-facts".into(),
        json!({ "names_draft_identity": text.contains(&identity) }),
    );
    Ok(Value::Object(steps))
}

/// Verify from the owner on a candidate nobody registered. Input: the scratch folder.
/// Output: the change's result.
fn verify_unknown_candidate(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let captured: Result<Value, String> = async {
            seeded(&mut owner, &scratch).await?;
            let mutation = OwnerMutation::Verify {
                candidate_id: typed("missing-candidate", RequestId::new)?,
                attempt_id: typed("attempt", RequestId::new)?,
            };
            owner_change(&mut owner, mutation).await
        }
        .await;
        (owner, captured)
    })
}

/// A baseline proposed from the owner without its test integrity check, which every
/// baseline needs at the lowest risk. Input: the scratch folder. Output: the result.
fn baseline_without_integrity_floor(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let captured: Result<Value, String> = async {
            seeded(&mut owner, &scratch).await?;
            let mut draft = draft()?;
            draft
                .checks
                .retain(|check| check.kind != CheckKind::TestIntegrity);
            owner_change(&mut owner, OwnerMutation::ProposeBaseline { draft }).await
        }
        .await;
        (owner, captured)
    })
}

/// A verify request naming every identity as a digest and two checks. Output: the request.
fn request() -> Result<VerifyRequest, String> {
    let digest = |fill: &str| fill.repeat(64);
    Ok(VerifyRequest {
        snapshot_id: typed(&digest("1"), SnapshotId::new)?,
        baseline_id: typed(&digest("2"), BaselineId::new)?,
        config_id: typed(&digest("3"), ConfigId::new)?,
        environment_id: typed("controlled-fixture", EnvironmentId::new)?,
        source_digest: digest("4"),
        head_digest: digest("5"),
        target_digest: digest("6"),
        checks: BTreeMap::from([
            (typed("code", RequestId::new)?, digest("7")),
            (typed("integrity", RequestId::new)?, digest("8")),
        ]),
    })
}

/// The runtime refusal for a well-formed request. The engine refuses Verify before any
/// request leaves it, so the public function it calls is the capture.
/// Input: the scratch folder, unused. Output: the refusal.
fn verify_request_valid(_scratch: &Path) -> Result<Value, String> {
    let refused = verify(&request()?).err().map(|error| error.0);
    Ok(json!({ "refused": refused }))
}

/// The refusal for a request whose config id is not a digest. Input: the scratch folder,
/// unused. Output: the refusal.
fn verify_request_invalid(_scratch: &Path) -> Result<Value, String> {
    let mut request = request()?;
    request.config_id = typed("not-a-digest", ConfigId::new)?;
    let refused = verify(&request).err().map(|error| error.0);
    Ok(json!({ "refused": refused }))
}

/// A receipt read from JSON that says every check passed, with a matching artifact: JSON
/// never proves execution, so Accept refuses it. Input: the scratch folder, unused.
/// Output: the refusal.
fn receipt_from_json_unproven(_scratch: &Path) -> Result<Value, String> {
    let current = request()?;
    let artifact = b"2 checks passed".to_vec();
    let digest = format!("{:x}", Sha256::digest(&artifact));
    let passed: BTreeMap<_, _> = current
        .checks
        .keys()
        .map(|id| (id.clone(), "Passed"))
        .collect();
    let receipt: VerificationReceipt = serde_json::from_value(json!({
        "request": current,
        "outcomes": passed,
        "artifacts": { "report.txt": artifact },
        "artifact_digests": { "report.txt": digest },
        "completed_at": 1,
    }))
    .map_err(|error| error.to_string())?;
    let refused = receipt.accepts(&current).err().map(|error| error.0);
    Ok(json!({ "refused": refused }))
}

/// Runs the engine binary's qualification command with a fixture HOME.
/// Inputs: the scratch folder and the arguments after `qualification`.
/// Output: the exit code, stdout and stderr.
fn qualification(scratch: &Path, arguments: &[&str]) -> Result<Value, String> {
    let home = scratch.join("home");
    if !home.is_dir() {
        private_folder(scratch, "home")?;
    }
    let ran = crazyagent::spawn_gate::collected(
        Process::new(ENGINE)
            .arg("qualification")
            .args(arguments)
            .env("HOME", &home),
    )
    .map_err(|error| error.to_string())?;
    Ok(json!({
        "arguments": arguments,
        "status": ran.status.code(),
        "stdout": String::from_utf8_lossy(&ran.stdout),
        "stderr": String::from_utf8_lossy(&ran.stderr),
    }))
}

/// Runs the engine's report command without native or provider runs.
/// Input: the scratch folder. Output: the command's result with its stdout as written.
fn report_ran(scratch: &Path) -> Result<Value, String> {
    let arguments = [
        "report",
        "--source-revision",
        SOURCE,
        "--profile-id",
        PROFILE,
    ];
    qualification(scratch, &arguments)
}

/// The engine's report read as JSON, with its own version taken out for a stable fact, and
/// whether it names the source revision given, which the runner masks.
/// Input: the scratch folder. Output: the command's result.
fn qualification_report(scratch: &Path) -> Result<Value, String> {
    let mut ran = report_ran(scratch)?;
    let stdout = ran["stdout"].as_str().ok_or("report has no stdout")?;
    let names_source = stdout.contains(SOURCE);
    let mut report: Value = serde_json::from_str(stdout).map_err(|error| error.to_string())?;
    let version = report
        .get_mut("versions")
        .and_then(Value::as_object_mut)
        .and_then(|versions| versions.remove("crazyagent"));
    let names_version = version.as_ref().and_then(Value::as_str) == Some(env!("CARGO_PKG_VERSION"));
    ran["stdout"] = report;
    ran["names_source"] = json!(names_source);
    ran["names_version"] = json!(names_version);
    Ok(ran)
}

/// Writes the engine's own report and an empty evidence folder for the admit scenarios.
/// Input: the scratch folder. Output: the report and evidence paths as text.
fn report_files(scratch: &Path) -> Result<(String, String), String> {
    let written = report_ran(scratch)?;
    let text = written["stdout"].as_str().ok_or("report has no stdout")?;
    let report = scratch.join("report.json");
    std::fs::write(&report, text).map_err(|error| error.to_string())?;
    let evidence = private_folder(scratch, "evidence")?;
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or("path is not UTF-8");
    Ok((text(&report)?, text(&evidence)?))
}

/// Admits the engine's own report for one required capability with the given source and
/// profile. Inputs: the scratch folder, the source revision and the profile.
/// Output: the command's result.
fn admit(scratch: &Path, source: &str, profile: &str) -> Result<Value, String> {
    let (report, evidence) = report_files(scratch)?;
    let arguments = [
        "admit",
        "--report",
        report.as_str(),
        "--source-revision",
        source,
        "--profile-id",
        profile,
        "--evidence-dir",
        evidence.as_str(),
        "--require",
        "confinement",
    ];
    qualification(scratch, &arguments)
}

/// Admission of the engine's own report, where no capability is proven.
/// Input: the scratch folder. Output: the command's result.
fn qualification_admit_unavailable(scratch: &Path) -> Result<Value, String> {
    admit(scratch, SOURCE, PROFILE)
}

/// Admission naming another source revision than the report's.
/// Input: the scratch folder. Output: the command's result.
fn admit_source_mismatch(scratch: &Path) -> Result<Value, String> {
    admit(scratch, &"f".repeat(40), PROFILE)
}

/// Admission naming another profile than the report's.
/// Input: the scratch folder. Output: the command's result.
fn admit_profile_mismatch(scratch: &Path) -> Result<Value, String> {
    admit(scratch, SOURCE, "another-profile")
}
