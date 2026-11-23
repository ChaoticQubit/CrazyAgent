use super::*;

/// The project instructions file every later agent and the master get as the owner's text
/// is read-only to the IDE server under every agent folders choice (SA vid 11): no
/// overwrite, replace, removal, move, hard link, make where missing or case variant, nor a
/// write of the file an owner's save stages before its rename (r2), while the rest of the
/// project stays writable. The owner edits it through the app.
#[test]
fn the_project_instructions_file_is_read_only_to_the_ide_server() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let state = root.join(".crazyagent");
    for path in [&home, &build, &state, &state.join("context")] {
        folder(path);
    }
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let instructions = state.join("instructions.md");
    let owner = "Owner rules.\n";
    let write = r#"printf 'DUMMY-MARKER' > "$1""#;
    for choice in [
        IdeAgentFiles::AllowAll,
        IdeAgentFiles::NoDeleteOrRename,
        IdeAgentFiles::ReadOnly,
    ] {
        let rule = AgentFilesRule {
            choice,
            older_agents: false,
        };
        let profile = install(&mine, &grants, PORT, closed(), &[], rule).unwrap();
        let _ = fs::remove_file(&instructions);
        assert!(
            !sh(&profile, write, &instructions),
            "{choice:?}: the server makes no instructions file"
        );
        assert!(!instructions.exists());
        fs::write(&instructions, owner).unwrap();
        assert!(
            !sh(&profile, write, &instructions),
            "{choice:?}: the server overwrites no instructions file"
        );
        assert!(!sh(&profile, write, &state.join("Instructions.MD")));
        assert!(!sh(&profile, r#"/bin/rm -f "$1""#, &instructions));
        assert!(!sh(&profile, r#"/bin/mv "$1" "$1-aside""#, &instructions));
        let planted = r#"printf 'DUMMY-MARKER' > "$1-new" && /bin/mv "$1-new" "$1""#;
        assert!(!sh(&profile, planted, &instructions));
        assert!(!sh(&profile, r#"/bin/ln "$1" "$1-alias""#, &instructions));
        assert!(!state.join("instructions.md-alias").exists());
        assert_eq!(fs::read_to_string(&instructions).unwrap(), owner);
        let staged = crate::agent_home::staged(
            &crate::agent_home::project_staging(&state),
            crate::agent_home::PROJECT_FILE,
            0,
        );
        assert!(
            !sh(&profile, write, &staged),
            "{choice:?}: the server makes no file a save stages"
        );
        assert!(!staged.exists());
        assert!(sh(&profile, write, &root.join("notes.txt")), "{choice:?}");
    }
    kernel_sandbox::toolchains::remove_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
}

/// V243-FARM2-PRODUCT-SMALL: no regex the IDE profile adds for its locks, the agents' IDE
/// config, their git admin folders or the agent folders rule holds a `(/.*)` tail, which
/// libsandbox fails to compile again in lower case after a fold group with no bracket class
/// or wildcard before it; each matches a path and everything below it as one `X(/|$)`.
#[test]
fn ide_denies_match_at_or_below_without_a_slash_tail() {
    let root = Path::new("/private/tmp/.tmpA1b2C3/project");
    let folder = root.join(".crazyagent").join("context");
    let text = [
        locks(&[(folder.as_path(), root)]),
        agent_config(root),
        agent_admin(root),
        AgentFilesRule {
            choice: IdeAgentFiles::ReadOnly,
            older_agents: true,
        }
        .seatbelt(root),
    ]
    .concat();
    let bodies: Vec<&str> = text
        .split("#\"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(body, _)| body))
        .collect();
    assert!(bodies.len() >= 6, "{text}");
    let tailed: Vec<&&str> = bodies
        .iter()
        .filter(|body| body.contains("(/.*)"))
        .collect();
    assert!(tailed.is_empty(), "{tailed:#?}");
    let below = bodies.iter().filter(|body| body.ends_with("(/|$)")).count();
    assert_eq!(below, 5, "{text}");
}
