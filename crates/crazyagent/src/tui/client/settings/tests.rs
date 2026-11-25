use super::*;
use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;

/// The error a settings client row stops on: any failed file, engine or connection step.
type Failure = Box<dyn std::error::Error>;

/// What a settings client row returns, so a failed setup step ends the row with its error.
type TestResult = Result<(), Failure>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_loads_read_every_page_and_delete_reaches_the_engine() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ca-tset-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);

    for name in ["Laptop", "Mini", "Studio"] {
        let draft = AccountDraft {
            provider: Provider::Local,
            method: SignInMethod::Url,
            name: name.into(),
            url: None,
            model: Some("qwen2.5:0.5b".into()),
        };
        let added = native
            .settings(SettingsJob::Accounts(AccountsRequest::Add {
                draft,
                key: None,
            }))
            .await;
        assert!(matches!(
            added,
            SettingsEvent::Accounts(Ok(AccountsReply::Account(_)))
        ));
    }
    let mut lost = false;
    let accounts = native.all_accounts(2, &mut lost).await?;
    assert!(!lost);
    let mut names: Vec<&str> = accounts
        .iter()
        .map(|account| account.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Laptop", "Mini", "Studio"]);

    let note = |generation| SettingsJob::AddMemory {
        scope: MemoryScope::Global,
        text: "Use metric units in every recipe.".into(),
        generation,
    };
    let written = native.settings(note(1)).await;
    assert!(matches!(written, SettingsEvent::MemoryAdded(Ok(()))));
    let twice = native.settings(note(2)).await;
    assert!(matches!(
        twice,
        SettingsEvent::MemoryAdded(Err(code)) if code == "memory_duplicate"
    ));
    let nowhere = native
        .settings(SettingsJob::AddMemory {
            scope: MemoryScope::Project {
                project_id: ProjectId::new("nowhere")?,
            },
            text: "A project that was never made.".into(),
            generation: 3,
        })
        .await;
    assert!(matches!(
        nowhere,
        SettingsEvent::MemoryAdded(Err(code)) if code == "memory_project_missing"
    ));
    let SettingsEvent::Memories(_, Ok(memories)) = native
        .settings(SettingsJob::Memories {
            scopes: vec![MemoryScope::Global],
        })
        .await
    else {
        panic!("memories required")
    };
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].text, "Use metric units in every recipe.");
    let memory = memories[0].id.clone();
    let deleted = native
        .settings(SettingsJob::DeleteMemory {
            memory: memory.clone(),
            generation: 4,
        })
        .await;
    assert!(matches!(deleted, SettingsEvent::MemoryDeleted(Ok(()))));
    let SettingsEvent::Memories(_, Ok(memories)) = native
        .settings(SettingsJob::Memories {
            scopes: vec![MemoryScope::Global],
        })
        .await
    else {
        panic!("memories required")
    };
    assert!(memories.is_empty());
    let again = native
        .settings(SettingsJob::DeleteMemory {
            memory,
            generation: 5,
        })
        .await;
    assert!(matches!(
        again,
        SettingsEvent::MemoryDeleted(Err(code)) if code == "memory_missing"
    ));

    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_selected_accounts_removes_each_on_the_engine_and_names_the_one_already_gone()
-> TestResult {
    use crate::tui::client::OwnerEvent;
    use crate::tui::client::accounts::{AccountsReply, AccountsRequest};
    use crate::tui::keys::Key;
    use crate::tui::screens::Screen;
    use crate::tui::screens::settings_accounts::Accounts;
    use crate::tui::seam::{Arrival, Request};

    let root = tempfile::Builder::new()
        .prefix("ca-tdel-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let (mut service, _stop) = engine(root.path()).await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);
    for name in ["Laptop", "Mini", "Studio"] {
        let added = native
            .settings(SettingsJob::Accounts(AccountsRequest::Add {
                draft: AccountDraft {
                    provider: Provider::Local,
                    method: SignInMethod::Url,
                    name: name.into(),
                    url: None,
                    model: Some("qwen2.5:0.5b".into()),
                },
                key: None,
            }))
            .await;
        assert!(matches!(
            added,
            SettingsEvent::Accounts(Ok(AccountsReply::Account(_)))
        ));
    }
    let mut lost = false;
    let accounts = native.all_accounts(2, &mut lost).await?;
    let mut ctx = crate::tui::screens::settings_accounts::tests::ctx(80, 24);
    let mut screen = Accounts::with(accounts.clone());
    for name in ["Laptop", "Mini", "Studio"] {
        screen.key(&Key::Char('/'), &mut ctx);
        for letter in name.chars() {
            screen.key(&Key::Char(letter), &mut ctx);
        }
        screen.key(&Key::Enter, &mut ctx);
        screen.key(&Key::Char(' '), &mut ctx);
        screen.key(&Key::Esc, &mut ctx);
    }
    assert_eq!(ctx.message().ok_or("no message")?.lead, "3 selected.");
    screen.key(&Key::Char('d'), &mut ctx);
    assert_eq!(
        screen
            .modal(&ctx)
            .ok_or("a confirm before deleting")?
            .0
            .title,
        "Delete accounts"
    );
    let laptop = accounts
        .iter()
        .find(|account| account.name == "Laptop")
        .ok_or("Laptop was not added")?
        .id
        .clone();
    let gone = native
        .settings(SettingsJob::Accounts(AccountsRequest::Remove {
            id: laptop,
        }))
        .await;
    assert!(matches!(
        gone,
        SettingsEvent::Accounts(Ok(AccountsReply::Removed(_)))
    ));
    screen.key(&Key::Right, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    assert!(screen.modal(&ctx).is_none());
    assert!(screen.busy(&ctx));
    let mut sent = 0;
    for _ in 0..32 {
        let Some(request) = ctx.next_request() else {
            break;
        };
        if let Request::Settings(job) = request {
            sent += 1;
            let event = native.settings(*job).await;
            screen.arrive(
                &Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event)))),
                &mut ctx,
            );
        }
    }
    assert_eq!(sent, 4, "three removes one at a time, then one list reload");
    assert!(!screen.busy(&ctx));
    let message = ctx.message().ok_or("no message")?;
    assert_eq!(message.lead, "Deleted 2 accounts.");
    assert_eq!(
        message.rest,
        "Could not delete Laptop: This account no longer exists."
    );
    assert!(native.all_accounts(2, &mut lost).await?.is_empty());

    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_account_offers_one_entry_per_company_and_adds_the_same_engine_accounts() -> TestResult
{
    use crate::accounts::AccountStatus;
    use crate::tui::client::OwnerEvent;
    use crate::tui::ctx::Ctx;
    use crate::tui::keys::Key;
    use crate::tui::screens::Screen;
    use crate::tui::screens::settings_add_account::AddAccount;
    use crate::tui::seam::{Arrival, Request};

    async fn pump(screen: &mut AddAccount, ctx: &mut Ctx, native: &mut NativeOwner) -> usize {
        let mut held = 0;
        for _ in 0..16 {
            let Some(request) = ctx.next_request() else {
                break;
            };
            match request {
                Request::Settings(job) if matches!(*job, SettingsJob::Accounts(_)) => {
                    let event = native.settings(*job).await;
                    screen.arrive(
                        &Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event)))),
                        ctx,
                    );
                }
                Request::Settings(_) => held += 1,
                _ => {}
            }
        }
        held
    }
    fn text(screen: &AddAccount, ctx: &Ctx) -> Vec<String> {
        screen
            .body(76, ctx)
            .0
            .iter()
            .map(|line| line.to_string())
            .collect()
    }
    fn type_text(screen: &mut AddAccount, ctx: &mut Ctx, value: &str) {
        for letter in value.chars() {
            screen.key(&Key::Char(letter), ctx);
        }
    }

    let root = tempfile::Builder::new()
        .prefix("ca-tadd-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let (mut service, _stop) = engine(root.path()).await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);
    let mut ctx = crate::tui::screens::settings_accounts::tests::ctx(80, 24);

    let mut screen = AddAccount::new();
    screen.shown(&mut ctx);
    assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);
    let rows = text(&screen, &ctx);
    assert_eq!(rows.len(), 3 + 6 + 2, "six entries: {rows:#?}");
    for (row, (name, line)) in rows[3..9].iter().zip([
        ("OpenAI", "Sign in with ChatGPT or use an OpenAI API key"),
        (
            "Anthropic",
            "Sign in with Claude or use an Anthropic API key",
        ),
        ("Gemini API", "Pay-as-you-go Google Gemini API key"),
        ("OpenRouter", "One API key for models from many companies"),
        (
            "OpenAI-compatible URL",
            "Any server that speaks the OpenAI API",
        ),
        (
            "Local model",
            "Ollama, LM Studio or another server on this Mac",
        ),
    ]) {
        assert!(
            row.contains(&format!(" {name} ")) && row.contains(line),
            "{row}"
        );
    }
    assert!(
        rows.iter().all(|row| !row.contains("ChatGPT / Codex")),
        "{rows:#?}"
    );

    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    let methods = text(&screen, &ctx).join("\n");
    let first = methods
        .find("Sign in with your Claude subscription")
        .ok_or_else(|| methods.clone())?;
    let key = methods
        .find("Use an API key")
        .ok_or_else(|| methods.clone())?;
    assert!(first < key && !methods.contains("Codex"), "{methods}");
    assert!(methods.starts_with("Anthropic\n"), "{methods}");
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Tab, &mut ctx);
    screen.key(
        &Key::Paste("unit-placeholder-anthropic-key-0001".into()),
        &mut ctx,
    );
    screen.key(&Key::Tab, &mut ctx);
    type_text(&mut screen, &mut ctx, "claude-sonnet-4-5");
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);
    assert_eq!(ctx.message().ok_or("no message")?.lead, "Account added.");

    let mut screen = AddAccount::new();
    screen.shown(&mut ctx);
    pump(&mut screen, &mut ctx, &mut native).await;
    screen.key(&Key::Enter, &mut ctx);
    let methods = text(&screen, &ctx).join("\n");
    let order: Vec<usize> = ["Sign in with ChatGPT", "Use an API key"]
        .iter()
        .map(|title| methods.find(title))
        .collect::<Option<_>>()
        .ok_or_else(|| methods.clone())?;
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{methods}");
    assert!(
        !methods.contains("on this Mac") && methods.starts_with("OpenAI\n"),
        "{methods}"
    );
    screen.key(&Key::Down, &mut ctx);
    screen.key(&Key::Enter, &mut ctx);
    screen.key(&Key::Tab, &mut ctx);
    screen.key(
        &Key::Paste("unit-placeholder-openai-key-00000001".into()),
        &mut ctx,
    );
    screen.key(&Key::Tab, &mut ctx);
    type_text(&mut screen, &mut ctx, "gpt-5");
    screen.key(&Key::Enter, &mut ctx);
    assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);

    for down in [1, 0] {
        let mut screen = AddAccount::new();
        screen.shown(&mut ctx);
        pump(&mut screen, &mut ctx, &mut native).await;
        for _ in 0..down {
            screen.key(&Key::Down, &mut ctx);
        }
        screen.key(&Key::Enter, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        assert_eq!(
            pump(&mut screen, &mut ctx, &mut native).await,
            1,
            "the sign-in itself is held back"
        );
    }

    let mut lost = false;
    let mut added: Vec<(Provider, SignInMethod, String, String, AccountStatus)> = native
        .all_accounts(2, &mut lost)
        .await?
        .into_iter()
        .map(|account| {
            (
                account.provider,
                account.method,
                account.provider_name,
                account.name,
                account.status,
            )
        })
        .collect();
    added.sort_by_key(|account| account.0);
    assert_eq!(
        added,
        [
            (
                Provider::Chatgpt,
                SignInMethod::Subscription,
                "ChatGPT / Codex".to_owned(),
                "ChatGPT / Codex".to_owned(),
                AccountStatus::NotSignedIn
            ),
            (
                Provider::Claude,
                SignInMethod::Subscription,
                "Claude".to_owned(),
                "Claude".to_owned(),
                AccountStatus::NotSignedIn
            ),
            (
                Provider::OpenaiApi,
                SignInMethod::ApiKey,
                "OpenAI API".to_owned(),
                "OpenAI API".to_owned(),
                AccountStatus::KeySaved
            ),
            (
                Provider::AnthropicApi,
                SignInMethod::ApiKey,
                "Anthropic API".to_owned(),
                "Anthropic API".to_owned(),
                AccountStatus::KeySaved
            ),
        ]
    );

    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

fn changed(event: SettingsEvent) {
    match event {
        SettingsEvent::PluginChanged(Ok(None)) => {}
        SettingsEvent::PluginChanged(Err(code)) => panic!("plugin change refused: {code}"),
        _ => panic!("a plugin change reply without a server required"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_reads_its_plugins_and_switches_an_every_project_skill_off() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ca-tplug-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);
    let _home =
        crate::tui::test_home::projects_home(native.controller.as_mut().ok_or("not attached")?, 1)
            .await;

    let project = ProjectId::new("tidepool")?;
    let created = native
        .controller
        .as_mut()
        .ok_or("not attached")?
        .setup_project(
            project.clone(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            crate::project_setup::ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![crate::project_setup::Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await?;
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));
    let SettingsEvent::Suggestions(Ok(waiting)) = native
        .settings(SettingsJob::Suggestions {
            projects: vec![project.clone()],
        })
        .await
    else {
        panic!("suggestions required")
    };
    assert!(
        waiting.is_empty(),
        "a project without research suggests nothing"
    );
    let missing = Suggestion {
        project: project.clone(),
        entry: RequestId::new("entry-never-made")?,
        version: 1,
        text: "Never researched.".into(),
        kept: false,
    };
    let refused = native
        .settings(SettingsJob::Suggestion {
            suggestion: missing.clone(),
            keep: true,
            generations: [2, 3],
        })
        .await;
    assert!(matches!(
        refused,
        SettingsEvent::SuggestionDone(done, Err(_)) if done == missing
    ));
    let folder = root.path().join("picked/release");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(
        folder.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Teaches release notes.\n---\n\nSteps.\n",
    )?;
    let added = native
        .settings(SettingsJob::Plugin(PluginChange::AddSkill {
            scope: Scope::Global,
            folder: folder.display().to_string(),
        }))
        .await;
    changed(added);

    let read = |event: SettingsEvent| match event {
        SettingsEvent::ProjectPlugins(read, Ok((skills, servers))) => {
            assert_eq!(read.as_str(), "tidepool");
            assert!(servers.is_empty());
            skills
                .into_iter()
                .map(|skill| (skill.name, skill.on_here))
                .collect::<Vec<_>>()
        }
        SettingsEvent::ProjectPlugins(_, Err(code)) => panic!("project plugins failed: {code}"),
        _ => panic!("project plugins required"),
    };
    let here = read(
        native
            .settings(SettingsJob::ProjectPlugins {
                project: project.clone(),
            })
            .await,
    );
    assert_eq!(here, vec![("release-notes".to_owned(), Some(true))]);
    let SettingsEvent::Plugins(Ok((skills, _))) = native.settings(SettingsJob::Plugins).await
    else {
        panic!("every-project plugins required")
    };
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].on_here, None, "Settings shows no project switch");
    assert_eq!(skills[0].line, "Teaches release notes.");
    match native
        .settings(SettingsJob::SkillDetail {
            id: skills[0].id.clone(),
            project: None,
        })
        .await
    {
        SettingsEvent::SkillDetail(None, Ok(detail)) => {
            assert_eq!(detail.skill.id, skills[0].id);
            assert!(detail.text.ends_with("\n\nSteps.\n"), "{}", detail.text);
            let files: Vec<&str> = detail.files.iter().map(|file| file.path.as_str()).collect();
            assert_eq!(files, vec!["SKILL.md"]);
        }
        SettingsEvent::SkillDetail(_, Err(code)) => panic!("skill detail failed: {code}"),
        _ => panic!("a skill detail reply required"),
    }

    let switched = native
        .settings(SettingsJob::Plugin(PluginChange::Switch {
            project: project.clone(),
            id: skills[0].id.clone(),
            on: false,
            version: skills[0].version,
        }))
        .await;
    changed(switched);
    let here = read(
        native
            .settings(SettingsJob::ProjectPlugins {
                project: project.clone(),
            })
            .await,
    );
    assert_eq!(here, vec![("release-notes".to_owned(), Some(false))]);

    let found = root.path().join("home/.claude/skills/zeta");
    std::fs::create_dir_all(&found)?;
    std::fs::write(
        found.join("SKILL.md"),
        "---\nname: zeta\ndescription: Zeta steps.\n---\n",
    )?;
    std::fs::write(found.join("deploy.pem"), "dummy")?;
    let folder = found.display().to_string();
    match native
        .settings(SettingsJob::ImportSkills {
            picks: vec![(folder.clone(), None), (folder.clone(), None)],
        })
        .await
    {
        SettingsEvent::Imported(Ok(done)) => {
            assert_eq!(done.len(), 2);
            let first = done[0].1.as_ref().map_err(|code| code.clone())?;
            assert_eq!(first.skill.name, "zeta");
            assert!(first.skill.enabled);
            assert_eq!(first.skipped, vec!["deploy.pem"]);
            assert_eq!(done[1].1.as_ref().unwrap_err(), "skill_name_taken");
        }
        SettingsEvent::Imported(Err(code)) => panic!("import failed: {code}"),
        _ => panic!("an import reply required"),
    }
    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_memory_writes_change_on_the_engine_and_only_a_requested_memory_is_approved()
-> TestResult {
    use crate::memory::library::MemoryOrigin;
    let root = tempfile::Builder::new()
        .prefix("ca-twrites-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let (mut service, _stop) = engine(root.path()).await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);
    let writes = |set| SettingsJob::AgentWrites { set };
    assert!(matches!(
        native.settings(writes(None)).await,
        SettingsEvent::AgentWrites(Ok(AgentWrites::Save))
    ));
    assert!(matches!(
        native.settings(writes(Some((AgentWrites::Ask, 1)))).await,
        SettingsEvent::AgentWrites(Ok(AgentWrites::Ask))
    ));
    assert!(matches!(
        native.settings(writes(None)).await,
        SettingsEvent::AgentWrites(Ok(AgentWrites::Ask))
    ));

    let scope = |set| SettingsJob::AgentScope { set };
    assert!(matches!(
        native.settings(scope(None)).await,
        SettingsEvent::AgentScope(Ok(AgentScope::Project))
    ));
    assert!(matches!(
        native.settings(scope(Some((AgentScope::Agent, 1)))).await,
        SettingsEvent::AgentScope(Ok(AgentScope::Agent))
    ));
    assert!(matches!(
        native.settings(scope(None)).await,
        SettingsEvent::AgentScope(Ok(AgentScope::Agent))
    ));

    let path = root.path().join("engine.db");
    let alpha = ProjectId::new("alpha")?;
    let saved = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let mut db = rusqlite::Connection::open(path).map_err(|error| error.to_string())?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        db.execute("INSERT INTO projects(id,body) VALUES ('alpha','{}')", [])
            .map_err(|error| error.to_string())?;
        crate::memory::library::agent_remember(
            &mut db,
            &alpha,
            "Ada (@lead-a)",
            true,
            "Ship on Fridays.",
        )
        .map_err(|error| error.to_string())
    })
    .await??;
    assert_eq!(
        saved,
        Ok(crate::memory::library::AgentSaved {
            waiting: true,
            narrowed: false
        })
    );
    let agent = "Ada (@lead-a)".to_owned();
    let listed = |event: SettingsEvent| {
        let SettingsEvent::Memories(_, Ok(list)) = event else {
            panic!("the memories were not listed");
        };
        list
    };
    let global = || SettingsJob::Memories {
        scopes: vec![MemoryScope::Global],
    };
    let memories = listed(native.settings(global()).await);
    assert_eq!(memories.len(), 1);
    assert_eq!(
        memories[0].origin,
        MemoryOrigin::Requested {
            agent: agent.clone()
        }
    );
    let id = memories[0].id.clone();
    assert!(matches!(
        native
            .settings(SettingsJob::ApproveMemory {
                memory: id.clone(),
                generation: 2,
            })
            .await,
        SettingsEvent::MemoryApproved(Ok(()))
    ));
    let memories = listed(native.settings(global()).await);
    assert_eq!(memories[0].origin, MemoryOrigin::Agent { agent });
    assert!(matches!(
        native
            .settings(SettingsJob::ApproveMemory {
                memory: id,
                generation: 3,
            })
            .await,
        SettingsEvent::MemoryApproved(Err(code)) if code == "memory_not_requested"
    ));
    assert!(matches!(
        native.settings(writes(Some((AgentWrites::Save, 4)))).await,
        SettingsEvent::AgentWrites(Ok(AgentWrites::Save))
    ));
    assert!(matches!(
        native.settings(scope(Some((AgentScope::Project, 5)))).await,
        SettingsEvent::AgentScope(Ok(AgentScope::Project))
    ));
    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

async fn engine(
    root: &std::path::Path,
) -> Result<(EngineService, tokio::sync::watch::Sender<bool>), Failure> {
    let global = root.join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start_with_settings_sources(
        root,
        "e.sock",
        &global,
        &root.join("project.toml"),
        &stopped,
    )
    .await?;
    Ok((service, stop))
}

fn failure(event: &SettingsEvent) -> Option<&str> {
    match event {
        SettingsEvent::Accounts(Err(code))
        | SettingsEvent::AllAccounts(Err(code))
        | SettingsEvent::Harnesses(Err(code))
        | SettingsEvent::Memories(_, Err(code))
        | SettingsEvent::MemoryAdded(Err(code))
        | SettingsEvent::Plugins(Err(code)) => Some(code),
        _ => None,
    }
}

#[test]
fn only_a_failed_transport_counts_as_a_lost_connection() {
    for gone in [
        "No such file or directory (os error 2)",
        "trusted_owner_connection_failed",
        "controller_disconnected",
        "accounts_owner_unavailable",
        "accounts_outcome_unknown",
        "owner_outcome_unknown_reconcile_same_identity",
        "owner_socket_replaced",
        "owner_root_replaced_or_unsafe",
    ] {
        assert!(connection_lost(gone), "{gone}");
    }
    for answered in [
        "account_not_found",
        "memory_duplicate",
        "controller_pending_intent",
        "trusted_connection_unavailable",
        "model_access_read_only",
    ] {
        assert!(!connection_lost(answered), "{answered}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_or_restarted_engine_detaches_every_settings_job_and_it_reattaches() -> TestResult
{
    type Job = (&'static str, fn() -> SettingsJob);
    let jobs: [Job; 7] = [
        ("accounts list", || {
            SettingsJob::Accounts(AccountsRequest::List {
                after: None,
                limit: 50,
            })
        }),
        ("accounts catalog", || {
            SettingsJob::Accounts(AccountsRequest::Catalog)
        }),
        ("all accounts", || SettingsJob::AllAccounts),
        ("harnesses", || SettingsJob::Harnesses),
        ("memories", || SettingsJob::Memories {
            scopes: vec![MemoryScope::Global],
        }),
        ("add memory", || SettingsJob::AddMemory {
            scope: MemoryScope::Global,
            text: "Written while the engine is stopped.".into(),
            generation: 1,
        }),
        ("plugins", || SettingsJob::Plugins),
    ];
    let mut kept = Vec::new();
    let mut last = None;
    for ((label, job), restart) in jobs.into_iter().flat_map(|job| [(job, false), (job, true)]) {
        let root = tempfile::Builder::new()
            .prefix("ca-tlost-")
            .tempdir_in(std::fs::canonicalize("/tmp")?)?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let (mut service, _stop) = engine(root.path()).await?;
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let _ = native.attach().await;
        assert!(native.ready, "{label}: attach");
        tokio::time::timeout(Duration::from_secs(10), service.shutdown()).await??;
        let restarted = if restart {
            Some(engine(root.path()).await?)
        } else {
            None
        };
        let event = native.settings(job()).await;
        let code = failure(&event).map(str::to_owned);
        assert!(
            code.is_some(),
            "{label} (restart {restart}): the old session answered"
        );
        if native.ready {
            kept.push(format!("{label} (restart {restart}): {code:?}"));
        }
        if let Some((mut service, _stop)) = restarted {
            tokio::time::timeout(Duration::from_secs(10), service.shutdown()).await??;
        }
        last = Some((root, native));
    }
    assert!(
        kept.is_empty(),
        "still attached after the engine went away: {kept:#?}"
    );

    let (root, mut native) = last.ok_or("no engine was started")?;
    let (mut service, _stop) = engine(root.path()).await?;
    let _ = native.attach().await;
    assert!(native.ready);
    let listed = native
        .settings(SettingsJob::Accounts(AccountsRequest::List {
            after: None,
            limit: 50,
        }))
        .await;
    assert!(matches!(
        listed,
        SettingsEvent::Accounts(Ok(AccountsReply::Page(_)))
    ));
    drop(native);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_read_on_a_stopped_engine_drops_the_trusted_connection() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ca-tsign-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let _ = native.attach().await;
    assert!(native.ready);
    let controller = native.controller.as_mut().ok_or("not attached")?;
    let read = controller.read(OwnerRead::Capabilities).await?;
    let ticket = SignInTicket {
        scope: crate::engine::owner::NativeAuthScope {
            expected_database_instance: controller.identity().database_instance().into(),
            connection: CredentialRef::new("missing-login")?,
            expected_config_id: read.config_id,
            expected_source_hash: "a".repeat(64),
        },
        operation: fresh_id()?,
    };
    let check = || {
        SettingsJob::SignIn(SignInJob::Read {
            ticket: ticket.clone(),
        })
    };

    let refused = native.settings(check()).await;
    assert!(matches!(refused, SettingsEvent::SignIn(Err(_))));
    assert!(
        native.ready,
        "a sign-in the engine refuses keeps the connection"
    );

    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await??;
    let unanswered = native.settings(check()).await;
    assert!(matches!(unanswered, SettingsEvent::SignIn(Err(_))));
    assert!(
        !native.ready,
        "a sign-in read the stopped engine never answers drops the connection"
    );
    Ok(())
}
