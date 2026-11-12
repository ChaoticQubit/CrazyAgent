use crate::args::Usage;
use crate::{Failure, Place, now_ms};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use trackwright_core::coverage;
use trackwright_core::host::Authority;
use trackwright_core::model::{Actor, OpKind, ProjectKey, ShortId};
use trackwright_core::results::{self, ResultFormat, ResultRow};
use trackwright_core::runs::{
    self, NetworkMode, NewResult, PolicyRef, RunEnd, RunFinish, RunRecord, RunShow, RunStart,
    RunView, SourceRef, Toolchain, ToolchainFamily,
};
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{Store, StoreError};
use trackwright_core::tests_model::{
    CaseKind, RESULT_ROWS_PER_RECORD_MAX, RESULTS_PER_RUN_MAX, RUN_TITLE_BYTES_MAX, TestId,
    TestObject,
};

/// The code of an import refused before parsing: a link, not a regular file, unreadable,
/// or over its format's byte bound.
pub const IMPORT_FILE_REFUSED: &str = "import_file_refused";
/// The code of an import whose report the parser refused, or that shows a build failure
/// while importing into a running run.
pub const IMPORT_REPORT_REFUSED: &str = "import_report_refused";
/// The digest a declared import binds where no profile, sandbox or environment exists.
const NO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const _: () = assert!(NO_DIGEST.len() == 64);
const _: () = assert!(RESULT_ROWS_PER_RECORD_MAX <= RESULTS_PER_RUN_MAX);

/// One `run import` call: the report's format and file, and where its results go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub format: ResultFormat,
    pub file: PathBuf,
    pub target: Target,
}

/// Where an import records: into a running run, or into a new declared run of a project
/// that the import starts and finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Run(TestId),
    Project(ProjectKey),
}

/// Reads `run import`'s flags: `--format`, `--file` and exactly one of `--run` and
/// `--project`. Inputs: the subcommand word and the flags. Output: the import, or a
/// one-line usage error.
pub fn command(sub: Option<&str>, fields: &[(String, &str)]) -> Result<Import, Usage> {
    if sub != Some("import") {
        return Err(Usage::new(
            "run takes the subcommand import; see run --help",
        ));
    }
    let mut flags = [
        ("format", None),
        ("file", None),
        ("run", None),
        ("project", None),
    ];
    for (name, value) in fields {
        let slot = flags
            .iter_mut()
            .find(|(flag, _)| flag == name)
            .ok_or_else(|| Usage::new(format!("unknown flag --{name} for run import")))?;
        if slot.1.replace(*value).is_some() {
            return Err(Usage::new(format!("--{name} given twice")));
        }
    }
    let [(_, format), (_, file), (_, run), (_, project)] = flags;
    let format = match format {
        Some("junit") => ResultFormat::Junit,
        Some("allure") => ResultFormat::Allure,
        Some("libtest") => ResultFormat::Libtest,
        _ => {
            return Err(Usage::new(
                "run import needs --format junit, allure or libtest",
            ));
        }
    };
    let file = file.ok_or_else(|| Usage::new("run import needs --file PATH"))?;
    let target = match (run, project) {
        (Some(run), None) => TestId::parse_of(run, TestObject::Run)
            .map(Target::Run)
            .map_err(|_| Usage::new("--run takes a run id such as CA-R7"))?,
        (None, Some(project)) => ProjectKey::new(project)
            .map(Target::Project)
            .map_err(|_| Usage::new("--project takes a key of 2 to 8 letters A-Z"))?,
        _ => {
            return Err(Usage::new(
                "run import needs exactly one of --run and --project",
            ));
        }
    };
    assert!(!file.is_empty());
    Ok(Import {
        format,
        file: PathBuf::from(file),
        target,
    })
}

/// Imports one report: reads the file (no link, a regular file within its format's
/// bound), parses it, then in one transaction records the rows in batches of at most 500
/// into the running run (a build-failed report is refused there), or starts, records and
/// finishes a declared run of the project. Inputs: the actor, the place and the import.
/// Output: the envelope, or the refusal; a refused import writes nothing.
pub(crate) fn run(actor: &str, place: &Place, import: &Import) -> Result<Value, Failure> {
    let actor = Actor::new(actor)?;
    let bytes = report(&import.file, import.format.bytes_max())?;
    let parsed = results::parse(import.format, &bytes).map_err(|refusal| {
        Failure::new(
            IMPORT_REPORT_REFUSED,
            format!("the report is refused: {refusal}"),
        )
    })?;
    if let (true, Target::Run(id)) = (parsed.build_failed, &import.target) {
        return Err(Failure::new(
            IMPORT_REPORT_REFUSED,
            format!("the report shows a build failure; finish {id} with end build_failed"),
        ));
    }
    assert!(parsed.rows.len() <= RESULTS_PER_RUN_MAX);
    let rows: Vec<NewResult> = parsed.rows.iter().map(new_result).collect();
    let mut store = Store::open(&place.existing()?)?;
    let tx = store.transaction()?;
    let now_ms = now_ms()?;
    let mut run = match &import.target {
        Target::Run(id) => {
            Standalone.actor_may(&tx, id.key(), &actor, OpKind::RunRecord, None)?;
            let shown = RunShow {
                id: id.clone(),
                after: None,
            };
            runs::show(&tx, &shown)?.run
        }
        Target::Project(key) => {
            Standalone.actor_may(&tx, key, &actor, OpKind::RunStart, None)?;
            runs::start(&tx, &actor, &start_of(import, key)?, now_ms)?
        }
    };
    for batch in rows.chunks(RESULT_ROWS_PER_RECORD_MAX) {
        let version = run.version;
        let input = RunRecord {
            id: run.id.clone(),
            version,
            results: batch.to_vec(),
        };
        run = runs::record(&tx, &actor, &input, now_ms)?;
        assert!(run.version > version);
    }
    let tickets = match &import.target {
        Target::Run(_) => Vec::new(),
        Target::Project(_) => {
            let finish = finish_of(&run, parsed.build_failed);
            let finished = coverage::finish(&tx, &Standalone, &actor, &finish, now_ms)?;
            run = finished.run;
            finished.tickets
        }
    };
    tx.commit().map_err(StoreError::from)?;
    Ok(envelope(&run, rows.len(), &tickets))
}

fn start_of(import: &Import, key: &ProjectKey) -> Result<RunStart, Failure> {
    let name = import
        .file
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| refused(&import.file, "has no UTF-8 file name"))?;
    assert!(!name.is_empty());
    Ok(RunStart {
        project: key.clone(),
        title: cut(&format!("import {} {name}", format_name(import.format))),
        plan: None,
        batch: None,
        source: SourceRef::Declared {
            reference: name.to_owned(),
        },
        policy: import_policy(import.format),
    })
}

fn finish_of(run: &RunView, build_failed: bool) -> RunFinish {
    assert!(
        run.finished_ms.is_none(),
        "an import finishes the run it started"
    );
    let end = if build_failed {
        RunEnd::BuildFailed
    } else {
        RunEnd::Completed
    };
    RunFinish {
        id: run.id.clone(),
        version: run.version,
        end,
        exit_code: None,
        reason: None,
        integrity: None,
    }
}

fn report(path: &Path, bytes_max: usize) -> Result<Vec<u8>, Failure> {
    let unreadable =
        |error: std::io::Error| refused(path, &format!("cannot be read: {}", error.kind()));
    let linked = fs::symlink_metadata(path).map_err(unreadable)?;
    if linked.file_type().is_symlink() {
        return Err(refused(
            path,
            "is a symbolic link; name the report file itself",
        ));
    }
    if !linked.is_file() {
        return Err(refused(path, "is not a regular file"));
    }
    let over = || {
        refused(
            path,
            &format!("is over the {bytes_max} byte bound of its format"),
        )
    };
    if linked.len() > bytes_max as u64 {
        return Err(over());
    }
    let file = File::open(path).map_err(unreadable)?;
    let opened = file.metadata().map_err(unreadable)?;
    if !same_file(&linked, &opened) {
        return Err(refused(path, "changed while it was opened"));
    }
    let mut bytes = Vec::new();
    file.take(bytes_max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > bytes_max {
        return Err(over());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(linked: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    opened.is_file() && linked.dev() == opened.dev() && linked.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_linked: &fs::Metadata, opened: &fs::Metadata) -> bool {
    opened.is_file()
}

fn refused(path: &Path, why: &str) -> Failure {
    Failure::new(IMPORT_FILE_REFUSED, format!("{} {why}", path.display()))
}

fn new_result(row: &ResultRow) -> NewResult {
    NewResult {
        name: row.name.clone(),
        outcome: row.outcome,
        duration_ms: row
            .duration_ms
            .map(|ms| u32::try_from(ms).unwrap_or(u32::MAX)),
        message: row.message.clone(),
        artifact: None,
    }
}

fn import_policy(format: ResultFormat) -> PolicyRef {
    PolicyRef {
        profile: NO_DIGEST.to_owned(),
        profile_approved_ms: 0,
        check: format!("import-{}", format_name(format)),
        kind: CaseKind::Unit,
        argv: Vec::new(),
        toolchain: Toolchain {
            family: ToolchainFamily::None,
            version: String::new(),
        },
        sandbox: NO_DIGEST.to_owned(),
        network: NetworkMode::Off,
        timeout_s: 0,
        environment: NO_DIGEST.to_owned(),
    }
}

fn format_name(format: ResultFormat) -> &'static str {
    match format {
        ResultFormat::Junit => "junit",
        ResultFormat::Allure => "allure",
        ResultFormat::Libtest => "libtest",
    }
}

fn cut(text: &str) -> String {
    let mut end = text.len().min(RUN_TITLE_BYTES_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    assert!(end > 0);
    text[..end].to_owned()
}

fn envelope(run: &RunView, recorded: usize, tickets: &[ShortId]) -> Value {
    assert!(run.totals.total as usize >= recorded);
    let id = run.id.to_string();
    let tickets: Vec<String> = tickets.iter().map(ShortId::to_string).collect();
    json!({
        "api": 1,
        "ok": true,
        "data": {
            "run": id,
            "recorded": recorded,
            "state": run.state,
            "verdict": run.verdict,
            "totals": run.totals,
            "tickets": tickets,
        },
        "next": [{"op": "run_show", "id": id, "why": "read the run's results and binding"}],
        "cursor": null,
    })
}
