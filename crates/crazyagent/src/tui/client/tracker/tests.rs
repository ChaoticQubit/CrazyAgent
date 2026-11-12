use super::*;
use crate::engine::owner::{CustomBounds, SettingChoices};
use crate::tui::client::settings::SettingsEvent;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use trackwright_core::ops::dispatch;
use trackwright_core::requests::Request;

const NOW: i64 = 1_800_000_000_000;

/// The engine project the TUI tracker tests use, with its tracker key `FM`.
pub(in crate::tui) fn fieldnotes() -> ProjectId {
    ProjectId::new("fieldnotes-mobile").unwrap()
}

/// The tracker the owner worker reaches, without the socket: a real engine database with
/// every authority schema and the tracker's tables, the project attached as `FM`, and each
/// call run through the core with the engine host as the owner, committed only when ok.
pub(in crate::tui) struct Engine {
    _tmp: tempfile::TempDir,
    db: Connection,
}

impl Engine {
    /// Opens the engine database with no project attached to the tracker. Output: the
    /// engine.
    pub(in crate::tui) fn bare() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let db = crate::landing::open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        Self { _tmp: tmp, db }
    }

    /// Opens the engine database and attaches Fieldnotes Mobile. Output: the engine, with
    /// no tickets yet.
    pub(in crate::tui) fn new() -> Self {
        let mut engine = Self::bare();
        engine
            .attach(
                &fieldnotes(),
                &ProjectKey::new("FM").unwrap(),
                "Fieldnotes Mobile",
            )
            .unwrap();
        engine
    }

    /// Attaches an engine project to the tracker as the owner worker's turn-on does.
    /// Inputs: the project, the key and the name. Output: the tracked project, or the
    /// engine's code.
    pub(in crate::tui) fn attach(
        &mut self,
        project: &ProjectId,
        key: &ProjectKey,
        name: &str,
    ) -> Result<TrackedProject, String> {
        let tx = self.tx();
        crate::tracker::attach(&tx, project, key, name, NOW).map_err(|error| error.0)?;
        tx.commit().unwrap();
        Ok(TrackedProject {
            project: project.clone(),
            key: key.clone(),
            name: name.to_owned(),
        })
    }

    /// The projects the tracker holds, as the owner worker lists them. Output: Fieldnotes
    /// Mobile with its key once attached, else nothing.
    pub(in crate::tui) fn tracked(&self) -> Vec<TrackedProject> {
        let tx = self.tx();
        crate::tracker::key_of(&tx, &fieldnotes())
            .unwrap()
            .map(|key| TrackedProject {
                project: fieldnotes(),
                key,
                name: "Fieldnotes Mobile".to_owned(),
            })
            .into_iter()
            .collect()
    }

    /// What a ticket settings screen reads for one scope, from the values the engine
    /// stores and the core's options for every key. Input: the project, None for the app.
    /// Output: the view.
    pub(in crate::tui) fn settings_view(&self, project: Option<&ProjectId>) -> TicketSettingsView {
        use crate::tracker::settings::{CiEvidence, ManualEvidence, Scope, TicketSettings, stored};
        let tx = self.tx();
        let app = stored(&tx, &Scope::App).unwrap();
        let (own, below) = match project {
            None => (app, TicketSettings::default()),
            Some(project) => {
                let below = app.values.over(&TicketSettings::default());
                (
                    stored(&tx, &Scope::Project(project.clone())).unwrap(),
                    below,
                )
            }
        };
        let mut choices: Vec<SettingChoices> =
            trackwright_core::policy::describe(&tx, &ProjectKey::new("FM").unwrap())
                .unwrap()
                .into_iter()
                .map(|setting| SettingChoices {
                    key: setting.key.as_str().to_owned(),
                    options: setting.options,
                    proposed: setting.default,
                    custom: setting.custom.map(|range| CustomBounds {
                        min: range.min,
                        max: range.max,
                    }),
                })
                .collect();
        choices.push(SettingChoices {
            key: "manual_evidence".to_owned(),
            options: ManualEvidence::ALL
                .iter()
                .map(|option| json!(option))
                .collect(),
            proposed: json!(ManualEvidence::default()),
            custom: None,
        });
        choices.push(SettingChoices {
            key: "ci_evidence".to_owned(),
            options: CiEvidence::ALL.iter().map(|option| json!(option)).collect(),
            proposed: json!(CiEvidence::default()),
            custom: None,
        });
        TicketSettingsView {
            project: project.cloned(),
            values: own.values,
            version: own.version,
            below,
            effective: own.values.over(&below),
            choices,
        }
    }

    /// Saves one scope's ticket settings as the owner worker's save does. Inputs: the
    /// project (None for the app), the values and the version the screen read. Output:
    /// saved at the new version, or stale with the current one.
    pub(in crate::tui) fn save_settings(
        &mut self,
        project: Option<&ProjectId>,
        values: &TicketOverride,
        version: u64,
    ) -> SettingsSaved {
        use crate::tracker::settings::{Scope, set};
        let scope = match project {
            None => Scope::App,
            Some(project) => Scope::Project(project.clone()),
        };
        let tx = self.tx();
        match set(&tx, &scope, values, version, NOW).unwrap() {
            Ok(version) => {
                tx.commit().unwrap();
                SettingsSaved::Saved { version }
            }
            Err(stale) => SettingsSaved::Stale {
                current: stale.current,
            },
        }
    }

    /// The engine with five tickets in four statuses, created and moved by the owner.
    /// Output: the engine; FM-1 is in progress, FM-2 and FM-5 are todo, FM-3 in review
    /// and FM-4 in the backlog.
    pub(in crate::tui) fn seeded() -> Self {
        let mut engine = Self::new();
        for (title, kind, priority) in [
            ("Sync notes when the signal returns", "feature", 1),
            ("Offline map tiles for survey sites", "feature", 2),
            ("Photo captions lose their accents", "bug", 0),
            ("Export each site as one CSV file", "feature", 3),
            ("Species list loads before the notebook opens", "chore", 2),
        ] {
            engine.ok(
                OpKind::Create,
                json!({
                    "project": "FM",
                    "title": title,
                    "body": "",
                    "kind": kind,
                    "priority": priority,
                }),
            );
        }
        for (id, status) in [
            ("FM-1", "in_progress"),
            ("FM-2", "todo"),
            ("FM-3", "in_review"),
            ("FM-4", "backlog"),
            ("FM-5", "todo"),
        ] {
            let version = engine.version(id);
            engine.ok(
                OpKind::Move,
                json!({ "id": id, "status": status, "version": version }),
            );
        }
        engine
    }

    fn tx(&self) -> Transaction<'_> {
        Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap()
    }

    /// Runs one op and asserts the core accepted it. Inputs: the op and its input.
    /// Output: the reply's data.
    pub(in crate::tui) fn ok(&mut self, op: OpKind, input: Value) -> Value {
        let reply = self.call(op, input.clone());
        assert_eq!(reply["ok"], json!(true), "{} {input}: {reply}", op.as_str());
        reply["data"].clone()
    }

    /// The version a ticket is at now. Input: the ticket id. Output: the version.
    pub(in crate::tui) fn version(&mut self, id: &str) -> i64 {
        self.ok(OpKind::Show, json!({ "id": id }))["ticket"]["version"]
            .as_i64()
            .unwrap()
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

/// Answers one tracker job the way the owner worker does, against the engine: views jobs
/// run their steps through the engine as the tracker client. Inputs: the job and the
/// engine. Output: the Settings event the screens take.
pub(in crate::tui) fn answer(job: TrackerJob, engine: &mut Engine) -> SettingsEvent {
    let event = match job {
        TrackerJob::Projects => TrackerEvent::Projects(Ok(engine.tracked())),
        TrackerJob::Attach { project, key, name } => TrackerEvent::Attached {
            result: engine.attach(&project, &key, &name),
            project,
        },
        TrackerJob::Views {
            project,
            views,
            steps,
        } => {
            run(&views, &steps, engine);
            TrackerEvent::Views {
                project,
                problem: None,
            }
        }
        TrackerJob::Settings { project } => TrackerEvent::Settings {
            result: Ok(engine.settings_view(project.as_ref())),
            project,
        },
        TrackerJob::SaveSettings {
            project,
            values,
            version,
        } => TrackerEvent::Saved {
            result: Ok(engine.save_settings(project.as_ref(), &values, version)),
            project,
        },
    };
    SettingsEvent::Tracker(Box::new(event))
}

/// The text of the views drawn into an area, one string per row with trailing spaces
/// dropped. Inputs: the views, the width and the height. Output: the rows.
pub(in crate::tui) fn drawn(views: &Views, width: u16, height: u16) -> Vec<String> {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    views.render(area, &mut buffer, &trackwright_cli::views::Theme::plain());
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn run_takes_the_steps_in_order_through_the_client() {
    let mut engine = Engine::seeded();
    let views = Mutex::new(Views::new(ProjectKey::new("FM").unwrap()));
    let mut recorder = Recorder {
        engine: &mut engine,
        sent: Vec::new(),
    };
    run(
        &views,
        &[Step::Open(Page::List), Step::Key(ViewKey::Down)],
        &mut recorder,
    );
    assert_eq!(
        recorder
            .sent
            .iter()
            .map(|(op, input)| (*op, input.clone()))
            .collect::<Vec<_>>(),
        vec![(OpKind::List, json!({ "project": "FM" }))]
    );
    let rows = drawn(&views.lock().unwrap(), 76, 12);
    assert!(rows[0].starts_with(" FM  Tickets"), "{rows:?}");
    assert!(rows[4].starts_with(" > FM-2"), "{rows:?}");
}

#[test]
fn a_step_list_longer_than_one_job_is_refused() {
    let mut engine = Engine::new();
    let views = Mutex::new(Views::new(ProjectKey::new("FM").unwrap()));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_lost_connection_a_views_job_calls_the_engine_no_more() {
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::OwnerClientKind;
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::Builder::new()
        .prefix("ca-tui-tk-")
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
        let mut tracker = OwnerTracker {
            client,
            handle,
            failure: None,
        };
        let answered = tracker.call(OpKind::Schema, json!({}));
        tracker.failure = Some(EngineError("tracker_unavailable".into()));
        let after_refused = tracker.call(OpKind::Schema, json!({}));
        tracker.failure = Some(EngineError("tracker_outcome_unknown".into()));
        let after_lost = tracker.call(OpKind::Schema, json!({}));
        (
            answered,
            after_refused,
            after_lost,
            tracker.failure.map(|error| error.0),
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
