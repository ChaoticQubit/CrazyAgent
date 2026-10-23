use super::*;
use crate::plugins::{Scope, SecretView};
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings_accounts::tests::{check, dataset, list_ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;

fn skill(name: &str, enabled: bool) -> SkillView {
    SkillView {
        id: format!("skill-{name}"),
        name: name.into(),
        description: format!("How to do {name} work."),
        scope: Scope::Global,
        scope_label: Scope::Global.label().into(),
        enabled,
        on_here: None,
        source: format!("/Users/mira/Skills/{name}"),
        folder: format!("/private/tmp/engine/skills/{name}"),
    }
}

fn server(name: &str, spec: ServerSpec, status: ServerStatus) -> ServerView {
    ServerView {
        id: format!("server-{name}"),
        name: name.into(),
        scope: Scope::Global,
        scope_label: Scope::Global.label().into(),
        runs: spec.runs(),
        spec,
        enabled: status != ServerStatus::Off,
        status,
        status_label: status.label().into(),
        problem: None,
        tools: Vec::new(),
        checks: 0,
        secrets: Vec::new(),
        on_here: None,
    }
}

fn stdio(command: &str, args: &[&str]) -> ServerSpec {
    ServerSpec::Stdio {
        command: command.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

fn lists() -> (Vec<SkillView>, Vec<ServerView>) {
    let skills = vec![
        skill("frontend-design", true),
        skill("tiger-style", true),
        skill("linear-method", false),
        skill("release-notes", true),
    ];
    let mut postgres = server(
        "postgres-local",
        stdio("/opt/homebrew/bin/uvx", &["mcp-server-postgres"]),
        ServerStatus::WaitingForApproval,
    );
    postgres.secrets = vec![SecretView {
        name: "DATABASE_URL".into(),
        saved: true,
    }];
    let servers = vec![
        server(
            "github",
            stdio(
                "/opt/homebrew/bin/npx",
                &["@modelcontextprotocol/server-github"],
            ),
            ServerStatus::Approved,
        ),
        postgres,
        server(
            "figma",
            ServerSpec::Http {
                url: "https://mcp.figma.com/mcp".into(),
            },
            ServerStatus::CantStart,
        ),
    ];
    (skills, servers)
}

fn plugin_ctx(width: u16, height: u16) -> Ctx {
    let mut ctx = list_ctx(width, height, dataset(None).1);
    ctx.store.harnesses = Load::Ready((2, 9));
    ctx.home = Some("/Users/mira".into());
    ctx
}

#[test]
fn plugin_frames_match_the_design() {
    for (width, height) in [(80, 24), (120, 36)] {
        let (skills, servers) = lists();
        let mut shell = Shell::with(
            plugin_ctx(width, height),
            Box::new(Plugins::with(skills, servers)),
        );
        check(
            &format!("plugins-{width}"),
            &render(&mut shell, width, height),
            &[],
            None,
        );
    }
}

#[test]
fn space_turns_the_focused_skill_or_server_off_and_reloads() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    screen.key(&Key::Char(' '), &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(
                matches!(*job, SettingsJob::Plugin(PluginChange::SetSkill { ref id, enabled: false }) if id == "skill-frontend-design")
            );
        }
        other => panic!("change expected, got {other:?}"),
    }
    screen.key(&Key::Char(' '), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one change at a time");
    let done = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::PluginChanged(Ok(None)),
    ))));
    screen.arrive(&done, &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "frontend-design is off.");
    assert!(
        matches!(ctx.next_request(), Some(Request::Settings(_))),
        "the lists reload"
    );
    for _ in 0..4 {
        screen.key(&Key::Down, &mut ctx);
    }
    screen.waiting = false;
    screen.key(&Key::Char(' '), &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(
                matches!(*job, SettingsJob::Plugin(PluginChange::SetServer { ref id, enabled: false }) if id == "server-github")
            );
        }
        other => panic!("change expected, got {other:?}"),
    }
}

#[test]
fn approving_sends_exactly_the_command_shown() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    for _ in 0..5 {
        screen.key(&Key::Down, &mut ctx);
    }
    screen.key(&Key::Enter, &mut ctx);
    let (modal, hints) = screen.modal(&ctx).unwrap();
    assert_eq!(modal.title, "Approve MCP server");
    let text: Vec<String> = modal
        .body
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        text.iter()
            .any(|line| line.contains("/opt/homebrew/bin/uvx mcp-server-postgres")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|line| line.contains("DATABASE_URL") && line.contains("Key saved")),
        "{text:?}"
    );
    assert!(text.last().unwrap().contains("[> Approve ]"));
    assert_eq!(hints.last().unwrap().label, "Not now");
    screen.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::Plugin(PluginChange::Approve { id, spec }) => {
                assert_eq!(id, "server-postgres-local");
                assert_eq!(
                    spec,
                    stdio("/opt/homebrew/bin/uvx", &["mcp-server-postgres"])
                );
            }
            other => panic!("approve expected, got {other:?}"),
        },
        other => panic!("approve expected, got {other:?}"),
    }
}

#[test]
fn remove_asks_first() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Right, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(screen.modal(&ctx).unwrap().0.title, "Remove");
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "Cancel is focused first");
    assert_eq!(ctx.queued(), 0);
}

#[test]
fn the_server_form_checks_locally_and_hides_secret_values() {
    let mut shell = Shell::with(plugin_ctx(80, 24), Box::new(AddPlugin::server()));
    for character in "postgres-local".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    for character in "uvx mcp-server-postgres".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    for character in "DATABASE_URL".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Paste("postgres://mira:hunter2@localhost/app".into()));
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 24);
    assert!(
        rows.iter().all(|row| !row.contains("hunter2")),
        "secret values never show"
    );
    check(
        "plugin-add-80",
        &rows,
        &[],
        Some(&[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23,
        ]),
    );
    for _ in 0..2 {
        shell.key(Key::Tab);
    }
    shell.key(Key::Enter);
    let rows = render(&mut shell, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("Use the full path of the command")),
        "a command without a path is refused before the engine sees it"
    );
    assert_eq!(shell.ctx.queued(), 0);
}

#[test]
fn local_checks_and_short_lines() {
    assert_eq!(
        command_spec("/opt/homebrew/bin/uvx mcp-server-postgres --read-only"),
        Ok(stdio(
            "/opt/homebrew/bin/uvx",
            &["mcp-server-postgres", "--read-only"]
        ))
    );
    assert_eq!(
        command_spec("uvx mcp"),
        Err("Use the full path of the command, such as /opt/homebrew/bin/uvx.")
    );
    assert_eq!(
        command_spec("   "),
        Err("Type the command that starts the server.")
    );
    let (_, servers) = lists();
    let short: Vec<String> = servers.iter().map(short_runs).collect();
    assert_eq!(
        short,
        [
            "npx @modelcontextprotocol/server-github",
            "uvx mcp-server-postgres",
            "https://mcp.figma.com/mcp"
        ]
    );
    let ctx = plugin_ctx(80, 24);
    assert_eq!(tilde("/Users/mira/Skills/x", &ctx), "~/Skills/x");
    assert_eq!(tilde("/Users/miranda/x", &ctx), "/Users/miranda/x");
    assert_eq!(
        server_word(ServerStatus::WaitingForApproval),
        "Needs approval"
    );
}

#[test]
fn a_skill_folder_under_home_is_sent_as_a_full_path() {
    let mut ctx = plugin_ctx(80, 24);
    let mut form = AddPlugin::skill();
    for character in "~/Skills/tiger-style".chars() {
        form.key(&Key::Char(character), &mut ctx);
    }
    form.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(
                matches!(*job, SettingsJob::Plugin(PluginChange::AddSkill { ref folder }) if folder == "/Users/mira/Skills/tiger-style")
            );
        }
        other => panic!("add skill expected, got {other:?}"),
    }
    assert!(form.busy(&ctx));
}
