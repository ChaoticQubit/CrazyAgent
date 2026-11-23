use super::*;

/// Every bundled update and install command is exactly the step the kernel's install
/// template builds from its recipe: each Homebrew name upgraded and installed, the npm
/// package installed into its prefix, and the own updater under each program name the
/// harness is found by.
#[test]
fn every_bundled_update_and_install_command_is_its_kernel_template() {
    let homebrew = Path::new("/opt/homebrew");
    let prefix = Path::new("/opt/crazyagent-npm");
    let mut seen = 0;
    for recipe in super::recipes::recipes() {
        for (word, verb) in [("upgrade", Verb::Update), ("install", Verb::Install)] {
            for brew in recipe.brews {
                let fixed = brew_fixed(homebrew, vec![word, flag(brew.kind), brew.name]);
                let template = brew_recipe(brew, verb);
                assert_eq!(
                    matched(fixed.clone(), &template),
                    Ok(fixed),
                    "{}",
                    recipe.id
                );
                seen += 1;
            }
            if let Some(npm) = &recipe.npm {
                let fixed = npm_fixed(prefix, npm_install_args(npm));
                let template = npm_recipe(npm, verb).unwrap();
                assert_eq!(
                    matched(fixed.clone(), &template),
                    Ok(fixed),
                    "{}",
                    recipe.id
                );
                seen += 1;
            }
        }
        if let Some(own) = &recipe.own {
            for binary in crate::harnesses::spec(recipe.id).unwrap().binaries {
                let fixed = Fixed {
                    program: Path::new("/usr/local/bin").join(binary),
                    args: own.args.to_vec(),
                    search: vec![PathBuf::from("/usr/local/bin")],
                    prefix: None,
                };
                let template = own_recipe(binary, own);
                assert_eq!(
                    matched(fixed.clone(), &template),
                    Ok(fixed),
                    "{}",
                    recipe.id
                );
                seen += 1;
            }
        }
    }
    assert!(seen >= super::recipes::recipes().len(), "{seen}");
}

/// One command and the recipe it claims to run, for the off-template rows.
type OffTemplate = (&'static str, Fixed, InstallRecipe);

/// A built command that is not exactly its kernel template is refused as unsafe before
/// anything runs: a Homebrew command with `--force`, with the other verb or kind, or with
/// another program, an npm install with a flag the recipe lacks or without its prefix, a
/// self-update with other words, and a relative program.
#[test]
fn a_command_off_its_kernel_template_is_refused() {
    let homebrew = Path::new("/opt/homebrew");
    let prefix = Path::new("/opt/crazyagent-npm");
    let cask = |verb| InstallRecipe::BrewCask {
        name: "codex".to_owned(),
        verb,
    };
    let npm = InstallRecipe::NpmGlobal {
        package: "@openai/codex".to_owned(),
        version: String::new(),
        verb: Verb::Install,
        flags: Vec::new(),
    };
    let own = InstallRecipe::SelfUpdate {
        program: "claude".to_owned(),
        args: vec!["update".to_owned()],
    };
    let cases: Vec<OffTemplate> = vec![
        (
            "force",
            brew_fixed(homebrew, vec!["install", "--cask", "--force", "codex"]),
            cask(Verb::Install),
        ),
        (
            "verb",
            brew_fixed(homebrew, vec!["install", "--cask", "codex"]),
            cask(Verb::Update),
        ),
        (
            "kind",
            brew_fixed(homebrew, vec!["upgrade", "--formula", "codex"]),
            cask(Verb::Update),
        ),
        (
            "program",
            Fixed {
                program: PathBuf::from("/opt/homebrew/bin/curl"),
                ..brew_fixed(homebrew, vec!["upgrade", "--cask", "codex"])
            },
            cask(Verb::Update),
        ),
        (
            "npm-flag",
            npm_fixed(prefix, vec!["install", "-g", "--force", "@openai/codex"]),
            npm.clone(),
        ),
        (
            "npm-prefix",
            Fixed {
                prefix: None,
                ..npm_fixed(prefix, vec!["install", "-g", "@openai/codex"])
            },
            npm,
        ),
        (
            "own-words",
            Fixed {
                program: PathBuf::from("/usr/local/bin/claude"),
                args: vec!["update", "--force"],
                search: vec![PathBuf::from("/usr/local/bin")],
                prefix: None,
            },
            own.clone(),
        ),
        (
            "relative",
            Fixed {
                program: PathBuf::from("claude"),
                args: vec!["update"],
                search: Vec::new(),
                prefix: None,
            },
            own,
        ),
    ];
    for (name, fixed, template) in cases {
        assert_eq!(matched(fixed, &template), Err(TOOL_UNSAFE), "{name}");
    }
}

/// The refusal codes the harness runner shows stay byte for byte as they were: the
/// administrator account is refused before anything starts, a tool past its time, a
/// program that cannot start, and a command off its kernel template.
#[test]
fn the_harness_refusal_codes_stay_byte_equal() {
    let machine = Machine::new();
    let home = machine.roots().home;
    let marker = machine.root.path().join("started");
    let marks = script(
        &machine,
        "bin/marks",
        &format!("printf started > {}\n", marker.display()),
        &[],
    );
    assert_eq!(
        run::run_raw_as(&marks, &home, CHECK_TIME, 0).err(),
        Some("harness_update_as_root")
    );
    assert!(!marker.exists());
    let slow = script(&machine, "bin/slow", "/bin/sleep 600\n", &[]);
    assert_eq!(
        run(&slow, &home, Duration::from_secs(1)).err(),
        Some("harness_update_timeout")
    );
    let unstartable = script(&machine, "bin/unstartable", "exit 0\n", &[]);
    std::fs::set_permissions(&unstartable.program, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        run(&unstartable, &home, CHECK_TIME).err(),
        Some("harness_update_start_failed")
    );
    let forced = brew_fixed(
        Path::new("/opt/homebrew"),
        vec!["install", "--cask", "--force", "codex"],
    );
    let template = InstallRecipe::BrewCask {
        name: "codex".to_owned(),
        verb: Verb::Install,
    };
    assert_eq!(
        matched(forced, &template).err(),
        Some("harness_update_tool_unsafe")
    );
}

/// The runner keeps the last bytes of each stream apart, within its stream bound, so the
/// error a tool prints at its end is what the engine reads.
#[test]
fn the_runner_keeps_the_last_bytes_of_each_stream() {
    let machine = Machine::new();
    let fixed = script(
        &machine,
        "bin/chatty",
        "i=0\nwhile [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789 >&2; i=$((i+1)); done\nprintf END >&2\nprintf out\n",
        &[],
    );
    let raw = run::run_raw(&fixed, &machine.roots().home, CHECK_TIME).unwrap();
    assert!(raw.success);
    assert_eq!(raw.stdout, b"out");
    assert_eq!(raw.stderr.len(), run::STREAM_BYTES_MAX);
    assert!(raw.stderr.ends_with(b"789\nEND"));
}

/// A tool that exits leaves nothing it started running: a child it put in the background
/// is ended with its process group when the run returns.
#[test]
fn a_tool_that_exits_leaves_no_process_it_started() {
    let machine = Machine::new();
    let noted = machine.root.path().join("child.pid");
    let fixed = script(
        &machine,
        "bin/leaves",
        &format!(
            "/bin/sleep 600 >/dev/null 2>&1 &\nprintf %s $! > {}\n",
            noted.display()
        ),
        &[],
    );
    assert!(
        run(&fixed, &machine.roots().home, CHECK_TIME)
            .unwrap()
            .success
    );
    let child: i32 = std::fs::read_to_string(&noted).unwrap().parse().unwrap();
    let child = rustix::process::Pid::from_raw(child).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while rustix::process::test_kill_process(child).is_ok() && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(rustix::process::test_kill_process(child).is_err());
}
