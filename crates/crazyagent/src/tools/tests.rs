use super::*;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

/// A scratch HOME with no tools in it, its real path, so found files compare equal.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tools-resolve-")
        .tempdir()
        .unwrap();
    let home = folder.path().canonicalize().unwrap();
    (folder, home)
}

/// Writes an executable file with the given text, its folders made first.
/// Inputs: the file and the text. Output: the file.
fn program(file: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file.to_path_buf()
}

/// A stand-in `node`: a shell script that says it ran the file it was given.
const NODE: &str = "#!/bin/sh\necho \"node ran $1\"\n";

/// An npm-built Codex: `<prefix>/bin/codex` links to the package's `bin/codex.js`, whose
/// first line asks `env` for `node`. Input: the npm prefix. Output: the real script.
fn npm_codex(prefix: &Path) -> PathBuf {
    let script = program(
        &prefix.join("lib/node_modules/@openai/codex/bin/codex.js"),
        "#!/usr/bin/env node\nconsole.log('codex');\n",
    );
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/@openai/codex/bin/codex.js",
        prefix.join("bin/codex"),
    )
    .unwrap();
    script
}

/// The places the engine builds for a scratch HOME, kept to the folders inside it, so a
/// tool installed on this computer never answers for the scratch one. Input: HOME.
/// Output: the places.
fn home_only(home: &Path) -> Places {
    let engine = Places::engine(None, None, Some(home.to_path_buf()), Some(OsStr::new("")));
    Places::new(
        engine
            .folders()
            .iter()
            .filter(|folder| folder.starts_with(home))
            .cloned()
            .collect(),
        Some(home.to_path_buf()),
    )
}

/// The owner's fresh Mac: Codex from npm under a prefix whose `bin` also holds `node`, and
/// a launch whose search path is only `/usr/bin:/bin`. The resolver names the script and the
/// `node` its first line asks for, and its words start Codex with no search path at all,
/// where the script run by name fails as it did on that Mac.
#[test]
fn an_npm_codex_with_an_env_node_line_starts_with_no_search_path() {
    let (_folder, home) = scratch();
    let prefix = home.join(".npm-global");
    let script = npm_codex(&prefix);
    let node = program(&prefix.join("bin/node"), NODE);
    let places = home_only(&home);

    let codex = resolve("codex", &places).unwrap();
    assert_eq!(codex.entry, prefix.join("bin/codex"));
    assert_eq!(codex.program, script);
    let interpreter = codex.interpreter().expect("a script names its interpreter");
    assert_eq!(interpreter.program, node);
    assert_eq!(
        codex.argv(),
        vec![
            node.clone().into_os_string(),
            script.clone().into_os_string()
        ]
    );
    assert_eq!(
        codex.grants(),
        vec![script.clone(), node, PathBuf::from("/bin/sh")]
    );

    let by_name = crate::spawn_gate::output(
        Command::new(prefix.join("bin/codex"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin"),
    )
    .unwrap();
    assert!(
        !by_name.status.success(),
        "env finds no node on /usr/bin:/bin"
    );
    let words = codex.argv();
    let run = crate::spawn_gate::output(
        Command::new(&words[0])
            .args(&words[1..])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        format!("node ran {}", script.display())
    );
}

/// Node is found wherever a version manager put it: nvm's versions, volta's, asdf's, mise's
/// and fnm's real installs, never a volta shim, which would start the shim, not node.
#[test]
fn node_is_found_in_each_version_manager_layout_and_never_as_a_shim() {
    for (installed, expected) in [
        (".nvm/versions/node/v22.11.0/bin/node", true),
        (".volta/tools/image/node/20.18.0/bin/node", true),
        (".asdf/installs/nodejs/22.1.0/bin/node", true),
        (".local/share/mise/installs/node/22.1.0/bin/node", true),
        (
            "Library/Application Support/fnm/node-versions/v22.1.0/installation/bin/node",
            true,
        ),
        (".volta/bin/node", false),
    ] {
        let (_folder, home) = scratch();
        let node = program(&below(&home, installed), NODE);
        let found = resolve("node", &home_only(&home));
        match expected {
            true => assert_eq!(found.unwrap().program, node, "{installed}"),
            false => assert_eq!(found.unwrap_err().reason, Reason::NotFound, "{installed}"),
        }
    }
}

/// The newest of several installed Node versions is found first.
#[test]
fn the_newest_node_version_of_a_manager_is_found_first() {
    let (_folder, home) = scratch();
    program(&home.join(".volta/tools/image/node/18.20.0/bin/node"), NODE);
    let newest = program(&home.join(".volta/tools/image/node/22.11.0/bin/node"), NODE);
    program(&home.join(".volta/tools/image/node/20.18.0/bin/node"), NODE);
    assert_eq!(resolve("node", &home_only(&home)).unwrap().program, newest);
}

/// The engine looks in the app's folder, then Homebrew on Apple silicon and Intel, all
/// before the PATH it inherited (R-M1), then HOME and the system folders, and last the app's
/// own setup and npm harness folders, which no probed folder passes: the owner's own
/// installs win and the app's are the fallback.
#[test]
fn the_engine_looks_in_homebrew_on_both_macs_and_the_system_folders_last() {
    let (_folder, home) = scratch();
    let places = Places::engine(
        Some(Path::new("/engine")),
        Some(Path::new("/app")),
        Some(home.clone()),
        Some(OsStr::new("/usr/bin:/custom/bin:/bin")),
    );
    let folders = places.folders();
    let at = |folder: &str| {
        folders
            .iter()
            .position(|f| f == Path::new(folder))
            .unwrap_or_else(|| panic!("{folder} is looked in"))
    };
    let setup_bin = PathBuf::from("/engine/setup/bin");
    let prefix_bin = crate::setup::harness_prefix(Path::new("/engine")).join("bin");
    assert_eq!(at("/app"), 0);
    assert_eq!(at("/opt/homebrew/bin"), 1);
    assert_eq!(at("/usr/local/bin"), 2);
    assert_eq!(at("/custom/bin"), 3);
    assert!(at("/custom/bin") < folders.iter().position(|f| f.starts_with(&home)).unwrap());
    let owners = folders.len() - 2;
    assert_eq!(
        folders[owners - 4..owners],
        SYSTEM_FOLDERS.map(PathBuf::from)
    );
    assert_eq!(
        folders[owners..],
        [setup_bin.clone(), prefix_bin.clone()],
        "the app's own installs are the fallback after every folder of the owner"
    );
    let probed = home.join("probed/bin");
    let with_probed = places
        .clone()
        .with_probed(vec![probed.clone(), setup_bin.clone()]);
    let folders = with_probed.folders();
    let probed_at = folders.iter().position(|f| *f == probed).unwrap();
    assert!(probed_at < folders.len() - 6, "{folders:?}");
    assert_eq!(
        folders[folders.len() - 2..],
        [setup_bin, prefix_bin],
        "a probed folder never moves the app's folders ahead"
    );
    assert!(folders.contains(&home.join(".local/bin")));
    assert!(folders.contains(&home.join(".cargo/bin")));
    assert!(folders.contains(&home.join(".claude/local")));
}

/// A tool found nowhere gives its typed reason and one fixed owner line with how to install
/// it; tmux's line is the live master's tmux line word for word.
#[test]
fn a_missing_tool_gives_its_typed_reason_and_how_to_install_it() {
    let (_folder, home) = scratch();
    let places = home_only(&home);

    let codex = resolve("codex", &places).unwrap_err();
    assert_eq!(codex.reason, Reason::NotFound);
    assert_eq!(codex.reason.code(), "tool_not_found");
    assert_eq!(codex.looked, places.folders());
    assert_eq!(
        codex.line(),
        "codex is missing. Install Codex: run npm install -g @openai/codex in Terminal. Then try again."
    );

    let tmux = resolve("tmux", &places).unwrap_err();
    assert_eq!(tmux.line(), TMUX_MISSING);
    assert_eq!(tmux.line(), crate::team::master::TERMINAL_MISSING);
}

/// An npm Codex whose `node` is gone needs node, and says so with how to install it.
#[test]
fn a_script_whose_interpreter_is_missing_says_it_needs_that_interpreter() {
    let (_folder, home) = scratch();
    npm_codex(&home.join(".npm-global"));
    let missing = resolve("codex", &home_only(&home)).unwrap_err();
    assert_eq!(missing.reason, Reason::NeedsInterpreter);
    assert_eq!(missing.reason.code(), "tool_needs_interpreter");
    assert_eq!(missing.needs.as_deref(), Some("node"));
    assert_eq!(
        missing.line(),
        "codex needs node, which is missing. Install Node.js from nodejs.org, or run brew install node in Terminal. Then try again."
    );
}

/// A pinned harness entry is read as it is: its script and interpreter, found by name.
#[test]
fn a_pinned_entry_reads_its_interpreter_from_the_places() {
    let (_folder, home) = scratch();
    let script = npm_codex(&home.join("elsewhere"));
    let node = program(&home.join(".local/bin/node"), NODE);
    let codex = resolve_entry(&home.join("elsewhere/bin/codex"), &home_only(&home)).unwrap();
    assert_eq!(codex.program, script);
    assert_eq!(codex.interpreter().unwrap().program, node);
}

/// `env -S` with flags and an absolute interpreter line both resolve, keeping their words.
#[test]
fn shebang_flags_and_absolute_interpreters_are_kept() {
    let (_folder, home) = scratch();
    let node = program(&home.join(".local/bin/node"), NODE);
    let flagged = program(
        &home.join(".local/bin/flagged"),
        "#!/usr/bin/env -S node --no-warnings\n",
    );
    let resolved = resolve("flagged", &home_only(&home)).unwrap();
    assert_eq!(
        resolved.argv(),
        vec![
            node.clone().into_os_string(),
            OsString::from("--no-warnings"),
            flagged.into_os_string(),
        ]
    );
    let absolute = program(
        &home.join(".local/bin/absolute"),
        &format!("#!{} --flag\n", node.display()),
    );
    let resolved = resolve("absolute", &home_only(&home)).unwrap();
    assert_eq!(resolved.interpreter().unwrap().program, node);
    assert_eq!(
        resolved.argv(),
        vec![
            node.clone().into_os_string(),
            OsString::from("--flag"),
            absolute.clone().into_os_string()
        ],
        "macOS runs no interpreter that is itself a script, so it is split out (N3)"
    );
    assert_eq!(
        resolved.grants(),
        vec![absolute, node, PathBuf::from("/bin/sh")]
    );
    let native = program(&home.join(".local/bin/native"), "#!/bin/sh --native\n");
    let resolved = resolve("native", &home_only(&home)).unwrap();
    assert_eq!(
        resolved.argv(),
        vec![native.into_os_string()],
        "the system runs a script whose interpreter by path is a program"
    );
}

/// Files in a project's `.crazyagent` folder, which agents write, are never used.
#[test]
fn a_tool_in_a_crazyagent_folder_is_never_used() {
    let (_folder, home) = scratch();
    let folder = home.join("project/.crazyagent/bin");
    program(&folder.join("tmux"), NODE);
    let places = Places::new(vec![folder], Some(home));
    assert_eq!(
        resolve("tmux", &places).unwrap_err().reason,
        Reason::NotFound
    );
}

/// Every release builds without the clean-Mac gate feature: Homebrew's folders are the real
/// ones whatever the environment says, and no manifest turns the feature on by default or
/// for the desktop app.
#[cfg(not(feature = "clean-mac-gate"))]
#[test]
fn a_release_build_never_remaps_homebrew() {
    assert_eq!(gate_root(), None);
    assert_eq!(homebrew_folders(), HOMEBREW_FOLDERS.map(PathBuf::from));
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let own = std::fs::read_to_string(crate_root.join("Cargo.toml")).unwrap();
    assert!(
        own.lines()
            .filter(|line| line.trim_start().starts_with("default"))
            .all(|line| !line.contains("clean-mac-gate"))
    );
    for manifest in [
        "../../Cargo.toml",
        "../../apps/desktop/src-tauri/Cargo.toml",
    ] {
        let text = std::fs::read_to_string(crate_root.join(manifest)).unwrap();
        assert!(!text.contains("clean-mac-gate"), "{manifest}");
    }
}

/// Probed folders go after every folder known without running a program and before the
/// system folders, and never from a `.crazyagent` folder.
#[test]
fn probed_folders_go_before_the_system_folders() {
    let places = Places::new(
        vec![
            PathBuf::from("/app"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ],
        None,
    )
    .with_probed(vec![
        PathBuf::from("/prefix/bin"),
        PathBuf::from("/work/.crazyagent/bin"),
        PathBuf::from("/usr/bin"),
    ]);
    assert_eq!(
        places.folders(),
        [
            PathBuf::from("/app"),
            PathBuf::from("/prefix/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ]
    );
}

/// Makes a folder a CrazyAgent project, which agents write: it holds a `.crazyagent` folder.
/// Input: the folder. Output: the folder.
fn project(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder.join(".crazyagent")).unwrap();
    folder.to_path_buf()
}

/// R-M1: an engine whose PATH holds a folder inside a project, such as a direnv `bin` or a
/// `node_modules/.bin`, never takes a tool from there: an agent may have planted it. The
/// folder is refused, not only ranked after Homebrew.
#[test]
fn a_path_folder_inside_a_project_is_never_looked_in() {
    let (_folder, home) = scratch();
    let bin = project(&home.join("work/app")).join("node_modules/.bin");
    program(&bin.join("tmux"), NODE);
    let path = std::env::join_paths([&bin]).unwrap();
    let engine = Places::engine(None, None, Some(home.clone()), Some(&path));
    assert!(!engine.folders().contains(&bin), "{:?}", engine.folders());
    assert_eq!(
        resolve("tmux", &home_only(&home)).unwrap_err().reason,
        Reason::NotFound
    );
    assert!(
        Places::new(Vec::new(), None)
            .with_probed(vec![bin.clone()])
            .folders()
            .is_empty(),
        "a probed folder inside a project is refused too"
    );
}

/// R-M1: a link in a folder the resolver trusts whose real file sits inside a project, or
/// inside the agents' temp folders, is refused.
#[test]
fn a_link_into_a_project_is_refused() {
    let (_folder, home) = scratch();
    let planted = program(&project(&home.join("work/app")).join("bin/tmux"), NODE);
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(&planted, home.join(".local/bin/tmux")).unwrap();
    assert_eq!(
        resolve("tmux", &home_only(&home)).unwrap_err().reason,
        Reason::Refused
    );
}

/// Removes a folder a row made outside its scratch folder when the row ends, passed or not.
struct Removed(PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 2.4.1 R4: an agent's short temp folder (`ca-` and 16 hex digits in `/private/tmp`, `/tmp`
/// off macOS), which the agent writes, counts as a place agents can write though this user
/// owns it, so a file there is refused even when an owner link in a trusted folder names it.
#[test]
fn a_file_in_an_agent_short_temp_folder_is_refused_even_through_an_owner_link() {
    let (_folder, home) = scratch();
    let agent_temp = crate::sandbox::toolchains::tmp_path(&home);
    std::fs::create_dir(&agent_temp).unwrap();
    let _removed = Removed(agent_temp.clone());
    let planted = program(&agent_temp.join("bin/tmux"), NODE);
    assert!(
        Places::new(Vec::new(), None).agent_writable(&planted),
        "{planted:?}"
    );
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(&planted, home.join(".local/bin/tmux")).unwrap();
    let missing = resolve("tmux", &home_only(&home)).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    let line = missing.line();
    assert!(line.contains("an agent's temp folder"), "{line}");
    assert!(!line.contains("outside your projects"), "{line}");
    assert_eq!(
        missing.refused.map(|refused| refused.why),
        Some(Refusal::AgentsCanWrite)
    );
}

/// R-L1 and F2: an interpreter named by path goes through the same checks as a tool found by
/// name: one inside a project's `.crazyagent` folder, or inside a project, is reported as
/// refused with its own file and why, never as a missing interpreter, and a pinned entry
/// inside a project is refused too.
#[test]
fn an_interpreter_or_entry_inside_a_project_is_refused() {
    let (_folder, home) = scratch();
    let work = project(&home.join("work/app"));
    let planted = program(&work.join(".crazyagent/bin/node"), NODE);
    program(
        &home.join(".local/bin/agentic"),
        &format!("#!{}\n", planted.display()),
    );
    let missing = resolve("agentic", &home_only(&home)).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    assert_eq!(
        missing.refused.map(|refused| (refused.file, refused.why)),
        Some((planted, Refusal::AgentsCanWrite))
    );
    let inside = program(&work.join("tools/node"), NODE);
    program(
        &home.join(".local/bin/inside"),
        &format!("#!/usr/bin/env {}\n", inside.display()),
    );
    let missing = resolve("inside", &home_only(&home)).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    assert_eq!(missing.refused.map(|refused| refused.file), Some(inside));
    let pinned = program(&work.join("bin/codex"), NODE);
    assert!(resolve_entry(&pinned, &home_only(&home)).is_err());
}

/// F2: a version-manager shim named by path in a first line is never launched: the
/// interpreter is resolved through to the real install by its name.
#[test]
fn a_shim_named_by_path_is_resolved_through_to_the_real_install() {
    let (_folder, home) = scratch();
    let shim = program(&home.join(".volta/bin/node"), NODE);
    let real = program(&home.join(".volta/tools/image/node/22.11.0/bin/node"), NODE);
    let script = program(
        &home.join(".local/bin/shimmed"),
        &format!("#!{}\n", shim.display()),
    );
    let resolved = resolve("shimmed", &home_only(&home)).unwrap();
    assert!(!resolved.argv().contains(&shim.clone().into_os_string()));
    assert!(!resolved.grants().contains(&shim));
    assert_eq!(resolved.interpreter().unwrap().program, real);
    assert_eq!(resolved.program, script);
}

/// F1: an npm script's `node` comes from its own npm prefix first, the `node` beside
/// `<prefix>/lib/node_modules`, even when another `node` sits earlier in the places, so a
/// harness installed under one Node never starts under another.
#[test]
fn an_npm_script_starts_with_the_node_of_its_own_prefix() {
    let (_folder, home) = scratch();
    program(&home.join(".local/bin/node"), NODE);
    let prefix = home.join("nvm-like");
    let script = npm_codex(&prefix);
    let own = program(&prefix.join("bin/node"), NODE);
    let places = home_only(&home);
    let codex = resolve_entry(&prefix.join("bin/codex"), &places).unwrap();
    assert_eq!(codex.program, script);
    assert_eq!(codex.interpreter().unwrap().program, own);
    assert_eq!(codex.argv()[0], own.into_os_string());
}

/// R-L2: a folder other users of the group can write, such as a 0775 staff folder made under
/// umask 002, is passed over; on macOS an admin-group folder, like Intel Homebrew's, is not.
#[test]
fn a_group_writable_folder_is_passed_over_unless_its_group_is_admin() {
    let (_folder, home) = scratch();
    let bin = home.join(".local/bin");
    program(&bin.join("node"), NODE);
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o775)).unwrap();
    let group = std::fs::metadata(&bin).unwrap().gid();
    assert!(
        group != 0 && group != 80,
        "the scratch folder has the user's group"
    );
    assert_eq!(
        resolve("node", &home_only(&home)).unwrap_err().reason,
        Reason::Refused
    );
    #[cfg(target_os = "macos")]
    if std::os::unix::fs::chown(&bin, None, Some(80)).is_ok() {
        assert!(
            resolve("node", &home_only(&home)).is_ok(),
            "admin's group is trusted"
        );
    }
}

/// 2.4.1 R3: an engine data folder inside a project, as a dev layout inside a repo has it,
/// keeps its `setup/bin` refused, since agents of any engine that has the project may write
/// there, and a tool found there is named in a plain refusal, never reported as missing.
#[test]
fn a_tool_in_the_setup_folder_of_an_engine_folder_inside_a_project_is_refused_by_name() {
    let (_folder, home) = scratch();
    let root = project(&home.join("work/repo")).join(".local-state/engine");
    let planted = program(&root.join("setup/bin/crazyagent-dev-tool"), NODE);
    let engine = Places::engine(Some(&root), None, Some(home), Some(OsStr::new("")));
    assert!(
        !engine.folders().contains(&root.join("setup/bin")),
        "{:?}",
        engine.folders()
    );
    let missing = resolve("crazyagent-dev-tool", &engine).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused, "{}", missing.line());
    assert_eq!(
        missing.refused,
        Some(Box::new(Refused {
            file: planted,
            why: Refusal::AgentsCanWrite
        }))
    );
}

/// 2.4.1 R3: a script found in a trusted folder whose `env` interpreter sits only in the
/// set-aside setup folder of an engine data folder inside a project is refused by the
/// interpreter's file, never reported as needing an interpreter with an install hint.
#[test]
fn a_script_whose_interpreter_is_only_in_a_refused_setup_folder_names_that_file() {
    let (_folder, home) = scratch();
    let root = project(&home.join("work/repo")).join(".local-state/engine");
    let interpreter = program(&root.join("setup/bin/crazyagent-dev-node"), NODE);
    program(
        &home.join(".local/bin/crazyagent-dev-script"),
        "#!/usr/bin/env crazyagent-dev-node\n",
    );
    let engine = Places::engine(Some(&root), None, Some(home), Some(OsStr::new("")));
    let missing = resolve("crazyagent-dev-script", &engine).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused, "{}", missing.line());
    assert_eq!(missing.tool, "crazyagent-dev-script");
    assert_eq!(
        missing.refused.map(|refused| refused.file),
        Some(interpreter)
    );
}

/// R-M1: the engine's data folder holds the agents' HOMEs, worktrees and team state, which
/// agents write. A PATH folder there is refused while the engine's own `setup/bin` is kept,
/// and a file found in `setup/bin` whose real file sits in an agent's HOME is passed over.
#[test]
fn a_path_folder_in_the_engine_data_folder_is_refused_and_its_setup_folder_is_kept() {
    let (_folder, home) = scratch();
    let root = home.join("engine");
    let setup = root.join("setup/bin");
    let agent = root.join("team/agents/agent-1/bin");
    let path = std::env::join_paths([&agent, &setup]).unwrap();
    let engine = Places::engine(Some(&root), None, Some(home.clone()), Some(&path));
    assert!(engine.folders().contains(&setup), "{:?}", engine.folders());
    assert!(!engine.folders().contains(&agent), "{:?}", engine.folders());

    let planted = program(&agent.join("tmux"), NODE);
    std::fs::create_dir_all(&setup).unwrap();
    std::os::unix::fs::symlink(&planted, setup.join("tmux")).unwrap();
    let places = Places::engine(Some(&root), None, Some(home.clone()), Some(OsStr::new("")));
    assert!(
        resolve("tmux", &places).map_or(true, |tmux| tmux.program != planted),
        "a link into an agent's HOME is never used"
    );
    assert!(resolve_entry(&setup.join("tmux"), &places).is_err());
    std::fs::remove_file(setup.join("tmux")).unwrap();
    let own = program(&setup.join("tmux"), NODE);
    assert_eq!(
        resolve_entry(&setup.join("tmux"), &places).unwrap().program,
        own,
        "the setup folder is kept, though an owner tmux found first wins by name"
    );
}

/// R-M1: a folder of the PATH the engine inherited, frozen from the shell that installed it,
/// that sits in a temp folder, which agents with the temp folder grant and other users
/// write, is refused. Probed folders meet the same rule on the engine's places (2.4.1 R1).
#[test]
fn a_path_folder_in_a_temp_folder_is_refused() {
    let (_folder, home) = scratch();
    let owner_temp = std::env::temp_dir().join("tools-planted/bin");
    let shared = PathBuf::from("/private/tmp/tools-planted/bin");
    let path = std::env::join_paths([&owner_temp, &shared]).unwrap();
    let engine = Places::engine(None, None, Some(home.clone()), Some(&path));
    assert!(
        !engine.folders().contains(&owner_temp),
        "{:?}",
        engine.folders()
    );
    assert!(
        !engine.folders().contains(&shared),
        "{:?}",
        engine.folders()
    );
}

/// 2.4.1 R1: a folder the probes found, a login shell PATH folder or npm's global prefix,
/// that sits in this user's temp folder or a shared one is refused on the engine's places,
/// as written and by its real path: agents with the owner's temp folder grant write the one
/// and other users the other, and the engine runs what it finds there outside any sandbox.
#[test]
fn a_probed_folder_in_a_temp_folder_is_refused_on_the_engine_places() {
    let (_folder, home) = scratch();
    let owner_temp = tempfile::Builder::new()
        .prefix("tools-probed-")
        .tempdir()
        .unwrap();
    let written = owner_temp.path().join("bin");
    std::fs::create_dir_all(&written).unwrap();
    let real = written.canonicalize().unwrap();
    let shared = PathBuf::from("/private/tmp/tools-planted/bin");
    let engine = Places::engine(None, None, Some(home), Some(OsStr::new("")));
    let probed = engine.with_probed(vec![written.clone(), real.clone(), shared.clone()]);
    for folder in [&written, &real, &shared] {
        assert!(
            !probed.folders().contains(folder),
            "{folder:?} in {:?}",
            probed.folders()
        );
    }
}

/// 2.4.1 r1 F1: a tool only in a temp folder the engine's places refuse, on the inherited
/// PATH or found by the probes, is refused by its file with the temp folder line, never
/// reported missing with an install hint, and so is a script whose `env` interpreter sits
/// only there.
#[test]
fn a_tool_only_in_a_refused_temp_folder_is_named_as_refused_not_missing() {
    let (_folder, home) = scratch();
    let owner_temp = tempfile::Builder::new()
        .prefix("tools-temp-")
        .tempdir()
        .unwrap();
    let on_path = owner_temp.path().join("path/bin");
    let probed = owner_temp.path().join("probed/bin");
    let listed = program(&on_path.join("crazyagent-temp-tool"), NODE);
    let found = program(&probed.join("crazyagent-probed-tool"), NODE);
    let interpreter = program(&on_path.join("crazyagent-temp-node"), NODE);
    program(
        &home.join(".local/bin/crazyagent-temp-script"),
        "#!/usr/bin/env crazyagent-temp-node\n",
    );
    let path = std::env::join_paths([&on_path]).unwrap();
    let engine =
        Places::engine(None, None, Some(home), Some(&path)).with_probed(vec![probed.clone()]);
    for (tool, file) in [
        ("crazyagent-temp-tool", &listed),
        ("crazyagent-probed-tool", &found),
        ("crazyagent-temp-script", &interpreter),
    ] {
        let missing = resolve(tool, &engine).unwrap_err();
        assert_eq!(missing.reason, Reason::Refused, "{}", missing.line());
        assert_eq!(missing.tool, tool);
        assert_eq!(
            missing.refused,
            Some(Box::new(Refused {
                file: file.clone(),
                why: Refusal::InTemp,
            }))
        );
        assert!(
            missing.line().contains("in a temp folder"),
            "{}",
            missing.line()
        );
    }
}

/// 2.4.1 r2 RH2-L1: a file found through a place outside every temp folder whose real file
/// lies in one, such as an owner link in `~/.local/bin` into the temp folder, leads into
/// temp; one whose place is itself in a temp folder, or whose real file is not, does not.
#[test]
fn a_link_from_a_place_outside_temp_into_a_temp_folder_leads_into_temp() {
    let temps = [
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders/xy/T"),
    ];
    let link = Path::new("/Users/ana/.local/bin/tool");
    assert!(leads_into_temp(
        link,
        Path::new("/private/var/folders/xy/T/x/tool"),
        &temps
    ));
    assert!(leads_into_temp(
        link,
        Path::new("/private/tmp/x/tool"),
        &temps
    ));
    assert!(!leads_into_temp(link, Path::new("/opt/tools/tool"), &temps));
    assert!(
        !leads_into_temp(
            Path::new("/private/tmp/home/.local/bin/tool"),
            Path::new("/private/tmp/other/tool"),
            &temps
        ),
        "a place itself in a temp folder"
    );
    assert!(!leads_into_temp(
        Path::new("/private/var/folders/xy/T/home/bin/tool"),
        Path::new("/private/tmp/x/tool"),
        &temps
    ));
}

/// 2.4.1 r2 RH2-L1: a kept place that is itself in a temp folder, as a test HOME is, still
/// gives its tools on the engine's places, a link from it into another temp folder included,
/// and an entry written there passes the launch check.
#[test]
fn a_tool_in_a_kept_place_inside_a_temp_folder_still_resolves() {
    let (_folder, home) = scratch();
    let other = tempfile::Builder::new()
        .prefix("tools-other-")
        .tempdir()
        .unwrap();
    let real = program(&other.path().join("crazyagent-linked-tool"), NODE);
    let plain = program(&home.join(".local/bin/crazyagent-plain-tool"), NODE);
    std::os::unix::fs::symlink(&real, home.join(".local/bin/crazyagent-linked-tool")).unwrap();
    let engine = Places::engine(None, None, Some(home.clone()), Some(OsStr::new("")));
    assert_eq!(
        resolve("crazyagent-plain-tool", &engine).unwrap().program,
        plain
    );
    assert!(resolve("crazyagent-linked-tool", &engine).is_ok());
    assert!(resolve_entry(&plain, &engine).is_ok());
}

/// A tool in this user's own folder under `/tmp` (root 1777, sticky) is used: no other user
/// can replace a folder this user owns there, so test stand-ins and engines started from a
/// `/tmp` folder resolve (leader 17:12, L8c).
#[test]
fn a_tool_in_an_own_folder_under_the_shared_temp_folder_is_used() {
    let folder = tempfile::Builder::new()
        .prefix("tools-tmp-")
        .tempdir_in("/tmp")
        .unwrap();
    let own = folder.path().canonicalize().unwrap();
    let tool = program(&own.join("tool"), NODE);
    let places = Places::new(vec![own.clone()], None);
    assert_eq!(resolve("tool", &places).unwrap().program, tool);
}

/// RC-M1 on Linux: the engine looks in Linuxbrew and the NixOS system and default profiles,
/// in that order and before the system folders. A tool in a Linuxbrew prefix and one a
/// profile links into its store are found; a profile link into a project is refused.
#[cfg(target_os = "linux")]
#[test]
fn linuxbrew_and_nixos_profile_tools_are_found_and_a_link_into_a_project_is_refused() {
    let (_folder, home) = scratch();
    let engine = Places::engine(None, None, Some(home.clone()), Some(OsStr::new("")));
    let at = |folder: &str| {
        engine
            .folders()
            .iter()
            .position(|f| f == Path::new(folder))
            .unwrap_or_else(|| panic!("{folder} is looked in"))
    };
    let linux = LINUX_FOLDERS.map(at);
    assert!(linux.windows(2).all(|pair| pair[0] < pair[1]), "{linux:?}");
    assert!(linux[2] < at(SYSTEM_FOLDERS[0]));

    let brew = home.join("home/linuxbrew/.linuxbrew/bin");
    let brewed = program(&brew.join("tmux"), NODE);
    let stored = program(&home.join("nix/store/0-uv/bin/uv"), NODE);
    let profile = home.join("nix/var/nix/profiles/default/bin");
    std::fs::create_dir_all(&profile).unwrap();
    std::os::unix::fs::symlink(&stored, profile.join("uv")).unwrap();
    let places = Places::new(vec![brew, profile.clone()], Some(home.clone()));
    assert_eq!(resolve("tmux", &places).unwrap().program, brewed);
    let uv = resolve("uv", &places).unwrap();
    assert_eq!((uv.entry, uv.program), (profile.join("uv"), stored));

    let planted = program(&project(&home.join("work/app")).join("uv"), NODE);
    let twin = home.join("run/current-system/sw/bin");
    std::fs::create_dir_all(&twin).unwrap();
    std::os::unix::fs::symlink(&planted, twin.join("uv")).unwrap();
    let refused = Places::new(vec![twin], Some(home));
    assert_eq!(resolve("uv", &refused).unwrap_err().reason, Reason::Refused);
}

/// The macOS group every local user is in and the resolver does not trust for writes.
#[cfg(target_os = "macos")]
const STAFF: u32 = 20;

/// F2 (r2): a tmux the resolver passes over, here in a Homebrew folder the staff group can
/// write, is reported as refused with the file and the folder to fix, never as missing with
/// how to install it: brew would answer that it is installed and Try again would loop.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_tool_names_its_file_and_why_and_never_says_it_is_missing() {
    let (_folder, home) = scratch();
    let bin = home.join("opt/homebrew/bin");
    let tmux = program(&bin.join("tmux"), NODE);
    std::os::unix::fs::chown(&bin, None, Some(STAFF)).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o775)).unwrap();
    let missing = resolve("tmux", &Places::new(vec![bin.clone()], Some(home))).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    assert_eq!(
        missing.refused,
        Some(Box::new(Refused {
            file: tmux.clone(),
            why: Refusal::OthersCanChange(bin.clone()),
        }))
    );
    let line = missing.line();
    assert_ne!(line, TMUX_MISSING);
    assert!(
        !line.contains("missing") && !line.contains("brew install"),
        "{line}"
    );
    assert!(line.contains(&tmux.display().to_string()), "{line}");
    assert!(
        line.contains(&format!("change {}.", bin.display())),
        "{line}"
    );
}

/// R2-L1: a tool file the staff group can write, in this user's 0755 folder, is passed over
/// with the refused line naming the file; at 0755 it is found, and an admin-group file is
/// still trusted.
#[cfg(target_os = "macos")]
#[test]
fn a_tool_file_the_staff_group_can_write_is_passed_over() {
    let (_folder, home) = scratch();
    let bin = home.join("tools");
    let tool = program(&bin.join("tool"), NODE);
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::chown(&tool, None, Some(STAFF)).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o775)).unwrap();
    let places = Places::new(vec![bin.clone()], Some(home));
    let missing = resolve("tool", &places).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    assert_eq!(
        missing.refused,
        Some(Box::new(Refused {
            file: tool.clone(),
            why: Refusal::OthersCanChange(tool.clone()),
        }))
    );
    assert!(missing.line().contains(&tool.display().to_string()));
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve("tool", &places).unwrap().program, tool);
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o775)).unwrap();
    if std::os::unix::fs::chown(&tool, None, Some(80)).is_ok() {
        assert_eq!(
            resolve("tool", &places).unwrap().program,
            tool,
            "admin's group is trusted"
        );
    }
}

/// r2 item 5: a tool only in the engine's `<root>/setup/bin`, like the node an in-app setup
/// installed, is found by setup and by a harness launch alike. Its name is one no machine
/// has, since the engine's folders come last and an owner's own node would win. A launch
/// uses the root this process's engine recorded, given here so no row records the global.
#[test]
fn a_launch_looks_in_the_engine_setup_folder_like_setup_does() {
    let (_folder, home) = scratch();
    let root = home.join("engine");
    let node = program(&root.join("setup/bin/crazyagent-dev-node"), NODE);
    let codex = program(
        &home.join("npm/bin/codex"),
        "#!/usr/bin/env crazyagent-dev-node\n",
    );
    let setup = Places::from_environment(Some(&root));
    assert_eq!(
        resolve("crazyagent-dev-node", &setup).unwrap().program,
        node
    );
    let launch = Places::at_roots(None, Some(&root));
    let started = crate::harnesses::interactive::launch_tool(&codex, &launch).unwrap();
    assert_eq!(started.argv()[0], node.into_os_string());
}

/// rv-v3sec M16-S1: an engine started with a non-default `--home`, `CRAZYAGENT_HOME` unset,
/// refuses a harness file agents can write under that root, such as one in an agent's
/// home: a launch takes the root the engine recorded, given here explicitly.
#[test]
fn a_harness_in_an_agent_home_under_a_custom_engine_root_is_refused() {
    let (_folder, home) = scratch();
    let root = home.join("custom-engine");
    let entry = program(
        &root.join("team/agents/agent-1/home/.local/bin/codex"),
        NODE,
    );
    let launch = Places::at_roots(None, Some(&root));
    let missing = resolve_entry(&entry, &launch).unwrap_err();
    assert_eq!(missing.reason, Reason::Refused);
    assert_eq!(
        missing.refused.map(|refused| refused.why),
        Some(Refusal::AgentsCanWrite)
    );
    let crate::harnesses::launch::LaunchError(line, _) =
        crate::harnesses::interactive::launch_tool(&entry, &launch).unwrap_err();
    assert!(line.contains(&entry.display().to_string()), "{line}");
}
