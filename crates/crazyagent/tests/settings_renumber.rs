#![cfg(target_os = "macos")]

use crazyagent::config::{ModelAccessEdit, resolve_config};
use crazyagent::engine::Engine;
use crazyagent::engine::owner::*;
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use serde_json::Value;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;

const JOURNAL_ABORT: &str = "CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'actual_journal_abort'); END;";
const LEDGER_ABORT: &str = "CREATE TRIGGER publication_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT, 'actual_ledger_abort'); END;";
const DROP_ABORT: &str = "DROP TRIGGER publication_abort";

/// How many times a detach the system refuses as busy is tried, and the pause between
/// tries: an eject can race the teardown of the APFS container under load.
const DETACH_ATTEMPTS: usize = 40;
const DETACH_PAUSE: Duration = Duration::from_millis(250);

/// Keeps the tests of this file from attaching and detaching images at the same time, so
/// a device number one test frees is never the one another test expects to change.
static VOLUMES: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A scratch APFS disk image attached without Finder at its own mount point, detached
/// when dropped, so a test can give its volume another device number as a restart does.
/// It holds the whole disk its last attach returned and only ever detaches that disk.
struct Volume {
    image: PathBuf,
    mount: PathBuf,
    disk: Mutex<Option<String>>,
}

impl Volume {
    fn new(base: &Path, name: &str) -> Self {
        let image = base.join(format!("{name}.dmg"));
        if let Err(refused) = hdiutil(
            &[
                "create", "-quiet", "-size", "8m", "-fs", "APFS", "-volname", name,
            ],
            &[&image],
        ) {
            panic!("hdiutil create {name}: {refused}");
        }
        Self {
            image,
            mount: base.join(name),
            disk: Mutex::new(None),
        }
    }

    fn attach(&self) {
        std::fs::create_dir_all(&self.mount).unwrap();
        let attached = hdiutil(
            &["attach", "-plist", "-nobrowse", "-mountpoint"],
            &[&self.mount, &self.image],
        )
        .unwrap_or_else(|refused| panic!("hdiutil attach {}: {refused}", self.image.display()));
        let disk = first_disk(&attached)
            .unwrap_or_else(|| panic!("hdiutil attach {} names a disk", self.image.display()));
        let previous = self.disk.lock().unwrap().replace(disk);
        assert!(
            previous.is_none(),
            "{} is attached once",
            self.image.display()
        );
    }

    fn detach(&self) {
        let disk = self.disk.lock().unwrap().clone();
        let disk = disk.unwrap_or_else(|| panic!("{} is attached", self.image.display()));
        if let Err(refused) = self.release(&disk, false) {
            panic!(
                "hdiutil detach {disk} for {}: {refused:?}",
                self.mount.display()
            );
        }
        *self.disk.lock().unwrap() = None;
    }

    fn device(&self) -> u64 {
        std::fs::metadata(&self.mount).unwrap().dev()
    }

    /// Detaches the disk this volume's image was attached as, while hdiutil still lists
    /// that disk under this image, and tries again after a refusal. Input: the disk and
    /// whether to force it. Output: nothing once hdiutil no longer lists the image, or
    /// every reason it gave, including a failed `hdiutil info`.
    fn release(&self, disk: &str, force: bool) -> Result<(), Vec<String>> {
        let detach: &[&str] = if force {
            &["detach", "-quiet", "-force"]
        } else {
            &["detach", "-quiet"]
        };
        let listed = format!("<string>{disk}</string>");
        let mut refused = Vec::new();
        for _ in 0..DETACH_ATTEMPTS {
            let info = hdiutil(&["info", "-plist"], &[])
                .map_err(|reason| vec![format!("hdiutil info: {reason}")])?;
            let Some(entry) = image_entry(&info, &self.image) else {
                return Ok(());
            };
            if !entry.contains(&listed) {
                refused.push(format!("{} is listed without {disk}", self.image.display()));
            } else if let Err(reason) = hdiutil(detach, &[Path::new(disk)]) {
                refused.push(format!("{disk}: {reason}"));
            } else {
                continue;
            }
            std::thread::sleep(DETACH_PAUSE);
        }
        Err(refused)
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let held = self
            .disk
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(disk) = held
            && let Err(refused) = self.release(&disk, true)
        {
            eprintln!(
                "hdiutil detach -force {disk} for {}: {refused:?}",
                self.image.display()
            );
        }
    }
}

/// The part of `hdiutil info -plist` that describes one image, from its path to the next
/// image's path, so a lookup never reads another image's disks. Input: the plist text and
/// the image. Output: that part, or none when hdiutil does not list the image.
fn image_entry<'a>(info: &'a str, image: &Path) -> Option<&'a str> {
    let path = format!("<string>{}</string>", image.display());
    info.split("<key>image-path</key>")
        .skip(1)
        .find_map(|entry| entry.trim_start().strip_prefix(path.as_str()))
}

/// The first disk `hdiutil attach -plist` lists, which is the image's own whole disk.
/// Input: the attach output. Output: its `/dev/diskN` path, or none if it lists no disk.
fn first_disk(attached: &str) -> Option<String> {
    const DISK: &str = "/dev/disk";
    let number: String = attached[attached.find(DISK)? + DISK.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!number.is_empty()).then(|| format!("{DISK}{number}"))
}

/// Runs hdiutil. Input: its arguments, then the paths it acts on. Output: what it printed
/// when it succeeded, or why it did not start or what it reported when it failed.
fn hdiutil(arguments: &[&str], paths: &[&Path]) -> Result<String, String> {
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/hdiutil")
            .args(arguments)
            .args(paths)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn private_folder(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// The engine folder on the boot volume and the settings files on one or two scratch
/// volumes, with spacer images that take the freed device numbers on each renumber.
struct Rig {
    settings: Volume,
    project_volume: Option<Volume>,
    spacers: Vec<Volume>,
    global: PathBuf,
    project: PathBuf,
    base: PathBuf,
    _root: tempfile::TempDir,
}

impl Rig {
    fn new(project_apart: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-renum-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        private_file(&base.join("engine.db"), b"");
        let settings = Volume::new(&base, "settings");
        settings.attach();
        private_folder(&settings.mount.join("s"));
        let global = settings.mount.join("s").join("global.toml");
        private_file(&global, b"version = 1\n");
        let project_volume = project_apart.then(|| {
            let volume = Volume::new(&base, "project");
            volume.attach();
            private_folder(&volume.mount.join("s"));
            volume
        });
        let project = project_volume
            .as_ref()
            .unwrap_or(&settings)
            .mount
            .join("s")
            .join("project.toml");
        private_file(&project, b"[routing]\npriority_weight = 3\n");
        let spacers = ["spacer-1", "spacer-2"]
            .into_iter()
            .map(|name| Volume::new(&base, name))
            .collect();
        Self {
            settings,
            project_volume,
            spacers,
            global,
            project,
            base,
            _root: root,
        }
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.base.join("engine.db")).unwrap()
    }

    fn staged(&self, request: &OwnerRequest) -> PathBuf {
        self.global.parent().unwrap().join(format!(
            ".crazyagent-publication-{}",
            request.mutation_hash().unwrap()
        ))
    }

    /// Detaches every settings volume and attaches them again behind the next spacer, as
    /// a restart that renumbers the volumes does. Returns the settings volume's new device.
    fn renumber(&self, round: usize) -> u64 {
        let before = self.settings.device();
        let volumes: Vec<&Volume> = std::iter::once(&self.settings)
            .chain(self.project_volume.as_ref())
            .collect();
        for volume in &volumes {
            volume.detach();
        }
        self.spacers[round].attach();
        for volume in &volumes {
            volume.attach();
        }
        let after = self.settings.device();
        assert_ne!(after, before, "the volume came back under another device");
        after
    }

    async fn open(&self) -> Result<Engine, String> {
        Engine::open_with_settings_sources(
            &self.base.join("engine.db"),
            &self.global,
            &self.project,
        )
        .await
        .map_err(|problem| problem.0)
    }

    async fn owner(&self, engine: Engine) -> Owner {
        let engine = Arc::new(engine);
        let config = resolve_config(&self.global, &self.project).unwrap();
        let (server, host) = OwnerServer::bind(&self.base, engine.clone(), &config)
            .await
            .unwrap();
        let (stop, rx) = oneshot::channel();
        let task = tokio::spawn(server.run(rx));
        let client =
            OwnerClient::attach(&self.base, OwnerClientKind::Tui, Duration::from_secs(2)).unwrap();
        Owner {
            engine,
            client,
            host,
            stop,
            task,
        }
    }
}

struct Owner {
    engine: Arc<Engine>,
    client: OwnerClient,
    host: OwnerRuntimeHandle,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), crazyagent::engine::EngineError>>,
}

impl Owner {
    async fn close(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap().unwrap();
        drop(self.host);
        self.engine.close().await.unwrap();
    }

    async fn request(&self, id: &str) -> OwnerRequest {
        let reply = self
            .client
            .request(owner_request(
                "read",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::ModelAccessPage {
                        collection: ModelAccessCollection::Accounts,
                        cursor: None,
                        limit: 16,
                    },
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("authoritative owner read required: {:?}", reply.response)
        };
        let OwnerData::ModelAccessPage { source_hash, .. } = &state.data else {
            panic!("catalog page required")
        };
        owner_request(
            id,
            state.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id.clone(),
                    expected_source_hash: source_hash.clone(),
                    edit: ModelAccessEdit::AddAccount {
                        account_ref: CredentialRef::new("renumbered-account").unwrap(),
                    },
                },
            },
        )
    }

    async fn interrupted(&self, id: &str, db: &rusqlite::Connection) -> OwnerRequest {
        let request = self.request(id).await;
        db.execute_batch(JOURNAL_ABORT).unwrap();
        assert!(matches!(
            self.client.request(request.clone()).await.unwrap().response,
            OwnerResponse::OutcomeUnknown { .. }
        ));
        db.execute_batch(DROP_ABORT).unwrap();
        request
    }

    async fn replayed(&self, request: OwnerRequest) {
        let replay = self.client.request(request).await.unwrap();
        assert!(
            matches!(replay.response, OwnerResponse::Applied { ref operation } if operation.historical_replay && operation.current_revision.value() == 1 && operation.result == OwnerResult::Applied),
            "{:?}",
            replay.response
        );
    }
}

fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn journal(db: &rusqlite::Connection) -> Value {
    let body: Vec<u8> = db
        .query_row(
            "SELECT body FROM settings_publication WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn rewrite_journal(db: &rusqlite::Connection, body: &Value) {
    db.execute(
        "UPDATE settings_publication SET body=?1 WHERE singleton=1",
        [serde_json::to_vec(body).unwrap()],
    )
    .unwrap();
}

/// The devices of every file the journal pins: the old global and project files and,
/// once staged, the new global file.
fn journal_devices(body: &Value) -> Vec<u64> {
    [
        &body["old"]["global"]["identity"],
        &body["old"]["project"]["identity"],
        &body["new_identity"],
    ]
    .into_iter()
    .filter_map(|pin| pin[0].as_u64())
    .collect()
}

fn count(db: &rusqlite::Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn published(rig: &Rig) -> bool {
    std::fs::read_to_string(&rig.global)
        .unwrap()
        .contains("renumbered-account")
}

#[tokio::test]
async fn a_publication_interrupted_twice_resumes_on_its_renumbered_volume_and_its_journal_keeps_one_device()
 {
    let _volumes = VOLUMES.lock().await;
    let rig = Rig::new(false);
    let db = rig.database();
    let owner = rig.owner(rig.open().await.unwrap()).await;
    let request = owner.interrupted("renumbered-twice", &db).await;
    owner.close().await;
    let first = rig.settings.device();
    let body = journal(&db);
    assert!(body["new_identity"].is_null(), "{body}");
    assert_eq!(journal_devices(&body), [first, first]);
    assert!(rig.staged(&request).is_file());
    assert!(!published(&rig));

    let second = rig.renumber(0);
    db.execute_batch(LEDGER_ABORT).unwrap();
    let stopped = rig.open().await.err().expect("the ledger write is aborted");
    assert!(stopped.contains("actual_ledger_abort"), "{stopped}");
    db.execute_batch(DROP_ABORT).unwrap();
    assert!(
        published(&rig),
        "the resume staged and renamed the change on the renumbered volume"
    );
    assert!(!rig.staged(&request).exists());
    let body = journal(&db);
    assert_eq!(
        journal_devices(&body),
        [first, first, first],
        "the file staged on device {second} is journaled under the journal's own device"
    );
    assert_eq!(
        body["new_identity"][1].as_u64(),
        Some(std::fs::metadata(&rig.global).unwrap().ino())
    );
    assert_eq!(count(&db, "owner_operations"), 0);

    let third = rig.renumber(1);
    assert_ne!(third, first);
    let owner = rig.owner(rig.open().await.unwrap()).await;
    assert_eq!(count(&db, "settings_publication"), 0);
    assert_eq!(count(&db, "owner_operations"), 1);
    owner.replayed(request).await;
    owner.close().await;
    assert!(published(&rig));
}

#[tokio::test]
async fn a_staged_file_journaled_before_a_restart_resumes_on_a_renumbered_volume_unless_it_was_replaced()
 {
    let _volumes = VOLUMES.lock().await;
    for replaced in [false, true] {
        let rig = Rig::new(false);
        let db = rig.database();
        let owner = rig.owner(rig.open().await.unwrap()).await;
        let request = owner.interrupted("staged-then-stopped", &db).await;
        owner.close().await;
        let staged = rig.staged(&request);
        let pin = std::fs::metadata(&staged).unwrap();
        let mut body = journal(&db);
        body["new_identity"] = serde_json::json!([pin.dev(), pin.ino()]);
        rewrite_journal(&db, &body);

        rig.renumber(0);
        if replaced {
            let bytes = std::fs::read(&staged).unwrap();
            std::fs::remove_file(&staged).unwrap();
            private_file(&staged, &bytes);
            assert_ne!(std::fs::metadata(&staged).unwrap().ino(), pin.ino());
            assert_eq!(
                rig.open().await.err().as_deref(),
                Some("settings_temporary_conflict")
            );
            assert!(
                !published(&rig),
                "a replaced staged file is never published"
            );
            assert_eq!(count(&db, "settings_publication"), 1);
            continue;
        }
        let owner = rig.owner(rig.open().await.unwrap()).await;
        assert!(published(&rig));
        assert_eq!(
            std::fs::metadata(&rig.global).unwrap().ino(),
            pin.ino(),
            "the journaled staged file is the one renamed into place"
        );
        assert_eq!(count(&db, "settings_publication"), 0);
        owner.replayed(request).await;
        owner.close().await;
    }
}

#[tokio::test]
async fn settings_split_across_two_renumbered_volumes_are_not_taken_for_the_journaled_ones() {
    let _volumes = VOLUMES.lock().await;
    let rig = Rig::new(true);
    let db = rig.database();
    let owner = rig.owner(rig.open().await.unwrap()).await;
    let request = owner.interrupted("split-volumes", &db).await;
    owner.close().await;
    let body = journal(&db);
    let devices = journal_devices(&body);
    assert_eq!(devices.len(), 2);
    assert_ne!(devices[0], devices[1], "the two files sit on two volumes");

    rig.renumber(0);
    assert_eq!(
        rig.open().await.err().as_deref(),
        Some("settings_source_conflict")
    );
    assert!(!published(&rig));
    assert_eq!(count(&db, "settings_publication"), 1);
    assert!(rig.staged(&request).is_file());
}

#[tokio::test]
async fn an_image_entry_of_hdiutil_info_holds_its_own_disk_and_never_the_next_images() {
    let _volumes = VOLUMES.lock().await;
    let root = tempfile::Builder::new()
        .prefix("ca-renum-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = root.path().canonicalize().unwrap();
    let volumes = [Volume::new(&base, "first"), Volume::new(&base, "second")];
    for volume in &volumes {
        volume.attach();
    }
    let info = hdiutil(&["info", "-plist"], &[]).unwrap();
    let path = |volume: &Volume| format!("<string>{}</string>", volume.image.display());
    let disk = |volume: &Volume| {
        let disk = volume.disk.lock().unwrap().clone().unwrap();
        format!("<string>{disk}</string>")
    };
    let [earlier, later] = if info.find(&path(&volumes[0])) < info.find(&path(&volumes[1])) {
        [&volumes[0], &volumes[1]]
    } else {
        [&volumes[1], &volumes[0]]
    };
    for (volume, other) in [(earlier, later), (later, earlier)] {
        let entry = image_entry(&info, &volume.image).unwrap();
        assert!(entry.contains(&disk(volume)), "{entry}");
        assert!(!entry.contains(&disk(other)), "{entry}");
    }

    let at = info.find(&path(earlier)).unwrap();
    let entities = at + info[at..].find("<key>system-entities</key>").unwrap();
    let close = entities + info[entities..].find("</array>").unwrap() + "</array>".len();
    let bare = format!("{}{}", &info[..entities], &info[close..]);
    let entry = image_entry(&bare, &earlier.image).unwrap();
    assert!(!entry.contains("/dev/disk"), "{entry}");
    assert!(
        image_entry(&bare, &later.image)
            .unwrap()
            .contains(&disk(later))
    );
}
