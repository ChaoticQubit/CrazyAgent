use crate::client_controller::OwnerController;
use crate::config::{AccountPool, ProviderQuotaPolicy, resolve_config};
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerClientKind;
use crate::protocol::CredentialRef;
use crate::tui::client::Connection;
use crate::tui::ctx::{Ctx, Link};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::screens::{Place, SettingsSection, open};
use crate::tui::seam::{Request, ShellRequest};
use crate::tui::shell::Shell;
use crate::tui::test_home::{ENGINE_WAIT, projects_home};
use crate::tui::theme::{Appearance, Colors, Depth};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SETTINGS: &str = r#"version = 1
timeout_ms = 10000
allowed_destinations = []

[model_access]
accounts = ["local-1"]
connections = [
  { id = "local-a", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-1", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
"#;
const STRICT_LINE: &str = "Provider quota policy: strict | p switches";
const BOUNDED_LINE: &str = "Provider quota policy: bounded_attempt | p switches";
const SETTLE_TIME: Duration = Duration::from_secs(8);
const QUIET_ROUNDS: usize = 20;

struct Engine {
    root: tempfile::TempDir,
    _home: tempfile::TempDir,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Engine {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui-pool-strict-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, SETTINGS).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client = Client::new(root.path().join("e.sock"), ENGINE_WAIT).unwrap();
        let (_, mut controller) = OwnerController::attach_configured(client, OwnerClientKind::Tui)
            .await
            .unwrap();
        let home = projects_home(&mut controller, 1).await;
        controller.detach();
        Self {
            root,
            _home: home,
            _service: service,
            _stop: stop,
        }
    }

    fn global(&self) -> PathBuf {
        self.root.path().join("global.toml")
    }

    fn connect(&self) -> Connection {
        Connection::connect_paired(
            Client::new(self.root.path().join("e.sock"), ENGINE_WAIT).unwrap(),
        )
        .unwrap()
    }
}

fn saved_pools(global: &Path, project: &Path) -> Vec<AccountPool> {
    resolve_config(global, project)
        .unwrap()
        .settings
        .model_access
        .unwrap_or_default()
        .pools
}

fn render(shell: &mut Shell) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn pump(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + SETTLE_TIME;
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
            moved = true;
        }
        if !connection.busy()
            && shell.ctx.link == Link::Ready
            && let Some(request) = shell.ctx.next_request()
        {
            moved = true;
            match request {
                Request::Pools => {
                    sent.push("pools");
                    connection.shell(ShellRequest::Pools).unwrap();
                }
                Request::PoolProjects => {
                    sent.push("pool projects");
                    connection.shell(ShellRequest::PoolProjects).unwrap();
                }
                Request::Projects => {
                    sent.push("projects");
                    connection.shell(ShellRequest::Projects).unwrap();
                }
                Request::EditPool { change } => {
                    sent.push("edit pool");
                    let generation = shell.ctx.next_generation();
                    connection
                        .shell(ShellRequest::EditPool { change, generation })
                        .unwrap();
                }
                Request::Settings(job) => {
                    sent.push("settings");
                    connection.settings(*job).unwrap();
                }
                other => panic!("Settings pools never send {other:?}"),
            }
        }
        quiet = if moved || connection.busy() {
            0
        } else {
            quiet + 1
        };
        if quiet > QUIET_ROUNDS {
            return sent;
        }
        assert!(
            Instant::now() < deadline,
            "engine reads never settled: {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn ctx() -> Ctx {
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 80;
    ctx.height = 24;
    ctx
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_settings_pool_editor_chooses_strict_quota_and_editing_keeps_it() {
    let engine = Engine::start().await;
    let project = engine.root.path().join("project.toml");
    let mut connection = engine.connect();
    let mut ctx = ctx();
    let pools = open(&Place::Settings(SettingsSection::Pools), &mut ctx);
    let mut shell = Shell::with(ctx, pools);
    pump(&mut shell, &mut connection).await;
    shell.key(Key::Char('a'));
    pump(&mut shell, &mut connection).await;
    let editor = render(&mut shell);
    assert!(editor.contains("New pool"), "{editor}");
    assert!(editor.contains(BOUNDED_LINE), "{editor}");

    for character in "Strict pool".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Char(' '));
    shell.key(Key::Char('p'));
    let strict = render(&mut shell);
    assert!(strict.contains(STRICT_LINE), "{strict}");
    assert!(
        strict.contains("Strict blocks unknown provider quota"),
        "{strict}"
    );
    assert!(
        shell.ctx.queued() == 0,
        "switching the policy saves nothing"
    );
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"edit pool"), "{sent:?}");
    assert!(sent.contains(&"pools"), "{sent:?}");
    let saved = saved_pools(&engine.global(), &project);
    assert_eq!(saved.len(), 1, "{saved:?}");
    let created = saved[0].clone();
    assert_eq!(created.name.as_deref(), Some("Strict pool"));
    assert_eq!(
        created.members,
        vec![CredentialRef::new("local-a").unwrap()]
    );
    assert_eq!(created.provider_quota_policy, ProviderQuotaPolicy::Strict);

    let mut ctx = std::mem::replace(&mut shell.ctx, self::ctx());
    let reopened = open(&Place::EditPool(created.id.clone()), &mut ctx);
    let mut shell = Shell::with(ctx, reopened);
    pump(&mut shell, &mut connection).await;
    let editor = render(&mut shell);
    assert!(editor.contains("Edit pool"), "{editor}");
    assert!(
        editor.contains(STRICT_LINE),
        "the saved policy is shown: {editor}"
    );
    shell.key(Key::Tab);
    shell.key(Key::Char('p'));
    let bounded = render(&mut shell);
    assert!(bounded.contains(BOUNDED_LINE), "{bounded}");
    assert!(
        bounded.contains("Bounded attempt still requires fresh readiness"),
        "{bounded}"
    );
    shell.key(Key::Tab);
    shell.key(Key::Enter);
    let sent = pump(&mut shell, &mut connection).await;
    assert_eq!(sent.first(), Some(&"edit pool"), "{sent:?}");
    let edited = saved_pools(&engine.global(), &project);
    assert_eq!(edited.len(), 1, "{edited:?}");
    let mut expected = created.clone();
    expected.provider_quota_policy = ProviderQuotaPolicy::BoundedAttempt;
    assert_eq!(edited[0], expected, "only the policy changed");
}
