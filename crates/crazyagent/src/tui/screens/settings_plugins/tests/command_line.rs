use super::*;

#[test]
fn a_command_line_reads_back_as_the_same_command() {
    let spec = stdio(UVX, &["mcp-server-postgres", "--name", "it's here", ""]);
    assert_eq!(command_spec(&spec.runs()), Ok(spec));
    #[cfg(not(windows))]
    {
        assert_eq!(
            words(r"a 'b c'd e\ f"),
            Ok(vec!["a".to_owned(), "b cd".to_owned(), "e f".to_owned()])
        );
        assert_eq!(words("a \\"), Err("A backslash ends the line."));
    }
    #[cfg(windows)]
    {
        assert_eq!(
            words(r"a 'b c'd e\ f"),
            Ok(vec![
                "a".to_owned(),
                "b cd".to_owned(),
                r"e\".to_owned(),
                "f".to_owned()
            ])
        );
        assert_eq!(words(r"a \"), Ok(vec!["a".to_owned(), r"\".to_owned()]));
        assert_eq!(
            words(r"it\'s C:\x"),
            Ok(vec!["it's".to_owned(), r"C:\x".to_owned()])
        );
    }
    assert_eq!(words("a 'b"), Err("A quote is not closed."));
    assert_eq!(words("  "), Ok(Vec::new()));
}

/// On Windows a backslash separates folders, so the command hint's own example, typed
/// without quotes, reads as that command; a backslash keeps the next character only before
/// a quote, as a saved line writes one.
#[cfg(windows)]
#[test]
fn the_windows_hint_example_typed_without_quotes_reads_as_written() {
    let example = r"C:\Users\you\.local\bin\uvx.exe";
    assert!(COMMAND_HINT.contains(example), "{COMMAND_HINT}");
    assert_eq!(
        command_spec(&format!("{example} mcp-x")),
        Ok(stdio(example, &["mcp-x"]))
    );
}

#[test]
fn an_unchanged_server_line_writes_nothing_and_an_added_word_keeps_the_rest_exact() {
    #[cfg(unix)]
    const SPACED: &str = "/opt/My Tools/uvx";
    #[cfg(not(unix))]
    const SPACED: &str = r"C:\My Tools\uvx.exe";
    #[cfg(unix)]
    const GREP: &str = "/usr/bin/grep";
    #[cfg(not(unix))]
    const GREP: &str = r"C:\Windows\System32\findstr.exe";
    let specs = [
        stdio(SPACED, &["mcp-server-x"]),
        stdio(
            GREP,
            &[r"--pattern=\d+", "it's", "a b", "", r"C:\path", "'", r"\"],
        ),
        stdio(NPX, &["@modelcontextprotocol/server-github"]),
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
    let github = || {
        let mut github = github_with_token();
        github.spec = stdio(NPX, &["@modelcontextprotocol/server-github"]);
        github.runs = github.spec.runs();
        github
    };
    let mut ctx = plugin_ctx(80, 24);
    let mut screen = EditServer::new(github());
    assert!(screen.typing(), "the page opens on the command");
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(ctx.queued(), 0);
    assert_eq!(ctx.message().unwrap().lead, "Nothing changed.");
    screen.key(&Key::Char(' '), &mut ctx);
    for character in "--read-only".chars() {
        screen.key(&Key::Char(character), &mut ctx);
    }
    screen.key(&Key::Enter, &mut ctx);
    let wanted = stdio(NPX, &["@modelcontextprotocol/server-github", "--read-only"]);
    match ctx.next_request() {
        Some(Request::Settings(job)) => match *job {
            SettingsJob::Plugin(PluginChange::Update { id, spec, version }) => {
                assert_eq!(id, "server-github");
                assert_eq!(spec, wanted);
                assert_eq!(version, 2, "the edit names the version the list showed");
            }
            other => panic!("update expected, got {other:?}"),
        },
        other => panic!("settings request expected, got {other:?}"),
    }
    assert!(screen.busy(&ctx));
    let mut updated = github();
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
