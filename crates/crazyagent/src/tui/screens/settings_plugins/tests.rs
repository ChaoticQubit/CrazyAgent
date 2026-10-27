use super::import_page::outcome_line;
use super::skill_detail::{file_size, skill_body};
use super::*;
use crate::plugins::{Scope, SecretView};
use crate::skills::import::{ImportCandidate, ImportFrom, ImportList, ImportState, Imported};
use crate::tui::client::OwnerEvent;
use crate::tui::screens::settings_accounts::tests::{check, dataset, list_ctx, render};
use crate::tui::seam::Request;
use crate::tui::shell::Shell;

fn skill(name: &str, enabled: bool) -> SkillView {
    SkillView {
        id: format!("skill-{name}"),
        name: name.into(),
        description: format!("How to do {name} work."),
        line: format!("How to do {name} work."),
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
    assert_eq!(command_spec("uvx mcp"), Err(COMMAND_HINT));
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
    assert_eq!(
        plugin_plain("server_changed"),
        "This server changed. Review it again before approving."
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
                matches!(*job, SettingsJob::Plugin(PluginChange::AddSkill { scope: Scope::Global, ref folder }) if folder == "/Users/mira/Skills/tiger-style")
            );
        }
        other => panic!("add skill expected, got {other:?}"),
    }
    assert!(form.busy(&ctx));
}

fn github_with_token() -> ServerView {
    let (_, servers) = lists();
    let mut github = servers[0].clone();
    github.secrets = vec![SecretView {
        name: "GITHUB_TOKEN".into(),
        saved: true,
    }];
    github
}

fn changed(server: Option<ServerView>) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::PluginChanged(Ok(server)),
    ))))
}

#[test]
fn details_of_an_approved_server_open_its_edit_page() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    for _ in 0..4 {
        screen.key(&Key::Down, &mut ctx);
    }
    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Right, &mut ctx);
    let flow = screen.key(&Key::Enter, &mut ctx);
    let Flow::Replace(next) = flow else {
        panic!("the secrets page replaces the list");
    };
    assert_eq!(next.crumbs(), ["Settings", "Plugins and skills", "github"]);
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(ctx.queued(), 0);
}

#[test]
fn saving_a_secret_sends_it_once_and_never_shows_the_value() {
    let mut shell = Shell::with(
        plugin_ctx(80, 24),
        Box::new(EditServer::new(github_with_token())),
    );
    let rows = render(&mut shell, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("GITHUB_TOKEN") && row.contains("Key saved")),
        "{rows:?}"
    );
    shell.key(Key::Tab);
    shell.key(Key::Tab);
    for character in "SENTRY_DSN".chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Tab);
    shell.key(Key::Paste("https://k9:tidewater42@sentry.io/7".into()));
    let rows = render(&mut shell, 80, 24);
    assert!(rows.iter().all(|row| !row.contains("tidewater42")));
    shell.key(Key::Enter);
    match shell.ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(!format!("{job:?}").contains("tidewater42"));
            match *job {
                SettingsJob::Plugin(PluginChange::SetSecret { id, name, .. }) => {
                    assert_eq!(id, "server-github");
                    assert_eq!(name, "SENTRY_DSN");
                }
                other => panic!("save expected, got {other:?}"),
            }
        }
        other => panic!("settings request expected, got {other:?}"),
    }
    let rows = render(&mut shell, 80, 24);
    assert!(rows.iter().all(|row| !row.contains("tidewater42")));
    shell.key(Key::Enter);
    assert_eq!(shell.ctx.queued(), 0, "one change at a time");
}

#[test]
fn a_saved_reply_updates_the_list_and_says_what_changed() {
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github_with_token());
    screen.key(&Key::Tab, &mut ctx);
    assert!(!screen.typing());
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.typing(), "Enter on a secret starts replacing it");
    screen.key(&Key::Paste("ghp_rotated".into()), &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(
                matches!(*job, SettingsJob::Plugin(PluginChange::SetSecret { ref name, .. }) if name == "GITHUB_TOKEN")
            );
        }
        other => panic!("save expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    let mut updated = github_with_token();
    updated.secrets.push(SecretView {
        name: "SENTRY_DSN".into(),
        saved: true,
    });
    screen.arrive(&changed(Some(updated)), &mut ctx);
    assert!(!screen.busy(&ctx));
    assert_eq!(ctx.message().unwrap().lead, "Saved GITHUB_TOKEN.");
    let (lines, _) = screen.body(76, &ctx);
    let text: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert!(
        text.iter().any(|line| line.contains("SENTRY_DSN")),
        "{text:?}"
    );
}

#[test]
fn removing_a_secret_asks_first() {
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github_with_token());
    screen.key(&Key::Tab, &mut ctx);
    screen.key(&Key::Char('d'), &mut ctx);
    assert_eq!(screen.modal(&ctx).unwrap().0.title, "Remove secret");
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none(), "Cancel is focused first");
    assert_eq!(ctx.queued(), 0);
    screen.key(&Key::Char('d'), &mut ctx);
    screen.key(&Key::Right, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => {
            assert!(
                matches!(*job, SettingsJob::Plugin(PluginChange::RemoveSecret { ref id, ref name }) if id == "server-github" && name == "GITHUB_TOKEN")
            );
        }
        other => panic!("remove expected, got {other:?}"),
    }
    let (_, servers) = lists();
    screen.arrive(&changed(Some(servers[0].clone())), &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Removed GITHUB_TOKEN.");
    assert!(
        screen.typing(),
        "with no secrets left the name field has focus"
    );
}

#[test]
fn a_bad_secret_name_is_refused_before_the_engine_sees_it() {
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github_with_token());
    screen.key(&Key::Tab, &mut ctx);
    screen.key(&Key::Tab, &mut ctx);
    for character in "9 bad".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Tab, &mut ctx);
    screen.key(&Key::Paste("value".into()), &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0);
    assert_eq!(
        screen.name.error.as_deref(),
        Some("Secret names use letters, digits, _ or -, and start with a letter or _.")
    );
}

#[test]
fn a_command_line_reads_back_as_the_same_command() {
    let spec = stdio(
        "/opt/homebrew/bin/uvx",
        &["mcp-server-postgres", "--name", "it's here", ""],
    );
    assert_eq!(command_spec(&spec.runs()), Ok(spec));
    assert_eq!(
        words(r"a 'b c'd e\ f"),
        Ok(vec!["a".to_owned(), "b cd".to_owned(), "e f".to_owned()])
    );
    assert_eq!(words("a 'b"), Err("A quote is not closed."));
    assert_eq!(words("a \\"), Err("A backslash ends the line."));
    assert_eq!(words("  "), Ok(Vec::new()));
}

#[test]
fn an_unchanged_server_line_writes_nothing_and_an_added_word_keeps_the_rest_exact() {
    let specs = [
        stdio("/opt/My Tools/uvx", &["mcp-server-x"]),
        stdio(
            "/usr/bin/grep",
            &[r"--pattern=\d+", "it's", "a b", "", r"C:\path", "'", r"\"],
        ),
        stdio(
            "/opt/homebrew/bin/npx",
            &["@modelcontextprotocol/server-github"],
        ),
    ];
    for spec in specs {
        let line = spec.runs();
        assert_eq!(command_spec(&line), Ok(spec.clone()), "{line}");
        let mut ctx = plugin_ctx(80, 24);
        let mut screen = EditServer::new(server("odd", spec.clone(), ServerStatus::Approved));
        screen.key(&Key::Enter, &mut ctx);
        assert_eq!(ctx.queued(), 0, "no write for an unchanged {line}");
        assert_eq!(ctx.message().unwrap().lead, "Nothing changed.");
        assert_eq!(screen.target.error, None, "{line}");
        for character in " --read-only".chars() {
            screen.key(&Key::Char(character), &mut ctx);
        }
        screen.key(&Key::Enter, &mut ctx);
        let ServerSpec::Stdio { command, mut args } = spec else {
            panic!("stdio spec expected");
        };
        args.push("--read-only".into());
        let wanted = ServerSpec::Stdio { command, args };
        match ctx.next_request() {
            Some(Request::Settings(job)) => match *job {
                SettingsJob::Plugin(PluginChange::Update { spec, .. }) => {
                    assert_eq!(spec, wanted, "{line}");
                }
                other => panic!("update expected, got {other:?}"),
            },
            other => panic!("settings request expected for {line}, got {other:?}"),
        }
    }
}

#[test]
fn changing_what_a_server_runs_sends_the_new_command_and_asks_for_approval() {
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github_with_token());
    assert!(screen.typing(), "the page opens on the command");
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0);
    assert_eq!(ctx.message().unwrap().lead, "Nothing changed.");
    screen.key(&Key::Char(' '), &mut ctx);
    for character in "--read-only".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Enter, &mut ctx);
    let wanted = stdio(
        "/opt/homebrew/bin/npx",
        &["@modelcontextprotocol/server-github", "--read-only"],
    );
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::Plugin(PluginChange::Update { id, spec }) => {
                assert_eq!(id, "server-github");
                assert_eq!(spec, wanted);
            }
            other => panic!("update expected, got {other:?}"),
        },
        other => panic!("settings request expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    let mut updated = github_with_token();
    updated.runs = wanted.runs();
    updated.spec = wanted;
    updated.status = ServerStatus::WaitingForApproval;
    screen.arrive(&changed(Some(updated)), &mut ctx);
    assert_eq!(
        ctx.message().unwrap().lead,
        "Changed github. Approve it before agents can use it."
    );
    let (lines, _) = screen.body(76, &ctx);
    let first: String = lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(first.contains("Needs approval"), "{first}");
}

#[test]
fn a_command_without_a_full_path_is_refused_on_the_edit_page() {
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github_with_token());
    screen.key(&Key::Ctrl('u'), &mut ctx);
    for character in "npx server".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0);
    assert_eq!(screen.target.error.as_deref(), Some(COMMAND_HINT));
}

#[test]
fn a_project_form_adds_for_that_project_and_returns_to_its_settings() {
    let tidepool = crate::protocol::ProjectId::new("tidepool").unwrap();
    let back = |flow: Flow| match flow {
        Flow::Replace(screen) => match screen.place() {
            Some(Place::Project { id, name, tab }) => {
                assert_eq!(id.as_str(), "tidepool");
                assert_eq!(name, "Tidepool");
                assert_eq!(tab, ProjectTab::Settings);
            }
            other => panic!("the project's Settings tab expected, got {other:?}"),
        },
        other => panic!("the form must give way to the project page, got {other:?}"),
    };
    let mut ctx = plugin_ctx(80, 24);
    let mut form = AddPlugin::skill().for_project(tidepool.clone(), "Tidepool".into());
    assert_eq!(form.crumbs(), ["Tidepool", "Settings", "Add skill"]);
    assert!(matches!(
        form.place(),
        Some(Place::Project {
            tab: ProjectTab::Settings,
            ..
        })
    ));
    let (lines, _) = form.body(76, &ctx);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string() == "Only this project can use it. Turn it off any time.")
    );
    for character in "/Users/mira/Skills/tidepool-style".chars() {
        form.key(&Key::Char(character), &mut ctx);
    }
    form.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => assert!(matches!(
            *job,
            SettingsJob::Plugin(PluginChange::AddSkill {
                scope: Scope::Project { ref project_id },
                ref folder,
            }) if project_id.as_str() == "tidepool" && folder == "/Users/mira/Skills/tidepool-style"
        )),
        other => panic!("add skill expected, got {other:?}"),
    }
    back(form.arrive(&changed(None), &mut ctx));
    assert_eq!(ctx.message().unwrap().lead, "Skill added for this project.");

    let mut form = AddPlugin::server().for_project(tidepool, "Tidepool".into());
    assert_eq!(form.crumbs(), ["Tidepool", "Settings", "Add MCP server"]);
    for character in "postgres".chars() {
        form.key(&Key::Char(character), &mut ctx);
    }
    form.key(&Key::Tab, &mut ctx);
    form.key(&Key::Tab, &mut ctx);
    for character in "/opt/homebrew/bin/uvx mcp-server-postgres".chars() {
        form.key(&Key::Char(character), &mut ctx);
    }
    form.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => assert!(matches!(
            *job,
            SettingsJob::Plugin(PluginChange::AddServer {
                scope: Scope::Project { ref project_id },
                ref name,
                ..
            }) if project_id.as_str() == "tidepool" && name == "postgres"
        )),
        other => panic!("add server expected, got {other:?}"),
    }
    back(form.key(&Key::Esc, &mut ctx));

    let (_, servers) = lists();
    let mut postgres = servers[0].clone();
    postgres.scope = Scope::Project {
        project_id: crate::protocol::ProjectId::new("tidepool").unwrap(),
    };
    let name = postgres.name.clone();
    let mut page = EditServer::new(postgres).for_project(
        crate::protocol::ProjectId::new("tidepool").unwrap(),
        "Tidepool".into(),
    );
    assert_eq!(
        page.crumbs(),
        ["Tidepool".to_owned(), "Settings".into(), name]
    );
    assert!(matches!(
        page.place(),
        Some(Place::Project {
            tab: ProjectTab::Settings,
            ..
        })
    ));
    back(page.key(&Key::Esc, &mut ctx));
}

fn texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

fn detail(enabled: bool) -> crate::skills::SkillDetail {
    let mut skill = skill("frontend-design", enabled);
    skill.source = "/Users/mira/Skills/frontend-design".into();
    crate::skills::SkillDetail {
        skill,
        text: "---\nname: frontend-design\ndescription: How to do frontend-design work.\n---\n\n# Steps\n\n- Read the brief\n- Run `npm run build`\n".into(),
        files: vec![
            crate::skills::SkillFile {
                path: "SKILL.md".into(),
                bytes: 812,
            },
            crate::skills::SkillFile {
                path: "references/tokens.md".into(),
                bytes: 4300,
            },
        ],
        files_more: 2,
    }
}

#[test]
fn a_skill_details_first_action_opens_its_full_skill_md() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    screen.key(&Key::Enter, &mut ctx);
    let (modal, _) = screen.modal(&ctx).unwrap();
    assert!(
        texts(&modal.body).last().unwrap().contains("Read SKILL.md"),
        "{:?}",
        texts(&modal.body)
    );
    let flow = screen.key(&Key::Enter, &mut ctx);
    let Flow::Replace(mut next) = flow else {
        panic!("the skill page replaces the list");
    };
    assert_eq!(
        next.crumbs(),
        ["Settings", "Plugins and skills", "frontend-design"]
    );
    assert!(screen.modal(&ctx).is_none());
    assert_eq!(ctx.queued(), 0);
    next.shown(&mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => assert!(
            matches!(*job, SettingsJob::SkillDetail { ref id } if id == "skill-frontend-design")
        ),
        other => panic!("a skill read expected, got {other:?}"),
    }
}

#[test]
fn the_skill_page_renders_skill_md_and_files_and_switches_the_skill() {
    let ctx = plugin_ctx(80, 24);
    let page = SkillDetailPage::with(detail(true));
    let lines = texts(&page.lines(70, &ctx));
    assert!(
        lines[0].starts_with("frontend-design  On for every project"),
        "{lines:?}"
    );
    assert!(lines.contains(&"How to do frontend-design work.".to_owned()));
    assert!(
        lines.contains(&"From ~/Skills/frontend-design".to_owned()),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line == "Steps"), "{lines:?}");
    assert!(lines.iter().any(|line| line.contains("Read the brief")));
    assert!(lines.iter().any(|line| line.contains("npm run build")));
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("name: frontend-design"))
    );
    assert!(lines.contains(&"Files  4".to_owned()));
    assert!(lines.contains(&"references/tokens.md  4.2 KB".to_owned()));
    assert!(lines.contains(&"SKILL.md  812 B".to_owned()));
    assert!(lines.contains(&"And 2 more".to_owned()));

    let mut ctx = plugin_ctx(80, 24);
    let mut page = SkillDetailPage::with(detail(true));
    page.key(&Key::Char(' '), &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => assert!(
            matches!(*job, SettingsJob::Plugin(PluginChange::SetSkill { ref id, enabled: false }) if id == "skill-frontend-design")
        ),
        other => panic!("a switch expected, got {other:?}"),
    }
    page.key(&Key::Char(' '), &mut ctx);
    assert_eq!(ctx.queued(), 0, "one change at a time");
    page.arrive(&changed(None), &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "frontend-design is off.");
    assert!(
        matches!(ctx.next_request(), Some(Request::Settings(job)) if matches!(*job, SettingsJob::SkillDetail { .. })),
        "the skill reloads"
    );
    let read = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::SkillDetail(Ok(detail(false))),
    ))));
    page.arrive(&read, &mut ctx);
    assert!(texts(&page.lines(70, &ctx))[0].starts_with("frontend-design  Off"));
    let Flow::Replace(back) = page.key(&Key::Esc, &mut ctx) else {
        panic!("Esc goes back to the list");
    };
    assert_eq!(back.crumbs(), ["Settings", "Plugins and skills"]);
}

#[test]
fn the_skill_page_scrolls_inside_the_window_and_says_why_it_failed() {
    let mut long = detail(true);
    long.text = format!("---\nname: a\n---\n{}", "- step\n".repeat(120));
    let mut shell = Shell::with(plugin_ctx(80, 24), Box::new(SkillDetailPage::with(long)));
    let first = render(&mut shell, 80, 24);
    assert_eq!(first.len(), 24);
    assert!(first.iter().any(|row| row.contains("frontend-design  On")));
    let mut ctx = plugin_ctx(80, 24);
    let mut page = SkillDetailPage::new("skill-x".into(), "notes".into());
    page.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    let failed = Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(
        SettingsEvent::SkillDetail(Err("skill_not_found".into())),
    ))));
    page.arrive(&failed, &mut ctx);
    let lines = texts(&page.lines(70, &ctx));
    assert_eq!(lines[2], plugin_plain("skill_not_found"));
    page.key(&Key::Char('r'), &mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    assert_eq!(skill_body("---\nname: a\n---\n\nBody"), "Body");
    assert_eq!(skill_body("No header\n---\n"), "No header\n---\n");
    assert_eq!(skill_body("---\nopen"), "---\nopen");
    assert_eq!(
        [0, 1023, 1024, 4300, 1_048_576].map(file_size),
        ["0 B", "1023 B", "1.0 KB", "4.2 KB", "1.0 MB"]
    );
}

fn found(name: &str, from: ImportFrom, state: ImportState) -> ImportCandidate {
    let folder = match from {
        ImportFrom::Claude => format!("/Users/mira/.claude/skills/{name}"),
        ImportFrom::Codex => format!("/Users/mira/.codex/skills/{name}"),
    };
    ImportCandidate {
        from,
        folder,
        name: name.into(),
        line: format!("The {name} skill."),
        state,
    }
}

fn scanned() -> ImportList {
    ImportList {
        found: vec![
            found("notes", ImportFrom::Claude, ImportState::New),
            found(
                "graph",
                ImportFrom::Codex,
                ImportState::Added {
                    id: "skill-graph".into(),
                },
            ),
            found(
                "style",
                ImportFrom::Claude,
                ImportState::Differs {
                    id: "skill-style".into(),
                },
            ),
            found(
                "heavy",
                ImportFrom::Claude,
                ImportState::Unusable {
                    code: "skill_import_too_large".into(),
                },
            ),
        ],
        more: 3,
    }
}

fn import_event(event: SettingsEvent) -> Arrival {
    Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event))))
}

#[test]
fn i_opens_import_my_skills_which_reads_the_folders_only_once_it_opens() {
    let (skills, servers) = lists();
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = Plugins::with(skills, servers);
    assert!(
        screen
            .hints(&ctx)
            .iter()
            .any(|hint| hint.key == "i" && hint.label == "Import")
    );
    let Flow::Replace(mut next) = screen.key(&Key::Char('i'), &mut ctx) else {
        panic!("i opens Import my skills");
    };
    assert_eq!(
        next.crumbs(),
        ["Settings", "Plugins and skills", "Import my skills"]
    );
    assert_eq!(ctx.queued(), 0, "nothing is read before the page opens");
    next.shown(&mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => assert!(matches!(*job, SettingsJob::ImportScan)),
        other => panic!("a scan expected, got {other:?}"),
    }
}

#[test]
fn found_skills_show_source_and_state_and_only_ticked_ones_are_imported() {
    let ctx = plugin_ctx(80, 24);
    let page = ImportSkillsPage::with(scanned());
    let (lines, at) = page.lines(76, &ctx);
    let lines = texts(&lines);
    let row = |name: &str| {
        lines
            .iter()
            .position(|line| line.contains(&format!("] {name}")))
            .unwrap_or_else(|| panic!("{name} row in {lines:?}"))
    };
    assert_eq!(at, Some(row("notes")));
    assert!(lines[row("notes")].starts_with("> [ ] notes"), "{lines:?}");
    assert!(
        lines[row("notes") + 1].contains("The notes skill.  Claude Code: ~/.claude/skills/notes")
    );
    assert!(
        lines[row("graph")].contains("[-] graph  Already added"),
        "{lines:?}"
    );
    assert!(lines[row("graph") + 1].contains("Codex: ~/.codex/skills/graph"));
    assert!(lines[row("style")].contains("[ ] style  Differs"));
    assert!(
        lines[row("style") + 1]
            .contains("Importing replaces the added copy and keeps its project switches.")
    );
    assert!(lines[row("heavy")].contains("[-] heavy  Can't import"));
    assert!(
        lines[row("heavy") + 1]
            .contains("A skill brought in from your skills folders can be up to 2 MB.")
    );
    assert!(lines.contains(&"And 3 more not shown".to_owned()));
    assert!(lines.iter().all(|line| !line.contains("skill-graph")));

    let mut ctx = plugin_ctx(80, 24);
    let mut page = ImportSkillsPage::with(scanned());
    page.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0, "nothing ticked, nothing imported");
    page.key(&Key::Char(' '), &mut ctx);
    page.key(&Key::Down, &mut ctx);
    page.key(&Key::Char(' '), &mut ctx);
    page.key(&Key::Down, &mut ctx);
    page.key(&Key::Char(' '), &mut ctx);
    page.key(&Key::Down, &mut ctx);
    page.key(&Key::Char(' '), &mut ctx);
    let (ticked, _) = page.lines(76, &ctx);
    let ticked = texts(&ticked);
    assert!(ticked.iter().any(|line| line.contains("[x] notes")));
    assert!(ticked.iter().any(|line| line.contains("[x] style")));
    page.key(&Key::Enter, &mut ctx);
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::ImportSkills { ref picks } => assert_eq!(
                picks,
                &vec![
                    ("/Users/mira/.claude/skills/notes".to_owned(), None),
                    (
                        "/Users/mira/.claude/skills/style".to_owned(),
                        Some("skill-style".to_owned())
                    ),
                ]
            ),
            ref other => panic!("an import expected, got {other:?}"),
        },
        other => panic!("an import expected, got {other:?}"),
    }
    page.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0, "one import at a time");

    let mut imported = skill("notes", true);
    imported.id = "skill-notes".into();
    let done = vec![
        (
            "/Users/mira/.claude/skills/notes".to_owned(),
            Ok(Imported {
                skill: imported,
                skipped: vec![".env".into(), "api_key.txt".into()],
                skipped_total: 3,
            }),
        ),
        (
            "/Users/mira/.claude/skills/style".to_owned(),
            Err("skill_import_name_changed".into()),
        ),
    ];
    page.arrive(&import_event(SettingsEvent::Imported(Ok(done))), &mut ctx);
    assert_eq!(ctx.message().unwrap().lead, "Imported 1 skill.");
    let after = texts(&page.lines(76, &ctx).0);
    assert!(
        after.contains(&"Imported notes. Left out .env, api_key.txt and 1 more.".to_owned()),
        "{after:?}"
    );
    assert!(
        after
            .iter()
            .any(|line| line.starts_with("Could not import style:"))
    );
    assert!(
        after.iter().all(|line| !line.contains("[x]")),
        "ticks clear"
    );
    assert!(
        matches!(ctx.next_request(), Some(Request::Settings(job)) if matches!(*job, SettingsJob::ImportScan)),
        "the list is read again"
    );
}

#[test]
fn the_import_page_scrolls_and_says_why_the_scan_failed() {
    let mut many = scanned();
    many.found = (0..30)
        .map(|index| {
            found(
                &format!("skill-{index:02}"),
                ImportFrom::Claude,
                ImportState::New,
            )
        })
        .collect();
    let mut shell = Shell::with(plugin_ctx(80, 24), Box::new(ImportSkillsPage::with(many)));
    for _ in 0..29 {
        shell.key(Key::Down);
    }
    let frame = render(&mut shell, 80, 24);
    assert_eq!(frame.len(), 24);
    assert!(
        frame.iter().any(|row| row.contains("> [ ] skill-29")),
        "{frame:?}"
    );

    let mut ctx = plugin_ctx(80, 24);
    let mut page = ImportSkillsPage::new();
    page.shown(&mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    page.arrive(
        &import_event(SettingsEvent::ImportScan(Err(
            "skill_import_home_unknown".into()
        ))),
        &mut ctx,
    );
    let lines = texts(&page.lines(70, &ctx).0);
    assert!(lines.contains(&plugin_plain("skill_import_home_unknown").to_owned()));
    assert!(lines.contains(&"Press r to try again.".to_owned()));
    page.key(&Key::Char('r'), &mut ctx);
    assert!(matches!(ctx.next_request(), Some(Request::Settings(_))));
    page.arrive(
        &import_event(SettingsEvent::ImportScan(Ok(ImportList {
            found: vec![],
            more: 0,
        }))),
        &mut ctx,
    );
    assert!(
        texts(&page.lines(70, &ctx).0).contains(&"No skills found in those folders.".to_owned())
    );
    let Flow::Replace(back) = page.key(&Key::Esc, &mut ctx) else {
        panic!("Esc goes back to the list");
    };
    assert_eq!(back.crumbs(), ["Settings", "Plugins and skills"]);
    assert_eq!(
        outcome_line("notes", &Err("skill_link_outside".into())),
        format!(
            "Could not import notes: {}",
            plugin_plain("skill_link_outside")
        )
    );
}
