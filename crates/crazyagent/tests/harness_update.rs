#![cfg(unix)]

use crazyagent::harnesses::update::{self, Fixed, Roots, Updates, VERSION_TIME};
use crazyagent::harnesses::{HarnessEntry, ProbeCache, Search};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const WAIT_STEPS: usize = 400;
const WAIT_STEP: Duration = Duration::from_millis(25);

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crazyagent::spawn_gate::guarded(|| std::fs::write(path, text)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

fn database() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    update::initialize(&db).unwrap();
    db
}

fn entry_of(probe: &Arc<ProbeCache>, search: Search, check: bool, id: &str) -> HarnessEntry {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let entries = runtime.block_on(probe.entries(search, check)).unwrap();
    entries.into_iter().find(|entry| entry.id == id).unwrap()
}

fn wait_idle(updates: &Updates) {
    let idle = (0..WAIT_STEPS).any(|_| {
        std::thread::sleep(WAIT_STEP);
        updates.idle()
    });
    assert!(idle);
}

fn version_at(executable: &Path, home: &Path) -> String {
    let fixed = Fixed {
        program: executable.to_path_buf(),
        args: vec!["--version"],
        search: vec![executable.parent().unwrap().to_path_buf()],
        prefix: None,
    };
    let ran = update::run(&fixed, home, VERSION_TIME).unwrap();
    assert!(ran.success);
    update::installed_version(&ran.output).unwrap()
}

struct FakeNative {
    root: tempfile::TempDir,
}

impl FakeNative {
    fn install(binary: &str, version: &str) -> Self {
        let machine = Self { root: temp() };
        let versions = machine.home().join(".local/share/claude/versions");
        let launcher = machine.launcher();
        assert_eq!(launcher.file_name().unwrap(), binary);
        let swap = format!(
            "ln -sfn {} {}",
            versions.join("B").join(binary).display(),
            launcher.display()
        );
        executable(
            &versions.join("A").join(binary),
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n--version) echo '{version} (Claude Code)';;\nupdate) {swap};;\nesac\n"
            ),
        );
        executable(
            &versions.join("B").join(binary),
            "#!/bin/sh\necho '2.0.0 (Claude Code)'\n",
        );
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(versions.join("A").join(binary), &launcher).unwrap();
        machine
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn launcher(&self) -> PathBuf {
        self.home().join(".local/bin/claude")
    }

    fn roots(&self) -> Roots {
        Roots {
            home: self.home(),
            brew_prefixes: Vec::new(),
            casks: true,
            npm: None,
            places: crazyagent::tools::Places::new(Vec::new(), Some(self.home())),
        }
    }

    fn path_value(&self) -> std::ffi::OsString {
        std::env::join_paths([self.launcher().parent().unwrap()]).unwrap()
    }

    fn engine_entry(&self) -> PathBuf {
        crazyagent::paths::resolve_harnesses(
            crazyagent::paths::Harnesses::default(),
            &self.search(),
            Some(&self.path_value()),
            None,
        )
        .claude
        .unwrap()
    }

    fn search(&self) -> Search {
        let folder = self.launcher().parent().unwrap().to_path_buf();
        Search::new(vec![folder], None, Vec::new())
    }

    fn catalog_entry(&self, probe: &Arc<ProbeCache>, check: bool) -> HarnessEntry {
        entry_of(probe, self.search(), check, "claude-code")
    }

    fn real_of(&self, entry: &HarnessEntry) -> PathBuf {
        std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap()
    }

    fn run_update_through_the_engine(&self) {
        let updates = Updates::new();
        let db = database();
        let entry = self.catalog_entry(&ProbeCache::new(Duration::ZERO), true);
        updates.start(&db, &entry, &self.roots(), |_| {}).unwrap();
        wait_idle(&updates);
        let last = updates
            .versions(&db, &entry, &self.roots(), false)
            .last
            .unwrap();
        assert!(last.ok, "{last:?}");
        assert_eq!(last.to.as_deref(), Some("2.0.0"));
    }
}

#[test]
fn an_updated_harness_is_launched_from_its_new_install_without_an_engine_restart() {
    let machine = FakeNative::install("claude", "1.0.0");
    let path = machine.path_value();
    let entry = machine.engine_entry();
    assert_eq!(entry, machine.launcher());
    let first = crazyagent::paths::launch_executable(&entry, Some(&path)).unwrap();
    assert!(first.ends_with("versions/A/claude"));
    machine.run_update_through_the_engine();
    assert_eq!(machine.engine_entry(), entry);
    let second = crazyagent::paths::launch_executable(&entry, Some(&path)).unwrap();
    assert_ne!(first, second);
    assert!(second.ends_with("versions/B/claude"));
    assert_eq!(version_at(&second, &machine.home()), "2.0.0");
}

#[test]
fn the_catalog_shows_the_new_real_path_right_after_an_update() {
    let machine = FakeNative::install("claude", "1.0.0");
    let probe = ProbeCache::new(Duration::from_secs(3600));
    let before = machine.catalog_entry(&probe, false);
    let old = machine.real_of(&before);
    machine.run_update_through_the_engine();
    let after = machine.catalog_entry(&probe, true);
    assert_eq!(before.executable, after.executable);
    assert_ne!(old, machine.real_of(&after));
    assert_eq!(
        version_at(after.executable.as_ref().unwrap(), &machine.home()),
        "2.0.0"
    );
}

struct FakeBrew {
    root: tempfile::TempDir,
}

impl FakeBrew {
    fn with_formula(binary: &str, version: &str) -> Self {
        let machine = Self { root: temp() };
        std::fs::create_dir_all(machine.home()).unwrap();
        let target = machine.prefix().join("bin").join(binary);
        executable(
            &machine.prefix().join("bin/brew"),
            &format!(
                "#!/bin/sh\n[ \"$1 $2 $3\" = 'install --formula {binary}' ] || exit 9\nprintf '#!/bin/sh\\necho \"{binary} {version}\"\\n' > {0}\nchmod 755 {0}\n",
                target.display()
            ),
        );
        machine
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn prefix(&self) -> PathBuf {
        self.root.path().join("brew")
    }

    fn roots(&self) -> Roots {
        Roots {
            home: self.home(),
            brew_prefixes: vec![self.prefix()],
            casks: true,
            npm: None,
            places: crazyagent::tools::Places::new(
                vec![self.prefix().join("bin")],
                Some(self.home()),
            ),
        }
    }

    fn search(&self) -> Search {
        Search::new(vec![self.prefix().join("bin")], None, Vec::new())
    }

    fn catalog_entry(&self, probe: &Arc<ProbeCache>, check: bool, id: &str) -> HarnessEntry {
        entry_of(probe, self.search(), check, id)
    }

    fn version_of(&self, executable: &Path) -> String {
        version_at(executable, &self.home())
    }

    fn run_install_through_the_engine(&self, id: &str) {
        let updates = Updates::new();
        let db = database();
        let entry = self.catalog_entry(&ProbeCache::new(Duration::ZERO), true, id);
        let (search, wanted) = (self.search(), id.to_owned());
        let after = move || {
            crazyagent::harnesses::catalog(&search)
                .into_iter()
                .find(|entry| entry.id == wanted)
                .and_then(|entry| entry.executable)
        };
        updates.install(&db, &entry, &self.roots(), after).unwrap();
        wait_idle(&updates);
        let last = updates
            .versions(&db, &entry, &self.roots(), false)
            .last
            .unwrap();
        assert!(last.ok, "{last:?}");
    }
}

#[test]
fn a_harness_installed_from_the_app_is_ready_in_the_catalog_without_an_engine_restart() {
    let machine = FakeBrew::with_formula("aider", "3.0.0");
    let probe = ProbeCache::new(Duration::from_secs(3600));
    assert!(
        machine
            .catalog_entry(&probe, false, "aider")
            .executable
            .is_none()
    );
    machine.run_install_through_the_engine("aider");
    let entry = machine.catalog_entry(&probe, true, "aider");
    assert_eq!(entry.executable, Some(machine.prefix().join("bin/aider")));
    assert_eq!(
        machine.version_of(entry.executable.as_ref().unwrap()),
        "3.0.0"
    );
}
