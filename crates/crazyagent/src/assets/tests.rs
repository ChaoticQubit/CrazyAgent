use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// How many times a detach the system refuses as busy is tried, and the pause between
/// tries: an eject can race the teardown of the APFS container under load.
const DETACH_ATTEMPTS: usize = 40;
const DETACH_PAUSE: Duration = Duration::from_millis(250);

/// Keeps the tests of this file from attaching and detaching images at the same time, so
/// a device number one test frees is never the one another test expects to change.
static VOLUMES: Mutex<()> = Mutex::new(());

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
    let output = crate::spawn_gate::output(
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

fn admitted(db: &Connection, id: &str) -> AdmittedSource {
    let body: String = db
        .query_row("SELECT body FROM asset_sources WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&body).unwrap()
}

#[test]
fn an_admitted_asset_is_read_after_its_volume_is_renumbered_and_a_replaced_one_is_refused() {
    let _volumes = VOLUMES.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::Builder::new()
        .prefix("ca-assets-dev-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = root.path().canonicalize().unwrap();
    let volume = Volume::new(&base, "assets");
    let spacer = Volume::new(&base, "spacer");
    volume.attach();
    let file = volume.mount.join("rules.md");
    std::fs::write(&file, "Use plain words.").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    let mut db = Connection::open(base.join("engine.db")).unwrap();
    initialize(&db).unwrap();
    let config = crate::config::resolve_config(&base.join("g"), &base.join("p")).unwrap();
    let source = AssetSource {
        id: RequestId::new("rules").unwrap(),
        name: "rules".into(),
        scope: AssetScope::Global,
        kind: AssetKind::Rules,
        path: file.clone(),
    };
    let tx = db.transaction().unwrap();
    assert_eq!(
        execute(&tx, &config, &AssetAction::Admit { source }).unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let pinned = admitted(&db, "rules");
    let maximum = config.settings.knowledge.asset_bytes_max;
    assert_eq!(
        content(&pinned, maximum).unwrap().bytes,
        b"Use plain words."
    );

    volume.detach();
    spacer.attach();
    volume.attach();
    let live = std::fs::metadata(&file).unwrap();
    assert_ne!(
        live.dev(),
        pinned.device,
        "the reattached volume has another device number"
    );
    assert_eq!(live.ino(), pinned.inode);
    let read = content(&pinned, maximum).unwrap();
    assert_eq!(read.bytes, b"Use plain words.");
    assert_eq!(read.content_hash, hash(b"Use plain words."));

    let other = volume.mount.join("other.md");
    std::fs::write(&other, "Use plain words.").unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&other, &file).unwrap();
    assert_ne!(std::fs::metadata(&file).unwrap().ino(), pinned.inode);
    assert_eq!(
        content(&pinned, maximum).unwrap_err().0,
        "asset_source_replaced"
    );
}

#[test]
fn an_image_entry_of_hdiutil_info_holds_its_own_disk_and_never_the_next_images() {
    let _volumes = VOLUMES.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::Builder::new()
        .prefix("ca-assets-dev-")
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
