use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How many times a detach the system refuses as busy is tried, and the pause between
/// tries: an eject can race the teardown of the APFS container under load.
const DETACH_ATTEMPTS: usize = 40;
const DETACH_PAUSE: Duration = Duration::from_millis(250);

/// A scratch APFS disk image attached without Finder at its own mount point, detached
/// when dropped, so a test can give its volume another device number as a restart does.
struct Volume {
    image: PathBuf,
    mount: PathBuf,
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
        }
    }

    fn attach(&self) {
        std::fs::create_dir_all(&self.mount).unwrap();
        if let Err(refused) = hdiutil(
            &["attach", "-quiet", "-nobrowse", "-mountpoint"],
            &[&self.mount, &self.image],
        ) {
            panic!("hdiutil attach {}: {refused}", self.image.display());
        }
    }

    fn detach(&self) {
        let mut attached = self.disk();
        assert!(
            attached.is_some(),
            "hdiutil info lists {} as attached",
            self.image.display()
        );
        let mut refused = Vec::new();
        while let Some(disk) = attached {
            match hdiutil(&["detach", "-quiet"], &[Path::new(&disk)]) {
                Ok(_) => return,
                Err(reason) => refused.push(format!("{disk}: {reason}")),
            }
            assert!(
                refused.len() < DETACH_ATTEMPTS,
                "hdiutil detach {}: {refused:?}",
                self.mount.display()
            );
            std::thread::sleep(DETACH_PAUSE);
            attached = self.disk();
        }
    }

    /// The whole disk hdiutil attached this volume's image as, found by the image, so a
    /// detach that unmounted the volume but could not eject the image can still finish.
    /// Output: the `/dev/diskN` path, or none when the image is not attached.
    fn disk(&self) -> Option<String> {
        const DISK: &str = "/dev/disk";
        let info = hdiutil(&["info", "-plist"], &[]).ok()?;
        let image = format!("<string>{}</string>", self.image.display());
        let entities = &info[info.find(&image)? + image.len()..];
        let number: String = entities[entities.find(DISK)? + DISK.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!number.is_empty()).then(|| format!("{DISK}{number}"))
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        if let Some(disk) = self.disk() {
            let _ = hdiutil(&["detach", "-quiet", "-force"], &[Path::new(&disk)]);
        }
    }
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
