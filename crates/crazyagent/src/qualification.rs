#[cfg(unix)]
use rustix::fs::{Mode, OFlags, open, openat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const REPORT_BYTES_MAX: u64 = 1_048_576;
pub const ARTIFACT_BYTES_MAX: u64 = 16_777_216;
pub const ARTIFACTS_MAX: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Confinement,
    LiveRecovery,
    NativeAuth,
    ChildControl,
}

impl Capability {
    pub const ALL: [Self; 4] = [
        Self::Confinement,
        Self::LiveRecovery,
        Self::NativeAuth,
        Self::ChildControl,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confinement => "confinement",
            Self::LiveRecovery => "live_recovery",
            Self::NativeAuth => "native_auth",
            Self::ChildControl => "child_control",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Proven,
    Failed,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct AdmissionContext {
    source_revision: String,
    profile_id: String,
    evidence_root: PathBuf,
}

impl AdmissionContext {
    pub fn new(source_revision: &str, profile_id: &str, evidence_root: &Path) -> Self {
        Self {
            source_revision: source_revision.into(),
            profile_id: profile_id.into(),
            evidence_root: evidence_root.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationReport {
    pub source_revision: String,
    pub profile_id: String,
    #[serde(deserialize_with = "unique_map")]
    pub versions: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique_map")]
    pub results: BTreeMap<Capability, Outcome>,
    pub artifacts: Vec<Artifact>,
    pub gaps: Vec<String>,
    #[serde(skip)]
    context: Option<AdmissionContext>,
}

impl QualificationReport {
    pub fn new(source_revision: &str, profile_id: &str) -> Self {
        Self {
            source_revision: source_revision.into(),
            profile_id: profile_id.into(),
            versions: BTreeMap::new(),
            results: BTreeMap::new(),
            artifacts: Vec::new(),
            gaps: Vec::new(),
            context: None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AdmissionError {
    UnboundReport,
    SourceMismatch,
    ProfileMismatch,
    InvalidReport(String),
    MissingCapabilities(Vec<Capability>),
    Evidence(String),
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnboundReport => write!(f, "report has no trusted admission context"),
            Self::SourceMismatch => write!(f, "source revision does not match trusted context"),
            Self::ProfileMismatch => write!(f, "profile does not match trusted context"),
            Self::InvalidReport(reason) => write!(f, "invalid qualification report: {reason}"),
            Self::Evidence(reason) => write!(f, "invalid evidence: {reason}"),
            Self::MissingCapabilities(capabilities) => {
                let names: Vec<_> = capabilities.iter().map(|c| c.as_str()).collect();
                write!(f, "required capabilities not proven: {}", names.join(", "))
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

pub fn load_report(
    path: &Path,
    context: AdmissionContext,
) -> Result<QualificationReport, AdmissionError> {
    let file = open_report(path)?;
    let metadata = file
        .metadata()
        .map_err(|e| AdmissionError::InvalidReport(e.to_string()))?;
    if !metadata.is_file() || metadata.len() > REPORT_BYTES_MAX {
        return Err(AdmissionError::InvalidReport(
            "expected a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(REPORT_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AdmissionError::InvalidReport(e.to_string()))?;
    if bytes.len() as u64 > REPORT_BYTES_MAX {
        return Err(AdmissionError::InvalidReport(
            "report exceeds size limit".into(),
        ));
    }
    let mut report: QualificationReport =
        serde_json::from_slice(&bytes).map_err(|e| AdmissionError::InvalidReport(e.to_string()))?;
    report.context = Some(context);
    Ok(report)
}

pub fn admit(report: &QualificationReport, required: &[Capability]) -> Result<(), AdmissionError> {
    let context = report
        .context
        .as_ref()
        .ok_or(AdmissionError::UnboundReport)?;
    if required.is_empty() || required.len() > Capability::ALL.len() {
        return Err(AdmissionError::InvalidReport(
            "expected 1 to 4 required capabilities".into(),
        ));
    }
    let required_unique: BTreeSet<_> = required.iter().copied().collect();
    if required_unique.len() != required.len() {
        return Err(AdmissionError::InvalidReport(
            "expected unique required capabilities".into(),
        ));
    }
    for identity in [
        &context.source_revision,
        &context.profile_id,
        &report.source_revision,
        &report.profile_id,
    ] {
        if !valid_text(identity, 256) {
            return Err(AdmissionError::InvalidReport(
                "invalid source/profile identifier".into(),
            ));
        }
    }
    if report.source_revision != context.source_revision {
        return Err(AdmissionError::SourceMismatch);
    }
    if report.profile_id != context.profile_id {
        return Err(AdmissionError::ProfileMismatch);
    }
    let missing: Vec<_> = required
        .iter()
        .copied()
        .filter(|c| report.results.get(c) != Some(&Outcome::Proven))
        .collect();
    if !missing.is_empty() {
        return Err(AdmissionError::MissingCapabilities(missing));
    }
    validate_shape(report)?;
    let root = open_evidence_root(&context.evidence_root)?;
    for artifact in &report.artifacts {
        let file = open_artifact(&root, &artifact.path)?;
        if hash_file(file, ARTIFACT_BYTES_MAX)? != artifact.sha256 {
            return Err(AdmissionError::Evidence("artifact SHA-256 mismatch".into()));
        }
    }
    Ok(())
}

fn valid_text(value: &str, bytes_max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= bytes_max && !value.chars().any(char::is_control)
}

fn validate_shape(report: &QualificationReport) -> Result<(), AdmissionError> {
    if report.versions.is_empty()
        || report.versions.len() > 128
        || report.results.len() > Capability::ALL.len()
        || report.gaps.len() > 128
    {
        return Err(AdmissionError::InvalidReport(
            "missing versions or excessive entries".into(),
        ));
    }
    if report
        .versions
        .iter()
        .any(|(k, v)| !valid_text(k, 256) || !valid_text(v, 1024))
        || report.gaps.iter().any(|v| !valid_text(v, 4096))
    {
        return Err(AdmissionError::InvalidReport(
            "invalid version or gap text".into(),
        ));
    }
    if report.artifacts.is_empty() || report.artifacts.len() > ARTIFACTS_MAX {
        return Err(AdmissionError::Evidence(
            "expected 1 to 16 artifacts".into(),
        ));
    }
    let mut paths = BTreeSet::new();
    for artifact in &report.artifacts {
        if artifact.path.as_os_str().len() > 1024 {
            return Err(AdmissionError::Evidence(
                "artifact path exceeds byte limit".into(),
            ));
        }
        let components: Vec<_> = artifact.path.components().collect();
        if components.is_empty()
            || components.len() > 32
            || components
                .iter()
                .any(|c| !matches!(c, Component::Normal(_)))
            || !paths.insert(&artifact.path)
        {
            return Err(AdmissionError::Evidence(
                "invalid or duplicate relative artifact path".into(),
            ));
        }
        if artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(AdmissionError::Evidence(
                "expected lowercase SHA-256 hex".into(),
            ));
        }
    }
    Ok(())
}

/// Opens a qualification report without following a final symlink.
/// Takes the report path. Returns the open file, or `InvalidReport` with the OS error.
#[cfg(unix)]
fn open_report(path: &Path) -> Result<File, AdmissionError> {
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    Ok(File::from(open(path, flags, Mode::empty()).map_err(
        |e| AdmissionError::InvalidReport(e.to_string()),
    )?))
}

/// Refuses a qualification report where no no-follow open exists.
/// Takes the report path, unused. Returns `InvalidReport` reading
/// `qualification_evidence_unavailable`.
#[cfg(not(unix))]
fn open_report(_path: &Path) -> Result<File, AdmissionError> {
    Err(AdmissionError::InvalidReport(
        "qualification_evidence_unavailable".into(),
    ))
}

/// Opens the evidence root directory without following a final symlink.
/// Takes its path. Returns the open directory, or `Evidence` with the OS error.
#[cfg(unix)]
fn open_evidence_root(path: &Path) -> Result<File, AdmissionError> {
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY;
    Ok(File::from(
        open(path, flags, Mode::empty()).map_err(|e| AdmissionError::Evidence(e.to_string()))?,
    ))
}

/// Refuses the evidence root where no directory-relative no-follow walk exists.
/// Takes its path, unused. Returns `Evidence` reading `qualification_evidence_unavailable`.
#[cfg(not(unix))]
fn open_evidence_root(_path: &Path) -> Result<File, AdmissionError> {
    Err(AdmissionError::Evidence(
        "qualification_evidence_unavailable".into(),
    ))
}

/// Refuses an evidence artifact where no directory-relative no-follow walk exists.
/// Takes the root and path, both unused. Returns `Evidence` reading
/// `qualification_evidence_unavailable`.
#[cfg(not(unix))]
fn open_artifact(_root: &File, _path: &Path) -> Result<File, AdmissionError> {
    Err(AdmissionError::Evidence(
        "qualification_evidence_unavailable".into(),
    ))
}

#[cfg(unix)]
fn open_artifact(root: &File, path: &Path) -> Result<File, AdmissionError> {
    let mut directory = root
        .try_clone()
        .map_err(|e| AdmissionError::Evidence(e.to_string()))?;
    let mut components = path.components().peekable();
    for _ in 0..32 {
        let Some(Component::Normal(name)) = components.next() else {
            return Err(AdmissionError::Evidence(
                "expected a relative artifact file".into(),
            ));
        };
        let last = components.peek().is_none();
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        if !last {
            flags |= OFlags::DIRECTORY;
        }
        let file = File::from(
            openat(&directory, name, flags, Mode::empty())
                .map_err(|e| AdmissionError::Evidence(e.to_string()))?,
        );
        if last {
            return Ok(file);
        }
        directory = file;
    }
    Err(AdmissionError::Evidence(
        "artifact path exceeds component limit".into(),
    ))
}

fn hash_file(mut file: File, bytes_max: u64) -> Result<String, AdmissionError> {
    assert!(bytes_max > 0);
    assert!(bytes_max <= ARTIFACT_BYTES_MAX);
    let metadata = file
        .metadata()
        .map_err(|e| AdmissionError::Evidence(e.to_string()))?;
    if !metadata.is_file() || metadata.len() > bytes_max {
        return Err(AdmissionError::Evidence(
            "expected a bounded regular artifact file".into(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 65_536];
    for _ in 0..=bytes_max / buffer.len() as u64 {
        let read = file
            .read(&mut buffer)
            .map_err(|e| AdmissionError::Evidence(e.to_string()))?;
        bytes += read as u64;
        if bytes > bytes_max {
            return Err(AdmissionError::Evidence(
                "artifact exceeds byte limit".into(),
            ));
        }
        if read == 0 {
            assert!(bytes <= bytes_max);
            return Ok(format!("{:x}", hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
    Err(AdmissionError::Evidence(
        "artifact exceeds read limit".into(),
    ))
}

fn unique_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct UniqueMap<K, V>(std::marker::PhantomData<(K, V)>);

    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> serde::de::Visitor<'de>
        for UniqueMap<K, V>
    {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at most 128 unique map entries")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            for _ in 0..=128 {
                let Some((key, value)) = map.next_entry()? else {
                    return Ok(values);
                };
                if values.len() == 128 || values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive map entries",
                    ));
                }
            }
            unreachable!("bounded map loop always returns")
        }
    }

    deserializer.deserialize_map(UniqueMap(std::marker::PhantomData))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn report() -> QualificationReport {
        QualificationReport::new("revision-a", "profile-a")
    }

    fn context(root: &TempDir) -> AdmissionContext {
        AdmissionContext::new("revision-a", "profile-a", root.path())
    }

    fn load(report: &QualificationReport, root: &TempDir) -> QualificationReport {
        let path = root.path().join("report.json");
        fs::write(&path, serde_json::to_vec(report).unwrap()).unwrap();
        load_report(&path, context(root)).unwrap()
    }

    fn complete(root: &TempDir) -> QualificationReport {
        fs::write(root.path().join("evidence.txt"), b"abc").unwrap();
        let mut report = report();
        report.versions = BTreeMap::from([("test-runtime".into(), "1.0".into())]);
        report.results = Capability::ALL
            .into_iter()
            .map(|capability| (capability, Outcome::Proven))
            .collect();
        report.artifacts.push(Artifact {
            path: "evidence.txt".into(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
        });
        report
    }

    #[test]
    fn missing_evidence_never_admits() {
        let root = TempDir::new().unwrap();
        let empty = load(&report(), &root);
        assert!(admit(&empty, &[Capability::LiveRecovery]).is_err());
        let mut unavailable = complete(&root);
        unavailable
            .results
            .insert(Capability::LiveRecovery, Outcome::Unavailable);
        assert!(admit(&load(&unavailable, &root), &[Capability::LiveRecovery]).is_err());
        let mut wrong = complete(&root);
        wrong.profile_id = "another-profile".into();
        assert!(admit(&load(&wrong, &root), &[Capability::Confinement]).is_err());
        wrong = complete(&root);
        wrong.source_revision = "another-revision".into();
        assert!(admit(&load(&wrong, &root), &[Capability::Confinement]).is_err());
        let intact = load(&complete(&root), &root);
        fs::write(root.path().join("evidence.txt"), b"tampered").unwrap();
        assert!(admit(&intact, &[Capability::Confinement]).is_err());
        let mut escaping = complete(&root);
        escaping.artifacts[0].path = "../evidence.txt".into();
        assert!(admit(&load(&escaping, &root), &[Capability::Confinement]).is_err());
    }

    #[test]
    fn complete_intact_evidence_admits_required_capabilities() {
        let root = TempDir::new().unwrap();
        let bound = load(&complete(&root), &root);
        assert_eq!(admit(&bound, &Capability::ALL), Ok(()));
        assert_eq!(bound.source_revision, "revision-a");
        assert_eq!(bound.profile_id, "profile-a");
    }

    #[test]
    fn serialized_report_cannot_supply_admission_authority() {
        let root = TempDir::new().unwrap();
        let bound = load(&complete(&root), &root);
        let json = serde_json::to_vec(&bound).unwrap();
        let unbound: QualificationReport = serde_json::from_slice(&json).unwrap();
        assert!(admit(&unbound, &Capability::ALL).is_err());
        assert!(!String::from_utf8(json).unwrap().contains("evidence_root"));
    }

    #[test]
    fn required_capabilities_fail_closed_individually() {
        let root = TempDir::new().unwrap();
        for capability in Capability::ALL {
            for outcome in [None, Some(Outcome::Failed), Some(Outcome::Unavailable)] {
                let mut report = complete(&root);
                report.results.remove(&capability);
                if let Some(outcome) = outcome {
                    report.results.insert(capability, outcome);
                }
                assert_eq!(
                    admit(&load(&report, &root), &[capability]),
                    Err(AdmissionError::MissingCapabilities(vec![capability]))
                );
            }
        }
        assert!(admit(&load(&complete(&root), &root), &[]).is_err());
    }

    #[test]
    fn optional_unavailable_result_does_not_block_proven_requirement() {
        let root = TempDir::new().unwrap();
        let mut report = complete(&root);
        report
            .results
            .insert(Capability::NativeAuth, Outcome::Unavailable);
        report.gaps.push("Native auth is not qualified".into());
        assert_eq!(
            admit(&load(&report, &root), &[Capability::Confinement]),
            Ok(())
        );
    }

    #[test]
    fn absent_invalid_or_duplicate_artifacts_and_versions_fail() {
        let root = TempDir::new().unwrap();
        let original = complete(&root);
        let mut variants = Vec::new();
        let mut report = original.clone();
        report.artifacts.clear();
        variants.push(report);
        report = original.clone();
        report.versions.clear();
        variants.push(report);
        report = original.clone();
        report.artifacts[0].sha256 = "not-a-hash".into();
        variants.push(report);
        report = original.clone();
        report.artifacts[0].path = "missing.txt".into();
        variants.push(report);
        report = original.clone();
        report.artifacts.push(report.artifacts[0].clone());
        variants.push(report);
        report = original.clone();
        report.artifacts = vec![report.artifacts[0].clone(); ARTIFACTS_MAX + 1];
        variants.push(report);
        report = original.clone();
        report.source_revision.clear();
        variants.push(report);
        for report in variants {
            assert!(admit(&load(&report, &root), &[Capability::Confinement]).is_err());
        }
    }

    #[test]
    fn absolute_parent_symlink_and_directory_artifacts_fail() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("evidence.txt"), b"abc").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("evidence.txt"),
            root.path().join("linked.txt"),
        )
        .unwrap();
        fs::create_dir(root.path().join("directory")).unwrap();
        for path in [
            outside.path().join("evidence.txt"),
            "../evidence.txt".into(),
            "link/evidence.txt".into(),
            "linked.txt".into(),
            "directory".into(),
        ] {
            let mut report = complete(&root);
            report.artifacts[0].path = path;
            assert!(admit(&load(&report, &root), &[Capability::Confinement]).is_err());
        }
    }

    #[test]
    fn nested_evidence_is_supported_and_oversized_files_fail() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        let mut report = complete(&root);
        fs::rename(
            root.path().join("evidence.txt"),
            root.path().join("nested/probe.txt"),
        )
        .unwrap();
        report.artifacts[0].path = "nested/probe.txt".into();
        assert_eq!(admit(&load(&report, &root), &Capability::ALL), Ok(()));
        File::options()
            .write(true)
            .open(root.path().join("nested/probe.txt"))
            .unwrap()
            .set_len(ARTIFACT_BYTES_MAX + 1)
            .unwrap();
        assert!(admit(&load(&report, &root), &Capability::ALL).is_err());
        let path = root.path().join("oversized.json");
        File::create(&path)
            .unwrap()
            .set_len(REPORT_BYTES_MAX + 1)
            .unwrap();
        assert!(load_report(&path, context(&root)).is_err());
    }

    #[test]
    fn malformed_unknown_and_duplicate_json_never_loads() {
        let root = TempDir::new().unwrap();
        let original = serde_json::to_string(&complete(&root)).unwrap();
        let variants = [
            "{".into(),
            original.replacen('{', "{\"context\":{},", 1),
            original.replace(
                "\"confinement\":\"proven\"",
                "\"confinement\":\"failed\",\"confinement\":\"proven\"",
            ),
            original.replace(
                "\"test-runtime\":\"1.0\"",
                "\"test-runtime\":\"0.0\",\"test-runtime\":\"1.0\"",
            ),
        ];
        for bytes in variants {
            let path = root.path().join("bad.json");
            fs::write(&path, bytes).unwrap();
            assert!(load_report(&path, context(&root)).is_err());
        }
    }

    #[test]
    #[should_panic(expected = "bytes_max > 0")]
    fn internal_hash_limit_cannot_be_zero() {
        let root = TempDir::new().unwrap();
        let file = File::create(root.path().join("probe.txt")).unwrap();
        let _ = hash_file(file, 0);
    }

    #[test]
    #[should_panic(expected = "bytes_max <= ARTIFACT_BYTES_MAX")]
    fn internal_hash_limit_cannot_exceed_policy_bound() {
        let root = TempDir::new().unwrap();
        let file = File::create(root.path().join("probe.txt")).unwrap();
        let _ = hash_file(file, ARTIFACT_BYTES_MAX + 1);
    }
}
