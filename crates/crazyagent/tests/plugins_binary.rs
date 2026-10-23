use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{AccountSecret, OwnerClientKind};
use crazyagent::plugins::{Scope, ServerSpec};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &str = include_str!("fixtures/mcp_fixture.py");
const SECRET: &[u8] = b"dummy-value";

struct Engine {
    child: Child,
}

impl Engine {
    fn start(root: &Path, run: &str) -> Self {
        let stdout = root.join(format!("{run}.stdout"));
        let child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg("engine.sock")
            .arg("--writable-settings")
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(root.join(format!("{run}.stderr"))).unwrap())
            .spawn()
            .unwrap();
        let mut engine = Self { child };
        let deadline = Instant::now() + Duration::from_secs(30);
        while std::fs::read(&stdout).unwrap() != b"ready\n" {
            assert!(
                engine.child.try_wait().unwrap().is_none(),
                "engine exited before ready"
            );
            assert!(Instant::now() < deadline, "engine readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        engine
    }

    fn stop(&mut self) {
        let terminated = Command::new("/bin/kill")
            .args(["-s", "TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(terminated.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "engine stop deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn files(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            found.extend(files(&path));
        } else if metadata.is_file() {
            found.push(path);
        }
    }
    found
}

async fn controller(root: &Path) -> OwnerController {
    OwnerController::attach(root, OwnerClientKind::Desktop, Duration::from_secs(20))
        .await
        .unwrap()
}

#[tokio::test]
async fn skills_and_approved_servers_survive_a_real_engine_binary_restart() {
    let root = tempfile::Builder::new()
        .prefix("ca-plug-bin-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let path = root.path().to_path_buf();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        path.join("global.toml"),
        "version = 1\ntimeout_ms = 5000\nallowed_destinations = []\n",
    )
    .unwrap();
    std::fs::set_permissions(
        path.join("global.toml"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let picked = path.join("picked/notes");
    std::fs::create_dir_all(&picked).unwrap();
    std::fs::write(
        picked.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Writes release notes.\n---\n",
    )
    .unwrap();
    let server_path = path.join("mcp_fixture.py");
    std::fs::write(&server_path, FIXTURE).unwrap();
    std::fs::set_permissions(&server_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let spec = ServerSpec::Stdio {
        command: server_path.display().to_string(),
        args: vec![],
    };

    let mut engine = Engine::start(&path, "first");
    let owner = controller(&path).await;
    let skill = owner
        .add_skill(Scope::Global, picked.display().to_string())
        .await
        .unwrap();
    let server = owner
        .add_mcp_server(
            Scope::Global,
            "fixture".into(),
            spec.clone(),
            vec![(
                "FIXTURE_TOKEN".into(),
                AccountSecret::new(SECRET.to_vec()).unwrap(),
            )],
        )
        .await
        .unwrap();
    assert_eq!(server.status_label, "Waiting for approval");
    owner
        .approve_mcp_server(server.id.clone(), spec)
        .await
        .unwrap();
    let checked = owner.check_mcp_server(server.id.clone()).await.unwrap();
    assert_eq!(checked.tools, vec!["echo", "authorized"]);
    drop(owner);
    engine.stop();

    let mut engine = Engine::start(&path, "second");
    let owner = controller(&path).await;
    let skills = owner.plugin_skills(None).await.unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].id, skill.id);
    assert!(PathBuf::from(&skills[0].folder).join("SKILL.md").is_file());
    let servers = owner.plugin_servers(None).await.unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].status_label, "Approved");
    assert_eq!(servers[0].tools, vec!["echo", "authorized"]);
    assert!(servers[0].secrets[0].saved);
    drop(owner);
    engine.stop();

    let holders: Vec<PathBuf> = files(&path)
        .into_iter()
        .filter(|file| {
            std::fs::read(file)
                .map(|bytes| bytes.windows(SECRET.len()).any(|window| window == SECRET))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(holders.len(), 1, "{holders:?}");
    assert!(holders[0].starts_with(path.join("plugins/secrets")));
    assert_eq!(
        std::fs::metadata(&holders[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
