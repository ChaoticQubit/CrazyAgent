use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

/// A scratch APFS disk image attached without Finder at its own mount point, detached
/// when dropped, so a test can give its volume another device number as a restart does.
struct Volume {
    image: PathBuf,
    mount: PathBuf,
}

impl Volume {
    fn new(base: &Path, name: &str) -> Self {
        let image = base.join(format!("{name}.dmg"));
        let created = hdiutil(
            &[
                "create", "-quiet", "-size", "8m", "-fs", "APFS", "-volname", name,
            ],
            &[&image],
        );
        assert!(created, "hdiutil create {name}");
        Self {
            image,
            mount: base.join(name),
        }
    }

    fn attach(&self) {
        std::fs::create_dir_all(&self.mount).unwrap();
        let attached = hdiutil(
            &["attach", "-quiet", "-nobrowse", "-mountpoint"],
            &[&self.mount, &self.image],
        );
        assert!(attached, "hdiutil attach {}", self.image.display());
    }

    fn detach(&self) {
        assert!(
            hdiutil(&["detach", "-quiet"], &[&self.mount]),
            "hdiutil detach {}",
            self.mount.display()
        );
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = hdiutil(&["detach", "-quiet", "-force"], &[&self.mount]);
    }
}

fn hdiutil(arguments: &[&str], paths: &[&Path]) -> bool {
    crate::spawn_gate::status(
        Command::new("/usr/bin/hdiutil")
            .args(arguments)
            .args(paths)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
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
