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
    BaselineDraft, BaselineOrigin, CandidateSnapshot, CheckKind, CheckSpec, OwnerVerification,
    Risk, RiskRule, VerificationReceipt, VerifyRequest, verify,
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
        check(
            "verify-refused-until-qualified",
            verify_refused_until_qualified,
        ),
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

/// The candidate source every verification scenario registers. Output: the snapshot.
fn snapshot() -> CandidateSnapshot {
    CandidateSnapshot {
        files: BTreeMap::from([(
            "src/main.rs".into(),
            b"fn sum(a: i32, b: i32) -> i32 { a + b }".to_vec(),
        )]),
        head_digest: "a".repeat(64),
        target_digest: "b".repeat(64),
    }
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
    /// Output: the outcome as JSON, or `{"refused": code}` when the engine refused it.
    async fn call(&mut self, command: Command) -> Result<Value, String> {
        self.sent += 1;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: typed("host", ClientId::new)?,
            request_id: typed(&format!("op-{}", self.sent), RequestId::new)?,
            expected_revision: Cursor::new(self.revision),
            command,
        };
        match self.client.request(request).await {
            Ok(answer) => {
                self.revision = answer.revision.value();
                serde_json::to_value(&answer.outcome).map_err(|error| error.to_string())
            }
            Err(error) => Ok(json!({ "refused": error.0 })),
        }
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

/// The whole verification path on a project at 2.4.2: a baseline proposed and approved, a
/// task pinned, a candidate registered, then Verify from the host and from the owner, both
/// refused as unavailable until the runtime is qualified, the candidate's state, Accept
/// refused, and whether the outcomes name the draft's identity and the registered digests,
/// which the runner masks. Input: the scratch folder. Output: each step's outcome by name.
fn verify_refused_until_qualified(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let socket = engine.socket();
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let captured = verify_steps(&mut owner, &scratch, &socket).await;
        (owner, captured)
    })
}

/// The steps of [`verify_refused_until_qualified`]. Inputs: the owner, the scratch folder and
/// the engine socket. Output: each step's outcome by name.
async fn verify_steps(owner: &mut Owner, scratch: &Path, socket: &Path) -> Result<Value, String> {
    let project = seeded(owner, scratch).await?;
    let config_id = config_id(owner).await?;
    let draft = draft()?;
    let baseline_id: BaselineId = draft.identity().map_err(|error| error.0)?;
    let identity = baseline_id.as_str().to_owned();
    let candidate = typed("candidate", RequestId::new)?;
    let task_id = typed("task", TaskId::new)?;
    let mut wire = Wire::new(socket)?;
    wire.call(Command::Snapshot).await?;
    let mut steps = Map::new();
    let host = |action| Command::OwnerVerification { action };
    let sent = [
        ("1-propose", Command::ProposeBaseline { draft }),
        (
            "2-approve",
            host(OwnerVerification::Approve {
                baseline_id: baseline_id.clone(),
            }),
        ),
        (
            "3-pin",
            Command::PinTask {
                context: ExecutionContext {
                    project_id: project,
                    task_id: task_id.clone(),
                    config_id,
                    snapshot_id: typed("initial", SnapshotId::new)?,
                    baseline_id,
                    allowed_destinations: vec![],
                    budget_ref: typed("local", CredentialRef::new)?,
                },
            },
        ),
        (
            "4-register",
            host(OwnerVerification::Register {
                id: candidate.clone(),
                task_id,
                snapshot: snapshot(),
                environment_id: typed("controlled-fixture", EnvironmentId::new)?,
            }),
        ),
        (
            "5-verify-host",
            host(OwnerVerification::Verify {
                candidate_id: candidate.clone(),
                attempt_id: typed("attempt", RequestId::new)?,
            }),
        ),
    ];
    for (name, command) in sent {
        steps.insert(name.into(), wire.call(command).await?);
    }
    let owner_verify = OwnerMutation::Verify {
        candidate_id: candidate.clone(),
        attempt_id: typed("owner-attempt", RequestId::new)?,
    };
    steps.insert(
        "6-verify-owner".into(),
        owner_change(owner, owner_verify).await?,
    );
    let inspect = host(OwnerVerification::Inspect {
        candidate_id: candidate.clone(),
    });
    steps.insert("7-inspect".into(), wire.call(inspect).await?);
    let accept = host(OwnerVerification::Accept {
        candidate_id: candidate,
        attempt_id: typed("attempt", RequestId::new)?,
    });
    steps.insert("8-accept".into(), wire.call(accept).await?);
    let text = Value::Object(steps.clone()).to_string();
    let registered = snapshot();
    steps.insert(
        "9-facts".into(),
        json!({
            "names_draft_identity": text.contains(&identity),
            "names_registered_head": text.contains(&registered.head_digest),
            "names_registered_target": text.contains(&registered.target_digest),
        }),
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

/// The report the engine writes without native or provider runs, and whether it names the
/// source revision given, which the runner masks. Input: the scratch folder.
/// Output: the command's result.
fn qualification_report(scratch: &Path) -> Result<Value, String> {
    let arguments = [
        "report",
        "--source-revision",
        SOURCE,
        "--profile-id",
        PROFILE,
    ];
    let mut ran = qualification(scratch, &arguments)?;
    let names_source = ran["stdout"]
        .as_str()
        .is_some_and(|text| text.contains(SOURCE));
    ran["names_source"] = json!(names_source);
    Ok(ran)
}

/// Writes the engine's own report and an empty evidence folder for the admit scenarios.
/// Input: the scratch folder. Output: the report and evidence paths as text.
fn report_files(scratch: &Path) -> Result<(String, String), String> {
    let written = qualification_report(scratch)?;
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
