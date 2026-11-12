use super::*;
use crate::checks::settings::{ChecksSettings, Scope as Layer, choices, read, write};
use crate::checks::{Actor, Trigger};
use crate::engine::owner::{CheckGapView, GATE_PATHS_SHOWN, SettingChoices};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::tui::client::settings::SettingsEvent;
use crate::tui::screens::new_project_view::Look;
use crate::tui::screens::tests::{TestPage, drawn};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use trackwright_cli::views::Key as ViewKey;
use trackwright_core::model::Actor as Writer;
use trackwright_core::ops::dispatch;
use trackwright_core::requests::Request;
use trackwright_core::runs::{
    ENGINE_ACTOR, IntegrityRef, Inventory, NetworkMode, NewResult, PolicyRef, ResultOutcome,
    RunEnd, RunFinish, RunRecord, RunStart, RunView, SourceRef, Toolchain, ToolchainFamily,
};
use trackwright_core::tests_model::CaseKind;

const NOW: i64 = 1_800_000_000_000;

/// The head commit of the change set the checks tests use.
pub(in crate::tui) const HEAD: &str = "4f1c2b9e7d3a6c8b0e5f2a1d9c7b3e6f8a0d2c4b";
/// A commit before that head, which a stale run covered.
pub(in crate::tui) const EARLIER: &str = "a7e3d1c9b5f2e8a4c0d6b2f9e1a3c7d5b8f0e2a6";
const TREE: &str = "9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c";
const BASE: &str = "0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c";
const PROFILE: &str = "5e7f9a1b3c5d7e9f1a3b5c7d9e1f3a5b7c9d1e3f5a7b9c1d3e5f7a9b1c3d5e7f";
const SANDBOX: &str = "c3a5e7f9b1d3c5e7a9f1b3d5c7e9a1f3b5d7c9e1a3f5b7d9c1e3a5f7b9d1c3e5";
const ENVIRONMENT: &str = "e1d3c5b7a9f1e3d5c7b9a1f3e5d7c9b1a3f5e7d9c1b3a5f7e9d1c3b5a7f9e1d3";

/// The engine project the TUI checks tests use, with its tracker key `FM`.
pub(in crate::tui) fn fieldnotes() -> ProjectId {
    ProjectId::new("fieldnotes-mobile").unwrap()
}

/// The change set the checks tests read and start.
pub(in crate::tui) fn change_set() -> ChangeSetId {
    ChangeSetId::new("c0ffee00c0ffee00c0ffee00c0ffee01").unwrap()
}

/// One check run as the engine's checks host records it: the check, the commit it covered,
/// and each result's name and outcome; the rest of its source, policy and integrity is the
/// same for every run.
pub(in crate::tui) struct Ran<'a> {
    pub check: &'a str,
    pub commit: &'a str,
    pub results: &'a [(&'a str, ResultOutcome)],
}

/// The engine the owner worker reaches, without the socket: a real engine database with
/// every authority schema, Fieldnotes Mobile as a project and, once attached, in the
/// tracker as `FM`. Tracker calls run through the core with the engine host as the owner,
/// committed only when ok; check runs are written by the engine's checks writer; settings
/// and profiles go through the engine's checks functions as the owner.
pub(in crate::tui) struct Engine {
    _tmp: tempfile::TempDir,
    db: Connection,
    latest: Vec<CheckRunView>,
}

impl Engine {
    /// Opens the engine database with Fieldnotes Mobile as a project and no tracker
    /// project. Output: the engine.
    pub(in crate::tui) fn bare() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let db = crate::landing::open(&root.join("engine")).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        let folder = root.join("fieldnotes-mobile");
        std::fs::create_dir_all(folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
        crate::ide::workspace::tests::add_project(&db, fieldnotes().as_str(), &folder);
        Self {
            _tmp: tmp,
            db,
            latest: Vec::new(),
        }
    }

    /// Opens the engine and attaches Fieldnotes Mobile to the tracker as `FM`. Output: the
    /// engine, with no runs yet.
    pub(in crate::tui) fn new() -> Self {
        let engine = Self::bare();
        let tx = engine.tx();
        crate::tracker::attach(
            &tx,
            &fieldnotes(),
            &ProjectKey::new("FM").unwrap(),
            "Fieldnotes Mobile",
            NOW,
        )
        .unwrap();
        tx.commit().unwrap();
        engine
    }

    /// The project's tracker key, as the owner worker reads it. Output: `FM` once
    /// attached, else none.
    pub(in crate::tui) fn tracked(&self) -> Option<ProjectKey> {
        let tx = self.tx();
        crate::tracker::key_of(&tx, &fieldnotes()).unwrap()
    }

    /// Records one finished check run of the change set as the checks host does: start,
    /// record the results, finish with integrity evidence. Input: the run. Output: the
    /// finished run.
    pub(in crate::tui) fn ran(&mut self, ran: &Ran<'_>) -> RunView {
        let writer = Writer::new(ENGINE_ACTOR).unwrap();
        let start = RunStart {
            project: ProjectKey::new("FM").unwrap(),
            title: ran.check.to_owned(),
            plan: None,
            batch: Some("b0123456789abcdef".to_owned()),
            source: SourceRef::Git {
                repository: "fieldnotes-mobile".to_owned(),
                commit: ran.commit.to_owned(),
                tree: TREE.to_owned(),
                base: Some(BASE.to_owned()),
                change_set: Some(change_set().as_str().to_owned()),
            },
            policy: PolicyRef {
                profile: PROFILE.to_owned(),
                profile_approved_ms: NOW - 60_000,
                check: ran.check.to_owned(),
                kind: CaseKind::Unit,
                argv: ["cargo", "test", "--locked", "--offline", "--workspace"]
                    .map(str::to_owned)
                    .to_vec(),
                toolchain: Toolchain {
                    family: ToolchainFamily::Rust,
                    version: "rustc 1.91.0".to_owned(),
                },
                sandbox: SANDBOX.to_owned(),
                network: NetworkMode::Off,
                timeout_s: 1800,
                environment: ENVIRONMENT.to_owned(),
            },
        };
        let tx = self.tx();
        let started = trackwright_core::runs::start(&tx, &writer, &start, NOW).unwrap();
        let results = ran
            .results
            .iter()
            .map(|(name, outcome)| NewResult {
                name: (*name).to_owned(),
                outcome: *outcome,
                duration_ms: Some(40),
                message: (*outcome == ResultOutcome::Failed)
                    .then(|| "expected the accented caption".to_owned()),
                artifact: None,
            })
            .collect();
        let recorded = trackwright_core::runs::record(
            &tx,
            &writer,
            &RunRecord {
                id: started.id.clone(),
                version: started.version,
                results,
            },
            NOW + 1_000,
        )
        .unwrap();
        let executed = ran
            .results
            .iter()
            .filter(|(_, outcome)| outcome.executed())
            .count();
        let finished = trackwright_core::runs::finish(
            &tx,
            &writer,
            &RunFinish {
                id: recorded.id.clone(),
                version: recorded.version,
                end: RunEnd::Completed,
                exit_code: Some(i32::from(executed != ran.results.len())),
                reason: None,
                integrity: Some(IntegrityRef {
                    executed: u32::try_from(executed).unwrap(),
                    inventory: Inventory {
                        baseline_run: None,
                        removed: Vec::new(),
                        removed_total: 0,
                    },
                    new_skips: Vec::new(),
                    protected_touched: Vec::new(),
                    holdouts_ran: false,
                    build_ok: true,
                }),
            },
            NOW + 2_000,
        )
        .unwrap();
        tx.commit().unwrap();
        self.latest.retain(|run| run.check != finished.policy.check);
        self.latest.push(checked(&finished));
        finished
    }

    /// What a check settings page reads for one scope, from the layers the engine stores
    /// and the engine's options for every key. Input: the project, None for the app.
    /// Output: the view.
    pub(in crate::tui) fn settings_view(&self, project: Option<&ProjectId>) -> ChecksSettingsView {
        let (app, app_version) = read(&self.db, &Layer::App).unwrap();
        let below_project = ChecksSettings::resolve(&app, &ChecksChoice::default());
        let (values, version, below) = match project {
            None => (app, app_version, ChecksSettings::default()),
            Some(project) => {
                let (own, version) = read(&self.db, &Layer::Project(project.clone())).unwrap();
                (own, version, below_project)
            }
        };
        let effective = match project {
            None => below_project,
            Some(_) => ChecksSettings::resolve(&app, &values),
        };
        ChecksSettingsView {
            project: project.cloned(),
            values,
            version,
            below,
            effective,
            choices: choices()
                .into_iter()
                .map(|choice| SettingChoices {
                    key: choice.key.to_owned(),
                    options: choice.options,
                    proposed: choice.default,
                    custom: None,
                })
                .collect(),
        }
    }

    /// Saves one scope's check settings as the owner. Inputs: the project (None for the
    /// app), the values and the version the page read. Output: saved at the new version,
    /// stale with the current one, or the engine's code.
    pub(in crate::tui) fn save_settings(
        &mut self,
        project: Option<&ProjectId>,
        values: &ChecksChoice,
        version: u64,
    ) -> Result<SettingsSaved, String> {
        let layer = match project {
            None => Layer::App,
            Some(project) => Layer::Project(project.clone()),
        };
        match write(&self.db, &layer, values, version, Actor::Owner) {
            Ok(version) => Ok(SettingsSaved::Saved { version }),
            Err(error) if error.0 == "checks_settings_stale" => Ok(SettingsSaved::Stale {
                current: read(&self.db, &layer).unwrap().1,
            }),
            Err(error) => Err(error.0),
        }
    }

    /// Stores a profile with the given checks as the owner proposes it. Input: the check
    /// ids. Output: the stored profile's id.
    pub(in crate::tui) fn propose(&self, checks: &[&str]) -> ProfileId {
        let checks: Vec<Value> = checks.iter().map(|id| check(id)).collect();
        self.propose_body(
            &json!({"version": 1, "checks": checks, "holdouts": [], "protected_paths": []}),
        )
    }

    /// Stores a whole profile as the owner proposes it. Input: the profile JSON. Output:
    /// the stored profile's id.
    pub(in crate::tui) fn propose_body(&self, body: &Value) -> ProfileId {
        crate::checks::propose(
            &self.db,
            &fieldnotes(),
            &serde_json::to_vec(body).unwrap(),
            Actor::Owner,
        )
        .unwrap()
        .id
    }

    /// Stores one holdout file's bytes as the owner gives them. Input: the bytes. Output:
    /// the digest a profile names the file by.
    pub(in crate::tui) fn holdout(&self, bytes: &[u8]) -> String {
        crate::checks::store_holdout_file(&self.db, &fieldnotes(), bytes, Actor::Owner).unwrap()
    }

    /// The project's stored profiles, newest first, as the engine lists them. Output: the
    /// summaries, or the engine's code.
    pub(in crate::tui) fn profiles(&self) -> Result<Vec<ProfileSummary>, String> {
        crate::checks::profiles(&self.db, &fieldnotes(), None)
            .map(|page| page.profiles)
            .map_err(|error| error.0)
    }

    fn tx(&self) -> Transaction<'_> {
        Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap()
    }
}

/// One ordinary check of a profile: a unit check running the workspace tests offline on
/// request. Input: its id. Output: the check's JSON.
pub(in crate::tui) fn check(id: &str) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": "rust",
        "argv": ["cargo", "test", "--locked", "--offline"],
        "cwd": ".",
        "format": "libtest",
        "report": null,
        "timeout_s": 600,
        "network": "off",
        "when": ["on_request"],
        "required": true,
        "protected": false
    })
}

/// The newest run of one check as a change set's runs reply carries it. Input: the run.
/// Output: the view.
fn checked(run: &RunView) -> CheckRunView {
    let SourceRef::Git { commit, tree, .. } = &run.source else {
        panic!("a check run covers a git commit");
    };
    CheckRunView {
        check: run.policy.check.clone(),
        run_id: run.id.clone(),
        batch: run.batch.clone().unwrap_or_default(),
        state: run.state,
        verdict: run.verdict,
        commit: commit.clone(),
        tree: tree.clone(),
        profile: ProfileId::new(run.policy.profile.clone()).unwrap(),
        profile_approved_ms: run.policy.profile_approved_ms,
        protected_touched: run.integrity.as_ref().map_or(0, |integrity| {
            u32::try_from(integrity.protected_touched.len()).unwrap()
        }),
        holdout: false,
        finished_ms: run.finished_ms,
    }
}

impl Client for Engine {
    fn call(&mut self, op: OpKind, input: Value) -> Value {
        let bytes = serde_json::to_vec(&input).unwrap();
        let request = match Request::parse(op.as_str(), &bytes) {
            Ok(request) => request,
            Err(refusal) => {
                return json!({
                    "api": 1,
                    "ok": false,
                    "code": refusal.code,
                    "message": refusal.message,
                    "next": [],
                });
            }
        };
        let tx = self.tx();
        let reply = dispatch(
            &tx,
            &crate::tracker::Host,
            &crate::tracker::owner(),
            request,
        );
        let wire: Value = serde_json::from_str(reply.json()).unwrap();
        assert_eq!(wire["ok"].as_bool(), Some(reply.is_ok()));
        if reply.is_ok() {
            tx.commit().unwrap();
        }
        wire
    }
}

/// A client that passes every call to the engine and keeps each op and input it sent, so a
/// test can read what a key asked for.
pub(in crate::tui) struct Recorder<'a> {
    pub engine: &'a mut Engine,
    pub sent: Vec<(OpKind, Value)>,
}

impl Client for Recorder<'_> {
    fn call(&mut self, op: OpKind, input: Value) -> Value {
        self.sent.push((op, input.clone()));
        self.engine.call(op, input)
    }
}

/// Answers one checks job the way the owner worker does, against the engine: views jobs
/// run their steps through the engine as the tracker client, settings and profiles go
/// through the engine's checks functions as the owner, a shown profile is held to the id
/// asked for as the owner worker holds it, a start goes to the engine's check
/// queue, and a runs read gives the newest recorded run of each check. Inputs: the job and
/// the engine. Output: the Settings event the screens take.
pub(in crate::tui) fn answer(job: ChecksJob, engine: &mut Engine) -> SettingsEvent {
    let event = match job {
        ChecksJob::Tracked { project } => ChecksEvent::Tracked {
            result: Ok(engine.tracked().filter(|_| project == fieldnotes())),
            project,
        },
        ChecksJob::Views {
            project,
            views,
            steps,
        } => {
            run(&views, &steps, engine);
            ChecksEvent::Views {
                project,
                problem: None,
            }
        }
        ChecksJob::Settings { project } => ChecksEvent::Settings {
            result: Ok(engine.settings_view(project.as_ref())),
            project,
        },
        ChecksJob::SaveSettings {
            project,
            values,
            version,
        } => ChecksEvent::Saved {
            result: engine.save_settings(project.as_ref(), &values, version),
            project,
        },
        ChecksJob::Profiles { project } => ChecksEvent::Profiles {
            result: engine.profiles(),
            project,
        },
        ChecksJob::Show { project, profile } => ChecksEvent::Shown {
            result: crate::checks::show(&engine.db, &project, &profile)
                .map_err(|error| error.0)
                .and_then(|found| found.ok_or_else(|| "checks_profile_unknown".to_owned()))
                .and_then(|stored| matching(stored, &profile)),
            project,
            profile,
        },
        ChecksJob::Approve { project, profile } => ChecksEvent::Profile {
            result: crate::checks::approve(&engine.db, &project, &profile, Actor::Owner)
                .map(|_| ())
                .map_err(|error| error.0),
            project,
            profile,
            approve: true,
        },
        ChecksJob::Revoke { project, profile } => ChecksEvent::Profile {
            result: crate::checks::revoke(&engine.db, &project, &profile, Actor::Owner)
                .map_err(|error| error.0),
            project,
            profile,
            approve: false,
        },
        ChecksJob::Runs { change_set, .. } => ChecksEvent::Runs {
            result: Ok(ChangeChecks {
                runs: engine.latest.clone(),
                pending: 0,
                gate: gate_view(&engine.db, &change_set),
            }),
            change_set,
        },
        ChecksJob::Run { change_set, .. } => ChecksEvent::Started {
            result: crate::checks::queue::submit(
                &engine.db,
                &change_set,
                Trigger::OnRequest,
                Actor::Owner,
                &[],
            )
            .map(|batch| batch.as_str().to_owned())
            .map_err(|error| error.0),
            change_set,
        },
    };
    SettingsEvent::Checks(Box::new(event))
}

/// What the Accept gate meets at the head, read through the engine's gate as the owner
/// protocol's gate view does. Inputs: the database and the change set. Output: the view,
/// or the gate's code.
fn gate_view(db: &Connection, change_set: &ChangeSetId) -> Result<CheckGateView, String> {
    let note = crate::checks::gate::assess(db, change_set, HEAD).map_err(|error| error.0)?;
    let refusal = crate::checks::gate::refusal(&note).map(str::to_owned);
    Ok(CheckGateView {
        gate: note.gate,
        removed_tests: note.removed_tests,
        head: note.head,
        profile: note.profile,
        refusal,
        gaps_total: u32::try_from(note.gaps.len()).unwrap(),
        gaps: note
            .gaps
            .into_iter()
            .map(|gap| CheckGapView {
                code: gap.code,
                check: gap.check,
                run: gap.run,
                paths: gap.paths.into_iter().take(GATE_PATHS_SHOWN).collect(),
                paths_total: gap.paths_total,
            })
            .collect(),
    })
}

/// The text of the views drawn into an area, one string per row with trailing spaces
/// dropped. Inputs: the views, the width and the height. Output: the rows.
pub(in crate::tui) fn text_of(views: &TestViews, width: usize, height: usize) -> Vec<String> {
    drawn(views, width, height, &Look::plain())
        .iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

/// The two runs the page tests read: `unit` on the head with one failure, then `lint` on
/// an earlier commit with every result passed. Input: the engine. Output: the engine.
pub(in crate::tui) fn with_runs(mut engine: Engine) -> Engine {
    engine.ran(&Ran {
        check: "unit",
        commit: HEAD,
        results: &[
            ("notes::sync::keeps_order", ResultOutcome::Passed),
            ("notes::sync::retries_offline", ResultOutcome::Passed),
            ("photos::caption::keeps_accents", ResultOutcome::Failed),
        ],
    });
    engine.ran(&Ran {
        check: "lint",
        commit: EARLIER,
        results: &[("clippy", ResultOutcome::Passed)],
    });
    engine
}

#[test]
fn run_takes_the_steps_in_order_through_the_client() {
    let mut engine = with_runs(Engine::new());
    let views = Mutex::new(TestViews::new(ProjectKey::new("FM").unwrap()));
    let mut recorder = Recorder {
        engine: &mut engine,
        sent: Vec::new(),
    };
    run(
        &views,
        &[Step::Open(TestPage::Runs), Step::Key(ViewKey::Down)],
        &mut recorder,
    );
    assert_eq!(
        recorder.sent,
        vec![(OpKind::RunList, json!({ "project": "FM" }))]
    );
    let rows = text_of(&views.lock().unwrap(), 76, 12);
    assert!(rows[0].starts_with(" FM  Runs"), "{rows:?}");
    assert!(rows[3].starts_with("   FM-R2"), "{rows:?}");
    assert!(rows[4].starts_with(" > FM-R1"), "{rows:?}");
}

#[test]
fn a_step_list_longer_than_one_job_is_refused() {
    let mut engine = Engine::new();
    let views = Mutex::new(TestViews::new(ProjectKey::new("FM").unwrap()));
    let steps = vec![Step::Key(ViewKey::Down); STEPS_PER_JOB_MAX + 1];
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&views, &steps, &mut engine);
    }));
    assert!(ran.is_err());
}

#[test]
fn a_call_the_engine_did_not_answer_reads_as_a_refusal_with_its_code() {
    let envelope = refused("tracker_unavailable");
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["code"], json!("tracker_unavailable"));
    assert_eq!(envelope["api"], json!(1));
}

#[test]
fn a_job_without_a_trusted_connection_answers_its_own_event_with_the_code() {
    let code = "trusted_connection_unavailable".to_owned();
    let profile = ProfileId::new(PROFILE).unwrap();
    let views = Arc::new(Mutex::new(TestViews::new(ProjectKey::new("FM").unwrap())));
    let jobs = [
        ChecksJob::Tracked {
            project: fieldnotes(),
        },
        ChecksJob::Views {
            project: fieldnotes(),
            views,
            steps: Vec::new(),
        },
        ChecksJob::Settings { project: None },
        ChecksJob::SaveSettings {
            project: Some(fieldnotes()),
            values: ChecksChoice::default(),
            version: 0,
        },
        ChecksJob::Profiles {
            project: fieldnotes(),
        },
        ChecksJob::Show {
            project: fieldnotes(),
            profile: profile.clone(),
        },
        ChecksJob::Approve {
            project: fieldnotes(),
            profile: profile.clone(),
        },
        ChecksJob::Revoke {
            project: fieldnotes(),
            profile: profile.clone(),
        },
        ChecksJob::Runs {
            project: fieldnotes(),
            change_set: change_set(),
        },
        ChecksJob::Run {
            project: fieldnotes(),
            change_set: change_set(),
        },
    ];
    let events: Vec<ChecksEvent> = jobs.iter().map(ChecksEvent::unavailable).collect();
    assert_eq!(
        events,
        vec![
            ChecksEvent::Tracked {
                project: fieldnotes(),
                result: Err(code.clone()),
            },
            ChecksEvent::Views {
                project: fieldnotes(),
                problem: Some(code.clone()),
            },
            ChecksEvent::Settings {
                project: None,
                result: Err(code.clone()),
            },
            ChecksEvent::Saved {
                project: Some(fieldnotes()),
                result: Err(code.clone()),
            },
            ChecksEvent::Profiles {
                project: fieldnotes(),
                result: Err(code.clone()),
            },
            ChecksEvent::Shown {
                project: fieldnotes(),
                profile: profile.clone(),
                result: Err(code.clone()),
            },
            ChecksEvent::Profile {
                project: fieldnotes(),
                profile: profile.clone(),
                approve: true,
                result: Err(code.clone()),
            },
            ChecksEvent::Profile {
                project: fieldnotes(),
                profile,
                approve: false,
                result: Err(code.clone()),
            },
            ChecksEvent::Runs {
                change_set: change_set(),
                result: Err(code.clone()),
            },
            ChecksEvent::Started {
                change_set: change_set(),
                result: Err(code),
            },
        ]
    );
    assert_eq!(
        jobs.iter().map(job_name).collect::<Vec<_>>(),
        [
            "ChecksTracked",
            "ChecksViews",
            "ChecksSettings",
            "ChecksSaveSettings",
            "ChecksProfiles",
            "ChecksShow",
            "ChecksApprove",
            "ChecksRevoke",
            "ChecksRuns",
            "ChecksRun",
        ]
    );
}

#[test]
fn a_save_at_an_old_version_is_stale_and_writes_nothing() {
    let mut engine = Engine::new();
    let values = ChecksChoice {
        parallel: Some(2),
        ..ChecksChoice::default()
    };
    assert_eq!(
        engine.save_settings(Some(&fieldnotes()), &values, 0),
        Ok(SettingsSaved::Saved { version: 1 })
    );
    let other = ChecksChoice {
        parallel: Some(4),
        ..ChecksChoice::default()
    };
    assert_eq!(
        engine.save_settings(Some(&fieldnotes()), &other, 0),
        Ok(SettingsSaved::Stale { current: 1 })
    );
    let view = engine.settings_view(Some(&fieldnotes()));
    assert_eq!((view.values, view.version), (values, 1));
    assert_eq!(view.effective.parallel, 2);
}

#[test]
fn a_shown_profile_is_kept_only_when_its_body_hashes_to_the_id_asked_for() {
    let engine = Engine::new();
    let id = engine.propose(&["unit"]);
    let other = engine.propose(&["unit", "lint"]);
    let stored = crate::checks::show(&engine.db, &fieldnotes(), &id)
        .unwrap()
        .unwrap();
    assert_eq!(matching(stored.clone(), &id), Ok(Box::new(stored.clone())));
    assert_eq!(
        matching(stored.clone(), &other),
        Err(UNKNOWN.to_owned()),
        "another id is not the profile asked for"
    );
    let mut altered = stored;
    altered.profile.checks[0].network = crate::checks::NetworkMode::Remotes;
    assert_eq!(
        matching(altered, &id),
        Err(UNKNOWN.to_owned()),
        "a body that does not hash to its id is not shown for approval"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_lost_connection_a_tests_views_job_calls_the_engine_no_more() {
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::OwnerClientKind;
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::Builder::new()
        .prefix("ca-tui-ck-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let (client, _) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Tui,
        std::time::Duration::from_secs(20),
    )
    .await
    .unwrap();
    let client = Arc::new(client);
    let handle = tokio::runtime::Handle::current();
    let (answered, after_refused, after_lost, kept) = tokio::task::spawn_blocking(move || {
        let mut tests = OwnerTests {
            client,
            handle,
            failure: None,
        };
        let answered = tests.call(OpKind::Schema, json!({}));
        tests.failure = Some(EngineError("tracker_unavailable".into()));
        let after_refused = tests.call(OpKind::Schema, json!({}));
        tests.failure = Some(EngineError("tracker_outcome_unknown".into()));
        let after_lost = tests.call(OpKind::Schema, json!({}));
        (
            answered,
            after_refused,
            after_lost,
            tests.failure.map(|error| error.0),
        )
    })
    .await
    .unwrap();
    assert_eq!(answered["ok"], json!(true), "{answered}");
    assert_eq!(
        after_refused["ok"],
        json!(true),
        "a connect the engine refused is no lost connection: {after_refused}"
    );
    assert_eq!(after_lost, refused("tracker_outcome_unknown"));
    assert_eq!(kept.as_deref(), Some("tracker_outcome_unknown"));
    tokio::time::timeout(std::time::Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
